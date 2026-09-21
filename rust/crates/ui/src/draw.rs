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
    let input_h = (typing.rows.len() as u16 + 2).clamp(3, input_limit(area.height));

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

    if area.width >= NARROW {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(30), Constraint::Length(SIDE)])
            .split(body);
        pane(f, cols[0], ui);
        side(f, cols[1], ui);
    } else {
        // On a narrow terminal the creature gives way to the words.
        pane(f, body, ui);
    }
    input(f, rows[2], ui, &typing);
    status(f, rows[3], ui);
}

/// Whichever view is being looked at, in the same frame, so switching costs nothing.
fn pane(f: &mut Frame, area: Rect, ui: &Ui) {
    match ui.pane {
        Pane::Conversation => conversation(f, area, ui, ui.bubbles.iter().collect()),
        Pane::Tools => conversation(f, area, ui, ui.tool_lines()),
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
fn meta(f: &mut Frame, area: Rect, ui: &Ui) {
    let mut lines: Vec<Line> = Vec::new();
    let get = |k: &str| ui.stats.get(k).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    // Nothing here wraps: these are rows, and a row that folds onto the next line stops being
    // a table. Anything too long is cut instead.
    let w = area.width.saturating_sub(4) as usize;

    let passes = get("learning");
    lines.push(head("learning"));
    if passes.is_empty() {
        lines.push(quiet("nothing has run yet"));
    }
    for p in passes.iter().take(14) {
        let kind = p.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
        let n = p.get("samples").and_then(|v| v.as_i64()).unwrap_or(0);
        let loss = p.get("loss").and_then(|v| v.as_f64());
        let note = p.get("note").and_then(|v| v.as_str()).unwrap_or("");
        let when = p.get("ts").and_then(|v| v.as_f64()).map(ago).unwrap_or_default();
        lines.push(Line::from(vec![
            Span::styled(format!("  {when:>8}  "), Style::default().fg(DIM)),
            Span::styled(format!("{kind:<12}"), Style::default().fg(VIOLET)),
            Span::styled(format!("{n:>5} samples  "), Style::default().fg(FG)),
            Span::styled(
                loss.map(|l| format!("loss {l:.3}")).unwrap_or_default(),
                Style::default().fg(MINT),
            ),
            Span::styled(format!("  {}", clip(note, w.saturating_sub(48))), Style::default().fg(DIM)),
        ]));
    }

    // Where the two numbers that matter are going. Loss should fall; feeling should not.
    lines.push(Line::from(""));
    lines.push(head("trend"));
    let losses: Vec<f64> = passes.iter().rev().filter_map(|p| p.get("loss").and_then(|v| v.as_f64())).collect();
    lines.push(spark("loss   ", &losses, MINT, w));
    let valence: Vec<f64> = get("feelings")
        .iter()
        .filter_map(|x| x.get("valence").and_then(|v| v.as_f64()))
        .collect();
    lines.push(spark("feeling", &valence, AMBER, w));

    lines.push(Line::from(""));
    lines.push(head("turns"));
    let turns = get("turns");
    let shown = turns.iter().take(10);
    if turns.is_empty() {
        lines.push(quiet("none recorded"));
    }
    for t in shown {
        let kind = t.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
        let secs = t.get("seconds").and_then(|v| v.as_f64());
        let tools = t.get("tools").and_then(|v| v.as_i64()).unwrap_or(0);
        let outcome = t.get("outcome").and_then(|v| v.as_str()).unwrap_or("");
        let flags = t.get("flags").and_then(|v| v.as_str()).unwrap_or("");
        let when = t.get("started").and_then(|v| v.as_f64()).map(ago).unwrap_or_default();
        let colour = if outcome == "ok" { FG } else { ROSE };
        lines.push(Line::from(vec![
            Span::styled(format!("  {when:>8}  "), Style::default().fg(DIM)),
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
        ]));
    }

    lines.push(Line::from(""));
    lines.push(head("commands"));
    for t in get("tools").iter().take(8) {
        let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let used = t.get("used").and_then(|v| v.as_i64()).unwrap_or(0);
        let failed = t.get("failed").and_then(|v| v.as_f64()).unwrap_or(0.0);
        lines.push(Line::from(vec![
            Span::styled(format!("  {name:<16}"), Style::default().fg(SKY)),
            Span::styled(format!("{used:>5} runs  "), Style::default().fg(FG)),
            Span::styled(
                format!("{:.0}% failed", failed * 100.0),
                Style::default().fg(if failed > 0.25 { ROSE } else { DIM }),
            ),
        ]));
    }

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

fn head(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!(" {text}"),
        Style::default().fg(MOSS).add_modifier(Modifier::BOLD),
    ))
}

fn quiet(text: &str) -> Line<'static> {
    Line::from(Span::styled(format!("  {text}"), Style::default().fg(DIM)))
}

/// A line of numbers as eight heights, oldest on the left.
///
/// Scaled between its own smallest and largest, so it shows the shape of the change rather
/// than the size of it; the numbers themselves are on the rows above.
fn spark(label: &str, v: &[f64], colour: Color, width: usize) -> Line<'static> {
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

/// How long ago, said the way a person would say it.
fn ago(ts: f64) -> String {
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
    let thoughts_h: u16 = 4;
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
        f.render_widget(Paragraph::new(creature::render(ui.age(now), ui.mood, ui.tick, true)), rows[i]);
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
    f.render_widget(
        Paragraph::new(thoughts(ui))
            // Not trimmed: the indent is what shows a goal belongs to the line above it.
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(EDGE))
                    .title(Span::styled(" inner thoughts ", Style::default().fg(DIM))),
            ),
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

fn thoughts(ui: &Ui) -> Vec<Line<'static>> {
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
        format!(" {} ", ui.mood.caption()),
        Style::default().fg(MINT).add_modifier(Modifier::BOLD),
    )];
    spans.push(Span::styled(
        format!(
            "\u{b7} queue {} \u{b7} thoughts {} \u{b7} turns {} ",
            n("queue"),
            n("thoughts"),
            n("turns")
        ),
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
        u.mood = crate::creature::Mood::Listening;
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
    fn the_panes_are_named_and_the_one_in_front_is_marked() {
        let mut u = ui();
        let s = render(100, 30, &u);
        assert!(s.contains("F1 conversation"), "{s}");
        assert!(s.contains("F2 commands"), "{s}");
        assert!(s.contains("F3 meta"), "{s}");

        u.pane = Pane::Tools;
        assert!(render(100, 30, &u).contains(" commands "), "the pane's own title should change");
    }

    #[test]
    fn the_commands_pane_shows_what_was_run_and_nothing_else() {
        let mut u = ui();
        u.push(Who::Human, "you", "what is in this directory?");
        u.push(Who::Tool, "", "\u{2699} shell(ls -1 | wc -l)");
        u.push(Who::System, "", "  6");
        u.push(Who::Groow, "groow", "There are six.");

        u.pane = Pane::Tools;
        let s = render(100, 30, &u);
        assert!(s.contains("shell(ls -1"), "{s}");
        assert!(!s.contains("There are six"), "what was said belongs to the other pane: {s}");
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
    fn inner_thoughts_are_listed_with_their_goals() {
        let mut u = ui();
        u.on_event("thought", &json!({"id": "ab12", "status": "running", "goal": "read about rivers"}));
        let s = render(120, 34, &u);
        assert!(s.contains("ab12"));
        assert!(s.contains("read about"));
    }

    #[test]
    fn the_creature_is_drawn_whole_with_its_caption() {
        let u = ui();
        let s = render(100, 34, &u);
        assert!(s.contains("waiting \u{b7} young") || s.contains("listening \u{b7} young"),
            "the caption under the creature is missing:\n{s}");
        assert!(s.contains("\u{2588}\u{2588}"), "the creature should be drawn");
    }

    #[test]
    fn the_side_panel_sheds_the_least_important_things_first() {
        let mut u = ui();
        u.on_event("thought", &json!({"id": "ab12", "status": "running", "goal": "read about rivers"}));
        // Tall: everything, including the whole card.
        let tall = render(100, 36, &u);
        assert!(tall.contains("3f2a19cd"), "the id should be there");
        assert!(tall.contains("mentor"), "the whole card should fit");
        assert!(tall.contains("ab12"));
        // Middling: the creature and the thoughts survive, the card is trimmed.
        let mid = render(100, 30, &u);
        assert!(mid.contains("\u{2588}\u{2588}"), "the creature should still be there at 30 rows");
        assert!(mid.contains("ab12"), "what it is thinking about must always be visible");
        // Short: the creature goes, the thoughts stay.
        let short = render(100, 16, &u);
        assert!(short.contains("ab12"));
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
        assert!(s.contains("none just now"));
    }
}
