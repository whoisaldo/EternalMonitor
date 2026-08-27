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

## Getting a tester onto the iPad app (TestFlight)

A tester needs no Xcode and no developer account. They need the TestFlight
app from the App Store and a link from you.

### One-time, on your Mac

Build number must be higher than anything already uploaded. `ios/project.yml`
carries `CURRENT_PROJECT_VERSION`; builds 3 and 4 are used, so 0.2.0 ships as
build 5. If you upload twice for the same version, bump it again.

The simplest path is Xcode:

1. `cd ios && xcodegen generate`, then open `EternalMonitor.xcodeproj`.
2. Select "Any iOS Device" as the destination, then Product, then Archive.
3. In the Organizer window that opens: Distribute App, then TestFlight and
   App Store Connect, then Upload. Signing is automatic against team
   `9X79V37Q89`.

The same thing without the GUI, if you have an App Store Connect API key:

```bash
cd ios
xcodegen generate
xcodebuild -project EternalMonitor.xcodeproj -scheme EternalMonitor \
  -configuration Release -destination 'generic/platform=iOS' \
  -archivePath build/EternalMonitor.xcarchive archive
xcodebuild -exportArchive \
  -archivePath build/EternalMonitor.xcarchive \
  -exportOptionsPlist exportOptions.plist \
  -exportPath build/export \
  -authenticationKeyPath ~/private_keys/AuthKey_XXXXXX.p8 \
  -authenticationKeyID XXXXXX -authenticationKeyIssuerID <issuer-uuid>
```

`exportOptions.plist` is already set to `app-store-connect` with
`destination: upload`, so the export step performs the upload.

### Then, in App Store Connect

1. TestFlight tab, wait for the build to finish processing (usually a few
   minutes; you get an email).
2. Export compliance is already answered in the app
   (`ITSAppUsesNonExemptEncryption` is false), so it will not ask per build.
3. Create an external testing group, add the build, and fill in "What to
   Test" (see the AMD notes below).
4. Submit for Beta App Review. The first build for external testers is
   reviewed by Apple, usually inside a day.
5. Once approved, enable the group's public link and send that to your
   tester. Anyone with the link can install; you can cap the number of
   testers on the same screen.

Internal testers skip review entirely and get builds immediately, but they
must be users on your App Store Connect team, so that route only makes sense
for people you want inside the developer account.

### What to send the tester

Two links, and they must match:

- The Windows installer for the same version. While a build is still in
  testing it is published as a GitHub pre-release and appears on
  eternalmonitor.dev/download.html under "Preview build for testers",
  marked as a test build.
- The TestFlight public link for the matching iPad build.

Protocol v2 is a clean break, so a preview host with a release app (or the
reverse) will not stream. Both sides say so plainly rather than showing
broken video, but it still wastes a tester's evening.

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
reverse) won't stream. Each side shows an explicit "update the other half"
message instead of corrupted video, so at least the failure is obvious. Hand
out the matching TestFlight build and installer together.

## New in v0.2.0, worth testing on purpose

- Input relay: tap, drag, two-finger scroll, hold for right-click, and the
  Pencil should feel like a trackpad. Check multi-monitor setups (clicks
  must land on the captured screen) and the "Control PC with touch" toggle
  off (the host must ignore touches).
- HEVC: flip "Prefer HEVC" in host Settings mid-stream. The codec in the
  iPad's Settings HOST module should flip to HEVC within a second, and
  back. If video breaks only in HEVC on some GPU, that encoder's HEVC path
  is the bug; collect logs and turn the toggle off.
- Recovery: kill the host mid-stream (Task Manager) and relaunch. The iPad
  should show SIGNAL LOST and reconnect by itself. Walk to the edge of WiFi
  range; the picture should coarsen (bitrate stepping down) rather than
  freeze, and recover afterwards.
- Extended display resolution: with "Match extended display to the iPad's
  resolution" on (the default), the virtual display should come up at the
  iPad's native aspect, with no letterboxing on the iPad.

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

No audio, no USB transport, and no retransmit of lost packets. Loss shows
as a brief artifact or frame skip, then the stream self-heals with a
requested keyframe and steps the bitrate down if loss persists. Sustained
stutter on a clean network IS reportable now; on hotel or guest WiFi it's
still the network. HEVC is experimental and off by default, so if a stream
misbehaves, confirm the codec on the Stream tab before filing it as a
general bug.
