# Friends Testing Guide (organizer notes)

Notes for coordinating a friends/beta test across mixed hardware. The tester-facing
instructions live in `scripts/QUICKSTART.txt` (shipped inside the Windows zip); this file is
for you, the person handing the build out.

## Build parity matters

The host and iPad app must be from the **same release**. The v0.1.1 transport changed the
fragment count to `u16` (see `ARCHITECTURE.md`), so an older iPad build talking to a newer
host (or vice-versa) shows corrupted or no video. When inviting a tester, give them the
matching iPad TestFlight build and the matching Windows zip together.

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
   `<host-exe-folder>/logs/eternal-host-session.log`.
3. **AMD only** — if streaming is broken on an AMD machine, ask for
   `<host-exe-folder>/diagnostics/amf-first-120-packets.h264` (the host captures the first
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

No NACK/retransmit, no congestion control, no jitter buffer (see `ARCHITECTURE.md` →
"Not implemented yet"). On a clean network the stream is smooth; on a lossy one it will drop
frames with no recovery. That's expected for this build, not a regression.
