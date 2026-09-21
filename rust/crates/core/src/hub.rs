//! The hub: one task that owns every piece of mutable state.
//!
//! # Why there can be no deadlock here
//!
//! Every command is handled by [`Hub::handle`], which is an ordinary synchronous function.
//! It has no `await` points, so while it runs nothing else touches the state and it cannot
//! wait on another task. A deadlock needs two parties each holding something the other wants;
//! here there is one party and it never waits. The file and database work it does is local and
//! measured in microseconds.
//!
//! Anything slow stays outside: generation, spawning processes and network traffic all happen
//! in connection tasks, which come back to the hub only to record what happened.
//!
//! Events go out through bounded queues with a non-blocking send. A user interface that stops
//! reading loses events; it can never slow down a turn. That is the one place where a naive
//! design would deadlock, so it is the one place that is deliberately lossy.


use groow_proto::event::{Event, EventName};
use groow_proto::turn::{Epoch, Message, SignalKind, TurnContext, TurnOutcome};
use groow_proto::WireError;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

use crate::config::Config;
use crate::db::Db;
use crate::paths::Paths;
use crate::store::birth::Birth;
use crate::store::identity::Identity;
use crate::store::journal::{restore_window_without, Journal};
use crate::store::inbox::{Inbox, Signal};
use crate::store::schedule::Schedule;
use crate::store::thoughts::{ThoughtStatus, Thoughts};

/// How many messages the window handed to a turn holds.
const WINDOW: usize = 30;

/// How many events a subscriber may fall behind before it starts losing them.
const SUBSCRIBER_DEPTH: usize = 512;

/// How many times a signal may kill the process handling it before it is given up on.
const MAX_ATTEMPTS: u32 = 3;

type Answer<T> = oneshot::Sender<Result<T, WireError>>;

/// What the scheduler should do next. The hub decides; the scheduler acts, because acting
/// means spawning processes and that is slow.
#[derive(Debug, Clone, PartialEq)]
pub enum Duty {
    /// Run one conscious turn for this signal, which the core has already opened under this id.
    Turn(Box<Signal>, String),
    /// Continue an inner thought in its own process.
    Thought(String),
    /// Run a learning pass. The mind never asks for this and cannot refuse it.
    Learn(&'static str),
    /// Nothing to do; look again in this many seconds.
    Idle(f64),
}

/// The turn currently in flight. At most one exists, which is what makes the conscious thread
/// single: there is no lock to take because there is only one scheduler and it holds this.
#[derive(Debug, Clone)]
struct Active {
    id: String,
    epoch: Epoch,
    kind: SignalKind,
    started: f64,
    signal: Signal,
    claimed: bool,
    rounds: u32,
}

pub enum Cmd {
    Hello { who: String, reply: Answer<Value> },
    Status { reply: Answer<Value> },
    Subscribe { reply: Answer<mpsc::Receiver<Event>> },
    Emit { event: Event },

    /// The scheduler asking what to do, and taking ownership of the answer.
    NextDuty { reply: Answer<Duty> },
    /// A harness process taking the turn it was spawned for.
    Claim { pid: u32, reply: Answer<TurnContext> },
    /// The turn writing a message into the conversation.
    Append { turn: String, epoch: Epoch, msg: Box<Message>, reply: Answer<Value> },
    /// The turn finishing, for better or worse.
    Finish { outcome: Box<TurnOutcome>, reply: Answer<Value> },
    /// The turn process went away without finishing.
    Abandon { turn: String, why: String },
    /// A thought's process ended without taking a step.
    ThoughtFailed { id: String, why: String },
    /// The brain is learning rather than answering. Nothing else runs meanwhile.
    Napping { what: Option<String> },
    /// Whether the brain is answering at all. A turn needs it, so nothing starts without it.
    BrainState { up: bool },

    Say { text: String, kind: SignalKind, meta: Value, reply: Answer<Value> },
    Think { goal: String, max_steps: u32, reply: Answer<Value> },
    ThoughtAction { action: String, id: String, text: String, reply: Answer<Value> },
    /// A thought process taking its next step.
    ThoughtClaim { id: String, pid: u32, reply: Answer<TurnContext> },
    ThoughtAppend { id: String, msg: Box<Message>, reply: Answer<Value> },
    ThoughtEnd { id: String, final_text: String, flags: Vec<String>, reply: Answer<Value> },
    ScheduleAction { action: String, text: String, when: String, every: String, id: String, by: String, reply: Answer<Value> },
    Recall { n: usize, before: Option<f64>, reply: Answer<Value> },
    Stats { n: usize, reply: Answer<Value> },
    ToolRan { turn: String, name: String, actor: String, ok: bool, seconds: f64 },
    Shutdown { reply: Answer<Value> },
}

/// A cheap, cloneable way to talk to the hub.
#[derive(Clone)]
pub struct Handle {
    tx: mpsc::Sender<Cmd>,
}

impl Handle {
    /// Send a command and wait for its answer.
    ///
    /// A closed hub is reported as an internal error rather than a panic, so a request that
    /// arrives during shutdown fails the one caller instead of taking the process down.
    async fn ask<T>(&self, make: impl FnOnce(Answer<T>) -> Cmd) -> Result<T, WireError> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(make(tx)).await.map_err(|_| WireError::Internal("the core is shutting down".into()))?;
        rx.await.map_err(|_| WireError::Internal("the core dropped a request".into()))?
    }

    /// Send something the hub never answers.
    async fn tell(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd).await;
    }

    pub async fn hello(&self, who: &str) -> Result<Value, WireError> {
        let who = who.to_string();
        self.ask(|reply| Cmd::Hello { who, reply }).await
    }
    pub async fn status(&self) -> Result<Value, WireError> {
        self.ask(|reply| Cmd::Status { reply }).await
    }
    pub async fn subscribe(&self) -> Result<mpsc::Receiver<Event>, WireError> {
        self.ask(|reply| Cmd::Subscribe { reply }).await
    }
    pub async fn emit(&self, event: Event) {
        self.tell(Cmd::Emit { event }).await
    }
    pub async fn next_duty(&self) -> Result<Duty, WireError> {
        self.ask(|reply| Cmd::NextDuty { reply }).await
    }
    pub async fn claim(&self, pid: u32) -> Result<TurnContext, WireError> {
        self.ask(|reply| Cmd::Claim { pid, reply }).await
    }
    pub async fn append(&self, turn: &str, epoch: Epoch, msg: Message) -> Result<Value, WireError> {
        let (turn, msg) = (turn.to_string(), Box::new(msg));
        self.ask(|reply| Cmd::Append { turn, epoch, msg, reply }).await
    }
    pub async fn finish(&self, outcome: TurnOutcome) -> Result<Value, WireError> {
        let outcome = Box::new(outcome);
        self.ask(|reply| Cmd::Finish { outcome, reply }).await
    }
    pub async fn abandon(&self, turn: &str, why: &str) {
        self.tell(Cmd::Abandon { turn: turn.to_string(), why: why.to_string() }).await
    }
    pub async fn thought_failed(&self, id: &str, why: &str) {
        self.tell(Cmd::ThoughtFailed { id: id.to_string(), why: why.to_string() }).await
    }
    pub async fn napping(&self, what: Option<&str>) {
        self.tell(Cmd::Napping { what: what.map(|s| s.to_string()) }).await
    }
    pub async fn brain_state(&self, up: bool) {
        self.tell(Cmd::BrainState { up }).await
    }
    pub async fn say(&self, text: &str, kind: SignalKind, meta: Value) -> Result<Value, WireError> {
        let text = text.to_string();
        self.ask(|reply| Cmd::Say { text, kind, meta, reply }).await
    }
    pub async fn think(&self, goal: &str, max_steps: u32) -> Result<Value, WireError> {
        let goal = goal.to_string();
        self.ask(|reply| Cmd::Think { goal, max_steps, reply }).await
    }
    pub async fn thought_claim(&self, id: &str, pid: u32) -> Result<TurnContext, WireError> {
        let id = id.to_string();
        self.ask(|reply| Cmd::ThoughtClaim { id, pid, reply }).await
    }
    pub async fn thought_append(&self, id: &str, msg: Message) -> Result<Value, WireError> {
        let (id, msg) = (id.to_string(), Box::new(msg));
        self.ask(|reply| Cmd::ThoughtAppend { id, msg, reply }).await
    }
    pub async fn thought_end(&self, id: &str, final_text: &str, flags: Vec<String>) -> Result<Value, WireError> {
        let (id, final_text) = (id.to_string(), final_text.to_string());
        self.ask(|reply| Cmd::ThoughtEnd { id, final_text, flags, reply }).await
    }
    pub async fn thought(&self, action: &str, id: &str, text: &str) -> Result<Value, WireError> {
        let (action, id, text) = (action.to_string(), id.to_string(), text.to_string());
        self.ask(|reply| Cmd::ThoughtAction { action, id, text, reply }).await
    }
    pub async fn schedule(&self, action: &str, text: &str, when: &str, every: &str, id: &str, by: &str)
        -> Result<Value, WireError>
    {
        let (action, text, when) = (action.to_string(), text.to_string(), when.to_string());
        let (every, id, by) = (every.to_string(), id.to_string(), by.to_string());
        self.ask(|reply| Cmd::ScheduleAction { action, text, when, every, id, by, reply }).await
    }
    pub async fn recall(&self, n: usize, before: Option<f64>) -> Result<Value, WireError> {
        self.ask(|reply| Cmd::Recall { n, before, reply }).await
    }
    pub async fn stats(&self, n: usize) -> Result<Value, WireError> {
        self.ask(|reply| Cmd::Stats { n, reply }).await
    }
    pub async fn tool_ran(&self, turn: &str, name: &str, actor: &str, ok: bool, seconds: f64) {
        self.tell(Cmd::ToolRan {
            turn: turn.to_string(), name: name.to_string(), actor: actor.to_string(), ok, seconds,
        }).await
    }
    pub async fn shutdown(&self) -> Result<Value, WireError> {
        self.ask(|reply| Cmd::Shutdown { reply }).await
    }
}

pub struct Hub {
    cfg: Config,
    paths: Paths,
    journal: Journal,
    inbox: Inbox,
    schedule: Schedule,
    thoughts: Thoughts,
    identity: Identity,
    birth: Birth,
    db: Db,
    subscribers: Vec<mpsc::Sender<Event>>,
    active: Option<Active>,
    epoch: Epoch,
    /// Consecutive idle nudges, so the mind is left alone for longer the less is happening.
    idle_streak: u32,
    /// What kind of learning pass is running, if any. While one is, the mind is asleep: no
    /// turn is started, because the brain it would need is busy changing itself.
    napping: Option<String>,
    /// Whether the brain is answering. It takes a while to load the weights, and a turn that
    /// starts before then fails for a reason that has nothing to do with the turn.
    brain_up: bool,
    /// Consecutive turns that ended badly, and when the last one did. A brain that is down
    /// must not turn into a spawn loop.
    fail_streak: u32,
    last_failure: f64,
    /// Turns since the last learning pass, and when the last night was.
    since_learned: u32,
    last_night: f64,
    last_human: f64,
    running: bool,
    /// The one source of time in the core.
    ///
    /// Everything that compares two moments reads it, so a test can move time without any
    /// part of the hub disagreeing with another about what "now" is. Taking the time from a
    /// caller for one decision and from the system clock for another is how a backoff quietly
    /// becomes permanent.
    clock: Box<dyn Fn() -> f64 + Send>,
}

/// How a signal is framed before the mind reads it. Only a person speaks to it unframed.
pub fn frame(kind: SignalKind, text: &str, meta: &Value) -> String {
    let thought = meta.get("thought").and_then(|v| v.as_str()).unwrap_or("?");
    match kind {
        // An unspecified kind can only come from a record written by something newer; show it
        // as it is rather than inventing a frame for it.
        SignalKind::SignalUser | SignalKind::SignalCommand | SignalKind::SignalUnspecified => text.to_string(),
        SignalKind::SignalFocus => format!("[inner thought {thought} says] {text}"),
        SignalKind::SignalThoughtDone => format!("[inner thought {thought} finished] {text}"),
        SignalKind::SignalReminder => format!(
            "[reminder, no reply needed] {text}. You may read that thought, pause it, or ignore this."),
        SignalKind::SignalAlarm => format!("[an alarm you set earlier] {text}"),
        SignalKind::SignalNote => format!("[a note you left yourself earlier] {text}"),
        SignalKind::SignalIdle => text.to_string(),
    }
}

/// How long to wait after a turn has failed, doubling with each consecutive failure.
pub fn backoff(streak: u32) -> f64 {
    (2f64.powi(streak.min(6) as i32)).min(60.0)
}

/// The gap before the next idle nudge, doubling each time nothing comes of it.
pub fn idle_gap(streak: u32, base_minutes: f64, max_minutes: f64) -> f64 {
    let gap = base_minutes * 2f64.powi(streak.min(8) as i32);
    gap.min(max_minutes) * 60.0
}

impl Hub {
    pub fn new(cfg: Config, paths: Paths, birth: Birth, db: Db) -> anyhow::Result<Hub> {
        paths.ensure()?;
        Ok(Hub {
            journal: Journal::open(paths.journal())?,
            inbox: Inbox::open(paths.inbox())?,
            schedule: Schedule::new(paths.schedule()),
            thoughts: Thoughts::new(paths.thoughts()),
            identity: Identity::new(paths.identity()),
            birth,
            db,
            subscribers: Vec::new(),
            active: None,
            epoch: Epoch(0),
            idle_streak: 0,
            napping: None,
            brain_up: false,
            fail_streak: 0,
            last_failure: 0.0,
            since_learned: 0,
            last_night: groow_proto::event::now(),
            last_human: groow_proto::event::now(),
            running: true,
            clock: Box::new(groow_proto::event::now),
            cfg,
            paths,
        })
    }

    /// Start the hub on its own task and hand back a way to talk to it.
    pub fn spawn(mut self) -> (Handle, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel::<Cmd>(1024);
        let join = tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                let keep = self.handle(cmd);
                if !keep {
                    break;
                }
            }
        });
        (Handle { tx }, join)
    }

    /// Handle one command. Returns false when the hub should stop.
    ///
    /// There is no `await` in this function or anything it calls. That is the property the
    /// freedom from deadlock rests on, and it is worth keeping.
    pub fn handle(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Hello { who, reply } => send(reply, Ok(self.hello(&who))),
            Cmd::Status { reply } => send(reply, Ok(self.status())),
            Cmd::Subscribe { reply } => {
                let (tx, rx) = mpsc::channel(SUBSCRIBER_DEPTH);
                self.subscribers.push(tx);
                send(reply, Ok(rx));
            }
            Cmd::Emit { event } => self.fanout(event),
            Cmd::NextDuty { reply } => {
                let d = self.next_duty();
                send(reply, d);
            }
            Cmd::Claim { pid, reply } => {
                let r = self.claim(pid);
                send(reply, r);
            }
            Cmd::Append { turn, epoch, msg, reply } => {
                let r = self.append(&turn, epoch, *msg);
                send(reply, r);
            }
            Cmd::Finish { outcome, reply } => {
                let r = self.finish(*outcome);
                send(reply, r);
            }
            Cmd::Abandon { turn, why } => self.abandon(&turn, &why),
            Cmd::ThoughtFailed { id, why } => self.thought_failed(&id, &why),
            Cmd::BrainState { up } => {
                if self.brain_up != up {
                    self.brain_up = up;
                    self.fanout(Event::new(EventName::Status, self.status()));
                }
            }
            Cmd::Napping { what } => {
                self.napping = what.clone();
                self.fanout(Event::new(EventName::Status, self.status()));
            }
            Cmd::Say { text, kind, meta, reply } => {
                let r = self.say(&text, kind, meta);
                send(reply, r);
            }
            Cmd::Think { goal, max_steps, reply } => {
                let r = self.think(&goal, max_steps);
                send(reply, r);
            }
            Cmd::ThoughtAction { action, id, text, reply } => {
                let r = self.thought_action(&action, &id, &text);
                send(reply, r);
            }
            Cmd::ThoughtClaim { id, pid, reply } => {
                let r = self.thought_claim(&id, pid);
                send(reply, r);
            }
            Cmd::ThoughtAppend { id, msg, reply } => {
                let r = self.thought_append(&id, *msg);
                send(reply, r);
            }
            Cmd::ThoughtEnd { id, final_text, flags, reply } => {
                let r = self.thought_end(&id, &final_text, &flags);
                send(reply, r);
            }
            Cmd::ScheduleAction { action, text, when, every, id, by, reply } => {
                let r = self.schedule_action(&action, &text, &when, &every, &id, &by);
                send(reply, r);
            }
            Cmd::Recall { n, before, reply } => {
                let r = self.recall(n, before);
                send(reply, r);
            }
            Cmd::Stats { n, reply } => {
                let r = self.stats(n);
                send(reply, r);
            }
            Cmd::ToolRan { turn, name, actor, ok, seconds } => {
                let _ = self.db.tool_call(&turn, groow_proto::event::now(), &name, &actor, ok, seconds);
            }
            Cmd::Shutdown { reply } => {
                self.running = false;
                self.fanout(Event::new(EventName::Log, json!({"level": "info", "text": "going to sleep"})));
                send(reply, Ok(json!({"ok": true})));
                return false;
            }
        }
        self.running
    }

    // ------------------------------------------------------------ events
    /// Hand an event to every subscriber that can take it right now.
    ///
    /// Non-blocking on purpose. An interface that has stopped reading must not be able to hold
    /// up a turn, so it loses events instead, and a subscriber whose channel has closed is
    /// forgotten.
    fn fanout(&mut self, ev: Event) {
        self.subscribers.retain(|s| !s.is_closed());
        for s in &self.subscribers {
            match s.try_send(ev.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => {}
            }
        }
    }

    // ------------------------------------------------------------ introspection
    fn hello(&self, who: &str) -> Value {
        let now = self.now();
        json!({
            "birth": {
                "id": self.birth.id, "name": self.birth.name, "born": self.birth.born,
                "born_text": self.birth.born_text(), "age": self.birth.age_text(now),
                "lineage": self.birth.lineage, "hardware": self.birth.hardware,
                "mentor": self.birth.mentor, "version": self.birth.version,
            },
            "you": who,
            "model": self.cfg.model_id,
            "status": self.status(),
        })
    }

    fn status(&self) -> Value {
        let now = self.now();
        let (pain, pleasure) = self.db.mood(now, self.cfg.mood_halflife_s).unwrap_or((0.0, 0.0));
        let tone = if pleasure - pain > 0.8 { "content" } else if pain - pleasure > 0.8 { "sore" } else { "even" };
        json!({
            "napping": self.napping,
            "brain": self.brain_up,
            "mood": self.mood(),
            "age": self.birth.age_text(now),
            "born": self.birth.born,
            "queue": self.inbox.len().unwrap_or(0),
            "busy": self.active.is_some(),
            "turn": self.active.as_ref().map(|a| a.id.clone()),
            "turns": self.db.turn_count().unwrap_or(0),
            "thoughts": self.thoughts.live().map(|v| v.len()).unwrap_or(0),
            "feeling": {"pain": pain, "pleasure": pleasure, "tone": tone},
            "idle_streak": self.idle_streak,
        })
    }

    /// What it looks like it is doing, for the creature and the status line.
    fn mood(&self) -> &'static str {
        if self.napping.is_some() {
            return "napping";
        }
        if !self.brain_up {
            return "waking";
        }
        match &self.active {
            Some(_) => "thinking",
            None if self.idle_streak > 0 => "idle",
            None => "listening",
        }
    }

    /// Read the conversation back: the last `n`, or the `n` before a time already held.
    ///
    /// Paging backwards is what lets a window show a day rather than a screenful. `more` is
    /// false when nothing older came back, which is how the window knows to stop asking.
    fn recall(&self, n: usize, before: Option<f64>) -> Result<Value, WireError> {
        let n = n.clamp(1, 500);
        let recs = match before {
            Some(ts) => self.journal.before(ts, n).map_err(io)?,
            None => self.journal.tail(n).map_err(io)?,
        };
        Ok(json!({"messages": recs, "more": recs.len() >= n}))
    }

    /// What the meta-processes have been doing: the learning passes, the turns behind them and
    /// how they felt. Read straight out of the database, which only the core can open.
    ///
    /// This is the one view the mind is not given. How it is being scored is not its business,
    /// and a mind that could read its own reward would learn to read it rather than to earn it.
    fn stats(&self, n: usize) -> Result<Value, WireError> {
        let n = n.clamp(1, 500);
        Ok(json!({
            "learning": self.db.recent_learning(n).map_err(other)?,
            "turns": self.db.recent_turns(n).map_err(other)?,
            "feelings": self.db.recent_feelings(n).map_err(other)?,
            "tools": self.db.tool_health().map_err(other)?
                .into_iter()
                .map(|(name, used, failed)| json!({"name": name, "used": used, "failed": failed}))
                .collect::<Vec<_>>(),
            "counters": self.db.counters().map_err(other)?,
            "turn_count": self.db.turn_count().unwrap_or(0),
        }))
    }

    // ------------------------------------------------------------ scheduling
    /// The current time, from the hub's own clock.
    pub fn now(&self) -> f64 {
        (self.clock)()
    }

    /// For tests and for a dry run: say whether the brain is answering.
    #[doc(hidden)]
    pub fn set_brain_up(&mut self, up: bool) {
        self.brain_up = up;
    }

    /// Replace the clock. For tests, so time can be moved deliberately.
    #[doc(hidden)]
    pub fn set_clock(&mut self, f: Box<dyn Fn() -> f64 + Send>) {
        self.clock = f;
        // Everything that was stamped with the old clock has to move with it, or the hub is
        // left comparing two different notions of now.
        let now = self.now();
        self.last_human = now;
        self.last_night = now;
        self.last_failure = 0.0;
    }

    /// What to do next. Alarms and expiries are folded into the queue here, so the scheduler
    /// itself holds no policy and cannot drift out of step with the state.
    fn next_duty(&mut self) -> Result<Duty, WireError> {
        let now = self.now();
        if self.active.is_some() {
            // A turn is already running. Say so with a short wait rather than queueing a
            // second one; two conscious turns at once is the thing this design forbids.
            return Ok(Duty::Idle(0.5));
        }
        // Asleep. A turn would need the brain, and the brain is busy becoming different.
        // Whatever arrives meanwhile waits in the queue, which is what sleeping means here.
        if self.napping.is_some() {
            return Ok(Duty::Idle(1.0));
        }
        // Not there yet. Loading the weights takes the best part of a minute, and a turn
        // started in that window fails for a reason that has nothing to do with the turn: it
        // would be counted against the message and the message eventually set down.
        if !self.brain_up {
            return Ok(Duty::Idle(2.0));
        }
        // After a failure, wait before trying again, longer each time. Without this, a brain
        // that is down becomes a loop that spawns a process as fast as the machine allows.
        if self.fail_streak > 0 {
            let wait = backoff(self.fail_streak);
            let since = now - self.last_failure;
            if since < wait {
                return Ok(Duty::Idle((wait - since).min(30.0)));
            }
        }

        for a in self.schedule.due(now).map_err(other)? {
            let sig = Signal::new(SignalKind::SignalAlarm, &a.text).with_meta(json!({"alarm": a.id}));
            self.inbox.push(&sig).map_err(io)?;
        }

        for id in self.thoughts.reap(pid_alive, now).map_err(io)? {
            self.fanout(Event::new(EventName::Thought, json!({"id": id, "status": "paused", "why": "its process went away"})));
        }

        if let Some(sig) = self.inbox.pop().map_err(io)? {
            if sig.kind.is_human() {
                self.last_human = now;
                self.idle_streak = 0;
            }
            let id = self.begin(sig.clone(), now);
            return Ok(Duty::Turn(Box::new(sig), id));
        }

        if let Some(t) = self.thoughts.ready(now).map_err(io)? {
            return Ok(Duty::Thought(t.id));
        }

        // With nothing waiting, this is the moment to digest what has happened. A night comes
        // round on the clock; a shorter pass comes round after a handful of turns.
        let hours = self.cfg.sleep_every_hours.max(0.0);
        if hours > 0.0 && now - self.last_night >= hours * 3600.0 {
            self.last_night = now;
            self.since_learned = 0;
            return Ok(Duty::Learn("night"));
        }
        if self.since_learned >= self.cfg.nap_max_samples.max(1) as u32 {
            self.since_learned = 0;
            return Ok(Duty::Learn("nap"));
        }

        // Nothing to do. Consider being curious, but less and less often.
        if self.cfg.curiosity {
            let gap = idle_gap(self.idle_streak, self.cfg.sense_idle_minutes, self.cfg.sense_idle_max_minutes);
            if now - self.last_human >= gap {
                self.idle_streak += 1;
                self.last_human = now;
                let sig = Signal::new(SignalKind::SignalIdle,
                    "Nothing is waiting for you. Pick something small you do not understand, look it up, and try it.");
                self.inbox.push(&sig).map_err(io)?;
                return Ok(Duty::Idle(0.2));
            }
        }
        Ok(Duty::Idle(2.0))
    }

    /// Open a turn for a signal. The epoch moves, which invalidates anything still holding the
    /// previous turn's credentials.
    ///
    /// What came in is written to the journal here, once, rather than by the process that
    /// handles it. A turn that fails and is retried would otherwise record the same message
    /// again on every attempt.
    fn begin(&mut self, sig: Signal, now: f64) -> String {
        self.epoch = self.epoch.next();
        let id = turn_id(now);
        let mut incoming = Message::user(frame(sig.kind, &sig.text, &sig.meta));
        incoming.kind = Some(sig.kind.as_str().to_string());
        if sig.attempts == 0 {
            if let Err(e) = self.write_record(&incoming, &id) {
                tracing::error!("could not record what came in: {e}");
            }
        }
        let _ = self.db.turn_started(&id, sig.kind.as_str(), now);
        self.fanout(Event::new(EventName::TurnStart, json!({
            "who": if sig.kind.is_human() { "user" } else { "signal" },
            "kind": sig.kind.as_str(),
            "text": sig.text,
            "turn": id,
        })));
        self.active = Some(Active {
            id: id.clone(), epoch: self.epoch, kind: sig.kind, started: now,
            signal: sig, claimed: false, rounds: 0,
        });
        id
    }

    /// Hand a harness process everything it needs. Called once per turn; a second call is
    /// refused so a stray process cannot steal a turn already in progress.
    fn claim(&mut self, pid: u32) -> Result<TurnContext, WireError> {
        let forget = self.db.turns_to_forget().unwrap_or_default();
        let window = restore_window_without(&self.journal, WINDOW, &forget).map_err(io)?;
        let birth_line = self.birth.line(self.now());
        // Only the part it cannot argue with. What else it sees at the start of a turn comes
        // from its own script, which the process running the turn executes in its home.
        let system = self.identity
            .system_prompt(&birth_line, self.cfg.identity_in_prompt)
            .map_err(io)?;
        let a = self.active.as_mut().ok_or(WireError::Idle)?;
        if a.claimed {
            return Err(WireError::Busy(format!("turn {} already has a process", a.id)));
        }
        a.claimed = true;
        tracing::debug!(turn = %a.id, pid, "turn claimed");
        Ok(TurnContext {
            turn: a.id.clone(),
            epoch: a.epoch.0,
            kind: a.kind as i32,
            text: a.signal.text.clone(),
            framed: frame(a.kind, &a.signal.text, &a.signal.meta),
            system,
            window,
            max_rounds: self.cfg.max_tool_rounds,
            meta: groow_proto::frame::to_struct(a.signal.meta.clone()),
        })
    }

    /// Check that a write belongs to the turn that is actually running.
    fn guard(&self, turn: &str, epoch: Epoch) -> Result<(), WireError> {
        match &self.active {
            Some(a) if a.id == turn && a.epoch == epoch => Ok(()),
            _ => Err(WireError::StaleEpoch(turn.to_string())),
        }
    }

    fn append(&mut self, turn: &str, epoch: Epoch, msg: Message) -> Result<Value, WireError> {
        self.guard(turn, epoch)?;
        let mut m = msg;
        if m.role == "user" {
            if let Some(a) = &self.active {
                m.kind = Some(a.kind.as_str().to_string());
            }
        }
        if let Some(a) = self.active.as_mut() {
            if m.role == "assistant" {
                a.rounds += 1;
            }
        }
        self.write_record(&m, turn).map_err(io)?;
        self.fanout(Event::new(EventName::Message, json!({
            "role": m.role,
            "content": m.content.clone().unwrap_or_default(),
            "turn": turn,
        })));
        Ok(json!({"ok": true}))
    }

    fn finish(&mut self, out: TurnOutcome) -> Result<Value, WireError> {
        self.guard(&out.turn, Epoch(out.epoch))?;
        let now = self.now();
        let a = self.active.take().expect("guard proved there is an active turn");
        let _ = self.db.turn_ended(&a.id, &crate::db::Ended {
            at: now, seconds: now - a.started, tools: out.tools_used, rounds: a.rounds,
            flags: out.flags.clone(), outcome: "ok".into(),
        });
        let _ = self.inbox.ack(&a.signal);
        self.fail_streak = 0;
        self.since_learned += 1;

        self.fanout(Event::new(EventName::TurnEnd, json!({
            "turn": a.id, "kind": a.kind.as_str(), "final": out.final_text,
            "tools_used": out.tools_used, "seconds": now - a.started, "flags": out.flags,
        })));
        Ok(json!({"ok": true, "seconds": now - a.started}))
    }

    /// Write one message to the journal, stamped with the turn it belongs to, so that a turn
    /// which went badly can later be left out of the window.
    fn write_record(&mut self, m: &Message, turn: &str) -> std::io::Result<()> {
        let mut v = serde_json::to_value(m).unwrap_or_else(|_| json!({}));
        if let Some(o) = v.as_object_mut() {
            o.insert("turn".into(), json!(turn));
        }
        self.journal.append(&v)?;
        Ok(())
    }

    /// The turn's process died. The signal goes back in the queue so nothing is lost, and the
    /// epoch moves so a late message from the corpse is refused.
    fn abandon(&mut self, turn: &str, why: &str) {
        let Some(a) = self.active.as_ref().filter(|a| a.id == turn).cloned() else { return };
        self.active = None;
        self.epoch = self.epoch.next();
        let now = self.now();
        let _ = self.db.turn_ended(&a.id, &crate::db::Ended {
            at: now, seconds: now - a.started, tools: 0, rounds: a.rounds,
            flags: vec!["abandoned".into()], outcome: why.to_string(),
        });
        self.fail_streak = self.fail_streak.saturating_add(1);
        self.last_failure = now;

        // A person's message is worth another try; a nudge is not worth looping on. Either
        // way there is a limit, so a message that kills every process it touches is
        // eventually set down rather than retried forever.
        let give_up = a.signal.attempts + 1 >= MAX_ATTEMPTS;
        if a.kind.is_human() && !give_up {
            let _ = self.inbox.requeue(&a.signal);
        } else {
            let _ = self.inbox.ack(&a.signal);
            if a.kind.is_human() {
                let note = Message::assistant(format!(
                    "I could not answer that: {why}. It has been tried {} times and I am setting it down.",
                    a.signal.attempts + 1
                ));
                // Stamped with the turn that failed, so this apology is never replayed to it
                // as an example of how to answer.
                let _ = self.write_record(&note, &a.id);
                self.fanout(Event::new(EventName::TurnEnd, json!({
                    "turn": a.id, "kind": a.kind.as_str(), "final": note.text(),
                    "tools_used": 0, "seconds": now - a.started, "flags": ["abandoned"],
                })));
            }
        }
        self.fanout(Event::new(EventName::Log, json!({
            "level": "error", "text": format!("turn {} ended badly: {why}", a.id),
        })));
    }

    // ------------------------------------------------------------ the mind's own ops
    fn say(&mut self, text: &str, kind: SignalKind, meta: Value) -> Result<Value, WireError> {
        if text.trim().is_empty() {
            return Err(WireError::BadArg("nothing to say".into()));
        }
        let sig = Signal::new(kind, text).with_meta(meta);
        self.inbox.push(&sig).map_err(io)?;
        if kind.is_human() {
            self.last_human = self.now();
            self.idle_streak = 0;
        }
        Ok(json!({"ok": true, "queued": kind.as_str()}))
    }

    fn think(&mut self, goal: &str, max_steps: u32) -> Result<Value, WireError> {
        if goal.trim().is_empty() {
            return Err(WireError::BadArg("a thought needs a goal".into()));
        }
        // Only thoughts actually being worked on count. A paused one is set aside and should
        // not stop the mind starting something it wants to do now; it is told about them so it
        // can resume one instead of starting another.
        let running = self.thoughts.running().map_err(io)?;
        if running.len() >= self.cfg.max_thoughts {
            let names: Vec<String> = running.iter().map(|t| format!("{} ({})", t.id, t.goal)).collect();
            return Err(WireError::Busy(format!(
                "you already have {} thoughts running: {}. Finish or kill one first.",
                running.len(),
                names.join("; ")
            )));
        }
        let system = format!(
            "You are one of Groow's inner thoughts, working alone on a single goal.\n\nGoal: {goal}\n\n\
             Work in small concrete steps with your tools. Call `focus` to tell the main thread \
             something it needs now, and `finish` with a summary when the goal is reached.");
        let t = self.thoughts.spawn(goal, max_steps, &system).map_err(io)?;
        self.fanout(Event::new(EventName::Thought, json!({
            "id": t.id, "status": "running", "goal": t.goal, "event": "spawn",
        })));
        Ok(json!({"ok": true, "id": t.id, "max_steps": t.max_steps}))
    }

    fn thought_action(&mut self, action: &str, id: &str, text: &str) -> Result<Value, WireError> {
        // Listing is the one action that is not about a particular thought, so it comes before
        // looking one up. Without this `groow thoughts` asked for the thought called "" and was
        // told there was no such thing.
        if action == "list" || action.is_empty() {
            let all = self.thoughts.all().map_err(io)?;
            return Ok(json!({
                "thoughts": all.iter().map(|t| json!({
                    "id": t.id, "goal": t.goal, "status": t.status.as_str(),
                    "steps": t.steps, "max_steps": t.max_steps, "summary": t.summary,
                })).collect::<Vec<_>>(),
                "running": all.iter().filter(|t| t.status.as_str() == "running").count(),
                "max": self.cfg.max_thoughts,
            }));
        }
        let t = self.thoughts.get(id).map_err(io)?
            .ok_or_else(|| WireError::NotFound("thought", id.to_string()))?;
        let out = match action {
            "read" => json!({
                "id": t.id, "goal": t.goal, "status": t.status.as_str(),
                "steps": t.steps, "max_steps": t.max_steps, "summary": t.summary,
            }),
            "pause" | "resume" | "kill" | "finish" | "focus" => {
                if t.status.is_final() && (action == "pause" || action == "resume") {
                    return Err(WireError::BadArg(format!("thought {id} has already finished")));
                }
                let (status, summary) = match action {
                    "pause" => (Some(ThoughtStatus::Paused), None),
                    "resume" => (Some(ThoughtStatus::Running), None),
                    "kill" => (Some(ThoughtStatus::Killed), Some(if text.is_empty() { "killed".to_string() } else { text.to_string() })),
                    "finish" => (Some(ThoughtStatus::Done), Some(text.to_string())),
                    _ => (None, None),
                };
                self.thoughts.patch(id, |t| {
                    if let Some(s) = status { t.status = s; }
                    if let Some(s) = summary { t.summary = s; }
                    if matches!(status, Some(ThoughtStatus::Paused) | Some(ThoughtStatus::Killed) | Some(ThoughtStatus::Done)) {
                        t.pid = None;
                    }
                }).map_err(io)?;
                // A thought that reached the main thread queues a signal for it.
                if action == "focus" {
                    let sig = Signal::new(SignalKind::SignalFocus, text).with_meta(json!({"thought": id}));
                    self.inbox.push(&sig).map_err(io)?;
                } else if action == "finish" {
                    let sig = Signal::new(SignalKind::SignalThoughtDone, text).with_meta(json!({"thought": id}));
                    self.inbox.push(&sig).map_err(io)?;
                }
                self.fanout(Event::new(EventName::Thought, json!({
                    "id": id, "event": action, "text": text, "goal": t.goal,
                    "status": status.map(|s| s.as_str()).unwrap_or(t.status.as_str()),
                })));
                json!({"ok": true, "id": id, "action": action})
            }
            other => return Err(WireError::BadArg(format!("no such action `{other}`"))),
        };
        Ok(out)
    }

    /// Hand a thought process its next step.
    ///
    /// A thought's context is its own trace, not the conversation: it is working alone, and
    /// what the person said has nothing to do with it. One process takes one step, which keeps
    /// every process short and leaves the core in charge of whether there is another.
    fn thought_claim(&mut self, id: &str, pid: u32) -> Result<TurnContext, WireError> {
        let t = self.thoughts.get(id).map_err(io)?
            .ok_or_else(|| WireError::NotFound("thought", id.to_string()))?;
        if t.status != ThoughtStatus::Running {
            return Err(WireError::BadArg(format!("thought {id} is {}", t.status.as_str())));
        }
        if t.steps >= t.max_steps {
            return Err(WireError::BadArg(format!("thought {id} has used its {} steps", t.max_steps)));
        }
        if let Some(other) = t.pid.filter(|p| *p != pid && pid_alive(*p)) {
            return Err(WireError::Busy(format!("thought {id} is already being run by {other}")));
        }
        self.thoughts.patch(id, |t| t.pid = Some(pid)).map_err(io)?;

        let system = t.history.first().map(|m| m.text().to_string()).unwrap_or_default();
        let window: Vec<Message> = t.history.iter().skip(1).cloned().collect();
        let framed = if t.steps == 0 {
            "Begin. Say briefly what you will do, then take the first concrete step with your tools.".to_string()
        } else {
            format!(
                "Step {} of {}. Carry on toward the goal. If you have reached it, call finish with what you found.",
                t.steps + 1, t.max_steps
            )
        };
        Ok(TurnContext {
            turn: t.id.clone(),
            epoch: t.steps,
            kind: SignalKind::SignalIdle as i32,
            text: t.goal.clone(),
            framed,
            system,
            window,
            max_rounds: self.cfg.max_tool_rounds,
            meta: groow_proto::frame::to_struct(json!({"thought": t.id})),
        })
    }

    fn thought_append(&mut self, id: &str, msg: Message) -> Result<Value, WireError> {
        let role = msg.role.clone();
        let content = msg.content.clone().unwrap_or_default();
        self.thoughts.patch(id, |t| t.history.push(msg)).map_err(io)?
            .ok_or_else(|| WireError::NotFound("thought", id.to_string()))?;
        // Whoever writes the record announces it, here as on the main thread. Anything else
        // means two places deciding what was said, and a watcher hearing it twice.
        self.fanout(Event::new(EventName::Message, json!({
            "role": role, "content": content, "turn": id,
        })));
        Ok(json!({"ok": true}))
    }

    /// Close out one step. The thought stops when it has spent its budget, and says so, rather
    /// than being cut off without a word.
    fn thought_end(&mut self, id: &str, final_text: &str, flags: &[String]) -> Result<Value, WireError> {
        let t = self.thoughts.patch(id, |t| {
            t.steps += 1;
            t.attempts = 0;
            t.pid = None;
            if t.status == ThoughtStatus::Running && t.steps >= t.max_steps {
                t.status = ThoughtStatus::Done;
                if t.summary.is_empty() {
                    t.summary = if final_text.is_empty() {
                        "it used all its steps without reaching the goal".to_string()
                    } else {
                        final_text.to_string()
                    };
                }
            }
        }).map_err(io)?.ok_or_else(|| WireError::NotFound("thought", id.to_string()))?;

        if t.status.is_final() {
            let sig = Signal::new(SignalKind::SignalThoughtDone, &t.summary).with_meta(json!({"thought": t.id}));
            self.inbox.push(&sig).map_err(io)?;
        }
        self.fanout(Event::new(EventName::Thought, json!({
            "id": t.id, "status": t.status.as_str(), "steps": t.steps,
            "goal": t.goal, "text": final_text, "flags": flags,
        })));
        Ok(json!({"ok": true, "steps": t.steps, "status": t.status.as_str()}))
    }

    /// A thought's process failed without taking a step. After a few of those it is parked,
    /// because respawning something that cannot start is a loop, not persistence.
    fn thought_failed(&mut self, id: &str, why: &str) {
        let parked = self.thoughts.patch(id, |t| {
            t.pid = None;
            t.attempts += 1;
            if t.attempts >= crate::store::thoughts::MAX_ATTEMPTS {
                t.status = ThoughtStatus::Paused;
                t.summary = format!("paused: its process kept failing ({why})");
            }
        }).ok().flatten();
        if let Some(t) = parked {
            if t.status == ThoughtStatus::Paused {
                self.fanout(Event::new(EventName::Thought, json!({
                    "id": t.id, "status": "paused", "goal": t.goal, "text": t.summary,
                })));
            }
        }
    }

    fn schedule_action(&mut self, action: &str, text: &str, when: &str, every: &str, id: &str, by: &str)
        -> Result<Value, WireError>
    {
        match action {
            "list" | "" => {
                let all = self.schedule.all().map_err(other)?;
                Ok(json!({"alarms": all}))
            }
            "add" => {
                let w = if when.is_empty() { None } else { Some(when) };
                let e = if every.is_empty() { None } else { Some(every) };
                let a = self.schedule.add(text, w, e, by).map_err(|e| WireError::BadArg(e.to_string()))?;
                Ok(json!({"ok": true, "id": a.id, "when": a.when}))
            }
            "cancel" => {
                let n = self.schedule.cancel(id).map_err(other)?;
                if n == 0 {
                    return Err(WireError::NotFound("alarm", id.to_string()));
                }
                Ok(json!({"ok": true, "cancelled": n}))
            }
            other => Err(WireError::BadArg(format!("no such schedule action `{other}`"))),
        }
    }

    // ------------------------------------------------------------ for tests
    pub fn paths(&self) -> &Paths { &self.paths }
    pub fn db(&self) -> &Db { &self.db }
    pub fn active_turn(&self) -> Option<(String, Epoch)> {
        self.active.as_ref().map(|a| (a.id.clone(), a.epoch))
    }
}

fn send<T>(tx: Answer<T>, v: Result<T, WireError>) {
    // The caller may have given up. That is not an error worth reporting.
    let _ = tx.send(v);
}

fn io(e: std::io::Error) -> WireError {
    WireError::Internal(e.to_string())
}

fn other(e: anyhow::Error) -> WireError {
    WireError::Internal(e.to_string())
}

/// Sortable and unique: the time it began, plus a counter for turns within the same moment.
fn turn_id(now: f64) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    format!("{:014}-{:04}", (now * 1000.0) as u64, N.fetch_add(1, Ordering::Relaxed) % 10_000)
}

/// Whether a process is still running. Signal zero checks for existence without disturbing it.
fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    unsafe { libc::kill(pid as i32, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
    #[cfg(not(unix))]
    { let _ = pid; true }
}

/// Shared by the tests in this module and the integration tests.
#[doc(hidden)]
pub fn hub_for_test(dir: &std::path::Path) -> anyhow::Result<Hub> {
    let cfg = Config {
        state_dir: dir.to_string_lossy().to_string(),
        curiosity: false,
        ..Default::default()
    };
    let paths = Paths::new(dir);
    paths.ensure()?;
    let birth = Birth::load_or_create(&paths.birth(), "test/model", "test", "Marlinski", "0.3.0")?;
    Hub::new(cfg, paths, birth, Db::memory()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clock the test moves by hand, shared with the hub.
    #[derive(Clone)]
    struct Clock(std::sync::Arc<std::sync::atomic::AtomicU64>);

    impl Clock {
        fn new(at: f64) -> Clock {
            Clock(std::sync::Arc::new(std::sync::atomic::AtomicU64::new((at * 1000.0) as u64)))
        }
        fn set(&self, at: f64) {
            self.0.store((at * 1000.0) as u64, std::sync::atomic::Ordering::SeqCst);
        }
        fn advance(&self, by: f64) {
            self.set(self.read() + by);
        }
        fn read(&self) -> f64 {
            self.0.load(std::sync::atomic::Ordering::SeqCst) as f64 / 1000.0
        }
        fn install(&self, h: &mut Hub) {
            let c = self.clone();
            h.set_clock(Box::new(move || c.read()));
        }
    }

    fn hub() -> (Hub, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let mut h = hub_for_test(d.path()).unwrap();
        h.brain_up = true;
        (h, d)
    }

    fn hub_at(at: f64) -> (Hub, Clock, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let mut h = hub_for_test(d.path()).unwrap();
        h.brain_up = true;
        let c = Clock::new(at);
        c.install(&mut h);
        (h, c, d)
    }

    fn take<T>(f: impl FnOnce(Answer<T>) -> Cmd, h: &mut Hub) -> Result<T, WireError> {
        let (tx, rx) = oneshot::channel();
        h.handle(f(tx));
        rx.blocking_recv().expect("the hub always answers")
    }

    #[test]
    fn a_signal_becomes_a_turn_and_the_turn_can_be_claimed() {
        let (mut h, _d) = hub();
        take(|reply| Cmd::Say { text: "hello".into(), kind: SignalKind::SignalUser, meta: json!({}), reply }, &mut h).unwrap();
        let duty = take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
        assert!(matches!(duty, Duty::Turn(_, _)));
        let ctx = take(|reply| Cmd::Claim { pid: 1, reply }, &mut h).unwrap();
        assert_eq!(ctx.text, "hello");
        assert_eq!(ctx.framed, "hello", "a person is not framed");
        assert!(ctx.window.is_empty());
        assert!(!ctx.system.is_empty());
    }

    #[test]
    fn only_one_turn_runs_at_a_time() {
        let (mut h, _d) = hub();
        for i in 0..3 {
            take(|reply| Cmd::Say { text: format!("m{i}"), kind: SignalKind::SignalUser, meta: json!({}), reply }, &mut h).unwrap();
        }
        assert!(matches!(take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap(), Duty::Turn(_, _)));
        let second = take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
        assert!(matches!(second, Duty::Idle(_)), "a second turn must not open while one runs");
    }

    #[test]
    fn a_turn_can_only_be_claimed_once() {
        let (mut h, _d) = hub();
        take(|reply| Cmd::Say { text: "hi".into(), kind: SignalKind::SignalUser, meta: json!({}), reply }, &mut h).unwrap();
        take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
        take(|reply| Cmd::Claim { pid: 1, reply }, &mut h).unwrap();
        let again = take(|reply| Cmd::Claim { pid: 2, reply }, &mut h);
        assert!(matches!(again, Err(WireError::Busy(_))), "a stray process must not steal a turn");
    }

    #[test]
    fn claiming_with_nothing_to_do_says_so() {
        let (mut h, _d) = hub();
        assert!(matches!(take(|reply| Cmd::Claim { pid: 1, reply }, &mut h), Err(WireError::Idle)));
    }

    #[test]
    fn a_write_from_a_finished_turn_is_refused() {
        let (mut h, _d) = hub();
        take(|reply| Cmd::Say { text: "hi".into(), kind: SignalKind::SignalUser, meta: json!({}), reply }, &mut h).unwrap();
        take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
        let (turn, epoch) = h.active_turn().unwrap();
        take(|reply| Cmd::Finish {
            outcome: Box::new(TurnOutcome {
                turn: turn.clone(), epoch: epoch.0, final_text: "done".into(),
                flags: vec![], tools_used: 0, seconds: 1.0,
            }), reply,
        }, &mut h).unwrap();

        let late = take(|reply| Cmd::Append {
            turn: turn.clone(), epoch, msg: Box::new(Message::assistant("too late")), reply,
        }, &mut h);
        assert!(matches!(late, Err(WireError::StaleEpoch(_))));
    }

    #[test]
    fn a_write_from_a_previous_life_is_refused() {
        let (mut h, _d) = hub();
        take(|reply| Cmd::Say { text: "one".into(), kind: SignalKind::SignalUser, meta: json!({}), reply }, &mut h).unwrap();
        take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
        let (turn, old_epoch) = h.active_turn().unwrap();
        h.handle(Cmd::Abandon { turn: turn.clone(), why: "process died".into() });

        let ghost = take(|reply| Cmd::Append {
            turn, epoch: old_epoch, msg: Box::new(Message::assistant("from the grave")), reply,
        }, &mut h);
        assert!(matches!(ghost, Err(WireError::StaleEpoch(_))));
    }

    #[test]
    fn an_abandoned_human_message_is_tried_again() {
        let (mut h, clock, _d) = hub_at(1000.0);
        take(|reply| Cmd::Say { text: "please answer".into(), kind: SignalKind::SignalUser, meta: json!({}), reply }, &mut h).unwrap();
        take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
        let (turn, _) = h.active_turn().unwrap();
        h.handle(Cmd::Abandon { turn, why: "crash".into() });
        clock.advance(100.0);
        let duty = take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
        match duty {
            Duty::Turn(s, _) => assert_eq!(s.text, "please answer"),
            other => panic!("a person's message was dropped: {other:?}"),
        }
    }

    #[test]
    fn an_abandoned_nudge_is_not_retried_forever() {
        let (mut h, _d) = hub();
        take(|reply| Cmd::Say { text: "be curious".into(), kind: SignalKind::SignalIdle, meta: json!({}), reply }, &mut h).unwrap();
        take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
        let (turn, _) = h.active_turn().unwrap();
        h.handle(Cmd::Abandon { turn, why: "crash".into() });
        assert!(matches!(take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap(), Duty::Idle(_)));
    }

    #[test]
    fn signals_that_are_not_from_a_person_are_framed() {
        let (mut h, _d) = hub();
        take(|reply| Cmd::Say { text: "the kettle".into(), kind: SignalKind::SignalAlarm, meta: json!({}), reply }, &mut h).unwrap();
        take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
        let ctx = take(|reply| Cmd::Claim { pid: 1, reply }, &mut h).unwrap();
        assert!(ctx.framed.starts_with("[an alarm you set earlier]"));
        assert_eq!(ctx.text, "the kettle", "the raw text is kept alongside");
    }

    #[test]
    fn the_window_comes_back_on_the_next_turn() {
        let (mut h, _d) = hub();
        take(|reply| Cmd::Say { text: "what is a river".into(), kind: SignalKind::SignalUser, meta: json!({}), reply }, &mut h).unwrap();
        take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
        let (turn, epoch) = h.active_turn().unwrap();
        take(|reply| Cmd::Append { turn: turn.clone(), epoch, msg: Box::new(Message::user("what is a river")), reply }, &mut h).unwrap();
        take(|reply| Cmd::Append { turn: turn.clone(), epoch, msg: Box::new(Message::assistant("water going downhill")), reply }, &mut h).unwrap();
        take(|reply| Cmd::Finish { outcome: Box::new(TurnOutcome {
            turn, epoch: epoch.0, final_text: "water going downhill".into(), flags: vec![], tools_used: 0, seconds: 1.0,
        }), reply }, &mut h).unwrap();

        take(|reply| Cmd::Say { text: "and a lake".into(), kind: SignalKind::SignalUser, meta: json!({}), reply }, &mut h).unwrap();
        take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
        let ctx = take(|reply| Cmd::Claim { pid: 1, reply }, &mut h).unwrap();
        assert_eq!(ctx.window.len(), 2, "the previous exchange should be there");
        assert_eq!(ctx.window[0].text(), "what is a river");
    }

    #[test]
    fn thoughts_are_capped_and_can_be_driven() {
        let (mut h, _d) = hub();
        let mut ids = Vec::new();
        for i in 0..4 {
            let r = take(|reply| Cmd::Think { goal: format!("goal {i}"), max_steps: 5, reply }, &mut h).unwrap();
            ids.push(r["id"].as_str().unwrap().to_string());
        }
        let too_many = take(|reply| Cmd::Think { goal: "one more".into(), max_steps: 5, reply }, &mut h);
        assert!(matches!(too_many, Err(WireError::Busy(_))));

        take(|reply| Cmd::ThoughtAction { action: "kill".into(), id: ids[0].clone(), text: "not useful".into(), reply }, &mut h).unwrap();
        take(|reply| Cmd::Think { goal: "now there is room".into(), max_steps: 5, reply }, &mut h).unwrap();
    }

    #[test]
    fn a_thought_reaching_back_queues_a_signal_for_the_main_thread() {
        let (mut h, _d) = hub();
        let r = take(|reply| Cmd::Think { goal: "look into rivers".into(), max_steps: 5, reply }, &mut h).unwrap();
        let id = r["id"].as_str().unwrap().to_string();
        take(|reply| Cmd::ThoughtAction { action: "focus".into(), id: id.clone(), text: "found something".into(), reply }, &mut h).unwrap();
        let duty = take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
        match duty {
            Duty::Turn(s, _) => {
                assert_eq!(s.kind, SignalKind::SignalFocus);
                assert_eq!(s.meta["thought"], id);
            }
            other => panic!("the main thread was not woken: {other:?}"),
        }
    }

    #[test]
    fn an_alarm_that_comes_due_wakes_the_mind() {
        let (mut h, _d) = hub();
        take(|reply| Cmd::ScheduleAction {
            action: "add".into(), text: "read the news".into(), when: "in 0s".into(),
            every: String::new(), id: String::new(), by: "groow".into(), reply,
        }, &mut h).unwrap();
        let duty = take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
        match duty {
            Duty::Turn(s, _) => assert_eq!(s.kind, SignalKind::SignalAlarm),
            other => panic!("the alarm did not fire: {other:?}"),
        }
    }

    #[test]
    fn what_came_in_is_recorded_once_even_when_the_turn_is_retried() {
        let (mut h, clock, _d) = hub_at(1000.0);
        take(|reply| Cmd::Say { text: "answer me".into(), kind: SignalKind::SignalUser, meta: json!({}), reply }, &mut h).unwrap();
        for _ in 0..2 {
            clock.advance(200.0);
            take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
            let (turn, _) = h.active_turn().expect("a turn should be open");
            h.handle(Cmd::Abandon { turn, why: "the brain is down".into() });
        }
        let recs = h.journal.tail(50).unwrap();
        let mine = recs.iter().filter(|r| r["content"] == "answer me").count();
        assert_eq!(mine, 1, "a retried turn duplicated the message: {recs:?}");
    }

    #[test]
    fn a_failing_turn_is_given_up_on_rather_than_retried_forever() {
        let (mut h, clock, _d) = hub_at(1000.0);
        take(|reply| Cmd::Say { text: "this will fail".into(), kind: SignalKind::SignalUser, meta: json!({}), reply }, &mut h).unwrap();
        let mut attempts = 0;
        for _ in 0..10 {
            clock.advance(1000.0);
            match take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap() {
                Duty::Turn(_, _) => {
                    attempts += 1;
                    let (turn, _) = h.active_turn().unwrap();
                    h.handle(Cmd::Abandon { turn, why: "the brain is down".into() });
                }
                Duty::Idle(_) => {}
                other => panic!("unexpected duty: {other:?}"),
            }
        }
        assert_eq!(attempts, 3, "it should try a few times and then set it down");
        let recs = h.journal.tail(50).unwrap();
        let said = recs.iter().any(|r| r["content"].as_str().unwrap_or("").contains("setting it down"));
        assert!(said, "a person should be told it gave up: {recs:?}");
    }

    #[test]
    fn a_failure_is_waited_out_before_trying_again() {
        let (mut h, clock, _d) = hub_at(1000.0);
        take(|reply| Cmd::Say { text: "hello".into(), kind: SignalKind::SignalUser, meta: json!({}), reply }, &mut h).unwrap();
        take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
        let (turn, _) = h.active_turn().unwrap();
        h.handle(Cmd::Abandon { turn, why: "crash".into() });

        // Immediately afterwards there is nothing to do but wait.
        clock.advance(0.5);
        match take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap() {
            Duty::Idle(secs) => assert!(secs > 0.0),
            other => panic!("it went straight back into a failing turn: {other:?}"),
        }
        // Once the wait is over it tries again.
        clock.advance(100.0);
        assert!(matches!(take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap(), Duty::Turn(_, _)));
    }

    #[test]
    fn the_wait_after_a_failure_grows_and_is_capped() {
        let gaps: Vec<f64> = (1..8).map(backoff).collect();
        assert!(gaps.windows(2).all(|w| w[1] >= w[0]), "{gaps:?}");
        assert_eq!(gaps[0], 2.0);
        assert!(*gaps.last().unwrap() <= 60.0, "the wait must not grow without limit");
    }

    #[test]
    fn a_turn_that_finishes_clears_the_failure_streak() {
        let (mut h, clock, _d) = hub_at(1000.0);
        take(|reply| Cmd::Say { text: "one".into(), kind: SignalKind::SignalUser, meta: json!({}), reply }, &mut h).unwrap();
        take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
        let (turn, _) = h.active_turn().unwrap();
        h.handle(Cmd::Abandon { turn, why: "crash".into() });
        assert_eq!(h.fail_streak, 1);

        clock.advance(100.0);
        take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
        let (turn, epoch) = h.active_turn().unwrap();
        take(|reply| Cmd::Finish { outcome: Box::new(TurnOutcome {
            turn, epoch: epoch.0, final_text: "done".into(), flags: vec![], tools_used: 0, seconds: 1.0,
        }), reply }, &mut h).unwrap();
        assert_eq!(h.fail_streak, 0, "one good turn should clear the backoff");
    }

    #[test]
    fn nothing_is_started_before_the_brain_can_answer() {
        let (mut h, clock, _d) = hub_at(1000.0);
        h.brain_up = false;
        take(|reply| Cmd::Say { text: "hello".into(), kind: SignalKind::SignalUser, meta: json!({}), reply }, &mut h).unwrap();
        clock.advance(10.0);
        assert!(matches!(take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap(), Duty::Idle(_)),
            "a turn was started while the weights were still loading");
        let s = take(|reply| Cmd::Status { reply }, &mut h).unwrap();
        assert_eq!(s["brain"], false);
        assert_eq!(s["mood"], "waking");

        h.handle(Cmd::BrainState { up: true });
        clock.advance(1.0);
        match take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap() {
            Duty::Turn(s, _) => assert_eq!(s.text, "hello", "and it is still there when the brain wakes"),
            other => panic!("the message was lost: {other:?}"),
        }
    }

    #[test]
    fn nothing_is_started_while_it_is_asleep() {
        let (mut h, clock, _d) = hub_at(1000.0);
        take(|reply| Cmd::Say { text: "hello".into(), kind: SignalKind::SignalUser, meta: json!({}), reply }, &mut h).unwrap();
        h.handle(Cmd::Napping { what: Some("night".into()) });
        clock.advance(10.0);
        assert!(matches!(take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap(), Duty::Idle(_)),
            "a turn was started while the brain was busy changing itself");
        let s = take(|reply| Cmd::Status { reply }, &mut h).unwrap();
        assert_eq!(s["napping"], "night");
        assert_eq!(s["mood"], "napping");
        assert_eq!(s["queue"], 1, "and what arrived meanwhile is still waiting");

        // When it wakes, the message is still there.
        h.handle(Cmd::Napping { what: None });
        clock.advance(1.0);
        match take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap() {
            Duty::Turn(s, _) => assert_eq!(s.text, "hello"),
            other => panic!("it did not pick up where it left off: {other:?}"),
        }
    }

    #[test]
    fn after_enough_turns_it_stops_to_digest_them() {
        let (mut h, clock, _d) = hub_at(1000.0);
        h.cfg.nap_max_samples = 2;
        h.cfg.sleep_every_hours = 0.0;
        for i in 0..2 {
            take(|reply| Cmd::Say { text: format!("m{i}"), kind: SignalKind::SignalUser, meta: json!({}), reply }, &mut h).unwrap();
            clock.advance(10.0);
            take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
            let (turn, epoch) = h.active_turn().unwrap();
            take(|reply| Cmd::Finish { outcome: Box::new(TurnOutcome {
                turn, epoch: epoch.0, final_text: "ok".into(), flags: vec![], tools_used: 0, seconds: 1.0,
            }), reply }, &mut h).unwrap();
        }
        clock.advance(10.0);
        assert_eq!(take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap(), Duty::Learn("nap"));
        // And not again straight away.
        clock.advance(10.0);
        assert!(matches!(take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap(), Duty::Idle(_)));
    }

    #[test]
    fn a_night_comes_round_on_the_clock() {
        let (mut h, clock, _d) = hub_at(1000.0);
        h.cfg.sleep_every_hours = 1.0;
        clock.advance(10.0);
        assert!(matches!(take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap(), Duty::Idle(_)));
        clock.advance(3700.0);
        assert_eq!(take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap(), Duty::Learn("night"));
    }

    #[test]
    fn a_person_waiting_always_comes_before_digesting_the_day() {
        let (mut h, clock, _d) = hub_at(1000.0);
        h.cfg.sleep_every_hours = 1.0;
        clock.advance(7200.0);
        take(|reply| Cmd::Say { text: "hello".into(), kind: SignalKind::SignalUser, meta: json!({}), reply }, &mut h).unwrap();
        match take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap() {
            Duty::Turn(s, _) => assert_eq!(s.text, "hello"),
            other => panic!("it went to sleep with someone waiting: {other:?}"),
        }
    }

    #[test]
    fn a_message_is_announced_by_whoever_records_it() {
        // On the main thread and inside a thought alike. If the core stays quiet for one of
        // them, whatever wrote it announces it instead, and then the two disagree.
        let (mut h, _d) = hub();
        let mut watching = take(|reply| Cmd::Subscribe { reply }, &mut h).unwrap();

        take(|reply| Cmd::Say { text: "hello".into(), kind: SignalKind::SignalUser, meta: json!({}), reply }, &mut h).unwrap();
        take(|reply| Cmd::NextDuty { reply }, &mut h).unwrap();
        let ctx = take(|reply| Cmd::Claim { pid: 1, reply }, &mut h).unwrap();
        take(|reply| Cmd::Append {
            turn: ctx.turn.clone(), epoch: Epoch(ctx.epoch),
            msg: Box::new(Message::assistant("hello yourself")), reply,
        }, &mut h).unwrap();

        let id = take(|reply| Cmd::Think { goal: "learn a thing".into(), max_steps: 4, reply }, &mut h)
            .unwrap()["id"].as_str().unwrap().to_string();
        take(|reply| Cmd::ThoughtAppend {
            id: id.clone(), msg: Box::new(Message::assistant("a step")), reply,
        }, &mut h).unwrap();

        let mut said = Vec::new();
        while let Ok(ev) = watching.try_recv() {
            if ev.name == "message" {
                said.push(ev.data["content"].as_str().unwrap_or("").to_string());
            }
        }
        assert_eq!(said, vec!["hello yourself".to_string(), "a step".to_string()], "{said:?}");
    }

    #[test]
    fn empty_requests_are_refused_rather_than_queued() {
        let (mut h, _d) = hub();
        assert!(take(|reply| Cmd::Say { text: "  ".into(), kind: SignalKind::SignalUser, meta: json!({}), reply }, &mut h).is_err());
        assert!(take(|reply| Cmd::Think { goal: " ".into(), max_steps: 3, reply }, &mut h).is_err());
    }

    #[test]
    fn unknown_actions_are_named_in_the_error() {
        let (mut h, _d) = hub();
        let e = take(|reply| Cmd::ScheduleAction {
            action: "burn".into(), text: String::new(), when: String::new(),
            every: String::new(), id: String::new(), by: "mentor".into(), reply,
        }, &mut h).unwrap_err();
        assert!(e.to_string().contains("burn"), "the error should say what was wrong: {e}");
    }

    #[test]
    fn the_conversation_can_be_read_back_a_page_at_a_time() {
        // What a window does when someone scrolls past the top: ask for what came before the
        // oldest line it holds, until nothing older comes back.
        let (mut h, _d) = hub();
        for i in 0..25 {
            let mut m = Message::assistant(format!("line {i}"));
            m.ts = Some(1000.0 + i as f64);
            h.journal.append_message(&m).unwrap();
        }

        let first = take(|reply| Cmd::Recall { n: 10, before: None, reply }, &mut h).unwrap();
        let newest = first["messages"].as_array().unwrap();
        assert_eq!(newest.len(), 10);
        assert_eq!(newest[9]["content"], "line 24", "the newest page ends at the newest line");
        assert_eq!(first["more"], true);

        let oldest_held = newest[0]["ts"].as_f64().unwrap();
        let next = take(|reply| Cmd::Recall { n: 10, before: Some(oldest_held), reply }, &mut h).unwrap();
        let page = next["messages"].as_array().unwrap();
        assert_eq!(page[9]["content"], "line 14", "the page before it ends where the first began");

        let rest = take(|reply| Cmd::Recall { n: 10, before: Some(1000.0), reply }, &mut h).unwrap();
        assert!(rest["messages"].as_array().unwrap().is_empty());
        assert_eq!(rest["more"], false, "and then it stops asking");
    }

    #[test]
    fn what_the_meta_processes_did_is_readable_but_not_by_the_mind() {
        let (mut h, _d) = hub();
        h.db.learned(10.0, "night", 12, Some(0.42), "merged the overlay").unwrap();
        h.db.learned(20.0, "nap", 4, Some(0.31), "").unwrap();
        h.db.felt("t1", 21.0, 0.2, Some(0.9), 0.5).unwrap();

        let v = take(|reply| Cmd::Stats { n: 10, reply }, &mut h).unwrap();
        let passes = v["learning"].as_array().unwrap();
        assert_eq!(passes[0]["kind"], "nap", "most recent first");
        assert_eq!(passes[1]["note"], "merged the overlay");
        assert_eq!(v["feelings"].as_array().unwrap()[0]["valence"], 0.5);

        // The mind must never be able to read how it is being scored.
        assert!(!groow_proto::ops::Op::Stats.allowed_for(groow_proto::ops::Role::Agent));
        assert!(!groow_proto::ops::Op::Stats.allowed_for(groow_proto::ops::Role::Viewer));
    }

    #[test]
    fn events_reach_every_subscriber() {
        let (mut h, _d) = hub();
        let mut a = take(|reply| Cmd::Subscribe { reply }, &mut h).unwrap();
        let mut b = take(|reply| Cmd::Subscribe { reply }, &mut h).unwrap();
        h.handle(Cmd::Emit { event: Event::log("info", "hello both") });
        for (who, rx) in [("a", &mut a), ("b", &mut b)] {
            let ev = rx.try_recv().unwrap_or_else(|_| panic!("{who} got nothing"));
            assert_eq!(ev.name, "log");
        }
    }

    #[test]
    fn a_subscriber_that_stops_reading_cannot_stall_the_core() {
        let (mut h, _d) = hub();
        let _stuck = take(|reply| Cmd::Subscribe { reply }, &mut h).unwrap();
        // Far more events than the queue can hold. This must simply return.
        for i in 0..(SUBSCRIBER_DEPTH * 3) {
            h.handle(Cmd::Emit { event: Event::log("info", format!("{i}")) });
        }
        // And the core still works afterwards.
        let s = take(|reply| Cmd::Status { reply }, &mut h).unwrap();
        assert!(s["queue"].is_number());
    }

    #[test]
    fn a_dropped_subscriber_is_forgotten() {
        let (mut h, _d) = hub();
        let rx = take(|reply| Cmd::Subscribe { reply }, &mut h).unwrap();
        assert_eq!(h.subscribers.len(), 1);
        drop(rx);
        h.handle(Cmd::Emit { event: Event::log("info", "nobody home") });
        assert!(h.subscribers.is_empty(), "a closed subscriber should be cleaned up");
    }

    #[test]
    fn idle_nudges_get_further_apart_the_longer_nothing_happens() {
        let gaps: Vec<f64> = (0..5).map(|s| idle_gap(s, 10.0, 90.0)).collect();
        assert_eq!(gaps[0], 600.0);
        assert_eq!(gaps[1], 1200.0);
        assert_eq!(gaps[2], 2400.0);
        assert_eq!(gaps[4], 5400.0, "and it stops at the ceiling");
        assert!(gaps.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn asking_about_a_thought_that_does_not_exist_is_a_clean_miss() {
        let (mut h, _d) = hub();
        let e = take(|reply| Cmd::ThoughtAction { action: "read".into(), id: "nope".into(), text: String::new(), reply }, &mut h).unwrap_err();
        assert!(matches!(e, WireError::NotFound(_, _)));
    }
}
