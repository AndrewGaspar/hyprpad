//! The builder's intermediate word-list format — the one text format that both
//! the shipped model and the committed test fixture are built from.
//!
//! The data pipeline (`osk/tools/build-model/build-model.py`) downloads and
//! merges the sources, and emits this; `hyprpad-osk-build-model` reads it and
//! writes the binary model ([`super::model::build`]). Splitting the two means
//! the format-critical half is ordinary Rust with ordinary tests, and the
//! network-and-gigabytes half is a script you run once.
//!
//! ```text
//! # comments and blank lines are ignored
//! the 0.055           <- a word and its unigram probability
//!   quick 0.02        <- indented: a successor, and S(successor | the)
//!   world 0.05
//! hello 0.0009
//!   world 0.30
//! ```
//!
//! Probabilities are decimal fractions in `(0, 1]`; they are quantised to the
//! model's `u8` log scale ([`super::model::quantize`]) on the way in, which is
//! where all the precision loss happens — the text format itself is exact, so
//! two runs of the pipeline that produce the same text produce the same model
//! byte for byte.

use super::model::{quantize, WordEntry};

/// Parse a word list. Returns the entries in the order they appear; ordering
/// does not matter to [`super::model::build`], which sorts.
pub fn parse(text: &str) -> Result<Vec<WordEntry>, String> {
    let mut out: Vec<WordEntry> = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim_end();
        let body = line.trim_start();
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        let indented = line.len() != body.len();
        let mut it = body.split_whitespace();
        let word = it.next().unwrap_or("");
        let p: f64 = match it.next() {
            Some(tok) => tok
                .parse()
                .map_err(|_| format!("line {}: bad probability {tok:?}", n + 1))?,
            None => return Err(format!("line {}: {word:?} has no probability", n + 1)),
        };
        if let Some(extra) = it.next() {
            return Err(format!("line {}: unexpected {extra:?}", n + 1));
        }
        if !(p > 0.0 && p <= 1.0) {
            return Err(format!("line {}: probability {p} is not in (0, 1]", n + 1));
        }
        if indented {
            let owner = out
                .last_mut()
                .ok_or_else(|| format!("line {}: successor {word:?} before any word", n + 1))?;
            owner.successors.push((word.to_string(), quantize(p)));
        } else {
            out.push(WordEntry { word: word.to_string(), unigram: quantize(p), successors: Vec::new() });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_words_successors_and_comments() {
        let entries = parse(
            "# a comment\n\
             \n\
             the 0.05\n\
             \x20 quick 0.02\n\
             \x20 world 0.5\n\
             hello 0.001\n",
        )
        .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].word, "the");
        assert_eq!(entries[0].unigram, quantize(0.05));
        assert_eq!(
            entries[0].successors,
            vec![("quick".to_string(), quantize(0.02)), ("world".to_string(), quantize(0.5))]
        );
        assert_eq!(entries[1].word, "hello");
        assert!(entries[1].successors.is_empty());
    }

    #[test]
    fn malformed_lines_are_named_not_guessed_at() {
        assert!(parse("the\n").unwrap_err().contains("no probability"));
        assert!(parse("the abc\n").unwrap_err().contains("bad probability"));
        assert!(parse("the 0\n").unwrap_err().contains("not in (0, 1]"));
        assert!(parse("the 1.5\n").unwrap_err().contains("not in (0, 1]"));
        assert!(parse("  orphan 0.1\n").unwrap_err().contains("before any word"));
        assert!(parse("the 0.1 0.2\n").unwrap_err().contains("unexpected"));
    }
}
