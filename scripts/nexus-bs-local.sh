#!/bin/bash
# SPDX-FileCopyrightText: 2026 Nexus-BS contributors
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

# Run the full Nexus-BS stack (core + control service + dashboard) directly
# from the source tree in a tmux session — no systemd required.
#
# Usage:
#   scripts/nexus-bs-local.sh [config.toml]   start tmux session "nexus-bs"
#   scripts/nexus-bs-local.sh --kill          stop the session
#   scripts/nexus-bs-local.sh --force ...     start even if ports are busy
#
# Defaults:
#   config    example_config/config.toml
#   binaries  target/release (built automatically if missing)
#   session   nexus-bs  (windows: 0=core, 1=control, 2=dashboard)
#   logs      .dev-run/{core,control,dashboard}.log
#   dashboard http://<host>:8080  (telemetry 9001, control 9002/9003)
#
# Send a control command while running:
#   NEXUS_BS_CONTROL_FIFO="$PWD/.dev-run/control.commands" \
#     scripts/nexus-bs-control '{"type":"..."}'

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
cd "$REPO_ROOT"

SESSION="${NEXUS_BS_TMUX_SESSION:-nexus-bs}"
BIN_DIR="$REPO_ROOT/target/release"
RUN_DIR="$REPO_ROOT/.dev-run"
CONFIG="$REPO_ROOT/example_config/config.toml"
FORCE=0

for arg in "$@"; do
    case "$arg" in
        --kill)
            if tmux has-session -t "$SESSION" 2>/dev/null; then
                tmux kill-session -t "$SESSION"
                echo "tmux session '$SESSION' stopped."
            else
                echo "tmux session '$SESSION' is not running."
            fi
            exit 0
            ;;
        --force) FORCE=1 ;;
        --*) echo "Unknown option: $arg" >&2; exit 2 ;;
        *) CONFIG="$arg" ;;
    esac
done

command -v tmux >/dev/null 2>&1 || { echo "error: tmux is required (apt install tmux)" >&2; exit 1; }
[ -f "$CONFIG" ] || { echo "error: config not found: $CONFIG" >&2; exit 1; }
CONFIG=$(CDPATH= cd -- "$(dirname -- "$CONFIG")" && pwd)/$(basename "$CONFIG")

if tmux has-session -t "$SESSION" 2>/dev/null; then
    echo "tmux session '$SESSION' already running."
    echo "  attach:  tmux attach -t $SESSION"
    echo "  stop:    $0 --kill"
    exit 1
fi

# --- build (release) if missing ---------------------------------------------
need_build=0
for bin in nexus-bs nexus-bs-control-service nexus-bs-dashboard; do
    [ -x "$BIN_DIR/$bin" ] || need_build=1
done
if [ "$need_build" -eq 1 ]; then
    echo "Building release binaries (first run may take a while)..."
    [ -f deps/env.sh ] && . ./deps/env.sh
    cargo build --release -p nexus-bs -p nexus-bs-control -p nexus-bs-dashboard
fi

# --- port check ---------------------------------------------------------------
port_in_use() {
    (echo > "/dev/tcp/127.0.0.1/$1") 2>/dev/null
}
busy=""
for port in 8080 9001 9002 9003; do
    if port_in_use "$port"; then busy="$busy $port"; fi
done
if [ -n "$busy" ] && [ "$FORCE" -eq 0 ]; then
    echo "error: port(s)$busy already in use (systemd services running?)."
    echo "  stop them:  sudo systemctl stop nexus-bs nexus-bs-control nexus-bs-dashboard"
    echo "  or use:     $0 --force $CONFIG"
    exit 1
fi

# --- run dir + launcher scripts ----------------------------------------------
mkdir -p "$RUN_DIR"
rm -f "$RUN_DIR/control.commands" "$RUN_DIR/core.log" "$RUN_DIR/control.log" "$RUN_DIR/dashboard.log"
mkfifo "$RUN_DIR/control.commands"

RUST_LOG_CORE="${RUST_LOG_CORE:-info,tetra_entities::umac=warn,tetra_entities::llc=warn,tetra_entities::mle=warn,tetra_entities::mm=warn,tetra_entities::cmce=warn,tetra_entities::sndcp=warn,tetra_entities::lmac=warn,tetra_entities::phy=warn,tetra_entities::network=warn,tetra_entities::net_telemetry=warn,tetra_entities::messagerouter=warn}"

cat > "$RUN_DIR/run-core.sh" <<'COREEOF'
#!/bin/sh
cd "$NEXUS_LOCAL_REPO"
[ -f "$NEXUS_LOCAL_REPO/deps/env.sh" ] && [ -d "$NEXUS_LOCAL_REPO/deps/soapy-install" ] && . "$NEXUS_LOCAL_REPO/deps/env.sh"
export RUST_LOG="${NEXUS_LOCAL_RUST_LOG}"
"$NEXUS_LOCAL_BIN/nexus-bs" "$NEXUS_LOCAL_CONFIG" 2>&1 | tee -a "$NEXUS_LOCAL_RUN/core.log"
echo ""
echo "[nexus-bs-local] core exited — log above / in core.log. Stop: scripts/nexus-bs-local.sh --kill"
exec sleep infinity
COREEOF

cat > "$RUN_DIR/run-control.sh" <<'CTLEOF'
#!/bin/sh
cd "$NEXUS_LOCAL_REPO"
{ while :; do cat "$NEXUS_LOCAL_RUN/control.commands"; done |
  "$NEXUS_LOCAL_BIN/nexus-bs-control-service" --listen 127.0.0.1:9002 --command-listen 127.0.0.1:9003 2>&1; } |
  tee -a "$NEXUS_LOCAL_RUN/control.log"
echo ""
echo "[nexus-bs-local] control service exited — log above / in control.log"
exec sleep infinity
CTLEOF

cat > "$RUN_DIR/run-dashboard.sh" <<'DASHEOF'
#!/bin/sh
cd "$NEXUS_LOCAL_REPO"
export NEXUS_BS_DASHBOARD_BIND="${NEXUS_BS_DASHBOARD_BIND:-0.0.0.0}"
export NEXUS_BS_DASHBOARD_PORT="${NEXUS_BS_DASHBOARD_PORT:-8080}"
export NEXUS_BS_DASHBOARD_STATIC_DIR="$NEXUS_LOCAL_REPO/dashboard"
export NEXUS_BS_PERSISTENT_CONFIG="$NEXUS_LOCAL_CONFIG"
export NEXUS_BS_DASHBOARD_TELEMETRY_LISTEN=127.0.0.1:9001
export NEXUS_BS_DASHBOARD_CONTROL_URL=http://127.0.0.1:9003/command
export RUST_LOG="${RUST_LOG:-info}"
"$NEXUS_LOCAL_BIN/nexus-bs-dashboard" 2>&1 | tee -a "$NEXUS_LOCAL_RUN/dashboard.log"
echo ""
echo "[nexus-bs-local] dashboard exited — log above / in dashboard.log"
exec sleep infinity
DASHEOF

# Bake the runtime paths into the launchers (quoted heredocs keep the
# override-able env vars above intact at run time).
for f in run-core.sh run-control.sh run-dashboard.sh; do
    sed -i "2i NEXUS_LOCAL_REPO='$REPO_ROOT'\nNEXUS_LOCAL_BIN='$BIN_DIR'\nNEXUS_LOCAL_RUN='$RUN_DIR'\nNEXUS_LOCAL_CONFIG='$CONFIG'\nNEXUS_LOCAL_RUST_LOG='$RUST_LOG_CORE'" "$RUN_DIR/$f"
done
chmod +x "$RUN_DIR"/run-*.sh

# --- tmux session -------------------------------------------------------------
tmux new-session  -d -s "$SESSION" -n core      "$RUN_DIR/run-core.sh"
tmux new-window   -t "$SESSION":1 -n control    "$RUN_DIR/run-control.sh"
tmux new-window   -t "$SESSION":2 -n dashboard  "$RUN_DIR/run-dashboard.sh"
tmux select-window -t "$SESSION":0

echo ""
echo "Nexus-BS dev session '$SESSION' started."
echo ""
echo "  config:     $CONFIG"
echo "  attach:     tmux attach -t $SESSION"
echo "  windows:    0=core  1=control  2=dashboard   (prefix Ctrl-b, then 0/1/2)"
echo "  stop:       $0 --kill"
echo "  dashboard:  http://$(hostname -I 2>/dev/null | awk '{print $1}'):${NEXUS_BS_DASHBOARD_PORT:-8080}"
echo "  logs:       $RUN_DIR/{core,control,dashboard}.log"
echo ""
echo "  control cmd: NEXUS_BS_CONTROL_FIFO=\"$RUN_DIR/control.commands\" scripts/nexus-bs-control '<json>'"
echo ""

if [ -t 0 ]; then
    tmux attach -t "$SESSION"
fi
