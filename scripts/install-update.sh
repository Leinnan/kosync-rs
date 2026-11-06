#!/usr/bin/env bash
# Runs on the server, started by `just deploy`: installs a new binary for the
# kosync systemd service, restarts it and waits until it is ready.
set -euo pipefail

new_binary="$1"
service=kosync

sudo=sudo
if [ "$(id -u)" -eq 0 ]; then
  sudo=
fi

# ExecStart is shown as `{ path=/usr/local/bin/kosync_rs ; argv[]=... }`.
exec_start="$($sudo systemctl show --value -p ExecStart "$service")"
installed="$(printf '%s\n' "$exec_start" | sed -n 's/^{ path=\([^ ;]*\).*/\1/p')"
if [ -z "$installed" ]; then
  echo "The $service service is not installed on this host: see README.md" >&2
  exit 1
fi
command -v curl >/dev/null || { echo "curl is required on the server to check readiness" >&2; exit 1; }

# EnvironmentFiles is shown as `/etc/kosync/kosync.env (ignore_errors=no)`.
env_file="$($sudo systemctl show --value -p EnvironmentFiles "$service" | sed 's/ (ignore_errors=.*//')"
port=
if [ -n "$env_file" ]; then
  port="$($sudo sed -n 's/^[[:space:]]*KOSYNC_RS_PORT=//p' "$env_file" | tail -n 1 | tr -d "\"' \r")"
fi
port="${port:-8090}"

previous=
if [ -e "$installed" ]; then
  previous="$installed.previous"
  $sudo cp -p "$installed" "$previous"
fi
# install, not mv: the new file takes its SELinux context from the target
# directory. The final mv swaps the binary atomically.
$sudo install -o root -g root -m 0755 "$new_binary" "$installed.new"
$sudo mv -f "$installed.new" "$installed"

echo "Restarting $service"
restarted=true
$sudo systemctl restart "$service" || restarted=false

if [ "$restarted" = true ]; then
  for _ in $(seq 1 30); do
    if curl -fsS --max-time 3 "http://127.0.0.1:$port/healthcheck" >/dev/null 2>&1; then
      echo "$installed updated and ready on port $port"
      exit 0
    fi
    sleep 1
  done
fi

echo "$service did not become ready on port $port" >&2
$sudo journalctl -u "$service" -n 50 --no-pager >&2 || true
if [ -n "$previous" ]; then
  # Not done automatically: migrations run at startup, and the previous binary
  # may refuse a database that a newer migration has already been applied to.
  echo "To go back to the previous binary, if no new migration has been applied:" >&2
  echo "  sudo install -o root -g root -m 0755 $previous $installed && sudo systemctl restart $service" >&2
fi
exit 1
