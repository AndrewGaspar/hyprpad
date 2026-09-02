//! The personal word cache — the words *this user* commits, with AOSP
//! LatinIME's forgetting curve and the privacy gates the research doc lists.
//!
//! `docs/research/osk-prediction.md` §5.1/§5.3. Phase 1 learns **unigrams only**
//! (a learned word can be completed and can appear as a candidate; learned
//! *bigrams* are phase 2), and every rule that keeps a password out of the file
//! is enforced here or by the daemon:
//!
//! * **Two sightings before it shows.** AOSP's `MIN_VISIBLE_LEVEL = 2` over
//!   `MAX_LEVEL = 15`, one level per sighting.
//! * **15-day decay.** AOSP's `DURATION_TO_LOWER_THE_LEVEL`: a level is lost per
//!   15 idle days, and an entry that decays to 0 is dropped on the next save.
//! * **Never learn a token with a digit or a symbol, or one longer than
//!   [`MAX_WORD_LEN`]** — SwiftKey's "long numbers" rule generalised (§5.3
//!   rule 4). This alone keeps API keys, PINs and card numbers out.
//! * **Never learn at all while the daemon says not to** — the `learn off`
//!   control command, which the daemon sends for password managers, polkit
//!   agents and terminals (§5.3 rule 2). That gate lives in
//!   [`super::Predictor`], because it must also silence learning that would
//!   otherwise come from an accepted candidate.
//!
//! The store is `$XDG_DATA_HOME/hyprpad-osk/learned.json`, mode 0600, written
//! whole on save. It is small (a cap of [`MAX_ENTRIES`] words) and readable, so
//! a user can see exactly what the keyboard remembers — and delete it, which is
//! what the `forget` command does.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The top of the forgetting curve (AOSP `MAX_LEVEL`).
pub const MAX_LEVEL: u8 = 15;
/// A word is only offered as a candidate at or above this level — i.e. after a
/// second sighting (AOSP `MIN_VISIBLE_LEVEL`).
pub const MIN_VISIBLE_LEVEL: u8 = 2;
/// Idle time that costs one level (AOSP `DURATION_TO_LOWER_THE_LEVEL`).
pub const DECAY_SECS: u64 = 15 * 24 * 60 * 60;
/// Longest token that may ever be learned (§5.3 rule 4).
pub const MAX_WORD_LEN: usize = 24;
/// Cap on the store; the lowest-level entries are evicted first.
pub const MAX_ENTRIES: usize = 20_000;

/// One remembered word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Learned {
    /// Level at [`Learned::seen`]; the *current* level is this decayed by age.
    pub level: u8,
    /// Unix seconds of the last sighting.
    pub seen: u64,
}

impl Learned {
    /// The level now, after the forgetting curve has been applied.
    pub fn level_at(&self, now: u64) -> u8 {
        let idle = now.saturating_sub(self.seen);
        let lost = (idle / DECAY_SECS).min(u8::MAX as u64) as u8;
        self.level.saturating_sub(lost)
    }
}

/// Whether a token may ever be learned, on its shape alone.
///
/// Rejects: anything with a digit, anything with a character that is neither a
/// letter nor an apostrophe/hyphen, anything longer than [`MAX_WORD_LEN`], and
/// single characters (nothing to learn). Pure, so the rule is testable without
/// a store.
pub fn learnable(word: &str) -> bool {
    let n = word.chars().count();
    if !(2..=MAX_WORD_LEN).contains(&n) {
        return false;
    }
    word.chars().all(|c| c.is_alphabetic() || c == '\'' || c == '-')
}

/// The learned-word store.
#[derive(Debug, Default)]
pub struct Personal {
    path: Option<PathBuf>,
    words: HashMap<String, Learned>,
    dirty: bool,
}

impl Personal {
    /// An in-memory store with nowhere to save (tests, and the case where no
    /// data directory could be resolved).
    pub fn in_memory() -> Personal {
        Personal { path: None, words: HashMap::new(), dirty: false }
    }

    /// The default store path: `$XDG_DATA_HOME/hyprpad-osk/learned.json`,
    /// falling back to `$HOME/.local/share/...`.
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
        Some(base.join("hyprpad-osk").join("learned.json"))
    }

    /// Load the store at `path`. A missing file is an empty store; an unreadable
    /// or malformed one is logged and treated as empty (a corrupt cache must
    /// never stop the keyboard from opening).
    pub fn load(path: PathBuf) -> Personal {
        let mut me = Personal { path: Some(path.clone()), words: HashMap::new(), dirty: false };
        match std::fs::read_to_string(&path) {
            Ok(text) => match parse(&text) {
                Ok(words) => me.words = words,
                Err(e) => eprintln!("hyprpad-osk: {}: {e} (starting empty)", path.display()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("hyprpad-osk: {}: {e} (starting empty)", path.display()),
        }
        me
    }

    /// Number of remembered words (of any level).
    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Whether anything has changed since the last save.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Record one sighting of a completed word. Returns whether it was stored —
    /// `false` when [`learnable`] rejects the token's shape.
    ///
    /// The level rises by one per sighting from its *decayed* value, so a word
    /// seen twice years apart is still only at level 1 and stays invisible.
    pub fn note(&mut self, word: &str, now: u64) -> bool {
        if !learnable(word) {
            return false;
        }
        let e = self.words.entry(word.to_string()).or_insert(Learned { level: 0, seen: now });
        let level = e.level_at(now);
        e.level = (level + 1).min(MAX_LEVEL);
        e.seen = now;
        self.dirty = true;
        if self.words.len() > MAX_ENTRIES {
            self.evict(now);
        }
        true
    }

    /// The current level of a word (0 when unknown or fully decayed).
    pub fn level(&self, word: &str, now: u64) -> u8 {
        self.words.get(word).map_or(0, |e| e.level_at(now))
    }

    /// Whether a word has been seen often enough recently enough to be offered
    /// as a candidate ([`MIN_VISIBLE_LEVEL`]).
    pub fn is_visible(&self, word: &str, now: u64) -> bool {
        self.level(word, now) >= MIN_VISIBLE_LEVEL
    }

    /// Every visible word starting with `prefix` (case-sensitively), with its
    /// level. Used to add learned words to the completion list.
    pub fn visible_with_prefix(&self, prefix: &str, now: u64) -> Vec<(&str, u8)> {
        let mut out: Vec<(&str, u8)> = self
            .words
            .iter()
            .filter(|(w, _)| w.starts_with(prefix))
            .map(|(w, e)| (w.as_str(), e.level_at(now)))
            .filter(|(_, lvl)| *lvl >= MIN_VISIBLE_LEVEL)
            .collect();
        // Deterministic order: level desc, then the word, so a tie never depends
        // on the hash map's iteration order.
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        out
    }

    /// Forget one word.
    pub fn forget(&mut self, word: &str) -> bool {
        let had = self.words.remove(word).is_some();
        self.dirty |= had;
        had
    }

    /// Forget everything, and delete the file on the next [`Self::save`].
    pub fn forget_all(&mut self) {
        if !self.words.is_empty() {
            self.dirty = true;
        }
        self.words.clear();
    }

    /// Drop fully-decayed entries; then, if still over [`MAX_ENTRIES`], the
    /// lowest-level ones.
    fn evict(&mut self, now: u64) {
        self.words.retain(|_, e| e.level_at(now) > 0);
        if self.words.len() <= MAX_ENTRIES {
            return;
        }
        let mut by_level: Vec<(String, u8)> =
            self.words.iter().map(|(w, e)| (w.clone(), e.level_at(now))).collect();
        by_level.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        for (w, _) in by_level.into_iter().take(self.words.len() - MAX_ENTRIES) {
            self.words.remove(&w);
        }
    }

    /// Write the store out (0600), dropping fully-decayed entries first. A
    /// store that has become empty deletes the file rather than leaving an empty
    /// one behind, so `forget` really does remove what the keyboard knew.
    ///
    /// A no-op when nothing changed or when there is nowhere to write.
    pub fn save(&mut self, now: u64) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        let Some(path) = self.path.clone() else {
            self.dirty = false;
            return Ok(());
        };
        self.words.retain(|_, e| e.level_at(now) > 0);
        if self.words.is_empty() {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("remove {}: {e}", path.display())),
            }
            self.dirty = false;
            return Ok(());
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
        }
        let text = self.to_json();
        // Write to a sibling temp file and rename, so an interrupted save never
        // leaves a half-written store.
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)
                .map_err(|e| format!("create {}: {e}", tmp.display()))?;
            set_owner_only(&f);
            f.write_all(text.as_bytes()).map_err(|e| format!("write {}: {e}", tmp.display()))?;
            f.flush().map_err(|e| format!("flush {}: {e}", tmp.display()))?;
        }
        std::fs::rename(&tmp, &path).map_err(|e| format!("rename {}: {e}", path.display()))?;
        self.dirty = false;
        Ok(())
    }

    /// The store as JSON — one object per word, sorted, so the file is stable
    /// across saves and diffs cleanly.
    pub fn to_json(&self) -> String {
        let mut items: Vec<(&String, &Learned)> = self.words.iter().collect();
        items.sort_by(|a, b| a.0.cmp(b.0));
        let mut s = String::from("{\n  \"version\": 1,\n  \"words\": [\n");
        for (i, (w, e)) in items.iter().enumerate() {
            s.push_str("    {\"w\": ");
            push_json_string(&mut s, w);
            s.push_str(&format!(", \"level\": {}, \"seen\": {}}}", e.level, e.seen));
            if i + 1 < items.len() {
                s.push(',');
            }
            s.push('\n');
        }
        s.push_str("  ]\n}\n");
        s
    }

    /// Replace the contents from a JSON document (tests, and [`Self::load`]).
    pub fn from_json(text: &str) -> Result<Personal, String> {
        Ok(Personal { path: None, words: parse(text)?, dirty: false })
    }
}

#[cfg(unix)]
fn set_owner_only(f: &std::fs::File) {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
}

fn push_json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

// ---------------------------------------------------------------------------
// A very small JSON reader
// ---------------------------------------------------------------------------
//
// The store is our own document, but it is a file on disk a user may edit, so it
// is parsed properly rather than scanned for substrings. This reads the subset
// JSON needs for that document — objects, arrays, strings, numbers, and the
// three literals — and nothing more (no floats with exponents, no deep nesting
// limit beyond the recursion the document itself needs).

enum Json {
    Str(String),
    Num(f64),
    Bool(#[allow(dead_code)] bool),
    Null,
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Num(n) if *n >= 0.0 => Some(*n as u64),
            _ => None,
        }
    }
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
    depth: u32,
}

impl<'a> Reader<'a> {
    fn ws(&mut self) {
        while matches!(self.b.get(self.at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }
    fn eat(&mut self, c: u8) -> Result<(), String> {
        self.ws();
        if self.b.get(self.at) == Some(&c) {
            self.at += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at byte {}", c as char, self.at))
        }
    }
    fn value(&mut self) -> Result<Json, String> {
        self.depth += 1;
        if self.depth > 32 {
            return Err("json nested too deeply".to_string());
        }
        self.ws();
        let out = match self.b.get(self.at) {
            Some(b'"') => Json::Str(self.string()?),
            Some(b'{') => {
                self.at += 1;
                let mut fields = Vec::new();
                self.ws();
                if self.b.get(self.at) == Some(&b'}') {
                    self.at += 1;
                } else {
                    loop {
                        self.ws();
                        let k = self.string()?;
                        self.eat(b':')?;
                        fields.push((k, self.value()?));
                        self.ws();
                        match self.b.get(self.at) {
                            Some(b',') => self.at += 1,
                            Some(b'}') => {
                                self.at += 1;
                                break;
                            }
                            _ => return Err(format!("bad object at byte {}", self.at)),
                        }
                    }
                }
                Json::Obj(fields)
            }
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                self.ws();
                if self.b.get(self.at) == Some(&b']') {
                    self.at += 1;
                } else {
                    loop {
                        items.push(self.value()?);
                        self.ws();
                        match self.b.get(self.at) {
                            Some(b',') => self.at += 1,
                            Some(b']') => {
                                self.at += 1;
                                break;
                            }
                            _ => return Err(format!("bad array at byte {}", self.at)),
                        }
                    }
                }
                Json::Arr(items)
            }
            Some(b't') if self.b[self.at..].starts_with(b"true") => {
                self.at += 4;
                Json::Bool(true)
            }
            Some(b'f') if self.b[self.at..].starts_with(b"false") => {
                self.at += 5;
                Json::Bool(false)
            }
            Some(b'n') if self.b[self.at..].starts_with(b"null") => {
                self.at += 4;
                Json::Null
            }
            Some(_) => {
                let start = self.at;
                while matches!(self.b.get(self.at),
                    Some(c) if c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.' | b'e' | b'E'))
                {
                    self.at += 1;
                }
                let text = std::str::from_utf8(&self.b[start..self.at])
                    .map_err(|_| "bad number".to_string())?;
                Json::Num(text.parse().map_err(|_| format!("bad number {text:?}"))?)
            }
            None => return Err("unexpected end of json".to_string()),
        };
        self.depth -= 1;
        Ok(out)
    }
    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            match self.b.get(self.at) {
                None => return Err("unterminated string".to_string()),
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    let c = *self.b.get(self.at).ok_or("unterminated escape")?;
                    self.at += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'u' => {
                            let hex = self
                                .b
                                .get(self.at..self.at + 4)
                                .and_then(|h| std::str::from_utf8(h).ok())
                                .ok_or("bad \\u escape")?;
                            let cp =
                                u32::from_str_radix(hex, 16).map_err(|_| "bad \\u escape")?;
                            self.at += 4;
                            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                        }
                        other => return Err(format!("bad escape '\\{}'", other as char)),
                    }
                }
                Some(_) => {
                    // Copy one whole UTF-8 character.
                    let rest = std::str::from_utf8(&self.b[self.at..])
                        .map_err(|_| "store is not valid UTF-8".to_string())?;
                    let c = rest.chars().next().ok_or("unterminated string")?;
                    out.push(c);
                    self.at += c.len_utf8();
                }
            }
        }
    }
}

fn parse(text: &str) -> Result<HashMap<String, Learned>, String> {
    let mut r = Reader { b: text.as_bytes(), at: 0, depth: 0 };
    let doc = r.value()?;
    let version = doc.get("version").and_then(Json::as_u64).unwrap_or(1);
    if version != 1 {
        return Err(format!("learned-word store version {version} is not readable by this build"));
    }
    let Some(Json::Arr(items)) = doc.get("words") else {
        return Err("learned-word store has no \"words\" array".to_string());
    };
    let mut out = HashMap::new();
    for item in items {
        let (Some(w), Some(level), Some(seen)) = (
            item.get("w").and_then(Json::as_str),
            item.get("level").and_then(Json::as_u64),
            item.get("seen").and_then(Json::as_u64),
        ) else {
            continue; // skip a row we cannot read rather than losing the file
        };
        if !learnable(w) {
            continue; // a hand-edited store cannot smuggle past the shape rule
        }
        out.insert(w.to_string(), Learned { level: level.min(MAX_LEVEL as u64) as u8, seen });
    }
    Ok(out)
}

/// Now, in unix seconds. Never panics: a clock before the epoch reads as 0.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The store path for a data directory (used by [`Personal::default_path`] and
/// by tests that point it somewhere temporary).
pub fn store_path_in(dir: &Path) -> PathBuf {
    dir.join("hyprpad-osk").join("learned.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_700_000_000;
    const DAY: u64 = 24 * 60 * 60;

    #[test]
    fn a_word_is_invisible_until_the_second_sighting() {
        let mut p = Personal::in_memory();
        assert!(p.note("hyprpad", T0));
        assert_eq!(p.level("hyprpad", T0), 1);
        assert!(!p.is_visible("hyprpad", T0), "one sighting is not enough");
        assert!(p.note("hyprpad", T0 + 60));
        assert_eq!(p.level("hyprpad", T0 + 60), 2);
        assert!(p.is_visible("hyprpad", T0 + 60));
    }

    #[test]
    fn the_forgetting_curve_lowers_a_level_every_fifteen_days() {
        let mut p = Personal::in_memory();
        for _ in 0..4 {
            p.note("gamescope", T0);
        }
        assert_eq!(p.level("gamescope", T0), 4);
        assert_eq!(p.level("gamescope", T0 + 14 * DAY), 4);
        assert_eq!(p.level("gamescope", T0 + 15 * DAY), 3);
        assert_eq!(p.level("gamescope", T0 + 45 * DAY), 1);
        assert!(!p.is_visible("gamescope", T0 + 45 * DAY));
        assert_eq!(p.level("gamescope", T0 + 400 * DAY), 0, "decays away entirely");
        // A sighting after the decay resumes from the decayed level, not the old one.
        p.note("gamescope", T0 + 45 * DAY);
        assert_eq!(p.level("gamescope", T0 + 45 * DAY), 2);
    }

    #[test]
    fn tokens_with_digits_symbols_or_length_are_never_learned() {
        let mut p = Personal::in_memory();
        for bad in [
            "hunter2",
            "4111111111111111",
            "sk-abc123",
            "p@ssw0rd",
            "a",
            "",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(!p.note(bad, T0), "{bad:?} must not be learned");
        }
        assert!(p.is_empty());
        // The shapes that ARE words.
        for good in ["hyprpad", "don't", "well-known", "Wayland"] {
            assert!(p.note(good, T0), "{good:?} should be learnable");
        }
        assert_eq!(p.len(), 4);
    }

    #[test]
    fn visible_prefix_lookup_is_deterministic_and_level_gated() {
        let mut p = Personal::in_memory();
        p.note("hyprpad", T0);
        p.note("hyprpad", T0);
        p.note("hyprland", T0);
        p.note("hyprland", T0);
        p.note("hyprland", T0);
        p.note("hypothesis", T0); // one sighting only
        let hits = p.visible_with_prefix("hypr", T0);
        assert_eq!(hits, vec![("hyprland", 3), ("hyprpad", 2)]);
        assert!(p.visible_with_prefix("hypo", T0).is_empty());
    }

    #[test]
    fn forget_clears_one_word_and_forget_all_clears_everything() {
        let mut p = Personal::in_memory();
        p.note("alpha", T0);
        p.note("beta", T0);
        assert!(p.forget("alpha"));
        assert!(!p.forget("alpha"));
        assert_eq!(p.len(), 1);
        p.forget_all();
        assert!(p.is_empty());
    }

    #[test]
    fn json_round_trips_and_a_hand_edited_store_cannot_smuggle_a_secret_in() {
        let mut p = Personal::in_memory();
        p.note("hyprpad", T0);
        p.note("hyprpad", T0 + 10);
        p.note("wayland", T0);
        let text = p.to_json();
        let back = Personal::from_json(&text).unwrap();
        assert_eq!(back.level("hyprpad", T0 + 10), 2);
        assert_eq!(back.level("wayland", T0), 1);

        // A row whose token would never be learned is dropped on load.
        let doctored = r#"{"version":1,"words":[
            {"w":"hunter2","level":9,"seen":1700000000},
            {"w":"ok","level":3,"seen":1700000000}
        ]}"#;
        let p = Personal::from_json(doctored).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p.level("ok", T0), 3);
    }

    #[test]
    fn a_malformed_store_is_an_error_not_a_panic() {
        assert!(Personal::from_json("").is_err());
        assert!(Personal::from_json("{").is_err());
        assert!(Personal::from_json("{\"version\":9,\"words\":[]}").is_err());
        assert!(Personal::from_json("{\"version\":1}").is_err());
        // Unreadable rows are skipped, the rest survives.
        let p =
            Personal::from_json("{\"version\":1,\"words\":[{\"w\":\"ok\"},{\"w\":\"fine\",\"level\":2,\"seen\":1}]}")
                .unwrap();
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn saving_writes_an_owner_only_file_and_an_emptied_store_removes_it() {
        let dir = std::env::temp_dir().join(format!("hyprpad-osk-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = store_path_in(&dir);
        let mut p = Personal::load(path.clone());
        assert!(p.is_empty());
        p.note("hyprpad", T0);
        p.save(T0).unwrap();
        assert!(path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let reloaded = Personal::load(path.clone());
        assert_eq!(reloaded.level("hyprpad", T0), 1);

        let mut p = Personal::load(path.clone());
        p.forget_all();
        p.save(T0).unwrap();
        assert!(!path.exists(), "forget removes the file, it does not leave an empty one");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fully_decayed_entry_is_dropped_on_save() {
        let dir = std::env::temp_dir().join(format!("hyprpad-osk-decay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = store_path_in(&dir);
        let mut p = Personal::load(path.clone());
        p.note("ephemeral", T0);
        p.note("kept", T0);
        p.note("kept", T0);
        p.note("kept", T0);
        // 20 days later the one-sighting word is gone; the three-sighting one is not.
        p.save(T0 + 20 * DAY).unwrap();
        let back = Personal::load(path.clone());
        assert_eq!(back.len(), 1);
        assert_eq!(back.level("kept", T0 + 20 * DAY), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
