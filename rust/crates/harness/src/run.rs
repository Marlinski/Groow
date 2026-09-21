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
    let ctx = match settings.surface {
        Surface::Main => client.claim().await?,
        Surface::Thought => client.claim_thought(&thought_id()).await?,
    };
    let started = std::time::Instant::now();
    let mut flags = Flags::default();

    // The conversation this turn works from: who it is, what has been said, and what just
    // arrived. Everything before this came from the core, not from this process.
    //
    // The core's part of the prompt is the part that cannot be argued with. Anything after it
    // comes from the mind's own script, run here because this is the process that runs as the
    // mind, in its home.
    let mut system = ctx.system.clone();
    if let Some(extra) = crate::greeting::greeting(&settings.home).await {
        system.push_str("\n\n");
        system.push_str(&extra);
    }
    let mut history: Vec<Message> = Vec::with_capacity(ctx.window.len() + 2);
    history.push(Message::system(&system));
    history.extend(ctx.window.iter().cloned());
    // The core has already recorded what came in, so that a turn which fails and is retried
    // does not write the same message to the conversation twice. A thought's own trace is
    // different: the step prompt is part of it, and it is written here.
    let incoming = Message::user(&ctx.framed);
    history.push(incoming.clone());
    if settings.surface == Surface::Thought {
        write(client, settings, &ctx, &incoming).await?;
    }

    let schemas = tools::schemas(settings.surface);
    let tool_names = tools::names(settings.surface);
    let mut seen_calls: Vec<String> = Vec::new();
    let mut last_call_failed = false;
    let mut final_text = String::new();
    let mut tools_used = 0u32;
    // What the turn cost: everything generated across every round, and the part of the wall
    // clock spent waiting for the brain rather than running commands.
    let mut tokens = 0u32;
    let mut thinking = 0.0f64;

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
        tokens += generated.get("tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        thinking += generated.get("seconds").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let raw = generated.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let p = parse::parse_with_tools(raw, &tool_names);
        if p.truncated {
            flags.truncated += 1;
        }

        // Record what was said, tool calls and all, before running anything. If this process
        // dies mid-tool, the conversation still shows what it was trying to do.
        let mut said = Message::assistant(&p.content);
        if !p.calls.is_empty() {
            said = said.with_calls(json!(p
                .calls
                .iter()
                .map(|c| json!({"name": c.name, "arguments": c.args}))
                .collect::<Vec<_>>()));
        }
        history.push(said.clone());
        // Writing it is what announces it: the core emits the event when it takes the record,
        // so saying it again here would have every watcher show the answer twice.
        write(client, settings, &ctx, &said).await?;

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
            write(client, settings, &ctx, &msg).await?;

            // `finish` ends an inner thought there and then.
            if call.name == "finish" {
                final_text = tools::arg_str(&call.args, "summary");
                flags.completed = true;
                return report(client, settings, &ctx, final_text, flags, Cost { tools: tools_used, tokens, thinking }, started).await;
            }
        }

        if round + 1 == ctx.max_rounds {
            flags.exhausted = true;
        }
    }

    if final_text.is_empty() && flags.exhausted {
        final_text = "I ran out of room before I got there. What I found is above.".into();
    }
    report(client, settings, &ctx, final_text, flags, Cost { tools: tools_used, tokens, thinking }, started).await
}

/// Record one message, on whichever trace this process is working on.
async fn write(
    client: &mut Client,
    settings: &Settings,
    ctx: &TurnContext,
    msg: &Message,
) -> Result<(), ClientError> {
    match settings.surface {
        Surface::Main => client.append(&ctx.turn, ctx.epoch, msg).await,
        Surface::Thought => client.append_thought(&ctx.turn, msg).await,
    }
}

/// What a turn cost, carried together so the three of them cannot drift apart.
#[derive(Debug, Clone, Copy, Default)]
struct Cost {
    tools: u32,
    tokens: u32,
    thinking: f64,
}

async fn report(
    client: &mut Client,
    settings: &Settings,
    ctx: &TurnContext,
    final_text: String,
    flags: Flags,
    cost: Cost,
    started: std::time::Instant,
) -> Result<TurnOutcome, ClientError> {
    let out = TurnOutcome {
        turn: ctx.turn.clone(),
        epoch: ctx.epoch,
        final_text,
        flags: flags.to_list(),
        tools_used: cost.tools,
        seconds: started.elapsed().as_secs_f64(),
        tokens: cost.tokens,
        thinking_seconds: cost.thinking,
    };
    match settings.surface {
        Surface::Main => client.end(&out).await?,
        Surface::Thought => client.end_thought(&ctx.turn, &out.final_text, &out.flags).await?,
    }
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
        other => ToolResult::bad(unknown_tool(other, settings)),
    }
}

/// What to say when it calls something that is not a tool.
///
/// Almost always it has reached for one of its own skills, which are commands and not tools.
/// Saying only that the tool does not exist leaves it to guess, and a small model guesses the
/// same thing again; saying how to run the very thing it wanted ends the loop.
fn unknown_tool(name: &str, settings: &Settings) -> String {
    let have = tools::names(settings.surface).join(", ");
    if is_a_command(name, &settings.home) {
        return format!(
            "`{name}` is one of your commands, not a tool. Run it in your shell, as \
shell with a command of \"{name} ...\". Your tools are: {have}."
        );
    }
    format!("there is no tool called `{name}`. You have: {have}")
}

/// Whether the mind has a command by that name, in its own bin directory or on its path.
fn is_a_command(name: &str, home: &std::path::Path) -> bool {
    if name.is_empty() || name.contains('/') {
        return false;
    }
    let mut dirs: Vec<std::path::PathBuf> = vec![home.join("bin")];
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.iter().any(|d| {
        let p = d.join(name);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(&p)
                .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            p.is_file()
        }
    })
}

/// Which thought this process is running, as the core told it when it started it.
fn thought_id() -> String {
    std::env::var("GROOW_THOUGHT").unwrap_or_default()
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
        /// Events the turn announced on its own account.
        emitted: Vec<(String, serde_json::Value)>,
        outcome: Option<TurnOutcome>,
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
                // An event is announced, not asked, so it has no reply and is only recorded.
                if let Some((name, _, data)) = f.event() {
                    log.lock().unwrap().emitted.push((name.to_string(), data));
                    continue;
                }
                let Some((id, op, arg)) = f.request() else { continue };
                let (op, arg) = (op.to_string(), arg);
                let reply = match op.as_str() {
                    "turn.claim" | "thought.claim" => Frame::rep(id, serde_json::to_value(&ctx).unwrap()),
                    "thought.append" => {
                        let m: Message = serde_json::from_value(arg["message"].clone()).unwrap();
                        log.lock().unwrap().appended.push(m);
                        Frame::rep(id, json!({"ok": true}))
                    }
                    "thought.end" => {
                        log.lock().unwrap().outcome = Some(TurnOutcome {
                            turn: arg["id"].as_str().unwrap_or("").to_string(),
                            epoch: 0,
                            final_text: arg["final_text"].as_str().unwrap_or("").to_string(),
                            flags: arg["flags"].as_array().map(|a| a.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string())).collect()).unwrap_or_default(),
                            tools_used: 0,
                            seconds: 0.0,
                            tokens: 0,
                            thinking_seconds: 0.0,
                        });
                        Frame::rep(id, json!({"ok": true}))
                    }
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
            epoch: 3,
            kind: SignalKind::SignalUser as i32,
            text: "what is in this folder?".into(),
            framed: "what is in this folder?".into(),
            system: "You are Groow.".into(),
            window: vec![],
            max_rounds,
            meta: None,
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
    async fn the_turn_does_not_record_what_came_in() {
        // The core writes that once, when it opens the turn. A retried turn that wrote it
        // again would leave the same message in the conversation several times over.
        let (_out, log, _d) = run(vec!["ok".into()], 10).await;
        let appended = &log.lock().unwrap().appended;
        assert!(
            !appended.iter().any(|m| m.role == "user"),
            "the turn wrote the incoming message itself: {appended:?}"
        );
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
    async fn a_call_the_model_wrote_without_tags_still_runs() {
        let (out, log, _d) = run(
            vec![
                "{\"name\": \"shell\", \"arguments\": {\"command\": \"echo untagged\"}}".into(),
                "It said untagged.".into(),
            ],
            10,
        )
        .await;
        assert_eq!(out.tools_used, 1, "the call was read as prose instead of run");
        let l = log.lock().unwrap();
        assert!(l.appended.iter().any(|m| m.text().contains("untagged")));
    }

    #[tokio::test]
    async fn calling_a_skill_as_a_tool_is_answered_with_how_to_run_it() {
        // What the real model does: it sees `web` in its prompt and calls it as a tool. Being
        // told only that no such tool exists sends it round the same loop again.
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let cmd = bin.join("web");
        std::fs::write(&cmd, "#!/bin/sh\necho page\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cmd, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let settings = Settings {
            home: d.path().to_path_buf(),
            tool_timeout: Duration::from_secs(5),
            surface: Surface::Main,
        };
        let said = unknown_tool("web", &settings);
        assert!(said.contains("not a tool"), "{said}");
        assert!(said.contains("in your shell"), "it must show how to run it: {said}");
        assert!(said.contains("web"), "{said}");
    }

    #[tokio::test]
    async fn something_that_is_not_a_command_either_is_simply_refused() {
        let d = tempfile::tempdir().unwrap();
        let settings = Settings {
            home: d.path().to_path_buf(),
            tool_timeout: Duration::from_secs(5),
            surface: Surface::Main,
        };
        let said = unknown_tool("rm_rf_everything", &settings);
        assert!(said.contains("there is no tool"), "{said}");
        assert!(said.contains("shell, think"), "{said}");
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

    async fn run_as(
        gens: Vec<String>,
        surface: Surface,
    ) -> (TurnOutcome, Arc<Mutex<Recorded>>, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("core.sock");
        let log = fake_core(p.clone(), ctx(10), gens).await;
        let mut c = Client::connect(&p).await.unwrap();
        let s = Settings { home: d.path().to_path_buf(), tool_timeout: Duration::from_secs(5), surface };
        let out = run_turn(&mut c, &s).await.unwrap();
        (out, log, d)
    }

    #[tokio::test]
    async fn a_thought_works_on_its_own_trace() {
        std::env::set_var("GROOW_THOUGHT", "ab12cd");
        let (out, log, _d) = run_as(vec!["I have found it.".into()], Surface::Thought).await;
        assert_eq!(out.final_text, "I have found it.");
        let appended = &log.lock().unwrap().appended;
        // Unlike a conscious turn, a thought records the step prompt it was given, because
        // that prompt is part of its own working history and nothing else records it.
        assert_eq!(appended[0].role, "user");
        assert!(appended.iter().any(|m| m.role == "assistant"));
        std::env::remove_var("GROOW_THOUGHT");
    }

    #[tokio::test]
    async fn a_thought_finishing_reports_through_its_own_contract() {
        std::env::set_var("GROOW_THOUGHT", "ab12cd");
        let (_out, log, _d) = run_as(
            vec!["<tool_call>{\"name\":\"finish\",\"arguments\":{\"summary\":\"eight files\"}}</tool_call>".into()],
            Surface::Thought,
        ).await;
        let o = log.lock().unwrap().outcome.clone().expect("the thought never closed its step");
        assert_eq!(o.final_text, "eight files");
        std::env::remove_var("GROOW_THOUGHT");
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
