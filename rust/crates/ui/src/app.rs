//! The interface's run loop.
//!
//! Closing this window does not touch the creature. It goes on thinking, and reopening the
//! window rejoins it where it is. Only `groow stop` puts it to sleep.

use std::io;
use std::time::Duration;

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as TermEvent, KeyCode, KeyEvent, KeyModifiers,
    MouseEventKind,
};
use crossterm::cursor::MoveTo;
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use serde_json::json;

use crate::link::{FromCore, Link};
use crate::line::Editor;
use crate::state::{Pane, Sent, Ui, Who};

/// How much of the conversation is loaded when the window opens, and how much each page adds
/// after that. Enough that a day is usually there before anyone scrolls.
const PAGE: usize = 300;

/// How many turns of measurements the meta pane asks for.
const STATS: usize = 120;

/// How far a held arrow moves in the journal. About a screenful.
const STRIDE: i32 = 10;

/// How often the meta pane is refreshed while it is the one being looked at.
const STATS_EVERY: u64 = 20;

/// How often the creature is redrawn: blink, breath and the ticking age.
const TICK: Duration = Duration::from_millis(500);

/// How long to wait before trying the core again after it goes away.
const RETRY: Duration = Duration::from_secs(3);

/// The requests this window has out, so a reply can be told from the others.
///
/// Every reply arrives on the same socket with only its id to say what it answers, and a
/// pending id must survive replies that are not the one being waited for. Clearing it on a
/// reply that did not match is how the first attempt at this lost every page it asked for.
#[derive(Debug, Default)]
pub struct Pending {
    pub history: Option<u32>,
    pub older: Option<u32>,
    pub stats: Option<u32>,
    pub alarms: Option<u32>,
    pub thoughts: Option<u32>,
    /// The inside of one run, and which run it was asked about.
    pub inside: Option<u32>,
    pub inside_of: Option<String>,
}

impl Pending {
    /// Give a reply to whichever request it answers. Returns false when it answers none of
    /// them, which means it is a status or a hello and belongs to the caller.
    pub fn take(&mut self, ui: &mut Ui, id: u32, v: &serde_json::Value) -> bool {
        if self.history == Some(id) {
            self.history = None;
            ui.history(v, false);
        } else if self.older == Some(id) {
            self.older = None;
            ui.history(v, true);
        } else if self.stats == Some(id) {
            self.stats = None;
            ui.stats = v.clone();
        } else if self.alarms == Some(id) {
            self.alarms = None;
            ui.alarms = v.clone();
        } else if self.thoughts == Some(id) {
            self.thoughts = None;
            ui.thoughts(v);
        } else if self.inside == Some(id) {
            self.inside = None;
            ui.opened = self.inside_of.take();
            ui.inside = v.clone();
        } else {
            return false;
        }
        true
    }

    /// The core went away: nothing that was asked for is coming.
    pub fn forget(&mut self, ui: &mut Ui) {
        *self = Pending::default();
        ui.loading = false;
    }
}

/// What a keystroke means.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Nothing,
    Redraw,
    Submit,
    Leave,
    Scroll(i32),
    /// Show a different pane.
    Show(Pane),
    /// Stop whatever the creature is doing now.
    Interrupt,
    /// Point the journal somewhere else.
    Pick(i32),
    /// Show the journal a different way.
    Filter(crate::state::Filter),
    /// Move the arrows to another of the journal's columns.
    Sideways(bool),
}

/// Interpret one key. Kept separate from the terminal so it can be tested.
pub fn on_key(ui: &mut Ui, key: KeyEvent) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Char('c') | KeyCode::Char('q') if ctrl => Action::Leave,
        // The panes. Function keys and Tab, because every ordinary character belongs to what
        // is being typed: a window you talk to cannot spend its letters on shortcuts.
        KeyCode::F(1) => Action::Show(Pane::Conversation),
        KeyCode::F(2) => Action::Show(Pane::Journal),
        KeyCode::F(3) => Action::Show(Pane::Admin),
        KeyCode::Tab => Action::Show(ui.pane.next()),
        KeyCode::BackTab => Action::Show(ui.pane.prev()),
        KeyCode::Char('l') if ctrl => {
            ui.bubbles.clear();
            Action::Redraw
        }

        // Editing the line. The readline keys, because they are the ones every terminal has
        // already taught everybody, and a window you talk to is a window you type in.
        KeyCode::Char('a') if ctrl => edit(ui, Editor::home),
        KeyCode::Char('e') if ctrl => edit(ui, Editor::end),
        KeyCode::Char('u') if ctrl => edit(ui, Editor::kill_to_start),
        KeyCode::Char('k') if ctrl => edit(ui, Editor::kill_to_end),
        KeyCode::Char('w') if ctrl => edit(ui, Editor::kill_word_left),
        // The journal's filters. Held with alt, because every ordinary letter belongs to what
        // is being typed: a window you talk to cannot spend its letters on shortcuts.
        KeyCode::Char(c) if alt && ui.pane == Pane::Journal && crate::state::Filter::of(c).is_some() => {
            Action::Filter(crate::state::Filter::of(c).expect("just checked"))
        }
        KeyCode::Char('b') if alt => edit(ui, Editor::word_left),
        KeyCode::Char('f') if alt => edit(ui, Editor::word_right),
        KeyCode::Char('d') if alt => edit(ui, Editor::kill_word_right),
        KeyCode::Char(c) => edit(ui, |e| e.insert(c)),

        KeyCode::Backspace if alt || ctrl => edit(ui, Editor::kill_word_left),
        KeyCode::Backspace => edit(ui, Editor::backspace),
        KeyCode::Delete if alt || ctrl => edit(ui, Editor::kill_word_right),
        KeyCode::Delete => edit(ui, Editor::delete),

        // In the journal the sideways arrows choose which of the three lists the other two
        // move in. There is no line being typed there to move about in.
        KeyCode::Left if ui.pane == Pane::Journal => Action::Sideways(false),
        KeyCode::Right if ui.pane == Pane::Journal => Action::Sideways(true),
        KeyCode::Left if alt || ctrl => edit(ui, Editor::word_left),
        KeyCode::Right if alt || ctrl => edit(ui, Editor::word_right),
        KeyCode::Left => edit(ui, Editor::left),
        KeyCode::Right => edit(ui, Editor::right),
        // In the journal these belong to the list: it is what is being read, and the line is
        // not even drawn there. Held with alt or ctrl the arrows stride, because a day is a
        // lot of runs. Everywhere else they are all the line's own.
        KeyCode::Home if ui.pane == Pane::Journal => Action::Pick(i32::MIN / 2),
        KeyCode::End if ui.pane == Pane::Journal => Action::Pick(i32::MAX / 2),
        KeyCode::Home => edit(ui, Editor::home),
        KeyCode::End => edit(ui, Editor::end),

        KeyCode::Up if ui.pane == Pane::Journal && (alt || ctrl) => Action::Pick(-STRIDE),
        KeyCode::Down if ui.pane == Pane::Journal && (alt || ctrl) => Action::Pick(STRIDE),
        KeyCode::Up if ui.pane == Pane::Journal => Action::Pick(-1),
        KeyCode::Down if ui.pane == Pane::Journal => Action::Pick(1),
        KeyCode::PageUp if ui.pane == Pane::Journal => Action::Pick(-STRIDE),
        KeyCode::PageDown if ui.pane == Pane::Journal => Action::Pick(STRIDE),
        KeyCode::Up => edit(ui, Editor::earlier),
        KeyCode::Down => edit(ui, Editor::later),

        KeyCode::Enter => Action::Submit,
        // Stop what it is doing. Clearing the line is ctrl-u, as it is everywhere else.
        KeyCode::Esc => Action::Interrupt,

        KeyCode::PageUp => Action::Scroll(-10),
        KeyCode::PageDown => Action::Scroll(10),
        _ => Action::Nothing,
    }
}

/// Change the line and redraw. Every editing key does exactly this.
fn edit(ui: &mut Ui, f: impl FnOnce(&mut Editor)) -> Action {
    f(&mut ui.input);
    Action::Redraw
}

/// Run the interface until the person leaves.
pub async fn run(socket: std::path::PathBuf) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    // Take the wheel, so scrolling reads the conversation rather than the terminal's own
    // scrollback, which in here holds nothing.
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;

    // A panic in here would otherwise leave the terminal in raw mode with the mouse still
    // captured: no echo, no line editing, and a spray of escape codes at every click. Whatever
    // went wrong, the terminal is given back first and the panic is printed afterwards.
    let existing = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        give_the_terminal_back();
        existing(info);
    }));

    // Being killed is not the same as leaving, and the terminal does not know the difference:
    // whatever happens, the escape codes that put it in this state have to be undone. A window
    // whose body is rebuilt under it, or whose session is hung up, used to leave a terminal in
    // the alternate screen with the mouse still captured and no way to type into it.
    let signals = tokio::spawn(async move {
        let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut hup = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
            Ok(s) => s,
            Err(_) => return,
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = hup.recv() => {}
        }
        give_the_terminal_back();
        eprintln!("the window was stopped. It is still awake.");
        std::process::exit(0);
    });

    let mut term = Terminal::new(CrosstermBackend::new(out))?;
    let result = main_loop(&mut term, socket).await;
    signals.abort();

    give_the_terminal_back();
    let _ = std::panic::take_hook();
    println!("Window closed. It is still awake; `groow ui` comes back, `groow stop` puts it to sleep.");
    result
}

/// Put the terminal back exactly as it was found, whatever happened.
///
/// Nothing here can fail the caller: every step is attempted even if an earlier one did not
/// work, because the alternative is a shell nobody can type into. The screen is cleared before
/// the alternate one is left so that a terminal without alternate screens is wiped rather than
/// left with the window painted across the prompt.
fn give_the_terminal_back() {
    let mut out = io::stdout();
    let _ = execute!(out, Clear(ClearType::All), MoveTo(0, 0));
    let _ = execute!(out, DisableMouseCapture);
    let _ = execute!(out, LeaveAlternateScreen);
    let _ = crossterm::execute!(out, crossterm::cursor::Show);
    let _ = disable_raw_mode();
}

async fn main_loop<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    socket: std::path::PathBuf,
) -> anyhow::Result<()> {
    let mut ui = Ui::default();
    let mut keys = crossterm::event::EventStream::new();
    let mut ticker = tokio::time::interval(TICK);
    let mut link: Option<Link> = None;
    let mut from_core: Option<tokio::sync::mpsc::Receiver<FromCore>> = None;
    let mut retry_at = std::time::Instant::now();
    // Which request a reply belongs to. Everything else on this socket is an event.
    let mut pending = Pending::default();

    loop {
        if ui.done {
            return Ok(());
        }
        // Reconnect quietly in the background; the conversation stays on screen throughout.
        if link.is_none() && std::time::Instant::now() >= retry_at {
            match Link::open(&socket).await {
                Ok((mut l, rx)) => {
                    let _ = l.send("hello", json!({})).await;
                    let _ = l.send("watch", json!({})).await;
                    let _ = l.send("status", json!({})).await;
                    // What was said before this window was opened. Asked for once; the rest
                    // arrives a page at a time as someone reads back.
                    if ui.bubbles.is_empty() {
                        ui.loading = true;
                        pending.history = l.send("recall", json!({"n": PAGE})).await.ok();
                    }
                    pending.stats = l.send("stats", json!({"n": STATS})).await.ok();
                    pending.alarms = l.send("schedule", json!({"action": "list"})).await.ok();
                    // What is already under way is not news and never arrives as an event.
                    pending.thoughts = l.send("thought", json!({"action": "list"})).await.ok();
                    link = Some(l);
                    from_core = Some(rx);
                    ui.connected = true;
                    ui.trouble = None;
                    ui.push(Who::System, "", "connected");
                }
                Err(e) => {
                    if ui.connected || ui.bubbles.is_empty() {
                        ui.push(Who::System, "", &format!("no core at {} ({e}); retrying", socket.display()));
                    }
                    ui.connected = false;
                    retry_at = std::time::Instant::now() + RETRY;
                }
            }
        }

        // The journal is pointing at a run nobody has read yet.
        if ui.want_inside {
            ui.want_inside = false;
            if let (Some(l), Some(r)) = (link.as_mut(), ui.pointing_at()) {
                if ui.opened.as_deref() != Some(r.id.as_str()) {
                    ui.inside = serde_json::Value::Null;
                    ui.opened = None;
                    // Exactly what was sent during it, which is what the other two columns
                    // are made of. A thought's trace is kept under its own id like a turn's.
                    pending.inside = l.send("trace", json!({"turn": r.id})).await.ok();
                    pending.inside_of = Some(r.id);
                }
            }
        }

        // Reading back past the top: ask for the page before the oldest thing held.
        if ui.want_more {
            ui.want_more = false;
            if let (Some(l), Some(oldest)) = (link.as_mut(), ui.oldest()) {
                ui.loading = true;
                pending.older = l.send("recall", json!({"n": PAGE, "before": oldest})).await.ok();
                if pending.older.is_none() {
                    ui.loading = false;
                }
            }
        }

        term.draw(|f| crate::draw::draw(f, &ui))?;

        tokio::select! {
            _ = ticker.tick() => {
                ui.tick = ui.tick.wrapping_add(1);
                // Ask for a fresh status every few seconds so the card and bar stay true.
                if ui.tick % 8 == 0 {
                    if let Some(l) = link.as_mut() {
                        if l.send("status", json!({})).await.is_err() {
                            ui.connected = false;
                        }
                    }
                }
                // The measurements move slowly and only matter while being looked at.
                if ui.tick % STATS_EVERY == 0 && ui.pane == Pane::Admin {
                    if let Some(l) = link.as_mut() {
                        pending.stats = l.send("stats", json!({"n": STATS})).await.ok();
                        pending.alarms = l.send("schedule", json!({"action": "list"})).await.ok();
                        pending.thoughts = l.send("thought", json!({"action": "list"})).await.ok();
                    }
                }
            }
            Some(Ok(ev)) = keys.next() => {
                match ev {
                    TermEvent::Key(k) if k.kind == crossterm::event::KeyEventKind::Press => {
                        match on_key(&mut ui, k) {
                            Action::Leave => return Ok(()),
                            Action::Scroll(by) => ui.scroll(by),
                            Action::Pick(by) => ui.pick(by),
                            Action::Filter(f) => ui.show(f),
                            Action::Sideways(right) => ui.sideways(right),
                            Action::Interrupt => match link.as_mut() {
                                Some(l) => {
                                    if l.send("interrupt", json!({})).await.is_err() {
                                        ui.connected = false;
                                    }
                                }
                                None => ui.push(Who::System, "", "not connected; nothing to stop"),
                            },
                            Action::Show(p) => {
                                ui.pane = p;
                                ui.scroll_back = 0;
                                // Arriving at the journal, it is already pointing at something
                                // and nobody has read the inside of it yet.
                                ui.want_inside = p == Pane::Journal;
                                if p == Pane::Admin {
                                    if let Some(l) = link.as_mut() {
                                        pending.stats = l.send("stats", json!({"n": STATS})).await.ok();
                                        pending.alarms = l.send("schedule", json!({"action": "list"})).await.ok();
                                    }
                                }
                            }
                            Action::Submit => {
                                if let Some(sent) = ui.submit() {
                                    match link.as_mut() {
                                        Some(l) => {
                                            let r = match &sent {
                                                Sent::Say(t) => l.say(t).await,
                                                Sent::Command(t) => l.command(t).await,
                                            };
                                            if r.is_err() {
                                                ui.connected = false;
                                                link = None;
                                                ui.push(Who::System, "", "that did not get through; reconnecting");
                                            }
                                        }
                                        None => ui.push(Who::System, "", "not connected yet; nothing was sent"),
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    // The wheel reads the conversation. Without capture this scrolls the
                    // terminal's own scrollback, which in an alternate screen holds nothing.
                    TermEvent::Mouse(m) => match m.kind {
                        MouseEventKind::ScrollUp => ui.scroll(-3),
                        MouseEventKind::ScrollDown => ui.scroll(3),
                        _ => {}
                    },
                    _ => {}
                }
            }
            Some(msg) = async {
                match from_core.as_mut() {
                    Some(rx) => rx.recv().await,
                    // Nothing to read from; let the other branches run.
                    None => std::future::pending().await,
                }
            } => {
                match msg {
                    FromCore::Event(name, data) => ui.on_event(&name, &data),
                    FromCore::Reply(id, v) => {
                        pending.take(&mut ui, id, &v);
                        if let Some(b) = v.get("birth") {
                            ui.birth = b.clone();
                        }
                        // A hello carries one inside it; a status reply is one.
                        if let Some(s) = v.get("status") {
                            ui.status(s.clone());
                        } else if v.get("queue").is_some() {
                            ui.status(v.clone());
                        }
                    }
                    FromCore::Failed(_, why) => ui.push(Who::System, "error", &why),
                    FromCore::Gone => {
                        ui.connected = false;
                        link = None;
                        from_core = None;
                        pending.forget(&mut ui);
                        retry_at = std::time::Instant::now() + RETRY;
                        ui.push(Who::System, "", "the core went away; reconnecting");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn a_reply_to_something_else_does_not_lose_the_page_being_waited_for() {
        // Everything answers on one socket. Clearing the pending id on the first reply that
        // came along, whatever it answered, left the window loading forever with no history.
        let mut ui = Ui::default();
        let mut p = Pending { history: Some(7), stats: Some(9), ..Default::default() };
        ui.loading = true;

        assert!(!p.take(&mut ui, 3, &json!({"queue": 0})), "a status answers none of them");
        assert_eq!(p.history, Some(7), "and must not consume what is still expected");

        assert!(p.take(&mut ui, 7, &json!({"more": false, "messages": [
            {"role": "user", "content": "hello", "ts": 1.0}
        ]})));
        assert_eq!(ui.bubbles.len(), 1);
        assert!(!ui.loading);
        assert_eq!(p.stats, Some(9), "the other request is still out");
    }

    #[test]
    fn a_page_that_is_never_coming_does_not_leave_it_loading() {
        let mut ui = Ui::default();
        ui.loading = true;
        let mut p = Pending { older: Some(4), ..Default::default() };
        p.forget(&mut ui);
        assert!(!ui.loading, "the core went away; nothing asked for is coming");
        assert_eq!(p.older, None);
    }

    #[test]
    fn the_journal_walks_one_run_at_a_time_and_strides_when_asked() {
        let mut ui = Ui::default();
        ui.pane = Pane::Journal;
        let down = |m| KeyEvent::new(KeyCode::Down, m);
        assert_eq!(on_key(&mut ui, down(KeyModifiers::NONE)), Action::Pick(1));
        assert_eq!(on_key(&mut ui, down(KeyModifiers::ALT)), Action::Pick(STRIDE));
        assert_eq!(on_key(&mut ui, down(KeyModifiers::CONTROL)), Action::Pick(STRIDE));
        assert_eq!(
            on_key(&mut ui, KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)),
            Action::Pick(STRIDE)
        );

        // And in the conversation the same keys are the line's, as they always were.
        ui.pane = Pane::Conversation;
        assert_eq!(on_key(&mut ui, down(KeyModifiers::NONE)), Action::Redraw);
        assert_eq!(
            on_key(&mut ui, KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)),
            Action::Scroll(10)
        );
    }

    #[test]
    fn a_stride_past_either_end_of_the_journal_lands_on_the_end() {
        let mut ui = Ui::default();
        ui.stats = json!({"turns": (0..5).map(|i| json!({
            "id": format!("t{i}"), "kind": "user", "started": 100.0 - i as f64,
            "seconds": 1.0, "outcome": "ok", "flags": ""
        })).collect::<Vec<_>>()});

        ui.pick(i32::MAX / 2);
        assert_eq!(ui.picked, 4, "the oldest, not past it");
        ui.pick(i32::MIN / 2);
        assert_eq!(ui.picked, 0, "and back to the newest");
    }

    #[test]
    fn the_panes_are_reachable_by_function_key_and_by_tab() {
        let mut ui = Ui::default();
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)), Action::Show(Pane::Journal));
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE)), Action::Show(Pane::Admin));
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)), Action::Show(Pane::Conversation));
        // Tab cycles from wherever it is, so one key reaches all three, and shift-tab goes
        // back the way it came. Both wrap: neither direction is a dead end.
        ui.pane = Pane::Journal;
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)), Action::Show(Pane::Admin));
        assert_eq!(
            on_key(&mut ui, KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
            Action::Show(Pane::Conversation)
        );
        ui.pane = Pane::Conversation;
        assert_eq!(
            on_key(&mut ui, KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
            Action::Show(Pane::Admin),
            "backwards from the first wraps to the last"
        );
    }

    #[test]
    fn typing_builds_up_the_line_and_backspace_takes_it_back() {
        let mut ui = Ui::default();
        for c in "hell".chars() {
            assert_eq!(on_key(&mut ui, key(c)), Action::Redraw);
        }
        on_key(&mut ui, KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        on_key(&mut ui, key('p'));
        assert_eq!(ui.input.text(), "help");
    }

    #[test]
    fn control_c_and_control_q_both_leave() {
        let mut ui = Ui::default();
        for c in ['c', 'q'] {
            assert_eq!(
                on_key(&mut ui, KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)),
                Action::Leave
            );
        }
    }

    #[test]
    fn a_plain_q_is_typed_rather_than_leaving() {
        let mut ui = Ui::default();
        assert_eq!(on_key(&mut ui, key('q')), Action::Redraw);
        assert_eq!(ui.input.text(), "q", "a person writing the word 'question' must not be thrown out");
    }

    #[test]
    fn escape_stops_what_it_is_doing_and_leaves_the_line_alone() {
        // It used to throw the line away, which is what ctrl-u is for. Stopping a turn is the
        // thing there was no key for at all, and it is the one anybody reaches for first.
        let mut ui = Ui::default();
        ui.input.set("half a thought");
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)), Action::Interrupt);
        assert_eq!(ui.input.text(), "half a thought", "what was being typed is not collateral");

        on_key(&mut ui, KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(ui.input.is_empty(), "and ctrl-u clears the line, as everywhere else");
    }

    #[test]
    fn the_line_is_edited_with_the_keys_every_terminal_teaches() {
        let mut ui = Ui::default();
        for c in "read the news".chars() {
            on_key(&mut ui, key(c));
        }
        // Stand at the start of the last word: what is deleted is the word before the cursor,
        // and what is after it is left exactly as it was.
        on_key(&mut ui, KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        on_key(&mut ui, KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(ui.input.text(), "read news", "alt-backspace takes the word behind it");

        on_key(&mut ui, KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT));
        assert_eq!(ui.input.text(), "read ", "and alt-d the word in front of it");

        for c in "the papers".chars() {
            on_key(&mut ui, key(c));
        }
        on_key(&mut ui, KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(ui.input.cursor(), 0, "ctrl-a goes to the start");
        on_key(&mut ui, KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
        assert_eq!(ui.input.cursor(), 4, "alt-right crosses a whole word");
        on_key(&mut ui, KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(ui.input.cursor(), ui.input.len(), "ctrl-e goes to the end");
    }

    #[test]
    fn the_arrows_reach_back_through_what_was_said() {
        let mut ui = Ui::default();
        ui.input.set("read the news");
        ui.submit();
        for c in "half".chars() {
            on_key(&mut ui, key(c));
        }
        on_key(&mut ui, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(ui.input.text(), "read the news");
        on_key(&mut ui, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(ui.input.text(), "half", "and forward again to what was being written");
    }

    #[test]
    fn the_page_keys_scroll_now_that_the_arrows_belong_to_the_line() {
        let mut ui = Ui::default();
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)), Action::Scroll(-10));
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)), Action::Scroll(10));
        // Up and down are the line's history; reading the conversation is the wheel and these.
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)), Action::Redraw);
    }

    #[test]
    fn control_l_clears_the_view_without_leaving() {
        let mut ui = Ui::default();
        ui.push(Who::Human, "you", "something");
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL)), Action::Redraw);
        assert!(ui.bubbles.is_empty());
        assert!(!ui.done);
    }

    #[test]
    fn enter_submits_what_was_typed() {
        let mut ui = Ui::default();
        ui.input.set("hello");
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), Action::Submit);
        assert_eq!(ui.submit(), Some(Sent::Say("hello".into())));
    }

    #[test]
    fn unicode_is_typed_correctly() {
        let mut ui = Ui::default();
        for c in "caf\u{e9} \u{1f331}".chars() {
            on_key(&mut ui, key(c));
        }
        assert_eq!(ui.input.text(), "caf\u{e9} \u{1f331}");
        on_key(&mut ui, KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(ui.input.text(), "caf\u{e9} ", "backspace should remove a character, not a byte");
    }
}
