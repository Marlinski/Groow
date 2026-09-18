//! The tool surface, kept deliberately small.
//!
//! Almost everything the mind can do, it does through a shell in its own home: reading files,
//! installing things, running its own skills, and reaching its own body through `groow`
//! commands. A shell gives it exit statuses and error messages, which are things it can learn
//! from, where a wrapper tool would give it a tidy result and teach it nothing.
//!
//! What is left is what a shell cannot express: putting a question to its mentor, setting work
//! aside for later, and, inside an inner thought, reaching back to the main thread.

use std::collections::BTreeMap;
use std::time::Duration;

use groow_proto::turn::ToolSchema;
use serde_json::{json, Value};

/// The outcome of running a tool. The exit status matters as much as the text: it is the
/// signal that says whether this went well, without anyone having to say so.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub text: String,
    pub ok: bool,
    pub seconds: f64,
}

impl ToolResult {
    pub fn good(text: impl Into<String>) -> ToolResult {
        ToolResult { text: text.into(), ok: true, seconds: 0.0 }
    }
    pub fn bad(text: impl Into<String>) -> ToolResult {
        ToolResult { text: text.into(), ok: false, seconds: 0.0 }
    }
}

/// Which tools a process gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// The conscious thread: a shell, a way to ask, and a way to set work aside.
    Main,
    /// An inner thought: a shell, and its two ways back to the main thread.
    Thought,
}

/// The longest tool output that goes into the conversation. Beyond this the middle is cut,
/// because the beginning and the end of a command's output are where the meaning usually is.
const MAX_OUTPUT: usize = 6000;

pub fn schemas(surface: Surface) -> Vec<ToolSchema> {
    let shell = ToolSchema {
        name: "shell".into(),
        description: "Run a shell command in your home directory and read what it prints. \
This is how you do almost everything: look at files, try things out, run your own skills, and \
use `groow` commands to reach your own alarms, thoughts and questions. You see the exit status, \
so a failure is information, not a dead end."
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "the command line to run"},
            },
            "required": ["command"],
        }),
    };
    match surface {
        Surface::Main => vec![
            shell,
            ToolSchema {
                name: "ask".into(),
                description: "Put a question to Marlinski, your mentor, and carry on. He may \
not answer, and you can only have a few questions open at once, so ask about things you cannot \
find out for yourself."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "question": {"type": "string", "description": "what you want to know"},
                        "context": {"type": "string", "description": "why you are asking, briefly"},
                    },
                    "required": ["question"],
                }),
            },
            ToolSchema {
                name: "think".into(),
                description: "Set a piece of work aside to run on its own while you carry on. \
It works alone and comes back to you when it has something worth saying."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "goal": {"type": "string", "description": "what the thought should achieve"},
                        "max_steps": {"type": "integer", "description": "how many steps it may take"},
                    },
                    "required": ["goal"],
                }),
            },
        ],
        Surface::Thought => vec![
            shell,
            ToolSchema {
                name: "focus".into(),
                description: "Tell the main thread something it needs now, without waiting \
until you are finished."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {"message": {"type": "string"}},
                    "required": ["message"],
                }),
            },
            ToolSchema {
                name: "finish".into(),
                description: "You have reached the goal. Say what you found, in a sentence or \
two, and stop."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {"summary": {"type": "string"}},
                    "required": ["summary"],
                }),
            },
        ],
    }
}

pub fn names(surface: Surface) -> Vec<String> {
    schemas(surface).into_iter().map(|s| s.name).collect()
}

/// Run a shell command and report what happened, including how it ended.
pub async fn run_shell(command: &str, home: &std::path::Path, timeout: Duration) -> ToolResult {
    let started = std::time::Instant::now();
    if command.trim().is_empty() {
        return ToolResult::bad("there was no command to run");
    }
    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ToolResult::bad(format!("the command could not be started: {e}")),
    };
    let waited = tokio::time::timeout(timeout, child.wait_with_output()).await;
    let seconds = started.elapsed().as_secs_f64();

    match waited {
        Ok(Ok(out)) => {
            let mut text = String::new();
            text.push_str(&String::from_utf8_lossy(&out.stdout));
            let err = String::from_utf8_lossy(&out.stderr);
            if !err.trim().is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&err);
            }
            let code = out.status.code();
            let ok = out.status.success();
            let mut text = clip(text.trim_end(), MAX_OUTPUT);
            if text.is_empty() {
                text = if ok { "(no output)".into() } else { "(no output)".into() };
            }
            let text = match code {
                Some(0) => text,
                Some(c) => format!("{text}\n[exit {c}]"),
                None => format!("{text}\n[the command was stopped by a signal]"),
            };
            ToolResult { text, ok, seconds }
        }
        Ok(Err(e)) => ToolResult { text: format!("the command failed: {e}"), ok: false, seconds },
        Err(_) => ToolResult {
            text: format!("the command was still running after {:.0}s and was stopped", timeout.as_secs_f64()),
            ok: false,
            seconds,
        },
    }
}

/// Shorten long output by cutting the middle, which is usually the least informative part.
pub fn clip(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let head: String = chars[..max * 2 / 3].iter().collect();
    let tail: String = chars[chars.len() - max / 3..].iter().collect();
    let cut = chars.len() - max;
    format!("{head}\n\n[\u{2026} {cut} characters left out \u{2026}]\n\n{tail}")
}

/// Pull the named string argument out of a call, tolerating the model quoting a number.
pub fn arg_str(args: &Value, key: &str) -> String {
    match args.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string().trim_matches('"').to_string(),
    }
}

pub fn arg_u32(args: &Value, key: &str, default: u32) -> u32 {
    match args.get(key) {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(default as u64) as u32,
        Some(Value::String(s)) => s.trim().parse().unwrap_or(default),
        _ => default,
    }
}

/// A count of how often each tool has been called this turn, used to notice a loop.
pub type Counts = BTreeMap<String, u32>;

/// Whether this exact call has been made before in the same turn. Repeating a call verbatim is
/// the clearest sign that a turn has stopped making progress.
pub fn is_repeat(seen: &mut Vec<String>, name: &str, args: &Value) -> bool {
    let key = format!("{name}:{}", canonical(args));
    let repeat = seen.contains(&key);
    seen.push(key);
    repeat
}

/// A stable rendering of arguments, so that key order cannot make two identical calls look
/// different.
fn canonical(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let sorted: BTreeMap<_, _> = m.iter().collect();
            let inner: Vec<String> = sorted.iter().map(|(k, v)| format!("{k}={}", canonical(v))).collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(a) => format!("[{}]", a.iter().map(canonical).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_surface_is_small_and_differs_by_who_is_thinking() {
        assert_eq!(names(Surface::Main), ["shell", "ask", "think"]);
        assert_eq!(names(Surface::Thought), ["shell", "focus", "finish"]);
    }

    #[test]
    fn a_thought_cannot_ask_the_mentor_or_spawn_more_thoughts() {
        let n = names(Surface::Thought);
        assert!(!n.contains(&"ask".to_string()), "only the conscious thread talks to a person");
        assert!(!n.contains(&"think".to_string()), "thoughts must not breed");
    }

    #[test]
    fn every_schema_is_a_well_formed_object_with_required_fields() {
        for s in [Surface::Main, Surface::Thought] {
            for t in schemas(s) {
                assert!(!t.description.trim().is_empty(), "{} has no description", t.name);
                assert_eq!(t.parameters["type"], "object", "{} is not an object", t.name);
                let req = t.parameters["required"].as_array().expect("required list");
                for r in req {
                    let key = r.as_str().unwrap();
                    assert!(t.parameters["properties"].get(key).is_some(),
                        "{} requires {key} but does not describe it", t.name);
                }
            }
        }
    }

    #[tokio::test]
    async fn a_command_that_works_reports_its_output_and_succeeds() {
        let d = tempfile::tempdir().unwrap();
        let r = run_shell("echo hello there", d.path(), Duration::from_secs(5)).await;
        assert!(r.ok);
        assert_eq!(r.text, "hello there");
    }

    #[tokio::test]
    async fn a_command_that_fails_says_so_and_keeps_the_reason() {
        let d = tempfile::tempdir().unwrap();
        let r = run_shell("echo 'no such thing' >&2; exit 3", d.path(), Duration::from_secs(5)).await;
        assert!(!r.ok, "a failure must be visible as a failure");
        assert!(r.text.contains("no such thing"), "the reason was lost: {}", r.text);
        assert!(r.text.contains("[exit 3]"));
    }

    #[tokio::test]
    async fn a_command_runs_in_the_home_it_was_given() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("marker.txt"), "x").unwrap();
        let r = run_shell("ls", d.path(), Duration::from_secs(5)).await;
        assert!(r.text.contains("marker.txt"));
    }

    #[tokio::test]
    async fn a_command_that_hangs_is_stopped_and_reported() {
        let d = tempfile::tempdir().unwrap();
        let r = run_shell("sleep 30", d.path(), Duration::from_millis(300)).await;
        assert!(!r.ok);
        assert!(r.text.contains("stopped"), "unhelpful: {}", r.text);
        assert!(r.seconds < 5.0, "it was not actually stopped");
    }

    #[tokio::test]
    async fn a_silent_command_still_says_something() {
        let d = tempfile::tempdir().unwrap();
        let r = run_shell("true", d.path(), Duration::from_secs(5)).await;
        assert!(r.ok);
        assert_eq!(r.text, "(no output)");
    }

    #[tokio::test]
    async fn an_empty_command_is_refused_rather_than_run() {
        let d = tempfile::tempdir().unwrap();
        let r = run_shell("   ", d.path(), Duration::from_secs(5)).await;
        assert!(!r.ok);
    }

    #[test]
    fn very_long_output_keeps_both_ends() {
        let long = (0..5000).map(|i| format!("line {i}\n")).collect::<String>();
        let c = clip(&long, 1000);
        assert!(c.chars().count() < long.chars().count());
        assert!(c.starts_with("line 0"), "the start was lost");
        assert!(c.trim_end().ends_with("line 4999"), "the end was lost: {}", &c[c.len().saturating_sub(40)..]);
        assert!(c.contains("left out"));
    }

    #[test]
    fn short_output_is_untouched() {
        assert_eq!(clip("brief", 1000), "brief");
    }

    #[test]
    fn a_repeated_call_is_noticed_however_the_arguments_are_ordered() {
        let mut seen = Vec::new();
        assert!(!is_repeat(&mut seen, "shell", &json!({"command": "ls", "extra": 1})));
        assert!(is_repeat(&mut seen, "shell", &json!({"extra": 1, "command": "ls"})),
            "key order must not disguise a repeat");
        assert!(!is_repeat(&mut seen, "shell", &json!({"command": "pwd"})));
    }

    #[test]
    fn arguments_are_read_leniently_but_never_invented() {
        assert_eq!(arg_str(&json!({"a": "x"}), "a"), "x");
        assert_eq!(arg_str(&json!({"a": 7}), "a"), "7");
        assert_eq!(arg_str(&json!({}), "a"), "");
        assert_eq!(arg_u32(&json!({"n": 5}), "n", 8), 5);
        assert_eq!(arg_u32(&json!({"n": "5"}), "n", 8), 5);
        assert_eq!(arg_u32(&json!({}), "n", 8), 8);
        assert_eq!(arg_u32(&json!({"n": "lots"}), "n", 8), 8);
    }
}
