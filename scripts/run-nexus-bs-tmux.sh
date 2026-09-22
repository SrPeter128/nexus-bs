#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Chris YO3TCO / Nexus-BS Project
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#
# Run the Nexus-BS stack (control service, core, dashboard) in a tmux session
# without systemd. Three windows: control, core, dashboard.
#
# Usage:
#   scripts/run-nexus-bs-tmux.sh start   # create the session (default)
#   scripts/run-nexus-bs-tmux.sh attach  # attach to the running session
#   scripts/run-nexus-bs-tmux.sh stop    # kill the session
#   scripts/run-nexus-bs-tmux.sh status  # show session status
#
# Environment overrides:
#   NEXUS_BS_CONFIG           config file (default: example_config/config.toml)
#   NEXUS_BS_DASHBOARD_BIND   dashboard bind (default: 0.0.0.0)
#   NEXUS_BS_DASHBOARD_PORT   dashboard port (default: 8080)
#   NEXUS_BS_TMUX_SESSION     tmux session name (default: nexus-bs)

set -euo pipefail

ROOT="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

SESSION="${NEXUS_BS_TMUX_SESSION:-nexus-bs}"
BIN_CORE="$ROOT/target/release/nexus-bs"
BIN_CONTROL="$ROOT/target/release/nexus-bs-control-service"
BIN_DASHBOARD="$ROOT/target/release/nexus-bs-dashboard"
CONFIG="/etc/nexus-bs/config.toml"
DASHBOARD_BIND="${NEXUS_BS_DASHBOARD_BIND:-0.0.0.0}"
DASHBOARD_PORT="${NEXUS_BS_DASHBOARD_PORT:-8080}"
FIFO="/tmp/nexus-bs-control.$(id -u).commands"

die() {
    echo "error: $*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command '$1' not found in PATH"
}

require_bins() {
    local missing=0
    local bin
    for bin in "$BIN_CORE" "$BIN_CONTROL" "$BIN_DASHBOARD"; do
        if [ ! -x "$bin" ]; then
            echo "missing binary: $bin" >&2
            missing=1
        fi
    done
    if [ "$missing" -eq 1 ]; then
        echo "build them with: cargo build --release -p nexus-bs -p nexus-bs-control -p nexus-bs-dashboard" >&2
        exit 1
    fi
}

session_running() {
    tmux has-session -t "$SESSION" 2>/dev/null
}

cmd_start() {
    require_command tmux
    require_bins

    if session_running; then
        echo "tmux session '$SESSION' already running; attach with: scripts/run-nexus-bs-tmux.sh attach"
        exit 0
    fi

    if [ ! -f "$CONFIG" ]; then
        echo "warning: config file '$CONFIG' not found; core window will fail until you set NEXUS_BS_CONFIG" >&2
    fi

    rm -f "$FIFO"
    mkfifo -m 0600 "$FIFO"

    tmux new-session -d -s "$SESSION" -n control \
        "tail -f $FIFO | $BIN_CONTROL --listen 127.0.0.1:9002 --command-listen 127.0.0.1:9003"
    tmux new-window -t "$SESSION" -n core \
        "$BIN_CORE $CONFIG"
    tmux new-window -t "$SESSION" -n dashboard \
        "$BIN_DASHBOARD --bind $DASHBOARD_BIND --port $DASHBOARD_PORT"

    echo "tmux session '$SESSION' started (windows: control | core | dashboard)"
    echo "  dashboard: http://$DASHBOARD_BIND:$DASHBOARD_PORT"
    echo "  attach:    scripts/run-nexus-bs-tmux.sh attach"
    echo "  stop:      scripts/run-nexus-bs-tmux.sh stop"
}

cmd_attach() {
    require_command tmux
    session_running || die "tmux session '$SESSION' not running; run start first"
    exec tmux attach -t "$SESSION"
}

cmd_stop() {
    require_command tmux
    if session_running; then
        tmux kill-session -t "$SESSION"
        echo "tmux session '$SESSION' stopped"
    else
        echo "tmux session '$SESSION' not running"
    fi
    rm -f "$FIFO"
}

cmd_status() {
    require_command tmux
    if session_running; then
        tmux list-windows -t "$SESSION"
        echo "fifo: $FIFO"
    else
        echo "tmux session '$SESSION' not running"
    fi
}

usage() {
    cat <<'EOF'
Run the Nexus-BS stack (control, core, dashboard) in a tmux session without systemd.

Usage:
  scripts/run-nexus-bs-tmux.sh start   # create the session (default)
  scripts/run-nexus-bs-tmux.sh attach  # attach to the running session
  scripts/run-nexus-bs-tmux.sh stop    # kill the session
  scripts/run-nexus-bs-tmux.sh status  # show windows/status
  scripts/run-nexus-bs-tmux.sh help    # this help

Environment overrides:
  NEXUS_BS_CONFIG           config file (default: example_config/config.toml)
  NEXUS_BS_DASHBOARD_BIND   dashboard bind (default: 0.0.0.0)
  NEXUS_BS_DASHBOARD_PORT   dashboard port (default: 8080)
  NEXUS_BS_TMUX_SESSION     tmux session name (default: nexus-bs)
EOF
}

case "${1:-start}" in
    start) cmd_start ;;
    attach) cmd_attach ;;
    stop) cmd_stop ;;
    status) cmd_status ;;
    -h|--help|help) usage ;;
    *) usage; exit 1 ;;
esac
