#!/usr/bin/env python3
"""Passive HID tap for the 2026 Steam Controller (Valve 28de:1304 "Puck").

Opens the puck's hidraw nodes read-only *alongside* Steam (hidraw is not
exclusive) and logs every input report with a monotonic timestamp.

  capture  -- record raw reports to a .jsonl log while you drive the controller
  live     -- print a running diff of which bytes changed, for interactive poking
  analyse  -- post-process a capture: which byte/bit moved, and when

Usage:
    ./sc2-capture.py live
    ./sc2-capture.py capture out.jsonl
    ./sc2-capture.py analyse out.jsonl
"""
import json, os, select, sys, time, glob

VID, PID = "28DE", "1304"


def find_nodes():
    """Return hidraw paths belonging to the Steam Controller Puck."""
    out = []
    for p in sorted(glob.glob("/sys/class/hidraw/hidraw*")):
        try:
            uevent = open(os.path.join(p, "device/uevent")).read()
        except OSError:
            continue
        if f"{VID}:{PID}".replace(":", "") in uevent.replace(":", "").upper() or \
           (VID in uevent.upper() and PID in uevent.upper()):
            out.append("/dev/" + os.path.basename(p))
    return out


def open_nodes(paths):
    fds = {}
    for path in paths:
        try:
            fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
        except OSError as e:
            print(f"  {path}: cannot open ({e.strerror})", file=sys.stderr)
            continue
        fds[fd] = path
    return fds


def pump(fds, on_report):
    poller = select.poll()
    for fd in fds:
        poller.register(fd, select.POLLIN)
    while True:
        for fd, _ in poller.poll(1000):
            try:
                data = os.read(fd, 512)
            except BlockingIOError:
                continue
            if data:
                on_report(time.monotonic(), fds[fd], data)


def cmd_capture(argv):
    path = argv[0] if argv else "sc2-capture.jsonl"
    nodes = find_nodes()
    fds = open_nodes(nodes)
    if not fds:
        sys.exit("no readable Steam Controller Puck hidraw nodes")
    print(f"tapping {len(fds)} node(s): {', '.join(fds.values())}")
    print(f"logging to {path} -- Ctrl-C to stop\n")
    t0 = time.monotonic()
    n = 0
    with open(path, "w") as fh:
        def write(ts, node, data):
            nonlocal n
            n += 1
            fh.write(json.dumps({"t": round(ts - t0, 4), "node": node,
                                 "hex": data.hex()}) + "\n")
            if n % 250 == 0:
                fh.flush()
                print(f"\r  {n} reports  ({ts - t0:6.1f}s)", end="", flush=True)
        try:
            pump(fds, write)
        except KeyboardInterrupt:
            print(f"\nwrote {n} reports to {path}")


def cmd_live(argv):
    """Print only the bytes that differ from a rolling baseline."""
    nodes = find_nodes()
    fds = open_nodes(nodes)
    if not fds:
        sys.exit("no readable Steam Controller Puck hidraw nodes")
    print(f"tapping: {', '.join(fds.values())}   Ctrl-C to stop\n")
    baseline = {}
    # Bytes that are pure counters/sensors -- too noisy to diff usefully.
    ignore = set(range(1, 5)) | set(range(10, 30))

    def show(ts, node, data):
        key = (node, data[0] if data else -1)
        base = baseline.get(key)
        if base is None or len(base) != len(data):
            baseline[key] = data
            print(f"{node} report 0x{data[0]:02x} len={len(data)} baseline set")
            return
        diff = [(i, base[i], data[i]) for i in range(len(data))
                if base[i] != data[i] and i not in ignore]
        if diff:
            parts = " ".join(f"b{i}:{a:02x}->{b:02x}" for i, a, b in diff)
            print(f"[{ts:8.2f}] {os.path.basename(node)} 0x{data[0]:02x}  {parts}")
            baseline[key] = data

    try:
        pump(fds, show)
    except KeyboardInterrupt:
        print()


def cmd_analyse(argv):
    path = argv[0] if argv else "sc2-capture.jsonl"
    by_key = {}
    for line in open(path):
        r = json.loads(line)
        data = bytes.fromhex(r["hex"])
        by_key.setdefault((r["node"], data[0]), []).append((r["t"], data))

    for (node, rid), reports in sorted(by_key.items()):
        lens = {len(d) for _, d in reports}
        print(f"\n=== {node} report 0x{rid:02x} "
              f"({len(reports)} reports, len {sorted(lens)}) ===")
        n = min(lens)
        for i in range(n):
            vals = {d[i] for _, d in reports}
            if len(vals) == 1:
                continue
            # Which bits ever toggle?
            or_, and_ = 0, 0xFF
            for _, d in reports:
                or_ |= d[i]
                and_ &= d[i]
            toggling = or_ & ~and_
            kind = "bitfield" if bin(toggling).count("1") <= 8 and len(vals) <= 16 \
                   else "analog "
            # First moment each toggling bit goes high.
            firsts = []
            for bit in range(8):
                if not toggling & (1 << bit):
                    continue
                for t, d in reports:
                    if d[i] & (1 << bit):
                        firsts.append(f"bit{bit}@{t:.2f}s")
                        break
            print(f"  b{i:<3} {kind} distinct={len(vals):<4} "
                  f"toggle_mask=0x{toggling:02x} "
                  f"range={min(vals):#04x}..{max(vals):#04x}  "
                  + " ".join(firsts[:8]))


if __name__ == "__main__":
    cmds = {"capture": cmd_capture, "live": cmd_live, "analyse": cmd_analyse}
    if len(sys.argv) < 2 or sys.argv[1] not in cmds:
        sys.exit(__doc__)
    cmds[sys.argv[1]](sys.argv[2:])
