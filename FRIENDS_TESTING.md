# Friends Testing Guide (organizer notes)

Notes for coordinating a friends/beta test across mixed hardware. The tester-facing
instructions live in `scripts/QUICKSTART.txt`; this file is for you, the person handing the
build out.

## Hand out the installer, not the zip

Build `EternalMonitor-Setup.exe` with `scripts/build-installer.ps1` and send that single
file. The tester double-clicks it, approves one Windows (UAC) prompt, and gets the host,
FFmpeg, and the virtual display driver installed in one run — no unzip, no manual driver
steps. To bundle the driver, drop the signed setup into `installer/vendor/vdd/` first (see
that folder's `README.txt`); without it the build still works but produces an app-only
installer and the extended-display option won't appear.

## Extended display vs mirror

By default the iPad mirrors the primary screen. To test the iPad as a real extended desktop:
**connect the iPad first**, then open Settings → Capture display, choose **"Extended display
(iPad)"**, and click Restart stream. Dragging a window past the edge of the main screen should
land it on the iPad. The virtual monitor is created **on demand and only while the iPad is
connected** — there is intentionally no second display when idle, so don't expect to see it in
Windows Display settings before connecting. If the extended display can't start, the host shows an
amber "Extended display unavailable" banner and mirrors the primary screen — re-run the installer
so its display task is registered.

## Build parity matters

The host and iPad app must be from the **same release**. v0.2.0's protocol v2
is a deliberate clean break: a v0.1.x app meeting a v0.2.0 host (or the
reverse) won't stream — each side shows an explicit "update the other half"
message instead of corrupted video, so at least the failure is obvious. Hand
out the matching TestFlight build and installer together.

## New in v0.2.0 — things worth testing on purpose

- **Input relay**: tap/drag/two-finger scroll/hold-for-right-click/Pencil
  should feel like a trackpad. Check multi-monitor setups (clicks must land
  on the captured screen) and the "Control PC with touch" toggle off (host
  must ignore touches).
- **HEVC**: flip "Prefer HEVC" in host Settings mid-stream; the codec on the
  iPad's Settings HOST module should flip to HEVC within a second, and back.
  If video breaks only in HEVC on some GPU, that encoder's HEVC path is the
  bug — collect logs and turn the toggle off.
- **Recovery**: kill the host mid-stream (Task Manager) and relaunch — the
  iPad should show SIGNAL LOST and reconnect by itself. Walk to the edge of
  WiFi range — the picture should coarsen (bitrate stepping down) rather
  than freeze, and recover afterwards.
- **Extended display resolution**: with "Match extended display to the
  iPad's resolution" on (default), the virtual display should come up at the
  iPad's native aspect — no letterboxing on the iPad.

## Cover all three GPU vendors

The encoder path differs per vendor and the AMD path is the newest, so try to get at least
one tester on each:

- **NVIDIA** — NVENC. Best-tested path.
- **AMD** — AMF. Has bespoke handling (redundant SPS/PPS on every IDR, a forced IDR every
  30 frames, closed-GOP flags). If an AMD tester sees periodic freezes or a black screen on
  connect, that's the path to scrutinize.
- **Intel** — QSV. Lightly tested.

If a tester's hardware encoder fails to open, the host falls back to **CPU (libx264)** and
now shows an **amber warning banner** on the Stream tab. CPU encoding is hot and high-latency
— tell them to update GPU drivers and click "Restart stream".

## What to collect from a tester when something breaks

1. **GPU + codec** — shown on the host Stream tab (e.g. "NVIDIA … / H.264 (NVENC)"). If the
   codec reads "H.264 (x264)", they're on the CPU fallback.
2. **Host logs** — "Copy logs" button on the Stream tab, or the full file at
   `%APPDATA%\EternalMonitor\logs\eternal-host-session.log` (paste `%APPDATA%` into Explorer's
   address bar — it expands to `C:\Users\<name>\AppData\Roaming`).
3. **AMD only** — if streaming is broken on an AMD machine, ask for
   `%APPDATA%\EternalMonitor\diagnostics\amf-first-120-packets.h264` (the host captures the first
   120 NAL packets there for offline inspection).
4. **What the iPad showed** — black screen / frozen / corrupted / "connecting" forever.

## The usual non-bug culprits

- **Firewall** — both "Private" and "Public" boxes must be checked on first run. The single
  most common "it won't connect" cause.
- **Wi-Fi quality** — freezes/stutter are almost always the network (there is intentionally no
  packet-loss recovery yet). Push testers to 5 GHz / proximity / wired host before assuming a
  bug.
- **Different subnets / guest networks** — mDNS discovery won't cross them; manual IP entry is
  the known-good path.
- **Unsigned binary** — SmartScreen "Windows protected your PC" → "More info" → "Run anyway".

## Known limitations (so you don't chase ghosts)

No audio, no USB transport, and no retransmit of lost packets — loss shows as
a brief artifact or frame skip, then the stream self-heals with a requested
keyframe (and steps the bitrate down if loss persists). Sustained stutter on
a clean network IS reportable now; on hotel/guest WiFi it's still the
network. HEVC is experimental and off by default — if a stream misbehaves,
confirm the codec on the Stream tab before filing it as a general bug.
