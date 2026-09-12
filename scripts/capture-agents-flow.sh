#!/usr/bin/env bash
# Capture synthetic Agents flow states in the native Linux app under Xvfb.
# Requires Xvfb, openbox, xdotool and ImageMagick; never uses provider credentials.
set -euo pipefail
fixture_repo_root="$(cd "$(dirname "$0")/.." && pwd)"
fixture_output="${1:?Usage: scripts/capture-agents-flow.sh OUTPUT_DIRECTORY}"
mkdir -p "$fixture_output"
fixture_output="$(cd "$fixture_output" && pwd)"
cd "$fixture_repo_root"
cargo build --locked -p zeron-ui --example agents-fixture --features browser-fixture
fixture_display_file=$(mktemp)
Xvfb -displayfd 3 -screen 0 1280x1100x24 3>"$fixture_display_file" >"$fixture_output/xvfb.log" 2>&1 &
fixture_xvfb_pid=$!
fixture_wm_pid=
cleanup() {
    if [ -n "$fixture_wm_pid" ]; then kill "$fixture_wm_pid" 2>/dev/null || true; fi
    kill "$fixture_xvfb_pid" 2>/dev/null || true
    rm -f "$fixture_display_file"
}
trap cleanup EXIT
for _ in {1..40}; do
    [ -s "$fixture_display_file" ] && break
    sleep 0.1
done
[ -s "$fixture_display_file" ] || { echo "Xvfb did not start" >&2; exit 1; }
export DISPLAY=":$(cat "$fixture_display_file")"
openbox >"$fixture_output/openbox.log" 2>&1 &
fixture_wm_pid=$!
unset WAYLAND_DISPLAY
export XDG_SESSION_TYPE=x11 LIBGL_ALWAYS_SOFTWARE=1 ZERON_NO_BROWSER=1
./target/debug/examples/agents-fixture "$fixture_output" >"$fixture_output/fixture.log" 2>&1
