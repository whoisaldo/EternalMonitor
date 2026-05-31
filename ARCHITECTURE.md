# EternalMonitor — Architecture

## Current pipeline

```text
[Windows desktop]
      |
      v
+---------------------------------------------+
| host/ (Rust Windows app)                    |
|                                             |
| DXGI Desktop Duplication                    |
|   -> primary display capture                |
|   -> CPU-readable BGRA frame                |
|                                             |
| ffmpeg-next + NVENC                         |
|   -> BGRA to YUV420P                        |
|   -> H.264 encode                           |
|                                             |
| UDP transport                               |
|   -> FlatBuffer FramePacket                 |
|   -> custom fragmentation header            |
|   -> mDNS advertisement                     |
+---------------------------------------------+
      |
      v
+---------------------------------------------+
| ios/ (Swift iPad app)                       |
|                                             |
| Connect UI                                  |
|   -> manual IP entry                        |
|   -> Bonjour scan attempt                   |
|                                             |
| UDP receiver                                |
|   -> fragment reassembly                    |
|   -> FramePacket parse                      |
|                                             |
| VideoToolbox                                |
|   -> H.264 decode                           |
|                                             |
| Metal MTKView                               |
|   -> render latest decoded frame            |
+---------------------------------------------+
```

## What is implemented

### Capture

- API: DXGI Desktop Duplication
- Current behavior: enumerates all adapters/outputs and duplicates a **selectable** output
  (primary by default), copying it into a CPU-readable staging texture
- The Settings tab exposes a capture-display picker; choosing a virtual output created by
  an Indirect Display Driver turns the iPad into an extended desktop instead of a mirror.
  The capture adapter follows the chosen output; encoder selection stays vendor-based.
- The managed virtual display is brought up **on demand, only once an iPad has connected**, and
  is disabled on exit / target change / startup (and via a panic hook), so it never lingers as a
  phantom monitor. If the captured display is idle, the loop resends the last frame so the iPad
  still receives a startup keyframe.
- Output format passed downstream: BGRA frame buffer plus frame metadata

This is functional but not yet the final zero-copy path described in earlier docs.

### Encode

- Crate: `ffmpeg-next`
- Codec path: `h264_nvenc`
- Current codec settings:
  - H.264
  - `baseline` profile
  - `gop=30`
  - `max_b_frames=0`
  - `zerolatency=1`
  - `rc=cbr`

The encoder emits Annex B H.264 byte streams that are wrapped in `FramePacket`.

### Transport

- Current transport: WiFi/local-network UDP only
- Registration handshake: iPad sends `ETERNALHELLO` plus its listen port
- Packetization:
  - each encoded frame becomes one FlatBuffer `FramePacket`
  - payload is fragmented into MTU-sized UDP datagrams
  - fragment header is currently `16` bytes
  - fragment index and fragment count are `u16`
  - the header's final 4 bytes carry a per-pipeline-run `stream_epoch`, so the receiver drops
    stale reassembly state immediately on a stream restart (seq reset) instead of inferring it
    from a sequence gap. These bytes were previously reserved/zero, so older receivers that
    ignore them stay wire-compatible.

The `u16` fragment-count change is important. Older host and iPad builds are not wire-compatible with the current transport fix.

### Discovery

- Host side: advertises `_eternaldisplay._udp.local.` over mDNS/DNS-SD
- iPad side: scans via `NetServiceBrowser`

This exists in code, but it is not reliable enough to treat as finished. Direct IP connect is the known-good path.

### iPad receive/decode/render

- UDP datagrams are received with `NWConnection`
- Fragments are reassembled by sequence number
- `FramePacket` is parsed manually from FlatBuffers
- H.264 is decoded with `VTDecompressionSession`
- Frames are rendered with `MTKView`

The renderer currently keeps the latest available decoded texture and draws that.

## Protocol

Implemented message shape in active use:

- `FramePacket`
  - `seq`
  - `timestamp_us`
  - `data`
  - `width`
  - `height`
  - `is_keyframe`

Not all planned protocol families are implemented yet. `InputEvent`, richer control messages, and display configuration exchange are still roadmap items.

## Known-good state

- Working end-to-end UDP stream: commit `bc44770`
- Manual IP connect works
- Network scan/discovery may still fail even when streaming works

## Not implemented yet

- First-party signed display driver (the host currently drives a third-party signed
  virtual display driver; a first-party in-tree IDD is a v0.2.0 goal)
- USB transport
- Reverse input channel
- Reliability controls such as selective NACK
- Dynamic transport switching
