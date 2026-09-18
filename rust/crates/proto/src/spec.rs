//! The protocol, as data.
//!
//! `nervous_system/ops.json` is the written specification of what may be said on the socket.
//! This builds the same table from the code, so a test can compare them. A specification that
//! nobody checks becomes fiction within a month, and this one describes who is allowed to do
//! what, which is the last thing that should quietly drift.

use serde::{Deserialize, Serialize};

use crate::event::EventName;
use crate::ops::{Op, Role};

pub const ALL_OPS: [Op; 21] = [
    Op::Hello, Op::Status, Op::Watch,
    Op::TurnClaim, Op::TurnAppend, Op::TurnEnd, Op::Complete,
    Op::ThoughtClaim, Op::ThoughtAppend, Op::ThoughtEnd,
    Op::Ask, Op::Think, Op::Thought, Op::Recall, Op::Schedule, Op::Inbox,
    Op::Say, Op::Command, Op::Consolidate, Op::Train, Op::Quit,
];

pub const ALL_EVENTS: [EventName; 12] = [
    EventName::TurnStart, EventName::TurnEnd, EventName::Message, EventName::Token,
    EventName::ToolCall, EventName::ToolResult, EventName::Thought, EventName::Feeling,
    EventName::Learned, EventName::Question, EventName::Log, EventName::Status,
];

pub const ERROR_CODES: [&str; 9] = [
    "unknown_op", "bad_arg", "denied", "stale_epoch", "not_found",
    "idle", "busy", "cancelled", "internal",
];

/// The shape of `nervous_system/ops.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    /// A note to whoever opens the file.
    #[serde(rename = "_", default)]
    pub note: String,
    /// Op name to the exact set of roles allowed to call it, sorted.
    pub ops: std::collections::BTreeMap<String, Vec<String>>,
    pub events: Vec<String>,
    pub droppable_events: Vec<String>,
    pub error_codes: Vec<String>,
}

fn role_name(r: Role) -> &'static str {
    match r {
        Role::Agent => "agent",
        Role::Viewer => "viewer",
        Role::Mentor => "mentor",
    }
}

impl Spec {
    /// The specification as the code actually implements it.
    pub fn from_code() -> Spec {
        let mut ops = std::collections::BTreeMap::new();
        for op in ALL_OPS {
            let allowed: Vec<String> = [Role::Agent, Role::Mentor, Role::Viewer]
                .into_iter()
                .filter(|r| op.allowed_for(*r))
                .map(|r| role_name(r).to_string())
                .collect();
            let mut allowed = allowed;
            allowed.sort();
            ops.insert(op.name().to_string(), allowed);
        }
        Spec {
            note: "Generated from the code. `cargo test -p groow-proto` fails if this file and \
                   the code disagree; the failure prints what it should say."
                .into(),
            ops,
            events: ALL_EVENTS.iter().map(|e| e.as_str().to_string()).collect(),
            droppable_events: ALL_EVENTS
                .iter()
                .filter(|e| e.is_droppable())
                .map(|e| e.as_str().to_string())
                .collect(),
            error_codes: ERROR_CODES.iter().map(|s| s.to_string()).collect(),
        }
    }

    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).unwrap_or_default();
        s.push('\n');
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where the written specification lives, relative to this crate.
    fn spec_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../nervous_system/ops.json")
    }

    #[test]
    fn the_written_protocol_matches_the_code() {
        let path = spec_path();
        let code = Spec::from_code();
        // Rewriting it by hand would be tedious and error-prone, so the test can do it.
        if std::env::var_os("GROOW_WRITE_SPEC").is_some() {
            std::fs::write(&path, code.to_json()).expect("could not write the specification");
            return;
        }
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let written: Spec = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{} is not a readable specification: {e}", path.display()));
        if written.ops != code.ops
            || written.events != code.events
            || written.droppable_events != code.droppable_events
            || written.error_codes != code.error_codes
        {
            panic!(
                "{} no longer describes the code.\n\nIt should say:\n\n{}",
                path.display(),
                code.to_json()
            );
        }
    }

    #[test]
    fn every_op_appears_in_the_list_used_to_check_the_specification() {
        // A new op that nobody added here would be invisible to the check above, which would
        // make the specification wrong and the test still green.
        for name in [
            "hello", "status", "watch", "turn.claim", "turn.append", "turn.end", "complete",
            "thought.claim", "thought.append", "thought.end", "ask", "think", "thought",
            "recall", "schedule", "inbox", "say", "command", "consolidate", "train", "quit",
        ] {
            let op = Op::parse(name).unwrap_or_else(|| panic!("{name} is not an op"));
            assert!(ALL_OPS.contains(&op), "{name} is missing from ALL_OPS");
        }
        assert_eq!(ALL_OPS.len(), 21);
    }

    #[test]
    fn nothing_is_callable_by_nobody() {
        for op in ALL_OPS {
            let any = [Role::Agent, Role::Viewer, Role::Mentor]
                .iter()
                .any(|r| op.allowed_for(*r));
            assert!(any, "{} can be called by nobody at all", op.name());
        }
    }
}
