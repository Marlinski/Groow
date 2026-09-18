//! One turn.
//!
//! The process starts, claims the turn it was spawned for, and runs a single exchange: read
//! what came in, generate, run whatever tools were asked for, generate again, until there is
//! an answer or the budget runs out. Then it reports what happened and exits.
//!
//! It holds nothing across runs. Everything it knows came from the claim, and everything it
//! changes it changes by asking the core. Running the same turn twice is therefore either the
//! same turn or a clean refusal; it is never half a turn.

use std::path::PathBuf;
use std::time::Duration;

use groow_proto::event::{Event, EventName};
use groow_proto::turn::{Message, SignalKind, TurnContext, TurnOutcome};
use serde_json::json;

use crate::client::{Client, ClientError};
use crate::parse::{self, Call};
use crate::tools::{self, Surface, ToolResult};

/// What the turn noticed about itself. These are read by the limbic system as the free
/// signals: things that can be observed without anyone having to say how it went.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Flags {
    pub tool_error: u32,
    pub repeat: u32,
    pub recovered: u32,
    pub exhausted: bool,
    pub completed: bool,
    pub truncated: u32,
}

impl Flags {
    pub fn to_list(&self) -> Vec<String> {
        let mut v = Vec::new();
        for _ in 0..self.tool_error { v.push("tool_error".to_string()) }
        for _ in 0..self.repeat { v.push("repeat".to_string()) }
        for _ in 0..self.recovered { v.push("recovered".to_string()) }
        for _ in 0..self.truncated { v.push("truncated".to_string()) }
        if self.exhausted { v.push("exhausted".into()) }
        if self.completed { v.push("completed".into()) }
        v
    }

    /// A turn worth learning from. One that went round in circles or ran out of room is not a
    /// good example of anything, and training on it teaches the habit.
    pub fn worth_learning_from(&self) -> bool {
        self.completed && !self.exhausted && self.repeat == 0 && self.tool_error <= 1
    }
}

pub struct Settings {
    pub home: PathBuf,
    pub tool_timeout: Duration,
    pub surface: Surface,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            home: std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")),
            tool_timeout: Duration::from_secs(120),
            surface: Surface::Main,
        }
    }
}

/// Run the turn this process was started for.
pub async fn run_turn(client: &mut Client, settings: &Settings) -> Result<TurnOutcome, ClientError> {
    let ctx = client.claim().await?;
    let started = std::time::Instant::now();
    let mut flags = Flags::default();

    // The conversation this turn works from: who it is, what has been said, and what just
    // arrived. Everything before this came from the core, not from this process.
    let mut history: Vec<Message> = Vec::with_capacity(ctx.window.len() + 2);
    history.push(Message::system(&ctx.system));
    history.extend(ctx.window.iter().cloned());
    let incoming = Message::user(&ctx.framed);
    history.push(incoming.clone());
    client.append(&ctx.turn, ctx.epoch, &incoming).await?;

    let schemas = tools::schemas(settings.surface);
    let mut seen_calls: Vec<String> = Vec::new();
    let mut last_call_failed = false;
    let mut final_text = String::new();
    let mut tools_used = 0u32;

    for round in 0..ctx.max_rounds {
        if client.was_cancelled() {
            break;
        }
        let generated = client
            .call_streaming(
                "complete",
                json!({
                    "messages": history,
                    "tools": schemas,
                    "turn": ctx.turn,
                    "priority": if settings.surface == Surface::Main { 0 } else { 2 },
                }),
                |_| {},
            )
            .await?;
        let raw = generated.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let p = parse::parse(raw);
        if p.truncated {
            flags.truncated += 1;
        }

        // Record what was said, tool calls and all, before running anything. If this process
        // dies mid-tool, the conversation still shows what it was trying to do.
        let mut said = Message::assistant(&p.content);
        if !p.calls.is_empty() {
            said.tool_calls = Some(json!(p
                .calls
                .iter()
                .map(|c| json!({"name": c.name, "arguments": c.args}))
                .collect::<Vec<_>>()));
        }
        history.push(said.clone());
        client.append(&ctx.turn, ctx.epoch, &said).await?;
        let _ = client
            .emit(Event::new(EventName::Message, json!({
                "role": "assistant", "content": p.content, "turn": ctx.turn,
            })))
            .await;

        if p.calls.is_empty() {
            final_text = p.content;
            flags.completed = true;
            break;
        }

        for call in &p.calls {
            tools_used += 1;
            if tools::is_repeat(&mut seen_calls, &call.name, &call.args) {
                flags.repeat += 1;
            }
            let _ = client
                .emit(Event::new(EventName::ToolCall, json!({
                    "name": call.name, "args": call.args, "turn": ctx.turn,
                    "actor": actor(settings.surface),
                })))
                .await;

            let result = run_call(client, call, settings).await;
            if result.ok && last_call_failed {
                // It worked out what went wrong and got past it. That is worth something.
                flags.recovered += 1;
            }
            if !result.ok {
                flags.tool_error += 1;
            }
            last_call_failed = !result.ok;

            let _ = client
                .emit(Event::new(EventName::ToolResult, json!({
                    "name": call.name, "result": tools::clip(&result.text, 600),
                    "ok": result.ok, "seconds": result.seconds,
                    "turn": ctx.turn, "actor": actor(settings.surface),
                })))
                .await;

            let msg = Message::tool(&call.name, &result.text);
            history.push(msg.clone());
            client.append(&ctx.turn, ctx.epoch, &msg).await?;

            // `finish` ends an inner thought there and then.
            if call.name == "finish" {
                final_text = tools::arg_str(&call.args, "summary");
                flags.completed = true;
                return report(client, &ctx, final_text, flags, tools_used, started).await;
            }
        }

        if round + 1 == ctx.max_rounds {
            flags.exhausted = true;
        }
    }

    if final_text.is_empty() && flags.exhausted {
        final_text = "I ran out of room before I got there. What I found is above.".into();
    }
    report(client, &ctx, final_text, flags, tools_used, started).await
}

async fn report(
    client: &mut Client,
    ctx: &TurnContext,
    final_text: String,
    flags: Flags,
    tools_used: u32,
    started: std::time::Instant,
) -> Result<TurnOutcome, ClientError> {
    let out = TurnOutcome {
        turn: ctx.turn.clone(),
        epoch: ctx.epoch,
        final_text,
        flags: flags.to_list(),
        tools_used,
        seconds: started.elapsed().as_secs_f64(),
    };
    client.end(&out).await?;
    Ok(out)
}

fn actor(s: Surface) -> &'static str {
    match s {
        Surface::Main => "main",
        Surface::Thought => "thought",
    }
}

/// Run one tool call. Everything that is not a shell command is a request to the core, so the
/// harness never changes state itself.
async fn run_call(client: &mut Client, call: &Call, settings: &Settings) -> ToolResult {
    match call.name.as_str() {
        "shell" => {
            let cmd = tools::arg_str(&call.args, "command");
            tools::run_shell(&cmd, &settings.home, settings.tool_timeout).await
        }
        "ask" => {
            let q = tools::arg_str(&call.args, "question");
            let c = tools::arg_str(&call.args, "context");
            match client.call("ask", json!({"question": q, "context": c})).await {
                Ok(v) => ToolResult::good(describe_receipt(&v)),
                Err(e) => ToolResult::bad(format!("the question could not be left: {e}")),
            }
        }
        "think" => {
            let goal = tools::arg_str(&call.args, "goal");
            let steps = tools::arg_u32(&call.args, "max_steps", 8);
            match client.call("think", json!({"goal": goal, "max_steps": steps})).await {
                Ok(v) => ToolResult::good(format!(
                    "started as thought {}; it will come back to you",
                    v.get("id").and_then(|i| i.as_str()).unwrap_or("?")
                )),
                Err(e) => ToolResult::bad(format!("that thought could not be started: {e}")),
            }
        }
        "focus" => {
            let m = tools::arg_str(&call.args, "message");
            match client.call("thought", json!({"action": "focus", "id": thought_id(), "text": m})).await {
                Ok(_) => ToolResult::good("passed to the main thread"),
                Err(e) => ToolResult::bad(format!("that did not reach the main thread: {e}")),
            }
        }
        "finish" => {
            let s = tools::arg_str(&call.args, "summary");
            match client.call("thought", json!({"action": "finish", "id": thought_id(), "text": s})).await {
                Ok(_) => ToolResult::good("finished"),
                Err(e) => ToolResult::bad(format!("could not finish cleanly: {e}")),
            }
        }
        other => ToolResult::bad(format!(
            "there is no tool called `{other}`. You have: {}",
            tools::names(settings.surface).join(", ")
        )),
    }
}

/// Which thought this process is running, as the core told it when it started it.
fn thought_id() -> String {
    std::env::var("GROOW_THOUGHT").unwrap_or_default()
}

/// Turn the receipt for a question into something the mind can act on: not just an
/// acknowledgement, but what it cost.
fn describe_receipt(v: &serde_json::Value) -> String {
    let open = v.get("open_questions").and_then(|x| x.as_u64()).unwrap_or(0);
    let budget = v.get("budget").and_then(|x| x.as_u64()).unwrap_or(0);
    let hours = v.get("expires_in_hours").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let mut s = format!(
        "left for your mentor. {open} of {budget} questions are now waiting, and this one \
expires in {hours:.0} hours if nobody answers."
    );
    if let Some(d) = v.get("dropped_to_make_room").and_then(|d| d.as_object()) {
        let q = d.get("question").and_then(|q| q.as_str()).unwrap_or("");
        let waited = d.get("waited").and_then(|w| w.as_str()).unwrap_or("");
        s.push_str(&format!(
            " To make room, your oldest question was dropped unanswered after {waited}: \u{201c}{q}\u{201d}."
        ));
    }
    s
}

/// The kind of signal, for anything that needs to treat a person differently.
pub fn is_from_a_person(k: SignalKind) -> bool {
    k.is_human()
}

#[cfg(test)]
mod tests {
    use super::*;
    use groow_proto::Frame;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    /// A stand-in core that answers a turn's requests from a script of generations, and
    /// records everything the harness sent it.
    #[derive(Default)]
    struct Recorded {
        appended: Vec<Message>,
        outcome: Option<TurnOutcome>,
        asked: Vec<serde_json::Value>,
        completions: usize,
    }

    async fn fake_core(
        path: PathBuf,
        ctx: TurnContext,
        generations: Vec<String>,
    ) -> Arc<Mutex<Recorded>> {
        let log = Arc::new(Mutex::new(Recorded::default()));
        let out = log.clone();
        let l = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let (r, mut w) = tokio::io::split(s);
            let mut r = BufReader::new(r);
            let mut gens = generations.into_iter();
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                let Ok(f) = Frame::decode(&line) else { continue };
                let Frame::Req { id, op, arg } = f else { continue };
                let reply = match op.as_str() {
                    "turn.claim" => Frame::rep(id, serde_json::to_value(&ctx).unwrap()),
                    "turn.append" => {
                        let m: Message = serde_json::from_value(arg["message"].clone()).unwrap();
                        log.lock().unwrap().appended.push(m);
                        Frame::rep(id, json!({"ok": true}))
                    }
                    "complete" => {
                        log.lock().unwrap().completions += 1;
                        let text = gens.next().unwrap_or_else(|| "I have nothing more.".into());
                        Frame::rep(id, json!({"text": text}))
                    }
                    "turn.end" => {
                        log.lock().unwrap().outcome = serde_json::from_value(arg).ok();
                        Frame::rep(id, json!({"ok": true}))
                    }
                    "ask" => {
                        log.lock().unwrap().asked.push(arg);
                        Frame::rep(id, json!({
                            "ok": true, "id": "abc123", "open_questions": 2, "budget": 5,
                            "expires_in_hours": 48.0,
                        }))
                    }
                    "think" => Frame::rep(id, json!({"ok": true, "id": "th0001"})),
                    "thought" => Frame::rep(id, json!({"ok": true})),
                    _ => Frame::rep(id, json!({"ok": true})),
                };
                if w.write_all(reply.encode().as_bytes()).await.is_err() {
                    return;
                }
                let _ = w.flush().await;
            }
        });
        out
    }

    fn ctx(max_rounds: u32) -> TurnContext {
        TurnContext {
            turn: "t-1".into(),
            epoch: groow_proto::turn::Epoch(3),
            kind: SignalKind::User,
            text: "what is in this folder?".into(),
            framed: "what is in this folder?".into(),
            system: "You are Groow.".into(),
            window: vec![],
            max_rounds,
            meta: json!({}),
        }
    }

    async fn run(gens: Vec<String>, rounds: u32) -> (TurnOutcome, Arc<Mutex<Recorded>>, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("core.sock");
        let log = fake_core(p.clone(), ctx(rounds), gens).await;
        let mut c = Client::connect(&p).await.unwrap();
        let s = Settings {
            home: d.path().to_path_buf(),
            tool_timeout: Duration::from_secs(5),
            surface: Surface::Main,
        };
        let out = run_turn(&mut c, &s).await.unwrap();
        (out, log, d)
    }

    #[tokio::test]
    async fn a_plain_answer_ends_the_turn_in_one_round() {
        let (out, log, _d) = run(vec!["Three files.".into()], 10).await;
        assert_eq!(out.final_text, "Three files.");
        assert!(out.flags.contains(&"completed".to_string()));
        assert_eq!(out.tools_used, 0);
        assert_eq!(log.lock().unwrap().completions, 1, "it should not have generated twice");
    }

    #[tokio::test]
    async fn the_incoming_message_is_recorded_before_anything_is_generated() {
        let (_out, log, _d) = run(vec!["ok".into()], 10).await;
        let appended = &log.lock().unwrap().appended;
        assert_eq!(appended[0].role, "user");
        assert_eq!(appended[0].text(), "what is in this folder?");
    }

    #[tokio::test]
    async fn a_tool_call_runs_and_the_answer_comes_in_the_next_round() {
        let (out, log, _d) = run(
            vec![
                "<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"echo two files\"}}</tool_call>".into(),
                "There are two files.".into(),
            ],
            10,
        )
        .await;
        assert_eq!(out.final_text, "There are two files.");
        assert_eq!(out.tools_used, 1);
        let appended = &log.lock().unwrap().appended;
        let tool_msg = appended.iter().find(|m| m.role == "tool").expect("the tool result was not recorded");
        assert_eq!(tool_msg.name.as_deref(), Some("shell"));
        assert!(tool_msg.text().contains("two files"));
    }

    #[tokio::test]
    async fn what_it_was_trying_to_do_is_recorded_before_the_tool_runs() {
        let (_out, log, _d) = run(
            vec![
                "Looking.<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"true\"}}</tool_call>".into(),
                "done".into(),
            ],
            10,
        )
        .await;
        let appended = &log.lock().unwrap().appended;
        let i_said = appended.iter().position(|m| m.role == "assistant").unwrap();
        let i_tool = appended.iter().position(|m| m.role == "tool").unwrap();
        assert!(i_said < i_tool, "the intention must be on record before the action");
        assert!(appended[i_said].tool_calls.is_some());
    }

    #[tokio::test]
    async fn a_failing_command_is_flagged_and_the_turn_carries_on() {
        let (out, _log, _d) = run(
            vec![
                "<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"exit 1\"}}</tool_call>".into(),
                "That did not work.".into(),
            ],
            10,
        )
        .await;
        assert!(out.flags.contains(&"tool_error".to_string()));
        assert!(out.flags.contains(&"completed".to_string()), "a failure is not the end of a turn");
    }

    #[tokio::test]
    async fn getting_past_a_failure_is_noticed() {
        let (out, _log, _d) = run(
            vec![
                "<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"exit 1\"}}</tool_call>".into(),
                "<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"echo better\"}}</tool_call>".into(),
                "Got it.".into(),
            ],
            10,
        )
        .await;
        assert!(out.flags.contains(&"recovered".to_string()), "recovery should be its own signal: {:?}", out.flags);
    }

    #[tokio::test]
    async fn going_round_in_circles_is_noticed() {
        let same = "<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}</tool_call>".to_string();
        let (out, _log, _d) = run(vec![same.clone(), same.clone(), "I am stuck.".into()], 10).await;
        assert!(out.flags.contains(&"repeat".to_string()));
    }

    #[tokio::test]
    async fn running_out_of_rounds_is_flagged_and_still_gives_an_answer() {
        let call = "<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"echo again\"}}</tool_call>".to_string();
        let (out, _log, _d) = run(vec![call.clone(), call.clone(), call], 2).await;
        assert!(out.flags.contains(&"exhausted".to_string()));
        assert!(!out.final_text.is_empty(), "a person should still get something back");
    }

    #[tokio::test]
    async fn a_question_to_the_mentor_comes_back_with_what_it_cost() {
        let (_out, log, _d) = run(
            vec![
                "<tool_call>{\"name\":\"ask\",\"arguments\":{\"question\":\"what next?\"}}</tool_call>".into(),
                "Asked.".into(),
            ],
            10,
        )
        .await;
        let l = log.lock().unwrap();
        assert_eq!(l.asked.len(), 1);
        assert_eq!(l.asked[0]["question"], "what next?");
        let result = l.appended.iter().find(|m| m.name.as_deref() == Some("ask")).unwrap();
        assert!(result.text().contains("of 5 questions"), "the cost was not shown: {}", result.text());
        assert!(result.text().contains("expires"));
    }

    #[tokio::test]
    async fn an_unknown_tool_is_refused_with_the_list_of_real_ones() {
        let (out, log, _d) = run(
            vec![
                "<tool_call>{\"name\":\"rm_rf\",\"arguments\":{}}</tool_call>".into(),
                "Sorry.".into(),
            ],
            10,
        )
        .await;
        let l = log.lock().unwrap();
        let r = l.appended.iter().find(|m| m.name.as_deref() == Some("rm_rf")).unwrap();
        assert!(r.text().contains("no tool called"));
        assert!(r.text().contains("shell"), "it should be told what it does have");
        assert!(out.flags.contains(&"tool_error".to_string()));
    }

    #[tokio::test]
    async fn a_cut_off_generation_is_flagged_rather_than_shown_as_speech() {
        let (out, log, _d) = run(
            vec!["Looking now.<tool_call>{\"name\": \"shel".into(), "done".into()],
            10,
        )
        .await;
        assert!(out.flags.contains(&"truncated".to_string()));
        let l = log.lock().unwrap();
        let said = l.appended.iter().find(|m| m.role == "assistant").unwrap();
        assert_eq!(said.text(), "Looking now.", "raw json must never reach the conversation");
    }

    #[test]
    fn a_turn_that_went_badly_is_not_worth_learning_from() {
        assert!(Flags { completed: true, ..Default::default() }.worth_learning_from());
        assert!(!Flags { completed: true, repeat: 1, ..Default::default() }.worth_learning_from());
        assert!(!Flags { completed: true, exhausted: true, ..Default::default() }.worth_learning_from());
        assert!(!Flags { completed: false, ..Default::default() }.worth_learning_from());
        assert!(!Flags { completed: true, tool_error: 3, ..Default::default() }.worth_learning_from());
    }

    #[test]
    fn the_flag_list_carries_how_often_each_thing_happened() {
        let f = Flags { tool_error: 2, repeat: 1, completed: true, ..Default::default() };
        let l = f.to_list();
        assert_eq!(l.iter().filter(|s| *s == "tool_error").count(), 2);
        assert_eq!(l.iter().filter(|s| *s == "repeat").count(), 1);
        assert!(l.contains(&"completed".to_string()));
    }
}
