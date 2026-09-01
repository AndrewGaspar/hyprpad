#!/usr/bin/env python3
# ---------------------------------------------------------------------------
#  uhid_active_probe.py  --  ACTIVE virtual Valve controller via /dev/uhid
#
#  Purpose: settle definitively whether Steam ADOPTS a parentless uhid device
#  as a Valve controller. It faithfully clones InputPlumber's proven
#  "deck-uhid" recipe: an emulated Steam *Deck* controller under a *non-Deck*
#  PID (0x28de:0x12f0), streamed at 250 Hz, that ANSWERS the GET_REPORT
#  handshake with the exact canned bytes the real device sends. This is the
#  load-bearing difference from the earlier PASSIVE probe (uhid_probe.py),
#  which streamed nothing and answered every GET_REPORT empty -> Steam ignored
#  it. See docs/research/uhid-steam-controller.md "Addendum" (A.1-A.9).
#
#  Throwaway experiment tool. Creates nothing persistent (UHID_DESTROY on
#  exit). Does NOT touch the real puck (28de:1304) at all -- purely synthetic.
#  Root only (/dev/uhid is 0600 root:root).
#
# ===========================================================================
#  SOURCE OF THE VERBATIM BYTES  (InputPlumber `main`, cloned locally today)
# ---------------------------------------------------------------------------
#  All refs are ShadowBlip/InputPlumber, verified against the local checkout
#  at scratchpad/ip (== raw.githubusercontent.com/ShadowBlip/InputPlumber/main).
#
#  (1) CreateParams for the uhid device -- bus/vendor/product/version/name:
#      src/input/target/steam_deck_uhid.rs:89-103  (fn create_virtual_device)
#        bus: Bus::USB (0x03), vendor: VID (0x28de),
#        product: config.product_id.to_u32(), version: 0x1000, country: 0,
#        phys: "", uniq: "",  rd_data: CONTROLLER_DESCRIPTOR
#      VID + ProductId enum: src/drivers/steam_deck/mod.rs:6-20
#        Generic = 0x12f0  (the generic uhid PID; NOT the real Deck 0x1205 --
#        the comment at steam_deck_uhid.rs:675-677 explains the true PID only
#        works with the VCHI target because Steam looks for a specific
#        bInterfaceNumber for that PID).
#      name/vendor strings: src/input/target/steam_deck.rs:61-66 (default
#        "Generic Steam Controller" / "InputPlumber") and steam_deck_uhid.rs:
#        672-678 (the actual "Steam Deck" branch sets name "Steam Controller",
#        vendor "Valve Corporation", product Generic). We use "Steam
#        Controller" -- InputPlumber's real name string for the Deck case, and
#        what Steam reads as the Product string.
#
#  (2) The 38-byte report descriptor (pure vendor page 0xFFFF, NO report IDs):
#      src/drivers/steam_deck/report_descriptor.rs:64-83  (CONTROLLER_DESCRIPTOR)
#      Because there are no REPORT_ID items, all three UHID_START numbered-
#      report dev_flags are UNSET -> reports are UNPREFIXED on the wire.
#
#  (3) The input report layout that write_state()/pack() emits:
#      src/drivers/steam_deck/hid_report.rs:262-473  (PackedInputDataReport,
#        #[packed_struct(size_bytes="64")]):
#        byte 0  major_ver  = 0x01
#        byte 1  minor_ver  = 0x00
#        byte 2  report_type= 0x09  (ReportType::InputData)
#        byte 3  report_size= 0x40  (64)
#        bytes 4-7   frame  (u32 LE)  <-- the frame counter
#        bytes 8-14  button bitfields (msb0)         -- neutral = 0
#        byte 15     _unk31
#        bytes 16-23 l_pad_x/y, r_pad_x/y (i16 LE)
#        bytes 24-29 accel_x/y/z (i16 LE)
#        bytes 30-35 pitch/yaw/roll  gyro (i16 LE)
#        bytes 36-43 magnetometer (i16 LE)
#        bytes 44-47 l_trigg, r_trigg (u16 LE)
#        bytes 48-55 l_stick_x/y, r_stick_x/y (i16 LE)
#        bytes 56-59 l_pad_force, r_pad_force (u16 LE)
#        bytes 60-63 l_stick_force, r_stick_force (u16 LE)
#      Header 01 00 09 40 confirmed. Neutral report => header + frame + zeros.
#      Frame is bumped frame.wrapping_add(1) each write:
#        steam_deck_uhid.rs:882-884 (poll) and write_state()@106-120.
#      Streaming is unconditional at 250 Hz (poll_rate 4 ms): the target driver
#      calls poll() every 4 ms and poll() calls write_state() on every path
#      (steam_deck_uhid.rs:853-982; buffer/poll options in src/input/target/mod.rs).
#
#  (4) GET_REPORT canned responses -- steam_deck_uhid.rs:419-537
#      (fn handle_get_report), selected by self.current_report, which a prior
#      SET_REPORT switches via data[1] (handle_set_report:539-557):
#        * GetAttributesValues (0x83): the verbatim 64-byte TLV blob at
#          steam_deck_uhid.rs:434-499, in-source comment "No idea what these
#          bytes mean, but this is what is sent from the real device."
#          data[2]=0x2d=45 = payload length; body is attr_id,u32 TLVs.
#        * GetStringAttribute/serial (0xAE): [0x00,0xAE,0x14,0x01] + serial,
#          resized to 64. Default serial "1NPU7PLUMB3R" (new_with_config:83).
#        * GetChipId (0xBA): [0x00,0xBA,0x11,0x00] + 15-byte chip_id
#          [0,1,2,3,4,5,6,7,8,9,0,1,2,3,4] (new_with_config:75), resized to 64.
#      Anything else: reply err=0 with empty data (never times out). Every
#      GET_REPORT and SET_REPORT is answered immediately (5 s kernel timeout
#      must never fire). ReportType enum values: hid_report.rs:41-92.
#
# ===========================================================================
#  TEST PROCEDURE  (you need sudo + Steam in a NORMAL, controller-detecting
#                   state -- NOT the masked bwrap `hyprpad-steam` wrapper)
# ---------------------------------------------------------------------------
#  Our owner's Steam is normally launched MASKED (bwrap `hyprpad-steam`), which
#  is why it holds no hidraw open. For THIS proof, Steam must be detecting
#  controllers normally. Exact steps:
#
#   1. Fully quit the masked Steam. Relaunch Steam NORMALLY (unmasked --
#      run `steam` directly, not the wrapper) so it detects controllers.
#      Prefer Big Picture, which grabs controllers more aggressively; ensure
#      Settings -> Controller has Steam Input enabled.
#
#   2. Confirm the BASELINE -- Steam actually grabs the REAL puck (this is the
#      confound that invalidated the passive run; do NOT skip it):
#        grep -iE 'opened|V1 HID' ~/.local/share/Steam/logs/controller.txt | tail
#      expect a line like "!! Steam controller device opened for index N", and
#        ls -l /proc/$(pgrep -x steam)/fd | grep hidraw
#      should list the puck's /dev/hidrawN. If Steam will NOT grab the real
#      puck even unmasked, STOP -- that's a separate issue to flag; the probe
#      would prove nothing (a fake can't be adopted if the real one isn't).
#
#   3. With Steam already up and grabbing controllers, run the probe:
#        sudo python3 uhid_active_probe.py 120
#      (creating the device AFTER Steam is up makes the hidraw `add` uevent hit
#      Steam's live udev monitor.)
#
#   4. PROVEN if ALL of:
#        - the probe logs SET_REPORT and/or OUTPUT lines from Steam (any
#          SET_REPORT/OUTPUT = Steam is configuring/driving the fake), ideally
#          incl. the lizard-disable 0x87 / rumble 0xEB / haptic 0xEA selectors;
#        - controller.txt gains a NEW "Steam controller device opened" line for
#          a 12f0 / Deck device during the probe's window:
#            grep -iE '12f0|opened|V1 HID|deck' ~/.local/share/Steam/logs/controller.txt | tail
#        - /proc/$(pgrep -x steam)/fd shows the probe's /dev/hidrawN node
#          (the probe prints which node it enumerated as).
#      Steam may also show the controller in Settings -> Controller.
#
#      Contrast: enumerated but never opened WHILE the real puck IS opened in
#      the same session => identity/parentless-ness rejected for 0x12f0.
#      Nothing, and the real puck is also not open => not a detecting state
#      (return to step 1); the result is meaningless.
#
#  Usage:  sudo python3 uhid_active_probe.py [seconds]     (default 120)
# ---------------------------------------------------------------------------
import os
import sys
import time
import glob
import math
import fcntl
import select
import struct
import signal
import threading

# --- /dev/uhid uapi (linux/uhid.h) -----------------------------------------
UHID_CREATE2          = 11
UHID_DESTROY          = 1
UHID_START            = 2
UHID_STOP             = 3
UHID_OPEN             = 4
UHID_CLOSE            = 5
UHID_OUTPUT           = 6
UHID_GET_REPORT       = 9
UHID_GET_REPORT_REPLY = 10
UHID_INPUT2           = 12
UHID_SET_REPORT       = 13
UHID_SET_REPORT_REPLY = 14

BUS_USB       = 0x03      # InputPlumber uses BUS_USB, NOT BUS_VIRTUAL (0x06)
UHID_DATA_MAX = 4096
EVENT_BUFSZ   = 4380      # sizeof(struct uhid_event) on x86-64; reads truncate

# --- device identity (InputPlumber deck-uhid recipe) -----------------------
VENDOR   = 0x28de
PRODUCT  = 0x12f0         # ProductId::Generic  (NOT the real Deck 0x1205)
VERSION  = 0x1000
COUNTRY  = 0
DEV_NAME = b"Steam Controller"   # InputPlumber's real Deck-case name string
DEV_PHYS = b""                   # InputPlumber: phys = ""
DEV_UNIQ = b""                   # InputPlumber: uniq = ""

# --- (2) the verbatim 38-byte vendor-page descriptor, NO report IDs --------
CONTROLLER_DESCRIPTOR = bytes([
    0x06, 0xff, 0xff,        # Usage Page (Vendor 0xFFFF)
    0x09, 0x01,              # Usage (0x01)
    0xa1, 0x01,              # Collection (Application)
    0x09, 0x02, 0x09, 0x03,  #  Usage 0x02, Usage 0x03
    0x15, 0x00,              #  Logical Minimum (0)
    0x26, 0xff, 0x00,        #  Logical Maximum (255)
    0x75, 0x08,              #  Report Size (8)
    0x95, 0x40,              #  Report Count (64)
    0x81, 0x02,              #  Input (Data,Var,Abs)
    0x09, 0x06, 0x09, 0x07,  #  Usage 0x06, Usage 0x07
    0x15, 0x00,              #  Logical Minimum (0)
    0x26, 0xff, 0x00,        #  Logical Maximum (255)
    0x75, 0x08,              #  Report Size (8)
    0x95, 0x40,              #  Report Count (64)
    0xb1, 0x02,              #  Feature (Data,Var,Abs)
    0xc0,                    # End Collection
])
assert len(CONTROLLER_DESCRIPTOR) == 38

# --- (4) canned GET_REPORT payloads (verbatim from InputPlumber) -----------
RT_INPUT_DATA          = 0x09
RT_GET_ATTRIBUTES      = 0x83   # GetAttributesValues
RT_GET_STRING_ATTR     = 0xAE   # GetStringAttribute (serial)
RT_GET_CHIP_ID         = 0xBA   # GetChipId
RT_TRIGGER_HAPTIC      = 0xEA   # TriggerHapticCommand
RT_TRIGGER_RUMBLE      = 0xEB   # TriggerRumbleCommand
RT_SET_SETTINGS        = 0x87   # SetSettingsValues (lizard-disable lives here)

# GetAttributesValues: the exact 64-byte blob (steam_deck_uhid.rs:434-499).
# "No idea what these bytes mean, but this is what is sent from the real device."
ATTRIBUTES_BLOB = bytes([
    0x00, RT_GET_ATTRIBUTES, 0x2d, 0x01, 0x05, 0x12, 0x00, 0x00, 0x02, 0x00,
    0x00, 0x00, 0x00, 0x0a, 0x2b, 0x12, 0xa9, 0x62, 0x04, 0xad,
    0xf1, 0xe4, 0x65, 0x09, 0x2e, 0x00, 0x00, 0x00, 0x0b, 0xa0,
    0x0f, 0x00, 0x00, 0x0d, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x00,
    0x00, 0x00, 0x00, 0x0e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
])
assert len(ATTRIBUTES_BLOB) == 64

SERIAL_NUMBER = b"1NPU7PLUMB3R"                          # default serial
CHIP_ID       = bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0, 1, 2, 3, 4])

def _serial_reply():
    d = bytearray([0x00, RT_GET_STRING_ATTR, 0x14, 0x01]) + bytearray(SERIAL_NUMBER)
    d = d[:64] + bytes(max(0, 64 - len(d)))
    return bytes(d[:64])

def _chip_id_reply():
    d = bytearray([0x00, RT_GET_CHIP_ID, 0x11, 0x00]) + bytearray(CHIP_ID)
    d = d[:64] + bytes(max(0, 64 - len(d)))
    return bytes(d[:64])

SERIAL_REPLY  = _serial_reply()
CHIP_ID_REPLY = _chip_id_reply()

# Human-readable names for logging SET_REPORT selectors (hid_report.rs:41-92)
REPORT_NAMES = {
    0x09: "InputData", 0x80: "SetDigitalMappings", 0x81: "ClearDigitalMappings",
    0x82: "GetDigitalMappings", 0x83: "GetAttributesValues",
    0x84: "GetAttributesLabel", 0x85: "SetDefaultDigitalMappings",
    0x86: "FactoryReset", 0x87: "SetSettingsValues(lizard/config)",
    0x88: "ClearSettingsValues", 0x89: "GetSettingsValues",
    0x8D: "SetControllerMode", 0x8E: "LoadDefaultSettings",
    0x8F: "TriggerHapticPulse", 0x9F: "TurnOffController",
    0xA1: "GetDeviceInfo", 0xAE: "GetStringAttribute(serial)",
    0xBA: "GetChipId", 0xEA: "TriggerHapticCommand", 0xEB: "TriggerRumbleCommand",
}

def rname(v):
    return REPORT_NAMES.get(v, "0x%02x?" % v)

# --- uhid event framing helpers --------------------------------------------
def ev_create2():
    buf  = struct.pack("<I", UHID_CREATE2)
    buf += DEV_NAME.ljust(128, b"\0")
    buf += DEV_PHYS.ljust(64, b"\0")
    buf += DEV_UNIQ.ljust(64, b"\0")
    buf += struct.pack("<HH", len(CONTROLLER_DESCRIPTOR), BUS_USB)
    buf += struct.pack("<IIII", VENDOR, PRODUCT, VERSION, COUNTRY)
    buf += CONTROLLER_DESCRIPTOR.ljust(UHID_DATA_MAX, b"\0")
    return buf

def ev_input2(report: bytes):
    # struct uhid_input2_req { __u16 size; __u8 data[UHID_DATA_MAX]; }
    # Kernel zero-extends short writes, so only the populated prefix is needed.
    return struct.pack("<IH", UHID_INPUT2, len(report)) + report

def ev_get_report_reply(rid: int, data: bytes):
    # { __u32 id; __u16 err; __u16 size; __u8 data[UHID_DATA_MAX]; }, err=0
    return (struct.pack("<I", UHID_GET_REPORT_REPLY)
            + struct.pack("<IHH", rid, 0, len(data))
            + data.ljust(UHID_DATA_MAX, b"\0"))

def ev_set_report_reply(rid: int):
    # { __u32 id; __u16 err; }
    return struct.pack("<IIH", UHID_SET_REPORT_REPLY, rid, 0)

def ev_destroy():
    return struct.pack("<I", UHID_DESTROY)

# --- the input report builder (neutral, live frame counter, gentle axis) ---
MOVING_AXIS = True   # gently oscillate left-stick X so liveness is visible

def build_input_report(frame: int, t: float) -> bytes:
    r = bytearray(64)
    r[0] = 0x01          # major_ver
    r[1] = 0x00          # minor_ver
    r[2] = RT_INPUT_DATA # 0x09
    r[3] = 0x40          # report_size = 64
    struct.pack_into("<I", r, 4, frame & 0xFFFFFFFF)   # frame counter bytes 4-7
    # buttons bytes 8-14 stay neutral (0). Optional visible liveness on stick:
    if MOVING_AXIS:
        stick_x = int(3000 * math.sin(2 * math.pi * 0.2 * t))  # ~+/-9% at 0.2 Hz
        struct.pack_into("<h", r, 48, stick_x)                  # l_stick_x i16 LE
    return bytes(r)

# ---------------------------------------------------------------------------
class Probe:
    def __init__(self, seconds):
        self.seconds = seconds
        self.fd = None
        self.wlock = threading.Lock()
        self.stop = threading.Event()
        self.current_report = RT_INPUT_DATA   # matches InputPlumber default
        self.t0 = time.perf_counter()
        # counters for the verdict
        self.frames_sent = 0
        self.get_reports = 0
        self.set_reports = 0
        self.outputs = 0
        self.opens = 0
        self.saw_lizard = False
        self.saw_rumble_haptic = False

    def _write(self, data):
        with self.wlock:
            try:
                os.write(self.fd, data)
            except OSError as e:
                print("    [!] write failed: %r" % e)

    # -- 250 Hz sender thread (the load-bearing fix vs the passive probe) ----
    def sender(self):
        period = 0.004  # 4 ms == 250 Hz
        next_t = time.perf_counter()
        frame = 0
        while not self.stop.is_set():
            now = time.perf_counter()
            if now < next_t:
                # short, bounded sleep; never a foreground long sleep
                self.stop.wait(min(period, next_t - now))
                continue
            report = build_input_report(frame, now - self.t0)
            self._write(ev_input2(report))
            self.frames_sent += 1
            frame = (frame + 1) & 0xFFFFFFFF
            next_t += period
            # if we fell far behind, resync rather than spin
            if now - next_t > 0.050:
                next_t = now + period

    # -- GET_REPORT: reply with the canned blob for current_report ----------
    def handle_get_report(self, ev):
        try:
            rid, rnum, rtype = struct.unpack("<IBB", ev[4:10])
        except struct.error:
            return
        self.get_reports += 1
        sel = self.current_report
        if sel == RT_GET_ATTRIBUTES:
            data = ATTRIBUTES_BLOB
        elif sel == RT_GET_STRING_ATTR:
            data = SERIAL_REPLY
        elif sel == RT_GET_CHIP_ID:
            data = CHIP_ID_REPLY
        else:
            data = b""   # InputPlumber replies success+empty for other types
        print("    <- GET_REPORT id=%d rnum=%d rtype=%d  selector=%s -> reply %dB"
              % (rid, rnum, rtype, rname(sel), len(data)))
        self._write(ev_get_report_reply(rid, data))

    # -- SET_REPORT: switch current_report from data[1], ack success --------
    def handle_set_report(self, ev):
        try:
            rid, rnum, rtype, size = struct.unpack("<IBBH", ev[4:12])
        except struct.error:
            return
        data = ev[12:12 + size]
        self.set_reports += 1
        # uhid prepends a report-number byte; the Valve command id is data[1].
        sel = data[1] if len(data) >= 2 else (data[0] if data else None)
        if sel is not None:
            self.current_report = sel
            if sel == RT_SET_SETTINGS:
                self.saw_lizard = True
            if sel in (RT_TRIGGER_RUMBLE, RT_TRIGGER_HAPTIC):
                self.saw_rumble_haptic = True
        print("    <- SET_REPORT rnum=%d rtype=%d size=%d selector=%s data=%s"
              "  ** STEAM IS DRIVING THE FAKE **"
              % (rnum, rtype, size, rname(sel) if sel is not None else "?",
                 data[:16].hex()))
        self._write(ev_set_report_reply(rid))

    def handle_output(self, ev):
        try:
            size = struct.unpack("<H", ev[4 + UHID_DATA_MAX:4 + UHID_DATA_MAX + 2])[0]
        except struct.error:
            return
        data = ev[4:4 + size]
        self.outputs += 1
        self.saw_rumble_haptic = True
        print("    <- OUTPUT size=%d data=%s  ** STEAM WROTE TO THE FAKE **"
              % (size, data[:16].hex()))

    # -- dispatch one host->device event ------------------------------------
    def dispatch(self, ev):
        if len(ev) < 4:
            return
        try:
            etype = struct.unpack("<I", ev[:4])[0]
        except struct.error:
            return
        if etype == UHID_START:
            flags = struct.unpack("<Q", ev[4:12])[0] if len(ev) >= 12 else 0
            print("    <- START (dev_flags=0x%x; numbered bits expected 0 for the "
                  "report-ID-free Deck descriptor)" % flags)
        elif etype == UHID_STOP:
            print("    <- STOP")
        elif etype == UHID_OPEN:
            self.opens += 1
            print("    <- OPEN  (a reader attached -- likely Steam)")
        elif etype == UHID_CLOSE:
            print("    <- CLOSE (last reader gone)")
        elif etype == UHID_GET_REPORT:
            self.handle_get_report(ev)
        elif etype == UHID_SET_REPORT:
            self.handle_set_report(ev)
        elif etype == UHID_OUTPUT:
            self.handle_output(ev)
        else:
            print("    <- (unhandled event type %d)" % etype)

    def find_node(self):
        node = None
        for h in sorted(glob.glob("/sys/class/hidraw/hidraw*")):
            try:
                ue = open(h + "/device/uevent").read()
            except OSError:
                continue
            hid_id = ""
            for line in ue.splitlines():
                if line.startswith("HID_ID="):
                    hid_id = line
            # 0x12f0 is unambiguous (real puck is 0x1304); confirm it is uhid.
            if "28DE:12F0" in ue.upper() or "000012F0" in ue.upper():
                try:
                    real = os.path.realpath(h)
                except OSError:
                    real = ""
                virt = "virtual/misc/uhid" in real
                node = os.path.basename(h)
                print("[+] enumerated as /dev/%s  (%s)%s"
                      % (node, hid_id, "  [uhid-backed]" if virt else ""))
        return node

    def run(self):
        self.fd = os.open("/dev/uhid", os.O_RDWR)
        self._write(ev_create2())
        print("[*] UHID_CREATE2: %04x:%04x on BUS_USB, version 0x%04x, name=%r"
              % (VENDOR, PRODUCT, VERSION, DEV_NAME.decode()))
        print("[*] 38-byte vendor descriptor (no report IDs); phys/uniq empty.")

        # nonblocking + poll on the fd
        fl = fcntl.fcntl(self.fd, fcntl.F_GETFL)
        fcntl.fcntl(self.fd, fcntl.F_SETFL, fl | os.O_NONBLOCK)
        poller = select.poll()
        poller.register(self.fd, select.POLLIN)

        # start streaming at 250 Hz immediately (from creation, per InputPlumber)
        tsend = threading.Thread(target=self.sender, name="sender", daemon=True)
        tsend.start()

        time.sleep(0.4)
        self.find_node() or print("[!] no 12f0 hidraw node yet -- enumeration "
                                  "may still be settling")
        print("[*] streaming 250 Hz input reports (frame counter live). Holding "
              "%ds." % self.seconds)
        print("[*] Ensure Steam is RUNNING and grabbing controllers. Watching "
              "for Steam to drive the fake:")

        deadline = time.time() + self.seconds
        while time.time() < deadline and not self.stop.is_set():
            try:
                events = poller.poll(200)   # ms
            except InterruptedError:
                continue
            if not events:
                continue
            # drain everything available this iteration (queue is 32 deep)
            while True:
                try:
                    ev = os.read(self.fd, EVENT_BUFSZ)
                except BlockingIOError:
                    break
                except OSError as e:
                    print("    [!] read failed: %r" % e)
                    break
                if not ev:
                    break
                try:
                    self.dispatch(ev)
                except Exception as e:   # never crash on an unexpected event
                    print("    [!] dispatch error (ignored): %r" % e)

        self.shutdown()

    def shutdown(self):
        if self.stop.is_set():
            pass
        self.stop.set()
        time.sleep(0.02)
        try:
            self._write(ev_destroy())
        finally:
            try:
                os.close(self.fd)
            except OSError:
                pass
        self.verdict()

    def verdict(self):
        print("")
        print("=" * 70)
        print("VERDICT SUMMARY")
        print("  input frames streamed : %d  (~%.0f Hz over %ds)"
              % (self.frames_sent, self.frames_sent / max(1, self.seconds), self.seconds))
        print("  UHID_OPEN events      : %d" % self.opens)
        print("  GET_REPORT answered   : %d" % self.get_reports)
        print("  SET_REPORT received   : %d" % self.set_reports)
        print("  OUTPUT received       : %d" % self.outputs)
        adopted = (self.set_reports > 0 or self.outputs > 0)
        print("-" * 70)
        if adopted:
            print("  RESULT: *** ADOPTED *** -- Steam sent SET_REPORT/OUTPUT, i.e.")
            print("          it is configuring/driving the fake as a Valve controller.")
            if self.saw_lizard:
                print("          (saw SetSettingsValues 0x87 -- Steam's lizard-disable/config.)")
            if self.saw_rumble_haptic:
                print("          (saw rumble/haptic -- Steam is actively driving actuators.)")
        elif self.get_reports > 0 or self.opens > 0:
            print("  RESULT: PARTIAL -- Steam opened/queried the device but sent no")
            print("          SET_REPORT/OUTPUT. Check controller.txt for a 12f0/Deck")
            print("          'opened' line; if the real puck IS opened this session but")
            print("          the fake is not, 0x12f0 identity was rejected.")
        else:
            print("  RESULT: NO ENGAGEMENT -- no OPEN/GET/SET/OUTPUT from Steam.")
            print("          Confirm Steam grabbed the REAL puck this session (baseline");
            print("          step 2); if not, this run is inconclusive, not negative.")
        print("  Now cross-check:")
        print("    grep -iE '12f0|opened|V1 HID|deck' ~/.local/share/Steam/logs/controller.txt | tail")
        print("    ls -l /proc/$(pgrep -x steam)/fd | grep hidraw")
        print("=" * 70)


def main():
    seconds = 120
    if len(sys.argv) > 1:
        try:
            seconds = int(sys.argv[1])
        except ValueError:
            print("usage: sudo python3 uhid_active_probe.py [seconds]")
            sys.exit(2)

    if not os.path.exists("/dev/uhid"):
        print("[!] /dev/uhid does not exist -- load the uhid module.")
        sys.exit(1)

    probe = Probe(seconds)

    def _sig(_signum, _frame):
        print("\n[*] signal received -- tearing down")
        probe.stop.set()
    signal.signal(signal.SIGINT, _sig)
    signal.signal(signal.SIGTERM, _sig)

    try:
        probe.run()
    except PermissionError:
        print("[!] permission denied on /dev/uhid -- run with sudo.")
        sys.exit(1)


if __name__ == "__main__":
    main()
