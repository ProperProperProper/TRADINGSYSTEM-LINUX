#!/bin/sh
# The open monitor (owner 2026-10-21): launchd (macOS) or a systemd user timer (Linux) starts this every 4 minutes; it acts only in the window around the US open (weekdays 08:50-10:30 New York)
# and runs ONE headless Claude session (the model and effort the project pins in crates/cli/src/review.rs) that checks the competition bot, fixes errors and learns.
# OPEN_MONITOR_FORCE=1 skips the window (a manual test). Output: data/open_monitor.log.
# Usage: ops/open_monitor.sh [install|remove|status]
LABEL="com.thomasai.openmonitor"
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT" || exit 1

case "${1:-}" in
  install)
    mkdir -p data
    if [ "$(uname -s)" = "Linux" ]; then
      DIR="$HOME/.config/systemd/user"
      mkdir -p "$DIR"
      cat > "$DIR/$LABEL.service" <<EOF
[Unit]
Description=TradingSystem $LABEL

[Service]
Type=oneshot
WorkingDirectory=$ROOT
ExecStart="$ROOT/ops/open_monitor.sh"
Nice=19
IOSchedulingClass=idle
Environment=PATH=%h/.local/bin:/usr/local/bin:/usr/bin:/bin
StandardOutput=append:$ROOT/data/open_monitor.log
StandardError=append:$ROOT/data/open_monitor.log
EOF
      cat > "$DIR/$LABEL.timer" <<EOF
[Unit]
Description=TradingSystem $LABEL timer

[Timer]
OnActiveSec=240s
OnUnitInactiveSec=240s
AccuracySec=1s

[Install]
WantedBy=timers.target
EOF
      systemctl --user daemon-reload && systemctl --user enable --now "$LABEL.timer"
    else
      DIR="$HOME/Library/LaunchAgents"
      mkdir -p "$DIR"
      launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
      cat > "$DIR/$LABEL.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>$LABEL</string>
<key>ProgramArguments</key><array><string>$ROOT/ops/open_monitor.sh</string></array>
<key>WorkingDirectory</key><string>$ROOT</string>
<key>StartInterval</key><integer>240</integer>
<key>RunAtLoad</key><false/>
<key>Nice</key><integer>19</integer>
<key>ProcessType</key><string>Background</string>
<key>LowPriorityIO</key><true/>
<key>StandardOutPath</key><string>$ROOT/data/open_monitor.log</string>
<key>StandardErrorPath</key><string>$ROOT/data/open_monitor.log</string>
</dict></plist>
EOF
      launchctl bootstrap "gui/$(id -u)" "$DIR/$LABEL.plist"
    fi
    echo "installed $LABEL"
    exit 0
    ;;
  remove)
    if [ "$(uname -s)" = "Linux" ]; then
      systemctl --user disable --now "$LABEL.timer" "$LABEL.service" 2>/dev/null || true
      rm -f "$HOME/.config/systemd/user/$LABEL.timer" "$HOME/.config/systemd/user/$LABEL.service"
      systemctl --user daemon-reload 2>/dev/null || true
    else
      launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
      rm -f "$HOME/Library/LaunchAgents/$LABEL.plist"
    fi
    echo "removed $LABEL"
    exit 0
    ;;
  status)
    if [ "$(uname -s)" = "Linux" ]; then
      systemctl --user is-enabled "$LABEL.timer"
    else
      launchctl print "gui/$(id -u)/$LABEL" >/dev/null 2>&1 && echo "enabled" || { echo "disabled"; exit 1; }
    fi
    exit 0
    ;;
esac

if [ -z "$OPEN_MONITOR_FORCE" ]; then
  dow=$(TZ=America/New_York date +%u)
  hm=$(TZ=America/New_York date +%H%M)
  [ "$dow" -le 5 ] || exit 0
  [ "$hm" -ge 850 ] && [ "$hm" -le 1030 ] || exit 0
fi
LOCK=data/.open_monitor.lock
if ! mkdir "$LOCK" 2>/dev/null; then
  # a session still running; a lock older than 25 minutes is stale
  if [ -n "$(find "$LOCK" -maxdepth 0 -mmin +25 2>/dev/null)" ]; then rmdir "$LOCK"; mkdir "$LOCK" || exit 0; else exit 0; fi
fi
trap 'rmdir "$LOCK" 2>/dev/null' EXIT
CLAUDE=$(command -v claude || echo "$HOME/.local/bin/claude")
[ -x "$CLAUDE" ] || { echo "$(date -u +%FT%TZ) claude not found" >> data/open_monitor.log; exit 1; }
echo "=== $(date -u +%FT%TZ) open monitor run" >> data/open_monitor.log
P="$ROOT/papercompmoomoo"
"$CLAUDE" -p "$(cat ops/open_monitor_prompt.md)" \
  --permission-mode bypassPermissions \
  --model claude-sonnet-5-5 --effort low --max-budget-usd 3.00 --no-session-persistence --output-format text >> data/open_monitor.log 2>&1
echo "=== $(date -u +%FT%TZ) done (exit $?)" >> data/open_monitor.log
