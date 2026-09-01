#!/usr/bin/env python3
"""Decisive probe: create a virtual Steam Controller (28de:1302, single-interface
wired identity) via /dev/uhid on BUS_VIRTUAL, using the captured 372-byte report
descriptor, hold it open, ANSWER Steam's GET/SET report queries so it can't time
out, and LOG every host->device event.

The proof we're after: with NO real 1302 plugged in, if Steam sends this device a
SET_REPORT / OUTPUT (its lizard-disable + config writes), those arrive on OUR uhid
fd -> Steam has adopted the fake as a Steam Controller. Enumeration + uaccess were
already confirmed; this catches the engagement that the real-hardware confound hid.

Root only (/dev/uhid is 0600 root:root). Creates NOTHING persistent; destroyed on
exit. Does not write to the real puck.

Usage:  sudo python3 uhid_probe.py [seconds]   (default 90)
"""
import os, struct, sys, time, glob, fcntl

RD_PATH = "/home/ajg/code/hyprsc/docs/research/assets/triton-wired-1302-report-descriptor.bin"

# --- /dev/uhid uapi (linux/uhid.h) -----------------------------------------
UHID_CREATE2 = 11
UHID_DESTROY = 1
UHID_START, UHID_STOP, UHID_OPEN, UHID_CLOSE = 2, 3, 4, 5
UHID_OUTPUT = 6
UHID_GET_REPORT, UHID_GET_REPORT_REPLY = 9, 10
UHID_SET_REPORT, UHID_SET_REPORT_REPLY = 13, 14
BUS_USB, BUS_VIRTUAL = 0x03, 0x06
UHID_DATA_MAX = 4096

EV = {UHID_START:"START", UHID_STOP:"STOP", UHID_OPEN:"OPEN (a reader attached)",
      UHID_CLOSE:"CLOSE"}

def create2_event(rd: bytes) -> bytes:
    name = b"Valve Software Steam Controller"
    phys = b"hyprpad-uhid/1302"
    uniq = b"FXA9961402A6C"
    buf  = struct.pack("<I", UHID_CREATE2)
    buf += name.ljust(128, b"\0") + phys.ljust(64, b"\0") + uniq.ljust(64, b"\0")
    buf += struct.pack("<HH", len(rd), BUS_VIRTUAL)
    buf += struct.pack("<IIII", 0x28de, 0x1302, 0x0307, 0)
    buf += rd.ljust(UHID_DATA_MAX, b"\0")
    return buf

def get_report_reply(rid: int, data: bytes) -> bytes:
    # type, then {u32 id; u16 err; u16 size; u8 data[4096]}
    return (struct.pack("<I", UHID_GET_REPORT_REPLY)
            + struct.pack("<IHH", rid, 0, len(data)) + data.ljust(UHID_DATA_MAX, b"\0"))

def set_report_reply(rid: int) -> bytes:
    return struct.pack("<IIH", UHID_SET_REPORT_REPLY, rid, 0)

def main():
    secs = int(sys.argv[1]) if len(sys.argv) > 1 else 90
    rd = open(RD_PATH, "rb").read()
    print(f"[*] report descriptor: {len(rd)} bytes")
    fd = os.open("/dev/uhid", os.O_RDWR)
    os.write(fd, create2_event(rd))
    print("[*] UHID_CREATE2: 28de:1302 on BUS_VIRTUAL, uniq=FXA9961402A6C")
    time.sleep(0.4)

    node = None
    for h in sorted(glob.glob("/sys/class/hidraw/hidraw*")):
        try: ue = open(h+"/device/uevent").read()
        except OSError: continue
        if "1302" in ue and "HID_PHYS=hyprpad-uhid" in ue:
            node = os.path.basename(h)
            print(f"[+] enumerated as /dev/{node}  ({[l for l in ue.splitlines() if l.startswith('HID_ID')][0]})")
    if not node:
        print("[!] no hidraw node for the fake — enumeration failed");
    print(f"[*] holding {secs}s. Make sure Steam is RUNNING. Watching for Steam to")
    print("    talk to the fake — SET_REPORT / OUTPUT means Steam adopted it:")

    writes = 0
    fl = fcntl.fcntl(fd, fcntl.F_GETFL); fcntl.fcntl(fd, fcntl.F_SETFL, fl | os.O_NONBLOCK)
    t0 = time.time()
    while time.time() - t0 < secs:
        try:
            ev = os.read(fd, 5000)
        except BlockingIOError:
            time.sleep(0.03); continue
        if not ev: continue
        etype = struct.unpack("<I", ev[:4])[0]
        if etype in EV:
            print(f"    <- {EV[etype]}")
        elif etype == UHID_GET_REPORT:
            rid, rnum, rtype = struct.unpack("<IBB", ev[4:10])
            print(f"    <- GET_REPORT  id={rid} rnum={rnum} rtype={rtype}  (Steam querying the fake) -> replying")
            os.write(fd, get_report_reply(rid, bytes(64)))
            writes += 1
        elif etype == UHID_SET_REPORT:
            rid, rnum, rtype, size = struct.unpack("<IBBH", ev[4:12])
            data = ev[12:12+size]
            print(f"    <- SET_REPORT  rnum={rnum} rtype={rtype} size={size}  data={data[:12].hex()}  ** STEAM IS CONFIGURING THE FAKE **")
            os.write(fd, set_report_reply(rid))
            writes += 1
        elif etype == UHID_OUTPUT:
            size = struct.unpack("<H", ev[4+UHID_DATA_MAX:4+UHID_DATA_MAX+2])[0]
            data = ev[4:4+size]
            print(f"    <- OUTPUT  size={size}  data={data[:12].hex()}  ** STEAM WROTE TO THE FAKE **")
            writes += 1
        else:
            print(f"    <- (event type {etype})")

    os.write(fd, struct.pack("<I", UHID_DESTROY)); os.close(fd)
    print(f"[*] destroyed. Steam write-events seen: {writes}")
    print("[*] VERDICT: any SET_REPORT/OUTPUT above = Steam adopted the uhid fake as a Steam Controller.")
    print("    Then check:  grep -iE '1302|opened|V1 HID' ~/.local/share/Steam/logs/controller.txt | tail")

if __name__ == "__main__":
    main()
