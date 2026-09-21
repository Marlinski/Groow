//! The op namespace, and who is allowed to call each one.
//!
//! Authority is decided from the peer credentials of the connection, never from anything the
//! caller puts in the frame. A harness process cannot promote itself by asking nicely.

use serde::{Deserialize, Serialize};

/// Who is on the other end of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The conscious mind or an inner thought. Least privilege.
    Agent,
    /// A user interface connected over the loopback gateway. May speak and read.
    Viewer,
    /// The owner, on the host side of the sandbox. May do lifecycle things.
    Mentor,
}

/// Every op the core answers, with the least role that may call it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Identify the connection. Always allowed; the reply states the role the core assigned.
    Hello,
    /// A snapshot of the creature: age, step counts, what is running.
    Status,
    /// Follow the event stream on this connection. Events arrive as pushes until it closes.
    Watch,

    // ---- the turn lifecycle, spoken only by a harness process -------------------------
    /// Take ownership of the signal this process was spawned for. Returns everything the
    /// turn needs to run, so that a harness holds no durable state of its own.
    TurnClaim,
    /// Append one message to the conversation. Idempotent on `(turn, seq)`.
    TurnAppend,
    /// Close the turn out. After this the epoch is stale and further writes are refused.
    TurnEnd,
    /// Generate. Replies stream as parts before the terminal frame.
    Complete,

    // ---- things the mind may do to itself -------------------------------------------
    /// Start an inner thought.
    Think,
    /// Act on an existing inner thought: focus, finish, pause, resume, kill, read.
    Thought,
    /// Take the next step of an inner thought. Spoken only by a thought process.
    ThoughtClaim,
    /// Append to an inner thought's own trace.
    ThoughtAppend,
    /// Close out one step of an inner thought.
    ThoughtEnd,
    /// Read the conversation back, further than the window reaches.
    Recall,
    /// The measurements behind the creature: learning passes, turns, feeling, tool health.
    Stats,
    /// Stop whatever turn is running, because a person said so.
    Interrupt,
    /// Set or cancel an alarm.
    Schedule,

    // ---- things only the mentor may do ------------------------------------------------
    /// Speak to the creature.
    Say,
    /// Run one of the mentor commands.
    Command,
    /// Merge the plastic overlay into the base weights.
    Consolidate,
    /// Run a learning pass now.
    Train,
    /// Stop the core.
    Quit,
}

impl Op {
    pub fn parse(s: &str) -> Option<Op> {
        Some(match s {
            "hello" => Op::Hello,
            "status" => Op::Status,
            "watch" => Op::Watch,
            "turn.claim" => Op::TurnClaim,
            "turn.append" => Op::TurnAppend,
            "turn.end" => Op::TurnEnd,
            "complete" => Op::Complete,
            "think" => Op::Think,
            "thought" => Op::Thought,
            "thought.claim" => Op::ThoughtClaim,
            "thought.append" => Op::ThoughtAppend,
            "thought.end" => Op::ThoughtEnd,
            "recall" => Op::Recall,
            "stats" => Op::Stats,
            "interrupt" => Op::Interrupt,
            "schedule" => Op::Schedule,
            "say" => Op::Say,
            "command" => Op::Command,
            "consolidate" => Op::Consolidate,
            "train" => Op::Train,
            "quit" => Op::Quit,
            _ => return None,
        })
    }

    pub fn name(&self) -> &'static str {
        match self {
            Op::Hello => "hello",
            Op::Status => "status",
            Op::Watch => "watch",
            Op::TurnClaim => "turn.claim",
            Op::TurnAppend => "turn.append",
            Op::TurnEnd => "turn.end",
            Op::Complete => "complete",
            Op::Think => "think",
            Op::Thought => "thought",
            Op::ThoughtClaim => "thought.claim",
            Op::ThoughtAppend => "thought.append",
            Op::ThoughtEnd => "thought.end",
            Op::Recall => "recall",
            Op::Stats => "stats",
            Op::Interrupt => "interrupt",
            Op::Schedule => "schedule",
            Op::Say => "say",
            Op::Command => "command",
            Op::Consolidate => "consolidate",
            Op::Train => "train",
            Op::Quit => "quit",
        }
    }

    /// Whether a caller with this role may call this op.
    ///
    /// This is an explicit table rather than a comparison, because the roles are not a ladder.
    /// A viewer is a person at a terminal: it may watch and it may speak, but it must never be
    /// able to claim a turn or write into the conversation, even though the mind can. Ranking
    /// the roles and comparing them would quietly grant a viewer everything the mind has.
    pub fn allowed_for(&self, role: Role) -> bool {
        match role {
            // The owner may do anything, including driving a turn by hand to debug one.
            Role::Mentor => true,
            // The mind: its own vocabulary, and nothing that touches its lifecycle.
            Role::Agent => matches!(
                self,
                Op::Hello | Op::Status | Op::TurnClaim | Op::TurnAppend | Op::TurnEnd
                    | Op::Complete | Op::Think | Op::Thought | Op::Recall
                    | Op::Schedule
                    | Op::ThoughtClaim | Op::ThoughtAppend | Op::ThoughtEnd
            ),
            // A person at a terminal: look, and talk.
            Role::Viewer => matches!(self, Op::Hello | Op::Status | Op::Watch | Op::Say | Op::Command | Op::Recall),
        }
    }

    /// Ops that only make sense from a process the core itself spawned for a turn.
    pub fn is_turn_op(&self) -> bool {
        matches!(
            self,
            Op::TurnClaim | Op::TurnAppend | Op::TurnEnd
                | Op::ThoughtClaim | Op::ThoughtAppend | Op::ThoughtEnd
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for op in [
            Op::Hello, Op::Status, Op::Watch, Op::TurnClaim, Op::TurnAppend, Op::TurnEnd,
            Op::Complete, Op::Think, Op::Thought, Op::ThoughtClaim, Op::ThoughtAppend,
            Op::ThoughtEnd, Op::Recall, Op::Stats, Op::Interrupt, Op::Schedule, Op::Say, Op::Command,
            Op::Consolidate, Op::Train, Op::Quit,
        ] {
            assert_eq!(Op::parse(op.name()), Some(op), "{} did not round trip", op.name());
        }
        assert_eq!(Op::parse("rm -rf"), None);
    }

    #[test]
    fn the_agent_cannot_reach_lifecycle_ops() {
        for op in [Op::Consolidate, Op::Quit, Op::Train, Op::Say, Op::Command] {
            assert!(!op.allowed_for(Role::Agent), "{} should be out of the mind's reach", op.name());
        }
    }

    #[test]
    fn the_mentor_may_do_everything() {
        for op in [Op::Hello, Op::TurnClaim, Op::Say, Op::Quit, Op::Train, Op::Complete] {
            assert!(op.allowed_for(Role::Mentor), "{} denied to mentor", op.name());
        }
    }

    #[test]
    fn a_viewer_may_watch_and_talk_and_nothing_more() {
        assert!(Op::Say.allowed_for(Role::Viewer));
        assert!(Op::Status.allowed_for(Role::Viewer));
        assert!(Op::Recall.allowed_for(Role::Viewer));
        for op in [Op::Quit, Op::Train, Op::Consolidate] {
            assert!(!op.allowed_for(Role::Viewer), "{} is not a viewer's to call", op.name());
        }
    }

    #[test]
    fn a_viewer_can_never_drive_a_turn() {
        // A terminal is not a mind. It must not be able to claim a turn, write into the
        // conversation, or generate, however the roles are ordered.
        for op in [Op::TurnClaim, Op::TurnAppend, Op::TurnEnd, Op::Complete, Op::Think,
                   Op::ThoughtClaim, Op::ThoughtAppend, Op::ThoughtEnd] {
            assert!(!op.allowed_for(Role::Viewer), "{} must not be reachable from a viewer", op.name());
        }
    }

    #[test]
    fn every_op_is_reachable_by_somebody() {
        for op in [
            Op::Hello, Op::Status, Op::Watch, Op::TurnClaim, Op::TurnAppend, Op::TurnEnd,
            Op::Complete, Op::Think, Op::Thought, Op::ThoughtClaim, Op::ThoughtAppend,
            Op::ThoughtEnd, Op::Recall, Op::Stats, Op::Interrupt, Op::Schedule, Op::Say, Op::Command,
            Op::Consolidate, Op::Train, Op::Quit,
        ] {
            let any = [Role::Agent, Role::Viewer, Role::Mentor].iter().any(|r| op.allowed_for(*r));
            assert!(any, "{} is callable by nobody", op.name());
        }
    }
}
