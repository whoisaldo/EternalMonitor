# v0.2.0 Hardware Verification Runbook

What CI cannot prove: real GPU encoders, the virtual display driver, real
WiFi, and touch feel. Run this on the Windows PC + a real iPad before
tagging the release. Budget ~45 minutes (plus per-GPU repeats if you can
borrow AMD/Intel machines). Check items off; anything that fails gets logs
(see the last section) and a fix round before the tag.

Conventions: **Expect** is the pass condition. `Host log` = Copy logs button
(Stream tab) or `%APPDATA%\EternalMonitor\logs\eternal-host-session.log`.

## A. Install & first light (5 min)

- [ ] **A1 — Installer**: run `EternalMonitor-Setup.exe` (SmartScreen → "Run
  anyway", one UAC prompt). *Expect*: install completes, host launches, no
  second UAC. Upgrading over a previous version keeps settings.
- [ ] **A2 — Firewall**: on first run tick BOTH Private and Public.
  *Expect*: prompt appears exactly once.
- [ ] **A3 — Banner**: host log shows `EternalMonitor v0.2.0`, the right GPU
  name, and a hardware encoder (not x264) with no fallback banner in the GUI.

## B. Basic streaming (8 min)

- [ ] **B1 — Manual IP connect**: iPad → enter the host IP. *Expect*:
  picture in under 2 s, iPad HUD ~60 fps, host Stream tab shows the client.
- [ ] **B2 — Discovery**: iPad Scan finds the host. Leave the scan list open
  4+ minutes (crosses two 60 s re-advertisements). *Expect*: the host never
  blinks out of the list. Quit the host app. *Expect*: it leaves the list
  within a few seconds (mDNS goodbye), not after minutes.
- [ ] **B3 — QR connect**: scan the host's QR from the iPad. *Expect*:
  connects to the same address the GUI shows.
- [ ] **B4 — Truthful readouts**: host Stream tab codec matches the iPad
  Settings → HOST module (name, resolution, fps, codec, bitrate), and the
  HOST bitrate follows the ABR rung, not just the slider.
- [ ] **B5 — Latency sanity**: drag a window in circles; the iPad HUD's ms
  readout should sit in the tens (typically 20–80 ms on good WiFi) and the
  motion should feel attached. If you have a 240 fps camera, film both
  screens and count frames — HUD claim within ~±20 ms of measured.
- [ ] **B6 — Decoder**: iPad Settings diagnostics say "hardware decoder"
  (the simulator's software path must not appear on device).

## C. Input relay (7 min)

- [ ] **C1 — Click targets**: tap small targets (window close buttons) in
  all four screen corners. *Expect*: exact hits — no offset (this validates
  desktop-rect mapping; test at 100% AND at 150% display scaling).
- [ ] **C2 — Drag**: drag a window smoothly; text selection works; no
  spurious clicks when starting a two-finger scroll.
- [ ] **C3 — Scroll**: two-finger scroll in a browser. *Expect*: content
  follows the fingers (direct-manipulation direction), smooth, both axes.
- [ ] **C4 — Right-click**: hold ~½ s. *Expect*: context menu at the touch
  point. Tap elsewhere dismisses it (single click, not double).
- [ ] **C5 — Pencil**: in Paint/whiteboard, ink starts immediately on
  contact (no tap-vs-drag delay) and pressure varies the stroke where
  supported.
- [ ] **C6 — Multi-monitor**: with a second physical monitor attached,
  capture monitor 2 — touches must land on monitor 2, never the primary.
- [ ] **C7 — View-only**: turn "Control PC with touch" off, reconnect.
  *Expect*: touches do nothing on the PC; single-tap toggles the HUD.

## D. Reliability (8 min)

- [ ] **D1 — Host death**: kill the host from Task Manager mid-stream.
  *Expect*: iPad shows SIGNAL LOST within ~3 s. Relaunch the host.
  *Expect*: the iPad reconnects by itself (no taps) within ~10 s.
- [ ] **D2 — ABR under real loss**: walk toward the edge of WiFi range (or
  microwave the link). *Expect*: picture softens (host GUI bitrate steps
  down), no multi-second freezes; walking back sharpens it within ~20 s.
- [ ] **D3 — Live bitrate change**: move the Max-bitrate slider mid-stream.
  *Expect*: a sub-second hiccup at most, no disconnect, no epoch weirdness.
- [ ] **D4 — Backgrounding**: swipe the app away to the switcher. *Expect*:
  host Stream tab returns to "waiting for client" within ~3 s (BYE), and if
  streaming the virtual display it tears down. Reopen the app. *Expect*:
  auto-resume ("Resume after switching apps" default on).
- [ ] **D5 — Second device busy**: while one iPad streams, connect from a
  second (or the simulator). *Expect*: clear "host is busy" message; the
  first stream is untouched.
- [ ] **D6 — Version mismatch UX** (if a v0.1.x build is still around):
  old app → new host and new app → old host each show an explicit "update
  the other side" message, not garbage video.

## E. HEVC (5 min, repeat per GPU vendor available)

- [ ] **E1 — Switch on**: mid-stream, tick "Prefer HEVC". *Expect*: iPad
  HOST module codec flips to HEVC within ~1 s, picture stays clean, no
  reconnect. Untick → back to H.264 the same way.
- [ ] **E2 — Quality/limits**: at the same bitrate HEVC should look no worse
  than H.264. Watch 2+ minutes for artifacts, especially on **AMF** (its
  `header_insertion_mode` handling is the least-proven path). Any breakage:
  note GPU + driver version, collect logs, and leave the toggle off.
- [ ] **E3 — Fallback**: on a GPU without an HEVC encoder, the toggle warns
  once in the log and keeps streaming H.264 (no error loop).

## F. Extended display & resolution match (8 min)

- [ ] **F1 — Lifecycle**: select "Extended display (iPad)" + Restart stream
  with the iPad connected. *Expect*: a new display appears in Windows
  Display settings, windows drag onto it, and the iPad shows it. Disconnect
  the iPad. *Expect*: the virtual display disappears within ~5 s
  (client-lost teardown). Quit/relaunch/crash never strands a phantom
  monitor.
- [ ] **F2 — Native resolution**: with "Match extended display to the iPad's
  resolution" on (default), the virtual display's mode equals the iPad's
  native landscape resolution (e.g. 2420×1668) — the iPad picture is
  edge-to-edge, no letterbox. Check
  `C:\VirtualDisplayDriver\vdd_settings.xml` exists and lists that mode
  first. Toggle the match off + restart. *Expect*: driver default mode
  (letterboxed picture is fine here).
- [ ] **F3 — 120 Hz mode** (ProMotion iPad): with the match on, Windows
  offers the panel refresh (or falls back to the 60 Hz variant without
  erroring).

## G. Encoder deep checks (per vendor; ~5 min each)

- [ ] **G1 — Idle VBV (real PTS)**: leave a static desktop for 60 s.
  *Expect*: bandwidth on the Performance tab collapses (keepalive only), and
  the first motion afterwards is clean, not a smear. If pacing looks wrong
  on NVENC/AMF, retry with `set ETERNAL_LEGACY_PTS=1` and report — that
  escape hatch existing is why this item is here.
- [ ] **G2 — AMF specifics** (AMD box): startup shows a keyframe (no black
  screen), recovery after loss works, and 10 minutes of streaming shows no
  periodic freeze. If broken: `set ETERNAL_AMF_DIAG=1`, reproduce, send
  `%APPDATA%\EternalMonitor\diagnostics\`.
- [ ] **G3 — High refresh**: `set ETERNAL_FPS=120` on a ProMotion iPad.
  *Expect*: HUD ~100+ fps on a strong network, no capture-side stutter.
- [ ] **G4 — Stop/Start**: GUI Stop then Start. *Expect*: clean halt and a
  fresh stream the iPad resumes automatically.

## H. Long soak (run in the background of the above)

- [ ] **H1**: keep one stream up 30+ minutes. *Expect*: no leak-shaped
  memory growth on either end (Task Manager / Xcode gauge), no thermal
  shutdown of the stream, HUD stats stay sane.

## When something fails

1. Host: **Copy logs** (Stream tab) or grab
   `%APPDATA%\EternalMonitor\logs\eternal-host-session.log`.
2. iPad: Settings → the diagnostics list (most recent events), plus what the
   screen showed.
3. Note GPU model + driver version, WiFi band, and which runbook item.
4. AMD encode issues: also `%APPDATA%\EternalMonitor\diagnostics\` with
   `ETERNAL_AMF_DIAG=1` set.

Fixes land, the failing items get re-run, and only then does `v0.2.0` get
tagged (the tag builds and publishes the installer automatically).
