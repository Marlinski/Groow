//! The interface's run loop.
//!
//! Closing this window does not touch the creature. It goes on thinking, and reopening the
//! window rejoins it where it is. Only `groow stop` puts it to sleep.

use std::io;
use std::time::Duration;

use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use serde_json::json;

use crate::link::{FromCore, Link};
use crate::state::{Sent, Ui, Who};

/// How often the creature is redrawn: blink, breath and the ticking age.
const TICK: Duration = Duration::from_millis(500);

/// How long to wait before trying the core again after it goes away.
const RETRY: Duration = Duration::from_secs(3);

/// What a keystroke means.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Nothing,
    Redraw,
    Submit,
    Leave,
    Scroll(i32),
}

/// Interpret one key. Kept separate from the terminal so it can be tested.
pub fn on_key(ui: &mut Ui, key: KeyEvent) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') | KeyCode::Char('q') if ctrl => Action::Leave,
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
    execute!(out, EnterAlternateScreen)?;
    let mut term = Terminal::new(CrosstermBackend::new(out))?;

    let result = main_loop(&mut term, socket).await;

    disable_raw_mode()?;
    execute!(term.backend_mut(), LeaveAlternateScreen)?;
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
            }
            Some(Ok(ev)) = keys.next() => {
                match ev {
                    TermEvent::Key(k) if k.kind == crossterm::event::KeyEventKind::Press => {
                        match on_key(&mut ui, k) {
                            Action::Leave => return Ok(()),
                            Action::Scroll(by) => ui.scroll(by),
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
                    TermEvent::Mouse(m) => match m.kind {
                        crossterm::event::MouseEventKind::ScrollUp => ui.scroll(-3),
                        crossterm::event::MouseEventKind::ScrollDown => ui.scroll(3),
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
                    FromCore::Reply(_, v) => {
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

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
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
