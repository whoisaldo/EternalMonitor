# EternalMonitor — Architecture

Accurate as of v0.2.0 (protocol v2).

## The pipeline

```text
[Windows desktop]                                [iPad]
      |                                            ^
      v                                            | Metal (NV12 + BT.601/709 shader,
+-------------------------- host/ ---------------|-- aspect-fit, draw-on-demand)
| capture thread          encode thread          | VideoToolbox (H.264/HEVC, hw on
|   DXGI duplication  -->   BGRA->YUV420P    -->  |  device, sw in the simulator)
|   (or synthetic)    slot  swscale, then         | FrameAssembler (per-frame
|   cursor composite        NVENC/AMF/QSV/x264/   |  fragment reassembly, caps)
|                           x265/VideoToolbox     | UDPReceiver (ephemeral port,
|                              |                  |  media/control demux)
|                              v  channel         |
|                    transport task (tokio)  -----+--> media datagrams (UDP)
|                      v2 fragmentation, pacing,  <--- control datagrams (same socket)
|                      session, heartbeats, ABR   |
+-------------------------------------------------+
         supervised by supervisor.rs (health, watchdogs, backoff restarts)
```

Host stages are dedicated OS threads connected by a **latest-wins frame slot**
(capture → encode: an unconsumed frame is displaced, so the encoder always
works on the freshest picture) and a **lossless channel** (encode → transport:
a dropped encoded P-frame would corrupt the GOP). Frame pixels travel in
`Arc<Vec<u8>>` buffers that are recycled — steady state does one full-frame
copy (the DXGI staging readback).

The **supervisor** owns the pipeline: stage threads report their exit, wedge
watchdogs fire on silent stalls (loop heartbeat stale 3 s, no frame 5 s with a
client, encoder flat 3 s), and restarts get exponential backoff with a
restart-storm brake. The client **session lives outside the pipeline**, so a
crash-restart resumes streaming to the same session — the client sees a new
stream epoch and resets reassembly, with no re-handshake.

## Wire protocol v2

One UDP socket carries both media and control. Every datagram starts with an
8-byte prefix: magic `"EM"`, version `2`, packet type, flags (media bit 0 =
keyframe), reserved, and a strict payload length. Legacy v1 datagrams began
with `"ET"`, so the two are unambiguous; v2 is otherwise a **clean break**
(each side tells the user to update the other on contact with v1).

**Media** (type 0x01): a 32-byte header — session id, stream epoch, frame
sequence, fragment index/count (≤3066 ≈ 4 MiB per frame), capture timestamp
(µs on the host process clock) — followed by a raw Annex B chunk. No
serialization framework; width/height/codec travel in the control plane.

**Control** (16-byte header: session id, message sequence, type):

| Message | Direction | Purpose |
| --- | --- | --- |
| HELLO2 / HELLO_ACK | C→H / H→C | Session establishment: capability bits (H.264/HEVC decode, wants-input), screen size/refresh, nonce-idempotent ACK carrying session id, host-dictated timing, and the stream config |
| HEARTBEAT | H→C | 1 Hz liveness + embedded stream config (self-heals lost config changes) |
| RECEIVER_REPORT | C→H | 500 ms cadence: loss, completion, jitter, depths — feeds ABR and doubles as client liveness |
| KEYFRAME_REQUEST | C→H | Loss/decode-error recovery; host rate-limits to 1 per 500 ms |
| PING / PONG | C→H→C | NTP-style clock sync (min-RTT offset) for the honest end-to-end latency readout |
| STREAM_CONFIG | H→C | Immediate notify on bitrate/codec/resolution change |
| INPUT_EVENT | C→H | Input relay (below) |
| BYE | both | Clean teardown with a reason (user, backgrounded, shutdown) |

**Session rules** (host, pure state machine): one client at a time — a second
device gets `busy`; the same device reconnecting supersedes in place with a
fresh session id; duplicate HELLO2 nonces get an identical ACK (retransmit
tolerance); liveness expires 3 s after the last report/input, which also
tears down the virtual display.

## Reliability

- **ABR**: a bitrate ladder (4–20 Mbps, capped by the GUI "Max bitrate"
  slider) driven by receiver reports — loss or PLI pressure steps down
  (cooldown 3 s), 15 s of clean reports steps back up. A bitrate change
  reopens the encoder session (~50–200 ms, same epoch) and forces an IDR.
- **Pacing**: keyframe bursts are sent in batches with microsleeps under a
  hard 3 ms wall-clock budget, plus a 4 MiB kernel send buffer — smoothing
  WiFi loss without adding latency.
- **Recovery**: the client requests keyframes on gaps/decode errors; the host
  honors at most 2/s. The app shows "SIGNAL LOST" after 3 s of silence and
  auto-reconnects with backoff.

## Input relay

The iPad normalizes touches over the displayed video (letterbox-corrected) to
a 0–65535 grid. A pure gesture machine turns raw touches into events: tap =
click at the touch-down point, drag commits after slop, two fingers scroll
(direct-manipulation direction), a 500 ms hold right-clicks, Apple Pencil
presses immediately with pressure. Press/release edges are sent twice with one
event id; the host dedupes, maps through the captured output's desktop
rectangle onto the Windows virtual screen, and injects with `SendInput`.
Sessions that didn't set the wants-input capability are never injected for.

## Codecs

H.264 is the default everywhere. With the host's "Prefer HEVC" setting on and
a client that advertises HEVC decode, the encoder live-switches to the HEVC
sibling (NVENC/AMF/QSV/VideoToolbox/x265) via the same reopen mechanism. The
iPad decoder detects the codec **from the bitstream** (an HEVC VPS in a
keyframe), so the switch has no config/media ordering race. HEVC encoders are
configured to repeat VPS/SPS/PPS in-band; H.264 keyframes get cached SPS/PPS
prepended (the AMF path has extra guards developed against real hardware).

## Discovery

The host advertises `_eternaldisplay._udp` over mDNS with `version`,
`proto=2`, and `platform` TXT records, re-upserted every 60 s (no
unregister gap) and withdrawn with goodbye packets on exit. Manual IP entry
and the QR code remain the fallback for networks that filter multicast.

## Portability and testing

Everything protocol- or logic-shaped lives in the pure `eternal-wire` crate
(v2 codecs, H.264/HEVC bitstream helpers, reassembly) or in host modules with
injected clocks (session, ABR, pacer, supervisor, input mapping) — all tested
on every platform. Windows-only code (DXGI, SendInput, VDD control) is
`cfg(windows)`-gated; a synthetic capture source with a machine-readable
frame counter lets the full host run on macOS/CI, where the Rust E2E suite
drives a fake receiver through handshake, loss, crash-recovery, input, and
HEVC scenarios, and `scripts/e2e_ios.sh` streams into the real iPad app in
the simulator. Golden wire vectors are parsed byte-for-byte by both
languages. What only hardware can prove (GPU encoders, the display driver,
real WiFi) is enumerated in the release hardware runbook.
