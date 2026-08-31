# Raw capture data

Produced by [`../tools/sc2-capture.py`](../tools/sc2-capture.py) on 2026-08-30
against a 2026 Steam Controller (`28de:1304`) on the machine described in
[../docs/03-hardware-findings.md](../docs/03-hardware-findings.md).

Both captures were taken **while the Steam client held all five of the puck's
`hidraw` nodes open** — that concurrency is the point.

| File | Contents |
|---|---|
| `sc2-guided-sequence.jsonl.gz` | 210 s, 55 205 × report `0x42`. A guided walk through every control. Contains two overlapping passes and an on-screen keyboard interruption, so parts are ambiguous. |
| `sc2-guide-button-isolated.jsonl.gz` | 45 s. Three isolated Steam-button taps, three isolated Quick Access taps, one 2.3 s Steam hold, one guide-plus-right-stick hold. This is the capture that confirmed the two known button bits. |

One JSON object per line: `{"t": seconds_since_start, "node": "/dev/hidrawN", "hex": "..."}`.

Analyse with:

```
zcat sc2-guide-button-isolated.jsonl.gz > /tmp/c.jsonl
../tools/sc2-capture.py analyse /tmp/c.jsonl
```

| `sc2-combined-lizard-correlation.jsonl.gz` | 227 s, Steam closed. Simultaneous hidraw `0x42` + lizard-mode evdev capture with a guided one-control-at-a-time script. The capture that completed the button map, decoded the analog layout (b6-b29), proved the IMU region is Steam-gated, and confirmed the lizard-mode vocabulary. Produced by `../tools/sc2-combined-capture.py`. |
