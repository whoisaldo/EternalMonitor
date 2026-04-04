# EternalMonitor

[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?style=flat&labelColor=111)](https://eternalmonitor.dev)

**Website:** [eternalmonitor.dev](https://eternalmonitor.dev)

Use your iPad as a low-latency Windows display receiver over local-network UDP.

This repo currently contains a working Windows host capture/encode/transport path and a working iPad receive/decode/render path. The known-good stream state is commit `bc44770` on branch `feature/rust-workspace-bootstrap`.

## Current status

Implemented now:

- Windows host captures the primary display with DXGI Desktop Duplication
- Host converts BGRA to YUV420P and encodes with NVENC H.264
- Host advertises an mDNS/DNS-SD service and streams frames over UDP on port `9876`
- iPad app receives fragmented UDP datagrams, reassembles `FramePacket` payloads, decodes with VideoToolbox, and renders with Metal
- Direct connect by IP works end to end with the current transport format

Not done yet:

- Virtual display driver integration
- USB transport
- Input relay back to Windows
- Reliability layer beyond basic UDP fragmentation/reassembly
- Production-grade zero-config discovery

Known caveat:

- The iPad "Scan network" path may fail to find the host even when direct IP connect works. Treat discovery as incomplete and use manual IP entry when needed.

## How it works

1. Windows host captures the desktop with DXGI Desktop Duplication
2. Frames are read back, converted, and encoded with NVENC H.264
3. Encoded `FramePacket` payloads are fragmented into UDP datagrams
4. The iPad app reassembles and parses those payloads
5. VideoToolbox decodes frames and Metal renders them

Current target is practical local-network streaming, not a finished second-monitor product yet.

## Repo layout

```text
host/     Rust Windows host: capture, encode, UDP transport, mDNS advertisement
ios/      Swift iPad app: connect UI, UDP receive, reassembly, decode, render
proto/    Shared protocol serialization code and schemas
```

## Build and run

Requires Windows with Rust stable MSVC and an FFmpeg/NVENC-capable setup for the host. The iOS app must be built on macOS with Xcode for a physical device.

### Windows host

```powershell
cargo build -p eternal-host
& "C:\Users\aliyo\OneDrive\Desktop\EternalMonitor\target\debug\eternal-host.exe"
```

If you need to change the listen port:

```powershell
& "C:\Users\aliyo\OneDrive\Desktop\EternalMonitor\target\debug\eternal-host.exe" 9876
```

### iOS app

The iOS project is generated with XcodeGen:

```bash
cd ios
xcodegen generate
```

Then open the generated Xcode project on macOS, build the `EternalMonitor` target, and run it on a physical iPad.

## Troubleshooting

- If the iPad says no complete frame was reassembled, make sure both the Windows host and the iPad app were rebuilt from the same revision. The UDP fragment header changed in the working transport fix.
- If scan finds nothing, try direct IP connect first. Discovery failure does not necessarily mean streaming is broken.
- If the host binary fails to rebuild on Windows with access denied for `eternal-host.exe`, close the running GUI process first.

## Reference docs

- [ARCHITECTURE.md](ARCHITECTURE.md)
- [DECISIONS.md](DECISIONS.md)
- [ROADMAP.md](ROADMAP.md)
- [CLAUDE.md](CLAUDE.md)

## License

See [LICENSE](LICENSE).
