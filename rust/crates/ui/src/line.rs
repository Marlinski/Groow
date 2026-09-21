//! The line you are typing.
//!
//! A window you talk to is a window you type in, and typing means editing: moving about the
//! line, fixing a word in the middle of it, reaching back for something said earlier. This is
//! the readline vocabulary, because that is the one every terminal has taught everybody.
//!
//! Positions are counted in characters, never in bytes. A line with an emoji or an accent in it
//! must not be splittable down the middle, and byte arithmetic on a `String` is exactly how
//! that happens.

/// How many lines are remembered. Enough to reach back through an evening.
const REMEMBERED: usize = 200;

#[derive(Debug, Default, Clone)]
pub struct Editor {
    chars: Vec<char>,
    /// Where the next character goes: 0 is before the first, len is after the last.
    cursor: usize,
    /// What has been sent from here, oldest first.
    history: Vec<String>,
    /// How far back through the history we are; None is the line being written.
    place: Option<usize>,
    /// What was being written when the history was first reached back into, so coming forward
    /// again returns it rather than an empty line.
    draft: String,
}

impl Editor {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// Where the cursor is, in characters from the start.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn len(&self) -> usize {
        self.chars.len()
    }

    pub fn set(&mut self, text: &str) {
        self.chars = text.chars().collect();
        self.cursor = self.chars.len();
    }

    pub fn insert(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    /// Delete the character under the cursor.
    pub fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.chars.len());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.chars.len();
    }

    /// The start of the word to the left: over any spaces first, then over the word itself.
    pub fn word_left(&mut self) {
        while self.cursor > 0 && self.chars[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
        }
        while self.cursor > 0 && !self.chars[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
        }
    }

    /// The end of the word to the right, by the same rule.
    pub fn word_right(&mut self) {
        let n = self.chars.len();
        while self.cursor < n && self.chars[self.cursor].is_whitespace() {
            self.cursor += 1;
        }
        while self.cursor < n && !self.chars[self.cursor].is_whitespace() {
            self.cursor += 1;
        }
    }

    /// Rub out the word before the cursor, leaving what is after it alone.
    pub fn kill_word_left(&mut self) {
        let was = self.cursor;
        self.word_left();
        self.chars.drain(self.cursor..was);
    }

    pub fn kill_word_right(&mut self) {
        let was = self.cursor;
        self.word_right();
        let to = self.cursor;
        self.cursor = was;
        self.chars.drain(was..to);
    }

    /// Everything before the cursor goes.
    pub fn kill_to_start(&mut self) {
        self.chars.drain(..self.cursor);
        self.cursor = 0;
    }

    /// Everything from the cursor on goes.
    pub fn kill_to_end(&mut self) {
        self.chars.truncate(self.cursor);
    }

    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
        self.place = None;
        self.draft.clear();
    }

    /// Take the line to send it, remembering it. Nothing empty is remembered, and nothing that
    /// repeats the line before it, because a history full of the same line is no history.
    pub fn take(&mut self) -> String {
        let text = self.text();
        self.chars.clear();
        self.cursor = 0;
        self.place = None;
        self.draft.clear();
        let trimmed = text.trim();
        if !trimmed.is_empty() && self.history.last().map(|l| l != trimmed).unwrap_or(true) {
            self.history.push(trimmed.to_string());
            if self.history.len() > REMEMBERED {
                self.history.remove(0);
            }
        }
        text
    }

    /// Remember a line somebody else already sent, as if it had been typed here.
    ///
    /// What "up" should give is everything you have said to it, not only what you have said
    /// since this window was opened. The conversation is loaded when the window opens, and the
    /// lines in it that came from a person are exactly that history.
    pub fn remember(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() || self.history.last().map(|l| l == line).unwrap_or(false) {
            return;
        }
        self.history.push(line.to_string());
        if self.history.len() > REMEMBERED {
            self.history.remove(0);
            self.place = self.place.map(|i| i.saturating_sub(1));
        }
    }

    /// Remember lines older than everything remembered so far, which is what arrives when the
    /// window reads further back. Where you are in the history does not move.
    pub fn remember_older(&mut self, lines: Vec<String>) {
        let mut older: Vec<String> = Vec::new();
        for l in lines {
            let l = l.trim().to_string();
            if l.is_empty() || older.last().map(|p| *p == l).unwrap_or(false) {
                continue;
            }
            older.push(l);
        }
        if older.is_empty() {
            return;
        }
        if older.last() == self.history.first() {
            older.pop();
        }
        let grew = older.len();
        older.append(&mut self.history);
        self.history = older;
        self.place = self.place.map(|i| i + grew);
        while self.history.len() > REMEMBERED {
            self.history.remove(0);
            self.place = self.place.map(|i| i.saturating_sub(1));
        }
    }

    /// Back through what was said before. The line being written is kept, so coming forward
    /// through the end of the history gives it back rather than an empty line.
    pub fn earlier(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.place {
            None => {
                self.draft = self.text();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.place = Some(next);
        let line = self.history[next].clone();
        self.set(&line);
    }

    /// Forward again, and past the newest back to what was being written.
    pub fn later(&mut self) {
        let Some(i) = self.place else { return };
        if i + 1 < self.history.len() {
            self.place = Some(i + 1);
            let line = self.history[i + 1].clone();
            self.set(&line);
        } else {
            self.place = None;
            let draft = std::mem::take(&mut self.draft);
            self.set(&draft);
        }
    }
}

/// Break a line to a width, and say where the cursor lands once it is broken.
///
/// The wrapping and the cursor have to come from the same place. Drawing the text with one
/// rule and guessing the cursor with another is how a cursor ends up a line and a half from
/// where the next character actually goes.
///
/// Words are kept whole where they fit and split where they do not, because a word longer than
/// the box has to go somewhere. The space a line is broken on is absorbed: it is what the break
/// is made of, and showing it would put the cursor a column past the edge.
pub fn wrap(text: &str, width: usize, cursor: usize) -> Wrapped {
    let width = width.max(1);
    let chars: Vec<char> = text.chars().collect();
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    let mut row_start = 0usize;
    let mut at = (0usize, 0usize);
    let mut i = 0usize;

    let place = |i: usize, row_start: usize, rows: &[String], at: &mut (usize, usize)| {
        if i == cursor {
            *at = (rows.len(), i - row_start);
        }
    };

    while i < chars.len() {
        place(i, row_start, &rows, &mut at);
        // Where the word containing this character ends.
        let word_end = if chars[i].is_whitespace() {
            i + 1
        } else {
            let mut j = i;
            while j < chars.len() && !chars[j].is_whitespace() {
                j += 1;
            }
            j
        };
        let word_len = word_end - i;
        let used = i - row_start;

        // Only at the start of a word. Applied part way through one, this would break the
        // tail of a word that had already been split, giving a ragged edge for no reason.
        let starts_a_word = i == 0 || chars[i - 1].is_whitespace();
        if starts_a_word && !chars[i].is_whitespace() && word_len <= width && used + word_len > width {
            // It does not fit here but would fit on a line of its own: break before it.
            rows.push(std::mem::take(&mut row));
            row_start = i;
            place(i, row_start, &rows, &mut at);
        }

        if chars[i].is_whitespace() && i - row_start >= width {
            // The break falls on a space: the space is the break.
            rows.push(std::mem::take(&mut row));
            i += 1;
            row_start = i;
            continue;
        }
        if i - row_start >= width {
            rows.push(std::mem::take(&mut row));
            row_start = i;
            place(i, row_start, &rows, &mut at);
        }
        row.push(chars[i]);
        i += 1;
    }
    place(i, row_start, &rows, &mut at);
    rows.push(row);
    Wrapped { rows, row: at.0, col: at.1 }
}

/// A line broken to fit, and where the cursor is in it.
#[derive(Debug, Clone, PartialEq)]
pub struct Wrapped {
    pub rows: Vec<String>,
    /// Which row the cursor is on, counting from zero.
    pub row: usize,
    /// How far along that row, in characters.
    pub col: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> Editor {
        let mut e = Editor::default();
        for c in s.chars() {
            e.insert(c);
        }
        e
    }

    #[test]
    fn typing_puts_the_cursor_after_what_was_typed() {
        let e = typed("hello");
        assert_eq!(e.text(), "hello");
        assert_eq!(e.cursor(), 5);
    }

    #[test]
    fn a_character_can_be_fixed_in_the_middle_without_retyping_the_rest() {
        let mut e = typed("helo there");
        for _ in 0..7 {
            e.left();
        }
        e.insert('l');
        assert_eq!(e.text(), "hello there");
        assert_eq!(e.cursor(), 4);
    }

    #[test]
    fn words_are_crossed_whole() {
        let mut e = typed("read the news please");
        e.word_left();
        assert_eq!(e.cursor(), 14, "back over `please`");
        e.word_left();
        assert_eq!(e.cursor(), 9, "back over `news`");
        e.word_right();
        assert_eq!(e.cursor(), 13, "and forward over it again");
    }

    #[test]
    fn a_word_is_rubbed_out_without_touching_what_follows_it() {
        let mut e = typed("read the news please");
        e.word_left();
        e.kill_word_left();
        assert_eq!(e.text(), "read the please");
        assert_eq!(e.cursor(), 9);
    }

    #[test]
    fn the_line_can_be_cut_at_the_cursor_either_way() {
        let mut e = typed("read the news");
        e.word_left();
        let mut a = e.clone();
        a.kill_to_end();
        assert_eq!(a.text(), "read the ");
        let mut b = e.clone();
        b.kill_to_start();
        assert_eq!(b.text(), "news");
        assert_eq!(b.cursor(), 0);
    }

    #[test]
    fn nothing_moves_past_either_end() {
        let mut e = typed("hi");
        for _ in 0..10 {
            e.left();
        }
        assert_eq!(e.cursor(), 0);
        e.backspace();
        assert_eq!(e.text(), "hi", "there is nothing before the start to delete");
        for _ in 0..10 {
            e.right();
        }
        assert_eq!(e.cursor(), 2);
        e.delete();
        assert_eq!(e.text(), "hi");
    }

    #[test]
    fn a_line_is_never_split_down_the_middle_of_a_character() {
        // Characters, not bytes: each of these is several bytes long.
        let mut e = typed("héllo 🌙");
        e.left();
        e.backspace();
        assert_eq!(e.text(), "héllo🌙");
        e.home();
        e.right();
        e.delete();
        assert_eq!(e.text(), "hllo🌙");
    }

    #[test]
    fn what_was_said_before_can_be_reached_back_for() {
        let mut e = typed("first");
        e.take();
        e.set("second");
        e.take();

        for c in "half a th".chars() {
            e.insert(c);
        }
        e.earlier();
        assert_eq!(e.text(), "second");
        e.earlier();
        assert_eq!(e.text(), "first");
        e.earlier();
        assert_eq!(e.text(), "first", "there is nothing older");

        e.later();
        assert_eq!(e.text(), "second");
        e.later();
        assert_eq!(e.text(), "half a th", "and past the newest is what was being written");
        assert_eq!(e.cursor(), 9);
    }

    #[test]
    fn the_same_line_twice_is_remembered_once() {
        let mut e = typed("news");
        e.take();
        e.set("news");
        e.take();
        e.set("");
        e.earlier();
        assert_eq!(e.text(), "news");
        e.earlier();
        assert_eq!(e.text(), "news", "there is only one of them");
    }

    #[test]
    fn what_was_said_before_this_window_was_opened_is_there_too() {
        // Opening a window is not the start of the conversation, so it is not the start of the
        // history either. Everything a person said is loaded with it.
        let mut e = Editor::default();
        e.remember("read the news");
        e.remember("what did you learn today?");

        e.earlier();
        assert_eq!(e.text(), "what did you learn today?");
        e.earlier();
        assert_eq!(e.text(), "read the news");
    }

    #[test]
    fn reading_further_back_lengthens_the_history_without_losing_your_place() {
        let mut e = Editor::default();
        e.remember("the newest");
        e.earlier();
        assert_eq!(e.text(), "the newest");

        // A page from earlier in the conversation arrives while you are looking at it.
        e.remember_older(vec!["much older".into(), "older".into()]);
        assert_eq!(e.text(), "the newest", "what is on screen does not change under you");
        e.earlier();
        assert_eq!(e.text(), "older");
        e.earlier();
        assert_eq!(e.text(), "much older");
    }

    #[test]
    fn a_page_that_ends_where_the_history_begins_does_not_repeat_the_join() {
        let mut e = Editor::default();
        e.remember("one");
        e.remember_older(vec!["zero".into(), "one".into()]);
        e.earlier();
        assert_eq!(e.text(), "one");
        e.earlier();
        assert_eq!(e.text(), "zero", "the shared line is not there twice");
    }

    #[test]
    fn a_line_too_long_for_the_box_is_broken_at_a_space() {
        // The space a row ends on stays on that row. It is invisible, and keeping it means a
        // cursor sitting on it has somewhere to sit.
        let w = wrap("read the news and tell me", 10, 0);
        assert_eq!(w.rows, vec!["read the ", "news and ", "tell me"]);
    }

    #[test]
    fn the_cursor_is_where_the_next_character_will_appear() {
        // Typing at the end of a wrapped line: the cursor must be on the last row, just after
        // the last character, or what you type appears somewhere you were not looking.
        let text = "read the news and tell me";
        let w = wrap(text, 10, text.chars().count());
        assert_eq!(w.row, 2);
        assert_eq!(w.col, 7, "after `tell me`");

        // And in the middle, on the row that character is drawn on.
        let w = wrap(text, 10, 9);
        assert_eq!((w.row, w.col), (1, 0), "the `n` of `news` starts the second row");
    }

    #[test]
    fn a_word_longer_than_the_box_is_split_rather_than_lost() {
        let w = wrap("supercalifragilistic", 8, 20);
        assert_eq!(w.rows, vec!["supercal", "ifragili", "stic"]);
        assert_eq!((w.row, w.col), (2, 4));
    }

    #[test]
    fn an_empty_line_is_one_empty_row_with_the_cursor_at_its_start() {
        let w = wrap("", 20, 0);
        assert_eq!(w.rows, vec![""]);
        assert_eq!((w.row, w.col), (0, 0));
    }

    #[test]
    fn what_is_drawn_and_where_the_cursor_is_come_from_the_same_rule() {
        // Whatever the text, the cursor must land inside the rows that were drawn.
        for text in ["a", "hello world", "read the news and tell me what you learned today",
                     "one  two   three", "   leading", "trailing   "] {
            for width in [4, 7, 12, 40] {
                let n = text.chars().count();
                for cursor in 0..=n {
                    let w = wrap(text, width, cursor);
                    assert!(w.row < w.rows.len(), "{text:?} {width} {cursor}: {w:?}");
                    assert!(w.col <= width, "{text:?} {width} {cursor}: past the edge: {w:?}");
                    let joined: String = w.rows.join("");
                    assert!(joined.chars().count() <= n, "{text:?}: text appeared from nowhere");
                }
            }
        }
    }

    #[test]
    fn nothing_empty_is_remembered() {
        let mut e = Editor::default();
        e.set("   ");
        e.take();
        e.earlier();
        assert_eq!(e.text(), "", "an empty line is not something to reach back for");
    }
}
