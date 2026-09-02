# Dictation into the launcher — why "Firefox." finds nothing

Research for: `guide+a` runs `voxtype record toggle`; the owner dictates app
names into the Omarchy menu (`omarchy-menu`) and the transcript arrives as a
*sentence* — "Firefox." / "Open Spotify." — which the menu's filter then fails
to match. The owner's direction is explicit: a trailing-period post-process is
the wrong shape. The right shapes are **contextual dictionary seeding**, a
**semantic-driven launcher**, or **comparison matching instead of strict
substring searching**. This report finds out which of those is real, where each
would live, and what each costs.

Everything below is marked **VERIFIED** (read from source at a cited line, or
observed live on this machine on 2026-09-02) or **INFERRED**. Live probing was
read-only: `voxtype --help` / `record --help` (no recording started), `systemctl
--user status`, `hyprctl layers`, `pacman -Q`, and a throwaway Node harness that
`require`d the shell's own `.js` modules against the real `/usr/share/
applications` tree. Nothing under `~/.config` was written; no menu was launched;
no audio was captured.

Sources of truth:

- **The launcher**: `~/code/omarchy-bluetooth-friendly-name/shell/` — VERIFIED
  live via `systemctl --user show-environment`:
  `OMARCHY_PATH=/home/ajg/code/omarchy-bluetooth-friendly-name`, so the dev
  checkout *is* the running shell. READ-ONLY for this report.
- **Stock Omarchy**: `/usr/share/omarchy/shell/`. VERIFIED — `services/
  AppSearch.js` is byte-identical between the two, and every matcher function in
  `plugins/menu/MenuModel.js` (`searchableToken`, `leafIdFor`, `nameSearchText`,
  `termInSearchWords`, `descriptionTextMatches`, `matchesQuery`, `searchScore`)
  is identical too; the 36-line local delta is entirely the owner's `disabled:`
  menu-entry carry. **Every finding in §1 is a finding about stock Omarchy**,
  and every patch in §5 is upstreamable without touching the local carry.
- **voxtype, installed**: `voxtype-bin 1.0.0-2` (VERIFIED, `pacman -Qo`),
  `/usr/bin/voxtype -> /usr/lib/voxtype/voxtype-avx512`.
- **voxtype, source**: `~/code/voxtype`, branch `dev`, remote
  `https://github.com/peteonrails/voxtype.git`. **This is ahead of what is
  installed, and has uncommitted work in it** (§2.1). READ-ONLY for this report.
- **hyprpad**: this repo, `master` @ `e1afe64`.

---

## 0. Recommendation in one paragraph

**Fix the launcher, not the dictation.** VERIFIED empirically (§1.3): case and
whitespace are already handled — `"CHROME"`, `"  chrome  "` and `"Chrome"` all
match today. The *only* two things that break a dictated query are (a) a
trailing `.` and (b) filler words like "open" and "the". That is a ~40-line
patch to one file, `plugins/menu/MenuModel.js`, with a Node test harness already
sitting in `test/shell.d/` waiting for it — and it fixes the same failure for a
keyboard user who types a question mark, for the on-screen keyboard, and for
every other Omarchy user. The dictation-side alternative (seed voxtype with the
launcher's vocabulary) is the *more* interesting idea and is genuinely the right
shape, but it is blocked three ways today: `voxtype record toggle` has **no**
per-invocation prompt or vocabulary lever at all in the installed 1.0.0
(VERIFIED, §2.5); the only per-invocation channel that exists in the dev tree
carries a *profile name*, and a `Profile` holds only a post-process command and
an output mode — no vocabulary (VERIFIED, `src/config/profile.rs:50-64`); and
Whisper's initial prompt is capped at 224 tokens, against a launcher corpus of
462 labels / ~4.8 kB that INFERRED needs ~1200 (§3.4). Upstream voxtype has
already planned exactly the missing piece — issue #696's stage 1 is literally
"Profiles gain a term list driving Whisper `initial_prompt`" — so the seeding
idea's correct expression is a contribution to #696/#519, not a local hack.
Do **A now, B later, and do not build C** (a local embedding model in the
launcher is overkill for a 462-row corpus that already ships hand-written
synonyms in `Keywords=` and `GenericName=`).

One more thing, unprompted but load-bearing: **dictation is currently broken on
this machine and has been for hours.** `voxtype.service` is in
`activating (auto-restart)` with **restart counter 3614**, dying on
`unknown variant 'muse'` — the config names an engine the installed 1.0.0
binary does not have (§2.1). Nothing in this report can be tested by voice until
that is resolved.

---

## 1. The launcher's matcher

### 1.1 Three query paths, only one of which matters

VERIFIED — `grep` across the whole shell tree for `sortedEntries(` and
`matchesQuery(`:

| path | code | used for | matcher |
|---|---|---|---|
| **menu + apps** | `Menu.qml:618-645` → `MenuModel.matchesQuery` (`MenuModel.js:323`) | the omarchy-menu search field, **including every app row** | multi-term substring |
| dmenu | `Menu.qml:562-573` | `omarchy-menu`'s dmenu mode | single-string `indexOf` on label and detail |
| `AppSearch` | `services/AppSearch.js:74` `fuzzyScore` | **nothing, for querying** | substring + acronym |

The third row is the surprise. `services/AppSearch.js` contains the file that
*looks* like the launcher's search — `fuzzyScore`, `entryAcronym`,
`allTermsMatch` — and `Menu.qml:289` even comments that app rows come from
"the shared AppLibrary … like the launcher". But VERIFIED: the only call site is
`AppLibrary.qml:52-54`, and the only caller of *that* is `Menu.qml:293`:

```qml
var rows = root.appLibrary.sortedEntries("")     // Menu.qml:293
```

— **with an empty query**. `AppSearch` is used purely to enumerate and
alphabetise; its scoring never sees a search term in this shell. Every app row
is then flattened into the menu's own item tree by `mergeAppRows()`
(`Menu.qml:287-320`) and searched by `MenuModel.matchesQuery` like any other
menu row.

> **INFERRED**: `AppSearch.fuzzyScore` is either dead code or is there for a
> standalone launcher binary that is not in this checkout (`Menu.qml:79` says
> "also used by the standalone launcher"). Either way it is not what filters the
> owner's dictated text. **Do not patch `AppSearch.js` expecting it to help.**

### 1.2 `matchesQuery`, the function that actually rejects "Firefox."

`shell/plugins/menu/MenuModel.js:323-339` (identical to stock):

```js
function matchesQuery(entry, query, visible) {
  if (!entry || entry.id === "root") return false
  if (!visible) return false

  var nameText = nameSearchText(entry)
  var descriptionText = String(entry.description || "").toLowerCase()
  var terms = String(query || "").toLowerCase().trim().split(/\s+/)

  for (var i = 0; i < terms.length; i++) {
    if (!terms[i]) continue
    if (nameText.indexOf(terms[i]) >= 0) continue
    if (termInSearchWords(terms[i], descriptionText)) continue
    return false
  }

  return true
}
```

Its inputs:

- `nameSearchText` (`:299-305`) = `label` + `searchableToken(leafIdFor(id))` +
  the aliases, all lowercased. `searchableToken` (`:290-292`) folds `. _ -` to
  spaces — **but only for the id and the aliases, never for the label**.
- `descriptionText` = `entry.description`, matched **whole-word only** via
  `termInSearchWords` (`:307-313`), which splits on `\s+` and compares `===`.
- For an app row, `mergeAppRows` (`Menu.qml:287-320`) sets
  `description = GenericName` and `aliases = [GenericName, ...Keywords]`.

So the answer to "does the launcher search `Keywords=` and `GenericName=`?" is
**yes, both** (VERIFIED): `GenericName` twice over (as description, word-wise,
and as an alias, substring-wise) and `Keywords` as aliases.

`searchScore` (`:341-363`) then ranks: exact label `0/2`, whole-word app label
`0`, label prefix `10`, label substring `30`, `nameText` substring `40`,
description word-match `60`, everything else `80`, with `kind` and depth
tiebreaks. It is a **tiered substring** score, not a fuzzy one: nothing in it
tolerates an inserted, deleted or transposed character.

### 1.3 What actually fails — measured

Probe: a Node script `require`ing the shell's own `MenuModel.js` and
`AppSearch.js`, fed the real machine's desktop entries (VERIFIED live: 235
`.desktop` files, of which 141 have a `Name` and are not `NoDisplay=true`; 74
carry `GenericName=`, 49 carry `Keywords=`), with app rows constructed exactly
as `mergeAppRows` does.

Against a single real row (`Google Chrome`, `GenericName=Web Browser`):

| query | `matchesQuery` |
|---|---|
| `chrome` | ✅ |
| `Chrome` | ✅ |
| `CHROME` | ✅ |
| `  chrome  ` | ✅ |
| `Google Chrome` | ✅ |
| `web browser` | ✅ |
| **`Chrome.`** | ❌ |
| **`Google chrome.`** | ❌ |
| **`Web Browser.`** | ❌ |
| **`open chrome`** | ❌ |

**Sentence casing is a red herring.** `matchesQuery` lowercases both sides
(`:328-329`) and `.trim()`s. Whisper's capitalisation costs nothing. The *only*
two failure modes are:

1. **The trailing period.** `"chrome."` is one term, and
   `"google chrome desktop web browser".indexOf("chrome.")` is `-1`.
2. **Extra words.** Every term must match. `"open"` matches nothing, so
   `"open chrome"` returns false even though `"chrome"` alone is a perfect hit.

Full-corpus results (141 visible apps):

| dictated query | matches now | first hits |
|---|---|---|
| `Chrome.` | **0** | — |
| `Open Spotify.` | **0** | — |
| `Spotify.` | **0** | — |
| `the browser` | **0** | — |
| `Music.` | **0** | — |
| `Terminal.` | **0** | — |
| `Settings.` | **0** | — |
| `chrome` | 1 | Google Chrome |
| `spotify` | 1 | Spotify |
| `browser` | 5 | Chromium, Google Chrome, 3× Avahi |
| `music` | 2 | cliamp, Spotify |

Note `browser` → 5 hits **already**, via `GenericName=Web Browser` on
`google-chrome.desktop` and `chromium.desktop` (VERIFIED by reading those
files). The "open the browser" case is *nearly* solved today — it is defeated
purely by the words "open" and "the", not by any missing semantics.

> Firefox is not installed on this machine (VERIFIED — no `firefox*.desktop`),
> so the owner's literal "Firefox." example cannot be reproduced locally.
> `Chrome.` / `Spotify.` are the exact same code path.

### 1.4 Two incidental bugs found on the way

Both VERIFIED, both in stock Omarchy, both fixable in the same patch:

**(a) Every app row is searchable as "desktop", and by nothing else from its
file name.** `nameSearchText` calls `leafIdFor(entry.id)` (`:294-297`), which
splits the id on `.` and takes the **last** segment. App ids are
`apps.google-chrome.desktop`, so the leaf is the literal string `"desktop"`:

```
nameSearchText({id:'apps.google-chrome.desktop', label:'Google Chrome',
                aliases:['Web Browser']})
  === "google chrome desktop web browser"
leafIdFor('apps.google-chrome.desktop') === "desktop"
```

Consequences, measured: the query `desktop` matches **235 of 235** app rows, and
the query `google-chrome` or `org.gnome.Calculator` matches **none** — the
desktop-file stem, the single most reliable machine name an app has, is thrown
away and replaced by a token shared by every app. (`AppSearch.js:23` gets this
right — it puts the whole `entry.id` in the haystack — which is why `Spotify.`
accidentally matches *there*: `spotify.desktop` contains the substring
`spotify.`. Another reason not to reason about behaviour from `AppSearch.js`.)

**(b) The label is never punctuation-folded.** `searchableToken` folds `._-`
for ids and aliases but the label goes in raw (`:304`). Measured against a
`Node.js` row: `node js` ✅ (two terms, both substrings), `nodejs` ❌. Against a
`btop++` row: `btop` ✅, `btop++` ✅, `btop.` ❌.

This matters for the fix's design: this machine really does have labels whose
punctuation is *meaningful* — `Node.js`, `.NET`, `Battle.net`, `Wi-Fi`,
`btop++`, `Xournal++`, `Cities: Skylines`, `Half-Life: Alyx`,
`FINAL FANTASY X/X-2 HD Remaster`, `Where am I?` — so a normalizer must **strip
trailing sentence punctuation and fold for matching**, never strip punctuation
out of the corpus wholesale.

### 1.5 Where the query enters, and where a normalizer goes

- Typed/dictated characters append at `Menu.qml:1167`:
  `root.setFilter(root.filterText + event.text)`. A `.` is ordinary printable
  text and lands in `filterText` verbatim.
- Backspace/Ctrl+U editing: `Commons/Util.qml:107-121`
  (`editsFilter`/`editedFilter`), reached at `Menu.qml:1141-1142`.
- `setFilter` (`Menu.qml:721`) stores it and triggers the rebuild.
- The rebuild's search branch is `Menu.qml:618-645`.
- "No matches for …" renders at `Menu.qml:1467`.

**The normalizer must not touch `filterText` itself.** `filterText` is what is
*displayed* (`Menu.qml:1211-1213`) and what dmenu returns as the user's answer
(`Menu.qml:763`, `applyDmenuSelection(root.filterText)`). Normalise on the
**matching side only** — inside `matchesQuery` / `searchScore`, so the user
still sees the period they said while the launcher quietly ignores it.

---

## 2. voxtype on this machine

### 2.1 Two voxtypes, and the daemon is down

VERIFIED, live:

```
$ pacman -Qo /usr/bin/voxtype        -> voxtype-bin 1.0.0-2
$ voxtype --version                  -> voxtype 1.0.0
/usr/bin/voxtype -> /usr/lib/voxtype/voxtype-avx512
```

`~/.config/voxtype/config.toml` (read, not modified):

```toml
state_file = "auto"
engine = "muse"
[hotkey] enabled = false            # "Hotkey is configured in Hyprland"
[audio] device = "default"; sample_rate = 16000; max_duration_secs = 60; pause_media = true
[whisper] model = "base.en"; language = "en"; translate = false
[output] mode = "type"; fallback_to_clipboard = true; type_delay_ms = 1
[output.notification] on_recording_start/stop/transcription = false
[muse] api_key = "…"; model = "muse-voice-transcribe-1.0"; streaming = true
```

`engine = "muse"` is **not a value the installed binary accepts**. Every
voxtype subcommand fails on config load:

```
$ voxtype config schema
Error: Configuration error: Invalid config: unknown variant `muse`,
expected one of `whisper`, `parakeet`, `moonshine`, `sensevoice`,
`paraformer`, `dolphin`, `omnilingual`, `cohere`, `soniox`  in `engine`
```

and so does the daemon:

```
$ systemctl --user status voxtype
   Active: activating (auto-restart) (Result: exit-code)
  Process: ExecStart=/usr/bin/voxtype daemon (code=exited, status=1/FAILURE)
  voxtype.service: Scheduled restart job, restart counter is at 3614.
```

The engine exists only in the owner's checkout, and only as **uncommitted
work**: `~/code/voxtype` is on branch `dev` with `src/transcribe/muse.rs`,
`src/config/engines/muse.rs` and `src/bin/muse_mic_test.rs` **untracked**, plus
11 modified tracked files (VERIFIED, `git status --porcelain`; `git log --
src/transcribe/muse.rs` returns nothing).

**Consequence for everything below**: the running dictation stack is *nothing*.
`guide+a` fires `voxtype record toggle`, which writes to a state file the daemon
never created. Any voice test in §6 needs either `voxtype config set engine
whisper` (or unsetting `engine`) or a locally-built binary from the dev tree
installed over the packaged one.

### 2.2 Engine, model, and where the punctuation comes from

- **Configured engine**: `muse` — Meta's hosted `muse-voice-transcribe-1.0`
  through the Meta Model API, streaming, ~80 ms chunks
  (`src/config/engines/muse.rs:1-14`). **Cloud**, with an API key in the config
  file.
- **Configured fallback model**: `[whisper] model = "base.en"` — 142 MB,
  ~7× realtime, ~8 % WER (`docs/USER_MANUAL.md:1195`).
- Upstream supports nine engines: whisper, parakeet, moonshine, sensevoice,
  paraformer, dolphin, omnilingual, cohere, soniox (VERIFIED, the error text
  above).

**Punctuation and capitalisation are the model's, not a post-process.**
VERIFIED two ways: (1) voxtype.io's own feature list says "Punctuation,
capitalization, and inverse text normalization out of the box"; (2) `src/text/
mod.rs` — the entire text stage — contains only `spoken_punctuation`,
`replacements`, `smart_auto_submit`, `filter_filler_words` and `filler_words`
(`src/config/text.rs:9-34`). There is **no capitalisation step and no
punctuation-insertion step in voxtype**. Whisper and Muse emit sentence-cased,
period-terminated text because that is what they were trained to emit.

Corroborating detail from upstream's own roadmap (`CLAUDE.md:346`): stage 3 of
the planned cleanup pipeline is "punctuation/casing … This is what makes CTC
output usable: SenseVoice, Dolphin, Omnilingual, Paraformer emit **neither**
punctuation nor casing today." — i.e. today voxtype adds none, and the CTC
engines therefore produce none.

> **INFERRED, and worth a one-line experiment**: switching `engine` to a CTC
> model (`sensevoice`, `paraformer`) would produce *raw, unpunctuated,
> uncased* text — accidentally the exact "raw mode" the owner asked about. It
> would also make ordinary prose dictation worse everywhere else, so it is only
> interesting as a per-context profile, which returns us to §2.5's blocker.

### 2.3 Is there a "raw" / no-punctuation mode?

**No.** VERIFIED by exhaustion:

- `voxtype --help` has a "Text Processing" section: `--spoken-punctuation`,
  `--shift-enter-newlines`, `--smart-auto-submit`, `--filter-fillers`,
  `--append-text`. Nothing removes punctuation.
- `src/config/text.rs:9-34` — the complete `TextConfig` — has no such field.
- `grep -rin "no_punctuation|strip_punct|raw_text"` over `src/` returns nothing
  relevant.

The nearest thing is `[text.replacements]`, a case-insensitive **word**
replacement table (`src/config/text.rs:17`), which cannot express "drop a
trailing period" — and would be the rejected shape anyway.

### 2.4 `initial_prompt` — the seeding lever, and its ceiling

It exists and it is real:

- Config: `[whisper] initial_prompt` (`src/config/whisper.rs:169`).
- CLI: `--initial-prompt <PROMPT>` — "Initial prompt to provide context for
  transcription. Hints at terminology, proper nouns, or formatting conventions."
  (`voxtype --help`, "Whisper" section; `src/cli/root.rs:139`).
- Applied at `src/app/overrides.rs:111-112` →
  `config.whisper.initial_prompt = Some(prompt)`.
- Reaches whisper.cpp at `src/transcribe/whisper.rs:117-120`:
  `params.set_initial_prompt(prompt)`.
- Also plumbed for the remote backend (`src/transcribe/remote.rs:184-186`, sent
  as the `prompt` field).
- Docs: `docs/CONFIGURATION.md:946-996`, `docs/USER_MANUAL.md:1136-1186`.
  Examples are term lists — `"Voxtype, Hyprland, Waybar, Sway, wtype, ydotool,
  systemd, journalctl."` — and the guidance is "Keep prompts concise (a few
  words or a short sentence)"; "The prompt doesn't need to be grammatically
  correct — a list of terms works well."

**Three hard limits:**

1. **It is a *daemon* flag, not a per-recording flag.** VERIFIED — the CLI
   example in the manual is `voxtype --initial-prompt "…" daemon`
   (`docs/USER_MANUAL.md:1176`). `voxtype record` does not take it (§2.5).
2. **224 tokens, and it is the *last* 224 that survive.** Whisper's text context
   is 448 tokens, split half for prompt and half for output; a longer prompt is
   silently truncated from the front. VERIFIED against upstream Whisper
   discussions (§7). voxtype does not guard, warn or truncate — `grep` for
   `224` / `n_text_ctx` / `truncat` in `src/transcribe/whisper.rs` and
   `src/config/whisper.rs` returns nothing.
3. **Whisper-only, in practice.** `docs/CONFIGURATION.md:996`: "This setting
   only applies when using the local whisper backend … Remote servers may ignore
   the initial_prompt parameter." The owner's configured engine is Muse, which
   has its **own** biasing field: `[muse] context`
   (`src/config/engines/muse.rs:55-58`, "Context biasing text … Mapped to
   server's `context` / `prompt` field"), sent as
   `request_obj["keywords"] = json!([ctx])` (`src/transcribe/muse.rs:137-140`,
   and the same in the WebSocket init frame at `:188-190`).

   > **Bug, VERIFIED**: the Muse mapping wraps the whole context string as a
   > *single* array element. The API's documented shape is a list —
   > `"keywords": ["Acme Mobile", "eSIM"]` per the comment on `:138` — so a
   > vocabulary of 60 app names is currently sent as one 600-character
   > "keyword". Splitting on commas is a two-line fix, and it is a prerequisite
   > for any Muse-side seeding. Muse's `keywords` has **no 224-token ceiling**
   > (INFERRED — it is a server-side biasing list, not a decoder prefix), which
   > makes it a *better* seeding target than Whisper for this use case.

### 2.5 What can actually be varied per invocation, today

`voxtype record` is a **signalling** front-end: `start` = SIGUSR1, `stop` =
SIGUSR2 (`src/cli/record.rs:16,76`). Signals carry no payload, so anything
richer travels through a file in `$XDG_RUNTIME_DIR/voxtype/`
(`Config::runtime_dir()`, `src/config/root.rs:253-259`).

**Installed 1.0.0** — VERIFIED, `voxtype record --help` lists **`-h` and
nothing else**. `record start|stop|toggle|cancel` take no flags at all. There is
**zero** per-invocation lever in what is installed.

**Dev tree** (`src/cli/record.rs:15-180`) — `start` and `toggle` gained:
`--type` / `--clipboard` / `--paste` / `--file[=PATH]`, `--model <MODEL>`,
**`--profile <NAME>`**, `--auto-submit` / `--no-auto-submit`,
`--shift-enter-newlines` / `--no-…`, `--no-osd`, `--smart-auto-submit` /
`--no-…`.

The `--profile` transport (VERIFIED):

```
voxtype record toggle --profile launcher
  └─ src/app/record.rs:71-100   validate name against config, then
                                write $XDG_RUNTIME_DIR/voxtype/profile_override
  └─ SIGUSR1/2 to the daemon
daemon
  └─ src/daemon.rs:2601-2605    read_profile_override() at *output* time,
                                config.get_profile(name)
  └─ src/daemon.rs:349-392      read-and-delete; also write_profile_override()
                                for the hotkey-modifier path
```

And what a profile *is* (`src/config/profile.rs:50-64`) — the whole struct:

```rust
pub struct Profile {
    pub post_process_command: Option<String>,
    pub post_process_timeout_ms: Option<u64>,
    pub output_mode: Option<OutputMode>,
}
```

**Three fields. No vocabulary, no prompt, no engine, no model, no language.**
And it is consumed *after* transcription, at output time — the audio has already
been decoded by then, so even adding a field would not by itself bias decoding.

Two further structural facts:

- `Config` is loaded once at daemon start (`Daemon::new(config, …)`,
  `src/daemon.rs:879`); there is no per-recording reload. Changing
  `initial_prompt` today means editing the file and restarting the unit — which
  reloads the model. Upstream issue #519 puts a number on that alternative:
  "each language switch would cost a full daemon restart + model load (~ several
  seconds)".
- The `Transcriber` trait takes **no per-call prompt**
  (`src/transcribe/mod.rs:102-105`): `fn transcribe(&self, samples: &[f32])`.
  The prompt is a field captured when the transcriber is built
  (`src/transcribe/whisper.rs:30,75`). Per-utterance seeding needs a new trait
  method or interior mutability — a real, if small, refactor.

The one *un*-blocked per-invocation channel is the post-process pipe
(`[output.post_process] command`, overridable per profile), which additionally
receives `VOXTYPE_CONTEXT` — the previous transcription, if within 60 s — as an
environment variable (`src/output/post_process.rs:96-98`). This is a genuine
context-scoped lever, but it operates on **text after decoding**, which is the
shape the owner ruled out.

### 2.6 Upstream has already designed the missing piece

VERIFIED via `gh issue view` on `peteonrails/voxtype` and the roadmap in the
checkout's `CLAUDE.md:346-348`:

- **#696 — "Dictation cleanup pipeline"** (OPEN, milestone 1.2.0). Thesis:
  "Almost everything users ask an LLM to do to dictation is not a generation
  problem. It is labeling and rules, with exact answers." Pipeline:
  `raw ASR → 1 vocabulary → 2 disfluency → 3 punctuation/casing → 4 ITN →
  5 user rules → output`. **Stage 1 is the exact feature this report needs**:

  > "**Vocabulary** — no new model. Profiles gain a term list driving Whisper
  > `initial_prompt` (already plumbed), remote `prompt`, Soniox `terms` (exists,
  > not in schema). CTC engines get post-hoc Double Metaphone + bounded edit
  > distance. Ship 3-4 curated starter vocabularies."

  The issue's own framing traces it to "the profile-vocabulary idea he raised
  independently."
- **#519 — "Engine, model, language and streaming configurable per profile"**
  (OPEN, `enhancement`). States the blocker in the reporter's words: "`[profiles.
  <name>]` — but only `post_process_command`, `post_process_timeout_ms`,
  `output_mode`. No engine/model/language/streaming." and "per-record overrides
  cover only `--model`." Roadmapped for 1.3.0, "absorbing **Dictation Intents**
  and per-record language (#484)".

"Dictation Intents" is upstream's name for precisely what the owner is asking
for. **The seeding idea is not a hack the owner would be inventing; it is a
feature two open upstream issues are already converging on**, and a
`[profiles.launcher] vocabulary = [...]` + `--profile launcher` patch would be a
contribution to #696 stage 1 rather than a fork.

### 2.7 How it types

`[output] mode = "type"` with `type_delay_ms = 1` and
`fallback_to_clipboard = true`. Available drivers, in `--driver` order:
**`wtype`, `dotool`, `ydotool`, `clipboard`** (`voxtype --help`, Output
section). Alternatives: `--clipboard` (copy only) and `--paste` (clipboard +
Ctrl+V, with `--restore-clipboard`). Relevant switches for a layer-shell target:
`--pre-type-delay <MS>` ("helps prevent first character drop"),
`--wait-for-modifier-release`, and `--pre-output-command` /
`--post-output-command` ("e.g. compositor submap switch").

Typed characters arrive at the menu as ordinary key events and are appended by
`Menu.qml:1167`. VERIFIED that this is the path a dictated `.` takes into
`filterText`. `auto_submit` is not set in the config, so nothing presses Enter —
the transcript sits in the search field, which is exactly why the owner *sees*
"No matches for “Firefox.”" rather than a wrong launch.

---

## 3. hyprpad's lever

### 3.1 The mode already exists

`config/hyprpad.lua:76-85` — the `omarchy-ui` allowlist, keyed on `ctx.layers`:

```lua
h.mode("omarchy-ui").when(function(ctx)
  for _, ns in ipairs({ "omarchy-menu", "omarchy-keyboard-panel", "omarchy-clipboard",
                        "omarchy-emojis", "omarchy-image-selector", "omarchy-reminders",
                        "omarchy-polkit", "omarchy-network-qr" }) do
    if ctx.layers:has(ns) then return true end
  end
  return false
end)
```

`omarchy-menu` is the launcher's namespace (VERIFIED,
`shell/plugins/menu/Menu.qml:1070`, `WlrLayershell.namespace: "omarchy-menu"`).
Layer tracking lives in `src/mode.rs:79` (an ordered `BTreeSet<String>`) and
`:283-302`. So **hyprpad already knows, exactly and cheaply, that the launcher
is open.**

### 3.2 But a chord cannot be bound twice

The obvious construction —

```lua
h.bind("guide+a", h.exec "voxtype record toggle")
h.bind("guide+a", h.exec "voxtype record toggle --profile launcher"):only_in("omarchy-ui")
```

— **does not work**. VERIFIED at `src/lua_config.rs:1041-1049`: for
`BindKind::Gesture` a re-bind is a plain overwrite:

```rust
let k = GestureKey::parse(&key).map_err(err)?;
b.bindings.insert(k, action);          // last one wins
```

Alternates exist only for **buttons** (`Entry::Occupied(_) => b.button_alts.
push(…)`, `:1067-1073`, with the comment "A button bound a *second* time is the
same button meaning something else in another mode"), and `Slot` has
`Button`/`ButtonAlt` but only a single `Binding` variant (`:683-690`). One
gesture key ⇒ one action ⇒ one guard (`:707-709`).

Also note `guide+a` is deliberately **unguarded** today — the guide chords are
"hyprpad's escape hatch and must work over a fullscreen game"
(`config/hyprpad.lua:224-229`). Guarding it into `omarchy-ui` would *remove*
dictation everywhere else.

### 3.3 The lever that does exist: `HYPRPAD_MODE`

VERIFIED end to end:

```rust
// src/run.rs:2584 — the mode as it is at the moment the binding fires
if let Err(e) = execute(hypr, action, modes.state().active()) {

// src/run.rs:2851-2861
Action::Exec(cmd) => { hypr.spawn_env(cmd, &[("HYPRPAD_MODE", mode)]); Ok(()) }

// src/hypr.rs:212-231
Command::new("/bin/sh").arg("-c").arg(&cmd).envs(env)…
```

Two gifts here: the command runs through **`/bin/sh -c`** (so command
substitution, pipes and scripts all work), and it is handed the live mode name.
The whole context-sensitivity problem is therefore solvable **with no hyprpad
change at all**:

```lua
-- config/hyprpad.lua:238, unchanged in shape, unguarded as it must be
h.bind("guide+a", "Dictation toggle", h.exec "hyprpad-dictate")
```

```sh
#!/bin/sh
# hyprpad-dictate — one binding, context-sensitive behaviour
case "$HYPRPAD_MODE" in
  omarchy-ui) exec voxtype record toggle --profile launcher ;;
  *)          exec voxtype record toggle ;;
esac
```

Cost: one extra `/bin/sh` (already being spawned) and a `case`. The
enumeration a richer script would need is free too — VERIFIED,
`grep ^Name= /usr/share/applications/*.desktop … | sort -u` over 235 files runs
in **10 ms** wall.

**The catch**: as of §2.5 there is nothing worth putting on the right-hand
side. With the installed 1.0.0, `--profile` does not exist. With the dev tree,
`--profile launcher` buys a post-process command and an output mode — the shape
the owner rejected. The hyprpad lever is **ready and free; the voxtype end of it
is what is missing.**

### 3.4 Would seeding even fit? The arithmetic

Corpus, measured on this machine:

| source | count | chars |
|---|---|---|
| unique app `Name=` (`/usr/share/applications` + `~/.local/share/applications`) | 131 | 1794 |
| menu labels (`$OMARCHY_PATH/default/omarchy/omarchy-menu.jsonc`) | 331 | 2977 |
| **total** | **462** | **4771** |

Against Whisper's 224-token prompt: proper nouns tokenize badly (INFERRED
~3 chars/token for capitalised app names vs ~4 for prose), so 4771 chars ⇒
**~1200-1600 tokens, 5-7× over the cap**, and the overflow is silently dropped
from the *front* — meaning a naive `--initial-prompt "$(all names)"` would seed
only whatever happened to sort last. **Seeding the whole launcher into Whisper
is not possible.**

What *does* fit: ~224 tokens ≈ **650-900 chars** ≈ **50-80 app names**. That is
comfortably more than the apps anyone actually launches by voice, so the right
construction is a **curated or frecency-ranked subset**, not the corpus. A
starter list of the 40 most-launched apps plus the ~20 top-level menu labels
would sit at ~400 chars and leave headroom.

Muse is the better target anyway (§2.4): `keywords` is a server-side biasing
list with no decoder-prefix budget, so the full 462-item vocabulary is plausibly
sendable there — *after* the one-element-array bug is fixed. **INFERRED, not
verified**: the Meta Model API's own limit on `keywords` length is undocumented
in the checkout and was not probed (that would mean sending audio).

And a caveat on what seeding buys: `initial_prompt` biases *recognition*. It
makes Whisper more likely to render the sound "spotify" as `Spotify` rather than
`Spot a Fi`. It does **not** stop the model emitting a trailing period —
`docs/USER_MANUAL.md:1184`: "The prompt guides Whisper's expectations but
doesn't guarantee exact transcription." A prompt written as a bare comma-list
(`"Spotify, Google Chrome, Ghostty, …"`) *may* nudge the output toward list-like
formatting, but that is a soft, unverifiable effect. **Seeding fixes
mis-transcription of app names; it does not fix the period.** The launcher patch
does.

---

## 4. Semantic matching

**Verdict: not worth it, and mostly unnecessary — say so plainly.**

An embedding model in the launcher would mean shipping a sentence encoder
(20-90 MB), an ONNX runtime in the Quickshell process, and a per-keystroke
inference budget, to rank **462 short strings**. The corpus is three orders of
magnitude too small to earn it, the latency budget is a keystroke, and the
failure it would fix ("the browser" → Chrome) is *already fixed in the data*.

Because the desktop-entry spec already ships hand-written synonyms, and Omarchy
already reads them:

- `GenericName=` — 74 of 235 files here. `Web Browser` on both Chrome and
  Chromium, `Music Player` on Spotify, `Terminal Emulator` on the terminals.
- `Keywords=` — 49 of 235 files.
- `Comment=` — nearly universal. Read by `AppSearch.js:23`, **not** by the menu.

VERIFIED: the menu already matches both `GenericName` and `Keywords`, twice over
(§1.2). `browser` → 5 hits *today*. `music` → Spotify *today*. The entire
"semantic" win is already in the corpus; what defeats it is one period and two
stop words.

So the realistic middle ground the owner named — "fuzzy + synonyms/aliases" — is
correct, and the synonyms half is **already done**. What is missing is:

1. **query normalisation** (trailing punctuation, stop words),
2. **subsequence/fuzzy comparison** instead of strict substring, so `gogle
   chrome` and `nodejs` work,
3. **the two data bugs in §1.4** (the desktop-file stem, the unfolded label),
4. optionally, **`Comment=` in the menu's haystack** — this is the one genuinely
   new signal available for free, and it is what would make "internet" find a
   browser (`Comment=Access the Internet` on both Chrome and Chromium). It is
   also the noisiest; put it in a lower `searchScore` tier, not in `nameText`.

A measured sketch of 1+2 (query-side normalizer: lowercase, strip trailing
`.,!?;:`, fold interior punctuation to spaces for matching, drop stop words
`the a an please open launch start run go to` unless they are the whole query;
match-side: substring over `label + desktop-stem + GenericName + Keywords`, with
a ≥3-char **subsequence** fallback against the label):

| dictated query | now | sketch |
|---|---|---|
| `Chrome.` | 0 | **1** — Google Chrome |
| `Open Spotify.` | 0 | **1** — Spotify |
| `Spotify.` | 0 | **1** — Spotify |
| `Open the browser.` | 0 | **5** — Chromium, Google Chrome, 3× Avahi |
| `Open Chrome, please.` | 0 | **1** — Google Chrome |
| `Music.` | 0 | **2** — cliamp, Spotify |
| `Terminal.` | 0 | **6** — Alacritty, Ghostty, … |
| `gogle chrome` (typo) | 0 | **1** — Google Chrome |
| `chrome` (regression check) | 1 | **1** — unchanged |

That is the whole reported problem, solved, in one file, with no model.

---

## 5. Recommendation, ranked

### A — Normalise and widen the launcher's matcher (DO THIS FIRST)

**Where**: `~/code/omarchy-bluetooth-friendly-name/shell/plugins/menu/
MenuModel.js`, in `matchesQuery` (`:323`) and `searchScore` (`:341`), plus the
dmenu branch at `Menu.qml:562-573` for consistency.

**Patch shape** (~40 lines, one file):

1. `normalizeQuery(q)` — lowercase; strip trailing `.,!?;:` from the whole
   string and from each term; fold interior `._-/` to spaces **for matching
   only**; collapse whitespace. ~8 lines.
2. `dropStopWords(terms)` — remove `the a an please open launch start run go to`
   unless that would empty the list. ~5 lines + the list.
3. Fix `leafIdFor` for app rows (§1.4a): strip a trailing `.desktop` before
   taking the leaf, so `apps.google-chrome.desktop` contributes
   `google chrome`, not `desktop`. ~3 lines. **This is a bug fix that stands on
   its own** — it removes a token that currently matches all 235 apps.
4. Fold the label through `searchableToken` when building `nameText` (§1.4b), so
   `nodejs`/`node.js` and `btop`/`btop++` behave. ~1 line.
5. Subsequence fallback in `matchesQuery` for terms ≥3 chars against the label,
   scored in a new low tier (`70`) in `searchScore` so exact and prefix hits
   still win. ~10 lines.
6. Optional: `Comment=` as a low-tier haystack (needs `mergeAppRows`,
   `Menu.qml:287-320`, to carry it — one extra field).

**Tests**: `test/shell.d/app-search-test.sh` is the template — a
`run_node_test` heredoc that `requireFromRoot('shell/services/AppSearch.js')`
(`test/shell.d/base-test.sh:120`). A sibling `menu-search-test.sh` asserting the
table in §4 is ~60 lines and would be expected by upstream.

**Risk**: low, and bounded. The normalizer runs on the *query*, never on
`filterText` (§1.5), so the search field still shows what was said and dmenu
still returns it verbatim. The corpus keeps its punctuation, so `Node.js`,
`btop++` and `Cities: Skylines` keep matching by their real names.

**Upstreamability**: high. The matcher functions are byte-identical to stock
(§ sources), so the patch is conflict-free against the owner's `disabled:`
carry, and there is standing demand: omacom/omarchy **#2074** ("Comprehensive
Fuzzy Search in Omarchy Menu … for Nested Items", closed) and **#2980** ("Extend
Walker fuzzy search to expose nested Omarchy menu actions", closed) are both
requests for the search to reach further. Neither is about punctuation, so a PR
titled *"Menu search: tolerate dictated punctuation and filler words"* is a new,
narrow, easily-reviewed contribution with a strong accessibility framing (it is
a voice-input fix, and it helps anyone using the on-screen keyboard too).

### B — Context-seeded dictation from hyprpad (DO THIS SECOND, UPSTREAM)

Three steps, in order:

**B0 — unblock the daemon** (minutes). `engine = "muse"` crash-loops the
installed binary (§2.1). Either `voxtype config set engine whisper` or install
the dev build. Nothing else in B is testable until this is done.

**B1 — the hyprpad half** (~10 lines, no Rust). Replace the literal command at
`config/hyprpad.lua:238` with a `hyprpad-dictate` script that switches on
`$HYPRPAD_MODE` (§3.3). This is the *whole* hyprpad change; it is free, it needs
no new hyprpad feature, and it can land today as a no-op wrapper so the plumbing
is proven before there is anything to switch on.

**B2 — the voxtype half** (the real work, ~150-250 lines of Rust, upstream).
Contribute stage 1 of **#696**:

- `Profile` gains `vocabulary: Vec<String>` (and/or `initial_prompt: Option<
  String>`) — `src/config/profile.rs:50-64`.
- The profile override must be consumed **before** transcription, not at output
  time — today it is read at `src/daemon.rs:2601`.
- `Transcriber::transcribe` needs a per-utterance prompt (a new trait method or
  a `set_vocabulary` hook) — `src/transcribe/mod.rs:102-105`.
- Whisper: join the terms into `initial_prompt`, **and cap it**: truncate to
  ~700 chars / ~224 tokens keeping the *front*, since whisper.cpp otherwise
  keeps the tail (§2.4).
- Muse: send the terms as a real array — fix
  `request_obj["keywords"] = json!([ctx])` → `json!(terms)` at
  `src/transcribe/muse.rs:137-140` and `:188-190`.

Then `[profiles.launcher] vocabulary = [...]`, generated by a small
`omarchy-menu`-aware script or just hand-curated to the 50 apps the owner
actually launches.

**Effort/benefit**: B2 is a week of upstream work for a soft accuracy win
(better recognition of app names) that does **not** by itself fix the reported
symptom (§3.4, last paragraph). Its real payoff is elsewhere — dictating into a
code editor with the project's identifiers seeded, into a terminal with command
names seeded. Worth doing; wrong thing to do first.

### C — Both, in that order

A alone fixes the reported failure completely and benefits every Omarchy user.
B alone does not fix it. **A, then B1 as cheap plumbing, then B2 upstream when
there is appetite.**

### Explicitly not recommended

- **A trailing-punctuation strip in voxtype** (post_process, a profile, or a
  `[text]` flag) — the owner already ruled it out, and correctly: it is a global
  text mutation to work around one consumer's parser, and it would break
  dictating actual sentences everywhere else.
- **An embedding model in the launcher** — §4. Overkill by orders of magnitude
  for 462 strings that already carry hand-written synonyms.
- **Config-swap-and-restart for per-context prompts** — upstream #519 measures
  it at "several seconds" per switch, on a keypress path.
- **Guarding `guide+a` into `omarchy-ui`** — impossible for a chord (§3.2), and
  it would delete dictation everywhere else.

---

## 6. Test plan the owner can run by voice

Prerequisite (**must**, §2.1): `voxtype.service` is crash-looping. Confirm it is
up before anything else:

```sh
systemctl --user status voxtype        # expect: active (running), not activating
voxtype status                         # expect: idle
```

### 6.1 Baseline — reproduce the failure (before any patch)

Open the launcher (`guide+menu`, or Super+Alt+Space), then `guide+a`, say the
phrase, `guide+a` again, and read the search field and the row count.

| # | say | expect **today** | expect after **A** |
|---|---|---|---|
| 1 | "Chrome." | field `Chrome.`, "No matches for “Chrome.”" | Google Chrome selected |
| 2 | "Spotify." | field `Spotify.`, no matches | Spotify selected |
| 3 | "Open Spotify." | field `Open Spotify.`, no matches | Spotify selected |
| 4 | "The browser." | field `The browser.`, no matches | Chromium + Google Chrome (+3 Avahi) |
| 5 | "Terminal." | no matches | Alacritty, Ghostty, … |
| 6 | "Chrome" (no period — pause before releasing) | **1 match** — proves the period is the whole story | unchanged |

Test 6 is the control. If it matches and test 1 does not, §1.3 is confirmed live
and the diagnosis is settled before a line is written.

### 6.2 Regression checks after patch A (type these, do not dictate)

| type | must still | why |
|---|---|---|
| `chrome` | match Google Chrome only | no widening of the common case |
| `btop++` | match btop++ | punctuation in labels survives |
| `node.js` and `node js` | both match Node.js | fold, don't strip |
| `desktop` | match **0-2** apps, not 235 | §1.4a fixed |
| `google-chrome` | match Google Chrome | §1.4a fixed |
| `style` / `theme` | reach the same nested menu rows as before | drilldown search untouched |
| `.` (a lone period) | not match everything | empty-after-normalise must mean "no filter", not "match all" |

### 6.3 If B2 is ever built

Seed `[profiles.launcher] vocabulary` with 40-60 names, wire `hyprpad-dictate`,
then dictate the *awkward* names — the ones Whisper gets wrong, not the ones the
filter drops: "Ghostty", "Alacritty", "Xournal++", "cliamp", "btop". Measure the
transcript text, not the match count; a win looks like `Ghostty` instead of
`Ghost E` or `Gaudi`. Confirm the prompt actually fits: log the joined string's
length and keep it under ~700 chars (§3.4).

---

## 7. Sources

Upstream issues and docs:

- voxtype #696, *Dictation cleanup pipeline* (stage 1 = "Profiles gain a term
  list driving Whisper `initial_prompt`"):
  <https://github.com/peteonrails/voxtype/issues/696>
- voxtype #519, *Engine, model, language and streaming configurable per profile*
  ("per-record overrides cover only `--model`"; "several seconds" for a
  config-swap restart): <https://github.com/peteonrails/voxtype/issues/519>
- voxtype docs — `initial_prompt`: <https://voxtype.io/docs/CONFIGURATION>,
  <https://github.com/peteonrails/voxtype/blob/main/docs/USER_MANUAL.md>
- voxtype project page (engines; "Punctuation, capitalization, and inverse text
  normalization out of the box"): <https://voxtype.io/>
- Whisper prompt ceiling — 224 tokens, `n_text_ctx/2`, earlier tokens silently
  dropped: <https://github.com/openai/whisper/discussions/1386>,
  <https://github.com/openai/whisper/discussions/1824>,
  <https://github.com/ggml-org/whisper.cpp/discussions/348>
- Omarchy menu-search demand: omacom/omarchy #2074
  <https://github.com/omacom/omarchy/issues/2074>, #2980
  <https://github.com/omacom/omarchy/issues/2980>

Omarchy shell — `~/code/omarchy-bluetooth-friendly-name/` (matcher functions
identical to `/usr/share/omarchy/shell/`):

- `shell/plugins/menu/MenuModel.js:290-292` `searchableToken`; `:294-297`
  `leafIdFor`; `:299-305` `nameSearchText`; `:307-313` `termInSearchWords`;
  `:315-321` `descriptionTextMatches`; `:323-339` `matchesQuery`; `:341-363`
  `searchScore`
- `shell/plugins/menu/Menu.qml:287-320` `mergeAppRows` (aliases ←
  `GenericName` + `Keywords`); `:293` `sortedEntries("")`; `:562-573` dmenu
  filter; `:618-645` search branch; `:721` `setFilter`; `:1070`
  `WlrLayershell.namespace: "omarchy-menu"`; `:1141-1142`, `:1167` key handling;
  `:1211-1213` displayed filter; `:1467` "No matches for …"
- `shell/services/AppSearch.js:23` `entrySearchText`; `:43` `entryAcronym`;
  `:52-63` `termMatches`; `:65-72` `allTermsMatch`; `:74-94` `fuzzyScore`;
  `:96-121` `sortedEntries` — **not used for querying in this shell**
- `shell/services/AppLibrary.qml:52-54`; `shell/Commons/Util.qml:107-121`
- `default/omarchy/omarchy-menu.jsonc` — 331 labels, 2977 chars
- `test/shell.d/app-search-test.sh`; `test/shell.d/base-test.sh:120`
  `requireFromRoot`

voxtype — `~/code/voxtype` (branch `dev`, uncommitted `muse` work):

- `src/cli/record.rs:15-180` `RecordAction` (`--profile`, `--model`, output
  overrides — **absent from the installed 1.0.0**)
- `src/app/record.rs:71-100` writes `profile_override`;
  `src/daemon.rs:349-392` reads/consumes it; `:2601-2614` applies it at output
  time; `:879` `Daemon::new(config, …)` — config loaded once
- `src/config/profile.rs:50-64` the whole `Profile` struct (three fields)
- `src/config/root.rs:253-259` `runtime_dir`
- `src/config/whisper.rs:169` `initial_prompt`; `src/cli/root.rs:139`
  `--initial-prompt`; `src/app/overrides.rs:111-112`
- `src/transcribe/whisper.rs:30,75,117-120` `params.set_initial_prompt`;
  `src/transcribe/remote.rs:184-186`; `src/transcribe/mod.rs:102-105`
  `Transcriber::transcribe` (no per-call prompt)
- `src/transcribe/muse.rs:34,80,137-140,188-190` `context` → `keywords: [ctx]`
  (single-element array — bug); `src/config/engines/muse.rs:1-14,55-58`
- `src/config/text.rs:9-34` `TextConfig` — no punctuation/casing controls;
  `src/text/mod.rs` — fillers, spoken punctuation, replacements only
- `src/output/post_process.rs:96-98` `VOXTYPE_CONTEXT`
- `CLAUDE.md:346,348` — the 1.2.0 / 1.3.0 roadmap entries quoted in §2.6
- `docs/CONFIGURATION.md:946-996`; `docs/USER_MANUAL.md:1136-1195`

hyprpad — this repository (`e1afe64`):

- `config/hyprpad.lua:76-85` `omarchy-ui` layer allowlist; `:224-229` why guide
  chords are unguarded; `:238` `h.bind("guide+a", …, h.exec "voxtype record
  toggle")`
- `src/run.rs:2584` `execute(hypr, action, modes.state().active())`;
  `:2846-2861` `HYPRPAD_MODE` contract and `Action::Exec`
- `src/hypr.rs:212-231` `spawn_env` — `/bin/sh -c`, env-injected, detached
- `src/lua_config.rs:683-712` `Slot` / guard routing; `:1041-1049` gesture
  re-bind **overwrites**; `:1060-1078` button alternates
- `src/mode.rs:79` `layers: BTreeSet<String>`; `:283-302` layer tracking
- `docs/research/omarchy-menu-navigation.md` §1 — the layer-namespace table this
  report's `omarchy-menu` cite rests on

Live observations, 2026-09-02 (read-only):

- `pacman -Qo /usr/bin/voxtype` → `voxtype-bin 1.0.0-2`; `voxtype --version` →
  `1.0.0`; `/usr/bin/voxtype -> /usr/lib/voxtype/voxtype-avx512`
- `systemctl --user status voxtype` → `activating (auto-restart)`,
  `restart counter is at 3614`, `unknown variant 'muse'`
- `systemctl --user show-environment` →
  `OMARCHY_PATH=/home/ajg/code/omarchy-bluetooth-friendly-name`
- 235 `.desktop` files; 141 visible with a `Name`; 131 unique names / 1794
  chars; 74 with `GenericName=`; 49 with `Keywords=`; 61 `NoDisplay=true`
- `diff /usr/share/omarchy/shell/... ` → `AppSearch.js` identical,
  `MenuModel.js` differs by 36 lines (all `disabled:`), matcher functions
  identical

---

## Appendix — commands used

Every one is read-only. No recording was started, no menu was launched, nothing
under `~/.config` was written. The Node harness lived in `~/tmp/research/` and
was deleted.

```sh
voxtype --help ; voxtype record --help ; voxtype config --help ; voxtype info --help
voxtype --version                       # 1.0.0
cat ~/.config/voxtype/config.toml       # read only
systemctl --user status voxtype --no-pager
systemctl --user show-environment | grep -i omarchy
pacman -Qo /usr/bin/voxtype ; ls -la /usr/lib/voxtype/
hyprctl layers | grep namespace         # omarchy-background, -bar, -notifications only
gh issue view 519 --repo peteonrails/voxtype
gh issue view 696 --repo peteonrails/voxtype
gh search issues --repo omacom/omarchy fuzzy
git -C ~/code/voxtype status --porcelain ; git -C ~/code/voxtype log --oneline -8
diff /usr/share/omarchy/shell/plugins/menu/MenuModel.js \
     ~/code/omarchy-bluetooth-friendly-name/shell/plugins/menu/MenuModel.js
grep -h '^Name=' /usr/share/applications/*.desktop ~/.local/share/applications/*.desktop \
  | sed 's/^Name=//' | sort -u | wc -l          # 131, 1794 chars, 10 ms
grep -c '"label"' ~/code/omarchy-bluetooth-friendly-name/default/omarchy/omarchy-menu.jsonc
# and a throwaway Node harness that require()d the shell's own MenuModel.js /
# AppSearch.js against the real /usr/share/applications tree (§1.3, §1.4, §4)
```

Session left at baseline: `voxtype.service` still crash-looping as found (not
touched), layers `omarchy-background` + `omarchy-bar` + `omarchy-notifications`.
