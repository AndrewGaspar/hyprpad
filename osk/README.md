# hyprpad-osk

A controller-driven **on-screen keyboard for Hyprland** — the standalone crate
that grows into the Steam-Deck-parity OSK for the `hyprpad` project.

This is a **kickoff skeleton**: a runnable, architecturally faithful foundation
that renders a QWERTY keyboard as a `zwlr_layer_shell_v1` **OVERLAY** surface and
types into the focused app via a **uinput** keyboard, with the harder parts
clearly stubbed and TODO'd against the authoritative research in
`docs/research/osk-technology.md` (referenced below as **§**).

The crate is fully self-contained: it has its own `Cargo.toml` with an empty
`[workspace]` table so it is **never** pulled into the parent `hyprpad` crate,
and it touches nothing outside `osk/`.

## Build & test

```sh
cd osk
cargo build --release
cargo test
cargo clippy --release
```

## Run

```sh
# default: listen on $XDG_RUNTIME_DIR/hyprpad-osk.sock
hyprpad-osk
# or drive it from stdin (handy for scripting / the live test)
printf 'show bottom\ntype hello world\nhide\nquit\n' | hyprpad-osk --stdin
# drive the two pad cursors, toggle shift, and type "!" via the shifted 1 key:
printf 'show bottom\ncursor L -0.45 0.15\ncursor R 0.45 0.15\nshift stuck\nlayer symbols\nquit\n' | hyprpad-osk --stdin
# with word prediction (see "Prediction" below for how to build a model)
hyprpad-osk --model ~/.local/share/hyprpad-osk/en.model
```

The hyprpad daemon will eventually drive the socket, forwarding the controller's
two trackpad cursors and clicks.

### Hyprland config (compositor-side layer rules, §2.4)

The OSK picks a stable layer **namespace** (`hyprpad-osk`) so these rules — which
live in Hyprland config, not the client protocol — can match it:

```
# keep the OSK out of screen captures (§2.4, non-negotiable)
layerrule = no_screen_share, hyprpad-osk
# for a future lock-screen OSK (§2.4): render (1) and receive input (2)
# layerrule = above_lock 2, hyprpad-osk
```

## Renderer choice

**`smithay-client-toolkit` (SCTK) + a software shm buffer + `font8x8`.**

SCTK wraps the `zwlr_layer_shell_v1` create/configure/ack boilerplate and gives a
ready `SlotPool` shm buffer to draw into — the pragmatic choice the research
calls out (§8). Default features are **off**: no `calloop` (we run our own
`poll(2)` loop so the control-channel fd and the Wayland fd share one thread) and
no `xkbcommon` (the OSK takes no keyboard focus, so it needs no seat/keymap
handling). Labels are drawn with `font8x8`, a pure-Rust embedded 8x8 bitmap font,
so there is **no cairo/pango/fontconfig C dependency** — the whole crate stays
light and deterministic. The research flags CPU rendering as the weak point for
Deck-grade animated cursors/glow (§8); a GPU path is a deliberate later decision
(§4.7, deferred).

### Frame pacing — draw *when* the display is ready, not per command

The daemon forwards `cursor <L|R>` at roughly the controller's report rate
(~250 Hz per pad). Drawing a full software frame on **every** command pegs a core
and, worse, commits far faster than the display refresh — the flicker/flash the
per-pad cursors showed live. The draw path is therefore decoupled from the
command stream:

* **Dirty, not draw.** A `cursor`/highlight/shift/layer/reflow change only marks
  the affected panel `dirty`; it never draws inline. A move that changes neither
  the focused key nor the cursor's **integer** pixel is dropped outright (a
  render-side dedup on top of the daemon's `%.4f` one), so sub-pixel churn costs
  nothing.
* **Paced by the poll loop.** Each wakeup, `render_tick` draws a dirty panel at
  most once per `MIN_FRAME` (~165 Hz cap, this display's max). A burst of ~250 Hz
  updates thus coalesces to one draw per display frame.
* **Frame callbacks, with a timer fallback.** After a draw the surface requests a
  `wl_surface.frame` callback and the panel waits for it — the standard Wayland
  way to pace to the compositor's refresh and to stop when the surface is
  occluded. But a compositor is *not obliged* to send the callback when it is not
  repainting the surface (Hyprland withholds it from an OVERLAY layer sitting
  under a full-screen overlay), which would freeze the cursors. So the poll
  `timeout` also carries a `FRAME_FALLBACK` (~11 ms) deadline: if the callback
  has not arrived by then, the panel draws anyway. A fresh callback is requested
  only when none is outstanding, so the never-answered case does not pile up
  `wl_callback` objects. When nothing is dirty the loop blocks (0 % CPU).
* **Buffer-release discipline.** Every draw goes into a **fresh** `SlotPool`
  buffer attached via `Buffer::attach_to` (which `activate()`s it), so SCTK holds
  the slot until the compositor sends `wl_buffer.release`; `create_buffer` then
  always lands on a slot that is *not* on screen. Attaching the raw `wl_buffer`
  directly (as the kickoff did) bypassed that tracking and recycled a slot
  mid-scanout — a second, independent source of tearing.

Net effect under a ~250 Hz dual-pad stream: full-core CPU (≈99 %, ≈94 % split)
drops to ≈13 % (≈17 % split), with no visible flashing.

## Architecture

| Module | Responsibility |
|---|---|
| `layout` | The **shared key/keycode model** (QWERTY grid + evdev scancodes, §4.6) and the geometry engine that serves **both** modes from it. |
| `surface` | Layer-shell anchoring / exclusive-zone policy — bakes in OVERLAY-only, destroy-on-dismiss, the stable namespace. |
| `render` | CPU renderer: keys + labels + highlights into an ARGB8888 shm buffer. |
| `output` | The **uinput** keyboard (real evdev keycodes → types everywhere incl. XWayland), adapted from the parent crate's proven `src/output.rs`. |
| `control` | The line-based control channel (unix socket or stdin) with a dual-trackpad-shaped vocabulary. |
| `predict` | Word completion and next-word prediction: the mmap'd unigram/bigram model, the composition buffer of what the keyboard typed, the personal word cache, and the ranker over all three (`osk-prediction.md` phase 1). |
| `app` | Binds the globals, owns the live surfaces, runs the poll loop over Wayland + the control channel. |

### The layout data model — one model, two modes

There is exactly **one** `Keyboard`: the QWERTY key set where every key carries
its label, its **raw Linux/evdev keycode** (the same number the uinput backend
emits — the §4.6 grid), its grid row, and crucially its **`Hand`** (Left / Right
/ Either). Everything mode-specific is *geometry* derived from that model by
`LayoutEngine`; the keys and keycodes never change between modes.

* **Mode A — `BottomDeck`** (implemented): the full grid in one bottom-docked
  panel. The two trackpad regions are the left 55% / right 55% of the surface
  with a 10% overlap (§4.1/§4.9) — the absolute-cursor Deck model.
* **Mode B — `SideSplit`** (data model + basic render): the **same keys**
  partitioned by `Hand` into two **edge-docked columns** — left-hand keys
  (`…QWERT / ASDFG / ZXCVB`) dock the **left** screen edge, right-hand keys
  (`YUIOP / HJKL / NM…`) dock the **right** edge, and workspace content reflows
  into the centre strip between them. The **left trackpad addresses the left
  column, the right trackpad the right column**, so the Deck's two-region model
  maps onto physical screen geography (each thumb → its own side). `Hand` is the
  single field that makes this fall out of the shared model.

The `surface` module turns a mode into panels: Mode A = one bottom panel; Mode B
= two panels (`LeftColumn` + `RightColumn`), one exclusive zone per edge. Two
surfaces (not one full-width surface with a transparent hole) because an
exclusive zone is a single scalar along one edge — you cannot carve a gap in the
middle of one surface, so genuine centre reflow *requires* an independent
exclusive zone on each edge.

**Reflow vs overlay** is an orthogonal axis: `reflow` sets an exclusive zone so
Hyprland shrinks the tiled area (the pan-up / side-squeeze feel); `overlay` sets
a zero exclusive zone to float over a fullscreen game. Both always use the
OVERLAY layer tier. **Overlay/float is the default**; reflow is opt-in. The
policy is live-toggleable without losing shift/layer state — from the control
channel (`reflow on|off`) or the on-screen `Push`/`Float` meta key — by
destroying and recreating the surface(s) with the flipped exclusive zone (in
split mode the same toggle governs both columns' zones together).

### Control-channel vocabulary

Line-based, designed with the **dual-trackpad future** in mind (§4.1/§4.5):

```
show <bottom|split> [reflow|overlay]   create + render the surface(s); OVERLAY is the default
hide                                    DESTROY the surface(s) — never unmap
cursor <L|R> <nx> <ny>                  pad absolute position, each axis in [-1,1]
commit <L|R>                            commit the key under that pad's cursor (click-down)
shift <off|oneshot|stuck|on>            set the latched shift/caps state (on = stuck / caps-lock)
shift <down|up>                         hold / release a physical Shift (momentary; the latch is untouched)
layer <base|symbols|toggle>             switch the base QWERTY ↔ numeric/symbols page
reflow <on|off>                         displace (on, exclusive zone) vs overlay/float (off)
key <keycode>                           commit a raw evdev keycode directly
type <text>                             type an ASCII string
candidate accept [n]                    accept the highlighted (or nth) suggestion — R1
candidate next|prev                     move the strip's highlight — L1
context reset                           forget the typed context (focus change)
learn on|off                            gate the personal word cache
predict on|off                          show / hide the suggestion strip entirely
forget                                  delete every learned word
quit                                    exit
```

`cursor`/`commit` speak the per-pad model the daemon forwards. `commit` respects
the live shift state, so `shift stuck` (or committing the on-screen Shift/Caps
key) then `commit`-ing `1` types `!`. `shift down` … `shift up` is the daemon
holding a trigger as a physical Shift (the Deck's L2): every legend draws shifted
and every commit types shifted for exactly as long as it is down, and the latched
state comes back untouched on release — a one-shot latched underneath still
clears after one character. `shift`, `layer`, and `reflow` let the daemon
drive/persist state the keyboard can also toggle from its own meta keys
(`?123`/`ABC` = layer, `Push`/`Float` = reflow). `display <overlay|displace>` is
accepted as an alias for `reflow`.

**Overlay is the default** now: a bare `show bottom` floats over content (zero
exclusive zone); pass `reflow` — or send `reflow on`, or press the on-screen
`Push` key — to reserve an exclusive zone and displace/reflow the workspace.

Bursts of lines written together (e.g. two `cursor` lines in one write) are all
processed on the same wakeup on **both** transports — stdin is set non-blocking
and drained in full — so the two pad cursors never lag a step behind.

### Events back to the daemon (stdout)

Commands come **in**; a small stream of machine-readable events goes **out on
stdout**, one per line:

```
event crossed <L|R>          # that pad's cursor moved onto a NEW key
event candidate <n> <text>   # the suggestion strip's highlight moved
```

Human-oriented logs stay on **stderr** (every `hyprpad-osk:` line), so a parent
that pipes stdout gets a clean event stream and still sees the logs.

`crossed` exists because responsibility for the Deck's key-crossing **haptic
tick** (§4.3) is split: the OSK owns the layout and the hit-test, so only it
knows when the cursor changes key — but the daemon owns the controller's
writable `hidraw` node, so only it can pulse the actuator. The line is emitted
**only on an actual change of focused key**, and never for a crossing onto a gap
(the Deck's "double-thunk" fix), so the daemon can tick once per line with no
filtering of its own. A layer switch or a resize re-derives focus without moving
the finger and deliberately emits nothing.

The daemon (`src/osk.rs`) pipes stdout, reads it on a thread, and turns each line
into a `tick` on that pad — gated by the `[haptics] crossing` config knob. Its
parser ignores any line it does not recognize, so this crate can add events (or
print anything else) without breaking an older daemon.

## Prediction

Word completion and next-word prediction, phase 1 of
`docs/research/osk-prediction.md` (referenced below as **§**). A three-candidate
strip sits above the top key row; `R1` accepts the highlighted suggestion and
`L1` moves the highlight along, and either pad can hover and click a slot.

**With no model installed there is no strip at all** — no extra panel height, no
learning, no behaviour change of any kind. Prediction is opt-in by building the
model.

### The model

A small in-process predictor, not an adopted engine: nothing in the Linux
ecosystem is both maintained and shaped for this (§3). The model is a
**unigram + per-word bigram-successor table**, mmap'd and used in place:

* an `fst::Map` lexicon (word → id, ids in the fst's own sorted order),
* one `u8` unigram probability per id,
* per-word successor lists — delta-varint ids plus a `u8` score, capped at 32.

Both scores are on AOSP LatinIME's log scale (255 = probability 1, each step a
factor of 1.15), so unigram and bigram are directly comparable and **stupid
backoff** (Brants et al. 2007) is one branch:

```text
S(w | prev) = c(prev,w) / c(prev)   when the bigram is stored
            = 0.4 · P(w)            otherwise
```

The AOSP ranking rules the doc names are kept (§2.1/§8.6): the **typed word is
always candidate 0** when it is a real word or when nothing else is confident,
candidates are deduplicated, and **nothing is ever auto-replaced** — with an
exact cursor and a deliberate click, what you typed is evidence, not noise.

Measured on the shipped 60,000-word model: a worst-case query (a one-letter
prefix after a common word) takes **~82 µs**, against a 1 ms budget (§7.3).
Candidates are recomputed on *commits* only — a few times a second — never on
cursor motion, so prediction never competes with the frame path.

### Data, and its licence

| Part | Source | Licence |
|---|---|---|
| Unigram probabilities | [wordfreq](https://github.com/rspeer/wordfreq) `large_en` | **CC BY-SA 4.0** (data) |
| Validity + casing | SCOWL / Hunspell `en_US` (LibreOffice dictionaries) | MIT-like |
| Bigram successors | [Leipzig Corpora Collection](https://wortschatz.uni-leipzig.de/) English news + Wikipedia | **CC BY 4.0** |

**The built model is CC BY-SA 4.0**, because wordfreq's data is share-alike. The
code stays MIT/Apache-2.0 — the licence applies to the data artifact, not to the
program that reads it. Google Books Ngrams v3 (CC BY 3.0) is the research doc's
first choice for bigrams and is ~230 GB for English 2-grams alone; the builder
reads a locally downloaded subset of it with `--books-dir`, so that upgrade is a
download away rather than a code change.
`tools/build-model/DATA-LICENSES.md` has the full analysis and the sources that
were deliberately **not** used.

### Building the model

Two steps, so the format-critical half is ordinary Rust with ordinary tests and
the network-and-gigabytes half is a script you run once:

```sh
# 1. download + merge the data into a plain-text word list (~1.4 GB of downloads,
#    cached under --cache; a few minutes)
python3 tools/build-model/build-model.py --out ~/tmp/osk-prediction/en.words

# 2. turn that into the binary model the keyboard maps
mkdir -p ~/.local/share/hyprpad-osk
cargo run --release --bin hyprpad-osk-build-model -- \
    ~/tmp/osk-prediction/en.words ~/.local/share/hyprpad-osk/en.model \
    --attribution-file tools/build-model/ATTRIBUTION.txt
```

That is 1.69 MB for 60,000 words and 386,894 bigrams, and it is reproducible
— both halves of the pipeline are deterministic, so the same sources always
produce the same file. The keyboard finds it at
`$XDG_DATA_HOME/hyprpad-osk/en.model` (then `$XDG_DATA_DIRS`), or wherever
`--model` / `$HYPRPAD_OSK_MODEL` says. **The artifact is not committed to git**;
`data/fixture.words` and `data/fixture.model` are a ~130-word hand-authored
fixture the test suite predicts against, so no test ever needs the download.

The build is deterministic — the same word list always produces the same bytes —
and `tests/fixture_model.rs` checks the committed fixture against a rebuild. If
you change `data/fixture.words`, rebuild `data/fixture.model` the same way.

### Controls

| Control | Does |
|---|---|
| `R1` | accept the highlighted candidate: types the rest of the word plus a space |
| `L1` | move the highlight one slot along, wrapping |
| either pad | hover a slot and click/trigger it, like a key |

The highlight defaults to the best *suggestion*: when the typed word occupies
slot 0, the highlight starts on slot 1, so `R1` is never a no-op that retypes
what you just typed (Gboard's convention, §7.2). Accepting types only the
characters that differ — "hel" + "hello" costs `l`, `o`, space — which matters
when every tap is ~12 ms of keystroke replay, and the backspaces it does emit
only ever remove characters this keyboard typed in this session (§8.6). After
an accept the strip immediately shows next-word predictions, so `R1`-`R1`-`R1`
chains a phrase.

The daemon sends `candidate accept` / `candidate next`; the OSK reads no
controller input of its own. They are bound as `osk accept` / `osk next` in the
daemon's keyboard button table, on R1/L1 by default and rebindable like any
other (`h.osk_button("r4", h.osk "accept")`).

### Context

Phase 1 has **no view of the focused application's text buffer** (§1), so the
context is exactly what this keyboard typed since it was shown: one string, with
Backspace popping one character. Every path that types feeds it — a committed
key, the daemon's `key <code>` (Y = Space, X = Backspace, R2 = Enter all reach
it), an accepted candidate, and the scripted `type <text>`. It is reset on
`show`, on `hide`, and on `context reset`, which the daemon sends on every focus
change while the keyboard is up — and on any keystroke it cannot account for:
the `<`/`>` arrow keys move the text cursor, and a modified chord like
Ctrl+Backspace eats a whole word, so after either the buffer no longer describes
what is in front of the cursor. Resetting is the honest answer; completing
against a stale prefix is not.

### Learning, and its gates

The keyboard remembers words you commit, in
`$XDG_DATA_HOME/hyprpad-osk/learned.json` (mode 0600, human-readable, delete it
whenever you like). AOSP's forgetting curve: a word is **only offered after a
second sighting** (`MIN_VISIBLE_LEVEL = 2`) and loses a level per 15 idle days,
so a word typed once never surfaces and an unused one fades.

Every gate the research doc lists (§5.3) is in place:

* **Shape.** Never a token with a digit or a symbol, never one over 24
  characters, never a single character. That alone keeps API keys, PINs and card
  numbers out — and a hand-edited store cannot smuggle one back in, since rows
  are re-filtered on load.
* **Window.** The daemon sends `learn off` when focus lands on a class in
  `[keyboard] learn_deny` / `h.keyboard_config { learn_deny = {...} }` —
  password managers, polkit agents, the lock screen, and terminals by default.
  The gate is remembered while the keyboard is down and re-stated on `show`, so
  a keyboard raised *inside* a password manager starts gated.
* **Provenance.** A word typed by the daemon's scripted `type <text>` is context
  but is never learned from (§8.5 rule 4).
* **Completion.** Only a word closed by a separator or an accepted candidate is
  ever offered to the cache.

`forget` deletes every learned word and removes the file. `predict off` hides
the strip entirely without unloading the model.

### What is next

* **Phase 2 — real context.** Bind `zwp_input_method_v2` opportunistically and
  **never** with `grab_keyboard` (§6.4): its value is not typing but
  `content_type` (password/PIN/sensitive → suggestions and learning off),
  `surrounding_text` (real context after a focus change), and
  `commit_string`/`delete_surrounding_text` for clean whole-word replacement in
  apps that speak text-input-v3. fcitx5 holds the single IME slot on the dev
  machine, so the cleaner long-term fix may be a HypXRland-side channel that
  exposes the focused text-input's content type regardless (§6.6).
* **Phase 3 — quality.** Trigram successors, and a ~1 M-parameter GRU rerank in
  `candle`/`tract` on a worker thread with a 16 ms deadline. Gboard's own
  numbers put the neural lift at ~3.5 points of top-1 over n-grams (§2.7), so
  this is a rerank, not a foundation.

## Done / Stubbed / Deferred — mapped to `osk-technology.md`

### Done (implemented + live-verified on Hyprland 0.56.2)

- **OVERLAY layer tier, never TOP** (§2.1). Verified: `hyprctl layers` lists
  namespace `hyprpad-osk` at overlay level 3.
- **Destroy the layer surface on dismiss, never hide** (§2.3). Verified: after
  `hide`, the OSK-owned overlay entry (its real pid) is gone from
  `hyprctl layers`.
- **Mode A bottom exclusive-zone reflow AND overlay (zero zone)** as a parameter
  (task Mode A; §2.3). Verified: bottom panel `2048x435` with exclusive zone.
- **Mode B two edge-docked columns** with per-edge exclusive zones (task Mode B).
  Verified: `show split` → left `450x1254@0` + right `450x1254@1598`.
- **Shared QWERTY key/keycode model** with the §4.6 grid + PC scancodes, drawn as
  a legible grid; one model serving both modes via `Hand`.
- **uinput injection with real evdev keycodes** (§3.1, Q14), reusing the parent's
  proven backend. Verified: device enumerated by Hyprland as seat keyboard
  `hyprpad-osk-virtual-keyboard`; sending `key 42/29/56/125` produced exactly
  those keycodes on the device's evdev node (control → uinput → evdev proven).
- **Control channel** (unix socket + stdin) with the dual-trackpad vocabulary.
- **Stable namespace** for the `no_screen_share` / `above_lock` layer rules
  (§2.4).
- **Absolute dual-trackpad cursor mapping + hit-testing** scaffold (§4.1 formula:
  `p = 0.5(1 ± clamp(v·scale, -1, 1))`, absolute, no accumulator) wired to
  `cursor`/`commit`.
- **Visible per-pad cursor sprites + per-pad key highlight** (§4.1/§4.5). Each
  `cursor <L|R>` draws that pad's cursor sprite (a `cursor_size`-diameter disc:
  pad-coloured body, `cursor_stroke` contrast halo + aim dot) at its mapped
  position and highlights the key under it in the pad's colour — **both pads at
  once** (left blue, right orange). Live-verified: two cursors on the bottom
  panel simultaneously, each over its highlighted key.
- **Shift / Caps state `{Off, OneShot, Stuck}`** (§4.6) with **visible
  indication and case-correct legends**. Tapping Shift cycles
  `Off→OneShot→Stuck→Off`; Caps (and `L3`) toggles `Off↔Stuck`; a one-shot
  clears after one character. When active the Shift/Caps keys fill with the
  `shift_active` token and every legend switches to its shifted glyph (letters
  uppercase, `1`→`!`, …); the committed keycode always matches the drawn glyph.
  Live-verified: `shift stuck` → uppercase + shifted number row + lit Shift/Caps.
- **Held shift** (§4.6's `Held` bit) — `shift down` / `shift up` from the daemon
  is a momentary physical Shift (its L2 binding): the level is
  `held || latched` (`ShiftModel`), legends re-render shifted while it is down,
  the latch is left exactly as it was, and `hide` drops the held bit so it can
  never outlive the keyboard.
- **Numeric / symbols layer** (§4.6 "Layers") — a second key set (`Keyboard::
  symbols()`) reached via the `?123`/`ABC` meta key or `layer <base|symbols|
  toggle>`. It mirrors the base grid geometry exactly, so switching never resizes
  a surface; digits + the fuller punctuation set live here (some via
  `Key::force_shift`, e.g. `!`/`{` commit their shifted glyph directly). The full
  shifted QWERTY symbol set (`! @ # … ~`) is also reachable on the base layer via
  Shift. Live-verified: symbols page renders in both modes with the toggle key.
- **Overlay-by-default + live reflow toggle** (task; §2.3). Bare `show` floats
  (zero exclusive zone); `reflow on|off` / the `Push`/`Float` meta key recreates
  the surface(s) with the flipped zone, preserving shift/layer. Split mode
  toggles both columns together.
- **Meta key commits**: `?123`/`ABC` (layer), `Push`/`Float` (reflow), and the
  arrow keys (real keycodes 105/106) now act on commit.

### Stubbed (structure in place, behaviour minimal)

- **Mode B render** — geometry/anchoring correct; keycap render is basic, final
  ergonomics deferred (§4).
- **Physical chording** (§4.6) — the held shift bit is tracked (see Done), but
  only Shift: there is no held Ctrl/Alt, since every input is a discrete commit.
- **Concurrent per-source highlights** (§4.5) — the two pad highlights + cursors
  are driven; the third `Highlight{Focus}` source (a d-pad/stick focus cursor)
  is wired through the renderer but not yet driven.
- **Emoji / close meta keys** — present in the model with `keycode 0`; commit is
  a no-op (§4.6 "Layers"). Layer/reflow/arrow meta keys DO act (see Done).

- **Per-key-crossing haptic tick** (§4.3), as far as this crate can take it: the
  crossing is detected here and reported as `event crossed <L|R>` on stdout, with
  the tick suppressed on key→gap; the daemon owns the actuator and fires the
  pulse. See "Events back to the daemon" above.

### Deferred (clear TODOs → research section)

- **Cached-rect re-hit-test** (§4.9 item 3) — the hit-test still scans the placed
  keys per move rather than short-circuiting on the last key's rect.
- **Commit-on-click-down nuances**: extended-character popups commit on up / open
  at 450 ms; backspace auto-repeat 450→200 ms; rollover (§4.2/§4.6).
- **Long-press extended-character popups** (§4.6 — keys carry `extended_keys`, a
  450 ms hold opens an accent/variant row addressable by either pad). Not
  implemented: the input model here is discrete commits with no hold timer, and
  the uinput backend is ASCII/US-only so most accented variants can't be emitted
  anyway. The reachability need it targeted is instead met by the numeric/symbols
  layer + Shift, which together cover the full ASCII punctuation set. TODO when a
  hold timer + a Unicode-capable backend (`zwp_virtual_keyboard_v1`, below) land.
- **`gamescope_input_method` second injection backend** for nested games —
  `set_string`/`set_action` + `commit(serial)` over `$GAMESCOPE_WAYLAND_DISPLAY`
  (§2.5); the keymap-swap route is dead through gamescope.
- **Full-Unicode into native apps** via `zwp_virtual_keyboard_v1` with a generated
  keymap that keeps an interpret-bearing modifier (to survive Hyprland's
  re-serialisation, §3.2), destroying the VK after each burst (§3.3B).
- **Optional IM-v2 binding**, runtime-detected because fcitx5 may own the single
  IME slot; on `unavailable`, fall back + set
  `device{ name=hl-virtual-keyboard-<osk> keybinds=false }` (§3.3A, §7).
- **`above_lock` lock-screen OSK** (§2.4).
- **Themes**: CSS-custom-property-equivalent token table on a root theme name +
  per-key/row/col hooks; glow/pulse animations (§4.7).
- **Vertical (Mode B) final ergonomics** and a GPU renderer for Deck-grade
  animation (§8).

## Live-verification notes

Verified on this machine (Hyprland 0.56.2, HypXRland snapshot). Two
environment quirks worth recording:

- This HypXRland fork retains **stale `hyprctl layers` entries with `pid=-1`**
  after a client dies — a compositor-side artifact, not an OSK leak. Destroy is
  proven by tracking the OSK's **real pid**: its overlay entry disappears after
  `hide` and after process exit.
- Full keystroke-into-a-target-window capture wasn't demonstrated because this
  fork would not let the agent programmatically de-focus its own terminal; the
  injection path is instead proven at the evdev level (correct keycodes on the
  device node) plus seat enumeration, on top of the parent crate's already
  Q14-validated end-to-end uinput result (types correctly incl. XWayland).

## Licence

MIT OR Apache-2.0 for the code.

The **prediction model** is a separate artifact with its own terms — it is built,
not committed, and it is CC BY-SA 4.0 because wordfreq's data is share-alike.
See `tools/build-model/DATA-LICENSES.md` and `tools/build-model/ATTRIBUTION.txt`,
which is also baked into every model file's header.
