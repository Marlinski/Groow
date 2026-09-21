//! The window's views, one at a time.
//!
//! A card is one thing worth looking at: what is ticking, what the learning has been doing,
//! where the numbers are going, how the turns went, which commands fail. Each knows its own
//! title and how to draw itself for a given width, and nothing else — not where it is, not
//! what is beside it, not which tab it is on.
//!
//! That is the whole point. A view is a list of cards, so moving one to another tab, putting
//! two side by side or dropping one is a change to that list and to nothing else.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::draw::{ago, clip, head, quiet, spark, summarise, took, AMBER, DIM, FG, MINT, MOSS, ROSE, SKY, VIOLET};
use crate::state::Ui;

/// Something worth looking at, drawn wherever it is put.
pub trait Card {
    /// What it is called, which is its heading wherever it is drawn.
    fn title(&self) -> &'static str;

    /// Its content at this width, as drawn lines. An empty answer means it has nothing to say
    /// today, which a view may use to leave it out.
    fn lines(&self, ui: &Ui, width: usize) -> Vec<Line<'static>>;
}

/// The cards of the operator's view, in the order they are read.
pub fn operator() -> Vec<Box<dyn Card>> {
    vec![
        Box::new(Ticker),
        Box::new(Learning),
        Box::new(Trend),
        Box::new(Turns),
        Box::new(Commands),
    ]
}

/// The cards beside the conversation, under the creature and its certificate.
pub fn beside() -> Vec<Box<dyn Card>> {
    vec![Box::new(Thoughts), Box::new(Alarms)]
}

/// What it is working on by itself.
pub struct Thoughts;

impl Card for Thoughts {
    fn title(&self) -> &'static str {
        "inner thoughts"
    }

    fn lines(&self, ui: &Ui, _w: usize) -> Vec<Line<'static>> {
    if ui.thoughts.is_empty() {
        return vec![Line::from(Span::styled("none just now", Style::default().fg(DIM)))];
    }
    let mut live: Vec<_> = ui.thoughts.values().filter(|t| t.status == "running" || t.status == "paused").collect();
    let mut done: Vec<_> = ui.thoughts.values().filter(|t| t.status != "running" && t.status != "paused").collect();
    live.sort_by(|a, b| a.id.cmp(&b.id));
    done.sort_by(|a, b| a.id.cmp(&b.id));
    // Only the last few finished ones; the rest are history, not status.
    let tail = done.split_off(done.len().saturating_sub(3));

    let mut out = Vec::new();
    for t in live.into_iter().chain(tail) {
        let (mark, colour) = match t.status.as_str() {
            "running" => ("\u{25c9}", VIOLET),
            "paused" => ("\u{25cc}", DIM),
            "done" => ("\u{2713}", DIM),
            "killed" => ("\u{2717}", DIM),
            _ => ("\u{b7}", DIM),
        };
        out.push(Line::from(vec![
            Span::styled(format!("{mark} {} ", t.id), Style::default().fg(colour)),
            Span::styled(format!("{}", t.steps), Style::default().fg(DIM)),
        ]));
        out.push(Line::from(Span::styled(
            format!("  {}", clip(&t.goal, 60)),
            Style::default().fg(colour),
        )));
        if !t.note.is_empty() {
            out.push(Line::from(Span::styled(
                format!("  {}", clip(&t.note, 90)),
                Style::default().fg(DIM),
            )));
        }
    }
    out
}
}

/// Everything set to wake it, soonest first.
///
/// The same list the operator's view shows at the top, drawn here because it is the answer to
/// "what is going to happen to it next" and that belongs beside the conversation.
pub struct Alarms;

impl Card for Alarms {
    fn title(&self) -> &'static str {
        "alarms"
    }

    fn lines(&self, ui: &Ui, w: usize) -> Vec<Line<'static>> {
        let all = ui.alarms.get("alarms").and_then(|a| a.as_array()).cloned().unwrap_or_default();
        if all.is_empty() {
            return vec![quiet("none set")];
        }
        let mut due: Vec<&Value> = all.iter().collect();
        due.sort_by(|a, b| when(a).partial_cmp(&when(b)).unwrap_or(std::cmp::Ordering::Equal));
        let mut out = Vec::new();
        for a in due {
            let id = a.get("id").and_then(|v| v.as_str()).unwrap_or("??????");
            let text = a.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let off = a.get("enabled").and_then(|v| v.as_bool()) == Some(false);
            let every = match (a.get("repeat_s").and_then(|v| v.as_f64()), a.get("repeat_daily").and_then(|v| v.as_str())) {
                (_, Some(at)) => format!("daily {at}"),
                (Some(s), _) if s > 0.0 => format!("every {}", took(s).trim()),
                _ => "once".to_string(),
            };
            out.push(Line::from(vec![
                Span::styled(format!("{id} "), Style::default().fg(DIM)),
                Span::styled(
                    if off { "off".to_string() } else { format!("in {}", took(when(a)).trim()) },
                    Style::default().fg(if off { DIM } else { AMBER }),
                ),
                Span::styled(format!(" \u{b7} {every}"), Style::default().fg(DIM)),
            ]));
            // The words, on their own line: the panel is narrow and this is what it is for.
            out.push(Line::from(Span::styled(
                format!("  {}", clip(text, w.saturating_sub(2).max(10))),
                Style::default().fg(FG),
            )));
        }
        out
    }
}

/// What is happening now and what happens next: the turn in flight, the queue behind it, the
/// alarms ticking, and when curiosity will next wake it.
pub struct Ticker;

impl Card for Ticker {
    fn title(&self) -> &'static str {
        "now"
    }

    fn lines(&self, ui: &Ui, w: usize) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        let n = |k: &str| ui.status.get(k).and_then(|v| v.as_i64()).unwrap_or(0);

        let doing = if let Some(nap) = ui.status.get("napping").and_then(|v| v.as_str()) {
            format!("asleep: {nap}")
        } else if let Some(turn) = ui.status.get("turn").and_then(|v| v.as_str()) {
            format!("a turn, {turn}")
        } else {
            "nothing just now".into()
        };
        out.push(row("doing", &doing, if ui.doing.asleep() { VIOLET } else { FG }));

        let queue = n("queue");
        out.push(row(
            "waiting",
            &match queue {
                0 => "nothing in its inbox".to_string(),
                1 => "one thing in its inbox".to_string(),
                n => format!("{n} things in its inbox"),
            },
            if queue > 0 { AMBER } else { DIM },
        ));

        // The thing that wakes it all day, which nothing outside could see before.
        match ui.status.get("idle_in").and_then(|v| v.as_f64()) {
            Some(secs) => out.push(row(
                "curiosity",
                &format!(
                    "nudges it in {} (it has been nudged {} times without an answer)",
                    took(secs).trim(),
                    n("idle_streak")
                ),
                MOSS,
            )),
            None => out.push(row("curiosity", "off; it waits to be spoken to", DIM)),
        }

        let alarms = ui.alarms.get("alarms").and_then(|a| a.as_array()).cloned().unwrap_or_default();
        out.push(Line::from(""));
        if alarms.is_empty() {
            out.push(quiet("nothing is set to wake it"));
            return out;
        }
        let mut due: Vec<&Value> = alarms.iter().collect();
        due.sort_by(|a, b| when(a).partial_cmp(&when(b)).unwrap_or(std::cmp::Ordering::Equal));
        for a in due {
            let text = a.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let by = a.get("by").and_then(|v| v.as_str()).unwrap_or("?");
            let off = a.get("enabled").and_then(|v| v.as_bool()) == Some(false);
            let every = match (a.get("repeat_s").and_then(|v| v.as_f64()), a.get("repeat_daily").and_then(|v| v.as_str())) {
                (_, Some(at)) => format!("daily at {at}"),
                (Some(s), _) if s > 0.0 => format!("every {}", took(s).trim()),
                _ => "once".to_string(),
            };
            let left = when(a);
            out.push(Line::from(vec![
                Span::styled(
                    format!("  {:>8}  ", if off { "off".into() } else { format!("in {}", took(left).trim()) }),
                    Style::default().fg(if off { DIM } else { AMBER }),
                ),
                Span::styled(format!("{:<26}", clip(text, 26)), Style::default().fg(FG)),
                Span::styled(format!("{every:<16}"), Style::default().fg(DIM)),
                Span::styled(format!("set by {by}"), Style::default().fg(DIM)),
            ]));
        }
        let _ = w;
        out
    }
}

/// How long until an alarm fires. Already overdue counts as now, which is what it looks like.
fn when(a: &Value) -> f64 {
    let at = a.get("when").and_then(|v| v.as_f64()).unwrap_or(f64::MAX);
    (at - now()).max(0.0)
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn row(label: &str, text: &str, colour: ratatui::style::Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {label:<10}"), Style::default().fg(DIM)),
        Span::styled(text.to_string(), Style::default().fg(colour)),
    ])
}

/// Every learning pass, the whole ones in front and the parts they are made of beneath.
pub struct Learning;

impl Card for Learning {
    fn title(&self) -> &'static str {
        "learning"
    }

    fn lines(&self, ui: &Ui, w: usize) -> Vec<Line<'static>> {
        let passes = list(ui, "learning");
        if passes.is_empty() {
            return vec![quiet("nothing has run yet")];
        }
        passes
            .iter()
            .take(14)
            .map(|p| {
                let kind = p.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                let n = p.get("samples").and_then(|v| v.as_i64()).unwrap_or(0);
                let loss = p.get("loss").and_then(|v| v.as_f64());
                let secs = p.get("seconds").and_then(|v| v.as_f64());
                let note = p.get("note").and_then(|v| v.as_str()).unwrap_or("");
                let at = p.get("ts").and_then(|v| v.as_f64()).map(ago).unwrap_or_default();
                // A whole pass is the headline; the parts it is made of belong under it.
                let whole = matches!(kind, "nap" | "night");
                let name = if whole {
                    Style::default().fg(VIOLET).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(DIM)
                };
                Line::from(vec![
                    Span::styled(format!("  {at:>8}  "), Style::default().fg(DIM)),
                    Span::styled(format!("{}{:<11}", if whole { "" } else { " " }, kind), name),
                    Span::styled(format!("{n:>5} samples  "), Style::default().fg(FG)),
                    Span::styled(
                        secs.map(|t| format!("{}  ", took(t))).unwrap_or_else(|| "        ".into()),
                        Style::default().fg(SKY),
                    ),
                    Span::styled(
                        loss.map(|l| format!("loss {l:.3}")).unwrap_or_else(|| "          ".into()),
                        Style::default().fg(MINT),
                    ),
                    Span::styled(
                        format!("  {}", clip(&summarise(note), w.saturating_sub(58))),
                        Style::default().fg(DIM),
                    ),
                ])
            })
            .collect()
    }
}

/// Where the two numbers that matter are going: loss should fall, feeling should not.
pub struct Trend;

impl Card for Trend {
    fn title(&self) -> &'static str {
        "trend"
    }

    fn lines(&self, ui: &Ui, w: usize) -> Vec<Line<'static>> {
        let losses: Vec<f64> = list(ui, "learning")
            .iter()
            .rev()
            .filter_map(|p| p.get("loss").and_then(|v| v.as_f64()))
            .collect();
        let valence: Vec<f64> = list(ui, "feelings")
            .iter()
            .filter_map(|x| x.get("valence").and_then(|v| v.as_f64()))
            .collect();
        vec![spark("loss   ", &losses, MINT, w), spark("feeling", &valence, AMBER, w)]
    }
}

/// The turns behind the learning: how long each took, and how it ended.
pub struct Turns;

impl Card for Turns {
    fn title(&self) -> &'static str {
        "turns"
    }

    fn lines(&self, ui: &Ui, w: usize) -> Vec<Line<'static>> {
        let turns = list(ui, "turns");
        if turns.is_empty() {
            return vec![quiet("none recorded")];
        }
        turns
            .iter()
            .take(10)
            .map(|t| {
                let kind = t.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                let secs = t.get("seconds").and_then(|v| v.as_f64());
                let tools = t.get("tools").and_then(|v| v.as_i64()).unwrap_or(0);
                let outcome = t.get("outcome").and_then(|v| v.as_str()).unwrap_or("");
                let flags = t.get("flags").and_then(|v| v.as_str()).unwrap_or("");
                let at = t.get("started").and_then(|v| v.as_f64()).map(ago).unwrap_or_default();
                let colour = if outcome == "ok" { FG } else { ROSE };
                Line::from(vec![
                    Span::styled(format!("  {at:>8}  "), Style::default().fg(DIM)),
                    Span::styled(format!("{kind:<10}"), Style::default().fg(SKY)),
                    Span::styled(
                        secs.map(|s| format!("{s:>6.1}s ")).unwrap_or_else(|| "     \u{b7} ".into()),
                        Style::default().fg(FG),
                    ),
                    Span::styled(format!("{tools} tools  "), Style::default().fg(DIM)),
                    Span::styled(outcome.to_string(), Style::default().fg(colour)),
                    Span::styled(
                        format!(" {}", clip(flags, w.saturating_sub(42))),
                        Style::default().fg(ROSE),
                    ),
                ])
            })
            .collect()
    }
}

/// Which of its tools it has not learned to drive.
pub struct Commands;

impl Card for Commands {
    fn title(&self) -> &'static str {
        "commands"
    }

    fn lines(&self, ui: &Ui, _w: usize) -> Vec<Line<'static>> {
        let tools = list(ui, "tools");
        if tools.is_empty() {
            return vec![quiet("nothing has been run yet")];
        }
        tools
            .iter()
            .take(8)
            .map(|t| {
                let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let used = t.get("used").and_then(|v| v.as_i64()).unwrap_or(0);
                let failed = t.get("failed").and_then(|v| v.as_f64()).unwrap_or(0.0);
                Line::from(vec![
                    Span::styled(format!("  {name:<16}"), Style::default().fg(SKY)),
                    Span::styled(format!("{used:>5} runs  "), Style::default().fg(FG)),
                    Span::styled(
                        format!("{:.0}% failed", failed * 100.0),
                        Style::default().fg(if failed > 0.25 { ROSE } else { DIM }),
                    ),
                ])
            })
            .collect()
    }
}

fn list(ui: &Ui, key: &str) -> Vec<Value> {
    ui.stats.get(key).and_then(|v| v.as_array()).cloned().unwrap_or_default()
}

/// Stack cards down an area under their headings, scrolled from the top.
///
/// One column of cards is how the operator's view is built today. A card knows nothing about
/// this, so putting two beside each other or moving one to its own tab is a change here.
pub fn stacked(ui: &Ui, cards: &[Box<dyn Card>], width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line> = Vec::new();
    for (i, c) in cards.iter().enumerate() {
        if i > 0 {
            out.push(Line::from(""));
        }
        out.push(head(c.title()));
        out.extend(c.lines(ui, width));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ui() -> Ui {
        Ui::default()
    }

    fn text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_card_draws_itself_and_knows_nothing_about_where_it_is() {
        // The whole reason for the trait: the same card, any width, no other context.
        let mut u = ui();
        u.stats = json!({"tools": [{"name": "shell", "used": 212, "failed": 0.47}]});
        for width in [40, 80, 200] {
            let s = text(&Commands.lines(&u, width));
            assert!(s.contains("shell"), "{s}");
            assert!(s.contains("47% failed"), "{s}");
        }
    }

    #[test]
    fn the_ticker_says_what_is_happening_and_what_happens_next() {
        let mut u = ui();
        u.status = json!({"turn": "01789-0000", "queue": 2, "idle_in": 420.0, "idle_streak": 3});
        u.alarms = json!({"alarms": [
            {"id": "a1", "text": "check the news", "when": now() + 3600.0, "repeat_s": 10800.0, "by": "groow", "enabled": true},
            {"id": "a2", "text": "the older one", "when": now() + 60.0, "repeat_daily": "08:00", "by": "mentor", "enabled": true},
        ]});
        let s = text(&Ticker.lines(&u, 90));
        assert!(s.contains("a turn, 01789-0000"), "{s}");
        assert!(s.contains("2 things in its inbox"), "{s}");
        assert!(s.contains("nudges it in 7m00s"), "{s}");
        assert!(s.contains("check the news"), "{s}");
        assert!(s.contains("daily at 08:00"), "{s}");
        // Soonest first, whatever order they were given in.
        let first = s.lines().position(|l| l.contains("the older one")).unwrap();
        let second = s.lines().position(|l| l.contains("check the news")).unwrap();
        assert!(first < second, "the next one to fire should be at the top:\n{s}");
    }

    #[test]
    fn with_nothing_ticking_the_ticker_says_so() {
        let s = text(&Ticker.lines(&ui(), 80));
        assert!(s.contains("nothing just now"), "{s}");
        assert!(s.contains("nothing in its inbox"), "{s}");
        assert!(s.contains("nothing is set to wake it"), "{s}");
    }

    #[test]
    fn curiosity_being_off_is_said_rather_than_left_blank() {
        let mut u = ui();
        u.status = json!({"queue": 0});
        assert!(text(&Ticker.lines(&u, 80)).contains("off; it waits to be spoken to"));
    }

    #[test]
    fn the_alarms_are_listed_beside_the_conversation_soonest_first() {
        let mut u = ui();
        assert!(text(&Alarms.lines(&u, 30)).contains("none set"));

        u.alarms = json!({"alarms": [
            {"id": "0aea53", "text": "check the news", "when": now() + 7200.0, "repeat_s": 10800.0, "by": "groow", "enabled": true},
            {"id": "c59f2e", "text": "read the paper", "when": now() + 60.0, "repeat_daily": "08:00", "by": "mentor", "enabled": true},
            {"id": "b17c02", "text": "an old one", "when": now() + 30.0, "enabled": false, "by": "mentor"},
        ]});
        let s = text(&Alarms.lines(&u, 34));
        assert!(s.contains("0aea53"), "the id is there to act on: {s}");
        // Truncated, not rounded: something two hours out is 1h59m and a bit away, and a
        // countdown that rounds up is a countdown that lies about having more time.
        assert!(s.contains("in 1h59m"), "{s}");
        assert!(s.contains("daily 08:00"), "{s}");
        assert!(s.contains("off"), "a cancelled one says so rather than counting down: {s}");
        let at = |t: &str| s.lines().position(|l| l.contains(t)).unwrap();
        assert!(at("b17c02") < at("c59f2e") && at("c59f2e") < at("0aea53"), "soonest first:\n{s}");
    }

    #[test]
    fn the_panel_beside_the_conversation_is_cards_like_everything_else() {
        let u = ui();
        let s = text(&stacked(&u, &beside(), 34));
        assert!(s.contains("inner thoughts"), "{s}");
        assert!(s.contains("alarms"), "{s}");
    }

    #[test]
    fn a_view_is_a_list_of_cards_and_nothing_more() {
        let u = ui();
        let s = text(&stacked(&u, &operator(), 100));
        for title in ["now", "learning", "trend", "turns", "commands"] {
            assert!(s.contains(title), "{title} is missing from the operator's view");
        }
        // Rearranged, dropped, or moved elsewhere: the cards do not notice.
        let two: Vec<Box<dyn Card>> = vec![Box::new(Commands), Box::new(Ticker)];
        let s = text(&stacked(&u, &two, 100));
        assert!(s.find("commands").unwrap() < s.find("now").unwrap());
        assert!(!s.contains("learning"));
    }
}
