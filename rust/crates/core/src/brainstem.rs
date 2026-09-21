//! The brainstem: what it is doing, and what each thing that happens to it does about that.
//!
//! Everything above this decides *what to think*. This decides whether it is in any condition
//! to think at all — awake or asleep, free or already busy — and what each stimulus means given
//! that. It is the organ that keeps the sleep and waking cycle and gates what reaches the rest
//! of the brain, which is exactly the job here.
//!
//! It is one pure function. A stimulus arrives, the state it is in decides what becomes of it,
//! and out comes the next state and what the body should do — never a caller reading the state
//! and deciding for itself. That is the whole design: the rule for "a message arrived while it
//! was asleep" is written once, in the state that was asleep, rather than spread across every
//! place that has to ask whether it is asleep.
//!
//! Nothing here touches the world. It holds no journal, no queue and no clock, which is what
//! makes every rule in it a table you can read and a test you can write.

use serde::{Deserialize, Serialize};

/// What it is doing.
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
    /// Asked to stop. Nothing new begins.
    Stopping,
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
            State::Stopping => "stopping",
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
            "stopping" => State::Stopping,
            _ => return None,
        })
    }

    /// Whether a turn is in progress, whatever it is waiting for.
    pub fn in_a_turn(&self) -> bool {
        matches!(self, State::Thinking | State::Working)
    }

    /// Whether a learning pass is running.
    pub fn asleep(&self) -> bool {
        matches!(self, State::Napping | State::Sleeping)
    }

    /// Whether it is free to begin something.
    pub fn free(&self) -> bool {
        matches!(self, State::Idle | State::Listening)
    }
}

/// Everything that can happen to it.
///
/// One list, so the question "what can happen here" has an answer you can read rather than one
/// you have to go and find.
#[derive(Debug, Clone, PartialEq)]
pub enum Stimulus {
    /// The weights finished loading, or went away.
    Brain { loaded: bool },
    /// Something arrived for it: a person's message, an alarm, a thought reporting back.
    Arrived { from_a_person: bool },
    /// The scheduler asking whether it may begin something, and what.
    Asked(About),
    /// A turn was opened for the next thing in the queue.
    TurnBegan,
    /// The turn sent a command out into the world.
    ToolCalled,
    /// The command came back.
    ToolReturned,
    /// The turn finished, was stopped, or its process died.
    TurnEnded,
    /// A learning pass began. `night` is the long one that ends in a merge.
    SleepBegan { night: bool },
    /// The learning pass finished.
    SleepEnded,
    /// Curiosity nudged it, having heard nothing for a while.
    Nudged,
    /// A person said something, which makes the conversation warm again.
    Spoke,
    /// Asked to stop for good.
    Stop,
}

/// The two things the scheduler can be asking about.
///
/// They are not the same question. A conscious turn is the one thread of the conversation and
/// there is only ever one; an inner thought is work set aside to run on its own, and making it
/// queue behind the conversation is refusing to set it aside at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum About {
    /// Opening a turn for the next thing in the queue.
    Work,
    /// Giving an inner thought its next step.
    Thought,
}

/// What the body should do about a stimulus. The state decides; the caller carries it out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reflex {
    /// Nothing, beyond whatever the state changed to.
    Nothing,
    /// Take the next thing in the queue and open a turn for it.
    Begin,
    /// Not now. Look again in this many seconds.
    Rest(f64),
    /// Put everything down: the core is going away.
    Halt,
}

/// The state, and the one function that changes it.
#[derive(Debug, Clone)]
pub struct Brainstem {
    state: State,
    /// What it was doing before a learning pass interrupted it, to go back to.
    before_sleep: Option<State>,
    /// The kind of pass under way, for anyone who needs to name it.
    pass: Option<String>,
    /// How many times it has been nudged with no answer, which turns listening into idle.
    unanswered: u32,
}

impl Default for Brainstem {
    fn default() -> Self {
        // Every creature starts here, on every start: the weights are not loaded yet, whatever
        // was true of the last process that ran.
        Brainstem { state: State::Waking, before_sleep: None, pass: None, unanswered: 0 }
    }
}

impl Brainstem {
    pub fn state(&self) -> State {
        self.state
    }

    /// The learning pass under way, if there is one.
    pub fn pass(&self) -> Option<&str> {
        self.pass.as_deref()
    }

    pub fn unanswered(&self) -> u32 {
        self.unanswered
    }

    /// One thing happened. What it means depends on what it was doing, and that is the only
    /// place that dependency is written down.
    pub fn feel(&mut self, s: Stimulus) -> Reflex {
        use State::*;
        use Stimulus as S;

        // Two things are true whatever it is doing, and saying so once here is better than
        // repeating them in every arm below.
        match &s {
            S::Stop => {
                self.state = Stopping;
                return Reflex::Halt;
            }
            S::Spoke => self.unanswered = 0,
            S::Nudged => self.unanswered = self.unanswered.saturating_add(1),
            _ => {}
        }
        if self.state == Stopping {
            // Nothing wakes it again. A core on its way out starts nothing.
            return Reflex::Nothing;
        }

        match (self.state, &s) {
            // ---------------------------------------------------------- waking
            // Until the weights are loaded there is nothing to think with. Everything that
            // arrives is kept — the queue is on disk and outlives all of this — and nothing is
            // begun, because a turn opened now fails for a reason that is not its own.
            (Waking, S::Brain { loaded: true }) => {
                self.state = if self.unanswered > 0 { Idle } else { Listening };
                Reflex::Nothing
            }
            (Waking, S::Asked(_)) => Reflex::Rest(2.0),
            (Waking, _) => Reflex::Nothing,

            // ---------------------------------------------------------- asleep
            // A learning pass is the brain becoming different, and a turn would need the brain.
            // What arrives waits, which is what sleeping means here. It is not woken early: the
            // pass is short, and interrupting it half way through would leave the weights in a
            // state nobody chose.
            (Napping | Sleeping, S::SleepEnded) => {
                self.state = self.before_sleep.take().unwrap_or(Listening);
                self.pass = None;
                // Whatever it was doing before is over; a pass only runs when it was free.
                if self.state.in_a_turn() {
                    self.state = Listening;
                }
                Reflex::Nothing
            }
            // Nothing runs during a pass, thoughts included: they would need the brain, and
            // the brain is busy becoming different.
            (Napping | Sleeping, S::Asked(_)) => Reflex::Rest(1.0),
            (Napping | Sleeping, S::Brain { loaded: false }) => {
                self.state = Waking;
                self.pass = None;
                Reflex::Nothing
            }
            (Napping | Sleeping, _) => Reflex::Nothing,

            // ---------------------------------------------------------- in a turn
            (Thinking | Working, S::ToolCalled) => {
                self.state = Working;
                Reflex::Nothing
            }
            (Thinking | Working, S::ToolReturned) => {
                self.state = Thinking;
                Reflex::Nothing
            }
            (Thinking | Working, S::TurnEnded) => {
                self.state = if self.unanswered > 0 { Idle } else { Listening };
                Reflex::Nothing
            }
            (Thinking | Working, S::Brain { loaded: false }) => {
                self.state = Waking;
                Reflex::Nothing
            }
            // One turn at a time is the reason the conversation is a single thread, so anything
            // that arrives while one is running waits for it. An inner thought does not: it was
            // set aside to run on its own, and it runs on its own.
            (Thinking | Working, S::Asked(About::Work)) => Reflex::Rest(0.5),
            (Thinking | Working, S::Asked(About::Thought)) => Reflex::Begin,
            (Thinking | Working, S::SleepBegan { .. }) => Reflex::Nothing,
            (Thinking | Working, _) => Reflex::Nothing,

            // ---------------------------------------------------------- free
            (Idle | Listening, S::Asked(_)) => Reflex::Begin,
            (Idle | Listening, S::TurnBegan) => {
                self.state = Thinking;
                Reflex::Nothing
            }
            (Idle | Listening, S::SleepBegan { night }) => {
                self.before_sleep = Some(self.state);
                self.pass = Some(if *night { "night" } else { "nap" }.to_string());
                self.state = if *night { Sleeping } else { Napping };
                Reflex::Nothing
            }
            (Idle | Listening, S::Brain { loaded: false }) => {
                self.state = Waking;
                Reflex::Nothing
            }
            // A person's message makes the conversation warm again; anything else leaves it as
            // it was. Either way it is queued by the caller and taken on the next ask.
            (Idle | Listening, S::Arrived { from_a_person }) => {
                if *from_a_person {
                    self.state = Listening;
                    self.unanswered = 0;
                }
                Reflex::Nothing
            }
            (Idle, S::Spoke) => {
                self.state = Listening;
                Reflex::Nothing
            }
            (Idle | Listening, S::Nudged) => {
                self.state = Idle;
                Reflex::Nothing
            }
            (Idle | Listening, _) => Reflex::Nothing,

            // Handled at the top, but the compiler wants every pair and so should we.
            (Stopping, _) => Reflex::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Stimulus as S;
    use super::*;

    /// Every stimulus there is, for the sweeps below.
    fn everything() -> Vec<Stimulus> {
        vec![
            S::Brain { loaded: true },
            S::Brain { loaded: false },
            S::Arrived { from_a_person: true },
            S::Arrived { from_a_person: false },
            S::Asked(About::Work),
            S::Asked(About::Thought),
            S::TurnBegan,
            S::ToolCalled,
            S::ToolReturned,
            S::TurnEnded,
            S::SleepBegan { night: false },
            S::SleepBegan { night: true },
            S::SleepEnded,
            S::Nudged,
            S::Spoke,
            S::Stop,
        ]
    }

    fn awake() -> Brainstem {
        let mut b = Brainstem::default();
        b.feel(S::Brain { loaded: true });
        b
    }

    #[test]
    fn it_begins_where_every_creature_begins() {
        let b = Brainstem::default();
        assert_eq!(b.state(), State::Waking, "the weights are not loaded on any first breath");
    }

    #[test]
    fn nothing_begins_before_the_weights_are_loaded() {
        let mut b = Brainstem::default();
        assert_eq!(b.feel(S::Asked(About::Work)), Reflex::Rest(2.0));
        // What arrives is not lost: the queue is on disk and outlives all of this.
        assert_eq!(b.feel(S::Arrived { from_a_person: true }), Reflex::Nothing);
        assert_eq!(b.state(), State::Waking);
        b.feel(S::Brain { loaded: true });
        assert_eq!(b.feel(S::Asked(About::Work)), Reflex::Begin, "and then it is taken");
    }

    #[test]
    fn a_turn_is_thinking_until_it_runs_something() {
        let mut b = awake();
        assert_eq!(b.feel(S::Asked(About::Work)), Reflex::Begin);
        b.feel(S::TurnBegan);
        assert_eq!(b.state(), State::Thinking);
        b.feel(S::ToolCalled);
        assert_eq!(b.state(), State::Working, "waiting on the world, not on itself");
        b.feel(S::ToolReturned);
        assert_eq!(b.state(), State::Thinking);
        assert_eq!(b.feel(S::Asked(About::Work)), Reflex::Rest(0.5), "one turn at a time");
        b.feel(S::TurnEnded);
        assert_eq!(b.state(), State::Listening);
    }

    #[test]
    fn a_message_arriving_while_it_sleeps_waits_and_does_not_wake_it() {
        // Interrupting a pass half way through would leave the weights in a state nobody
        // chose, so the message waits. It is on disk; nothing is lost by waiting.
        let mut b = awake();
        b.feel(S::SleepBegan { night: true });
        assert_eq!(b.state(), State::Sleeping);
        assert_eq!(b.feel(S::Arrived { from_a_person: true }), Reflex::Nothing);
        assert_eq!(b.state(), State::Sleeping, "it is not woken early");
        assert_eq!(b.feel(S::Asked(About::Work)), Reflex::Rest(1.0));
        b.feel(S::SleepEnded);
        assert_eq!(b.state(), State::Listening);
        assert_eq!(b.feel(S::Asked(About::Work)), Reflex::Begin, "and then the message is taken");
    }

    #[test]
    fn a_nap_and_a_night_are_told_apart_and_it_returns_to_what_it_was() {
        let mut b = awake();
        b.feel(S::Nudged);
        assert_eq!(b.state(), State::Idle);
        b.feel(S::SleepBegan { night: false });
        assert_eq!((b.state(), b.pass()), (State::Napping, Some("nap")));
        b.feel(S::SleepEnded);
        assert_eq!(b.state(), State::Idle, "it goes back to what it was, not to a default");
        assert_eq!(b.pass(), None);
    }

    #[test]
    fn being_nudged_with_no_answer_is_what_makes_it_idle() {
        let mut b = awake();
        assert_eq!(b.state(), State::Listening);
        b.feel(S::Nudged);
        assert_eq!((b.state(), b.unanswered()), (State::Idle, 1));
        b.feel(S::Nudged);
        assert_eq!(b.unanswered(), 2);
        b.feel(S::Spoke);
        assert_eq!((b.state(), b.unanswered()), (State::Listening, 0), "being answered ends it");
    }

    #[test]
    fn a_person_arriving_warms_the_conversation_and_a_nudge_does_not() {
        let mut b = awake();
        b.feel(S::Nudged);
        assert_eq!(b.state(), State::Idle);
        b.feel(S::Arrived { from_a_person: false });
        assert_eq!(b.state(), State::Idle, "an alarm is not company");
        b.feel(S::Arrived { from_a_person: true });
        assert_eq!(b.state(), State::Listening);
    }

    #[test]
    fn the_brain_going_away_puts_it_back_to_waking_from_wherever_it_was() {
        for reach in [vec![S::TurnBegan], vec![S::TurnBegan, S::ToolCalled], vec![S::SleepBegan { night: true }]] {
            let mut b = awake();
            for s in reach.clone() {
                b.feel(s);
            }
            b.feel(S::Brain { loaded: false });
            assert_eq!(b.state(), State::Waking, "after {reach:?}");
            assert_eq!(b.feel(S::Asked(About::Work)), Reflex::Rest(2.0));
        }
    }

    #[test]
    fn stopping_is_the_end_from_anywhere_and_nothing_starts_again() {
        for reach in [vec![], vec![S::TurnBegan], vec![S::SleepBegan { night: false }]] {
            let mut b = awake();
            for s in reach {
                b.feel(s);
            }
            assert_eq!(b.feel(S::Stop), Reflex::Halt);
            assert_eq!(b.state(), State::Stopping);
            for s in everything() {
                b.feel(s);
                assert_eq!(b.state(), State::Stopping, "something restarted a core on its way out");
            }
        }
    }

    #[test]
    fn a_thought_runs_alongside_a_turn_and_a_turn_does_not_run_alongside_a_turn() {
        // `think` is for setting work aside to run on its own. Making it wait for the
        // conversation to fall silent is refusing to set it aside at all.
        let mut b = awake();
        b.feel(S::TurnBegan);
        assert_eq!(b.feel(S::Asked(About::Work)), Reflex::Rest(0.5), "one conversation");
        assert_eq!(b.feel(S::Asked(About::Thought)), Reflex::Begin, "but thinking on its own");

        // Not while it is asleep, though: a thought needs the brain too, and the brain is
        // busy becoming different. (A pass only ever begins when no turn is running, which is
        // why the turn is ended here first rather than slept through.)
        b.feel(S::TurnEnded);
        b.feel(S::SleepBegan { night: false });
        assert_eq!(b.feel(S::Asked(About::Thought)), Reflex::Rest(1.0));
    }

    #[test]
    fn every_state_answers_every_stimulus() {
        // The point of one list of stimuli and one function: there is no pair nobody thought
        // about. This walks all of them from every reachable state and asserts the machine
        // always lands somewhere sensible.
        let reaches: Vec<Vec<Stimulus>> = vec![
            vec![],
            vec![S::Brain { loaded: true }],
            vec![S::Brain { loaded: true }, S::Nudged],
            vec![S::Brain { loaded: true }, S::TurnBegan],
            vec![S::Brain { loaded: true }, S::TurnBegan, S::ToolCalled],
            vec![S::Brain { loaded: true }, S::SleepBegan { night: false }],
            vec![S::Brain { loaded: true }, S::SleepBegan { night: true }],
        ];
        for reach in reaches {
            for s in everything() {
                let mut b = Brainstem::default();
                for r in reach.clone() {
                    b.feel(r);
                }
                let before = b.state();
                let reflex = b.feel(s.clone());
                // A turn is only ever begun when it is free to begin one. A thought may also
                // begin during a turn, because that is what setting work aside means — but
                // never while the weights are loading, during a pass, or on the way out.
                if reflex == Reflex::Begin {
                    let allowed = match s {
                        S::Asked(About::Thought) => before.free() || before.in_a_turn(),
                        _ => before.free(),
                    };
                    assert!(allowed, "{before:?} said Begin on {s:?}");
                }
                assert!(
                    State::parse(b.state().as_str()).is_some(),
                    "{before:?} on {s:?} left it somewhere unnameable"
                );
            }
        }
    }

    #[test]
    fn every_state_survives_being_written_down_and_read_back() {
        for s in [State::Waking, State::Idle, State::Listening, State::Thinking, State::Working,
                  State::Napping, State::Sleeping, State::Stopping] {
            assert_eq!(State::parse(s.as_str()), Some(s), "{} did not round trip", s.as_str());
        }
        assert_eq!(State::parse("dancing"), None);
    }
}
