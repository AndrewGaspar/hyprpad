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
