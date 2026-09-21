//! What the core's answers look like to a person.
//!
//! Every command here is one request over the socket and nothing else; this is only how the
//! answer is drawn. `--json` prints the reply itself, which is what a script wants. Nothing in
//! this module asks the core for anything extra: if something is not in the reply, it is not
//! on the screen.
//!
//! The house style is the same throughout. A box for a state of affairs, because it is a thing
//! you look at; a list for a collection, because you read down it and then act on one of them,
//! and every entry shows the id you would need to do that. Times are shown as how long ago,
//! since that is the question you are actually asking.

use serde_json::Value;

use crate::args::Command;

/// Draw the reply, or say there is nothing to draw and let the caller print the JSON.
pub fn render(cmd: &Command, v: &Value) -> Option<String> {
    Some(match cmd {
        Command::Status => box_of(v),
        Command::Thoughts => thoughts_list(v),
        Command::Schedule { action, .. } if action == "list" => alarms(v),
        Command::Recall { .. } => conversation(v),
        _ => return None,
    })
}

/// What it is working on by itself.
fn thoughts_list(v: &Value) -> String {
    let all = arr(v, "thoughts");
    if all.is_empty() {
        return "It is not working on anything of its own just now.".into();
    }
    let running = num(v, "running");
    let max = num(v, "max");
    let mut out = format!("{} of its own, {running} running", all.len());
    if max > 0 {
        out.push_str(&format!(" of {max} it may have at once"));
    }
    out.push_str(":
");
    for t in all {
        let steps = match (num(&t, "steps"), num(&t, "max_steps")) {
            (s, m) if m > 0 => format!("{s}/{m} steps"),
            (s, _) => format!("{s} steps"),
        };
        out.push('\n');
        out.push_str(&format!(
            "  {}  {:<8} {}
          {}
",
            id_of(&t),
            str_of(&t, "status").unwrap_or_default(),
            steps,
            str_of(&t, "goal").unwrap_or_default()
        ));
        if let Some(s) = str_of(&t, "summary") {
            out.push_str(&format!("          {s}
"));
        }
    }
    out.push_str("
Read one with `groow thought read <id>`.");
    out
}

/// The alarms it has set, and the ones you have set for it.
fn alarms(v: &Value) -> String {
    let all = arr(v, "alarms");
    if all.is_empty() {
        return "Nothing is set to wake it.".into();
    }
    let mut out = match all.len() {
        1 => "One alarm:
".to_string(),
        n => format!("{n} alarms:
"),
    };
    for a in all {
        let every = match (a.get("repeat_s").and_then(|r| r.as_f64()), str_of(&a, "repeat_daily")) {
            (_, Some(d)) => format!(", daily at {d}"),
            (Some(s), _) if s > 0.0 => format!(", every {}", duration(s)),
            _ => String::new(),
        };
        let off = if a.get("enabled").and_then(|e| e.as_bool()) == Some(false) { " (off)" } else { "" };
        out.push('\n');
        out.push_str(&format!(
            "  {}  {}
          {}{}{}, set by {}
",
            id_of(&a),
            str_of(&a, "text").unwrap_or_default(),
            due(&a, "when"),
            every,
            off,
            str_of(&a, "by").unwrap_or_else(|| "?".into())
        ));
    }
    out.push_str("
Drop one with `groow schedule remove <id>`.");
    out
}

/// The conversation, read back.
fn conversation(v: &Value) -> String {
    let ms = arr(v, "messages");
    if ms.is_empty() {
        return "Nothing has been said yet.".into();
    }
    let mut out = String::new();
    let mut last_turn = String::new();
    for m in ms {
        // A blank line between turns, so a long exchange reads as exchanges rather than lines.
        let turn = str_of(&m, "turn").unwrap_or_default();
        if turn != last_turn && !out.is_empty() {
            out.push('\n');
        }
        last_turn = turn;

        let who = str_of(&m, "role").unwrap_or_default();
        let mut body = str_of(&m, "content").unwrap_or_default();
        // What a command printed can be a hundred lines, and then the exchange around it is
        // unreadable. It is clipped here and nowhere else: `--json` still has all of it.
        if who == "tool" {
            body = clip(&body, TOOL_LINES);
        }
        if !body.is_empty() {
            out.push_str(&wrapped(&who, &body));
        }
        for call in m.get("tool_calls").and_then(|c| c.as_array()).into_iter().flatten() {
            let name = str_of(call, "name").unwrap_or_default();
            let args = call.get("arguments").map(compact).unwrap_or_default();
            out.push_str(&wrapped("", &format!("{name} {args}")));
        }
    }
    out.trim_end().to_string()
}

/// How much of a command's output is worth reading back in the conversation.
const TOOL_LINES: usize = 8;

/// Keep the first few lines and say how many were left.
fn clip(body: &str, keep: usize) -> String {
    let lines: Vec<&str> = body.lines().collect();
    if lines.len() <= keep {
        return body.to_string();
    }
    let rest = lines.len() - keep;
    format!("{}\n… {rest} more lines (`--json` for all of it)", lines[..keep].join("\n"))
}

/// One speaker's line, with everything after the first line hanging under it.
fn wrapped(who: &str, body: &str) -> String {
    const INDENT: &str = "             ";
    let mut out = format!("  {who:<11}");
    for (i, line) in body.lines().enumerate() {
        if i > 0 {
            out.push_str(INDENT);
        }
        out.push_str(line);
        out.push('\n');
    }
    if body.lines().next().is_none() {
        out.push('\n');
    }
    out
}

/// A tool call's arguments on one line, which is how they are read.
fn compact(v: &Value) -> String {
    match v {
        Value::Object(m) => m
            .values()
            .map(|x| x.as_str().map(|s| s.to_string()).unwrap_or_else(|| x.to_string()))
            .collect::<Vec<_>>()
            .join(" "),
        other => other.to_string(),
    }
}

fn arr(v: &Value, k: &str) -> Vec<Value> {
    v.get(k).and_then(|x| x.as_array()).cloned().unwrap_or_default()
}

/// The id, which is the thing you type next, so it is the first thing on the line.
fn id_of(v: &Value) -> String {
    str_of(v, "id").unwrap_or_else(|| "??????".into())
}

/// When something is next due, which may be in the past if it is overdue.
fn due(v: &Value, k: &str) -> String {
    match v.get(k).and_then(|t| t.as_f64()) {
        Some(ts) if ts >= now() => format!("in {}", duration(ts - now())),
        Some(ts) => format!("overdue by {}", duration(now() - ts)),
        None => "no time set".into(),
    }
}

/// Seconds as something a person says out loud.
fn duration(s: f64) -> String {
    // Rounded, not truncated: two hours away is computed a few microseconds after it was read,
    // and "1h 59m" for something set exactly two hours out is a needless lie.
    let s = (s.max(0.0) + 0.5) as u64;
    match s {
        0..=90 => format!("{s}s"),
        91..=5400 => format!("{}m", s / 60),
        5401..=172_800 => format!("{}h {}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d {}h", s / 86400, (s % 86400) / 3600),
    }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The narrowest the box is allowed to be, so it does not jump about between calls.
const MIN_WIDTH: usize = 52;

/// Draw the reply to a `status` request.
pub fn box_of(v: &Value) -> String {
    let rows = rows(v);
    let label = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    let age = v.get("age").and_then(|a| a.as_str()).unwrap_or("").to_string();

    let body: Vec<String> =
        rows.iter().map(|(k, t)| format!("  {k:label$}   {t}", label = label)).collect();
    let inner = body
        .iter()
        .map(|l| l.chars().count())
        .chain(std::iter::once(age.chars().count() + 12))
        .max()
        .unwrap_or(MIN_WIDTH)
        .max(MIN_WIDTH)
        + 2;

    let mut out = String::new();
    // The header carries the age, because it is the one number that is true of the creature
    // rather than of this moment.
    let head = if age.is_empty() { String::new() } else { format!(" {age} old ─") };
    let dashes = inner.saturating_sub(head.chars().count() + 8);
    out.push_str(&format!("╭─ groow {}{}╮\n", "─".repeat(dashes), head));
    out.push_str(&format!("│{}│\n", " ".repeat(inner)));
    for line in &body {
        let pad = inner.saturating_sub(line.chars().count());
        out.push_str(&format!("│{line}{}│\n", " ".repeat(pad)));
    }
    out.push_str(&format!("│{}│\n", " ".repeat(inner)));
    out.push_str(&format!("╰{}╯", "─".repeat(inner)));
    out
}

/// The same rows every time, so the box does not change shape while you watch it.
fn rows(v: &Value) -> Vec<(&'static str, String)> {
    vec![
        ("state", str_of(v, "state").unwrap_or_else(|| "?".into())),
        ("doing", doing(v)),
        ("feeling", feeling(v)),
        ("brain", brain(v)),
        ("turns", turns(v)),
        ("thoughts", thoughts(v)),
    ]
}

/// The mood, which is the one thing here that is actually a mood: what the last while felt
/// like. Whether it is awake or asleep is its state, and lives a row above.
fn feeling(v: &Value) -> String {
    let f = v.get("feeling");
    let tone = f.and_then(|f| f.get("tone")).and_then(|t| t.as_str()).unwrap_or("");
    let pain = f.and_then(|f| f.get("pain")).and_then(|p| p.as_f64()).unwrap_or(0.0);
    let joy = f.and_then(|f| f.get("pleasure")).and_then(|p| p.as_f64()).unwrap_or(0.0);
    let mut s = if tone.is_empty() { "unknown".to_string() } else { tone.to_string() };
    // Only when there is something to say: an even day should not be a row of zeroes.
    if pain > 0.0 || joy > 0.0 {
        s.push_str(&format!(" (pleasure {joy:.2}, pain {pain:.2})"));
    }
    s
}

fn doing(v: &Value) -> String {
    let queue = num(v, "queue");
    let waiting = match queue {
        0 => String::new(),
        1 => " · one message waiting".into(),
        n => format!(" · {n} messages waiting"),
    };
    let what = if let Some(n) = str_of(v, "napping") {
        format!("napping ({n})")
    } else if let Some(t) = str_of(v, "turn") {
        format!("a turn, {t}")
    } else if v.get("busy").and_then(|b| b.as_bool()).unwrap_or(false) {
        "thinking".into()
    } else {
        "nothing just now".into()
    };
    format!("{what}{waiting}")
}

fn brain(v: &Value) -> String {
    match v.get("brain").and_then(|b| b.as_bool()) {
        Some(true) => "loaded and answering".into(),
        Some(false) => "not loaded yet, so a turn would fail".into(),
        None => "unknown".into(),
    }
}

fn turns(v: &Value) -> String {
    match num(v, "turns") {
        0 => "none yet".into(),
        1 => "one so far".into(),
        n => format!("{n} so far"),
    }
}

fn thoughts(v: &Value) -> String {
    match num(v, "thoughts") {
        0 => "none of its own just now".into(),
        1 => "one of its own (`groow thoughts`)".into(),
        n => format!("{n} of its own (`groow thoughts`)"),
    }
}

/// A string field, treating null and empty as absent, which is how the core sends "no".
fn str_of(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string())
}

fn num(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(|x| x.as_i64()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn idle() -> Value {
        json!({
            "brain": true, "busy": false, "age": "22h 16m", "napping": null,
            "idle_streak": 1, "queue": 0, "thoughts": 0,
            "feeling": {"pain": 0, "tone": "even", "pleasure": 0},
            "turn": null, "state": "idle",
            "born": 1789731096.238, "turns": 38
        })
    }

    fn cmd(args: &[&str]) -> Command {
        use clap::Parser;
        crate::args::Cli::try_parse_from(args).unwrap().cmd
    }

    #[test]
    fn an_acknowledgement_is_left_as_it_came() {
        // Only a listing is worth drawing; `say` answers with an ok, and there is nothing to
        // make of that which the JSON does not already say.
        assert!(render(&cmd(&["groow", "say", "hello"]), &json!({"ok": true})).is_none());
        assert!(render(&cmd(&["groow", "stop"]), &json!({"ok": true})).is_none());
    }

    #[test]
    fn a_thought_shows_how_far_along_it_is() {
        let v = json!({"running": 1, "max": 4, "thoughts": [
            {"id": "ac59aa", "goal": "learn what a socket is", "status": "running", "steps": 3, "max_steps": 12},
            {"id": "b17c02", "goal": "tidy my notes", "status": "done", "steps": 8, "max_steps": 8,
             "summary": "there was nothing to tidy"},
        ]});
        let s = render(&cmd(&["groow", "thoughts"]), &v).unwrap();
        assert!(s.contains("2 of its own, 1 running of 4"), "{s}");
        assert!(s.contains("3/12 steps"), "{s}");
        assert!(s.contains("there was nothing to tidy"), "{s}");
        assert!(s.contains("groow thought read <id>"), "{s}");
    }

    #[test]
    fn an_alarm_says_when_it_is_next_due_rather_than_a_timestamp() {
        let v = json!({"alarms": [
            {"id": "0aea53", "text": "check the news", "when": now() + 7200.0, "enabled": true,
             "by": "groow", "repeat_s": 10800, "repeat_daily": null},
            {"id": "c59f2e", "text": "an old one", "when": now() - 600.0, "enabled": false,
             "by": "mentor", "repeat_s": 0, "repeat_daily": null},
        ]});
        let s = render(&cmd(&["groow", "schedule"]), &v).unwrap();
        assert!(s.contains("in 2h 0m, every 3h 0m, set by groow"), "{s}");
        assert!(s.contains("overdue by 10m"), "{s}");
        assert!(s.contains("(off)"), "a disabled alarm should say so: {s}");
    }

    #[test]
    fn a_daily_alarm_says_the_hour_rather_than_the_interval() {
        let v = json!({"alarms": [{"id": "a1", "text": "the news", "when": now() + 60.0,
                                   "repeat_daily": "08:00", "repeat_s": 86400, "by": "mentor"}]});
        assert!(render(&cmd(&["groow", "schedule"]), &v).unwrap().contains("daily at 08:00"));
    }

    #[test]
    fn the_conversation_reads_as_exchanges() {
        let v = json!({"messages": [
            {"role": "user", "content": "what is in this directory?", "turn": "t1"},
            {"role": "assistant", "content": "", "turn": "t1",
             "tool_calls": [{"name": "shell", "arguments": {"command": "ls -1 | wc -l"}}]},
            {"role": "tool", "name": "shell", "content": "6", "turn": "t1"},
            {"role": "assistant", "content": "There are 6 entries.", "turn": "t1"},
            {"role": "user", "content": "thank you", "turn": "t2"},
        ]});
        let s = render(&cmd(&["groow", "recall"]), &v).unwrap();
        assert!(s.contains("  user       what is in this directory?"), "{s}");
        assert!(s.contains("shell ls -1 | wc -l"), "a call shows what it ran: {s}");
        assert!(s.contains("\n\n  user       thank you"), "a new turn starts a new block: {s}");
    }

    #[test]
    fn a_command_that_printed_a_wall_of_text_does_not_bury_the_exchange() {
        let wall = (1..=40).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let v = json!({"messages": [
            {"role": "tool", "name": "shell", "content": wall, "turn": "t1"},
            {"role": "assistant", "content": "so there are forty of them", "turn": "t1"},
        ]});
        let s = render(&cmd(&["groow", "recall"]), &v).unwrap();
        assert!(s.contains("line 8"), "{s}");
        assert!(!s.contains("line 9"), "{s}");
        assert!(s.contains("… 32 more lines (`--json` for all of it)"), "{s}");
        assert!(s.contains("so there are forty of them"), "what was said after it must survive: {s}");
    }

    #[test]
    fn what_it_said_itself_is_never_clipped() {
        let long = (1..=40).map(|i| format!("thought {i}")).collect::<Vec<_>>().join("\n");
        let v = json!({"messages": [{"role": "assistant", "content": long, "turn": "t1"}]});
        let s = render(&cmd(&["groow", "recall"]), &v).unwrap();
        assert!(s.contains("thought 40"), "only command output is clipped: {s}");
    }

    #[test]
    fn a_long_answer_hangs_under_the_speaker() {
        let v = json!({"messages": [{"role": "assistant", "content": "first line\nsecond line", "turn": "t1"}]});
        let s = render(&cmd(&["groow", "recall"]), &v).unwrap();
        assert!(s.contains("  assistant  first line\n             second line"), "{s:?}");
    }

    #[test]
    fn the_box_says_what_the_json_said() {
        let s = box_of(&idle());
        assert!(s.contains("22h 16m old"), "{s}");
        assert!(s.contains("idle"), "{s}");
        assert!(s.contains("even"), "and what it feels, separately: {s}");
        assert!(s.contains("nothing just now"), "{s}");
        assert!(s.contains("loaded and answering"), "{s}");
        assert!(s.contains("38 so far"), "{s}");
    }

    #[test]
    fn every_line_of_the_box_is_the_same_width() {
        // Including the ones with box-drawing characters, which are not one byte each.
        for v in [idle(), json!({}), json!({"state": "thinking", "turn": "01789810553254-0000",
                                            "busy": true, "queue": 3, "age": "3d 4h"})] {
            let s = box_of(&v);
            let widths: Vec<usize> = s.lines().map(|l| l.chars().count()).collect();
            assert!(widths.windows(2).all(|w| w[0] == w[1]), "{s}\n{widths:?}");
        }
    }

    #[test]
    fn a_missing_field_does_not_make_a_hole_in_it() {
        let s = box_of(&json!({}));
        assert!(s.contains("unknown"), "the brain is not assumed to be up: {s}");
        assert!(s.contains("none yet"), "{s}");
    }

    #[test]
    fn what_it_is_doing_comes_before_what_is_waiting_for_it() {
        let s = box_of(&json!({"turn": "01789-0000", "queue": 2, "state": "thinking"}));
        assert!(s.contains("a turn, 01789-0000 · 2 messages waiting"), "{s}");
        let s = box_of(&json!({"napping": "since 03:00", "state": "sleeping"}));
        assert!(s.contains("napping (since 03:00)"), "{s}");
    }

    #[test]
    fn a_feeling_is_only_spelled_out_when_there_is_one() {
        let s = box_of(&idle());
        assert!(!s.contains("pain 0"), "an even day is not a row of zeroes: {s}");
        let hurt = json!({"state": "idle", "feeling": {"tone": "low", "pain": 0.4, "pleasure": 0.1}});
        assert!(box_of(&hurt).contains("(pleasure 0.10, pain 0.40)"));
    }
}
