//! Reading what the model said.
//!
//! A generation is prose with tool calls embedded in it. Pulling them apart has to be done
//! carefully, because a half-streamed call or a malformed one must never be shown to a person
//! as if it were speech, and must never be run as if it were a valid call.

use serde_json::Value;

/// One tool the model asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub name: String,
    pub args: Value,
}

/// What a generation breaks down into.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    /// Reasoning the model was asked to keep to itself.
    pub reasoning: String,
    /// What a person should see.
    pub content: String,
    pub calls: Vec<Call>,
    /// A call that was started but never closed, or could not be read. Worth knowing about,
    /// because it usually means the answer was cut off rather than that nothing was asked.
    pub truncated: bool,
}

const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";
const CALL_OPEN: &str = "<tool_call>";
const CALL_CLOSE: &str = "</tool_call>";

/// Split a raw generation into reasoning, speech and tool calls.
pub fn parse(raw: &str) -> Parsed {
    let mut out = Parsed::default();
    let mut rest = raw;

    // Reasoning comes first when it comes at all, and only the first block counts.
    if let Some(start) = rest.find(THINK_OPEN) {
        let after = &rest[start + THINK_OPEN.len()..];
        match after.find(THINK_CLOSE) {
            Some(end) => {
                out.reasoning = after[..end].trim().to_string();
                // Anything before the reasoning was preamble; keep it with the rest.
                let before = &rest[..start];
                let tail = &after[end + THINK_CLOSE.len()..];
                out.content = format!("{before}{tail}");
                rest = "";
            }
            None => {
                // The answer was cut off inside its own reasoning. There is no speech in it.
                out.reasoning = after.trim().to_string();
                out.content = rest[..start].to_string();
                out.truncated = true;
                rest = "";
            }
        }
    }
    if !rest.is_empty() {
        out.content = rest.to_string();
    }

    // Now lift the tool calls out of what is left.
    let mut speech = String::new();
    let mut cursor = 0usize;
    let body = out.content.clone();
    while let Some(rel) = body[cursor..].find(CALL_OPEN) {
        let start = cursor + rel;
        speech.push_str(&body[cursor..start]);
        let after = start + CALL_OPEN.len();
        match body[after..].find(CALL_CLOSE) {
            Some(rel_end) => {
                let inner = &body[after..after + rel_end];
                match read_call(inner) {
                    Some(c) => out.calls.push(c),
                    None => out.truncated = true,
                }
                cursor = after + rel_end + CALL_CLOSE.len();
            }
            None => {
                // An unterminated call. Drop it rather than showing the JSON to a person.
                out.truncated = true;
                cursor = body.len();
            }
        }
    }
    speech.push_str(&body[cursor.min(body.len())..]);
    out.content = speech.trim().to_string();
    out
}

fn read_call(inner: &str) -> Option<Call> {
    let v: Value = serde_json::from_str(inner.trim()).ok()?;
    let name = v.get("name")?.as_str()?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let args = match v.get("arguments") {
        // Some generations quote the arguments as a JSON string rather than an object.
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or_else(|_| Value::Object(Default::default())),
        Some(Value::Object(m)) => Value::Object(m.clone()),
        _ => Value::Object(Default::default()),
    };
    Some(Call { name, args })
}

/// What a person should see of a partial generation, while it is still arriving.
///
/// Used for the live stream, where a tool call may be halfway written. Anything unterminated
/// is hidden, so reasoning and raw JSON never flash up on the screen.
pub fn visible(partial: &str) -> String {
    let p = parse(partial);
    p.content
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plain_speech_passes_through() {
        let p = parse("Rivers run downhill.");
        assert_eq!(p.content, "Rivers run downhill.");
        assert!(p.calls.is_empty());
        assert!(!p.truncated);
    }

    #[test]
    fn a_tool_call_is_lifted_out_of_the_speech() {
        let raw = "Let me look.\n<tool_call>\n{\"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}\n</tool_call>";
        let p = parse(raw);
        assert_eq!(p.content, "Let me look.");
        assert_eq!(p.calls.len(), 1);
        assert_eq!(p.calls[0].name, "shell");
        assert_eq!(p.calls[0].args["command"], "ls");
    }

    #[test]
    fn several_calls_keep_their_order() {
        let raw = "<tool_call>{\"name\":\"a\"}</tool_call>middle<tool_call>{\"name\":\"b\"}</tool_call>";
        let p = parse(raw);
        assert_eq!(p.calls.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(p.content, "middle");
    }

    #[test]
    fn reasoning_is_kept_out_of_what_a_person_sees() {
        let p = parse("<think>they probably mean the river</think>The Loire is the longest.");
        assert_eq!(p.reasoning, "they probably mean the river");
        assert_eq!(p.content, "The Loire is the longest.");
    }

    #[test]
    fn an_unterminated_thought_never_leaks_into_speech() {
        let p = parse("<think>I am still working this out and the answer was cut off");
        assert_eq!(p.content, "", "reasoning must not be shown as speech");
        assert!(p.truncated);
        assert!(p.reasoning.contains("still working"));
    }

    #[test]
    fn an_unterminated_call_is_hidden_rather_than_shown_as_json() {
        let p = parse("Looking now.\n<tool_call>{\"name\": \"shell\", \"argum");
        assert_eq!(p.content, "Looking now.");
        assert!(p.calls.is_empty());
        assert!(p.truncated, "a cut-off call should be known about");
    }

    #[test]
    fn a_malformed_call_is_refused_not_guessed_at() {
        let p = parse("<tool_call>{\"name\": }</tool_call>ok");
        assert!(p.calls.is_empty());
        assert!(p.truncated);
        assert_eq!(p.content, "ok");
    }

    #[test]
    fn a_call_without_a_name_is_not_a_call() {
        let p = parse("<tool_call>{\"arguments\": {\"a\": 1}}</tool_call>");
        assert!(p.calls.is_empty());
        let p2 = parse("<tool_call>{\"name\": \"   \"}</tool_call>");
        assert!(p2.calls.is_empty());
    }

    #[test]
    fn arguments_quoted_as_a_string_are_still_read() {
        let p = parse("<tool_call>{\"name\":\"shell\",\"arguments\":\"{\\\"command\\\":\\\"pwd\\\"}\"}</tool_call>");
        assert_eq!(p.calls[0].args["command"], "pwd");
    }

    #[test]
    fn missing_arguments_become_an_empty_object_not_a_failure() {
        let p = parse("<tool_call>{\"name\":\"finish\"}</tool_call>");
        assert_eq!(p.calls.len(), 1);
        assert_eq!(p.calls[0].args, json!({}));
    }

    #[test]
    fn a_partial_generation_shows_only_what_is_finished() {
        assert_eq!(visible("Hello ther"), "Hello ther");
        assert_eq!(visible("Hello.\n<tool_ca"), "Hello.\n<tool_ca", "a fragment that is not yet a tag is just text");
        assert_eq!(visible("Hello.\n<tool_call>{\"na"), "Hello.");
        assert_eq!(visible("<think>hmm"), "");
    }

    #[test]
    fn text_around_a_thought_block_survives() {
        let p = parse("before <think>middle</think> after");
        assert_eq!(p.content, "before  after".trim());
        assert_eq!(p.reasoning, "middle");
    }

    #[test]
    fn nothing_at_all_is_handled() {
        let p = parse("");
        assert_eq!(p, Parsed::default());
    }
}
