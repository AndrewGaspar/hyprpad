//! The composition buffer — what the keyboard itself has typed since it was
//! shown.
//!
//! Phase 1 has **no view of the focused application's text buffer**
//! (`docs/research/osk-prediction.md` §1: "There is no text buffer"), so the
//! only context available is the keystrokes the OSK emitted. That is enough for
//! bigram prediction and for completion, and it is the exact set of characters
//! the OSK may safely delete again when a candidate is accepted (§8.6 — a
//! correction only ever applies to text the OSK typed).
//!
//! The model is deliberately literal: one `String` of everything typed since the
//! last reset, with Backspace popping one character. Every question the ranker
//! asks — "what is the partial word?", "what was the word before it?", "did the
//! last keystroke close a word?" — is a scan of that string's tail, so the
//! buffer can never disagree with itself.
//!
//! It is reset on `show`, on `hide`, and on the daemon's `context reset` (sent
//! on a focus change, when whatever we knew about the field stopped being true).

/// How much typed text the buffer keeps. Only the tail matters — the partial
/// word and the word before it — so anything older is dropped rather than grown
/// without bound. Comfortably longer than the longest sentence anyone types on a
/// controller before a separator.
const MAX_TEXT: usize = 512;

/// Characters that belong *inside* a word. Everything else closes one.
///
/// The apostrophe is in (so "don't" is one word, as every English word list
/// spells it); digits are not, which keeps "v2" and "3pm" out of the word
/// stream entirely — the same instinct as the learning filter in
/// [`super::personal`].
fn is_word_char(c: char) -> bool {
    c.is_alphabetic() || c == '\''
}

/// What the OSK has typed since the last reset.
#[derive(Clone, Debug, Default)]
pub struct Context {
    text: String,
}

impl Context {
    pub fn new() -> Context {
        Context { text: String::new() }
    }

    /// Forget everything (show / hide / `context reset`).
    pub fn reset(&mut self) {
        self.text.clear();
    }

    /// Record one character the keyboard typed.
    pub fn push_char(&mut self, c: char) {
        self.text.push(c);
        // Trim from the front, on a char boundary. Only ancient words can be
        // affected — never the partial word or the one before it.
        while self.text.len() > MAX_TEXT {
            let drop = self.text.chars().next().map_or(0, |c| c.len_utf8());
            self.text.drain(..drop);
        }
    }

    /// Record a typed string (the `type` command, an accepted candidate).
    pub fn push_str(&mut self, s: &str) {
        for c in s.chars() {
            self.push_char(c);
        }
    }

    /// Record a Backspace: one character disappears from the field, so one
    /// disappears from here.
    pub fn backspace(&mut self) {
        self.text.pop();
    }

    /// The word being typed right now — the trailing run of word characters,
    /// empty right after a separator.
    pub fn partial(&self) -> &str {
        let start = self
            .text
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_word_char(*c))
            .last()
            .map_or(self.text.len(), |(i, _)| i);
        &self.text[start..]
    }

    /// The completed word immediately before the partial one — the bigram
    /// context. `None` at the start of the buffer.
    pub fn prev_word(&self) -> Option<&str> {
        let head = &self.text[..self.text.len() - self.partial().len()];
        let end = head.trim_end_matches(|c| !is_word_char(c)).len();
        if end == 0 {
            return None;
        }
        let start = head[..end]
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_word_char(*c))
            .last()
            .map_or(0, |(i, _)| i);
        Some(&head[start..end])
    }

    /// The word a just-typed separator closed, if the last keystroke closed one.
    ///
    /// This is the learning trigger (§5.3 rule 6 — learn only on a *completed*
    /// word): `"hello "` answers `Some("hello")`, `"hello"` and `"hello  "`
    /// answer `None`, so a word is offered to the learner exactly once.
    pub fn just_closed_word(&self) -> Option<&str> {
        let last = self.text.chars().next_back()?;
        if is_word_char(last) {
            return None; // still typing it
        }
        let head = &self.text[..self.text.len() - last.len_utf8()];
        let mut it = head.char_indices().rev();
        let (end, c) = it.next()?;
        if !is_word_char(c) {
            return None; // the separator run is longer than one; already closed
        }
        let end = end + c.len_utf8();
        let start = head[..end]
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_word_char(*c))
            .last()
            .map_or(0, |(i, _)| i);
        Some(&head[start..end])
    }

    /// Everything typed since the reset (diagnostics and tests).
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether anything has been typed since the reset.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(s: &str) -> Context {
        let mut c = Context::new();
        c.push_str(s);
        c
    }

    #[test]
    fn typing_builds_a_partial_word_and_a_previous_word() {
        let c = ctx("hello wor");
        assert_eq!(c.partial(), "wor");
        assert_eq!(c.prev_word(), Some("hello"));

        let c = ctx("hello ");
        assert_eq!(c.partial(), "");
        assert_eq!(c.prev_word(), Some("hello"));

        let c = ctx("hello");
        assert_eq!(c.partial(), "hello");
        assert_eq!(c.prev_word(), None);

        let c = Context::new();
        assert_eq!(c.partial(), "");
        assert_eq!(c.prev_word(), None);
    }

    #[test]
    fn punctuation_and_digits_close_a_word() {
        let c = ctx("one, two");
        assert_eq!(c.partial(), "two");
        assert_eq!(c.prev_word(), Some("one"));
        // Digits are separators, so a version number never becomes a word.
        let c = ctx("hyprpad v2 sh");
        assert_eq!(c.partial(), "sh");
        assert_eq!(c.prev_word(), Some("v"));
        // The apostrophe stays inside a word.
        let c = ctx("don't ");
        assert_eq!(c.prev_word(), Some("don't"));
    }

    #[test]
    fn backspace_edits_the_partial_word_then_reopens_the_previous_one() {
        let mut c = ctx("hello wor");
        c.backspace();
        assert_eq!(c.partial(), "wo");
        c.backspace();
        c.backspace();
        assert_eq!(c.partial(), "");
        assert_eq!(c.prev_word(), Some("hello"));
        // One more removes the space, so the earlier word is being edited again.
        c.backspace();
        assert_eq!(c.partial(), "hello");
        assert_eq!(c.prev_word(), None);
        c.backspace();
        assert_eq!(c.partial(), "hell");
    }

    #[test]
    fn a_separator_closes_exactly_one_word_once() {
        let mut c = ctx("hello");
        assert_eq!(c.just_closed_word(), None);
        c.push_char(' ');
        assert_eq!(c.just_closed_word(), Some("hello"));
        c.push_char(' ');
        assert_eq!(c.just_closed_word(), None, "the second space closes nothing");
        c.push_str("there");
        assert_eq!(c.just_closed_word(), None);
        c.push_char('\n');
        assert_eq!(c.just_closed_word(), Some("there"));
        // A separator at the very start closes nothing.
        let mut c = Context::new();
        c.push_char(' ');
        assert_eq!(c.just_closed_word(), None);
    }

    #[test]
    fn reset_forgets_everything() {
        let mut c = ctx("hello world");
        c.reset();
        assert!(c.is_empty());
        assert_eq!(c.partial(), "");
        assert_eq!(c.prev_word(), None);
    }

    #[test]
    fn the_buffer_is_bounded_but_keeps_the_tail() {
        let mut c = Context::new();
        for _ in 0..200 {
            c.push_str("word ");
        }
        c.push_str("tai");
        assert!(c.text().len() <= MAX_TEXT + 8);
        assert_eq!(c.partial(), "tai");
        assert_eq!(c.prev_word(), Some("word"));
    }
}
