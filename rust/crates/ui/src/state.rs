//! What the interface knows, and how each event changes it.
//!
//! All of it is plain data with plain transitions, so the behaviour can be tested without a
//! terminal. Drawing reads this and nothing else.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::creature::Mood;

/// One thing shown in the conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct Bubble {
    pub who: Who,
    pub label: String,
    pub text: String,
    /// When it happened, where that is known. History has it; a live event usually does not,
    /// and something without a time is simply shown without one.
    pub at: Option<f64>,
}

impl Bubble {
    pub fn new(who: Who, label: &str, text: &str) -> Bubble {
        Bubble { who, label: label.to_string(), text: text.to_string(), at: None }
    }

    pub fn at(mut self, ts: Option<f64>) -> Bubble {
        self.at = ts;
        self
    }
}

/// Which view is on screen. The conversation is the creature; the other two are about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// What was said.
    Conversation,
    /// Every command it has run, and what came back.
    Tools,
    /// The meta-processes: what learning ran, how long it took, where the feeling is going.
    Admin,
}

impl Pane {
    pub fn title(&self) -> &'static str {
        match self {
            Pane::Conversation => "conversation",
            Pane::Tools => "commands",
            Pane::Admin => "meta",
        }
    }

    /// Left to right, which is also the order the function keys are in.
    pub const ALL: [Pane; 3] = [Pane::Conversation, Pane::Tools, Pane::Admin];

    pub fn next(&self) -> Pane {
        match self {
            Pane::Conversation => Pane::Tools,
            Pane::Tools => Pane::Admin,
            Pane::Admin => Pane::Conversation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who {
    /// A person.
    Human,
    /// The creature.
    Groow,
    /// Something that woke it that was not a person.
    Signal,
    /// A tool going out.
    Tool,
    /// The interface itself, and anything diagnostic.
    System,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThoughtRow {
    pub id: String,
    pub status: String,
    pub goal: String,
    pub steps: u32,
    pub note: String,
}

/// Everything on screen.
pub struct Ui {
    pub bubbles: Vec<Bubble>,
    pub thoughts: BTreeMap<String, ThoughtRow>,
    pub status: Value,
    pub birth: Value,
    pub mood: Mood,
    pub tick: u64,
    pub input: String,
    pub connected: bool,
    /// The answer being streamed, which replaces itself as more arrives.
    speaking: Option<usize>,
    /// Where this turn's answer ended up, so the end of the turn corrects it rather than
    /// saying the same thing twice.
    said: Option<usize>,
    /// What was typed here and shown at once, waiting for the core to announce the same thing.
    echoed: Option<String>,
    /// How far back the conversation is scrolled; zero is the newest.
    pub scroll_back: u16,
    /// How far down the meta pane is scrolled; zero is the top. A conversation is read from
    /// the bottom because the newest matters most, and a table from the top because the
    /// heading does, so the two do not share a number.
    pub meta_scroll: u16,
    /// Set when the person asks to leave.
    pub done: bool,
    /// The last thing that went wrong, shown once.
    pub trouble: Option<String>,
    /// Which view is on screen.
    pub pane: Pane,
    /// The measurements behind the creature, as the core last reported them.
    pub stats: Value,
    /// The time of the oldest thing loaded, which is where reading further back starts.
    oldest: Option<f64>,
    /// Whether there is more history behind what is loaded.
    pub more_history: bool,
    /// Set when the view has been scrolled to the top and there is more to fetch. The run loop
    /// clears it by asking; keeping the request out of here keeps this file free of sockets.
    pub want_more: bool,
    /// True between asking for a page and being given it, so one scroll does not ask twice.
    pub loading: bool,
}

/// The most messages kept on screen. Older ones are still in the journal.
///
/// High enough that a day of conversation fits without paging, because the commonest thing
/// anyone does with this window is read back what happened.
const KEEP: usize = 4000;

/// How close to the top counts as the top, in messages. A page is asked for before the ceiling
/// is actually reached, so scrolling does not stop and wait.
const NEARLY_TOP: usize = 20;

impl Default for Ui {
    fn default() -> Self {
        Ui {
            bubbles: Vec::new(),
            thoughts: BTreeMap::new(),
            status: Value::Null,
            birth: Value::Null,
            mood: Mood::Idle,
            tick: 0,
            input: String::new(),
            connected: false,
            speaking: None,
            said: None,
            echoed: None,
            scroll_back: 0,
            meta_scroll: 0,
            done: false,
            trouble: None,
            pane: Pane::Conversation,
            stats: Value::Null,
            oldest: None,
            more_history: true,
            want_more: false,
            loading: false,
        }
    }
}

impl Ui {
    pub fn push(&mut self, who: Who, label: &str, text: &str) {
        if text.trim().is_empty() && who != Who::Groow {
            return;
        }
        self.bubbles.push(Bubble::new(who, label, text));
        if self.bubbles.len() > KEEP {
            let cut = self.bubbles.len() - KEEP;
            self.bubbles.drain(..cut);
            self.speaking = self.speaking.and_then(|i| i.checked_sub(cut));
            self.said = self.said.and_then(|i| i.checked_sub(cut));
        }
        // Anything new brings the view back to the present, unless a person is reading back.
        if self.scroll_back == 0 {
            self.speaking = self.speaking.filter(|_| false).or(self.speaking);
        }
    }

    /// The creature's age in seconds, from the birth certificate.
    pub fn age(&self, now: f64) -> f64 {
        self.birth.get("born").and_then(|b| b.as_f64()).map(|b| (now - b).max(0.0)).unwrap_or(0.0)
    }

    pub fn name(&self) -> &str {
        self.birth.get("name").and_then(|v| v.as_str()).unwrap_or("groow")
    }

    /// Apply one event from the core.
    pub fn on_event(&mut self, name: &str, data: &Value) {
        let s = |k: &str| data.get(k).and_then(|v| v.as_str()).unwrap_or("");
        match name {
            "turn_start" => {
                self.speaking = None;
                self.said = None;
                if s("who") == "user" {
                    // What was typed here is already on screen: showing it again the moment the
                    // core announces the same turn would make every question appear twice.
                    if self.echoed.as_deref() == Some(s("text")) {
                        self.echoed = None;
                    } else {
                        self.push(Who::Human, "you", s("text"));
                    }
                } else {
                    self.push(Who::Signal, &format!("signal \u{b7} {}", s("kind")), s("text"));
                }
                self.mood = Mood::Thinking;
            }
            "token" => {
                let delta = s("delta");
                if delta.is_empty() {
                    return;
                }
                match self.speaking {
                    Some(i) if i < self.bubbles.len() => {
                        self.bubbles[i].text.push_str(delta);
                    }
                    _ => {
                        self.bubbles.push(Bubble::new(Who::Groow, &self.name().to_lowercase(), delta));
                        self.speaking = Some(self.bubbles.len() - 1);
                    }
                }
                self.mood = Mood::Speaking;
            }
            "message" => {
                if s("role") == "assistant" {
                    let text = groow_harness::parse::visible(s("content"));
                    if text.is_empty() {
                        return;
                    }
                    match self.speaking {
                        // The complete message replaces whatever was streamed, so a dropped
                        // delta never leaves a hole in what a person reads.
                        Some(i) if i < self.bubbles.len() => {
                            self.bubbles[i].text = text;
                            self.said = Some(i);
                        }
                        _ => {
                            self.push(Who::Groow, &self.name().to_lowercase().clone(), &text);
                            self.said = Some(self.bubbles.len() - 1);
                        }
                    }
                    self.speaking = None;
                }
            }
            "turn_end" => {
                let text = groow_harness::parse::visible(s("final"));
                if !text.is_empty() {
                    // The end of a turn carries the same words the last message did. It is a
                    // correction to what is already there, not another thing said.
                    match self.speaking.or(self.said) {
                        Some(i) if i < self.bubbles.len() => self.bubbles[i].text = text,
                        _ => self.push(Who::Groow, &self.name().to_lowercase().clone(), &text),
                    }
                }
                self.speaking = None;
                self.said = None;
                self.mood = Mood::Listening;
            }
            "tool_call" => {
                let args = data.get("args").map(compact).unwrap_or_default();
                self.push(Who::Tool, "", &format!("\u{2699} {}({})", s("name"), clip(&args, 100)));
                self.mood = Mood::Tooling;
            }
            "tool_result" => {
                if data.get("actor").and_then(|v| v.as_str()).unwrap_or("main") == "main" {
                    let ok = data.get("ok").and_then(|v| v.as_bool()).unwrap_or(true);
                    let mark = if ok { "\u{21b3}" } else { "\u{2717}" };
                    self.push(Who::System, "", &format!("  {mark} {}", clip(s("result"), 220)));
                }
            }
            "thought" => {
                let id = s("id").to_string();
                if id.is_empty() {
                    return;
                }
                let row = self.thoughts.entry(id.clone()).or_default();
                row.id = id.clone();
                if !s("status").is_empty() {
                    row.status = s("status").to_string();
                }
                if !s("goal").is_empty() {
                    row.goal = s("goal").to_string();
                }
                if let Some(n) = data.get("steps").and_then(|v| v.as_u64()) {
                    row.steps = n as u32;
                }
                if !s("text").is_empty() {
                    row.note = s("text").to_string();
                }
                let ev = s("event");
                if matches!(ev, "spawn" | "focus" | "finish" | "kill") {
                    let body = if s("text").is_empty() { s("goal") } else { s("text") };
                    self.push(Who::Signal, &format!("thought {id}"), &format!("{ev}: {body}"));
                }
                if matches!(self.mood, Mood::Idle | Mood::Listening) {
                    self.mood = Mood::Dreaming;
                }
            }
            "learned" => {
                let loss = data.get("loss").and_then(|v| v.as_f64());
                let n = data.get("samples").and_then(|v| v.as_u64()).unwrap_or(0);
                let text = match loss {
                    Some(l) => format!("  \u{21ba} learned from {n} samples \u{b7} loss {l:.3}"),
                    None => format!("  \u{21ba} learned from {n} samples"),
                };
                self.push(Who::System, "", &text);
                self.mood = Mood::Learning;
            }
            "feeling" => {
                self.status["feeling"] = data.clone();
            }
            "status" => {
                self.status = data.clone();
                if let Some(m) = data.get("mood").and_then(|v| v.as_str()).and_then(Mood::parse) {
                    self.mood = m;
                }
            }
            "log" => {
                let level = s("level");
                let text = s("text");
                if level == "error" {
                    self.trouble = Some(text.to_string());
                }
                self.push(Who::System, level, &clip(text, 2000));
            }
            _ => {}
        }
    }

    /// A line the person typed. Returns what should be sent, if anything.
    pub fn submit(&mut self) -> Option<Sent> {
        let text = std::mem::take(&mut self.input);
        let text = text.trim().to_string();
        if text.is_empty() {
            return None;
        }
        // Leaving closes this window and nothing else. The creature stays awake.
        if matches!(text.as_str(), "/quit" | "/exit" | "/detach") {
            self.done = true;
            return None;
        }
        if text == "/clear" {
            self.bubbles.clear();
            self.speaking = None;
            self.said = None;
            return None;
        }
        if let Some(rest) = text.strip_prefix('/') {
            return Some(Sent::Command(rest.to_string()));
        }
        self.push(Who::Human, "you", &text);
        self.echoed = Some(text.clone());
        Some(Sent::Say(text))
    }

    pub fn scroll(&mut self, by: i32) {
        if self.pane == Pane::Admin {
            self.meta_scroll = (self.meta_scroll as i32 + by).clamp(0, 2000) as u16;
            return;
        }
        let next = self.scroll_back as i32 - by;
        self.scroll_back = next.clamp(0, self.bubbles.len() as i32) as u16;
        // Near the top with more behind it: say so, and let the run loop do the asking. The
        // margin means the page is on its way before the ceiling is actually hit.
        if self.pane == Pane::Conversation
            && self.more_history
            && !self.loading
            && self.scroll_back as usize + NEARLY_TOP >= self.bubbles.len()
        {
            self.want_more = true;
        }
    }

    /// Where reading further back starts: the time of the oldest thing loaded.
    pub fn oldest(&self) -> Option<f64> {
        self.oldest
    }

    /// Take a page of the journal.
    ///
    /// `older` is a page from before what is already held, which goes on top; anything else is
    /// the first load. Either way the view does not move: a person reading something does not
    /// want it to jump because more arrived above it.
    pub fn history(&mut self, v: &Value, older: bool) {
        self.loading = false;
        let records = v.get("messages").and_then(|m| m.as_array()).cloned().unwrap_or_default();
        self.more_history = v.get("more").and_then(|m| m.as_bool()).unwrap_or(false);
        if records.is_empty() {
            return;
        }
        if let Some(first) = records.first().and_then(|r| r.get("ts")).and_then(|t| t.as_f64()) {
            self.oldest = Some(match self.oldest {
                Some(o) if o < first => o,
                _ => first,
            });
        }

        let mut made: Vec<Bubble> = Vec::new();
        let me = self.name().to_lowercase();
        for r in &records {
            let get = |k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("");
            let at = r.get("ts").and_then(|t| t.as_f64());
            let body = groow_harness::parse::visible(get("content"));
            match get("role") {
                "user" => made.push(Bubble::new(Who::Human, "you", &body).at(at)),
                "assistant" => {
                    if !body.trim().is_empty() {
                        made.push(Bubble::new(Who::Groow, &me, &body).at(at));
                    }
                    for c in r.get("tool_calls").and_then(|c| c.as_array()).into_iter().flatten() {
                        let name = c.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        let args = c.get("arguments").map(compact).unwrap_or_default();
                        made.push(
                            Bubble::new(Who::Tool, "", &format!("\u{2699} {name}({})", clip(&args, 200)))
                                .at(at),
                        );
                    }
                }
                // What a command printed. Shown as the interface shows a live one, so reading
                // back looks like watching it happen.
                "tool" => made.push(
                    Bubble::new(Who::System, "", &format!("  {}", clip(&body, 400))).at(at),
                ),
                _ => {}
            }
        }

        if older {
            let grew = made.len();
            made.append(&mut self.bubbles);
            self.bubbles = made;
            // Hold the view where it was: everything that was on screen is now that much
            // further from the bottom.
            self.scroll_back = self.scroll_back.saturating_add(grew as u16);
            self.speaking = self.speaking.map(|i| i + grew);
            self.said = self.said.map(|i| i + grew);
        } else {
            made.append(&mut self.bubbles);
            self.bubbles = made;
        }
    }

    /// Only the commands, which is what the second pane shows.
    pub fn tool_lines(&self) -> Vec<&Bubble> {
        self.bubbles.iter().filter(|b| b.who == Who::Tool || b.who == Who::System).collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Sent {
    Say(String),
    Command(String),
}

fn compact(v: &Value) -> String {
    match v {
        Value::Object(m) => m
            .iter()
            .map(|(k, v)| format!("{k}: {}", v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string())))
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}\u{2026}", s.chars().take(n).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ui() -> Ui {
        Ui { birth: json!({"name": "Groow", "born": 1000.0}), ..Default::default() }
    }

    #[test]
    fn a_persons_message_and_the_answer_appear_in_order() {
        let mut u = ui();
        u.on_event("turn_start", &json!({"who": "user", "text": "what is a river?"}));
        u.on_event("turn_end", &json!({"final": "water going downhill"}));
        assert_eq!(u.bubbles.len(), 2);
        assert_eq!(u.bubbles[0].who, Who::Human);
        assert_eq!(u.bubbles[1].who, Who::Groow);
        assert_eq!(u.bubbles[1].text, "water going downhill");
    }

    #[test]
    fn a_streamed_answer_builds_up_in_one_place() {
        let mut u = ui();
        u.on_event("turn_start", &json!({"who": "user", "text": "hi"}));
        for d in ["wa", "ter ", "flows"] {
            u.on_event("token", &json!({"delta": d}));
        }
        assert_eq!(u.bubbles.len(), 2, "each piece must not become its own message");
        assert_eq!(u.bubbles[1].text, "water flows");
        assert_eq!(u.mood, Mood::Speaking);
    }

    #[test]
    fn the_finished_message_replaces_what_was_streamed() {
        let mut u = ui();
        u.on_event("turn_start", &json!({"who": "user", "text": "hi"}));
        u.on_event("token", &json!({"delta": "wate"}));
        u.on_event("turn_end", &json!({"final": "water going downhill"}));
        assert_eq!(u.bubbles.len(), 2);
        assert_eq!(u.bubbles[1].text, "water going downhill", "a dropped piece must not leave a hole");
    }

    #[test]
    fn reasoning_and_tool_json_never_reach_the_screen() {
        let mut u = ui();
        u.on_event("message", &json!({
            "role": "assistant",
            "content": "<think>they mean the river</think>The Loire.<tool_call>{\"name\":\"shell\"}</tool_call>",
        }));
        assert_eq!(u.bubbles.len(), 1);
        assert_eq!(u.bubbles[0].text, "The Loire.");
    }

    #[test]
    fn a_signal_is_shown_as_a_signal_not_as_a_person() {
        let mut u = ui();
        u.on_event("turn_start", &json!({"who": "signal", "kind": "alarm", "text": "read the news"}));
        assert_eq!(u.bubbles[0].who, Who::Signal);
        assert!(u.bubbles[0].label.contains("alarm"));
    }

    #[test]
    fn a_failing_tool_is_marked_differently_from_one_that_worked() {
        let mut u = ui();
        u.on_event("tool_result", &json!({"actor": "main", "ok": false, "result": "no such file"}));
        u.on_event("tool_result", &json!({"actor": "main", "ok": true, "result": "fine"}));
        assert!(u.bubbles[0].text.contains('\u{2717}'), "a failure should look like one");
        assert!(u.bubbles[1].text.contains('\u{21b3}'));
    }

    #[test]
    fn a_thoughts_own_tool_results_stay_out_of_the_conversation() {
        let mut u = ui();
        u.on_event("tool_result", &json!({"actor": "thought", "ok": true, "result": "busy"}));
        assert!(u.bubbles.is_empty(), "inner work should not fill the conversation");
    }

    #[test]
    fn thought_rows_accumulate_rather_than_replacing_each_other() {
        let mut u = ui();
        u.on_event("thought", &json!({"id": "ab12", "status": "running", "goal": "read about rivers", "event": "spawn"}));
        u.on_event("thought", &json!({"id": "ab12", "steps": 3}));
        let row = &u.thoughts["ab12"];
        assert_eq!(row.goal, "read about rivers", "a later update must not wipe the goal");
        assert_eq!(row.steps, 3);
        assert_eq!(row.status, "running");
    }

    #[test]
    fn a_whole_exchange_appears_once() {
        // What a person types is shown at once and then announced back by the core; what the
        // mind says arrives as tokens, then as a message, then again at the end of the turn.
        // Every one of those is the same words, and each was being added to the screen.
        let mut u = ui();
        u.input = "what is in this directory?".into();
        u.submit();

        u.on_event("turn_start", &json!({"who": "user", "kind": "user",
                                         "text": "what is in this directory?", "turn": "t1"}));
        u.on_event("token", &json!({"delta": "There are "}));
        u.on_event("token", &json!({"delta": "six."}));
        u.on_event("message", &json!({"role": "assistant", "content": "There are six.", "turn": "t1"}));
        u.on_event("turn_end", &json!({"turn": "t1", "kind": "user", "final": "There are six."}));

        let said: Vec<(Who, &str)> =
            u.bubbles.iter().map(|b| (b.who, b.text.as_str())).collect();
        assert_eq!(said, vec![
            (Who::Human, "what is in this directory?"),
            (Who::Groow, "There are six."),
        ], "{said:?}");
    }

    #[test]
    fn a_turn_nobody_here_started_is_still_shown() {
        // The echo only silences the one thing this window just sent. A message left from
        // another window, or the same words said again later, must still appear.
        let mut u = ui();
        u.on_event("turn_start", &json!({"who": "user", "text": "hello", "turn": "t1"}));
        u.on_event("turn_end", &json!({"turn": "t1", "final": "hello yourself"}));
        u.on_event("turn_start", &json!({"who": "user", "text": "hello", "turn": "t2"}));
        assert_eq!(u.bubbles.iter().filter(|b| b.text == "hello").count(), 2, "{:?}", u.bubbles);
    }

    #[test]
    fn an_answer_that_was_never_streamed_still_arrives() {
        // A short turn can end without a single token event, and then the end of the turn is
        // the only place the words come from.
        let mut u = ui();
        u.on_event("turn_start", &json!({"who": "signal", "kind": "alarm", "text": "the news"}));
        u.on_event("turn_end", &json!({"turn": "t1", "final": "nothing worth reporting"}));
        assert_eq!(u.bubbles.last().unwrap().text, "nothing worth reporting");
        assert_eq!(u.bubbles.iter().filter(|b| b.who == Who::Groow).count(), 1);
    }

    #[test]
    fn the_end_of_a_turn_corrects_what_is_on_screen() {
        // The final text is authoritative: a dropped delta is repaired rather than appended.
        let mut u = ui();
        u.on_event("turn_start", &json!({"who": "user", "text": "hello", "turn": "t1"}));
        u.on_event("token", &json!({"delta": "There are s"}));
        u.on_event("message", &json!({"role": "assistant", "content": "There are s", "turn": "t1"}));
        u.on_event("turn_end", &json!({"turn": "t1", "final": "There are six."}));
        assert_eq!(u.bubbles.last().unwrap().text, "There are six.");
        assert_eq!(u.bubbles.iter().filter(|b| b.who == Who::Groow).count(), 1);
    }

    #[test]
    fn opening_the_window_shows_what_was_said_before_it_was_opened() {
        let mut u = ui();
        u.history(&json!({"more": true, "messages": [
            {"role": "user", "content": "what is in this directory?", "ts": 100.0},
            {"role": "assistant", "content": "", "ts": 101.0,
             "tool_calls": [{"name": "shell", "arguments": {"command": "ls -1 | wc -l"}}]},
            {"role": "tool", "name": "shell", "content": "6", "ts": 102.0},
            {"role": "assistant", "content": "There are six.", "ts": 103.0},
        ]}), false);

        let said: Vec<(Who, &str)> = u.bubbles.iter().map(|b| (b.who, b.text.as_str())).collect();
        assert_eq!(said[0], (Who::Human, "what is in this directory?"));
        assert_eq!(said[1].0, Who::Tool);
        assert!(said[1].1.contains("shell(command: ls -1 | wc -l)"), "history reads like the live view: {said:?}");
        assert_eq!(said[3], (Who::Groow, "There are six."));
        assert_eq!(u.oldest(), Some(100.0), "the oldest is where reading further back starts");
        assert!(u.more_history);
    }

    #[test]
    fn reading_past_the_top_asks_for_the_page_before() {
        let mut u = ui();
        u.history(&json!({"more": true, "messages":
            (0..40).map(|i| json!({"role": "user", "content": format!("m{i}"), "ts": 100.0 + i as f64}))
                   .collect::<Vec<_>>()}), false);
        assert!(!u.want_more, "nothing is asked for while the newest is in view");

        u.scroll(-60);
        assert!(u.want_more, "scrolling past the top should ask for more");

        // What comes back goes on top, and what was being read stays where it was.
        u.loading = true;
        let was = u.bubbles.len();
        u.history(&json!({"more": false, "messages":
            (0..10).map(|i| json!({"role": "user", "content": format!("old{i}"), "ts": 50.0 + i as f64}))
                   .collect::<Vec<_>>()}), true);
        assert_eq!(u.bubbles[0].text, "old0", "the older page goes above");
        assert_eq!(u.bubbles.len(), was + 10);
        assert_eq!(u.oldest(), Some(50.0));
        assert!(!u.more_history, "and it is told there is no more");
        assert!(!u.loading);
    }

    #[test]
    fn nothing_is_asked_for_twice_while_a_page_is_on_its_way() {
        let mut u = ui();
        u.history(&json!({"more": true, "messages":
            (0..40).map(|i| json!({"role": "user", "content": format!("m{i}"), "ts": 100.0 + i as f64}))
                   .collect::<Vec<_>>()}), false);
        u.scroll(-60);
        u.want_more = false;
        u.loading = true;
        u.scroll(-5);
        assert!(!u.want_more, "one page at a time");
    }

    #[test]
    fn the_meta_pane_is_read_from_the_top_and_the_conversation_from_the_bottom() {
        let mut u = ui();
        for i in 0..50 {
            u.push(Who::Human, "you", &format!("m{i}"));
        }
        u.scroll(-5);
        assert_eq!(u.scroll_back, 5, "the conversation scrolls back from the newest");

        u.pane = Pane::Admin;
        assert_eq!(u.meta_scroll, 0, "a table starts at its heading");
        u.scroll(4);
        assert_eq!(u.meta_scroll, 4, "and scrolling down moves down it");
        u.scroll(-10);
        assert_eq!(u.meta_scroll, 0, "never above the top");
        assert_eq!(u.scroll_back, 5, "and the conversation kept its own place");
    }

    #[test]
    fn the_commands_pane_is_the_conversation_with_the_words_taken_out() {
        let mut u = ui();
        u.push(Who::Human, "you", "what is in this directory?");
        u.push(Who::Tool, "", "\u{2699} shell(ls)");
        u.push(Who::System, "", "  6");
        u.push(Who::Groow, "groow", "There are six.");
        let only: Vec<&str> = u.tool_lines().iter().map(|b| b.text.as_str()).collect();
        assert_eq!(only, vec!["\u{2699} shell(ls)", "  6"]);
    }

    #[test]
    fn leaving_closes_the_window_and_sends_nothing() {
        for word in ["/quit", "/exit", "/detach"] {
            let mut u = ui();
            u.input = word.to_string();
            assert_eq!(u.submit(), None);
            assert!(u.done, "{word} should close the interface");
        }
    }

    #[test]
    fn a_command_goes_to_the_core_and_ordinary_text_is_spoken() {
        let mut u = ui();
        u.input = "/status".into();
        assert_eq!(u.submit(), Some(Sent::Command("status".into())));
        u.input = "hello there".into();
        assert_eq!(u.submit(), Some(Sent::Say("hello there".into())));
        assert_eq!(u.bubbles.last().unwrap().who, Who::Human, "what was said should appear at once");
    }

    #[test]
    fn an_empty_line_does_nothing() {
        let mut u = ui();
        u.input = "   ".into();
        assert_eq!(u.submit(), None);
        assert!(u.bubbles.is_empty());
        assert!(!u.done);
    }

    #[test]
    fn clearing_empties_the_view_without_touching_the_creature() {
        let mut u = ui();
        u.push(Who::Human, "you", "something");
        u.input = "/clear".into();
        assert_eq!(u.submit(), None);
        assert!(u.bubbles.is_empty());
        assert!(!u.done, "clearing is not leaving");
    }

    #[test]
    fn the_conversation_does_not_grow_without_limit() {
        let mut u = ui();
        for i in 0..(KEEP + 50) {
            u.push(Who::Human, "you", &format!("m{i}"));
        }
        assert_eq!(u.bubbles.len(), KEEP);
        assert_eq!(u.bubbles.last().unwrap().text, format!("m{}", KEEP + 49), "the newest must survive");
    }

    #[test]
    fn scrolling_stays_within_the_conversation() {
        let mut u = ui();
        for i in 0..10 {
            u.push(Who::Human, "you", &format!("m{i}"));
        }
        u.scroll(-100);
        assert!(u.scroll_back <= 10);
        u.scroll(100);
        assert_eq!(u.scroll_back, 0, "scrolling forward returns to the present");
    }

    #[test]
    fn age_comes_from_the_certificate_and_is_never_negative() {
        let u = ui();
        assert_eq!(u.age(1100.0), 100.0);
        assert_eq!(u.age(900.0), 0.0);
        assert_eq!(Ui::default().age(1000.0), 0.0, "before the certificate arrives, nothing is known");
    }

    #[test]
    fn an_unknown_event_is_ignored_rather_than_breaking_the_view() {
        let mut u = ui();
        u.on_event("from_the_future", &json!({"anything": 1}));
        assert!(u.bubbles.is_empty());
    }
}
