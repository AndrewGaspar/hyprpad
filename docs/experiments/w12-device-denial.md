# W12 experiment log — denying Steam the controller device

Goal: find a mechanism by which hyprpad owns the puck's `hidraw` nodes and Steam
cannot open them, so Steam only ever sees a virtual controller hyprpad feeds
under focus routing ([06 Tier 1](../06-recommendation.md#tier-1--the-structural-answer)).
Three candidates from the research; this log records what each actually did.

Dated 2026-08-31, Framework 16 dev machine, unprivileged unless noted.

## Candidate A — systemd cgroup device controller  ❌ RULED OUT (unprivileged)

`systemd-run --user -p DevicePolicy=closed -p DeviceAllow='/dev/null rw'` around
a process that opens `/dev/hidraw7`:

- Baseline (no policy): open succeeds (expected).
- `DevicePolicy=closed`: **open still succeeds.** The restriction did not take
  effect from the user manager.

Consistent with the cgroup-v2 device controller requiring BPF-program
delegation that a user-session unit does not have. **A user-scoped hyprpad
cannot deny device access this way.** Might work from a *system* unit (hyprpad
as a system daemon) — untested, and it inherits the same "hyprpad must be
privileged" cost as the udev path without udev's robustness.

## Candidate B — mount-namespace mask (bwrap)  ⚠️ INCONCLUSIVE, mechanism sound

The mask itself is proven: `bwrap --dev-bind /dev /dev --dev-bind /dev/null
/dev/hidrawN …` over each puck node yields `1:3` (`/dev/null`, major:minor)
for all five nodes when statted inside the namespace. `scripts/steam-masked`
builds this from the live puck node list.

**But launching Steam under it is fragile and the live test was contaminated:**

- Steam is a re-exec chain: `/usr/bin/steam` → `steam.sh` → reaper → the
  `ubuntu12_32/steam` client, and its runtime uses its *own* nested bwrap
  (`pressure-vessel`/`srt-bwrap`) for games. Whether the outer mask namespace
  survives to the process that actually opens hidraw was **not** cleanly
  determined.
- Test procedure error: the masked launch was started while a previous Steam
  was still `-shutdown`ing; they raced on Steam's single-instance lock
  (`~/.steam/steam.pid`), and the process first inspected turned out to be the
  leftover `-shutdown` handler (running *outside* any namespace, holding the
  real `f3:7` nodes) — not the masked client. No valid read of the masked
  client's fds was obtained before cleanup.
- Cleanup was itself messy: `--die-with-parent` bwrap outlived the launching
  shell; killing the client out from under `steam.sh -shutdown` left it stuck
  holding the instance lock, blocking a clean relaunch.

**Lesson (independent of the mask result):** wrapping Steam's launch is
inherently brittle — the instance lock, the shutdown handshake, and the nested
pressure-vessel runtime all fight a naive `bwrap steam`. Even if the mask
works, this is an operationally fragile way to run it every session.

To retest properly: ensure Steam is fully down (no `steam.sh`, no pidfile)
*before* launching masked; then inspect `/proc/<client-pid>/root/dev/hidraw7`
of the process genuinely parented by the mask bwrap, not by pgrep name.

## Candidate C — udev permission override  ▶ NEXT, and now favoured

Not yet prototyped. The reasoning above promotes it: denying read on the node
itself is **re-exec-proof** — it does not matter how Steam forks, execs, or
nests namespaces, because the file permission travels with the inode, not the
process tree. Steam's own `60-steam-input.rules` grants `MODE="0660",
TAG+="uaccess"` to every `28de` hidraw by vendor id; an override ordered after
it would restrict the puck to a dedicated group/owner that only a privileged
hyprpad holds.

Costs (already in the programme): hyprpad must run privileged (system daemon or
setgid helper), because any permission a same-user session-hyprpad holds,
same-user Steam also holds. The udev `TAG-=`/uaccess-clearing question
(whether an inherited `uaccess` tag can be removed vs. only overriding
`MODE`/`GROUP`) still needs a live test. This is the next session's work, on a
clean rig — not against the live daily Steam.

## Interim conclusion

Ownership almost certainly means **hyprpad as a privileged daemon + a udev rule
that restricts the puck nodes to it**, with the bwrap mask as a
non-privileged-but-fragile fallback if udev proves unable to clear `uaccess`.
The cgroup path is out for a user daemon.

---

## Candidate C — udev permission override  ▶ INVESTIGATED 2026-08-31, architecture settled

Non-disruptive investigation (read-only `getfacl`/`udevadm info`, no Steam cycle,
no rule applied). Findings:

**How access is granted today (VERIFIED).** The puck's hidraw nodes carry
`TAGS=:uaccess:seat:`. The `uaccess` tag makes systemd-logind apply a POSIX
**ACL** granting the active-session user read/write:

```
# getfacl /dev/hidraw7
owner: root   group: root
user::rw-
user:ajg:rw-        <-- the uaccess ACL: granted to the SESSION USER
group::rw-
```

Both Steam's `60-steam-input.rules` (`TAG+="uaccess"` for any `28de` hidraw)
and the generic `70-uaccess.rules` contribute the tag. Access is **not**
group-based — it is an ACL keyed on the *session user*.

**The decisive consequence — same-user denial is impossible.** Steam and a
session-user hyprpad both run as `ajg`. The kernel's permission check cannot tell
them apart: any node permission or ACL that lets one open the device lets the
other open it too. Therefore **Tier 1 is not achievable with a session-user
daemon**. hyprpad *must* run under a different identity than the desktop session:
a system daemon (root) or a setgid helper in a dedicated group. This was listed
as a "cost" in [06](../06-recommendation.md#tier-1--the-structural-answer); the
investigation upgrades it to a **hard architectural requirement** that dictates
the daemon's shape.

**The mechanism (drafted, not yet live-tested).** Deny the session user by
removing the ACL and gating on a dedicated group — `scripts/99-hyprpad-claim-puck.rules`:

```
ACTION=="add|change", SUBSYSTEM=="hidraw", ATTRS{idVendor}=="28de", ATTRS{idProduct}=="1304",
  TAG-="uaccess", GROUP="hyprpad", MODE="0660"
```

`TAG-="uaccess"` strips the tag (so logind grants no ACL); `GROUP="hyprpad"` +
`MODE="0660"` restricts to a `hyprpad`-group process. `TAG-=` is supported in
systemd ≥ 250 (this machine runs systemd 261). Ordered `99-` to run after the
`60-`/`70-` rules that add the tag.

**Open (needs a live test WITH the user — sudo + one Steam cycle):**
1. Does `TAG-="uaccess"` actually clear the logind ACL on this systemd? (If not,
   the fallback is a rule ordered *before* `70-uaccess`/logind that sets
   `ENV{ID_SEAT}=""` or otherwise suppresses the tag source.) Verify with
   `getfacl` after `udevadm trigger` — the `user:ajg` ACL entry must be gone.
2. Does Steam, run as the session user, then genuinely fail to open the node?
   (Expected: yes — no ACL, not in `hyprpad` group.)
3. Can a `hyprpad`-group process still open it, and does concurrent access work
   the way the passive tap did?

These are quick but disruptive (they change device perms and require restarting
Steam), so they belong in a hands-on session, not an autonomous run. The
architectural result above does **not** depend on them.

## Final architecture (from the investigation)

hyprpad ships as a **privileged system component** in a `hyprpad` group, with a
udev rule that removes the puck's `uaccess` ACL and assigns it to that group.
The session user (hence Steam) is denied the physical device by construction;
hyprpad reads it and emits a focus-gated virtual controller. The cgroup path is
out (Candidate A); the bwrap wrapper is a fragile non-privileged fallback
(Candidate B); this udev rule is the mechanism, pending the three live checks.

### Candidate C — LIVE RESULT (2026-08-31): TAG-=uaccess FAILS to deny

Ran the three live checks (sudo + Steam cycle + physical replugs). The rule's
`GROUP="hyprpad" MODE="0660"` applied cleanly (node became `root:hyprpad 660`),
but **denial failed**: the logind `uaccess` ACL (`user:ajg:rw-`) survived every
attempt, including a clean replug with **Steam fully shut down and no process
holding the node**. A session-user `open()` still succeeded throughout.

Root cause, from the evidence:

```
TAGS=:uaccess:seat:      <- sticky tag set (udev database) — logind reads this
CURRENT_TAGS=:seat:      <- current-event set — what TAG-="uaccess" cleared
getfacl: user:ajg:rw-    <- ACL logind granted; never revoked
loginctl seat-status: 35 puck nodes still listed under seat0
```

`TAG-="uaccess"` in a `99-` rule removes `uaccess` from the *current-event* tag
set but NOT from the sticky `TAGS` persisted in the udev database, and
systemd-logind (v261) grants the seat `uaccess` ACL off the sticky tag. Ordering
after `60-steam`/`70-uaccess` doesn't help; a clean remove→add (physical replug,
Steam absent) doesn't help. **This mechanism cannot deny the session user.**

### Where Candidate C goes next (untested, deferred)

Not pursued live to avoid more disruption. Ranked candidates for a future
hands-on session:

1. **Strip the `seat` tag too** (`TAG-="seat" TAG-="uaccess"`) and/or override
   `ENV{ID_SEAT}` to a non-seat value, so logind stops managing the device as a
   seat-uaccess device at all. Same sticky-tag risk applies — verify `TAGS` (not
   just `CURRENT_TAGS`) actually loses the tags, else this fails identically.
2. **`OWNER`/`GROUP`/`MODE` alone is not enough** — ACLs are additive over the
   mode, so as long as the uaccess ACL exists the session user wins regardless
   of `MODE`. The ACL itself must be prevented.
3. **A logind-level override** (mask the device from logind's seat management) or
   a `systemd`/`udev` `OPTIONS` flag — needs research into how to suppress the
   ACL grant specifically.

### Reassessment: the namespace approach (Candidate B) is now the front-runner

Candidate B (bwrap mount-namespace mask) **sidesteps this entirely** — it hides
the node inside Steam's namespace, so logind's ACL is irrelevant. Its only
problem was operational fragility wrapping Steam's launch (the instance lock +
pressure-vessel re-exec), which is a more tractable engineering problem than
defeating logind's ACL. **Recommendation: pursue Candidate B (robustly this
time — fully stop Steam, launch masked, verify the client's namespace) and
treat udev as a fallback only if the seat-strip variant (C.1) proves to work.**

The architectural result is unchanged and independent of mechanism: hyprpad must
run privileged, and Steam must be denied the physical node by *some* means.

---

## Candidate B — LIVE SUCCESS (2026-08-31, second attempt): the namespace mask denies Steam

Redid the bwrap mask properly — Steam fully stopped first, masked launch tracked
by pid, and the **actual masked client's** namespace inspected (not a leftover).
Result: **it works.**

- All five puck nodes read `1:3` (`/dev/null`) inside the masked client's mount
  namespace (`/proc/<client>/root/dev/hidraw*`). ✓
- **Steam held zero puck hidraw fds** — the mask stopped every `open()`. ✓
- Steam degraded **gracefully**: `controller.txt` logged `Unable to open local
  device: /dev/hidraw11` once per node and **stopped** — 0 new lines over the
  next 6 s. No retry spam, no error loop, no crash. ✓
- **hyprpad read the real device** from outside the namespace throughout (live
  `0x42` reports). ✓

**This is the Tier-1 denial the udev approach could not deliver.** Steam is
denied the physical controller; hyprpad owns it; Steam copes cleanly.

### Nuances and the remaining work

1. **Steam still *enumerates* the puck** via `/sys` (visible in the namespace),
   so it logs "Local Device Found" before failing to open. Harmless here (one
   log line), but the node mask does not make the device *invisible* — only
   unusable. Optionally also mask `/sys/class/hidraw/hidrawN` (and the parent
   `/sys/.../0003:28DE:1304.*`) to hide it from enumeration entirely; not
   required given the graceful failure.
2. **Lizard mode stays on.** Because Steam can never open the hidraw, it cannot
   send the exit-lizard-mode feature report — so the puck keeps emitting its
   firmware keyboard/mouse (arrows + cursor) on its evdev nodes. With hyprpad
   *also* driving the cursor this means double input. **The real design must
   have hyprpad take ownership of lizard mode**: hyprpad opens the hidraw and
   writes the disable-lizard feature report itself (it now owns the device), or
   grabs/ignores the lizard evdev nodes. This is a new hyprpad responsibility
   surfaced by the test.
3. **Replug gap (unchanged):** the mask is a snapshot of node paths at launch;
   a puck replug creates fresh `hidrawN` nodes that are unmasked until Steam
   relaunches. Mitigation options: mask the parent USB `/sys` path, or a
   stable-by-id bind, or re-launch Steam on hotplug. Lower priority.
4. **Operational fragility (managed):** launching Steam under bwrap is clean
   *if Steam is fully stopped first* (no instance-lock race). Cleanup needs the
   whole session/process-group killed — `--die-with-parent` leaves
   double-forked grandchildren (webhelper, any Wine) orphaned; kill by session
   id / process group, not just the bwrap pid.

### W12 conclusion

**Mechanism chosen: the bwrap mount-namespace mask.** It denies Steam the puck
without root, without fighting logind's ACL (the udev dead-end), and Steam
tolerates it gracefully. hyprpad reads the real device unaffected. The
productionization work is: (a) a clean "launch Steam masked" wrapper that stops
any existing Steam first and manages the process group; (b) hyprpad taking
ownership of lizard mode so the firmware kbd/mouse doesn't fight the daemon;
(c) optionally hiding the device from `/sys` enumeration; (d) replug handling.
The earlier architectural finding stands — hyprpad owns the device and emits a
focus-gated virtual controller — but note this path does **not** require hyprpad
to run as root: the mask is unprivileged, and hyprpad reads the device via the
normal session `uaccess` ACL (which the udev investigation showed is
unavoidable anyway). That simplifies the daemon: **no privileged system service
required** — a user daemon plus a masked-Steam launcher suffices.
