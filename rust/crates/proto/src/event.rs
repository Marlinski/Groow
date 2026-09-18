//! Event names, in one place.
//!
//! Events travel up from a harness process to the core on the same connection that carries
//! its requests, and the core fans them out to every attached interface. They are advisory:
//! nothing waits on one, and a slow interface drops them rather than stalling a turn.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The vocabulary of the event stream. Interfaces switch on this, so the strings are stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventName {
    /// A turn began. `{who, kind, text}`
    TurnStart,
    /// A turn produced its answer. `{kind, final, tools_used, seconds}`
    TurnEnd,
    /// A complete message was added to the conversation. `{role, content}`
    Message,
    /// A piece of the answer as it is generated. `{delta}`
    Token,
    /// A tool is about to run. `{name, args, actor}`
    ToolCall,
    /// A tool finished. `{name, result, actor, ok}`
    ToolResult,
    /// An inner thought changed state. `{id, status, goal}`
    Thought,
    /// The creature's mood moved. `{valence, pain, pleasure}`
    Feeling,
    /// A learning pass ran. `{kind, samples, loss}`
    Learned,
    /// A question was opened, answered or expired. `{id, status, text}`
    Question,
    /// Free-form diagnostics. `{level, text}`
    Log,
    /// The core's view of itself changed. `{...}`
    Status,
}

impl EventName {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventName::TurnStart => "turn_start",
            EventName::TurnEnd => "turn_end",
            EventName::Message => "message",
            EventName::Token => "token",
            EventName::ToolCall => "tool_call",
            EventName::ToolResult => "tool_result",
            EventName::Thought => "thought",
            EventName::Feeling => "feeling",
            EventName::Learned => "learned",
            EventName::Question => "question",
            EventName::Log => "log",
            EventName::Status => "status",
        }
    }

    pub fn parse(s: &str) -> Option<EventName> {
        Some(match s {
            "turn_start" => EventName::TurnStart,
            "turn_end" => EventName::TurnEnd,
            "message" => EventName::Message,
            "token" => EventName::Token,
            "tool_call" => EventName::ToolCall,
            "tool_result" => EventName::ToolResult,
            "thought" => EventName::Thought,
            "feeling" => EventName::Feeling,
            "learned" => EventName::Learned,
            "question" => EventName::Question,
            "log" => EventName::Log,
            "status" => EventName::Status,
            _ => return None,
        })
    }

    /// Token deltas are the only high-rate event. When an interface falls behind, these are
    /// the ones worth dropping, because the completed message follows them anyway.
    pub fn is_droppable(&self) -> bool {
        matches!(self, EventName::Token | EventName::Log)
    }
}

/// An event with its timestamp, ready to be framed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub name: String,
    pub t: f64,
    #[serde(default)]
    pub data: Value,
}

impl Event {
    pub fn new(name: EventName, data: Value) -> Self {
        Event { name: name.as_str().to_string(), t: now(), data }
    }

    pub fn log(level: &str, text: impl Into<String>) -> Self {
        Event::new(EventName::Log, json!({"level": level, "text": text.into()}))
    }

    pub fn kind(&self) -> Option<EventName> {
        EventName::parse(&self.name)
    }

    pub fn is_droppable(&self) -> bool {
        self.kind().map(|k| k.is_droppable()).unwrap_or(false)
    }
}

/// Seconds since the epoch, rounded to the microsecond.
///
/// The rounding is not cosmetic. A raw `f64` of the current time does not survive a JSON
/// round trip: the parser is off by one bit for roughly a tenth of all values, so a timestamp
/// written and read back is not always the timestamp that was written. Anything compared for
/// equality after a reload, such as a record's own timestamp, would then quietly disagree with
/// itself. At microsecond resolution the shortest decimal form is exact, and a microsecond is
/// far finer than anything here measures.
pub fn now() -> f64 {
    let raw = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    round_us(raw)
}

/// Round a time in seconds to the microsecond, so that it survives being written and read.
pub fn round_us(t: f64) -> f64 {
    (t * 1e6).round() / 1e6
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_round_trips() {
        for n in [
            EventName::TurnStart, EventName::TurnEnd, EventName::Message, EventName::Token,
            EventName::ToolCall, EventName::ToolResult, EventName::Thought, EventName::Feeling,
            EventName::Learned, EventName::Question, EventName::Log, EventName::Status,
        ] {
            assert_eq!(EventName::parse(n.as_str()), Some(n), "{} did not round trip", n.as_str());
        }
    }

    #[test]
    fn only_chatter_is_droppable() {
        assert!(EventName::Token.is_droppable());
        assert!(EventName::Log.is_droppable());
        for n in [EventName::TurnEnd, EventName::Message, EventName::Question] {
            assert!(!n.is_droppable(), "{} must not be dropped", n.as_str());
        }
    }

    #[test]
    fn a_timestamp_survives_being_written_and_read_back() {
        // A raw f64 of the clock does not always round trip through JSON. Every timestamp in
        // the system goes through `now`, so this is the one place that has to hold.
        for _ in 0..2000 {
            let t = now();
            let s = serde_json::to_string(&t).unwrap();
            let back: f64 = serde_json::from_str(&s).unwrap();
            assert_eq!(back, t, "timestamp {t} came back as {back}");
        }
    }

    #[test]
    fn rounding_keeps_microseconds_and_drops_what_is_below() {
        assert_eq!(round_us(1.123_456_789), 1.123_457);
        assert_eq!(round_us(0.0), 0.0);
        assert!(now() > 1_700_000_000.0, "the clock should be past 2023");
    }

    #[test]
    fn an_unknown_event_is_not_droppable_by_default() {
        let e = Event { name: "from_the_future".into(), t: 0.0, data: Value::Null };
        assert!(!e.is_droppable());
        assert_eq!(e.kind(), None);
    }
}
