//! The static prediction model: an on-disk, mmap-able unigram + bigram-successor
//! table, and the writer that builds one.
//!
//! This is `docs/research/osk-prediction.md` §8.3's `en.model`, with the shape
//! §4.3 specifies: an `fst::Map` lexicon (word → id), a flat `u8` unigram table
//! indexed by id, and per-word successor lists (delta-varint ids + a `u8`
//! score). It is mapped and used **in place** — no allocation per query, and the
//! page cache is shared between processes.
//!
//! # Why this shape
//!
//! `fst` (MIT/Unlicense, BurntSushi) gives an ordered map with prefix range
//! scans, which is exactly the completion query, at ~5 bytes/key. Ids are
//! assigned in the fst's own sorted order, so id ↔ rank and every side table is
//! a flat array indexed by id. Successor lists are capped (the builder's
//! `--successors`, 32 by default — §4.3) so a next-word query touches a bounded
//! number of entries.
//!
//! # Scoring scale
//!
//! Every stored score is one `u8` on AOSP LatinIME's log scale: **255 means
//! probability 1.0 and each step down is a factor of 1.15**
//! (`forgetting_curve_utils.cpp`'s ÷1.15 per step, reused here for
//! probabilities). So `p = 1.15^(q - 255)`, and the whole `u8` range reaches
//! down to ~1e-15 — far below any word we store.
//!
//! * **Unigram** `q`: the word's absolute probability on that scale.
//! * **Successor** `q`: the *conditional* `S(w | prev) = c(prev,w) / c(prev)` on
//!   the same scale, so unigram and bigram scores are directly comparable and
//!   stupid backoff (§2.2, Brants et al. 2007) is one branch:
//!   `S(w|prev) = c(prev,w)/c(prev)` if the bigram is stored, else
//!   `0.4 · P(w)`.
//!
//! # File layout
//!
//! ```text
//! 0..8    magic  b"HPOSKMD1"
//! 8..12   u32 version
//! 12..16  u32 vocab (V)
//! 16..20  u32 fst_off        20..24  u32 fst_len
//! 24..28  u32 uni_off        (V bytes, one per id)
//! 28..32  u32 succ_idx_off   ((V+1) * u32, byte offsets into the successor stream)
//! 32..36  u32 succ_off       36..40  u32 succ_len
//! 40..44  u32 attr_off       44..48  u32 attr_len   (UTF-8 attribution/licence line)
//! 48..64  reserved (zero)
//! ```
//!
//! All integers are little-endian and read with [`u32::from_le_bytes`] off byte
//! slices, so the mapping needs no particular alignment.
//!
//! Successor stream, per word, `succ_idx[id] .. succ_idx[id+1]`: entries sorted
//! by **successor id ascending** so the ids delta-encode to one or two varint
//! bytes; each entry is `varint(delta_id) u8(score)`. A query reads the whole
//! (≤ 32) list and ranks it, which is cheaper than storing absolute ids.

use std::path::Path;
use std::sync::Arc;

/// File magic. The trailing digit is the format generation; a reader refuses
/// anything else rather than guessing.
pub const MAGIC: &[u8; 8] = b"HPOSKMD1";
/// Format version inside the magic's generation.
pub const VERSION: u32 = 1;
/// Fixed header size in bytes.
pub const HEADER_LEN: usize = 64;

/// One step of the stored log scale: each `u8` step down is a factor of 1.15
/// (AOSP LatinIME's `forgetting_curve_utils.cpp` constant).
pub const SCORE_STEP: f32 = 1.15;

/// Stupid-backoff factor α for an unseen bigram (Brants et al. 2007 use 0.4).
pub const BACKOFF: f32 = 0.4;

/// Turn a stored `u8` score into a natural-log probability.
///
/// `255` is `ln(1) = 0`; each step down subtracts `ln(1.15)`.
#[inline]
pub fn score_logp(q: u8) -> f32 {
    (q as f32 - 255.0) * SCORE_STEP.ln()
}

/// Quantise a probability in `(0, 1]` to the stored `u8` scale, clamped to
/// `1..=255` (0 is reserved for "absent" and never stored).
pub fn quantize(p: f64) -> u8 {
    if p.is_nan() || p <= 0.0 {
        return 1;
    }
    let q = 255.0 + p.min(1.0).ln() / (SCORE_STEP as f64).ln();
    q.round().clamp(1.0, 255.0) as u8
}

// ---------------------------------------------------------------------------
// Building
// ---------------------------------------------------------------------------

/// One vocabulary entry handed to [`build`]: the word, its unigram score, and
/// its (already truncated and ranked) successor list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WordEntry {
    pub word: String,
    /// Unigram probability on the [`score_logp`] scale.
    pub unigram: u8,
    /// `(successor word, S(successor | word))`, scores on the same scale.
    /// Successors whose word is not in the vocabulary are dropped by [`build`].
    pub successors: Vec<(String, u8)>,
}

/// Serialise a model. `attribution` is the data-provenance line stored in the
/// file (sources and licences — see `osk/tools/build-model/DATA-LICENSES.md`).
///
/// Deterministic: entries are sorted by word, ids are assigned in that order,
/// and successor lists are sorted by id, so the same input always produces the
/// same bytes. The builder's fixture test pins that.
///
/// Duplicate words are an error rather than a silent last-wins, because a
/// duplicate means the pipeline that produced the vocabulary is wrong.
pub fn build(entries: &[WordEntry], attribution: &str) -> Result<Vec<u8>, String> {
    let mut sorted: Vec<&WordEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| a.word.as_bytes().cmp(b.word.as_bytes()));
    for pair in sorted.windows(2) {
        if pair[0].word == pair[1].word {
            return Err(format!("duplicate word in vocabulary: {:?}", pair[0].word));
        }
    }
    if sorted.len() > u32::MAX as usize {
        return Err("vocabulary too large".to_string());
    }

    // Word → id, in the fst's own (sorted) order, so id == rank.
    let mut ids: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
    for (i, e) in sorted.iter().enumerate() {
        ids.insert(e.word.as_str(), i as u32);
    }

    let mut fst_builder = fst::MapBuilder::memory();
    for (i, e) in sorted.iter().enumerate() {
        fst_builder
            .insert(e.word.as_bytes(), i as u64)
            .map_err(|e| format!("fst insert: {e}"))?;
    }
    let fst_bytes = fst_builder.into_inner().map_err(|e| format!("fst finish: {e}"))?;

    let v = sorted.len();
    let mut unigrams = Vec::with_capacity(v);
    let mut succ_idx: Vec<u32> = Vec::with_capacity(v + 1);
    let mut succ_data: Vec<u8> = Vec::new();
    for e in &sorted {
        unigrams.push(e.unigram);
        succ_idx.push(succ_data.len() as u32);
        // Resolve, drop out-of-vocabulary successors, dedupe, sort by id.
        let mut resolved: Vec<(u32, u8)> = e
            .successors
            .iter()
            .filter_map(|(w, q)| ids.get(w.as_str()).map(|&id| (id, (*q).max(1))))
            .collect();
        resolved.sort_unstable();
        resolved.dedup_by_key(|(id, _)| *id);
        let mut prev = 0u32;
        for (id, q) in resolved {
            write_varint(&mut succ_data, (id - prev) as u64);
            succ_data.push(q);
            prev = id;
        }
    }
    succ_idx.push(succ_data.len() as u32);

    // Assemble: header, then each section in header order.
    let attr = attribution.as_bytes();
    let fst_off = HEADER_LEN;
    let uni_off = fst_off + fst_bytes.len();
    let succ_idx_off = uni_off + unigrams.len();
    let succ_off = succ_idx_off + succ_idx.len() * 4;
    let attr_off = succ_off + succ_data.len();
    let total = attr_off + attr.len();

    let mut out = vec![0u8; HEADER_LEN];
    out.reserve(total);
    out[0..8].copy_from_slice(MAGIC);
    let put = |at: usize, val: u32, buf: &mut Vec<u8>| {
        buf[at..at + 4].copy_from_slice(&val.to_le_bytes());
    };
    put(8, VERSION, &mut out);
    put(12, v as u32, &mut out);
    put(16, fst_off as u32, &mut out);
    put(20, fst_bytes.len() as u32, &mut out);
    put(24, uni_off as u32, &mut out);
    put(28, succ_idx_off as u32, &mut out);
    put(32, succ_off as u32, &mut out);
    put(36, succ_data.len() as u32, &mut out);
    put(40, attr_off as u32, &mut out);
    put(44, attr.len() as u32, &mut out);

    out.extend_from_slice(&fst_bytes);
    out.extend_from_slice(&unigrams);
    for o in &succ_idx {
        out.extend_from_slice(&o.to_le_bytes());
    }
    out.extend_from_slice(&succ_data);
    out.extend_from_slice(attr);
    Ok(out)
}

fn write_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn read_varint(buf: &[u8], at: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let b = *buf.get(*at)?;
        *at += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// The model's bytes: either a memory map (the shipped model) or an owned
/// buffer (tests, and a model built in memory).
enum Backing {
    Mapped(memmap2::Mmap),
    Owned(Vec<u8>),
}

impl AsRef<[u8]> for Backing {
    fn as_ref(&self) -> &[u8] {
        match self {
            Backing::Mapped(m) => m,
            Backing::Owned(v) => v,
        }
    }
}

/// A shared handle to the model bytes, so the `fst::Map` can borrow its slice
/// out of the same mapping the side tables are read from.
#[derive(Clone)]
struct Blob(Arc<Backing>);

/// The `fst` sub-slice of [`Blob`], as the owning type `fst::Map` needs.
#[derive(Clone)]
struct FstSlice {
    blob: Blob,
    start: usize,
    end: usize,
}

impl AsRef<[u8]> for FstSlice {
    fn as_ref(&self) -> &[u8] {
        &self.blob.0.as_ref().as_ref()[self.start..self.end]
    }
}

/// A successor of some word: its id and the stored `S(successor | word)` score.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Successor {
    pub id: u32,
    pub score: u8,
}

/// The mapped static model.
pub struct Model {
    blob: Blob,
    lexicon: fst::Map<FstSlice>,
    vocab: usize,
    uni_off: usize,
    succ_idx_off: usize,
    succ_off: usize,
    attr: String,
    /// Reverse index (id → word), built by streaming the fst once at open.
    ///
    /// Storing the words again in the file would cost ~0.7 MB on a 60 k
    /// vocabulary for a table we can rebuild in a few milliseconds, so we pay
    /// the milliseconds. Words are concatenated into one buffer with `u32`
    /// offsets, which is ~0.7 MB of RAM rather than 60 k separate `String`s.
    word_bytes: Vec<u8>,
    word_offs: Vec<u32>,
}

impl Model {
    /// Map a model file. Fails on a missing file, a bad magic, or a truncated
    /// section — a partial model is never used half-way.
    pub fn open(path: &Path) -> Result<Model, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
        // SAFETY: the model file is a read-only artifact the OSK owns; a
        // concurrent truncation would be a use-after-unmap, which is the same
        // caveat every mmap-based reader carries. We never write through it.
        let mmap = unsafe { memmap2::Mmap::map(&file) }
            .map_err(|e| format!("mmap {}: {e}", path.display()))?;
        Model::from_backing(Backing::Mapped(mmap))
    }

    /// Read a model from bytes already in memory (tests, and the builder's
    /// round-trip check).
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Model, String> {
        Model::from_backing(Backing::Owned(bytes))
    }

    fn from_backing(backing: Backing) -> Result<Model, String> {
        let blob = Blob(Arc::new(backing));
        let bytes = blob.0.as_ref().as_ref();
        if bytes.len() < HEADER_LEN || &bytes[0..8] != MAGIC {
            return Err("not a hyprpad-osk model (bad magic)".to_string());
        }
        let u32_at = |at: usize| -> usize {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize
        };
        let version = u32_at(8) as u32;
        if version != VERSION {
            return Err(format!("model version {version}, this build reads {VERSION}"));
        }
        let vocab = u32_at(12);
        let fst_off = u32_at(16);
        let fst_len = u32_at(20);
        let uni_off = u32_at(24);
        let succ_idx_off = u32_at(28);
        let succ_off = u32_at(32);
        let succ_len = u32_at(36);
        let attr_off = u32_at(40);
        let attr_len = u32_at(44);

        let end = |off: usize, len: usize| off.checked_add(len).filter(|e| *e <= bytes.len());
        end(fst_off, fst_len).ok_or("truncated model: lexicon")?;
        end(uni_off, vocab).ok_or("truncated model: unigram table")?;
        end(succ_idx_off, (vocab + 1) * 4).ok_or("truncated model: successor index")?;
        end(succ_off, succ_len).ok_or("truncated model: successor stream")?;
        end(attr_off, attr_len).ok_or("truncated model: attribution")?;

        let attr = String::from_utf8_lossy(&bytes[attr_off..attr_off + attr_len]).into_owned();
        let slice = FstSlice { blob: blob.clone(), start: fst_off, end: fst_off + fst_len };
        let lexicon = fst::Map::new(slice).map_err(|e| format!("lexicon: {e}"))?;

        // Reverse index. `fst::Map` streams in sorted key order and our ids were
        // assigned in that same order, so the nth key has id n; we assert that
        // rather than trusting it, since a hand-built file could disagree.
        let mut word_bytes = Vec::new();
        let mut word_offs = Vec::with_capacity(vocab + 1);
        {
            use fst::Streamer as _;
            let mut stream = lexicon.stream();
            let mut n = 0u64;
            while let Some((key, id)) = stream.next() {
                if id != n {
                    return Err("lexicon ids are not in sorted order".to_string());
                }
                word_offs.push(word_bytes.len() as u32);
                word_bytes.extend_from_slice(key);
                n += 1;
            }
            if n as usize != vocab {
                return Err(format!("header says {vocab} words, lexicon holds {n}"));
            }
        }
        word_offs.push(word_bytes.len() as u32);

        Ok(Model {
            blob,
            lexicon,
            vocab,
            uni_off,
            succ_idx_off,
            succ_off,
            attr,
            word_bytes,
            word_offs,
        })
    }

    fn bytes(&self) -> &[u8] {
        self.blob.0.as_ref().as_ref()
    }

    /// Number of words in the lexicon.
    pub fn len(&self) -> usize {
        self.vocab
    }

    pub fn is_empty(&self) -> bool {
        self.vocab == 0
    }

    /// The data-provenance line baked into the file.
    pub fn attribution(&self) -> &str {
        &self.attr
    }

    /// The id of an exact word, or `None` when it is not in the lexicon.
    pub fn id(&self, word: &str) -> Option<u32> {
        self.lexicon.get(word.as_bytes()).map(|v| v as u32)
    }

    /// The word for an id.
    pub fn word(&self, id: u32) -> Option<&str> {
        let i = id as usize;
        let (a, b) = (*self.word_offs.get(i)? as usize, *self.word_offs.get(i + 1)? as usize);
        std::str::from_utf8(&self.word_bytes[a..b]).ok()
    }

    /// The stored unigram score for an id (`0` when out of range).
    pub fn unigram(&self, id: u32) -> u8 {
        self.bytes().get(self.uni_off + id as usize).copied().unwrap_or(0)
    }

    /// The successors stored for `id`, in ascending-id order (the storage
    /// order). Callers rank them; the list is capped by the builder.
    pub fn successors(&self, id: u32) -> Vec<Successor> {
        let i = id as usize;
        if i >= self.vocab {
            return Vec::new();
        }
        let bytes = self.bytes();
        let idx_at = |n: usize| -> usize {
            let at = self.succ_idx_off + n * 4;
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize
        };
        let (start, end) = (idx_at(i), idx_at(i + 1));
        let mut out = Vec::new();
        let mut at = self.succ_off + start;
        let stop = self.succ_off + end;
        let mut prev = 0u32;
        while at < stop {
            let Some(delta) = read_varint(bytes, &mut at) else { break };
            let Some(&score) = bytes.get(at) else { break };
            at += 1;
            prev = prev.saturating_add(delta as u32);
            out.push(Successor { id: prev, score });
        }
        out
    }

    /// The stored `S(next | prev)` score, or `None` when the bigram is absent.
    pub fn bigram(&self, prev: u32, next: u32) -> Option<u8> {
        self.successors(prev).into_iter().find(|s| s.id == next).map(|s| s.score)
    }

    /// Every word whose key starts with `prefix`, as `(id, unigram score)`,
    /// stopping after `limit` hits so a one-letter prefix cannot make a query
    /// unbounded.
    pub fn prefix_ids(&self, prefix: &str, limit: usize, out: &mut Vec<u32>) {
        use fst::{IntoStreamer, Streamer as _};
        if limit == 0 {
            return;
        }
        let mut stream = self.lexicon.range().ge(prefix.as_bytes()).into_stream();
        let mut n = 0;
        while let Some((key, id)) = stream.next() {
            if !key.starts_with(prefix.as_bytes()) {
                break;
            }
            out.push(id as u32);
            n += 1;
            if n >= limit {
                break;
            }
        }
    }
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Model").field("vocab", &self.vocab).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(w: &str, u: u8, s: &[(&str, u8)]) -> WordEntry {
        WordEntry {
            word: w.to_string(),
            unigram: u,
            successors: s.iter().map(|(a, b)| (a.to_string(), *b)).collect(),
        }
    }

    fn sample() -> Vec<WordEntry> {
        vec![
            entry("the", 230, &[("quick", 200), ("cat", 210)]),
            entry("quick", 190, &[("brown", 220)]),
            entry("brown", 185, &[("fox", 225)]),
            entry("fox", 180, &[]),
            entry("cat", 195, &[("sat", 215)]),
            entry("sat", 188, &[]),
            // A successor that is NOT in the vocabulary: must be dropped.
            entry("dog", 170, &[("wolfhound", 200)]),
        ]
    }

    #[test]
    fn round_trips_words_unigrams_and_successors() {
        let bytes = build(&sample(), "test data").unwrap();
        let m = Model::from_bytes(bytes).unwrap();
        assert_eq!(m.len(), 7);
        assert_eq!(m.attribution(), "test data");

        let the = m.id("the").unwrap();
        assert_eq!(m.unigram(the), 230);
        assert_eq!(m.word(the), Some("the"));
        // Ids are the fst's sorted rank, so a round trip through both holds.
        for w in ["the", "quick", "brown", "fox", "cat", "sat", "dog"] {
            let id = m.id(w).unwrap();
            assert_eq!(m.word(id), Some(w));
        }

        let quick = m.id("quick").unwrap();
        let cat = m.id("cat").unwrap();
        assert_eq!(m.bigram(the, quick), Some(200));
        assert_eq!(m.bigram(the, cat), Some(210));
        assert_eq!(m.bigram(the, m.id("fox").unwrap()), None);
        // Successors come back in ascending-id order (the storage order).
        let succ = m.successors(the);
        assert_eq!(succ.len(), 2);
        assert!(succ[0].id < succ[1].id);
        // The out-of-vocabulary successor was dropped, not stored as a dangling id.
        assert!(m.successors(m.id("dog").unwrap()).is_empty());
        // A word with no successors at all.
        assert!(m.successors(m.id("fox").unwrap()).is_empty());
    }

    #[test]
    fn prefix_scan_is_bounded_and_ordered() {
        let bytes = build(&sample(), "").unwrap();
        let m = Model::from_bytes(bytes).unwrap();
        let mut ids = Vec::new();
        m.prefix_ids("c", 16, &mut ids);
        assert_eq!(ids.iter().filter_map(|&i| m.word(i)).collect::<Vec<_>>(), vec!["cat"]);

        ids.clear();
        m.prefix_ids("", 3, &mut ids);
        assert_eq!(ids.len(), 3, "the limit bounds an empty prefix");

        ids.clear();
        m.prefix_ids("zzz", 16, &mut ids);
        assert!(ids.is_empty());
    }

    #[test]
    fn build_is_deterministic_regardless_of_input_order() {
        let mut shuffled = sample();
        shuffled.reverse();
        assert_eq!(build(&sample(), "x").unwrap(), build(&shuffled, "x").unwrap());
    }

    #[test]
    fn duplicate_words_are_rejected() {
        let dup = vec![entry("the", 200, &[]), entry("the", 100, &[])];
        assert!(build(&dup, "").unwrap_err().contains("duplicate"));
    }

    #[test]
    fn bad_magic_and_truncation_are_refused() {
        assert!(Model::from_bytes(vec![0u8; 64]).is_err());
        let mut bytes = build(&sample(), "x").unwrap();
        bytes.truncate(HEADER_LEN + 4);
        assert!(Model::from_bytes(bytes).is_err());
    }

    #[test]
    fn quantized_scores_survive_the_round_trip_to_log_probability() {
        // 255 is p = 1; each step is a factor of 1.15.
        assert_eq!(quantize(1.0), 255);
        assert!((score_logp(255)).abs() < 1e-6);
        for p in [0.5_f64, 0.05, 1e-3, 1e-6, 1e-9] {
            let q = quantize(p);
            let back = score_logp(q).exp() as f64;
            let ratio = back / p;
            assert!(ratio > 1.0 / 1.08 && ratio < 1.08, "p={p} q={q} back={back}");
        }
        // Never 0 — 0 is reserved for "absent".
        assert_eq!(quantize(0.0), 1);
        assert_eq!(quantize(1e-30), 1);
    }

    #[test]
    fn varints_round_trip() {
        let mut buf = Vec::new();
        let vals = [0u64, 1, 127, 128, 300, 16384, 1 << 20];
        for v in vals {
            write_varint(&mut buf, v);
        }
        let mut at = 0;
        for v in vals {
            assert_eq!(read_varint(&buf, &mut at), Some(v));
        }
        assert_eq!(at, buf.len());
    }
}
