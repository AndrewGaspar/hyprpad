#!/usr/bin/env python3
"""Combined capture: puck hidraw 0x42 reports + lizard-mode evdev events.

One timestamped jsonl so raw HID bits can be correlated with the lizard-mode
keyboard/mouse output. Run with Steam closed.

Usage: ./sc2-combined-capture.py out.jsonl [seconds]
"""
import json, os, select, struct, sys, time, glob

def puck_nodes():
    hid, ev = [], []
    for p in sorted(glob.glob("/sys/class/hidraw/hidraw*")):
        try:
            u = open(p + "/device/uevent").read()
        except OSError:
            continue
        if "28DE" in u.upper() and "1304" in u:
            hid.append("/dev/" + os.path.basename(p))
    for link in sorted(glob.glob("/dev/input/by-id/*Valve*event*")):
        ev.append(os.path.realpath(link))
    return hid, ev

def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "combined.jsonl"
    dur = float(sys.argv[2]) if len(sys.argv) > 2 else 240
    hid, ev = puck_nodes()
    fds = {}
    for path in hid + ev:
        try:
            fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
            fds[fd] = path
        except OSError as e:
            print(f"skip {path}: {e.strerror}", file=sys.stderr)
    print(f"capturing {len(fds)} nodes ({len(hid)} hidraw, {len(ev)} evdev) for {dur:.0f}s")
    poller = select.poll()
    for fd in fds:
        poller.register(fd, select.POLLIN)
    t0 = time.monotonic()
    n = {"hid": 0, "ev": 0}
    EVFMT = "qqHHi"; EVSZ = struct.calcsize(EVFMT)
    with open(out, "w") as fh:
        while time.monotonic() - t0 < dur:
            for fd, _ in poller.poll(500):
                path = fds[fd]
                try:
                    data = os.read(fd, 4096)
                except (BlockingIOError, OSError):
                    continue
                t = round(time.monotonic() - t0, 4)
                if path.startswith("/dev/hidraw"):
                    fh.write(json.dumps({"t": t, "src": path, "hex": data.hex()}) + "\n")
                    n["hid"] += 1
                else:
                    for off in range(0, len(data) - EVSZ + 1, EVSZ):
                        s, us, typ, code, val = struct.unpack_from(EVFMT, data, off)
                        if typ == 0:
                            continue
                        fh.write(json.dumps({"t": t, "src": path, "type": typ,
                                             "code": code, "val": val}) + "\n")
                        n["ev"] += 1
    print(f"done: {n['hid']} hid reports, {n['ev']} evdev events -> {out}")

if __name__ == "__main__":
    main()
