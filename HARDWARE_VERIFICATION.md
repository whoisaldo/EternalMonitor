# v0.2.0 Hardware Verification Runbook

What CI cannot prove: real GPU encoders, the virtual display driver, real
WiFi, and touch feel. Run this on the Windows PC plus a real iPad before
tagging the release. Budget about 45 minutes, plus per-GPU repeats if you
can borrow AMD or Intel machines. Check items off; anything that fails gets
logs (see the last section) and a fix round before the tag.

Conventions: "Expect" is the pass condition. "Host log" means the Copy logs
button on the Stream tab, or
`%APPDATA%\EternalMonitor\logs\eternal-host-session.log`.

## A. Install & first light (5 min)

- [ ] **A1 Installer.** Run `EternalMonitor-Setup.exe` (SmartScreen → "Run
  anyway", one UAC prompt). Expect: install completes, host launches, no
  second UAC. Upgrading over a previous version keeps settings.
- [ ] **A2 Firewall.** On first run tick BOTH Private and Public. Expect:
  the prompt appears exactly once.
- [ ] **A3 Banner.** Host log shows `EternalMonitor v0.2.0`, the right GPU
  name, and a hardware encoder (not x264), with no fallback banner in the
  GUI.

## B. Basic streaming (8 min)

- [ ] **B1 Manual IP connect.** Enter the host IP on the iPad. Expect: a
  picture in under 2 s, iPad HUD around 60 fps, host Stream tab shows the
  client.
- [ ] **B2 Discovery.** The iPad Scan list finds the host. Leave the list
  open 4+ minutes, which crosses two 60 s re-advertisements. Expect: the
  host never blinks out of the list. Quit the host app. Expect: it leaves
  the list within a few seconds (mDNS goodbye), not after minutes.
- [ ] **B3 QR connect.** Scan the host's QR from the iPad. Expect: it
  connects to the same address the GUI shows.
- [ ] **B4 Truthful readouts.** The host Stream tab codec matches the iPad
  Settings HOST module (name, resolution, fps, codec, bitrate), and the
  HOST bitrate follows the adaptive rung, not just the slider.
- [ ] **B5 Latency sanity.** Drag a window in circles. The iPad HUD's ms
  readout should sit in the tens (typically 20 to 80 ms on good WiFi) and
  the motion should feel attached. With a 240 fps camera, film both screens
  and count frames; the HUD claim should land within about ±20 ms of
  measured.
- [ ] **B6 Decoder.** iPad Settings diagnostics say "hardware decoder". The
  simulator's software path must not appear on a real device.

## C. Input relay (7 min)

- [ ] **C1 Click targets.** Tap small targets (window close buttons) in all
  four screen corners. Expect: exact hits with no offset. This validates
  the desktop-rect mapping; test at 100% AND at 150% display scaling.
- [ ] **C2 Drag.** Drag a window smoothly; select text; starting a
  two-finger scroll must not produce a stray click.
- [ ] **C3 Scroll.** Two-finger scroll in a browser. Expect: content
  follows the fingers (direct-manipulation direction), smooth, both axes.
- [ ] **C4 Right-click.** Hold about half a second. Expect: a context menu
  at the touch point. A tap elsewhere dismisses it with a single click, not
  a double.
- [ ] **C5 Pencil.** In Paint or a whiteboard, ink starts immediately on
  contact (no tap-vs-drag delay) and pressure varies the stroke where the
  app supports it.
- [ ] **C6 Multi-monitor.** With a second physical monitor attached,
  capture monitor 2. Touches must land on monitor 2, never the primary.
- [ ] **C7 View-only.** Turn "Control PC with touch" off and reconnect.
  Expect: touches do nothing on the PC, and a single tap toggles the HUD.

## D. Reliability (8 min)

- [ ] **D1 Host death.** Kill the host from Task Manager mid-stream.
  Expect: the iPad shows SIGNAL LOST within about 3 s. Relaunch the host.
  Expect: the iPad reconnects by itself within about 10 s, no taps.
- [ ] **D2 ABR under real loss.** Walk toward the edge of WiFi range (or
  run the microwave). Expect: the picture softens as the host bitrate steps
  down, with no multi-second freezes; walking back sharpens it within about
  20 s.
- [ ] **D3 Live bitrate change.** Move the Max-bitrate slider mid-stream.
  Expect: at most a sub-second hiccup, no disconnect.
- [ ] **D4 Backgrounding.** Swipe the app away to the switcher. Expect: the
  host Stream tab returns to "waiting for client" within about 3 s (BYE),
  and a virtual display tears down. Reopen the app. Expect: it resumes by
  itself ("Resume after switching apps" defaults on).
- [ ] **D5 Second device busy.** While one iPad streams, connect from a
  second device. Expect: a clear "host is busy" message, and the first
  stream is untouched.
- [ ] **D6 Version mismatch UX** (if a v0.1.x build is still around). Old
  app → new host and new app → old host each show an explicit "update the
  other side" message, not garbage video.

## E. HEVC (5 min, repeat per GPU vendor available)

- [ ] **E1 Switch on.** Mid-stream, tick "Prefer HEVC". Expect: the iPad
  HOST module codec flips to HEVC within about 1 s, the picture stays
  clean, no reconnect. Untick and it returns to H.264 the same way.
- [ ] **E2 Quality and limits.** At the same bitrate HEVC should look no
  worse than H.264. Watch 2+ minutes for artifacts, especially on AMF,
  whose `header_insertion_mode` handling is the least-proven path. On any
  breakage: note the GPU and driver version, collect logs, and leave the
  toggle off.
- [ ] **E3 Fallback.** On a GPU without an HEVC encoder, the toggle warns
  once in the log and keeps streaming H.264 with no error loop.

## F. Extended display & resolution match (8 min)

- [ ] **F1 Lifecycle.** Select "Extended display (iPad)" and Restart stream
  with the iPad connected. Expect: a new display appears in Windows Display
  settings, windows drag onto it, and the iPad shows it. Disconnect the
  iPad. Expect: the virtual display disappears within about 5 s. Quit,
  relaunch, or crash must never strand a phantom monitor.
- [ ] **F2 Native resolution.** With "Match extended display to the iPad's
  resolution" on (the default), the virtual display's mode equals the
  iPad's native landscape resolution (for example 2420×1668) and the iPad
  picture is edge-to-edge with no letterbox. Check that
  `C:\VirtualDisplayDriver\vdd_settings.xml` exists and lists that mode
  first. Toggle the match off and restart. Expect: the driver's default
  mode (a letterboxed picture is fine here).
- [ ] **F3 120 Hz mode** (ProMotion iPad). With the match on, Windows
  offers the panel refresh, or falls back to the 60 Hz variant without
  erroring.

## G. Encoder deep checks (per vendor, about 5 min each)

- [ ] **G1 Idle VBV (real PTS).** Leave a static desktop for 60 s. Expect:
  bandwidth on the Performance tab collapses to keepalives, and the first
  motion afterwards is clean, not a smear. If pacing looks wrong on NVENC
  or AMF, retry with `set ETERNAL_LEGACY_PTS=1` and report; that escape
  hatch existing is why this item is here.
- [ ] **G2 AMF specifics** (AMD box). Startup shows a keyframe (no black
  screen), recovery after loss works, and 10 minutes of streaming shows no
  periodic freeze. If broken: `set ETERNAL_AMF_DIAG=1`, reproduce, send
  `%APPDATA%\EternalMonitor\diagnostics\`.
- [ ] **G3 High refresh.** `set ETERNAL_FPS=120` with a ProMotion iPad.
  Expect: HUD at 100+ fps on a strong network with no capture-side stutter.
- [ ] **G4 Stop/Start.** GUI Stop, then Start. Expect: a clean halt and a
  fresh stream that the iPad resumes automatically.

## H. Long soak (run in the background of the above)

- [ ] **H1.** Keep one stream up 30+ minutes. Expect: no leak-shaped memory
  growth on either end (Task Manager, Xcode gauge), no thermal shutdown of
  the stream, and sane HUD stats throughout.

## When something fails

1. Host: Copy logs (Stream tab) or grab
   `%APPDATA%\EternalMonitor\logs\eternal-host-session.log`.
2. iPad: the diagnostics list in Settings (most recent events), plus what
   the screen showed.
3. Note the GPU model and driver version, the WiFi band, and which runbook
   item failed.
4. AMD encode issues: also send `%APPDATA%\EternalMonitor\diagnostics\`
   captured with `ETERNAL_AMF_DIAG=1`.

Fixes land, the failing items get re-run, and only then does `v0.2.0` get
tagged. The tag builds and publishes the installer automatically.
