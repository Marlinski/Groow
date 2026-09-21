//! Drawing the screen.
//!
//! The night-garden palette: mint for the creature, amber for the person, violet for its inner
//! life, sky for tools, and everything else kept quiet so the conversation is what the eye
//! lands on.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::creature;
use crate::cards::Card;
use crate::creature::Looks;
use crate::state::{Bubble, Pane, Ui, Who};

pub const MINT: Color = Color::Rgb(0x7e, 0xe8, 0xc8);
pub const AMBER: Color = Color::Rgb(0xf2, 0xc9, 0x7d);
pub const VIOLET: Color = Color::Rgb(0xb8, 0x9c, 0xff);
pub const SKY: Color = Color::Rgb(0x6f, 0xb7, 0xff);
pub const DIM: Color = Color::Rgb(0x5c, 0x6b, 0x7a);
pub const ROSE: Color = Color::Rgb(0xff, 0x7b, 0x72);
pub const MOSS: Color = Color::Rgb(0x4e, 0x9a, 0x7a);
pub const BG: Color = Color::Rgb(0x0b, 0x10, 0x16);
pub const FG: Color = Color::Rgb(0xd7, 0xdd, 0xe3);
pub const EDGE: Color = Color::Rgb(0x1e, 0x2a, 0x33);

/// The side panel is fixed; below this width it is dropped so the conversation still fits.
const SIDE: u16 = 36;
const NARROW: u16 = 74;

pub fn draw(f: &mut Frame, ui: &Ui) {
    let area = f.area();
    f.render_widget(Block::default().style(Style::default().bg(BG).fg(FG)), area);

    // The box for what you are typing grows with it, so a long message is visible as it is
    // written rather than running off the edge into somewhere you cannot see.
    let typing = crate::line::wrap(
        &ui.input.text(),
        area.width.saturating_sub(2) as usize,
        ui.input.cursor(),
    );
    // The line you type in and the creature beside it belong to the conversation, not to the
    // window. The journal and the operator's view are for reading, and they get the whole of
    // it: a chat box under a log is a chat box in the way.
    let talking = ui.pane == Pane::Conversation;
    let input_h = if talking {
        (typing.rows.len() as u16 + 2).clamp(3, input_limit(area.height))
    } else {
        0
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(4),
            Constraint::Length(input_h),
            Constraint::Length(1),
        ])
        .split(area);

    f.render_widget(Paragraph::new(tabs(ui)), rows[0]);
    let body = rows[1];

    if talking && area.width >= NARROW {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(30), Constraint::Length(SIDE)])
            .split(body);
        pane(f, cols[0], ui);
        side(f, cols[1], ui);
    } else {
        // On a narrow terminal the creature gives way to the words, and everywhere but the
        // conversation there was never anything beside them.
        pane(f, body, ui);
    }
    if talking {
        input(f, rows[2], ui, &typing);
    }
    status(f, rows[3], ui);
}

/// Whichever view is being looked at, in the same frame, so switching costs nothing.
fn pane(f: &mut Frame, area: Rect, ui: &Ui) {
    match ui.pane {
        Pane::Conversation => conversation(f, area, ui, ui.bubbles.iter().collect()),
        Pane::Journal => journal(f, area, ui),
        Pane::Admin => meta(f, area, ui),
    }
}

/// The tab bar: which pane is on screen, and how to reach the others.
fn tabs(ui: &Ui) -> Line<'static> {
    let mut spans = vec![Span::styled(" ", Style::default().fg(DIM))];
    for (i, p) in Pane::ALL.iter().enumerate() {
        let on = *p == ui.pane;
        let style = if on {
            Style::default().fg(MINT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        };
        spans.push(Span::styled(format!("F{} {} ", i + 1, p.title()), style));
    }
    Line::from(spans)
}

fn conversation(f: &mut Frame, area: Rect, ui: &Ui, bubbles: Vec<&Bubble>) {
    let mut lines: Vec<Line> = Vec::new();
    if ui.loading {
        lines.push(Line::from(Span::styled(
            "reading further back…",
            Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
        )));
    } else if !ui.more_history && ui.pane == Pane::Conversation {
        lines.push(Line::from(Span::styled(
            "\u{2015} the beginning \u{2015}",
            Style::default().fg(DIM),
        )));
    }
    for b in bubbles {
        let (colour, label) = match b.who {
            Who::Human => (AMBER, b.label.as_str()),
            Who::Groow => (MINT, b.label.as_str()),
            Who::Signal => (VIOLET, b.label.as_str()),
            Who::Tool => (SKY, ""),
            Who::System => (DIM, ""),
        };
        if !label.is_empty() {
            lines.push(Line::from(Span::styled(
                label.to_string(),
                Style::default().fg(colour).add_modifier(Modifier::BOLD),
            )));
        }
        for l in b.text.lines() {
            lines.push(Line::from(Span::styled(l.to_string(), Style::default().fg(colour))));
        }
        lines.push(Line::from(""));
    }
    // Last of all, so it sits directly under what was said and is the thing the eye lands on
    // when nothing else has happened.
    if ui.pane == Pane::Conversation {
        if let Some(note) = ui.waiting() {
            lines.push(Line::from(Span::styled(
                format!("\u{2026} {note}"),
                Style::default().fg(AMBER).add_modifier(Modifier::ITALIC),
            )));
        }
    }

    // Keep the newest in view unless the person has scrolled back.
    let inner_h = area.height.saturating_sub(2) as usize;
    let total = lines.len();
    let from_bottom = ui.scroll_back as usize;
    let end = total.saturating_sub(from_bottom);
    let start = end.saturating_sub(inner_h);
    let view: Vec<Line> = lines[start.min(total)..end.min(total)].to_vec();

    let title = if ui.scroll_back > 0 {
        format!(" {} \u{b7} back {} ", ui.pane.title(), ui.scroll_back)
    } else {
        format!(" {} ", ui.pane.title())
    };
    f.render_widget(
        Paragraph::new(view)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(EDGE))
                    .title(Span::styled(title, Style::default().fg(DIM))),
            ),
        area,
    );
}

/// The meta pane: what the processes the mind never sees have been doing.
///
/// Everything here comes out of the statistics database, which only the core can open. It is
/// deliberately the plainest view in the window: a list of what ran and how long it took, and
/// two lines showing where the numbers are going. A graph that flatters is worse than no graph.
/// The journal: the list of runs on the left, what is inside the one being pointed at on the
/// right. Everything that happened, in one place, with the filters across the top.
///
/// On a narrow terminal the detail gives way to the list, because a list you cannot read is
/// worse than a detail you cannot see.
fn journal(f: &mut Frame, area: Rect, ui: &Ui) {
    // Wide enough for both only counts the pane, not the window: the side panel has
    // already taken its share by the time this is drawn.
    let wide = area.width >= 84 && ui.filter != crate::state::Filter::Commands;
    let cols = if wide {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(area)
    } else {
        Layout::default().direction(Direction::Horizontal).constraints([Constraint::Min(10)]).split(area)
    };

    // The filters, and which one is on, across the title.
    let mut legend = vec![Span::styled(" runs ", Style::default().fg(DIM))];
    for fl in crate::state::Filter::ALL {
        let on = fl == ui.filter;
        legend.push(Span::styled(
            format!("alt-{} {} ", fl.key(), fl.name()),
            if on {
                Style::default().fg(MINT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(DIM)
            },
        ));
    }

    let w = cols[0].width.saturating_sub(4) as usize;
    let lines = crate::cards::Runs.lines(ui, w);
    // Keep what is pointed at on screen, whether it is at the top of a long list or the end.
    let room = cols[0].height.saturating_sub(2) as usize;
    let from = ui.picked.saturating_sub(room.saturating_sub(3).max(1));
    let view: Vec<Line> = lines[from.min(lines.len())..(from + room).min(lines.len())].to_vec();
    f.render_widget(
        Paragraph::new(view).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(EDGE))
                .title(Line::from(legend)),
        ),
        cols[0],
    );

    if !wide {
        return;
    }
    let w = cols[1].width.saturating_sub(4) as usize;
    let inside = crate::cards::Inside.lines(ui, w);
    let room = cols[1].height.saturating_sub(2) as usize;
    let from = inside.len().saturating_sub(room.max(1)).min(ui.meta_scroll as usize);
    let view: Vec<Line> = inside[from.min(inside.len())..(from + room).min(inside.len())].to_vec();
    f.render_widget(
        Paragraph::new(view).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(EDGE))
                .title(Span::styled(" inside ", Style::default().fg(DIM))),
        ),
        cols[1],
    );
}

/// The operator's view: a column of cards, scrolled from the top.
///
/// It knows which cards it shows and nothing about what any of them contains. Moving one to
/// its own tab, or putting two side by side, is a change to `cards::operator()` and to the
/// layout here, and to nothing else.
fn meta(f: &mut Frame, area: Rect, ui: &Ui) {
    // Nothing here wraps: these are rows, and a row that folds onto the next line stops being
    // a table. Each card cuts its own content to the width it is given.
    let w = area.width.saturating_sub(4) as usize;
    let lines = crate::cards::stacked(ui, &crate::cards::operator(), w);

    // From the top, and never past the end, so scrolling down stops at the last row rather
    // than running off into blank space.
    let inner_h = area.height.saturating_sub(2) as usize;
    let total = lines.len();
    let start = (ui.meta_scroll as usize).min(total.saturating_sub(inner_h.min(total)));
    let view: Vec<Line> = lines[start..(start + inner_h).min(total)].to_vec();

    f.render_widget(
        Paragraph::new(view).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(EDGE))
                .title(Span::styled(" meta ", Style::default().fg(DIM))),
        ),
        area,
    );
}

pub fn head(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!(" {text}"),
        Style::default().fg(MOSS).add_modifier(Modifier::BOLD),
    ))
}

pub fn quiet(text: &str) -> Line<'static> {
    Line::from(Span::styled(format!("  {text}"), Style::default().fg(DIM)))
}

/// A line of numbers as eight heights, oldest on the left.
///
/// Scaled between its own smallest and largest, so it shows the shape of the change rather
/// than the size of it; the numbers themselves are on the rows above.
pub fn spark(label: &str, v: &[f64], colour: Color, width: usize) -> Line<'static> {
    const BARS: [char; 8] = ['\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}'];
    if v.len() < 2 {
        return Line::from(vec![
            Span::styled(format!("  {label}  "), Style::default().fg(DIM)),
            Span::styled("not enough yet", Style::default().fg(DIM)),
        ]);
    }
    // Only as many bars as there is room for, taking the most recent, so the line never wraps
    // and what is shown is the part anyone is actually asking about.
    let room = width.saturating_sub(label.len() + 22).max(8);
    let v = &v[v.len().saturating_sub(room)..];
    let lo = v.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let span = (hi - lo).max(f64::EPSILON);
    let bars: String = v
        .iter()
        .map(|x| {
            let i = (((x - lo) / span) * (BARS.len() - 1) as f64).round() as usize;
            BARS[i.min(BARS.len() - 1)]
        })
        .collect();
    Line::from(vec![
        Span::styled(format!("  {label}  "), Style::default().fg(DIM)),
        Span::styled(bars, Style::default().fg(colour)),
        Span::styled(format!("  {lo:.3} \u{2192} {:.3}", v[v.len() - 1]), Style::default().fg(DIM)),
    ])
}

/// A pass's note, which is JSON, said as a person would read it.
///
/// `{"scored": 1, "harvested": 2, "sft_steps": 1}` is a fact about the creature and should
/// read like one. Anything that is not an object, or a value that is itself a structure, is
/// left alone rather than mangled into something that looks like data and is not.
pub fn summarise(note: &str) -> String {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(note) else {
        return note.to_string();
    };
    map.iter()
        // The timings of the parts are on the parts' own rows; repeating them here would fill
        // the line with what is already directly underneath it.
        .filter(|(k, _)| k.as_str() != "parts")
        .filter_map(|(k, v)| match v {
            serde_json::Value::Bool(false) => None,
            serde_json::Value::Bool(true) => Some(k.replace('_', " ")),
            serde_json::Value::Number(n) if n.as_f64() == Some(0.0) => None,
            serde_json::Value::Number(n) => Some(format!("{} {n}", k.replace('_', " "))),
            serde_json::Value::String(t) if !t.is_empty() => Some(format!("{}: {t}", k.replace('_', " "))),
            serde_json::Value::Array(a) if !a.is_empty() => Some(format!("{} {}", a.len(), k.replace('_', " "))),
            serde_json::Value::Object(o) if !o.is_empty() => {
                Some(o.iter().map(|(k2, v2)| format!("{k2} {v2}")).collect::<Vec<_>>().join(" "))
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

/// How long something took, in the unit that suits it.
pub fn took(seconds: f64) -> String {
    match seconds {
        s if s < 1.0 => format!("{:>5.0}ms", s * 1000.0),
        s if s < 90.0 => format!("{s:>6.1}s"),
        // The smaller unit is truncated, never rounded: rounding 59.6 minutes gives "2h60m".
        s if s < 5400.0 => format!("{:>4.0}m{:02.0}s", (s / 60.0).floor(), (s % 60.0).floor()),
        s if s < 86_400.0 => format!("{:>4.0}h{:02.0}m", (s / 3600.0).floor(), ((s % 3600.0) / 60.0).floor()),
        s => format!("{:>4.0}d{:02.0}h", (s / 86_400.0).floor(), ((s % 86_400.0) / 3600.0).floor()),
    }
}

/// How long ago, said the way a person would say it.
pub fn ago(ts: f64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let s = (now - ts).max(0.0) as u64;
    match s {
        0..=90 => format!("{s}s"),
        91..=5400 => format!("{}m", s / 60),
        5401..=172_800 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

/// How tall the creature is when it is drawn: twelve rows of pixels, a line for the breath,
/// and the caption underneath.
const CREATURE_H: u16 = 14;
/// The card is a fixed list of facts plus the rule above it.
const CARD_H: u16 = 12;

fn side(f: &mut Frame, area: Rect, ui: &Ui) {
    // What matters most when space runs out: what it is thinking about, then the creature
    // itself, then the facts on the card, which never change and can be trimmed.
    // Room for both lists, which is what the panel is mostly for once the card is drawn.
    let thoughts_h: u16 = 8;
    let show_creature = area.height >= CREATURE_H + thoughts_h + 3;
    let used = if show_creature { CREATURE_H } else { 0 } + thoughts_h;
    let card_h = area.height.saturating_sub(used).min(CARD_H);

    let mut parts: Vec<Constraint> = Vec::new();
    if show_creature {
        parts.push(Constraint::Length(CREATURE_H));
    }
    if card_h >= 4 {
        parts.push(Constraint::Length(card_h));
    }
    parts.push(Constraint::Min(thoughts_h));

    let rows = Layout::default().direction(Direction::Vertical).constraints(parts).split(area);
    let now = groow_proto::event::now();
    let mut i = 0;
    if show_creature {
        f.render_widget(Paragraph::new(creature::render(ui.age(now), ui.doing, ui.tick, true)), rows[i]);
        i += 1;
    }
    if card_h >= 4 {
        f.render_widget(
            Paragraph::new(card(ui, now)).block(
                Block::default().borders(Borders::TOP).border_style(Style::default().fg(EDGE)),
            ),
            rows[i],
        );
        i += 1;
    }
    // What is left of the panel is the alarms: what is going to happen to it next. What it is
    // thinking about on its own used to be here too, squeezed into thirty characters where it
    // could not be read; it is in the journal now, where a list belongs.
    let width = rows[i].width.saturating_sub(1) as usize;
    let mut lines: Vec<Line> = Vec::new();
    for (n, c) in crate::cards::beside().iter().enumerate() {
        if n > 0 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            format!(" {} ", c.title()),
            Style::default().fg(DIM).add_modifier(Modifier::BOLD),
        )));
        lines.extend(c.lines(ui, width));
    }
    f.render_widget(
        Paragraph::new(lines)
            // Not trimmed: the indent is what shows a goal belongs to the line above it.
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::TOP).border_style(Style::default().fg(EDGE))),
        rows[i],
    );
}

/// The identity card. Everything on it was decided at birth and cannot be edited.
pub fn card(ui: &Ui, now: f64) -> Vec<Line<'static>> {
    if ui.birth.is_null() {
        return vec![Line::from(Span::styled("connecting\u{2026}", Style::default().fg(DIM)))];
    }
    let g = |k: &str| ui.birth.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let s = |k: &str| ui.status.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let n = |k: &str| ui.status.get(k).and_then(|v| v.as_u64()).unwrap_or(0);

    let age = {
        let secs = ui.age(now) as u64;
        format!("{}d {:02}:{:02}:{:02}", secs / 86400, (secs % 86400) / 3600, (secs % 3600) / 60, secs % 60)
    };
    let lineage = g("lineage");
    let lineage = lineage.rsplit('/').next().unwrap_or(&lineage).to_string();

    let mut out = vec![Line::from(Span::styled(
        g("name"),
        Style::default().fg(MINT).add_modifier(Modifier::BOLD),
    ))];
    for (label, value, colour) in [
        ("id", g("id"), FG),
        ("born", g("born_text"), FG),
        ("age", age, AMBER),
        ("lineage", lineage, FG),
        // Which creature these weights are. It moves every time it sleeps.
        ("weights", if s("version").is_empty() { "\u{2014}".into() } else { s("version") }, MINT),
        ("body", g("hardware"), FG),
        ("mentor", g("mentor"), FG),
        ("turns", n("turns").to_string(), FG),
        ("thinking", n("thoughts").to_string(), FG),
        ("feeling", if s("tone").is_empty() { tone(ui) } else { s("tone") }, FG),
    ] {
        out.push(Line::from(vec![
            Span::styled(format!("{label:<9}"), Style::default().fg(DIM)),
            Span::styled(value, Style::default().fg(colour)),
        ]));
    }
    out
}

fn tone(ui: &Ui) -> String {
    ui.status
        .get("feeling")
        .and_then(|f| f.get("tone"))
        .and_then(|v| v.as_str())
        .unwrap_or("\u{2014}")
        .to_string()
}


/// How tall the input box may grow: a third of the window, so the conversation is never
/// squeezed out by something being typed at it.
fn input_limit(height: u16) -> u16 {
    (height / 3).clamp(3, 12)
}

fn input(f: &mut Frame, area: Rect, ui: &Ui, typing: &crate::line::Wrapped) {
    let border = if ui.connected { MINT } else { MOSS };
    let text = if ui.input.is_empty() && !ui.connected {
        Span::styled("waiting for the core\u{2026}", Style::default().fg(DIM))
    } else if ui.input.is_empty() {
        Span::styled(
            "talk to it \u{b7} /quit closes this window and leaves it awake",
            Style::default().fg(DIM),
        )
    } else {
        Span::styled(String::new(), Style::default().fg(FG))
    };

    // More rows than fit: show the ones around the cursor, so typing is always visible even
    // when what is being written is longer than the box can ever be.
    let visible = area.height.saturating_sub(2) as usize;
    let first = typing.row.saturating_sub(visible.saturating_sub(1));
    let shown: Vec<Line> = if ui.input.is_empty() {
        vec![Line::from(text)]
    } else {
        typing.rows[first..(first + visible).min(typing.rows.len())]
            .iter()
            .map(|r| Line::from(Span::styled(r.clone(), Style::default().fg(FG))))
            .collect()
    };

    f.render_widget(
        Paragraph::new(shown).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border)),
        ),
        area,
    );
    // The cursor sits where the next character would go, which is not always the end of the
    // line: it is the only thing on screen saying where an edit will land.
    let x = area.x + 1 + typing.col.min(area.width.saturating_sub(3) as usize) as u16;
    let y = area.y + 1 + (typing.row - first) as u16;
    f.set_cursor_position((x, y));
}

fn status(f: &mut Frame, area: Rect, ui: &Ui) {
    let n = |k: &str| ui.status.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    let mut spans = vec![Span::styled(
        format!(" {} ", ui.doing.caption()),
        Style::default().fg(MINT).add_modifier(Modifier::BOLD),
    )];
    if let Some(what) = ui.status.get("napping").and_then(|v| v.as_str()) {
        spans.push(Span::styled(
            format!("\u{b7} asleep ({what}) "),
            Style::default().fg(VIOLET).add_modifier(Modifier::BOLD),
        ));
    }
    let queue = n("queue");
    spans.push(Span::styled(
        format!("\u{b7} queue {queue} "),
        Style::default().fg(if queue > 0 { AMBER } else { DIM }),
    ));
    spans.push(Span::styled(
        format!("\u{b7} thoughts {} \u{b7} turns {} ", n("thoughts"), n("turns")),
        Style::default().fg(DIM),
    ));
    if !ui.connected {
        spans.push(Span::styled("\u{b7} not connected ", Style::default().fg(ROSE)));
    }
    if let Some(t) = &ui.trouble {
        spans.push(Span::styled(format!("\u{b7} {}", clip(t, 60)), Style::default().fg(ROSE)));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Rgb(0x0f, 0x16, 0x1d))),
        area,
    );
}

pub fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}\u{2026}", s.chars().take(n).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use serde_json::json;

    fn ui() -> Ui {
        let mut u = Ui::default();
        u.birth = json!({
            "name": "Groow", "id": "3f2a19cd", "born": groow_proto::event::now() - 90000.0,
            "born_text": "2026-09-17 08:00 UTC", "lineage": "Qwen/Qwen3-4B",
            "hardware": "Tesla V100 31 GB", "mentor": "Marlinski",
        });
        u.status = json!({"queue": 0, "thoughts": 1, "turns": 12, "feeling": {"tone": "even"}});
        u.connected = true;
        u
    }

    fn render(w: u16, h: u16, u: &Ui) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, u)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Print one frame, for looking at by eye: `cargo test -p groow-ui -- --nocapture look`.
    #[test]
    fn look() {
        let mut u = ui();
        u.push(Who::Human, "you", "what is in this folder?");
        u.push(Who::Tool, "", "\u{2699} shell(command: ls)");
        u.push(Who::System, "", "  \u{21b3} notes.md  rivers.md");
        u.push(Who::Groow, "groow", "Two files: notes and something about rivers.");
        u.on_event("thought", &json!({"id": "ab12", "status": "running", "goal": "read about the Loire"}));
        u.doing = crate::creature::State::Listening;
        println!("\n{}\n", render(96, 30, &u));
    }

    #[test]
    fn a_full_screen_shows_the_conversation_the_creature_and_the_card() {
        let mut u = ui();
        u.push(Who::Human, "you", "what is a river?");
        u.push(Who::Groow, "groow", "water going downhill");
        let s = render(120, 34, &u);
        assert!(s.contains("what is a river?"));
        assert!(s.contains("water going downhill"));
        assert!(s.contains("Groow"), "the card should be there");
        assert!(s.contains("3f2a19cd"), "the id is part of the card");
        assert!(s.contains("\u{2588}"), "the creature should be drawn");
    }

    #[test]
    fn a_narrow_terminal_keeps_the_words_and_drops_the_decoration() {
        let mut u = ui();
        u.push(Who::Human, "you", "still readable");
        let s = render(50, 20, &u);
        assert!(s.contains("still readable"));
        assert!(!s.contains("3f2a19cd"), "the card should give way on a small screen");
    }

    #[test]
    fn a_tiny_terminal_does_not_panic() {
        let u = ui();
        for (w, h) in [(20u16, 6u16), (10, 4), (1, 1), (200, 60)] {
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            t.draw(|f| draw(f, &u)).expect("drawing must never fail");
        }
    }

    #[test]
    fn a_long_conversation_shows_the_newest_messages() {
        let mut u = ui();
        for i in 0..200 {
            u.push(Who::Human, "you", &format!("message number {i}"));
        }
        let s = render(100, 30, &u);
        assert!(s.contains("message number 199"), "the newest should be in view");
        assert!(!s.contains("message number 0 "), "the oldest should have scrolled off");
    }

    #[test]
    fn scrolling_back_shows_older_messages_and_says_so() {
        let mut u = ui();
        for i in 0..200 {
            u.push(Who::Human, "you", &format!("message number {i}"));
        }
        u.scroll(-60);
        let s = render(100, 30, &u);
        assert!(s.contains("back 60"), "a person should know they are not at the present: {s}");
    }

    #[test]
    fn a_long_message_is_visible_as_it_is_written() {
        // It used to run off the right edge into somewhere nobody could see, which is a poor
        // way to write anything longer than a sentence.
        let mut u = ui();
        let long = "please read the news and tell me which of it you think is worth remembering \
                    tomorrow, and why you think so";
        u.input.set(long);
        let s = render(60, 24, &u);
        assert!(s.contains("please read the news"), "{s}");
        assert!(s.contains("and why you think so"), "the end of it must be on screen too: {s}");
    }

    #[test]
    fn the_box_grows_but_never_swallows_the_conversation() {
        let mut u = ui();
        u.push(Who::Groow, "groow", "something it said");
        u.input.set(&"word ".repeat(400));
        let s = render(60, 24, &u);
        let box_rows = s.lines().filter(|l| l.contains("word")).count();
        assert!(box_rows <= 8, "the box took over the window: {box_rows} rows");
        assert!(s.contains("something it said"), "the conversation is still there: {s}");
    }

    #[test]
    fn nothing_happening_is_explained_rather_than_left_blank() {
        // What the window used to do: show what you typed, then nothing, which looks exactly
        // like a message that went nowhere.
        let mut u = ui();
        u.push(Who::Human, "you", "are you there?");
        u.status = json!({"queue": 1, "busy": false, "napping": "night", "thoughts": 0, "turns": 3});
        let s = render(100, 24, &u);
        assert!(s.contains("are you there?"), "{s}");
        assert!(s.contains("asleep (night)"), "{s}");
        // The note wraps, so the end of the sentence is on the next line.
        assert!(s.contains("when it wakes"), "{s}");

        // And with nothing queued, nothing is said about it. ("waiting" on its own is one of
        // the creature's own captions, so the note is what is checked for.)
        u.status = json!({"queue": 0, "busy": false, "turns": 3});
        assert!(!render(100, 24, &u).contains("message is waiting"));
    }

    #[test]
    fn the_panes_are_named_and_the_one_in_front_is_marked() {
        let mut u = ui();
        let s = render(100, 30, &u);
        assert!(s.contains("F1 conversation"), "{s}");
        assert!(s.contains("F2 journal"), "{s}");
        assert!(s.contains("F3 meta"), "{s}");

        u.pane = Pane::Journal;
        assert!(render(100, 30, &u).contains("runs"), "the pane's own title should change");
    }

    #[test]
    fn the_commands_filter_shows_what_was_run_and_nothing_else() {
        let mut u = ui();
        u.push(Who::Human, "you", "what is in this directory?");
        u.push(Who::Tool, "", "\u{2699} shell(ls -1 | wc -l)");
        u.push(Who::System, "", "  6");
        u.push(Who::Groow, "groow", "There are six.");

        u.pane = Pane::Journal;
        u.show(crate::state::Filter::Commands);
        let s = render(100, 30, &u);
        assert!(s.contains("shell(ls -1"), "{s}");
        assert!(!s.contains("There are six"), "what was said belongs to the conversation: {s}");
    }

    #[test]
    fn the_meta_pane_shows_what_the_mind_never_sees() {
        let mut u = ui();
        u.pane = Pane::Admin;
        u.stats = json!({
            "learning": [
                {"ts": 1.0, "kind": "night", "samples": 42, "loss": 0.31, "note": "merged"},
                {"ts": 0.0, "kind": "nap", "samples": 8, "loss": 0.44, "note": ""},
            ],
            "turns": [{"id": "t1", "kind": "user", "started": 0.0, "seconds": 3.5, "tools": 2,
                       "rounds": 2, "flags": "", "outcome": "ok"}],
            "feelings": [{"ts": 0.0, "valence": 0.1}, {"ts": 1.0, "valence": 0.4}],
            "tools": [{"name": "shell", "used": 91, "failed": 0.12}],
            "counters": {},
        });
        let s = render(110, 40, &u);
        assert!(s.contains("night"), "{s}");
        assert!(s.contains("42 samples"), "{s}");
        assert!(s.contains("loss 0.310"), "{s}");
        assert!(s.contains("91 runs"), "{s}");
        assert!(s.contains("12% failed"), "{s}");
    }

    #[test]
    fn nothing_in_the_meta_pane_folds_onto_the_next_line() {
        // A row that wraps stops being a row. Long flags and long notes are cut instead.
        let mut u = ui();
        u.pane = Pane::Admin;
        let flags = "tool_error,".repeat(20);
        u.stats = json!({
            "learning": [{"ts": 0.0, "kind": "train", "samples": 8, "loss": 0.2,
                          "note": "x".repeat(300)}],
            "turns": [{"id": "t1", "kind": "idle", "started": 0.0, "seconds": 1.0, "tools": 6,
                       "rounds": 6, "flags": flags, "outcome": "ok"}],
            "feelings": (0..400).map(|i| json!({"ts": i as f64, "valence": (i % 7) as f64 / 7.0}))
                                .collect::<Vec<_>>(),
            "tools": [], "counters": {},
        });
        let s = render(100, 30, &u);
        for line in s.lines().filter(|l| l.contains('│')) {
            let inner: String = line.chars().skip_while(|c| *c != '│').skip(1).collect();
            let inner = inner.split('│').next().unwrap_or("");
            assert!(inner.chars().count() <= 100, "a row spilled: {inner:?}");
        }
        assert!(s.contains("tool_error"), "and what fits is still shown: {s}");
    }

    #[test]
    fn a_whole_pass_is_shown_with_what_it_cost_and_how_long_it_took() {
        let mut u = ui();
        u.pane = Pane::Admin;
        u.stats = json!({"learning": [
            {"ts": 0.0, "kind": "night", "samples": 32, "loss": 0.71, "seconds": 184.2,
             "note": "{\"merged\":true,\"sft_steps\":9}"},
            {"ts": 0.0, "kind": "consolidate", "samples": 0, "seconds": 41.0, "note": ""},
            {"ts": 0.0, "kind": "train", "samples": 32, "loss": 0.71, "seconds": 6.4, "note": ""},
        ]});
        let s = render(110, 40, &u);
        assert!(s.contains("night"), "{s}");
        assert!(s.contains("3m04s"), "a long pass is minutes and seconds: {s}");
        assert!(s.contains("41.0s"), "{s}");
        assert!(s.contains("6.4s"), "{s}");
        assert!(s.contains("loss 0.710"), "{s}");
    }

    #[test]
    fn a_note_reads_as_a_sentence_rather_than_as_json() {
        // In the order the pass wrote them, which is the order they happened in.
        assert_eq!(
            summarise(r#"{"conversation": 1, "actions": 2}"#),
            "conversation 1 \u{b7} actions 2"
        );
        // Nothing that did not happen is mentioned: a row of zeroes says less than nothing.
        assert_eq!(
            summarise(r#"{"sft_steps": 2, "pg_steps": 0, "merged": true, "scored": 0}"#),
            "sft steps 2 \u{b7} merged"
        );
        assert_eq!(summarise("not json at all"), "not json at all", "left alone");
        assert_eq!(summarise(""), "");
    }

    #[test]
    fn how_long_something_took_is_said_in_the_unit_that_suits_it() {
        assert_eq!(took(0.412).trim(), "412ms");
        assert_eq!(took(6.4).trim(), "6.4s");
        assert_eq!(took(184.2).trim(), "3m04s");
        // Past an hour and a half, minutes stop being how anyone says it.
        assert_eq!(took(10_800.0).trim(), "3h00m");
        assert_eq!(took(7_431.0).trim(), "2h03m", "truncated, not rounded");
        assert_eq!(took(200_000.0).trim(), "2d07h");
        assert_eq!(took(7_199.0).trim(), "1h59m", "never a sixtieth minute");
    }

    #[test]
    fn a_pass_that_recorded_no_duration_still_lines_up() {
        // A row written before there was a column for it must not shift everything after it.
        let mut u = ui();
        u.pane = Pane::Admin;
        u.stats = json!({"learning": [
            {"ts": 0.0, "kind": "train", "samples": 8, "loss": 0.5, "seconds": 2.0, "note": ""},
            {"ts": 0.0, "kind": "train", "samples": 8, "note": ""},
        ]});
        let s = render(110, 40, &u);
        let rows: Vec<&str> = s.lines().filter(|l| l.contains("8 samples")).collect();
        assert_eq!(rows.len(), 2);
        let at = |l: &str| l.find("8 samples").unwrap();
        assert_eq!(at(rows[0]), at(rows[1]), "the columns moved: {rows:?}");
    }

    #[test]
    fn a_trend_needs_more_than_one_number_before_it_says_anything() {
        let mut u = ui();
        u.pane = Pane::Admin;
        u.stats = json!({"learning": [{"ts": 0.0, "kind": "nap", "samples": 1, "loss": 0.5}]});
        assert!(render(110, 40, &u).contains("not enough yet"));
    }

    #[test]
    fn the_card_shows_a_live_age_that_ticks() {
        let u = ui();
        let now = groow_proto::event::now();
        let text: String = card(&u, now).iter().map(|l| l.to_string()).collect();
        assert!(text.contains("1d 01:00"), "the age should read as a clock: {text}");
        let later: String = card(&u, now + 1.0).iter().map(|l| l.to_string()).collect();
        assert_ne!(text, later, "the age should move");
    }

    #[test]
    fn before_the_certificate_arrives_the_card_says_so_rather_than_lying() {
        let u = Ui::default();
        let text: String = card(&u, 0.0).iter().map(|l| l.to_string()).collect();
        assert!(text.contains("connecting"));
    }

    #[test]
    fn a_lost_connection_is_visible_without_hiding_the_conversation() {
        let mut u = ui();
        u.connected = false;
        u.push(Who::Groow, "groow", "said earlier");
        let s = render(110, 24, &u);
        assert!(s.contains("not connected"));
        assert!(s.contains("said earlier"), "history should stay on screen");
    }

    #[test]
    fn inner_thoughts_are_listed_in_the_journal_where_there_is_room_for_them() {
        // They used to be squeezed into the side panel, thirty characters wide, where a goal
        // and a summary could not be read. The journal is where a list of runs belongs.
        let mut u = ui();
        u.on_event("thought", &json!({"id": "ab12", "status": "running", "goal": "read about rivers"}));
        assert!(!render(120, 34, &u).contains("ab12"), "not beside the conversation any more");

        u.pane = Pane::Journal;
        let s = render(120, 34, &u);
        assert!(s.contains("ab12"), "{s}");
        assert!(s.contains("read about"), "{s}");
    }

    #[test]
    fn reading_panes_get_the_whole_window() {
        // The line you type in and the creature beside it are the conversation's, not the
        // window's. A chat box under a log is a chat box in the way.
        let mut u = ui();
        u.push(Who::Human, "you", "hello");
        assert!(render(120, 34, &u).contains("talk to it"), "the conversation has its box");

        for p in [Pane::Journal, Pane::Admin] {
            u.pane = p;
            let s = render(120, 34, &u);
            assert!(!s.contains("talk to it"), "{p:?} kept the chat box");
            assert!(!s.contains("born     "), "{p:?} kept the certificate");
        }
    }

    #[test]
    fn the_creature_is_drawn_whole_with_its_caption() {
        // A window that has not heard from the core yet says so rather than guessing: until a
        // status arrives, all it knows is that something is still coming up.
        let mut u = ui();
        let s = render(100, 34, &u);
        assert!(s.contains("waking up"), "the caption under the creature is missing:\n{s}");
        assert!(s.contains("\u{2588}\u{2588}"), "the creature should be drawn");

        u.on_event("status", &json!({"state": "listening", "queue": 0}));
        assert!(render(100, 34, &u).contains("listening \u{b7} young"));
    }

    #[test]
    fn the_side_panel_sheds_the_least_important_things_first() {
        let mut u = ui();
        u.alarms = json!({"alarms": [{"id": "c59f2e", "text": "read the news",
                                      "when": 1e12, "repeat_s": 10800.0, "by": "groow"}]});
        // Tall: everything, including the whole card.
        let tall = render(100, 36, &u);
        assert!(tall.contains("3f2a19cd"), "the id should be there");
        assert!(tall.contains("mentor"), "the whole card should fit");
        assert!(tall.contains("c59f2e"), "and what is set to wake it");
        // Short: the creature goes first, and what is going to happen next stays.
        let short = render(100, 20, &u);
        assert!(short.contains("c59f2e"), "the alarms must always be visible");
    }

    #[test]
    fn nothing_in_the_side_panel_spills_over_the_conversation() {
        let mut u = ui();
        for i in 0..4 {
            u.on_event("thought", &json!({
                "id": format!("id{i}"), "status": "running",
                "goal": "a goal long enough to need wrapping on a narrow panel",
            }));
        }
        for h in [12u16, 18, 24, 30, 40] {
            let s = render(100, h, &u);
            for line in s.lines() {
                // The conversation's right border must be intact on every row it owns.
                let chars: Vec<char> = line.chars().collect();
                if chars.first() == Some(&'\u{2502}') {
                    assert!(chars.len() > 60, "a row was truncated at height {h}");
                }
            }
        }
    }

    #[test]
    fn with_nothing_going_on_the_panel_says_so() {
        let u = ui();
        let s = render(120, 34, &u);
        assert!(s.contains("none set"), "the alarms say there are none rather than showing blank");
    }
}
