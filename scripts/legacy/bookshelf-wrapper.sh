#!/bin/sh
# bookshelf-wrapper.sh — startup hook for the pbemu custom bookshelf.
#
# Deployed to /mnt/ext1/system/bin/bookshelf.app on the device.
# monitor.app checks this path BEFORE /ebrmain/bin/bookshelf.app
# (verified in the launcher's disassembly at 0x33b48–0x33b74), so
# this script runs in place of the firmware bookshelf on every boot.
#
# What it does:
#   1. Launches the custom bookshelf (/mnt/ext1/applications/books.app)
#      in the background, fire-and-forget.
#   2. Execs the REAL firmware bookshelf (/ebrmain/bin/bookshelf.app)
#      with the original argv, so the system stays fully functional.
#
# The custom bookshelf runs as a separate task alongside the stock UI.
# If it crashes, nothing breaks — the stock bookshelf is unaffected.
#
# Idempotency: uses a PID file to avoid spawning duplicate copies.
# The PID file is cleaned up on next launch if the process is gone.

CUSTOM_APP="/mnt/ext1/applications/books.app"
PID_FILE="/tmp/pbemu_bookshelf.pid"
LOG="/mnt/ext1/applications/bookshelf-wrapper.log"

# Log startup (append, keep it short).
{
    echo "==== wrapper $(date '+%Y-%m-%d %H:%M:%S') argv: $* ===="
} >> "$LOG" 2>/dev/null

# Launch the custom bookshelf if it exists and isn't already running.
if [ -x "$CUSTOM_APP" ]; then
    # Clean up stale PID file.
    if [ -f "$PID_FILE" ]; then
        OLD_PID=$(cat "$PID_FILE" 2>/dev/null)
        if [ -n "$OLD_PID" ] && ! kill -0 "$OLD_PID" 2>/dev/null; then
            rm -f "$PID_FILE"
        fi
    fi

    # Spawn if not already running.
    if [ ! -f "$PID_FILE" ] || ! kill -0 "$(cat "$PID_FILE" 2>/dev/null)" 2>/dev/null; then
        "$CUSTOM_APP" >> "$LOG" 2>&1 &
        echo $! > "$PID_FILE"
        echo "launched custom bookshelf (pid $!)" >> "$LOG" 2>/dev/null
    else
        echo "custom bookshelf already running (pid $(cat "$PID_FILE"))" >> "$LOG" 2>/dev/null
    fi
else
    echo "WARN: $CUSTOM_APP not found or not executable" >> "$LOG" 2>/dev/null
fi

# Exec the real firmware bookshelf — same argv, same env.
# monitor.app waits for this to be ready before sending EVT_INIT,
# so we must not block.  exec replaces this shell process entirely.
exec /ebrmain/bin/bookshelf.app "$@"
