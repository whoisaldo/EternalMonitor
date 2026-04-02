# EternalMonitor — Project Overview

> Use your iPad as a true second monitor on Windows. Free, open-source, low-latency.
> Part of the Eternal suite (see also: EternalRichPresence).

## What it is

EternalMonitor is a Rust + Swift application that turns an iPad into a genuine extended
Windows display — not a mirror, not a VNC hack. Windows sees a real second monitor via a
virtual display driver. Frames are GPU-encoded and streamed over WiFi or USB to the iPad,
which decodes and renders them at up to 120Hz via Metal.

## Guiding principles

- **Latency first, then quality** — target sub-20ms glass-to-glass on USB, sub-35ms on WiFi
- **GPU everywhere** — encode on the Windows GPU (NVENC), decode on VideoToolbox; CPU
  is never in the frame pipeline
- **No paid dependencies** — fully free and open-source stack
- **Native feel** — IddCx virtual display so Windows genuinely extends to the iPad; apps
  snap, DPI works, display settings work
- **Dual transport** — USB for lowest latency (wired), UDP/WiFi for wireless convenience

## Repo layout

```
EternalMonitor/
├── host/       # Rust — Windows daemon (capture, encode, transport)
├── driver/     # C — IddCx virtual display driver
├── ios/        # Swift — iPad app (receive, decode, render, input relay)
└── proto/      # Shared protocol definitions (FlatBuffers)
```

## Reference files

| File | Purpose |
|------|---------|
| `CLAUDE.md` | This file — top-level orientation |
| `ARCHITECTURE.md` | Full pipeline design and component contracts |
| `DECISIONS.md` | Why each tech was chosen (and what was rejected) |
| `ROADMAP.md` | Phased build plan with milestones |
