#!/usr/bin/env python3
"""Type 'hello' via a uinput virtual keyboard with REAL evdev keycodes."""
import time
from evdev import UInput, ecodes as e
KEYS = [e.KEY_H, e.KEY_E, e.KEY_L, e.KEY_L, e.KEY_O]
ui = UInput({e.EV_KEY: KEYS}, name="hyprsc-test-kbd")
time.sleep(1.0)   # let Hyprland/libinput pick the device up
for k in KEYS:
    ui.write(e.EV_KEY, k, 1); ui.syn(); time.sleep(0.03)
    ui.write(e.EV_KEY, k, 0); ui.syn(); time.sleep(0.05)
time.sleep(0.2)
ui.close()
print("typed hello via uinput (real keycodes)")
