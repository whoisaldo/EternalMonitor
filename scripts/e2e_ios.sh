#!/usr/bin/env bash
# End-to-end system test on a Mac: the real host binary (synthetic capture,
# libx264, headless) streams over localhost UDP to the real iPad app running
# in the Simulator. Success = the app decodes and renders >= EM_WANT_DECODED
# frames at the synthetic source's resolution, proven via the app's
# machine-readable E2E log milestones.
#
# Requirements: Xcode (DEVELOPER_DIR aware), xcodegen, Rust toolchain,
# the ffmpeg@7 Homebrew keg. See README "Development on macOS".
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
export PATH="$HOME/.cargo/bin:$PATH"
export PKG_CONFIG_PATH="${PKG_CONFIG_PATH:-/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig}"

SIM_NAME="${EM_SIM_NAME:-iPad Pro 11-inch (M4)}"
PORT="${EM_PORT:-9876}"
WANT_DECODED="${EM_WANT_DECODED:-120}"
TIMEOUT_SECS="${EM_TIMEOUT:-120}"
SYNTH_W=640
SYNTH_H=360
# BSD mktemp only substitutes TRAILING Xs — no suffix after them.
APP_LOG="$(mktemp /tmp/em_e2e_app.XXXXXX)"
HOST_LOG="$(mktemp /tmp/em_e2e_host.XXXXXX)"

HOST_PID=""
LOG_PID=""
UDID=""
cleanup() {
    [ -n "$LOG_PID" ] && kill "$LOG_PID" 2>/dev/null || true
    [ -n "$UDID" ] && xcrun simctl terminate "$UDID" com.eternal.monitor 2>/dev/null || true
    [ -n "$HOST_PID" ] && kill "$HOST_PID" 2>/dev/null || true
}
trap cleanup EXIT

echo "==> Generating Xcode project"
(cd "$ROOT/ios" && xcodegen generate >/dev/null)

echo "==> Building app for the simulator"
xcodebuild build \
    -project "$ROOT/ios/EternalMonitor.xcodeproj" \
    -scheme EternalMonitor \
    -destination "platform=iOS Simulator,name=$SIM_NAME" \
    -derivedDataPath "$ROOT/ios/build/e2e" \
    CODE_SIGNING_ALLOWED=NO -quiet
APP="$ROOT/ios/build/e2e/Build/Products/Debug-iphonesimulator/EternalMonitor.app"
[ -d "$APP" ] || { echo "FAIL: app bundle not found at $APP"; exit 1; }

echo "==> Building host"
cargo build -q --release -p eternal-host

echo "==> Starting host on 127.0.0.1:$PORT (synthetic ${SYNTH_W}x${SYNTH_H}, libx264, headless)"
ETERNAL_HEADLESS=1 \
ETERNAL_CAPTURE=synthetic \
ETERNAL_SYNTH_SIZE="${SYNTH_W}x${SYNTH_H}" \
ETERNAL_ENCODER=libx264 \
    "$ROOT/target/release/eternal-host" "$PORT" >"$HOST_LOG" 2>&1 &
HOST_PID=$!

echo "==> Booting simulator: $SIM_NAME"
UDID=$(xcrun simctl list -j devices available | /usr/bin/python3 -c '
import json, sys
data = json.load(sys.stdin)["devices"]
name = sys.argv[1]
for devices in data.values():
    for device in devices:
        if device["name"] == name:
            print(device["udid"]); sys.exit(0)
sys.exit(1)
' "$SIM_NAME")
xcrun simctl bootstatus "$UDID" -b >/dev/null

echo "==> Installing app"
xcrun simctl install "$UDID" "$APP"
xcrun simctl terminate "$UDID" com.eternal.monitor 2>/dev/null || true

echo "==> Streaming app E2E log"
xcrun simctl spawn "$UDID" log stream --style compact \
    --predicate 'subsystem == "com.eternal.monitor.e2e"' >"$APP_LOG" 2>&1 &
LOG_PID=$!
sleep 2 # let the log stream attach before the milestones start

echo "==> Launching app with EM_AUTOCONNECT=127.0.0.1:$PORT"
SIMCTL_CHILD_EM_AUTOCONNECT="127.0.0.1:$PORT" \
SIMCTL_CHILD_EM_E2E_LOG=1 \
    xcrun simctl launch "$UDID" com.eternal.monitor >/dev/null

echo "==> Waiting for $WANT_DECODED decoded frames (timeout ${TIMEOUT_SECS}s)"
elapsed=0
decoded=0
until [ "$decoded" -ge "$WANT_DECODED" ]; do
    sleep 2
    elapsed=$((elapsed + 2))
    decoded=$(grep -o 'decoded=[0-9]*' "$APP_LOG" | tail -1 | cut -d= -f2 || true)
    decoded=${decoded:-0}
    if [ "$elapsed" -ge "$TIMEOUT_SECS" ]; then
        echo "FAIL: only $decoded decoded frames after ${TIMEOUT_SECS}s"
        echo "----- app milestones -----"; tail -20 "$APP_LOG"
        echo "----- host log -----"; tail -30 "$HOST_LOG"
        exit 1
    fi
done

first_frame=$(grep -m1 'E2E_FIRST_FRAME' "$APP_LOG" || true)
decoder_kind=$(grep -m1 'E2E_DECODER' "$APP_LOG" || true)
last_stats=$(grep 'E2E_STATS' "$APP_LOG" | tail -1)

if ! grep -q "w=$SYNTH_W h=$SYNTH_H" <<<"$last_stats"; then
    echo "FAIL: decoded resolution mismatch: $last_stats (expected ${SYNTH_W}x${SYNTH_H})"
    exit 1
fi

echo "PASS: $last_stats"
echo "      $first_frame"
echo "      $decoder_kind"
