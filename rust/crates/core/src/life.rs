//! What the creature is doing, as one thing rather than five.
//!
//! It used to be four booleans and an Option read in a particular order in a particular
//! function, and every part of the system that wanted to know had to know that order. This is
//! the one place that decides, and everything else asks.
//!
//! The facts come in as they happen — the brain finished loading, a turn started, a command
//! went out, a learning pass began — and the state falls out of them. Deriving it rather than
//! storing it is deliberate: a stored state drifts from the facts the moment one transition is
//! missed, and the way that shows is a creature that says it is thinking hours after it
//! stopped.

use serde::{Deserialize, Serialize};

/// What it is doing now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// The weights are still loading. Nothing can happen yet.
    Waking,
    /// Nothing to do, and nobody has said anything for a while.
    Idle,
    /// Nothing to do, but the conversation is warm.
    Listening,
    /// In a turn, waiting for the brain.
    Thinking,
    /// In a turn, waiting for a command of its own.
    Working,
    /// A short learning pass between turns.
    Napping,
    /// The long pass: practise, then merge what was practised into the base weights.
    Sleeping,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Waking => "waking",
            State::Idle => "idle",
            State::Listening => "listening",
            State::Thinking => "thinking",
            State::Working => "working",
            State::Napping => "napping",
            State::Sleeping => "sleeping",
        }
    }

    pub fn parse(s: &str) -> Option<State> {
        Some(match s {
            "waking" => State::Waking,
            "idle" => State::Idle,
            "listening" => State::Listening,
            "thinking" => State::Thinking,
            "working" => State::Working,
            "napping" => State::Napping,
            "sleeping" => State::Sleeping,
            _ => return None,
        })
    }

    /// Whether a turn is in progress, whatever it is waiting for.
    pub fn in_a_turn(&self) -> bool {
        matches!(self, State::Thinking | State::Working)
    }
}

/// Inside a turn, the thing it is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Waiting {
    Brain,
    Command,
}

/// The facts, and the state they add up to.
#[derive(Debug, Clone, Default)]
pub struct Life {
    brain_up: bool,
    /// The learning pass under way, if there is one: "nap" or "night".
    asleep: Option<String>,
    turn: Option<Waiting>,
    quiet_for: u32,
}

impl Life {
    /// What it is doing, in one word.
    ///
    /// The order is the priority: a creature whose weights are not loaded cannot be thinking,
    /// and one that is asleep is asleep whatever else was true a moment ago.
    pub fn state(&self) -> State {
        if !self.brain_up {
            return State::Waking;
        }
        match &self.asleep {
            Some(what) if what == "night" => return State::Sleeping,
            Some(_) => return State::Napping,
            None => {}
        }
        match self.turn {
            Some(Waiting::Brain) => State::Thinking,
            Some(Waiting::Command) => State::Working,
            None if self.quiet_for > 0 => State::Idle,
            None => State::Listening,
        }
    }

    /// Whether the weights are loaded. Nothing can be done before they are.
    pub fn awake(&self) -> bool {
        self.brain_up
    }

    pub fn brain(&mut self, up: bool) {
        self.brain_up = up;
    }

    pub fn asleep(&mut self, what: Option<String>) {
        self.asleep = what;
    }

    pub fn quiet_for(&mut self, turns: u32) {
        self.quiet_for = turns;
    }

    /// A turn began, and is thinking until something says otherwise.
    pub fn turn_began(&mut self) {
        self.turn = Some(Waiting::Brain);
    }

    pub fn turn_ended(&mut self) {
        self.turn = None;
    }

    /// One of the events a turn announces as it goes. Anything else is not about the state and
    /// is ignored rather than guessed at.
    pub fn saw(&mut self, event: &str) {
        match event {
            // A command went out: it is waiting on the world now, not on itself.
            "tool_call" => self.turn = Some(Waiting::Command),
            // The command came back, so it is thinking about the answer.
            "tool_result" => self.turn = Some(Waiting::Brain),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn awake() -> Life {
        let mut l = Life::default();
        l.brain(true);
        l
    }

    #[test]
    fn nothing_happens_until_the_weights_are_loaded() {
        let mut l = Life::default();
        assert_eq!(l.state(), State::Waking);
        // Even mid-turn: a turn cannot be thinking with nothing to think with.
        l.turn_began();
        assert_eq!(l.state(), State::Waking);
        l.brain(true);
        assert_eq!(l.state(), State::Thinking);
    }

    #[test]
    fn a_turn_is_thinking_until_it_runs_something() {
        let mut l = awake();
        assert_eq!(l.state(), State::Listening);
        l.turn_began();
        assert_eq!(l.state(), State::Thinking);
        l.saw("tool_call");
        assert_eq!(l.state(), State::Working, "it is waiting on the world, not on itself");
        l.saw("tool_result");
        assert_eq!(l.state(), State::Thinking, "and back to thinking about the answer");
        l.turn_ended();
        assert_eq!(l.state(), State::Listening);
    }

    #[test]
    fn asleep_outranks_whatever_it_was_doing() {
        let mut l = awake();
        l.turn_began();
        l.asleep(Some("night".into()));
        assert_eq!(l.state(), State::Sleeping);
        l.asleep(Some("nap".into()));
        assert_eq!(l.state(), State::Napping);
        l.asleep(None);
        assert_eq!(l.state(), State::Thinking, "and what it was doing is still true underneath");
    }

    #[test]
    fn quiet_for_long_enough_is_idle_rather_than_listening() {
        let mut l = awake();
        assert_eq!(l.state(), State::Listening);
        l.quiet_for(1);
        assert_eq!(l.state(), State::Idle);
        l.turn_began();
        assert_eq!(l.state(), State::Thinking, "being nudged is still a turn");
    }

    #[test]
    fn an_event_that_says_nothing_about_the_state_changes_nothing() {
        let mut l = awake();
        l.turn_began();
        for noise in ["message", "log", "feeling", "learned", "thought"] {
            l.saw(noise);
            assert_eq!(l.state(), State::Thinking, "{noise} moved the state");
        }
    }

    #[test]
    fn every_state_survives_being_written_down_and_read_back() {
        for s in [State::Waking, State::Idle, State::Listening, State::Thinking,
                  State::Working, State::Napping, State::Sleeping] {
            assert_eq!(State::parse(s.as_str()), Some(s), "{} did not round trip", s.as_str());
        }
        assert_eq!(State::parse("dancing"), None);
    }
}
