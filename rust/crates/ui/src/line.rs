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
    fn nothing_empty_is_remembered() {
        let mut e = Editor::default();
        e.set("   ");
        e.take();
        e.earlier();
        assert_eq!(e.text(), "", "an empty line is not something to reach back for");
    }
}
