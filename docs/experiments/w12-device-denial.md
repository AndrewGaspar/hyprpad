# W12 experiment log — denying Steam the controller device

Goal: find a mechanism by which hyprsc owns the puck's `hidraw` nodes and Steam
cannot open them, so Steam only ever sees a virtual controller hyprsc feeds
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
delegation that a user-session unit does not have. **A user-scoped hyprsc
cannot deny device access this way.** Might work from a *system* unit (hyprsc
as a system daemon) — untested, and it inherits the same "hyprsc must be
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
hyprsc holds.

Costs (already in the programme): hyprsc must run privileged (system daemon or
setgid helper), because any permission a same-user session-hyprsc holds,
same-user Steam also holds. The udev `TAG-=`/uaccess-clearing question
(whether an inherited `uaccess` tag can be removed vs. only overriding
`MODE`/`GROUP`) still needs a live test. This is the next session's work, on a
clean rig — not against the live daily Steam.

## Interim conclusion

Ownership almost certainly means **hyprsc as a privileged daemon + a udev rule
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
session-user hyprsc both run as `ajg`. The kernel's permission check cannot tell
them apart: any node permission or ACL that lets one open the device lets the
other open it too. Therefore **Tier 1 is not achievable with a session-user
daemon**. hyprsc *must* run under a different identity than the desktop session:
a system daemon (root) or a setgid helper in a dedicated group. This was listed
as a "cost" in [06](../06-recommendation.md#tier-1--the-structural-answer); the
investigation upgrades it to a **hard architectural requirement** that dictates
the daemon's shape.

**The mechanism (drafted, not yet live-tested).** Deny the session user by
removing the ACL and gating on a dedicated group — `scripts/99-hyprsc-claim-puck.rules`:

```
ACTION=="add|change", SUBSYSTEM=="hidraw", ATTRS{idVendor}=="28de", ATTRS{idProduct}=="1304",
  TAG-="uaccess", GROUP="hyprsc", MODE="0660"
```

`TAG-="uaccess"` strips the tag (so logind grants no ACL); `GROUP="hyprsc"` +
`MODE="0660"` restricts to a `hyprsc`-group process. `TAG-=` is supported in
systemd ≥ 250 (this machine runs systemd 261). Ordered `99-` to run after the
`60-`/`70-` rules that add the tag.

**Open (needs a live test WITH the user — sudo + one Steam cycle):**
1. Does `TAG-="uaccess"` actually clear the logind ACL on this systemd? (If not,
   the fallback is a rule ordered *before* `70-uaccess`/logind that sets
   `ENV{ID_SEAT}=""` or otherwise suppresses the tag source.) Verify with
   `getfacl` after `udevadm trigger` — the `user:ajg` ACL entry must be gone.
2. Does Steam, run as the session user, then genuinely fail to open the node?
   (Expected: yes — no ACL, not in `hyprsc` group.)
3. Can a `hyprsc`-group process still open it, and does concurrent access work
   the way the passive tap did?

These are quick but disruptive (they change device perms and require restarting
Steam), so they belong in a hands-on session, not an autonomous run. The
architectural result above does **not** depend on them.

## Final architecture (from the investigation)

hyprsc ships as a **privileged system component** in a `hyprsc` group, with a
udev rule that removes the puck's `uaccess` ACL and assigns it to that group.
The session user (hence Steam) is denied the physical device by construction;
hyprsc reads it and emits a focus-gated virtual controller. The cgroup path is
out (Candidate A); the bwrap wrapper is a fragile non-privileged fallback
(Candidate B); this udev rule is the mechanism, pending the three live checks.
