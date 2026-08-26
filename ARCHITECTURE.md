# EternalMonitor architecture

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

Host stages are dedicated OS threads. Capture hands frames to the encoder
through a latest-wins slot: an unconsumed frame gets displaced, so the
encoder always works on the freshest picture. The encoder hands access units
to transport through a lossless channel, because dropping an encoded P-frame
would corrupt the GOP. Frame pixels travel in `Arc<Vec<u8>>` buffers that
get recycled; steady state does one full-frame copy, the DXGI staging
readback.

The supervisor owns the pipeline. Stage threads report their exits, wedge
watchdogs fire on silent stalls (loop heartbeat stale 3 s, no frame for 5 s
with a client connected, encoder flat 3 s), and restarts back off
exponentially with a restart-storm brake. The client session lives outside
the pipeline, so a crash-restart resumes streaming to the same session. The
client sees a new stream epoch, resets reassembly, and never re-handshakes.

## Wire protocol v2

One UDP socket carries both media and control. Every datagram starts with an
8-byte prefix: magic `"EM"`, version `2`, packet type, flags (media bit 0 =
keyframe), a reserved byte, and a strict payload length. Legacy v1 datagrams
began with `"ET"`, so the two never get confused. v2 is otherwise a clean
break; each side tells the user to update the other on contact with v1.

Media (type 0x01) is a 32-byte header followed by a raw Annex B chunk. The
header carries session id, stream epoch, frame sequence, fragment
index/count (up to 3066, about 4 MiB per frame), and the capture timestamp
in microseconds on the host process clock. There is no serialization
framework; width, height, and codec travel in the control plane.

Control messages share a 16-byte header (session id, message sequence,
type):

| Message | Direction | Purpose |
| --- | --- | --- |
| HELLO2 / HELLO_ACK | C→H / H→C | Session establishment. Capability bits (H.264/HEVC decode, wants-input), screen size and refresh, a nonce-idempotent ACK carrying the session id, host-dictated timing, and the stream config |
| HEARTBEAT | H→C | 1 Hz liveness plus the embedded stream config, which self-heals lost config changes |
| RECEIVER_REPORT | C→H | Every 500 ms: loss, completion, jitter, queue depths. Feeds ABR and doubles as client liveness |
| KEYFRAME_REQUEST | C→H | Recovery after loss or a decode error; the host rate-limits to one per 500 ms |
| PING / PONG | C→H→C | NTP-style clock sync (min-RTT offset) behind the HUD's measured end-to-end latency |
| STREAM_CONFIG | H→C | Immediate notify on a bitrate, codec, or resolution change |
| INPUT_EVENT | C→H | Input relay (below) |
| BYE | both | Clean teardown with a reason (user, backgrounded, shutdown) |

Session rules, implemented as a pure state machine on the host: one client
at a time, and a second device gets `busy`. The same device reconnecting
supersedes in place with a fresh session id. Duplicate HELLO2 nonces get an
identical ACK, which makes handshake retransmits harmless. Liveness expires
3 s after the last report or input event, and expiry also tears down the
virtual display.

## Reliability

- ABR: a bitrate ladder (4 to 20 Mbps, capped by the GUI "Max bitrate"
  slider) driven by receiver reports. Loss or keyframe-request pressure
  steps down with a 3 s cooldown; 15 s of clean reports steps back up. A
  bitrate change reopens the encoder session (50 to 200 ms, same epoch) and
  forces an IDR.
- Pacing: keyframe bursts go out in batches with microsleeps under a hard
  3 ms wall-clock budget, on a socket with a 4 MiB kernel send buffer. This
  smooths WiFi loss without adding latency.
- Recovery: the client requests keyframes on gaps and decode errors; the
  host honors at most two per second. The app shows "SIGNAL LOST" after 3 s
  of silence and reconnects with backoff.

## Input relay

The iPad normalizes touches over the displayed video (letterbox-corrected)
to a 0–65535 grid. A pure gesture machine turns raw touches into events: tap
means click at the touch-down point, a drag commits after slop, two fingers
scroll in the direct-manipulation direction, a 500 ms hold right-clicks, and
Apple Pencil presses immediately with pressure. Press/release edges are sent
twice with one event id; the host dedupes, maps through the captured
output's desktop rectangle onto the Windows virtual screen, and injects with
`SendInput`. Sessions that didn't set the wants-input capability never get
injected for.

## Codecs

H.264 is the default everywhere. With the host's "Prefer HEVC" setting on
and a client that advertises HEVC decode, the encoder live-switches to the
HEVC sibling (NVENC/AMF/QSV/VideoToolbox/x265) through the same reopen
mechanism. The iPad decoder detects the codec from the bitstream itself (an
HEVC VPS in a keyframe), so the switch has no ordering race between config
and media. HEVC encoders repeat VPS/SPS/PPS in-band; H.264 keyframes get
cached SPS/PPS prepended. The AMF path carries extra guards developed
against real hardware.

## Discovery

The host advertises `_eternaldisplay._udp` over mDNS with `version`,
`proto=2`, and `platform` TXT records. The advertisement re-upserts every
60 s without an unregister gap and goes out with goodbye packets on exit.
Manual IP entry and the QR code remain the fallback for networks that
filter multicast.

## Portability and testing

Everything protocol- or logic-shaped lives in the pure `eternal-wire` crate
(v2 codecs, H.264/HEVC bitstream helpers, reassembly) or in host modules
with injected clocks (session, ABR, pacer, supervisor, input mapping), all
tested on every platform. Windows-only code (DXGI, SendInput, VDD control)
sits behind `cfg(windows)`. A synthetic capture source with a
machine-readable frame counter lets the full host run on macOS and CI,
where the Rust E2E suite drives a fake receiver through handshake, loss,
crash-recovery, input, and HEVC scenarios, and `scripts/e2e_ios.sh` streams
into the real iPad app in the simulator. Golden wire vectors are parsed
byte-for-byte by both languages. What only hardware can prove (GPU
encoders, the display driver, real WiFi) is enumerated in
HARDWARE_VERIFICATION.md.
