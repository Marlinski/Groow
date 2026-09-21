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

use crate::draw::{ago, clip, head, quiet, spark, summarise, took, AMBER, DIM, FG, MINT, MOSS, PICKED, ROSE, SKY, VIOLET};
use crate::state::{compact, Filter, Ui, Who};

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

/// The journal: every run it has done, and what is inside the one being pointed at.
pub fn journal() -> Vec<Box<dyn Card>> {
    vec![Box::new(Runs)]
}

/// Every run, newest first, with what set it off and what it cost.
///
/// A turn of the conversation and an inner thought are the same kind of thing — a process,
/// given a reason to run, that did some work and ended somehow — so they are one list, tagged,
/// rather than two places to look.
pub struct Runs;

impl Card for Runs {
    fn title(&self) -> &'static str {
        "runs"
    }

    fn lines(&self, ui: &Ui, w: usize) -> Vec<Line<'static>> {
        // One filter is not a list of runs at all: it is every command it has run, which is
        // the other thing anyone opens this pane to look at.
        if ui.filter == Filter::Commands {
            let ran = ui.tool_lines();
            if ran.is_empty() {
                return vec![quiet("nothing has been run yet")];
            }
            return ran
                .iter()
                .map(|b| {
                    let colour = if b.who == Who::Tool { SKY } else { DIM };
                    Line::from(vec![
                        Span::styled(format!("  {:>8} ", b.at.map(clock).unwrap_or_default()), Style::default().fg(DIM)),
                        Span::styled(clip(b.text.trim_end(), w.saturating_sub(12).max(10)), Style::default().fg(colour)),
                    ])
                })
                .collect();
        }
        let runs = ui.listed();
        if runs.is_empty() {
            return vec![quiet("nothing has run yet")];
        }
        let mut out = Vec::new();
        for (i, r) in runs.iter().enumerate().take(200) {
            let here = i == ui.picked;
            let mark = if here { "\u{25b8} " } else { "  " };
            let tag = if r.inner { "inner" } else { "main" };
            let colour = if r.inner { VIOLET } else { SKY };
            let cost = match (r.seconds, r.tokens) {
                (Some(s), Some(t)) if t > 0 => format!("{} \u{b7} {t} tok", took(s).trim()),
                (Some(s), _) => took(s).trim().to_string(),
                _ => "running".to_string(),
            };
            let end = if r.outcome == "ok" || r.outcome == "done" {
                Style::default().fg(DIM)
            } else {
                Style::default().fg(ROSE)
            };
            let mut spans = vec![
                Span::styled(
                    mark.to_string(),
                    if here { Style::default().fg(MINT).add_modifier(Modifier::BOLD) } else { Style::default() },
                ),
                Span::styled(format!("{:>8} ", clock(r.at)), Style::default().fg(DIM)),
                // The id, because it is the thing you would quote or look up, and it was
                // nowhere on this screen.
                Span::styled(format!("{:<10}", clip(&short_id(&r.id), 9)), Style::default().fg(MOSS)),
                Span::styled(format!("{tag:<6}"), Style::default().fg(colour)),
                Span::styled(format!("{:<9} ", clip(&r.tag, 9)), Style::default().fg(DIM)),
                Span::styled(format!("{:<18}", clip(&cost, 18)), Style::default().fg(FG)),
                // Which creature ran it. The weights are not the ones it has now.
                Span::styled(format!("{:<8}", r.version), Style::default().fg(MOSS)),
                Span::styled(
                    clip(&if r.what.is_empty() { format!("{} {}", r.outcome, r.flags) } else { r.what.clone() },
                         w.saturating_sub(46).max(10)),
                    end,
                ),
            ];
            // The whole row is what is being pointed at, so the whole row is lit. A mark on
            // its own is easy to lose in a long list, and the eye follows a block, not a dot.
            // Padded to the full width first, or the highlight stops where the words do.
            if here {
                let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
                spans.push(Span::raw(" ".repeat(w.saturating_sub(used))));
                out.push(Line::from(spans).style(Style::default().bg(PICKED)));
            } else {
                out.push(Line::from(spans));
            }
        }
        out
    }
}

/// What is inside the run the journal is pointing at, step by step.
///
/// Read from the trace — what was actually sent and what actually came back — rather than from
/// the conversation, because the conversation is the tidied version and this pane is for the
/// times those two differ.
pub struct Steps;

impl Card for Steps {
    fn title(&self) -> &'static str {
        "inside"
    }

    fn lines(&self, ui: &Ui, w: usize) -> Vec<Line<'static>> {
        let Some(r) = ui.pointing_at() else {
            return vec![quiet("nothing to look into")];
        };
        if ui.opened.as_deref() != Some(r.id.as_str()) {
            return vec![quiet("reading it\u{2026}")];
        }
        let steps = ui.steps();
        if steps.is_empty() {
            return vec![
                quiet("nothing was traced for this one."),
                quiet("older runs, or tracing turned off."),
            ];
        }
        steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let here = i == ui.step && ui.column != crate::state::Column::Runs;
                let what = s.get("what").and_then(|w| w.as_str()).unwrap_or("?");
                let body = s.get("body").cloned().unwrap_or(Value::Null);
                let (colour, said) = match what {
                    "request" => (AMBER, format!(
                        "{} messages \u{b7} {} tools",
                        body.get("messages").and_then(|m| m.as_array()).map(|a| a.len()).unwrap_or(0),
                        body.get("tools").and_then(|t| t.as_array()).map(|a| a.len()).unwrap_or(0),
                    )),
                    "answer" => (MINT, format!(
                        "{} tokens in {}",
                        body.get("tokens").and_then(|t| t.as_u64()).unwrap_or(0),
                        took(body.get("seconds").and_then(|s| s.as_f64()).unwrap_or(0.0)).trim(),
                    )),
                    "tool_call" => (SKY, clip(&compact(body.get("args").unwrap_or(&Value::Null)), 40)),
                    "tool_result" => (
                        if body.get("ok").and_then(|o| o.as_bool()) == Some(false) { ROSE } else { DIM },
                        clip(body.get("result").and_then(|r| r.as_str()).unwrap_or("").trim(), 40),
                    ),
                    _ => (DIM, String::new()),
                };
                let mut spans = vec![
                    Span::styled(if here { "\u{25b8} " } else { "  " }, Style::default().fg(MINT)),
                    Span::styled(
                        format!("{:>8} ", s.get("at").and_then(|a| a.as_f64()).map(clock).unwrap_or_default()),
                        Style::default().fg(DIM),
                    ),
                    Span::styled(format!("{what:<13}"), Style::default().fg(colour)),
                    Span::styled(clip(&said, w.saturating_sub(25).max(8)), Style::default().fg(FG)),
                ];
                if here {
                    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
                    spans.push(Span::raw(" ".repeat(w.saturating_sub(used))));
                    return Line::from(spans).style(Style::default().bg(PICKED));
                }
                Line::from(spans)
            })
            .collect()
    }
}

/// One step, exactly as it went over the wire.
///
/// The whole request: the system prompt the model was given, every message of the window it was
/// given with it, the tool schemas it could see, and the sampling settings. This is the answer
/// to "why did it say that", and nothing reconstructed can be.
pub struct Exactly;

impl Card for Exactly {
    fn title(&self) -> &'static str {
        "exactly"
    }

    fn lines(&self, ui: &Ui, w: usize) -> Vec<Line<'static>> {
        let Some(step) = ui.at_step() else {
            return vec![quiet("pick a step on the left")];
        };
        let body = step.get("body").cloned().unwrap_or(Value::Null);
        let mut out = Vec::new();

        // A request is the one thing worth laying out rather than printing: it is mostly the
        // messages, and the messages are what anyone came here to read.
        if step.get("what").and_then(|w| w.as_str()) == Some("request") {
            for (k, label) in [("temperature", "temperature"), ("top_p", "top p"), ("top_k", "top k"),
                               ("max_new_tokens", "max tokens")] {
                if let Some(v) = body.get(k) {
                    out.push(Line::from(vec![
                        Span::styled(format!("  {label:<12}"), Style::default().fg(DIM)),
                        Span::styled(v.to_string(), Style::default().fg(FG)),
                    ]));
                }
            }
            out.push(Line::from(""));
            for m in body.get("messages").and_then(|m| m.as_array()).into_iter().flatten() {
                let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("?");
                out.push(Line::from(Span::styled(
                    format!("  {role}"),
                    Style::default()
                        .fg(match role {
                            "system" => MOSS,
                            "user" => AMBER,
                            "assistant" => MINT,
                            _ => SKY,
                        })
                        .add_modifier(Modifier::BOLD),
                )));
                let text = m.get("content").and_then(|c| c.as_str()).unwrap_or("");
                for line in text.lines() {
                    for row in crate::line::wrap(line, w.saturating_sub(4).max(8), 0).rows {
                        out.push(Line::from(Span::styled(format!("    {row}"), Style::default().fg(FG))));
                    }
                }
                out.push(Line::from(""));
            }
            return out;
        }

        // Everything else is shown as it was written down, which for an answer or a command is
        // exactly what anyone wants.
        let text = serde_json::to_string_pretty(&body).unwrap_or_default();
        for line in text.lines() {
            for row in crate::line::wrap(line, w.saturating_sub(2).max(8), 0).rows {
                out.push(Line::from(Span::styled(format!("  {row}"), Style::default().fg(FG))));
            }
        }
        out
    }
}

/// The heading of the run being pointed at.
pub struct Inside;

impl Card for Inside {
    fn title(&self) -> &'static str {
        "inside"
    }

    fn lines(&self, ui: &Ui, w: usize) -> Vec<Line<'static>> {
        let Some(r) = ui.pointing_at() else {
            return vec![quiet("nothing to look into")];
        };
        let mut out = vec![Line::from(vec![
            Span::styled(format!("  {} ", r.id), Style::default().fg(MINT).add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("{} \u{b7} {}", if r.inner { "inner" } else { "main" }, stamp(r.at)),
                Style::default().fg(DIM),
            ),
            Span::styled(
                if r.version.is_empty() { String::new() } else { format!(" \u{b7} weights {}", r.version) },
                Style::default().fg(MOSS),
            ),
        ])];
        if !r.what.is_empty() {
            out.push(Line::from(Span::styled(format!("  {}", r.what), Style::default().fg(FG))));
        }
        if !r.flags.is_empty() {
            out.push(Line::from(Span::styled(format!("  {}", r.flags), Style::default().fg(ROSE))));
        }
        out.push(Line::from(""));

        if ui.opened.as_deref() != Some(r.id.as_str()) {
            out.push(quiet("reading it\u{2026}"));
            return out;
        }
        // A turn answers with the lines of the conversation; a thought with its own trace. The
        // shape is the same either way, which is the point of them being one list.
        let lines = ui
            .inside
            .get("messages")
            .or_else(|| ui.inside.get("history"))
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default();
        if lines.is_empty() {
            out.push(quiet("nothing was written down for this one"));
        }
        for m in lines {
            let g = |k: &str| m.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let who = match (g("role").as_str(), g("kind")) {
                ("user", k) if !k.is_empty() && k != "user" => k,
                (role, _) => role.to_string(),
            };
            let colour = match who.as_str() {
                "user" => AMBER,
                "assistant" => MINT,
                "tool" => SKY,
                "system" => MOSS,
                _ => VIOLET,
            };
            let at = m.get("ts").and_then(|t| t.as_f64()).map(clock).unwrap_or_default();
            let body = groow_harness::parse::visible(&g("content"));
            for (n, line) in body.lines().enumerate() {
                out.push(Line::from(vec![
                    Span::styled(
                        if n == 0 { format!("  {at:>8} {who:<10}") } else { " ".repeat(20) },
                        Style::default().fg(if n == 0 { colour } else { DIM }),
                    ),
                    Span::styled(clip(line, w.saturating_sub(20).max(10)), Style::default().fg(FG)),
                ]));
            }
            for c in m.get("tool_calls").and_then(|c| c.as_array()).into_iter().flatten() {
                let name = c.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = c.get("arguments").map(compact).unwrap_or_default();
                out.push(Line::from(vec![
                    Span::styled(" ".repeat(20), Style::default()),
                    Span::styled(
                        format!("\u{2699} {name}({})", clip(&args, w.saturating_sub(24).max(10))),
                        Style::default().fg(SKY),
                    ),
                ]));
            }
        }
        out
    }
}

/// A run's id, short enough for a column. A turn's is a timestamp and a counter, and the tail
/// is the part that tells two of them apart.
fn short_id(id: &str) -> String {
    match id.rsplit_once('-') {
        Some((head, tail)) => format!("{}-{tail}", &head[head.len().saturating_sub(4)..]),
        None => id.chars().take(6).collect(),
    }
}

/// The time of day, which is what you look for in a log.
fn clock(ts: f64) -> String {
    if ts <= 0.0 {
        return String::new();
    }
    let s = ts as u64 % 86400;
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

/// The whole moment, for the heading of one run: the time of day and how long ago that was,
/// because both are things you want and neither answers for the other.
fn stamp(ts: f64) -> String {
    if ts <= 0.0 {
        return "at some point".into();
    }
    format!("{} \u{b7} {} ago", clock(ts), ago(ts))
}

/// The cards beside the conversation, under the creature and its certificate.
pub fn beside() -> Vec<Box<dyn Card>> {
    vec![Box::new(Alarms)]
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
                let version = p.get("version").and_then(|v| v.as_str()).unwrap_or("");
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
                    Span::styled(format!("  {version:<8}"), Style::default().fg(MOSS)),
                    Span::styled(
                        format!("  {}", clip(&summarise(note), w.saturating_sub(68))),
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
                let last = t.get("last").and_then(|v| v.as_f64());
                Line::from(vec![
                    Span::styled(format!("  {name:<10}"), Style::default().fg(SKY)),
                    Span::styled(format!("{used:>5} runs  "), Style::default().fg(FG)),
                    Span::styled(
                        format!("{:>3.0}% failed  ", failed * 100.0),
                        Style::default().fg(if failed > 0.25 { ROSE } else { DIM }),
                    ),
                    // When it was last reached for, because a tool nobody has used for days
                    // is history rather than a problem.
                    Span::styled(
                        last.map(|l| format!("last {} ago", ago(l))).unwrap_or_default(),
                        Style::default().fg(DIM),
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
        assert!(s.contains("alarms"), "{s}");
    }

    #[test]
    fn the_journal_lists_every_run_newest_first_whatever_kind_it_was() {
        // A turn and an inner thought are the same kind of thing: a process, given a reason to
        // run, that did some work and ended somehow. One list, tagged, not two places to look.
        let mut u = ui();
        u.stats = json!({"turns": [
            {"id": "t2", "kind": "user", "started": 200.0, "seconds": 3.0, "tools": 1,
             "outcome": "ok", "flags": "", "tokens": 120},
            {"id": "t1", "kind": "alarm", "started": 100.0, "seconds": 9.0, "tools": 6,
             "outcome": "ok", "flags": "tool_error,exhausted", "tokens": 900},
        ]});
        u.thoughts(&json!({"thoughts": [
            {"id": "a97d91", "goal": "count the files", "status": "done", "steps": 3,
             "created": 150.0, "updated": 170.0},
        ]}));

        let runs = u.runs();
        assert_eq!(runs.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["t2", "a97d91", "t1"]);
        assert!(runs[1].inner, "a thought is an inner run");
        assert_eq!(runs[1].seconds, Some(20.0), "from when it started to its last step");

        let s = text(&Runs.lines(&u, 100));
        assert!(s.contains("main"), "{s}");
        assert!(s.contains("inner"), "{s}");
        assert!(s.contains("a97d91"), "a thought's id is the one it is known by: {s}");
        assert!(s.contains("count the files"), "a thought shows its goal: {s}");
        assert!(s.contains("tool_error"), "a turn shows how it ended: {s}");
    }

    #[test]
    fn a_run_says_which_creature_ran_it() {
        // The weights move every time it sleeps, so a run from yesterday was not done by the
        // thing you are talking to now, and a log that does not say so is misleading.
        let mut u = ui();
        u.stats = json!({"turns": [
            {"id": "t1", "kind": "user", "started": 100.0, "seconds": 1.0, "outcome": "ok",
             "flags": "", "version": "0.4.2"},
        ]});
        assert_eq!(u.runs()[0].version, "0.4.2");
        assert!(text(&Runs.lines(&u, 120)).contains("0.4.2"));

        u.opened = Some("t1".into());
        u.inside = json!({"messages": []});
        assert!(text(&Inside.lines(&u, 70)).contains("weights 0.4.2"));
    }

    #[test]
    fn the_journal_can_be_narrowed_to_one_question_at_a_time() {
        let mut u = ui();
        u.stats = json!({"turns": [
            {"id": "t1", "kind": "user", "started": 100.0, "seconds": 1.0, "outcome": "ok", "flags": ""},
            {"id": "t2", "kind": "idle", "started": 90.0, "seconds": 1.0, "outcome": "abandoned", "flags": ""},
        ]});
        u.thoughts(&json!({"thoughts": [{"id": "a9", "goal": "g", "status": "done", "created": 95.0}]}));

        u.show(Filter::Main);
        assert_eq!(u.listed().iter().map(|r| r.id.clone()).collect::<Vec<_>>(), ["t1", "t2"]);
        u.show(Filter::Inner);
        assert_eq!(u.listed().iter().map(|r| r.id.clone()).collect::<Vec<_>>(), ["a9"]);
        u.show(Filter::Failed);
        assert_eq!(u.listed().iter().map(|r| r.id.clone()).collect::<Vec<_>>(), ["t2"],
                   "the list anyone actually goes looking for");
        u.show(Filter::All);
        assert_eq!(u.listed().len(), 3);
    }

    #[test]
    fn pointing_at_a_run_asks_for_what_is_inside_it() {
        let mut u = ui();
        u.stats = json!({"turns": [
            {"id": "t1", "kind": "user", "started": 100.0, "seconds": 1.0, "outcome": "ok", "flags": ""},
            {"id": "t2", "kind": "user", "started": 90.0, "seconds": 1.0, "outcome": "ok", "flags": ""},
        ]});
        assert_eq!(u.pointing_at().unwrap().id, "t1", "the newest, until told otherwise");
        u.pick(1);
        assert_eq!(u.pointing_at().unwrap().id, "t2");
        assert!(u.want_inside, "and what it points at has to be fetched");
        u.pick(5);
        assert_eq!(u.pointing_at().unwrap().id, "t2", "never past the end");

        // Until it arrives, the detail says so rather than showing the run before it.
        assert!(text(&Inside.lines(&u, 60)).contains("reading it"));
        u.opened = Some("t2".into());
        u.inside = json!({"messages": [{"role": "user", "content": "hello", "ts": 90.0}]});
        let s = text(&Inside.lines(&u, 60));
        assert!(s.contains("hello"), "{s}");
        assert!(s.contains("t2"), "{s}");
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
