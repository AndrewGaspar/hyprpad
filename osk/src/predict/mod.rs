//! Word completion and next-word prediction.
//!
//! This is phase 1 of `docs/research/osk-prediction.md`: a small in-process
//! predictor over a static unigram + bigram-successor model ([`model`]), the
//! composition buffer of what the keyboard itself typed ([`context`]), and a
//! personal word cache with AOSP's forgetting curve ([`personal`]). No engine
//! is adopted; nothing leaves the machine; there is no network and no thread.
//!
//! # The three tasks, one ranking (§2.1)
//!
//! * **Completion** — a non-empty prefix: every lexicon word starting with it,
//!   ranked.
//! * **Next-word** — an empty prefix after a separator: the previous word's
//!   stored successors, ranked.
//! * **Correction** — deferred. Phase 1 offers candidates and never replaces
//!   what was typed (§8.6): with an exact cursor and a deliberate click, the
//!   typed word is *evidence*, not noise.
//!
//! One score per candidate, in natural-log space, from stupid backoff
//! (Brants et al. 2007, §2.2):
//!
//! ```text
//! S(w | prev) = c(prev,w)/c(prev)          when the bigram is stored
//!             = 0.4 · P(w)                 otherwise
//! score(w)    = ln S(w | prev) + personal boost
//! ```
//!
//! # The AOSP rules that survive (§2.1, §8.6)
//!
//! * The **typed prefix is candidate 0** whenever it is itself a valid word, or
//!   whenever nothing else clears [`CONFIDENCE_FLOOR`]. It is never silently
//!   replaced.
//! * Candidates are deduplicated, and a candidate equal to the typed prefix is
//!   *moved* to slot 0 rather than shown twice.
//!
//! # Budget
//!
//! ≤ 1 ms per query on the poll thread (§7.3). A query touches at most
//! [`PREFIX_LIMIT`] lexicon entries and the previous word's ≤ 32 successors, all
//! out of the mapped model with no allocation until the winners are copied out.
//! `predictor::tests::a_query_is_well_inside_the_latency_budget` asserts a
//! generous upper bound so a regression is caught without making the suite
//! flaky on a loaded machine.

pub mod context;
pub mod model;
pub mod personal;
pub mod wordlist;

use std::path::Path;

pub use context::Context;
pub use model::Model;
pub use personal::Personal;

/// How many lexicon entries a prefix scan may visit. A one-letter prefix on a
/// 60 k vocabulary matches thousands of words; only the best three are ever
/// shown, and the scan is in fst order (not score order), so the bound trades a
/// vanishing amount of recall for a hard latency ceiling (§8.4).
pub const PREFIX_LIMIT: usize = 512;

/// The log-probability a candidate must reach for the ranker to call it
/// confident. Below it, the typed prefix is inserted at slot 0 even when it is
/// not a known word — the "nothing else is confident" half of the AOSP rule.
///
/// `ln(1e-6)` ≈ −13.8: about a one-in-a-million word after the backoff penalty.
/// A starting point, not a measurement (§8, "all numeric budgets are starting
/// points").
pub const CONFIDENCE_FLOOR: f32 = -14.0;

/// How much one level of the personal cache is worth, in nats. Four sightings
/// (level 4) lift a word by ~1.4 nats — enough to beat a similarly-frequent
/// neighbour, not enough to bury a much commoner word.
pub const LEARN_BONUS: f32 = 0.35;

/// The score a learned word that is *not* in the static model starts from,
/// before its level bonus: roughly a one-in-a-million unigram.
pub const LEARN_BASE: f32 = -13.8;

/// Where a candidate came from. The strip does not colour by this today; it is
/// what the tests assert on, and what a future "learned word" affordance would
/// read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateKind {
    /// Exactly what the user typed (AOSP `KIND_TYPED`). Always slot 0 when present.
    Typed,
    /// A lexicon word extending the typed prefix.
    Completion,
    /// A successor of the previous word, with no prefix typed yet.
    NextWord,
    /// A word from the personal cache.
    Learned,
}

/// One ranked suggestion.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub text: String,
    pub kind: CandidateKind,
    /// Natural-log score; higher is better. `f32::NEG_INFINITY` for a typed
    /// word promoted to slot 0 without a model score of its own.
    pub score: f32,
}

/// The predictor: a static model (optional — with none, prediction is simply
/// off and the keyboard behaves exactly as it did before), the personal cache,
/// and the two gates the daemon drives.
pub struct Predictor {
    model: Option<Model>,
    personal: Personal,
    /// `learn off` from the daemon (password managers, polkit, terminals).
    learn: bool,
    /// `predict off` — suppress suggestions entirely without unloading.
    enabled: bool,
}

impl Default for Predictor {
    fn default() -> Self {
        Predictor::disabled()
    }
}

impl Predictor {
    /// A predictor with no model: [`Self::is_ready`] is false, every query
    /// returns nothing, and the keyboard shows no strip at all.
    pub fn disabled() -> Predictor {
        Predictor { model: None, personal: Personal::in_memory(), learn: true, enabled: true }
    }

    /// Open a model file and the personal store beside it. A missing or
    /// unreadable model is **not** fatal: it is reported and prediction stays
    /// off (§8.3 — "No model → prediction silently off").
    pub fn open(path: &Path) -> Result<Predictor, String> {
        let model = Model::open(path)?;
        let personal = match Personal::default_path() {
            Some(p) => Personal::load(p),
            None => Personal::in_memory(),
        };
        Ok(Predictor { model: Some(model), personal, learn: true, enabled: true })
    }

    /// A predictor over an already-built model, with an in-memory personal
    /// cache. For tests and for a caller that built a model itself.
    pub fn with_model(model: Model) -> Predictor {
        Predictor { model: Some(model), personal: Personal::in_memory(), learn: true, enabled: true }
    }

    /// Whether a model is loaded *and* prediction is switched on — i.e. whether
    /// the keyboard should show a candidate strip at all.
    pub fn is_ready(&self) -> bool {
        self.model.is_some() && self.enabled
    }

    /// The loaded model, if any.
    pub fn model(&self) -> Option<&Model> {
        self.model.as_ref()
    }

    /// The personal cache (read-only access for diagnostics and tests).
    pub fn personal(&self) -> &Personal {
        &self.personal
    }

    /// `predict on|off`.
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    /// `learn on|off` — the daemon's window-deny-list gate (§5.3 rule 2).
    pub fn set_learn(&mut self, on: bool) {
        self.learn = on;
    }

    pub fn learn_enabled(&self) -> bool {
        self.learn
    }

    /// Record a completed word, if learning is on and the token passes the shape
    /// filters ([`personal::learnable`]). Returns whether it was learned.
    pub fn note_word(&mut self, word: &str) -> bool {
        self.note_word_at(word, personal::now_secs())
    }

    /// [`Self::note_word`] with an explicit clock (tests).
    pub fn note_word_at(&mut self, word: &str, now: u64) -> bool {
        if !self.learn {
            return false;
        }
        self.personal.note(word, now)
    }

    /// `forget` — drop every learned word and delete the store.
    pub fn forget_all(&mut self) {
        self.personal.forget_all();
        let _ = self.personal.save(personal::now_secs());
    }

    /// Persist the personal cache (called on hide and at exit). Errors are
    /// reported, never fatal.
    pub fn save(&mut self) {
        if let Err(e) = self.personal.save(personal::now_secs()) {
            eprintln!("hyprpad-osk: could not save learned words: {e}");
        }
    }

    /// Rank up to `n` candidates for `prefix` in `ctx`.
    ///
    /// `prefix` is the word being typed (normally `ctx.partial()`, passed
    /// separately so a caller can ask "what would follow here?" without one).
    /// An empty prefix asks for next-word prediction; a non-empty one asks for
    /// completion. See the module docs for the ranking rules.
    pub fn candidates(&self, ctx: &Context, prefix: &str, n: usize) -> Vec<Candidate> {
        self.candidates_at(ctx, prefix, n, personal::now_secs())
    }

    /// [`Self::candidates`] with an explicit clock, so the personal cache's
    /// decay is testable.
    pub fn candidates_at(&self, ctx: &Context, prefix: &str, n: usize, now: u64) -> Vec<Candidate> {
        let Some(model) = self.model.as_ref() else { return Vec::new() };
        if !self.enabled || n == 0 {
            return Vec::new();
        }

        // The previous word's successor list, fetched once and kept sorted by id
        // (the storage order) so scoring a candidate is a binary search.
        let succ = ctx
            .prev_word()
            .and_then(|w| lookup_id(model, w))
            .map(|id| model.successors(id))
            .unwrap_or_default();
        let bigram = |id: u32| -> Option<u8> {
            succ.binary_search_by_key(&id, |s| s.id).ok().map(|i| succ[i].score)
        };

        let mut scored: Vec<(String, CandidateKind, f32)> = Vec::new();
        let upper_first = prefix.chars().next().is_some_and(char::is_uppercase);

        if prefix.is_empty() {
            for s in &succ {
                let Some(w) = model.word(s.id) else { continue };
                let boost = self.boost(w, now);
                scored.push((w.to_string(), CandidateKind::NextWord, model::score_logp(s.score) + boost));
            }
        } else {
            let mut ids = Vec::new();
            let lower = prefix.to_lowercase();
            model.prefix_ids(&lower, PREFIX_LIMIT, &mut ids);
            // Proper nouns and "I" keep their capital in the lexicon, so a
            // lower-cased prefix would never reach them; scan the title-cased
            // form too when it differs.
            let title = title_case(&lower);
            if title != lower {
                model.prefix_ids(&title, PREFIX_LIMIT, &mut ids);
            }
            ids.sort_unstable();
            ids.dedup();
            for id in ids {
                let Some(w) = model.word(id) else { continue };
                let base = match bigram(id) {
                    Some(q) => model::score_logp(q),
                    None => BACKOFF_LN + model::score_logp(model.unigram(id)),
                };
                let text = if upper_first { title_case(w) } else { w.to_string() };
                let boost = self.boost(w, now);
                scored.push((text, CandidateKind::Completion, base + boost));
            }
            // Learned words the static model does not know.
            for (w, level) in self.personal.visible_with_prefix(&lower, now) {
                if model.id(w).is_some() {
                    continue; // already scored above, with its boost
                }
                let text = if upper_first { title_case(w) } else { w.to_string() };
                scored.push((text, CandidateKind::Learned, LEARN_BASE + LEARN_BONUS * level as f32));
            }
        }

        // Best first, ties broken by the word so the order is deterministic.
        scored.sort_by(|a, b| {
            b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(&b.0))
        });
        scored.dedup_by(|a, b| a.0 == b.0);

        // The typed word is candidate 0 when it is a real word, or when nothing
        // else is confident (AOSP `Suggest.java`; §2.1/§8.6).
        let mut typed: Option<Candidate> = None;
        if !prefix.is_empty() {
            let confident = scored.first().is_some_and(|(_, _, s)| *s >= CONFIDENCE_FLOOR);
            let valid = self.is_word(prefix, now);
            if valid || !confident {
                // If the prefix is already in the list, take its score with it.
                let existing = scored.iter().position(|(w, _, _)| w == prefix);
                let score = match existing {
                    Some(i) => scored.remove(i).2,
                    None => f32::NEG_INFINITY,
                };
                typed = Some(Candidate {
                    text: prefix.to_string(),
                    kind: CandidateKind::Typed,
                    score,
                });
            }
        }

        let mut out = Vec::with_capacity(n);
        if let Some(t) = typed {
            out.push(t);
        }
        for (text, kind, score) in scored {
            if out.len() >= n {
                break;
            }
            out.push(Candidate { text, kind, score });
        }
        out
    }

    /// The personal-cache bonus for a word (0 when it is not visible yet).
    fn boost(&self, word: &str, now: u64) -> f32 {
        let level = self.personal.level(word, now);
        if level < personal::MIN_VISIBLE_LEVEL {
            0.0
        } else {
            LEARN_BONUS * level as f32
        }
    }

    /// Whether `word` is a word the keyboard knows — in the lexicon (in any
    /// casing) or visible in the personal cache.
    fn is_word(&self, word: &str, now: u64) -> bool {
        if self.personal.is_visible(word, now) {
            return true;
        }
        self.model.as_ref().is_some_and(|m| lookup_id(m, word).is_some())
    }
}

/// `ln(0.4)` — the stupid-backoff penalty, precomputed.
const BACKOFF_LN: f32 = -0.916_290_7;

/// Find a word's id, tolerating the casing the user typed: as written, then
/// lower-cased (so "The" finds "the"), then title-cased (so "wayland" finds
/// "Wayland" when only the proper noun is in the lexicon).
fn lookup_id(model: &Model, word: &str) -> Option<u32> {
    if let Some(id) = model.id(word) {
        return Some(id);
    }
    let lower = word.to_lowercase();
    if lower != word {
        if let Some(id) = model.id(&lower) {
            return Some(id);
        }
    }
    let title = title_case(&lower);
    if title != word {
        return model.id(&title);
    }
    None
}

/// Upper-case the first character, leave the rest alone.
fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

#[cfg(test)]
pub(crate) mod fixture {
    //! The tiny in-memory model the predictor tests rank against. It is built
    //! from the same word-list format as the committed fixture
    //! (`osk/data/fixture.words`), by the same code path, so a test here and the
    //! shipped fixture can never disagree about the format.
    use super::model::{build, Model};
    use crate::predict::wordlist;

    /// The committed fixture word list itself, so a unit test here and the
    /// shipped `data/fixture.model` can never disagree about either.
    pub const WORDS: &str = include_str!("../../data/fixture.words");

    pub fn model() -> Model {
        let entries = wordlist::parse(WORDS).expect("fixture word list parses");
        Model::from_bytes(build(&entries, "fixture").unwrap()).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(text: &str) -> Context {
        let mut c = Context::new();
        c.push_str(text);
        c
    }

    fn predictor() -> Predictor {
        Predictor::with_model(fixture::model())
    }

    fn texts(c: &[Candidate]) -> Vec<&str> {
        c.iter().map(|x| x.text.as_str()).collect()
    }

    #[test]
    fn completion_ranks_by_bigram_then_unigram() {
        let p = predictor();
        // No context: "hello" and "help" both start with "hel"; "hello" is the
        // commoner unigram, "helm" the rarest.
        let c = ctx("hel");
        let got = p.candidates(&c, "hel", 3);
        assert_eq!(texts(&got), vec!["hello", "help", "helm"]);
        // "hel" is not a word and "hello" is confident, so no typed slot 0.
        assert_eq!(got[0].kind, CandidateKind::Completion);
    }

    #[test]
    fn the_previous_word_reorders_the_completions() {
        let p = predictor();
        // With no context, "k…" completes to the commonest k-word.
        assert_eq!(p.candidates(&ctx("k"), "k", 1)[0].text, "know");
        // After "the", the stored `the → keyboard` bigram promotes a word two
        // orders of magnitude rarer past it. That reordering is the whole point
        // of carrying context.
        let got = p.candidates(&ctx("the k"), "k", 3);
        assert_eq!(got[0].text, "keyboard", "the>keyboard is a stored bigram");
        let bare = p.candidates(&ctx("k"), "k", 32);
        let bare_keyboard = bare.iter().find(|c| c.text == "keyboard").expect("keyboard completes k");
        assert!(
            got[0].score > bare_keyboard.score,
            "the bigram must score the word higher than the bare unigram does"
        );
    }

    #[test]
    fn next_word_prediction_uses_the_previous_words_successors() {
        let p = predictor();
        let c = ctx("hello ");
        let got = p.candidates(&c, "", 3);
        assert_eq!(texts(&got), vec!["world", "there", "hyprland"]);
        assert!(got.iter().all(|x| x.kind == CandidateKind::NextWord));
    }

    #[test]
    fn a_typed_word_that_is_a_real_word_is_always_candidate_zero() {
        let p = predictor();
        let c = ctx("hello");
        let got = p.candidates(&c, "hello", 3);
        assert_eq!(got[0].text, "hello");
        assert_eq!(got[0].kind, CandidateKind::Typed);
        // …and it is not also shown as a completion of itself.
        assert_eq!(got.iter().filter(|x| x.text == "hello").count(), 1);
    }

    #[test]
    fn a_typed_prefix_nothing_can_complete_is_candidate_zero_too() {
        let p = predictor();
        let c = ctx("zx");
        let got = p.candidates(&c, "zx", 3);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "zx");
        assert_eq!(got[0].kind, CandidateKind::Typed);
    }

    #[test]
    fn nothing_is_offered_with_no_prefix_and_no_context() {
        let p = predictor();
        assert!(p.candidates(&Context::new(), "", 3).is_empty());
        // An unknown previous word has no successors either.
        assert!(p.candidates(&ctx("frobnicate "), "", 3).is_empty());
    }

    #[test]
    fn casing_follows_what_was_typed() {
        let p = predictor();
        let c = ctx("Hel");
        let got = p.candidates(&c, "Hel", 3);
        assert_eq!(texts(&got), vec!["Hello", "Help", "Helm"]);
        // A capitalised previous word still resolves to its lexicon entry, so
        // the bigram context survives "The " at the start of a sentence.
        assert_eq!(p.candidates(&ctx("The k"), "k", 1)[0].text, "keyboard");
    }

    #[test]
    fn a_disabled_predictor_offers_nothing() {
        let mut p = predictor();
        p.set_enabled(false);
        assert!(!p.is_ready());
        assert!(p.candidates(&ctx("hel"), "hel", 3).is_empty());
        assert!(Predictor::disabled().candidates(&ctx("hel"), "hel", 3).is_empty());
    }

    #[test]
    fn learned_words_join_the_completions_once_they_are_visible() {
        const T0: u64 = 1_700_000_000;
        let mut p = predictor();
        // A word the static model has never heard of.
        assert!(p.note_word_at("hyprpad", T0));
        assert!(p.candidates_at(&ctx("hyp"), "hyp", 3, T0).iter().all(|c| c.text != "hyprpad"));
        assert!(p.note_word_at("hyprpad", T0 + 5));
        let got = p.candidates_at(&ctx("hyp"), "hyp", 3, T0 + 5);
        assert!(got.iter().any(|c| c.text == "hyprpad" && c.kind == CandidateKind::Learned));
        // …and it fades again.
        const DAY: u64 = 86_400;
        let later = T0 + 40 * DAY;
        assert!(p.candidates_at(&ctx("hyp"), "hyp", 3, later).iter().all(|c| c.text != "hyprpad"));
    }

    #[test]
    fn learning_is_gated_off_by_the_daemons_command() {
        let mut p = predictor();
        p.set_learn(false);
        assert!(!p.note_word_at("hyprpad", 1));
        assert!(!p.note_word_at("hyprpad", 2));
        assert!(p.personal().is_empty());
        p.set_learn(true);
        assert!(p.note_word_at("hyprpad", 3));
        assert_eq!(p.personal().len(), 1);
        p.forget_all();
        assert!(p.personal().is_empty());
    }

    #[test]
    fn sightings_lift_a_word_the_model_already_knows() {
        const T0: u64 = 1_700_000_000;
        let mut p = predictor();
        // "wayland" (9e-7) is rarer than "Wayland" is common, and both are in
        // the fixture; four sightings of the lower-case one put it first.
        let before = p.candidates_at(&ctx("wayl"), "wayl", 3, T0);
        let bare = before.iter().find(|c| c.text == "wayland").unwrap().score;
        for i in 0..4 {
            assert!(p.note_word_at("wayland", T0 + i));
        }
        let got = p.candidates_at(&ctx("wayl"), "wayl", 3, T0 + 4);
        assert_eq!(got[0].text, "wayland");
        assert!(got[0].score > bare, "four sightings must raise the score");
        // A word the model knows keeps its Completion kind; the cache only
        // reweights it.
        assert_eq!(got[0].kind, CandidateKind::Completion);
    }

    #[test]
    fn a_query_is_well_inside_the_latency_budget() {
        let p = predictor();
        let c = ctx("the h");
        // Warm the page cache / branch predictors before timing.
        for _ in 0..50 {
            let _ = p.candidates(&c, "h", 3);
        }
        let start = std::time::Instant::now();
        const N: u32 = 500;
        for _ in 0..N {
            let got = p.candidates(&c, "h", 3);
            assert!(!got.is_empty());
        }
        let per = start.elapsed() / N;
        // The design budget is 1 ms (§7.3); assert a generous 5 ms so this
        // catches an algorithmic regression without failing on a loaded CI box.
        assert!(per.as_micros() < 5_000, "candidate query took {per:?} (budget 1 ms, bound 5 ms)");
    }
}
