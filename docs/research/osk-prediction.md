# Research: word completion / next-word prediction for `hyprpad-osk`

*Produced 2026-09-02. Verification basis: **VERIFIED(local)** = read from this
repo (`osk/`, `src/`), from the compositor source running on the target machine
(`/home/ajg/code/hypxrland`, HEAD `ba5e361f8`, 2026-08-27), from installed
packages, or from the Steam client bundle on this machine
(`~/.local/share/Steam/steamui/chunk~2dcc5aaf7.js`). **VERIFIED(web)** =
confirmed on a cited upstream page this session (a "(snippet)" qualifier means
only a search-engine excerpt of that page could be read). **INFERRED** =
reasoned from verified facts. **UNVERIFIED** = from memory or a secondary
summary; treat as a lead, not a fact. All numeric budgets in §8 are starting
points, not measurements.*

**Scope.** `hyprpad-osk` is a Rust layer-shell keyboard driven by the
controller's two trackpads (per-pad absolute cursors; pad click or full trigger
pull commits the key under the cursor) that types through a **uinput** virtual
keyboard and therefore has *no view of the focused application's text buffer*.
This document answers: which prediction models and open components are worth
using, what freely licensed data exists, how to personalise without learning
passwords, whether the Wayland input-method protocols are the way to get
context and to commit whole words, how candidates should be selected with a
controller, and what to build first.

---

## 0. Summary and recommendation

1. **Build a small in-process predictor in Rust; do not adopt an existing
   engine.** Nothing in the Linux ecosystem is both maintained and shaped for
   this: Presage is dead upstream (0.9.1, 2015) and Maliit removed it in 2024;
   Squeekboard and wvkbd have no prediction; onboard's `pypredict` is GPL C++
   with tiny bigram data; libime/KenLM are LGPL C++ built for CJK/MT and expose
   no "enumerate next words" API; there is **no maintained pure-Rust n-gram LM
   crate** (§3). The two things worth *porting* are AOSP LatinIME's
   forgetting-curve personalisation and its "typed word is always candidate 0,
   auto-correct only above a confidence threshold" ranking rules; the things
   worth *consuming as data* are permissively licensed word lists (§4).
2. **Phase 1 model = unigram FST + per-word bigram successor lists with stupid
   backoff**, ~3–4 MB on disk, sub-millisecond lookups, fully offline. That is
   the same class of model Gboard shipped in production for years (a 1.25 M
   n-gram, 164 k-word Katz 5-gram; §2.2) and it already gives roughly 13 %
   top-1 / 22 % top-3 next-word recall on English; the shipped LSTM only lifts
   that to 16.5 % / 27 % (§2.7). Neural is a phase-3 rerank, not a foundation.
3. **Context in phase 1 is local only**: the words the OSK itself has committed
   since it was shown, reset on show/hide and on focus change (the daemon
   already observes focus). That is enough for bigram prediction and for
   completion; it is not enough to *correct* text the OSK did not type.
4. **Phase 2 binds `zwp_input_method_v2` opportunistically** — never with
   `grab_keyboard`, and degrading to uinput-only on `unavailable` (fcitx5 is
   running on the dev machine and takes the single IME slot; §6.4). Its value
   is not typing — it is `content_type` (password/PIN/sensitive → suggestions
   and learning off), `surrounding_text` (real context after a focus change),
   and `commit_string`/`delete_surrounding_text` for clean whole-word replace in
   the apps that speak text-input-v3. Because the owner controls the
   compositor, the cleaner long-term fix is a small HypXRland-side channel
   that exposes the focused text-input's content-type to the OSK even when
   fcitx5 owns the IME (§6.6).
5. **Selection must never require pointer travel**: a 3-candidate strip above
   the top row, **R1 accepts the highlighted (centre/best) candidate, L1 cycles
   the highlight**, the pads can also hover-and-click the strip. This is the
   PS5/Xbox pattern (§7.2) and the accessibility literature is blunt that a
   suggestion list that costs more to scan and reach than typing the remaining
   letters is a net loss (§7.4). Steam Deck's own keyboard has **no**
   prediction at all (VERIFIED(local): no suggestion identifiers in the
   current bundle), so this is a parity-plus feature with no Valve behaviour
   to copy.
6. **Personalisation is an on-disk cache with AOSP's decay constants**, only
   surfaced after a word has been seen twice, and gated off by content-type
   hints (phase 2), by a daemon-side "never learn in these windows" list
   (password managers, polkit agents, terminals — the daemon already inspects
   the focused window and its process tree), by digit/symbol heuristics, and by
   an explicit incognito toggle (§5).
7. **Latency budget**: candidate generation ≤ 1 ms on the OSK's poll thread
   (it coalesces into the existing frame pacing), anything slower runs async
   and is dropped if stale. Gboard's own target is "visible response within
   ~20 ms" (§7.3).

---

## 1. Where predictions plug in today — VERIFIED(local)

Reading `osk/src/app.rs`, `osk/src/output.rs`, `osk/src/control.rs`,
`src/osk.rs`, `src/run.rs`:

- **Commit path.** `commit <L|R>` → `Osk::commit(pad)` → `commit_key_index(k)`
  (`app.rs:447-494`): `KeyRole::Char` calls `tap(keycode, shift)`; Space,
  Enter, Tab, Backspace tap their keycodes; Shift/Caps/LayerToggle/DisplayToggle
  mutate state. `type <ascii>` → `type_str` → `VirtualKeyboard::type_text`,
  which maps each char through `key_for` (`output.rs:202`) — **US-ASCII only;
  anything else is silently skipped**. Each `tap` sleeps 6 ms between press and
  release and 6 ms after (`output.rs:136-152`), so a k-character burst costs
  ≈ 12·k ms; a 10-letter completion is ~120 ms of keystroke replay.
- **There is no text buffer.** The OSK knows only which keycodes it emitted.
  Nothing tells it whether the app auto-capitalised, whether an earlier
  Backspace deleted a real character, or what was in the field before the OSK
  opened. Any phase-1 context model is therefore *what we typed since show()*.
- **Daemon routing while the OSK is up** (`run.rs:2059-2107`, `route_osk`):
  B or Menu dismiss; pad click or **full trigger** on the same side commits
  under that pad's cursor; every other edge-down consults `[osk_buttons]` (the
  shipped config binds Y = Space, X = Backspace, `config/hyprpad.lua:151-152`)
  and taps that keycode through the OSK via `key <keycode>`. **Bumpers L1/R1,
  grips L4/L5/R4/R5, sticks, d-pad, View, R3/L3 are currently unbound while
  the OSK is active** — the free buttons for candidate selection.
- **Back-channel.** The OSK prints `event crossed <L|R>` on stdout; the daemon
  fires the haptic tick. The same channel can carry `event candidate …` lines
  for a haptic cue on candidate change.
- **Rendering.** CPU shm renderer, `font8x8` glyphs, dirty-flag + frame-callback
  paced (`README.md` "Frame pacing"). A suggestion strip is one more panel
  region redrawn on the same dirty path; the 8×8 font is legible but will want
  a real font for proportional candidate text (already a deferred item, §4.7 of
  `osk-technology.md`).
- **`osk-technology.md` already settled** the injection landscape (§3 there):
  IM-v2 `commit_string` is silently dropped for XWayland and for Wayland apps
  without text-input-v3 (most games); uinput works everywhere including
  XWayland but is ASCII-only; the seat keymap follows the last keyboard that
  sent a key; and Hyprland allows exactly one IME (§7 there). This document
  builds on those findings rather than re-deriving them.

---

## 2. Prediction models — what the literature and production keyboards do

### 2.1 Three different tasks, one ranking

Production keyboards blend three things into one candidate list:

| Task | Input | Output | Where the OSK differs from a phone |
|---|---|---|---|
| **Completion** | prefix typed so far + context | words starting with the prefix | identical |
| **Next-word prediction** | context only (after a separator) | likely next words | identical |
| **Correction** | noisy prefix/word + context | words *near* the typed one | the cursor is exact (§2.5) — noise is *targeting* error on a large key grid, not fat-finger Gaussian spread; sloppy-swipe decoding is irrelevant |

AOSP LatinIME's `Suggest.java` is the clearest public statement of how they
are combined (VERIFIED(web), `android.googlesource.com` main branch): the typed
word is always inserted at index 0 as `KIND_TYPED`; candidates are deduped;
auto-correct fires only when the native engine's
`mFirstSuggestionExceedsConfidenceThreshold` **and**
`AutoCorrectionUtils.suggestionExceedsThreshold` agree, and never for digits,
mostly-caps input, resumed composition, or when the first suggestion is a
shortcut; `KIND_WHITELIST` entries bypass scoring. The historical thresholds
(`jb-release` `config.xml`): modest 0.185, aggressive 0.067, very aggressive 0;
the native cost model (`scoring_params.cpp`) charges SUBSTITUTION 0.3806,
OMISSION 0.467, INSERTION 0.7248, TRANSPOSITION 0.5608, PROXIMITY 0.0694, with
`EXACT_MATCH_PROMOTION 1.1` and a language cost = unigram/bigram improbability
× `DISTANCE_WEIGHT_LANGUAGE` added to the spatial/edit cost. Firefox OS's Gaia
Latin IME did the same thing in JS with multiplicative weights (match 1.0,
insertion 0.3, transposition 0.3, deletion 0.1, nearby-key substitution scaled
by QWERTY distance) over a ternary-search-tree blob with 5-bit frequencies
(VERIFIED(web), Mozilla wiki). The pattern to copy: **one score per candidate
= language log-prob − edit cost, typed word always present, correction opt-in
above a threshold.**

### 2.2 N-gram language models

- **Modified Kneser–Ney** (Chen & Goodman 1998/1999) remains the reference
  smoothing; **KenLM** (Heafield 2011; Heafield et al. 2013 for streaming
  estimation) is the standard toolkit — probing hash for speed, trie for size,
  `-q`/`-b` quantisation of prob/backoff to 8/7 bits, `-a` pointer
  compression (VERIFIED(web), kheafield.com). Query cost is ≈ 0.55 µs per
  5-gram on 2011 hardware (Table 1 of the KenLM paper), so ranking a few
  hundred candidates is well under a millisecond.
- **Stupid backoff** (Brants et al. 2007): `S(w|ctx) = count ratio if seen,
  else 0.4·S(w|shorter ctx)`; not a normalised distribution, but for *ranking*
  candidates (all we need) it is indistinguishable from KN at small sizes and
  trivial to implement (VERIFIED(web) for the paper; the "indistinguishable
  for ranking" claim is INFERRED).
- **Production sizes.** Gboard's production n-gram LMs "do not exceed 1.5 M
  n-grams and include fewer than 200 K unigrams", use a 64 K vocabulary for
  higher orders, and "do not exceed ten megabytes"; Hard et al. 2018 describe
  the baseline as a Katz-smoothed interpolated 5-gram with 1.25 M n-grams and
  164 K unigrams (VERIFIED(web), arXiv 1910.03432 and 1811.03604). Pauls &
  Klein 2011 reach 23 bits per n-gram lossless. So a keyboard-grade English
  model is a few MB, not hundreds.
- **Character-level PPM** (Cleary & Witten 1984; Dasher, Ward/Blackwell/MacKay
  2000) is the right model for *letter-by-letter* interfaces (Dasher zooms
  through letters) and is still used in AAC research — Adhikary & Vertanen
  2023 get +9.9 % relative keystroke savings from PPM/RNN personalisation
  (VERIFIED(web) snippet). For a *word-chip* strip it is the wrong shape; no
  shipping commercial keyboard is known to use it (UNVERIFIED — none found).
  Dasher's PPM defaults (`Parameters.cpp`): max order 5, α 49, β 77, node
  budget 1000–3000 (VERIFIED(web)).

### 2.3 Neural models

- **Gboard** (Hard et al. 2018, arXiv 1811.03604; VERIFIED(web)): single-layer
  CIFG-LSTM, 670 units, 96-d tied embeddings, 10 k vocab, 1.4 M params,
  **1.4 MB quantised**, trained federated. The 2024 "Neural Search Space"
  paper (arXiv 2410.15575) says the deployed NN-LM is now 30 k vocab / 6.4 M
  params, added +17–28 % decoder latency, and improved words-modified-ratio by
  only 0.26–1.19 % — while restating the **20 ms** feedback target. Gboard's
  decoder itself is an FST composition `C ∘ L ∘ G` (key sequence → lexicon →
  n-gram grammar) with beam search (Ouyang et al. 2017, arXiv 1704.03987; the
  2017 Google Research blog). "Proofread" (2024) is a *server-side* PaLM2-XS
  feature, not on-device.
- **Apple** (iOS 17 press release, VERIFIED(web)): autocorrect and inline
  prediction use "a transformer language model … on-device". Third-party
  reverse engineering (Jack Cook, 2023) found a GPT-2-style 6-block, 512-hidden,
  ~34 M-param model with a 15 k vocabulary; Apple has published no numbers.
- **SwiftKey** shipped neural predictions in 2016; a 2024 Microsoft paper
  (arXiv 2505.05648) describes replacing a GRU with a 4-layer/4-head/512-hidden
  GPT-2-style transformer ≈ 6 MB quantised plus a 64 K unigram back-off, edit
  rate 7.21 % vs 7.34 % (VERIFIED(web)). **Samsung** (Yu et al. 2017,
  arXiv 1707.01662): compressed LSTM 7.40 MB, 6.47 ms/word on-device, KSS
  65.1 % vs Apple 64.4 / SwiftKey 62.4 / Google 58.9 on their benchmark.
- **On-device cost datapoints**: 2.2 M-param LSTM next-word ≈ 33 ms on a
  Galaxy S9, an optimised 2.1 M-param conv ≈ 22 ms (Desai et al. 2020,
  arXiv 2002.01535, VERIFIED(web)); a desktop x86 core is several × faster
  (INFERRED). A ~1 M-param GRU at hidden 256 is ~2–3 MFLOP per step —
  sub-millisecond in `candle`/`tract` (INFERRED, arithmetic). Tiny GGUF LLMs:
  SmolLM2-135M is 105 MB (Q4_K_M) / 145 MB (Q8_0); Qwen2.5-0.5B 491 MB; >15
  tok/s on a Raspberry Pi 4 for ≤135 M models (VERIFIED(web)). Per-keystroke
  use would re-evaluate the prompt each time, so the practical cost is
  prompt-eval latency, not generation; and 135 M-class models are weak
  (Willison: "94 MB really isn't enough space for a model that can do anything
  useful") — next-word *ranking* is easier than chat, but that is UNVERIFIED
  for these models.

### 2.4 Federated / DP personalisation (for the record)

Gboard trains next-word models federated with differential privacy (Xu et
al. 2023, arXiv 2305.18465: "all next-word prediction neural network models in
Gboard now have DP"); Apple's 2017 "Learning with Privacy at Scale" uses local
DP sketches to *discover* new words (VERIFIED(web)). None of this applies to a
single-user local keyboard; it is cited only to make the point that the
commercial keyboards' personalisation is server-assisted and ours will not be.

### 2.5 Joint decoding and what an exact cursor changes

Touch keyboards model each tap as a 2-D Gaussian around the key centre and
decode `argmax P(word) · Π P(touch_i | key_i)` (Goodman et al. 2002 —
error rate down by 1.67–1.87×; Fowler et al. 2015 — a spatial+LM decoder takes
WER 38.4 % → 5.7 %, a unigram personalisation cache → 4.6 %; Bi/Ouyang/Zhai
2014 on jointly optimising completion and correction; VelociTap for
sentence-level decoding at 41 wpm/3 % CER; all VERIFIED(web)).

The trackpad OSK is different in kind: the cursor is exact and the commit is a
discrete click, so the dominant errors are (a) **wrong-key-adjacent** — the
finger stopped one key early/late on a large grid, i.e. a *substitution by a
neighbour*, and (b) **double or missed clicks** — insertion/omission. There is
no continuous touch-point likelihood to exploit, so the right correction model
is the AOSP/Firefox-OS one: edit distance with a discounted cost for
neighbour substitutions (the layout is known, so a QWERTY adjacency table is
free). And because a click is deliberate, **auto-correct-on-space should be
off by default** — the typed word stays candidate 0 (INFERRED; Alharbi et al.
2020 found fixing a wrong autocorrect costs ≈ 5.5 s and autocorrect gave no
significant speed gain even on touch, VERIFIED(web)).

### 2.6 Cursor-, gaze- and switch-driven keyboards

Relevant because this OSK is pointer-driven, not touch-driven:

- Zeng et al. 2023 (eye typing): no prediction 3.39 wpm, letter prediction
  3.42, letter+**word** prediction 5.48 — letter prediction alone gave nothing
  (VERIFIED(web)).
- Trnka et al. 2008 (on-screen keyboard, 5-word list, 28 participants):
  users took *longer* per selection with prediction (2.26 s → 2.95 s per
  keystroke) but the trigram model still raised communication rate from 5.15 to
  8.17 wpm (+58.6 %) because it saved 52 % of keystrokes; a weaker "basic"
  model saved 19.8 % and gained far less. Utilisation of available savings was
  93.6 % for the good model vs 78.2 % for the weak one — **users learn to trust
  a good list and ignore a bad one** (VERIFIED(web)).
- Koester & Levine 1996 (mouth-stick users): word prediction *decreased*
  entry rate — "the cognitive cost … largely overwhelmed the benefit"; Anson
  et al. 2004 (mouse-driven OSK, able-bodied): ≈ +10 % but "many participants
  were frustrated" (VERIFIED(web) via secondary sources).

Design consequence: prediction quality and a near-zero selection cost are not
nice-to-haves; below a quality bar the feature should be off.

### 2.7 What to expect (numbers to set targets against)

| Metric | n-gram | small LSTM | Source |
|---|---|---|---|
| Next-word top-1 recall (en, Gboard logs) | 13.0 % | 16.5 % | Hard 2018 |
| Next-word top-3 recall | 22.1 % | 27.1 % | Hard 2018 |
| Live Gboard n-gram top-1 en_US | ≈ 10 % | — | Chen 2019 (arXiv 1910.03432) |
| Keystroke savings, completion+prediction | 46–49 % (C-PAK on Gboard-class) / 59.7 % (AAC corpus) | 62–65 % (commercial, Samsung benchmark) / 65.2 % (LLM, AAC) | TOCHI 2023; Gaines & Vertanen 2025; Yu 2017 |
| Inference | ~0.1 ms (n-gram) | 55–120 ms (LLM search, GPU) | Gaines & Vertanen 2025 |

All VERIFIED(web) as reported in the cited papers; none measured here.

---

## 3. Open-source components a Rust project could use or port

### 3.1 Comparison

| Component | Licence | Language | Model / data size | Maintenance (2026) | Quality signal | Integration | Verdict |
|---|---|---|---|---|---|---|---|
| **Presage** (ex-Soothsayer) | GPL-2.0+ | C++ (+SWIG) | SQLite n-gram DBs, trained by `text2ngram` | Upstream dead: 0.9.1 (2015-04-21); Debian orphaned | Maliit **removed** it 2024 ("worked quite poorly … people commonly turn off"); still used by phosh-osk-stevia, lomiri-keyboard, OptiKey | Don't link (GPL/C++). Its SQLite schema (`_N_gram` tables, `LIKE 'prefix%'`) is trivially readable; the predictor-combination idea (n-gram + recency + dejavu) is worth copying | **Port ideas, not code** |
| sailfish-keyboard/presage fork | GPL-2 | C++ | MARISA-backed DBs ("several times smaller") | commits 2025-11 | Sailfish OS ships it | as above | Same |
| **AOSP LatinIME** | Apache-2.0 code; dictionaries "© Lexiteria LLC. Used by permission" (NOTICE) | Java + C++ | `main_en.dict` 1.07 MB; `en_US_wordlist.combined.gz` 862 KB, 160,715 words *with* bigrams (2014) | AOSP main branch; HeliBoard keeps it alive | Shipped on every Android until Gboard | Parse the `.combined` text format (gzip + `word=…,f=…` / `bigram=…,f=…` lines, f 0–255 log scale, ÷1.15 per step); port `forgetting_curve_utils.cpp` and the `Suggest.java` rules. Binary `.dict` (v2/v4 PtNode format) has **no Rust reader**; not worth writing | **Port the ranking + decay logic; dictionary data licence is unclear → do not ship** |
| **KenLM** / `kenlm-rs` 0.1.0 | LGPL-2.1+ (crate LGPL-3.0+) | C++ (crate vendors + compiles it) | ARPA/binary; a pruned word trigram for 50 k vocab ≈ 5–20 MB (UNVERIFIED estimate) | KenLM commits 2025-03; crate has 4 commits (2026-04-19) | Standard in MT/ASR | FFI build, C++ toolchain, LGPL relink obligations; **no next-word enumeration API** — you iterate candidates and score | Only if a KN 4/5-gram is ever wanted; overkill for phase 1 |
| **libime** (fcitx) | LGPL-2.1+ | C++ | Chinese LMs only shipped | 1.1.15 (2026-06) | fcitx5 CJK | Has `Prediction`, `HistoryBigram`, `UserLanguageModel` classes over vendored KenLM; no English data | No |
| **fcitx5 keyboard "word hint" + spell module** | LGPL-2.1+ | C++ | `en_dict.fscd` 1.4 MB (VERIFIED(local)) — magic `FSCD0000`, u32 count, `u16 + NUL-terminated UTF-8` entries | 5.1.21 (installed here) | Completion-only, off by default | `.fscd` is readable from Rust in ~20 lines; its hint = constrained Levenshtein over the whole list | Data format is a fallback word list; **not** a predictor |
| **ibus-typing-booster** | GPL-3 | Python | Hunspell word lists + user SQLite | 2.31.0 (2026-08-26) | GNOME OSK's suggestion source | Subprocess/IBus only | No (Python, IBus) |
| **Maliit keyboard** | LGPL-3 (BSD src) | C++/QML Qt5 | Hunspell | 2.3.1 (2024-07); commits 2024-09 | Plasma Mobile past | Hunspell completion in `plugins/westernsupport` | No |
| **Squeekboard** | GPL-3+ | Rust + C, GTK3 | none | 1.43.1 (2024-11); 2025–26 commits are translations | README lists "Text prediction/correction" under TODO; issue #54 open since 2019 | — | Nothing to take |
| **phosh-osk-stevia** | GPL-3 | C | completers: hunspell / presage / `pipe` / fzf / varnam | 0.57.0 (2026-08-19) | Phosh's actual prediction work | Its `pipe` completer (stdin words in → suggestions out) is a ready-made subprocess contract | Copy the completer *interface* idea only |
| **wvkbd** | **GPL-3** (not MIT) | C | none | 0.20 (2026-07) | "a keyboard, not …" — explicitly no prediction | — | Nothing |
| **onboard** (onboard-osk fork) / `pypredict` | GPL-3+ (COPYING default covers `models/`) | C++ + Python | `models/en_US.lm` 682 KB: **42,635 unigrams, 6,398 bigrams** as counts (VERIFIED(web)) | 1.4.4-5 (2026-08-14), Wayland "experimental" via uinput | The only Linux OSK that shipped n-gram prediction; README: KN/Witten-Bell/absolute discounting, "low double to single digit ms @3 GHz, ~35 000 words", "<30 MiB per million n-grams" | `.lm` is a trivial `count word` text format; the C++ dynamic trie is ~portable | Reference implementation to read; **bigram data too thin and GPL** |
| **Dasher** / **DasherCore** | classic GPL-2; DasherCore **MIT** (relicensed) | C++17, C API | alphabets XML + training text | DasherCore commits 2026-09-01 | AAC standard | PPM is character-level | Not for a word strip |
| **wordfreq** (Python) / **wordfreq-rs** | code Apache-2 / MIT+Apache-2; **data CC BY-SA 4.0** | Python / Rust | `large_en` 1.30 MB, `small_en` 126 KB zstd text (Rust release assets) | Python frozen (SUNSET, Sept 2024: web "full of slop"); Rust 0.2.3 (2023-06) | The de-facto multilingual frequency list | `wordfreq-model` embeds a chosen list at build time via feature flags (`large-en`, `small-en`, …); see §4.1 for the download-vs-vendor packaging note | **Yes for unigrams** (share-alike on the data file) |
| **SymSpell**: `symspell` 0.5.2 (reneklacan), `symspell_rs` 6.8.3 (wolfgarbe), `symspell_complete_rs` 0.0.4 | MIT | Rust | 82,765-word English list bundled; ED-2 index ≈ 1.5 M delete keys per 100 k words | 2026-03 / 2025-12 / 2026-06 | 0.033 ms/word at ED2 (C#) | Pure Rust, `lookup_compound` + bigrams; `symspell_complete_rs` does prefix+fuzzy in one | Optional for correction; an `fst` Levenshtein automaton does the same with no extra index |
| **`fst`** 0.4.7 | MIT / Unlicense | Rust | 80 k words ≈ 250–400 KB (INFERRED) | 2021 release; repo commits 2024-09; 26 M downloads | BurntSushi | `Map<Vec<u8>>` with u64 values, prefix ranges, `levenshtein` feature, mmap-able | **Core of the phase-1 lexicon** |
| `rsmarisa` 0.4.2 | BSD-2 | Rust | LOUDS trie ≈ 5 B/key | 2026-06 | port of marisa | Alternative to `fst` | Only if `fst` size disappoints |
| `spellbook` 0.4.2 / `zspell` 0.5.5 / `hunspell-rs` | MPL-2 / Apache-2 / C bindings | Rust | needs `.dic/.aff` | 2026-07 / 2024-06 / 2022 | Zed, Helix ecosystem | Hunspell check+suggest, pure Rust | Optional: affix expansion at build time |
| `candle` 0.11 / `burn` 0.21 / `tract-onnx` 0.23.5 / `ort` 2.0-rc | MIT/Apache | Rust | model-dependent | all active 2026 | `candle_nn::rnn::{LSTM,GRU}`, `burn::nn::{Lstm,Gru}` | Train in Python, run a ~1 M-param GRU in-process | Phase 3 rerank |
| `llama-cpp-2` 0.1.155 + SmolLM2-135M GGUF | MIT/Apache; model Apache-2 | Rust bindings | 105–145 MB RAM | 2026-08-31 | — | Subprocess or in-process; prompt-eval per keystroke | Experiment only |
| FlorisBoard `nlp` | Apache-2 | C++ ("alpha", last commit 2023-12) | trie dictionaries | stalled; a Rust engine PR (#2970) is an unmerged draft | — | — | Nothing shippable |
| GNOME Shell OSK suggestions | GPL-2 | JS | none of its own | current | — | `keyboard.js` just renders the active IBus engine's lookup table (`ibusCandidatePopup.js`) | Confirms "OSK renders, engine predicts" split |

All rows VERIFIED(web) unless marked; local `cargo info` confirms versions
`wordfreq 0.2.3`, `kenlm-rs 0.1.0`, `symspell 0.5.2`, `fst 0.4.7`,
`zspell 0.5.5` (VERIFIED(local)).

### 3.2 Why "port, don't adopt"

- The only maintained engines are Python (typing-booster), GPL C++ with no
  API for embedding (Presage, onboard), or LGPL C++ built around scoring a
  fixed sentence (KenLM/libime). None enumerate next words; Presage does it
  with `SELECT … WHERE word LIKE 'prefix%' ORDER BY count`.
- The predictor this OSK needs is ~1,000 lines of Rust over `fst` plus a
  successor-list file (§8.3); the hard part is data and UX, not the model.
- `rusqlite` reading a Presage DB is the one "adopt" path that is cheap, but
  the DBs are trained from Wikipedia dumps by a dead tool and are slow at
  scale (the Sailfish fork replaced them with MARISA for exactly that reason).

---

## 4. Data

### 4.1 Sources

| Source | Licence | Size / entries | Register | Use | Verified |
|---|---|---|---|---|---|
| **wordfreq** data (`large_en` / `small_en`) | **CC BY-SA 4.0** (code Apache-2) | large: words ≥ 1 per 10⁸ (Zipf ≥ 1); small: ≥ 1 per 10⁶; snapshot "through about 2021" | blended: Wikipedia, OpenSubtitles 2018, SUBTLEX, NewsCrawl, GlobalVoices, Google Books 2012, OSCAR, Twitter, Reddit | **Unigram frequencies** | VERIFIED(web) |
| **Google Books Ngrams v3** (2020-02-17) | **CC BY 3.0** | eng 1-grams 24 files, 2-grams 589, 3-grams 6,881 (multi-GB); only n-grams seen > 40 times | books; scientific-text bias grows through the 1900s (Pechenick 2015) | **Bigrams** (filter to year ≥ 1980 and to the vocabulary at build time) | VERIFIED(web) |
| **Norvig `count_1w` / `count_2w`** | **unclear** — page licenses only the code (MIT); data derived from the LDC-distributed Google Web 1T; `google-10000-english`'s LICENSE says commercial use needs LDC licensing | 1/3 M words (4.9 MB), 1/4 M bigrams (5.6 MB) | web | Tempting, but do not ship a derived model | VERIFIED(web) |
| **hermitdave/FrequencyWords** (OpenSubtitles 2016/2018) | code MIT, **content CC BY-SA 4.0** | `en_50k.txt` 608 KB; `en_full.txt` 19.1 MB | conversational (subtitles) | Alternative unigram source, closest to chat register | VERIFIED(web) |
| OPUS OpenSubtitles 2018 corpus | attribution requested (Lison & Tiedemann 2016 + link); underlying subtitle copyright unaddressed | 62 languages | conversational | Could build bigrams; legal status murky | VERIFIED(web) for the OPUS terms; risk INFERRED |
| **Wiktionary frequency lists** (TV/2006 etc.) | CC BY-SA (Wikimedia) | TV/2006: 41,284 entries from 29.2 M words of scripts | conversational, 2006 | unigram alternative | VERIFIED(web) |
| **SCOWL / Hunspell en_US** (LibreOffice `en_US.dic`) | MIT-like ("Permission to use, copy, modify, distribute and sell … granted without fee") | 49,568 stems + affixes (SCOWL size 60) | — | **Validity filter, casing, affix expansion**; no frequencies, no bigrams | VERIFIED(web); no `hunspell-en_us` installed locally (`/usr/share/hunspell` empty, VERIFIED(local)) |
| **AOSP `en_US_wordlist.combined.gz`** | NOTICE: Apache-2.0 **plus** "Includes Dictionaries © Lexiteria LLC. Used by permission." | 160,715 words with bigram (`f` 0–15 in binary, 0–255 in text) data, 2014 | general | Read the format and the *shape* (per-word successor lists); **don't redistribute** | VERIFIED(web) (NOTICE re-read this session) |
| **HeliBoard "experimental" en_US** (Helium314/aosp-dictionaries) | **CC BY 4.0** (built from Wikipedia + OpenSubtitles + Leipzig, hunspell-filtered) | 279,960 words (2024), `main_en_us.dict` 2.96 MB | blended | Permissive unigram (and possibly bigram — UNVERIFIED) source in `.combined` form | VERIFIED(web) for licence and counts |
| **onboard `en_US.lm`** | GPL-3+ (repo default stanza) | 42,635 unigrams, 6,398 bigrams (counts) | — | too thin; GPL | VERIFIED(web) |
| Leipzig Corpora | CC BY 4.0 (LOD page; primary site blocked) | per-size word lists 10 K–1 M | news/web | alternative unigrams | partly VERIFIED |
| SUBTLEX-US | "any purpose" per wordfreq README; openlexicon says CC BY-SA; no licence on the UGent page | 74,286 words / 51 M-word corpus | spoken (US film) | alternative unigrams | VERIFIED(web), inconsistent statements |
| Wikipedia dumps | CC BY-SA 4.0 + GFDL | — | encyclopedic | build your own bigrams | VERIFIED(web) |
| Tatoeba | CC BY 2.0 FR (attribution per sentence) | — | learner sentences | no | VERIFIED(web) |
| Reddit/Pushshift, OSCAR/Common Crawl, COCA, BNC | revoked / CC ToU with copyright caveats / commercial / non-commercial | — | — | **no** | VERIFIED(web) except COCA (site blocked) |

Packaging note on wordfreq-rs (VERIFIED(web), docs.rs): `wordfreq-model`
"downloads specified model files and embeds the models directly into the
source code" at build time, selected by feature flags such as `large-en` /
`small-en` ("There is no default feature"); the files come from the GitHub
`models-v1` release as zstd-compressed `word freq` text marked CC BY-SA 4.0.
A build-time network fetch is unfriendly to packaging, so for hyprpad the
list should be fetched once by the offline model builder (§8.3), not by
`cargo build` — i.e. use the release asset, not the crate.

### 4.2 Recommended phase-1 data recipe

1. **Vocabulary + validity**: SCOWL size 60 (via `en_US.dic/.aff`, expanded
   with `spellbook` at build time) ∪ the top ~60 k of wordfreq `large_en`.
   SCOWL supplies casing (proper nouns, "I") that lower-cased frequency lists
   lose.
2. **Unigram log-probs**: wordfreq `large_en` Zipf values (CC BY-SA 4.0). If
   share-alike on the data artifact is unwanted, substitute HeliBoard's CC BY
   4.0 experimental list or Google Books 1-grams (CC BY 3.0, bookish).
3. **Bigrams**: Google Books v3 English 2-grams (CC BY 3.0), years ≥ 1980,
   both words in vocabulary, top-K = 32 successors per word by count, counts
   quantised to 8-bit log scale. Register mismatch (books vs. chat) is real
   and is what the personal cache (§5) exists to fix. An OpenSubtitles-built
   bigram table would match register better but has the murkiest provenance.
4. **Shipped artifact**: one `en.model` file (§8.3), attribution text alongside
   it, licence = CC BY-SA 4.0 for the data (the code stays MIT/Apache-2). Size
   target ≤ 4 MB.

INFERRED throughout; the licences are VERIFIED(web) as tabulated.

### 4.3 Compact on-disk formats

- **Word list**: `fst::Map` (LOUDS-free FST, MIT/Unlicense), keys = words,
  value = word id; 80 k words ≈ 250–400 KB (INFERRED from BurntSushi's
  Wikipedia-title numbers: 15.8 M titles → 157 MB ≈ 10 B/key; short common
  words compress better). Prefix completion = a range scan; fuzzy = a
  `levenshtein` automaton intersection. `rsmarisa` (≈ 5.2 B/key on Wikipedia
  titles, VERIFIED(web)) is the fallback if size matters.
- **Unigram probs**: one `u8` per id (log-scale, AOSP-style 255 = p 1, each
  step ÷ 1.15) → 80 KB.
- **Bigrams**: per-word successor lists — `offsets[u32; V+1]` (320 KB) then
  for each word its successors as delta-varint ids + `u8` prob; 1 M bigrams
  ≈ 2.5–3 MB; capped at 32 successors/word → ≤ 2.56 M entries. AOSP's binary
  dictionary is itself a per-word successor list with 4-bit freqs
  (`FormatSpec.java`, `MAX_BIGRAMS_IN_A_PTNODE = 10000`; VERIFIED(web)), so
  the shape has production precedent; a fixed K = 32 cap is our choice
  (UNVERIFIED as an industry practice).
- **Total**: ≈ 3–3.5 MB, mmap-able, zero parse at startup. KenLM-style
  quantised tries only pay off at 4/5-gram scale.

---

## 5. Personalisation and privacy

### 5.1 How keyboards learn

- **AOSP LatinIME** (VERIFIED(web)): `UserHistoryDictionary` stores unigrams
  and n-gram context with timestamps; `forgetting_curve_utils.cpp`:
  `MAX_LEVEL 15`, `MIN_VISIBLE_LEVEL 2`, level rises after
  `OCCURRENCES_TO_RAISE_THE_LEVEL = 1` more sighting, decays over
  `DURATION_TO_LOWER_THE_LEVEL = 15 days` in 32 steps, `DECAY_INTERVAL 2 h`,
  level-0 entries discarded after 30 idle steps. Net effect: a word must be
  seen twice before it surfaces and fades if unused for weeks. Learning is
  skipped for 0-frequency (profanity) words and when the field disallows
  auto-correct.
- **Presage**: `recencyPredictor` (exponentially decayed recent words) and
  `dejavuPredictor` (recent *phrases*) layered over the static n-gram DB —
  the same "cache LM" idea in another shape.
- **Gboard/SwiftKey/iOS** describe on-device learning, a "delete learned
  words" reset, and (SwiftKey, VERIFIED(web)) "does not learn anything from
  fields marked as password fields … nor … long numbers such as credit card
  numbers", with the caveat that a "show password" toggle can un-mark a field.

### 5.2 Not learning passwords — how it is actually done

- **Android**: `InputTypeUtils.isPasswordInputType` (TEXT_PASSWORD /
  VISIBLE_PASSWORD / WEB_PASSWORD / NUMBER_PASSWORD) → suggestions suppressed
  → no learning; `EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING` (Chromium
  sets it for incognito; Firefox for private browsing). AOSP itself never
  reads that flag; HeliBoard adds `mIncognitoModeEnabled = always || noLearning
  || isPasswordField` (VERIFIED(web)).
- **Wayland**: `zwp_text_input_v3.set_content_type(hint, purpose)` with
  `content_hint` bits `hidden_text 0x40` ("characters should be hidden") and
  `sensitive_data 0x80` ("typed text should not be stored"), and
  `content_purpose` `password 8`, `pin 9`, `terminal 13` (VERIFIED(web), wayland.app).
  Who sets what (VERIFIED(web) from toolkit source):

  | Toolkit | purpose=password? | Notes |
  |---|---|---|
  | GTK4 | yes | `GtkPasswordEntry` sets purpose PASSWORD; IM context adds `HIDDEN_TEXT\|SENSITIVE_DATA` |
  | GTK3 | only if the app set `input-purpose` | `visibility=FALSE` alone sends purpose *normal* |
  | Qt6 | **no purpose**, hints only | `ImhHiddenText→HIDDEN_TEXT`, `ImhSensitiveData→SENSITIVE_DATA`; `QLineEdit` echo modes set both |
  | Chromium/Electron | yes | `InputTypeToContentPurpose` PASSWORD; `HAS_BEEN_PASSWORD` → hidden+sensitive; text-input-v3 default-on since 2025-04 |
  | Firefox | yes via GTK3 | `IMContextWrapper` maps password/email/url/tel |
  | SDL3 | yes | `TEXT_PASSWORD_HIDDEN → PASSWORD + hidden+sensitive`; `TEXT_USERNAME → NORMAL + sensitive` |
  | Steam client, XWayland, Proton | no text-input at all | nothing to read |

  **Rule**: treat `purpose ∈ {password, pin}` **or** `hint & (sensitive_data |
  hidden_text)` as *no suggestions, no learning* — Qt only sends the hints.
- **Linux OSKs are weak here** (VERIFIED(web)): onboard's AT-SPI
  `DomainPassword` only silences key feedback and still records insertions;
  ibus-typing-booster has an "off the record" toggle and "disable in terminals"
  by purpose/window title, and an old issue about terminal passwords landing
  in `user.db`. Do better.

### 5.3 Rules for hyprpad (INFERRED, with the precedent named)

1. Never learn when content-type says password/PIN/sensitive/hidden
   (SwiftKey, HeliBoard, Chromium) — phase 2, needs IM-v2 or the compositor
   channel (§6.6).
2. **Daemon-side deny list by focused window** — hyprpad's mode engine
   already resolves the focused window class/title and the process inside it
   (`docs/13-modality-design.md`), so `learn = false` for classes matching
   password managers (`1Password`, `KeePassXC`, `Bitwarden`), polkit agents,
   lock screens, and — by default — terminals (typing-booster precedent);
   the daemon sends `learn off|on` over the control channel on every focus
   change. This works in phase 1 with no protocol support at all.
3. Never learn while the OSK was raised over the lock screen (`above_lock`
   path) — precedent: none found, obvious.
4. Never learn tokens containing digits or ≥ 2 symbol characters, or longer
   than 24 characters (SwiftKey's "long numbers" rule generalised); never
   learn a token that was typed with the symbols layer active for > half its
   characters.
5. Surface a learned word only after **two** sightings on different
   `show()` sessions (AOSP `MIN_VISIBLE_LEVEL 2`); decay with AOSP's curve.
6. Learn only on a **completed** word (separator or accepted candidate), and
   in terminals (if allowed at all) only on Enter (onboard `DomainTerminal`).
7. An explicit **incognito** toggle key on the OSK and a `learn off` control
   command; a `forget <word>` command; a "delete learned words" action that
   removes the file.
8. Storage: `$XDG_DATA_HOME/hyprpad/osk/personal-<lang>.bin`, mode 0600, an
   append-only log of `(word_id_or_text, prev_word, timestamp)` compacted on
   load into two maps (unigram cache, bigram cache) with levels; never
   uploaded anywhere. Cap at e.g. 20 k entries with level-0 eviction.

---

## 6. The Wayland question

### 6.1 The protocols

- **`zwp_input_method_v2`** (wlroots protocol; VERIFIED(web), wayland.app):
  one IME per seat; `unavailable` if the slot is taken; `activate`/`deactivate`
  bracket a text field; `surrounding_text(text, cursor, anchor)` (byte
  offsets, ≤ 4000 bytes, only if the app sent it), `text_change_cause`,
  `content_type(hint, purpose)`, all double-buffered behind `done`;
  IME → app: `commit_string`, `set_preedit_string`,
  `delete_surrounding_text(before, after)` (byte lengths — impossible to
  compute without surrounding text; Squeekboard calls this "a bug in the
  protocol" and falls back to Backspace keycodes), applied by `commit(serial)`
  where serial = number of `done`s received. `grab_keyboard` swallows all key
  events after forwarding them to the grab holder.
- **`zwp_text_input_v3`** (VERIFIED(web)): app ↔ compositor half —
  `enable`, `set_surrounding_text`, `set_content_type`, `set_cursor_rectangle`,
  `commit`; events `preedit_string`, `commit_string`,
  `delete_surrounding_text`, `done`. The compositor relays between the two.

### 6.2 HypXRland status — VERIFIED(local)

`ProtocolManager.cpp:186,193` advertises `zwp_text_input_manager_v3` v1 and
`zwp_input_method_manager_v2` v1 (plus text-input-v1; **no v2/v4**, so Qt < 6.7
gets nothing). Relevant behaviour read from the source:

- `CTextInput::commitStateToIME` (`TextInput.cpp:235-256`) forwards
  `surroundingText` (when updated), `textChangeCause`, `textContentType(hint,
  purpose)` (when updated), then `done()` — so the IME **does** receive
  password/PIN/sensitive hints from any text-input-v3 client that sets them.
- `updateIMEState` (`TextInput.cpp:263-272`) relays `preeditString`,
  `commitString`, **`deleteSurroundingText(before, after)`** and `sendDone()`
  to v3 clients (v1 gets an arithmetic emulation).
- `InputMethodRelay::onKeyboardFocus` (`InputMethodRelay.cpp:130-160`): on
  every keyboard-focus change all text inputs `leave()`, and `CTextInput::leave`
  ends with `deactivateIME(this)` (default `shouldCommit = true`, so a `done`
  follows); then only v3 text inputs **owned by the newly focused client** get
  `enter`. Consequence: focusing an XWayland window, a game, or any client
  without a text-input object yields `deactivate` — a reliable "no text field
  here, fall back to uinput" signal. `activate` only arrives after the client
  `enable`s.
- `onNewIME` rejects a second IME with `unavailable` (documented in
  `osk-technology.md` §7). **fcitx5 5.1.21 is running on this machine right
  now** (`ps`: `/usr/bin/fcitx5 --disable notificationitem`), so on the dev
  box the OSK would get `unavailable` today.

### 6.3 Which apps would actually give us context — VERIFIED(web)

| Client | text-input-v3 | surrounding text | content-type |
|---|---|---|---|
| GTK3 ≥ 3.24, GTK4 | yes (built-in Wayland IM module, 4000-byte window) | yes | yes (purpose only if the app sets it on GTK3) |
| Firefox | via GTK3 | yes | yes |
| Qt ≥ 6.7 (6.8+ recommended); Qt5 / < 6.7 | yes / **no** (v2 only) | yes | hints only, no purpose |
| Chromium / Electron | yes, default since 2025-04-10 (`WaylandTextInputV3`), older Electron may need `--enable-wayland-ime` / `--wayland-text-input-version=3` (UNVERIFIED per Electron version); known `enable`-before-`enter` bugs vs sway | yes | yes |
| SDL2/SDL3 games | only while the game calls `SDL_StartTextInput` — most never do | **never** (`set_surrounding_text` not implemented) | yes (SDL3 purposes) |
| foot, kitty (`wayland_enable_ime`), wezterm, Alacritty | yes | none (no editable buffer) | `terminal` (kitty) |
| Steam client (CEF, XWayland), Proton/wine games, any X11 app | **none** | — | — |

So IM-v2 context covers browsers, Electron chat apps, GTK/Qt6 desktop apps
— i.e. the desktop half of the living-room use case — and covers nothing in
the game half, which is where the OSK is most needed. This is the same split
`osk-technology.md` §3.1 found for injection.

### 6.4 Coexistence: IM-v2 object + uinput in one process

- Holding an `zwp_input_method_v2` object **without** `grab_keyboard` is
  safe: the IME only receives context events; the uinput device is an ordinary
  seat keyboard. (INFERRED from the protocol + Hyprland routing; consistent
  with how wvkbd `--auto` behaves.)
- **Do not grab.** With a grab held, key events — including those from the
  OSK's own uinput device, which is a kernel evdev keyboard like any other —
  are routed to the grab holder instead of the app (`osk-technology.md` §3.3
  A; field report in omarchy#7346 of wtype losing Shift releases under an IME
  grab on Hyprland 0.56.2, VERIFIED(web)). A grab also captures the user's
  physical keyboard while the OSK is shown.
- **fcitx5 owns the slot on this machine.** Options: (a) opportunistic bind,
  degrade on `unavailable` (wvkbd/Squeekboard pattern; loses all context when
  fcitx5 is running); (b) tell users to stop fcitx5 for the living-room
  session (Omarchy installs it; acceptable for a gaming session, not for a CJK
  user); (c) compositor-side change (§6.6).
- With `deactivate` as the fallback trigger, the OSK can keep **one policy**:
  while `active`, commit candidates with `commit_string` and delete with
  `delete_surrounding_text`; otherwise type/backspace via uinput. That is
  exactly Squeekboard's `submission.rs` logic (VERIFIED(web)).

### 6.5 Alternatives for context without IM-v2

- **AT-SPI2**: `Atspi.Text.get_text/get_caret_offset` on the focused
  accessible gives surrounding text for GTK, Qt and Chromium/Electron (the
  latter only when accessibility is detected or forced with
  `--force-renderer-accessibility`), with D-Bus round-trip latency and no
  coverage for SDL/wine/XWayland-only apps (VERIFIED(web) for the API and
  Chromium flags; latency UNVERIFIED). It does supply the `PASSWORD_TEXT`
  role (onboard uses it). Heavier and less reliable than IM-v2; a possible
  phase-2b for the fcitx5-present case, not a foundation.
- Clipboard / `wl_data_device`: no.

### 6.6 Compositor-side options (the owner authored HypXRland)

Because the fork is the owner's, the "one IME" constraint is negotiable
(INFERRED design options; none exist upstream):

1. **Content-type side channel.** HypXRland already holds
   `m_current.contentType` for the focused text input. Expose "focused
   text-input purpose/hint changed" as an event on the Hyprland IPC event
   socket (or a `hyprctl` query). The daemon (which already talks to Hyprland
   for focus) forwards `learn off` / `predict off` to the OSK. Zero protocol
   surface, works while fcitx5 owns the IME, ~50 lines of C++. **Cheapest,
   highest-value fork change.**
2. **Observer IME.** Allow a second `zwp_input_method_v2` bound as read-only
   observer (receives `activate/deactivate/content_type/surrounding_text`,
   ignores its requests). Non-standard; clients must not rely on it.
3. **Commit relay.** A Hyprland-private request (or reuse of
   `hyprctl`) to `commit_string` into the focused text-input on behalf of a
   non-IME client — gives clean Unicode commits without the IME slot. Same
   silent-drop caveat for non-text-input clients.

### 6.7 Compare: stay on uinput + local context vs. move to IM-v2

| | uinput + local context (phase 1) | IM-v2 opportunistic (phase 2) |
|---|---|---|
| Works in games / XWayland / Steam / gamescope | yes | no (falls back to uinput anyway) |
| Knows what is in the field before show() | no | yes for text-input-v3 apps |
| Knows the field is a password | only via the daemon's window deny-list | yes (GTK4/Chromium/Firefox/SDL3 purpose; Qt hints) |
| Whole-word commit | keystroke replay, ASCII only, ≈ 12 ms/char today | `commit_string` UTF-8 atomically |
| Replace a mistyped word | N × Backspace + retype (only for words the OSK typed) | `delete_surrounding_text` + `commit_string` |
| Conflicts with fcitx5 | no | yes (`unavailable`) unless §6.6 |
| Effort | model + strip only | + protocol binding, state machine, two commit paths, tests per toolkit |

Conclusion: phase 1 on uinput is not a compromise for the game use case — it
is the only thing that works there. IM-v2 is additive.

---

## 7. UX

### 7.1 Steam Deck and other controller keyboards — VERIFIED

- **Steam Deck OSK has no prediction, completion or autocorrect.**
  VERIFIED(local): the current bundle (`chunk~2dcc5aaf7.js`) has 208
  `VirtualKeyboard` references and zero `WordSuggest|TextPrediction|
  Autocorrect|Predictive|SuggestedWords` identifiers; its only `Candidate*`
  symbols belong to the CJK IME path (navigated with L1/R1, `osk-technology.md`
  §4.6). VERIFIED(web): Steam Community threads from 2022–2024 asking for
  word suggestions confirm absence; no SteamOS 3.6–3.8 note adds it. The old
  Big Picture "Daisywheel" never had prediction (UNVERIFIED).
- **PS5**: predictive row above the keys; "push R1 when you see the correct
  word" (VERIFIED(web), Push Square).
- **Xbox**: suggestions above the top row; "use the right stick to quickly
  select a suggestion … or select suggestions using the left stick or d-pad"
  (VERIFIED(web) snippet).
- **Windows 11 gamepad keyboard**: X = Backspace, Y = Space, LB/RB move the
  cursor, Menu = Enter (VERIFIED(web) snippet) — note the X/Y assignment
  matches hyprpad's shipped `[osk_buttons]`.
- **Gboard**: three candidates, "the center position … is reserved for the
  highest-probability candidate" (VERIFIED(web), Hard 2018 Fig. 1); Gboard
  for Android TV keeps a suggestion row above a floating QWERTY.
- **Nintendo Switch**: remembers entered text and shows predictive
  suggestions (VERIFIED(web), Nintendo support).

Pattern: **one shoulder button accepts the top candidate; a stick/d-pad
reaches the rest.** Nobody makes you point at the strip.

### 7.2 Proposed selection scheme for two pads + face buttons + triggers

Free inputs while the OSK is up (VERIFIED(local), §1): L1/R1, L4/L5/R4/R5,
both sticks, d-pad, View, L3/R3.

- **Strip**: 3 candidates (5 in Split mode's wider columns is possible but
  Gboard/phones settled on 3; Trnka used 5 with an enforced scan). Bottom
  mode: a row above the top key row spanning the panel. Split mode: the same
  three candidates repeated at the top of **both** columns so either thumb's
  region contains them.
- **Highlight**: the best candidate is highlighted by default (centre slot,
  Gboard convention; or leftmost — pick one and keep it stable).
- **R1 = accept highlighted candidate** (PS5's R1). Types the missing suffix
  + a trailing space (uinput) or `commit_string` (IM-v2). After a
  next-word prediction is accepted, the strip immediately shows next-word
  predictions again, so R1-R1-R1 chains phrases.
- **L1 = move highlight to the next candidate** (wraps). Optional: L4/L5
  grips = previous/next. R4/R5 stay free.
- **Pads still work**: the strip is part of the surface, so a pad cursor can
  hover a candidate (haptic tick on crossing, as for keys) and click/trigger
  to accept it. This is the discoverable path; R1/L1 is the fast path.
- **Backspace after accept** removes the auto-inserted space first (Gboard
  behaviour, UNVERIFIED as documented; INFERRED as expected) — only
  implementable because the OSK knows it just typed that space.
- **Haptics**: a light tick when the highlighted candidate changes; the
  existing `Commit` pulse on accept (`event candidate <n>` on the stdout
  back-channel, daemon fires the actuator).
- **Config**: expose `osk_button("r1", h.osk.accept)` / `h.osk.next` in the
  Lua config so the scheme is rebindable like Y/X today.

### 7.3 Latency budget

- Human/keyboard targets: "users typically expect a visible keyboard response
  within 20 milliseconds" (Hard 2018; restated in 2410.15575); Nielsen's
  0.1 s "feels instantaneous" bound (VERIFIED(web)).
- The OSK already coalesces ~250 Hz cursor updates into ≤ 165 Hz draws
  (README "Frame pacing", VERIFIED(local)); candidate updates happen only on
  *commits* (a few Hz), never on cursor motion, so prediction never competes
  with cursor rendering.
- Costs: FST prefix range + successor-list merge ≈ tens of µs; KenLM-class
  n-gram queries ≈ 0.5 µs each; a ~1 M-param GRU ≈ < 1 ms on x86 (INFERRED);
  a 2 M-param LSTM on a 2018 phone ≈ 22–33 ms (VERIFIED(web)); a 135 M LLM
  prompt-eval ≈ 20–100 ms on CPU (UNVERIFIED).
- Budget: **synchronous ≤ 1 ms** on the poll thread; anything else on a
  worker thread with the result applied only if the composed prefix is
  unchanged. Keystroke replay for an accepted 8-letter completion is ~100 ms
  at today's 6 ms sleeps — consider a burst mode with 1–2 ms gaps for
  `type` (INFERRED; the 6 ms was chosen for XWayland safety, re-test).

### 7.4 When to hide the strip

Trnka/Koester (§2.6): a bad list is worse than none. Hide (or grey) the strip
when the best candidate's score is below a floor, when the field is
password/sensitive, when the symbols layer is active, and when the user has
turned prediction off; show it empty rather than shifting the key rows
(layout stability matters more for absolute-cursor targeting than the strip
does).

---

## 8. Proposed architecture for `hyprpad-osk`

### 8.1 Module boundaries (new modules in `osk/src/`)

| Module | Responsibility |
|---|---|
| `predict::model` | mmap'd static model: `fst::Map` lexicon, unigram `u8` probs, successor lists; `complete(prefix, ctx) -> Vec<Cand>`, `next(ctx) -> Vec<Cand>`, `correct(word, ctx)`. Pure, `#[no_std]`-ish, unit-tested against a fixture model. |
| `predict::personal` | the learned cache: unigram/bigram levels with the AOSP forgetting curve, load/compact/save, `learn(word, prev)`, `forget(word)`, `score_boost(word, prev)`. |
| `predict::rank` | one score per candidate: `log P_bigram(w\|prev)` with stupid backoff → unigram, + personal boost, − edit cost for corrections, + exact-match promotion; typed word always candidate 0; returns top-3. |
| `compose` | the **composition buffer**: current word prefix, previous 2 committed words, whether the last thing typed was an auto-space, a "dirty since focus change" flag. Fed by every commit the OSK performs (key, `key`, `type`, candidate accept), reset on `show`, `hide`, `context reset`. |
| `strip` (in `layout`/`render`) | the candidate row as placed rects with `Hand::Either`, hit-tested like keys, highlighted like keys; drawn on the existing dirty path. |
| `inject` (refactor of `output`) | two backends behind one trait: `Uinput` (today) and, phase 2, `ImV2` (`commit_string`/`delete_surrounding_text`); the app picks per commit by `active` state. |
| `control` additions | `predict on\|off`, `learn on\|off`, `forget <word>`, `context reset`, `accept [n]`, `next`, `prev`; events `event candidate <n> <text>` and `event learned <word>` (optional, for debugging). |

The daemon (`src/osk.rs`, `src/run.rs`) gains: R1/L1 (configurable) →
`accept`/`next`; `learn off|on` on focus change from the mode engine's window
deny-list; `context reset` on focus change.

### 8.2 Data flow

```
pad click / trigger / osk_button
  └─ Osk::commit_key_index  ──► inject.tap(keycode)          (unchanged)
        │
        └─ compose.push(char | separator | backspace)
              └─ predict::rank(model, personal, compose)  ≤ 1 ms
                    └─ strip.set(candidates)  ──► mark_all_dirty ──► frame-paced draw
                    └─ emit "event candidate 0 <text>" (haptic tick on change)

R1 (daemon: `accept`)
  └─ strip.highlighted → text
        ├─ uinput backend: type(suffix + ' ')       (phase 1; ASCII only, sleeps)
        └─ IM-v2 backend, if active: commit_string(word + ' ') + commit(serial)
        └─ compose.accept(word) → personal.learn(word, prev) if learn_enabled
        └─ rank again with empty prefix → next-word candidates

separator typed (space/enter/punct)
  └─ compose.finish_word → personal.learn(word, prev) if learn_enabled && passes §5.3 filters
```

### 8.3 On-disk formats

- **`en.model`** (built offline by a new `tools/osk-model-build` Rust binary,
  reproducible from the data URLs in §4; committed as a release asset, not in
  git): header (magic, version, language, vocab size, build attribution
  string) · FST bytes · `u8[V]` unigram probs · `u32[V+1]` successor offsets ·
  successor byte stream. mmap and use in place (`fst::Map::new(&bytes[..])`).
- **`personal-en.bin`**: versioned little-endian record log; compact on
  save; 0600.
- **Model selection**: `--model <path>` and `$HYPRPAD_OSK_MODEL`; default
  `$XDG_DATA_DIRS/hyprpad/osk/en.model`. No model → prediction silently off,
  the OSK behaves exactly as today.

### 8.4 Memory / CPU budget (starting points)

- Model 3–4 MB mmap'd (page cache, shared); personal cache < 1 MB; candidate
  scratch < 100 KB. Total RSS growth ≤ 5 MB.
- Per commit: prefix range scan over the FST bounded to the first 64 hits ∪
  the previous word's ≤ 32 successors ∪ personal cache hits → ≤ ~100
  candidates scored in the tens of µs. Levenshtein-1 automaton only when the
  prefix has < 3 completions (INFERRED; measure).
- No new threads in phase 1. Phase 3's neural rerank gets one worker thread
  and a 16 ms deadline.

### 8.5 Password / sensitive-field handling (layered; §5.3)

1. Phase 1: daemon deny-list by window class/process → `learn off` and,
   optionally, `predict off` (no strip in password managers at all).
2. Phase 2: IM-v2 `content_type` → `predict off`+`learn off` on
   password/pin/sensitive/hidden; `terminal` → `learn off` (configurable).
3. Always: token filters (digits, symbols, length, symbols-layer share),
   two-sighting threshold, incognito key, `forget`, delete-all.
4. Never learn from `type <text>` sent by the daemon (that is scripted
   input, e.g. a future paste/dictation path), only from user commits.

### 8.6 Correction policy

- Candidate 0 is always the literal typed prefix/word (AOSP rule).
- Corrections appear as candidates only; **no auto-replace on space** unless
  the user enables `autocorrect = "modest"` (threshold ≈ AOSP's 0.185 on a
  normalised score) — and only for words the OSK itself typed in this session
  (uinput can only reliably delete what it typed).
- Neighbour-key substitutions are discounted using the layout's own geometry
  (the `LayoutEngine` knows every key rect), so "wrong adjacent key" — the
  trackpad's characteristic error — is cheap; transposition and omission use
  AOSP's relative costs.

---

## 9. Phased plan

**Phase 0 — spike (days).** `tools/osk-model-build`: download wordfreq
`large_en` + SCOWL + Google Books eng 2-grams (subset), build `en.model`,
print size; a `bench` that replays a sample chat/README corpus and reports
top-3 next-word hit rate and keystroke savings for completion. Go/no-go:
≥ 20 % top-3 next-word, ≥ 35 % KSS on prose, ≤ 4 MB, ≤ 1 ms per query. This
is also where the register question (books vs. chat) is measured, not argued.

**Phase 1 — shippable (uinput + local context).** `predict::{model,
personal, rank}`, `compose`, the strip in both modes, `accept/next/prev`
control commands, R1/L1 bindings in the daemon with Lua config hooks, personal
cache with AOSP decay and the §5.3 filters, daemon-side deny-list → `learn
off`, incognito key, `forget`, `predict on|off`. Real font for the strip if
`font8x8` proves too cramped. Tests: rank determinism on a fixture model,
personal-cache decay, the composition buffer under backspace/auto-space, and
a live check that an accepted completion lands correctly in a native app, an
XWayland app and a gamescope window (the same three targets as
`osk-technology.md`'s injection tests).

**Phase 2 — context and clean commits.** Opportunistic
`zwp_input_method_v2` binding (no grab), `unavailable` fallback,
`content_type` gating, `surrounding_text` → seed `compose` on `activate`
(previous two words before the cursor), `commit_string`/`delete_surrounding_text`
backend while active, uinput otherwise. In HypXRland: the content-type IPC
event (§6.6 option 1) so gating works with fcitx5 present. Per-toolkit
verification matrix (GTK4 entry, GtkPasswordEntry, Qt6 QLineEdit password,
Chromium URL bar + password field, Firefox, foot, SDL3 sample, an XWayland
app). Consider AT-SPI only if fcitx5 coexistence turns out to matter to
users.

**Phase 3 — quality.** Trigram successor lists (or a KenLM 4-gram via
`kenlm-rs` if LGPL is acceptable) if phase-0 numbers plateau; a ~1 M-param
GRU rerank in `candle`/`tract` trained on the same data (async, 16 ms
deadline); en_GB/de/fr/es models from the same build tool; emoji candidates
(`en_emoji`-style list); Unicode commits via `zwp_virtual_keyboard_v1` keymap
generation (already deferred in `osk-technology.md` §3.2) so non-ASCII
candidates can be typed without IM-v2. A SmolLM2-class LLM rerank is an
experiment, not a plan.

---

## 10. Open questions

1. **Data licence choice** — is CC BY-SA on the shipped `en.model` acceptable
   (wordfreq / OpenSubtitles lists), or should the model be built only from
   CC BY sources (Google Books 1/2-grams, HeliBoard's CC BY 4.0 list, Leipzig)
   at some cost in chat-register coverage? Norvig's and AOSP's lists are out
   regardless.
2. **Register** — how badly do book bigrams mismatch living-room typing
   (search boxes, chat, URLs)? Phase 0 measures it; the personal cache may or
   may not close the gap.
3. **fcitx5 policy** — accept "no IM-v2 context while fcitx5 runs", or make
   the HypXRland IPC event (§6.6) the primary path and treat IM-v2 as
   optional? The IPC route also sidesteps the one-IME rule entirely.
4. **Chromium/Electron on this stack** — text-input-v3 is default-on
   upstream since 2025-04, but the `enable`-before-`enter` ordering bug seen
   with sway may also bite Hyprland; needs a live test with an Electron app
   and with Chromium's password field.
5. **Keystroke-replay speed** — can the 6 ms `tap` sleeps be cut for bursts
   without breaking XWayland/Electron delivery (the reason they exist)?
6. **Split-mode strip placement** — duplicated at the top of both columns
   (proposed) vs. one strip on the side of the last-used pad.
7. **Auto-space after accept** — phone convention, but in terminals, URL
   bars and search boxes it is often wrong; content-type (`url`, `terminal`)
   can gate it in phase 2, phase 1 needs a heuristic or a config knob.
8. **Highlight position** — centre-best (Gboard) vs. leftmost-best (reads
   naturally with L1 = next). Pick by trial.
9. **Neural at all?** — Gboard's own numbers say +3.5 points top-1 for the
    LSTM over n-grams; on a keyboard whose bottleneck is thumb travel, the
    phase-0 KSS measurement should decide whether phase 3 is worth doing.

---

## Sources

### Repo / local (VERIFIED(local))
- `osk/README.md`, `osk/src/app.rs` (commit path 446-530), `osk/src/output.rs` (`tap`, `key_for`), `osk/src/control.rs`, `src/osk.rs`, `src/run.rs` (`route_osk` 2059-2136), `config/hyprpad.lua:151-152`, `docs/13-modality-design.md`, `docs/research/osk-technology.md` §§1, 3, 4.6, 7.
- HypXRland `/home/ajg/code/hypxrland` @ `ba5e361f8`: `src/managers/ProtocolManager.cpp:186,193`, `src/managers/input/InputMethodRelay.{cpp,hpp}`, `src/managers/input/TextInput.cpp`, `src/protocols/TextInputV3.cpp`, `src/protocols/InputMethodV2.cpp`.
- `~/.local/share/Steam/steamui/chunk~2dcc5aaf7.js` identifier counts; `pacman -Q` (fcitx5 5.1.21, enchant, hunspell, no hunspell dictionaries); `/usr/share/fcitx5/spell/en_dict.fscd`; `cargo info` for the crates listed in §3.

### Models and decoding
- Chen & Goodman, *An Empirical Study of Smoothing Techniques for Language Modeling* (1998/1999): <https://dash.harvard.edu/handle/1/25104739>
- Heafield, *KenLM: Faster and Smaller Language Model Queries* (WMT 2011): <https://aclanthology.org/W11-2123/> · data structures / quantisation: <https://kheafield.com/code/kenlm/structures/> · estimation: <https://kheafield.com/code/kenlm/estimation/> · paper PDF (query timings): <https://kheafield.com/papers/avenue/kenlm.pdf>
- Heafield et al., *Scalable Modified Kneser-Ney LM Estimation* (ACL 2013): <https://aclanthology.org/P13-2121/>
- Pauls & Klein, *Faster and Smaller N-Gram Language Models* (ACL 2011): <https://aclanthology.org/P11-1027/>
- Brants et al., *Large Language Models in Machine Translation* (stupid backoff, 2007): <https://aclanthology.org/D07-1090/>
- Cleary & Witten 1984 (PPM): <https://www.semanticscholar.org/paper/50e2733d2a8a9b3929bda278d382c37711f3fa8e>
- Ward, Blackwell, MacKay, *Dasher* (UIST 2000): <https://dl.acm.org/doi/10.1145/354401.354427> · Dasher PPM source: <https://raw.githubusercontent.com/dasher-project/dasher/master/Src/DasherCore/LanguageModelling/PPMLanguageModel.cpp> · parameters: <https://raw.githubusercontent.com/dasher-project/dasher/master/Src/DasherCore/Parameters.cpp> · DasherCore (MIT): <https://github.com/dasher-project/DasherCore>
- Google Research, *The Machine Intelligence Behind Gboard* (2017): <https://research.google/blog/the-machine-intelligence-behind-gboard/>
- Ouyang et al., *Mobile Keyboard Input Decoding with FSTs* (2017): <https://arxiv.org/abs/1704.03987>
- Hard et al., *Federated Learning for Mobile Keyboard Prediction* (2018): <https://ar5iv.labs.arxiv.org/html/1811.03604>
- Chen et al., *Federated Learning of N-gram Language Models* (2019): <https://ar5iv.labs.arxiv.org/html/1910.03432>
- Chen et al., *Federated Learning of Out-of-Vocabulary Words* (2019): <https://arxiv.org/abs/1903.10635>
- Xu et al., *Federated Learning of Gboard LMs with Differential Privacy* (2023): <https://arxiv.org/abs/2305.18465>
- Zhang et al., *Neural Search Space in Gboard Decoder* (2024): <https://arxiv.org/html/2410.15575>
- Liu et al., *Proofread* (2024): <https://arxiv.org/abs/2406.04523>
- Apple iOS 17 press release (transformer autocorrect): <https://www.apple.com/newsroom/2023/06/ios-17-makes-iphone-more-personal-and-intuitive/> · Jack Cook's reverse engineering: <https://jackcook.com/2023/09/08/predictive-text.html> · Apple, *Learning with Privacy at Scale*: <https://machinelearning.apple.com/research/learning-with-privacy-at-scale>
- SwiftKey neural (2016): <https://www.droid-life.com/2016/09/15/swiftkey-neural-network/> · Microsoft transformer keyboard paper (2024/25): <https://arxiv.org/html/2505.05648>
- Yu et al. (Samsung), *An Embedded Deep Learning based Word Prediction* (2017): <https://ar5iv.labs.arxiv.org/html/1707.01662>
- Desai et al., MLSys 2020 on-device LM latency: <https://arxiv.org/pdf/2002.01535>
- Goodman et al., *Language modeling for soft keyboards* (2002): <https://www.microsoft.com/en-us/research/publication/language-modeling-for-soft-keyboards/>
- Bi, Ouyang, Zhai, *Both Complete and Correct?* (CHI 2014): DOI 10.1145/2556288.2557414
- Vertanen et al., *VelociTap* (CHI 2015): <https://digitalcommons.mtu.edu/michigantech-p/1126/>
- Fowler et al., *Effects of Language Modeling and its Personalization on Touchscreen Typing* (CHI 2015): <https://research.google/pubs/effects-of-language-modeling-and-its-personalization-on-touchscreen-typing-performance/>
- Ghosh & Kristensson 2017: <https://ar5iv.labs.arxiv.org/html/1709.06429>
- Adhikary & Vertanen, Interspeech 2023 (PPM/RNN personalisation): <https://www.isca-archive.org/interspeech_2023/adhikary23_interspeech.html>
- Gaines & Vertanen 2025 (n-gram vs LLM for AAC): <https://arxiv.org/html/2501.10582>
- Zeng et al. 2023 (eye typing with prediction): <https://arxiv.org/html/2312.08731v1>
- Alharbi, Stuerzlinger, Putze 2020 (cost of autocorrect/prediction): DOI 10.1145/3427311
- Palin et al., *How do people type on mobile devices?* (2019): <https://userinterfaces.aalto.fi/typing37k/>
- Trnka et al. 2008 (word prediction with an on-screen keyboard): <https://www.eecis.udel.edu/~mccoy/publications/2008/trnka08at.pdf> · Trnka & McCoy, KSS bounds (ACL 2008): <https://aclanthology.org/P08-2066.pdf>
- Nel, Kristensson, MacKay 2017 (summarises Koester & Levine 1996, Koester & Simpson 2014): <https://arxiv.org/pdf/1712.10073>
- Google jslm (PPM in JS): <https://github.com/google-research/google-research/tree/master/jslm>

### AOSP LatinIME (all `android.googlesource.com/platform/packages/inputmethods/LatinIME/+/refs/heads/main/`)
- `NOTICE` (Apache-2.0 + "Includes Dictionaries © Lexiteria LLC. Used by permission.")
- `dictionaries/sample.combined` (text format) · `dictionaries/` listing · `java/res/raw/main_en.dict`
- `java/src/com/android/inputmethod/latin/Suggest.java` · `utils/AutoCorrectionUtils.java` · `utils/InputTypeUtils.java` · `InputAttributes.java` · `settings/SettingsValues.java` · `inputlogic/InputLogic.java` · `DictionaryFacilitatorImpl.java` · `personalization/UserHistoryDictionary.java` · `makedict/FormatSpec.java`
- `native/jni/src/suggest/policyimpl/typing/scoring_params.cpp` · `typing_weighting.h` · `typing_scoring.h` · `native/jni/src/utils/autocorrection_threshold_utils.cpp` · `native/jni/src/dictionary/utils/forgetting_curve_utils.cpp`
- `jb-release/java/res/values/config.xml` (historical thresholds)
- `EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING`: <https://android.googlesource.com/platform/frameworks/base/+/refs/heads/main/core/java/android/view/inputmethod/EditorInfo.java>
- HeliBoard incognito handling: <https://raw.githubusercontent.com/Helium314/HeliBoard/main/app/src/main/java/helium314/keyboard/latin/InputAttributes.java> · dictionaries + licences: <https://codeberg.org/Helium314/aosp-dictionaries/raw/branch/main/README.md>
- Chromium Android incognito flag: <https://chromium.googlesource.com/chromium/src/+/refs/heads/main/content/public/android/java/src/org/chromium/content/browser/input/ImeAdapterImpl.java> · Firefox bug 1266683: <https://bugzilla.mozilla.org/show_bug.cgi?id=1266683>
- Firefox OS Gaia Latin IME: <https://wiki.mozilla.org/Gaia/System/Keyboard/IME/Latin/Dictionary_Blob> · <https://wiki.mozilla.org/Gaia/System/Keyboard/IME/Latin/Prediction_&_Auto_Correction>

### Components
- Presage: <https://presage.sourceforge.io/> · files: <https://sourceforge.net/projects/presage/files/> · DB connector: <https://raw.githubusercontent.com/Manouchehri/presage/master/src/lib/predictors/dbconnector/databaseConnector.cpp> · Debian tracker: <https://tracker.debian.org/pkg/presage> · Sailfish fork: <https://github.com/sailfish-keyboard/presage> · Maliit removal: <https://github.com/maliit/keyboard/pull/146> · phosh-osk-stevia completion post: <https://phosh.mobi/posts/osk-completion/>
- KenLM: <https://github.com/kpu/kenlm> · `kenlm-rs`: <https://github.com/RustedBytes/kenlm-rs> · Malay small-model sizes: <https://malaya.readthedocs.io/en/stable/load-kenlm.html>
- libime: <https://github.com/fcitx/libime> · `prediction.h`: <https://raw.githubusercontent.com/fcitx/libime/master/src/libime/core/prediction.h>
- fcitx5 keyboard hint options: <https://raw.githubusercontent.com/fcitx/fcitx5/master/src/im/keyboard/keyboard.h> · spell custom dict: <https://raw.githubusercontent.com/fcitx/fcitx5/master/src/modules/spell/spell-custom-dict.cpp> · Wayland notes: <https://fcitx-im.org/wiki/Using_Fcitx_5_on_Wayland>
- ibus-typing-booster: <https://github.com/mike-fabian/ibus-typing-booster> · docs: <https://mike-fabian.github.io/ibus-typing-booster/docs/user/>
- Maliit keyboard: <https://github.com/maliit/keyboard> · lomiri-keyboard CMake: <https://gitlab.com/ubports/development/core/lomiri-keyboard/-/raw/main/CMakeLists.txt>
- Squeekboard: <https://gitlab.gnome.org/World/Phosh/squeekboard> · `submission.rs`: <https://gitlab.gnome.org/World/Phosh/squeekboard/-/raw/master/src/submission.rs> · `imservice.rs`: <https://gitlab.gnome.org/World/Phosh/squeekboard/-/raw/master/src/imservice.rs>
- wvkbd: <https://github.com/jjsullivan5196/wvkbd> · LICENSE (GPL-3): <https://raw.githubusercontent.com/jjsullivan5196/wvkbd/master/LICENSE>
- onboard fork: <https://github.com/onboard-osk/onboard> · pypredict README: <https://raw.githubusercontent.com/onboard-osk/onboard/main/Onboard/pypredict/README> · `en_US.lm`: <https://raw.githubusercontent.com/onboard-osk/onboard/main/models/en_US.lm> · learning settings: <https://raw.githubusercontent.com/dr-ni/onboard/master/Onboard/Config.py> · text domains: <https://raw.githubusercontent.com/dr-ni/onboard/master/Onboard/TextDomain.py>
- wordfreq: <https://github.com/rspeer/wordfreq> · SUNSET: <https://raw.githubusercontent.com/rspeer/wordfreq/master/SUNSET.md> · wordfreq-rs: <https://github.com/kampersanda/wordfreq-rs> · model assets: <https://api.github.com/repos/kampersanda/wordfreq-rs/releases/tags/models-v1> · docs.rs: <https://docs.rs/wordfreq-model/latest/wordfreq_model/>
- SymSpell: <https://github.com/wolfgarbe/SymSpell> · `symspell_rs`: <https://github.com/wolfgarbe/symspell_rs> · `symspell` (reneklacan): <https://github.com/reneklacan/symspell> · SeekStorm blog (1000× correction): <https://seekstorm.com/blog/1000x-spelling-correction/>
- `fst`: <https://github.com/BurntSushi/fst> · transducers essay: <https://burntsushi.net/transducers/> · marisa-trie: <https://github.com/s-yata/marisa-trie> · `rsmarisa`: <https://crates.io/crates/rsmarisa>
- `spellbook`: <https://github.com/helix-editor/spellbook> · `zspell`: <https://github.com/pluots/zspell>
- `candle-nn` rnn: <https://docs.rs/candle-nn/latest/candle_nn/rnn/index.html> · `burn` nn: <https://docs.rs/burn/latest/burn/nn/index.html> · `tract-onnx`: <https://crates.io/crates/tract-onnx> · `llama-cpp-2`: <https://crates.io/crates/llama-cpp-2> · SmolLM2-135M GGUF: <https://huggingface.co/QuantFactory/SmolLM2-135M-GGUF/tree/main> · small-model CPU throughput: <https://arxiv.org/html/2511.07425v1> · Willison on SmolLM2: <https://simonwillison.net/2025/Feb/7/pip-install-llm-smollm2/>
- FlorisBoard nlp: <https://github.com/florisboard/nlp> · Rust engine PR: <https://github.com/florisboard/florisboard/pull/2970> · wkeys (Rust OSK, no prediction): <https://github.com/ptazithos/wkeys>
- GNOME Shell OSK suggestions: <https://gitlab.gnome.org/GNOME/gnome-shell/-/raw/main/js/ui/keyboard.js> · <https://gitlab.gnome.org/GNOME/gnome-shell/-/raw/main/js/ui/ibusCandidatePopup.js>

### Data
- Google Books Ngrams v3 (CC BY 3.0): <https://storage.googleapis.com/books/ngrams/books/datasetsv3.html> · Pechenick et al. 2015 bias: <https://journals.plos.org/plosone/article?id=10.1371/journal.pone.0137041>
- Norvig n-grams: <https://norvig.com/ngrams/> · google-10000-english LICENSE: <https://raw.githubusercontent.com/first20hours/google-10000-english/master/LICENSE.md>
- FrequencyWords: <https://github.com/hermitdave/FrequencyWords> · OPUS OpenSubtitles 2018: <https://opus.nlpl.eu/legacy/OpenSubtitles-v2018.php>
- Wiktionary frequency lists: <https://en.wiktionary.org/wiki/Wiktionary:Frequency_lists/English>
- SCOWL / en_US README: <https://raw.githubusercontent.com/LibreOffice/dictionaries/master/en/README_en_US.txt> · SCOWL v2: <https://raw.githubusercontent.com/en-wl/wordlist/v2/README.md>
- SUBTLEX-US: <https://www.ugent.be/pp/experimentele-psychologie/en/research/documents/subtlexus> · Tatoeba terms: <https://tatoeba.org/en/terms_of_use> · Wikimedia dumps legal: <https://dumps.wikimedia.org/legal.html> · Common Crawl ToU: <https://commoncrawl.org/terms-of-use> · BNC licence: <https://www.natcorp.ox.ac.uk/docs/licence.html>

### Wayland protocols, compositors, toolkits
- input-method-unstable-v2: <https://wayland.app/protocols/input-method-unstable-v2> · text-input-unstable-v3: <https://wayland.app/protocols/text-input-unstable-v3> · `ContentPurpose` enum (docs.rs): <https://docs.rs/wayland-protocols/latest/wayland_protocols/wp/text_input/zv3/client/zwp_text_input_v3/enum.ContentPurpose.html>
- Hyprland upstream relay: <https://raw.githubusercontent.com/hyprwm/Hyprland/main/src/managers/input/InputMethodRelay.cpp> · <https://raw.githubusercontent.com/hyprwm/Hyprland/main/src/managers/input/TextInput.cpp> · IME grab issue #7623 / PR #7660: <https://github.com/hyprwm/Hyprland/pull/7660> · omarchy#7346 (wtype under IME grab): <https://github.com/omacom/omarchy/issues/7346>
- GTK4 IM context: <https://gitlab.gnome.org/GNOME/gtk/-/raw/main/gtk/gtkimcontextwayland.c> · GTK3: <https://gitlab.gnome.org/GNOME/gtk/-/raw/gtk-3-24/modules/input/imwayland.c> · GtkPasswordEntry: <https://gitlab.gnome.org/GNOME/gtk/-/raw/main/gtk/gtkpasswordentry.c>
- Qt6 text-input-v3 builder: <https://raw.githubusercontent.com/qt/qtwayland/6.8/src/shared/qwaylandinputmethodeventbuilder.cpp> · protocol preference: <https://raw.githubusercontent.com/qt/qtwayland/6.8/src/client/qwaylanddisplay.cpp>
- Chromium Ozone text-input-v3: <https://chromium.googlesource.com/chromium/src/+/refs/heads/main/ui/ozone/platform/wayland/host/zwp_text_input_v3.cc> · feature default: <https://chromium.googlesource.com/chromium/src/+/e48954bdc391fcba2fd20a5a40a8a16332cf1638> · switches: <https://chromium.googlesource.com/chromium/src/+/refs/heads/main/ui/ozone/public/ozone_switches.cc> · sway#8276: <https://github.com/swaywm/sway/issues/8276>
- Firefox IMContextWrapper: <https://raw.githubusercontent.com/mozilla/gecko-dev/master/widget/gtk/IMContextWrapper.cpp>
- SDL Wayland keyboard (SDL2/SDL3): <https://raw.githubusercontent.com/libsdl-org/SDL/main/src/video/wayland/SDL_waylandkeyboard.c>
- winit text-input: <https://raw.githubusercontent.com/rust-windowing/winit/master/winit-wayland/src/seat/text_input/mod.rs> · foot README: <https://codeberg.org/dnkl/foot/raw/branch/master/README.md> · kitty `wl_text_input.c`: <https://raw.githubusercontent.com/kovidgoyal/kitty/master/glfw/wl_text_input.c>
- Steam client Wayland status: <https://github.com/ValveSoftware/steam-for-linux/issues/4924>
- AT-SPI2 Text interface: <https://docs.gtk.org/atspi2/iface.Text.html> · Chromium accessibility flags: <https://chromium.googlesource.com/chromium/src/+/main/docs/accessibility/overview.md> · Electron accessibility: <https://www.electronjs.org/docs/latest/tutorial/accessibility>

### UX / consoles / latency
- Steam Community, word suggestions request (2024): <https://steamcommunity.com/app/1675200/discussions/0/4300445879342501658/>
- PS5 R1 accepts prediction: <https://www.pushsquare.com/news/2020/11/20_secret_ps5_features_you_may_not_know_about>
- Xbox suggestion selection (2016 preview): <https://www.windowscentral.com/xbox-one-preview-update-brings-text-suggestions-and-arena-changes>
- Windows 11 gamepad keyboard: <https://blogs.windows.com/windows-insider/2025/01/24/announcing-windows-11-insider-preview-build-22635-4805-beta-channel/>
- Nintendo Switch predictive text: <https://www.nintendo.com/en-gb/Support/Nintendo-Switch/FAQ/How-do-I-reset-the-keyboard-s-predictive-text-suggestions-/How-do-I-reset-the-keyboard-s-predictive-text-suggestions-1208464.html>
- Gboard for Android TV design notes: <https://websiddu.com/work/g-board-for-tv>
- Nielsen response-time limits: <https://www.nngroup.com/articles/response-times-3-important-limits/>
- Gboard privacy / delete learned words: <https://support.google.com/gboard/answer/9058584> · SwiftKey privacy (no learning in password fields): <https://support.microsoft.com/en-us/topic/microsoft-swiftkey-keyboard-privacy-questions-and-your-data-07e13677-6b38-4ad0-bad0-d41207cab6de>
