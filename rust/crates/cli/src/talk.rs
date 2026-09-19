//! The commands that are questions or instructions for a running core.
//!
//! Each one is a single request over the socket. What comes back is printed as JSON on
//! standard output, so these compose with other tools, while anything a person needs to read
//! goes to standard error.
//!
//! Except that a person typing one of these wants to read the answer, not parse it, so where
//! there is a good way to draw a reply it is drawn: see `show`. `--json` turns that off and
//! prints the reply, which is all the renderer ever had to work with anyway.

use groow_ui::link::{FromCore, Link};
use serde_json::{json, Value};

use crate::args::Command;

pub async fn talk(cmd: Command, state: std::path::PathBuf, json: bool) -> anyhow::Result<()> {
    let (op, arg) = request_for(&cmd)?;
    let socket = state.join("core.sock");
    let (mut link, mut rx) = match Link::open(&socket).await {
        Ok(v) => v,
        Err(e) => {
            // Pointing at the sandbox's state while its body is asleep is the likeliest way to
            // get here, and "start it" is not obviously the answer unless it is said.
            if crate::sandbox::is_the_sandboxes(state.parent().unwrap_or(&state)) {
                anyhow::bail!(
                    "that one lives in the sandbox and its body is not running. `make start` wakes it."
                )
            }
            anyhow::bail!(
                "nothing is listening at {} ({e}). `groow start` runs the core here; \
`make start` wakes it in its container.",
                socket.display()
            )
        }
    };
    let id = link.send(&op, arg).await?;

    // Wait for the answer to this request, ignoring the event traffic on the way past.
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv()).await {
            Err(_) => anyhow::bail!("the core did not answer within thirty seconds"),
            Ok(None) | Ok(Some(FromCore::Gone)) => anyhow::bail!("the core closed the connection"),
            Ok(Some(FromCore::Reply(got, v))) if got == id => {
                match crate::show::render(&cmd, &v).filter(|_| !json) {
                    Some(drawn) => println!("{drawn}"),
                    None => {
                        println!("{}", serde_json::to_string_pretty(&v)?);
                        // The drawn answers say this themselves, and better.
                        if let Some(note) = human_note(&cmd, &v) {
                            eprintln!("{note}");
                        }
                    }
                }
                return Ok(());
            }
            Ok(Some(FromCore::Failed(got, why))) if got == id => anyhow::bail!("{why}"),
            Ok(Some(_)) => {}
        }
    }
}

/// Turn a command into the one request that carries it out.
pub fn request_for(cmd: &Command) -> anyhow::Result<(String, Value)> {
    Ok(match cmd {
        Command::Status => ("status".into(), json!({})),
        Command::Stop => ("quit".into(), json!({})),
        Command::Say { text } => {
            let t = text.join(" ");
            if t.trim().is_empty() {
                anyhow::bail!("say what?");
            }
            ("say".into(), json!({"text": t}))
        }
        Command::Recall { n } => ("recall".into(), json!({"n": n})),
        Command::Inbox { action, id, answer } => (
            "inbox".into(),
            json!({
                "action": action,
                "id": id.clone().unwrap_or_default(),
                "answer": answer.join(" "),
            }),
        ),
        Command::Remind { text, in_, at, every } => {
            let t = text.join(" ");
            if t.trim().is_empty() {
                anyhow::bail!("remind it of what?");
            }
            let when = match (in_, at) {
                (Some(d), _) => format!("in {d}"),
                (None, Some(a)) => a.clone(),
                (None, None) => String::new(),
            };
            (
                "schedule".into(),
                json!({
                    "action": "add", "text": t, "when": when,
                    "every": every.clone().unwrap_or_default(),
                    "by": if crate::args::is_the_mind() { "groow" } else { "mentor" },
                }),
            )
        }
        Command::Schedule { action, id } => (
            "schedule".into(),
            json!({"action": action, "id": id.clone().unwrap_or_default()}),
        ),
        Command::Thoughts => ("thought".into(), json!({"action": "list"})),
        Command::Thought { action, id, text } => (
            "thought".into(),
            json!({"action": action, "id": id, "text": text.join(" ")}),
        ),
        other => anyhow::bail!("`{}` is not something to ask the core", other.name()),
    })
}

/// A line for a person, when the JSON alone would not be obvious.
fn human_note(cmd: &Command, v: &Value) -> Option<String> {
    match cmd {
        Command::Stop => Some("it is going to sleep".into()),
        Command::Say { .. } => Some("left for it; it will answer on its next turn".into()),
        Command::Inbox { action, .. } if action == "list" => {
            let n = v.get("open").and_then(|o| o.as_array()).map(|a| a.len()).unwrap_or(0);
            Some(match n {
                0 => "it has not asked you anything".into(),
                1 => "it is waiting on one answer".into(),
                n => format!("it is waiting on {n} answers"),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::Cli;
    use clap::Parser;

    fn cmd(args: &[&str]) -> Command {
        Cli::try_parse_from(args).unwrap().cmd
    }

    #[test]
    fn saying_something_becomes_one_request() {
        let (op, arg) = request_for(&cmd(&["groow", "say", "hello", "there"])).unwrap();
        assert_eq!(op, "say");
        assert_eq!(arg["text"], "hello there");
    }

    #[test]
    fn saying_nothing_is_refused_before_anything_is_sent() {
        assert!(request_for(&cmd(&["groow", "say", "   "])).is_err());
    }

    #[test]
    fn the_three_ways_of_setting_an_alarm_all_reach_the_core() {
        let (op, arg) = request_for(&cmd(&["groow", "remind", "news", "--in", "30m"])).unwrap();
        assert_eq!(op, "schedule");
        assert_eq!(arg["when"], "in 30m");
        assert_eq!(arg["action"], "add");

        let (_, arg) = request_for(&cmd(&["groow", "remind", "news", "--at", "18:30"])).unwrap();
        assert_eq!(arg["when"], "18:30");

        let (_, arg) = request_for(&cmd(&["groow", "remind", "news", "--every", "daily 06:30"])).unwrap();
        assert_eq!(arg["every"], "daily 06:30");
    }

    #[test]
    fn an_alarm_records_who_set_it() {
        let (_, arg) = request_for(&cmd(&["groow", "remind", "news", "--in", "1h"])).unwrap();
        assert_eq!(arg["by"], "mentor", "by default a person is setting it");
    }

    #[test]
    fn answering_a_question_carries_the_id_and_the_words() {
        let (op, arg) = request_for(&cmd(&["groow", "inbox", "answer", "ab12", "look", "at", "rivers"])).unwrap();
        assert_eq!(op, "inbox");
        assert_eq!(arg["id"], "ab12");
        assert_eq!(arg["answer"], "look at rivers");
    }

    #[test]
    fn the_note_for_a_person_matches_what_is_waiting() {
        let list = cmd(&["groow", "inbox"]);
        assert!(human_note(&list, &json!({"open": []})).unwrap().contains("not asked"));
        assert!(human_note(&list, &json!({"open": [1]})).unwrap().contains("one answer"));
        assert!(human_note(&list, &json!({"open": [1, 2, 3]})).unwrap().contains("3 answers"));
    }

    #[test]
    fn commands_that_are_not_questions_for_the_core_are_refused_here() {
        assert!(request_for(&cmd(&["groow", "doctor"])).is_err());
        assert!(request_for(&cmd(&["groow", "run-turn"])).is_err());
    }
}
