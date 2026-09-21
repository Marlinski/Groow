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
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use serde_json::json;

use crate::link::{FromCore, Link};
use crate::state::{Pane, Sent, Ui, Who};

/// How much of the conversation is loaded when the window opens, and how much each page adds
/// after that. Enough that a day is usually there before anyone scrolls.
const PAGE: usize = 300;

/// How many turns of measurements the meta pane asks for.
const STATS: usize = 120;

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
}

/// Interpret one key. Kept separate from the terminal so it can be tested.
pub fn on_key(ui: &mut Ui, key: KeyEvent) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') | KeyCode::Char('q') if ctrl => Action::Leave,
        // The panes. Function keys and Tab, because every ordinary character belongs to what
        // is being typed: a window you talk to cannot spend its letters on shortcuts.
        KeyCode::F(1) => Action::Show(Pane::Conversation),
        KeyCode::F(2) => Action::Show(Pane::Tools),
        KeyCode::F(3) => Action::Show(Pane::Admin),
        KeyCode::Tab => Action::Show(ui.pane.next()),
        KeyCode::Char('l') if ctrl => {
            ui.bubbles.clear();
            Action::Redraw
        }
        KeyCode::Char(c) => {
            ui.input.push(c);
            Action::Redraw
        }
        KeyCode::Backspace => {
            ui.input.pop();
            Action::Redraw
        }
        KeyCode::Enter => Action::Submit,
        KeyCode::Esc => {
            ui.input.clear();
            Action::Redraw
        }
        KeyCode::PageUp => Action::Scroll(-10),
        KeyCode::PageDown => Action::Scroll(10),
        KeyCode::Up => Action::Scroll(-1),
        KeyCode::Down => Action::Scroll(1),
        KeyCode::End => Action::Scroll(i32::MAX / 2),
        _ => Action::Nothing,
    }
}

/// Run the interface until the person leaves.
pub async fn run(socket: std::path::PathBuf) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    // Take the wheel, so scrolling reads the conversation rather than the terminal's own
    // scrollback, which in here holds nothing.
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let mut term = Terminal::new(CrosstermBackend::new(out))?;

    let result = main_loop(&mut term, socket).await;

    disable_raw_mode()?;
    execute!(term.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    term.show_cursor()?;
    println!("Window closed. It is still awake; `groow ui` comes back, `groow stop` puts it to sleep.");
    result
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
                    }
                }
            }
            Some(Ok(ev)) = keys.next() => {
                match ev {
                    TermEvent::Key(k) if k.kind == crossterm::event::KeyEventKind::Press => {
                        match on_key(&mut ui, k) {
                            Action::Leave => return Ok(()),
                            Action::Scroll(by) => ui.scroll(by),
                            Action::Show(p) => {
                                ui.pane = p;
                                ui.scroll_back = 0;
                                if p == Pane::Admin {
                                    if let Some(l) = link.as_mut() {
                                        pending.stats = l.send("stats", json!({"n": STATS})).await.ok();
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
                        if let Some(s) = v.get("status") {
                            ui.status = s.clone();
                        } else if v.get("queue").is_some() {
                            ui.status = v.clone();
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
    fn the_panes_are_reachable_by_function_key_and_by_tab() {
        let mut ui = Ui::default();
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)), Action::Show(Pane::Tools));
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE)), Action::Show(Pane::Admin));
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)), Action::Show(Pane::Conversation));
        // Tab cycles from wherever it is, so one key reaches all three.
        ui.pane = Pane::Tools;
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)), Action::Show(Pane::Admin));
    }

    #[test]
    fn typing_builds_up_the_line_and_backspace_takes_it_back() {
        let mut ui = Ui::default();
        for c in "hell".chars() {
            assert_eq!(on_key(&mut ui, key(c)), Action::Redraw);
        }
        on_key(&mut ui, KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        on_key(&mut ui, key('p'));
        assert_eq!(ui.input, "help");
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
        assert_eq!(ui.input, "q", "a person writing the word 'question' must not be thrown out");
    }

    #[test]
    fn escape_abandons_the_line_without_sending_it() {
        let mut ui = Ui::default();
        ui.input = "half a thought".into();
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)), Action::Redraw);
        assert!(ui.input.is_empty());
    }

    #[test]
    fn the_arrows_and_page_keys_scroll() {
        let mut ui = Ui::default();
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)), Action::Scroll(-10));
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)), Action::Scroll(-1));
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)), Action::Scroll(1));
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
        ui.input = "hello".into();
        assert_eq!(on_key(&mut ui, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), Action::Submit);
        assert_eq!(ui.submit(), Some(Sent::Say("hello".into())));
    }

    #[test]
    fn unicode_is_typed_correctly() {
        let mut ui = Ui::default();
        for c in "caf\u{e9} \u{1f331}".chars() {
            on_key(&mut ui, key(c));
        }
        assert_eq!(ui.input, "caf\u{e9} \u{1f331}");
        on_key(&mut ui, KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(ui.input, "caf\u{e9} ", "backspace should remove a character, not a byte");
    }
}
