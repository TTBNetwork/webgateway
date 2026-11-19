#!/usr/bin/env bash
# Nezha gRPC probe: connect a real Nezha agent through the gateway and verify
# that the dashboard accepts it over a long-lived gRPC stream.
#
# The gateway must already have a website whose host matches NEZHA_SERVER
# (the gRPC :authority / Host header) and whose backend points at the Nezha
# dashboard HTTP port (the dashboard serves the NezhaService gRPC API on the
# same port via h2c).
#
# Required environment:
#   NEZHA_AGENT          path to the nezha-agent binary
#   NEZHA_DASHBOARD_DB   path to the dashboard SQLite DB (data/sqlite.db)
#   NEZHA_SERVER         gateway host:port the agent should dial
#   NEZHA_SECRET         Nezha agent secret (admin user's agent_secret)
# Optional:
#   NEZHA_UUID           agent UUID (default: random)
#   NEZHA_TLS            true/false (default false)
#   NEZHA_INSECURE_TLS   true/false (default true)
#   HOLD_SECONDS         how long to keep the agent connected (default 30)
set -euo pipefail

: "${NEZHA_AGENT:?path to the nezha-agent binary}"
: "${NEZHA_DASHBOARD_DB:?path to the Nezha dashboard data/sqlite.db}"
: "${NEZHA_SERVER:?gateway host:port for the agent}"
: "${NEZHA_SECRET:?Nezha agent secret}"

NEZHA_UUID="${NEZHA_UUID:-$(python3 -c 'import uuid; print(uuid.uuid4())')}"
NEZHA_TLS="${NEZHA_TLS:-false}"
NEZHA_INSECURE_TLS="${NEZHA_INSECURE_TLS:-true}"
HOLD_SECONDS="${HOLD_SECONDS:-30}"

workdir="$(mktemp -d)"
config="$workdir/agent.yaml"
log="$workdir/agent.log"

cat >"$config" <<EOF
debug: true
server: $NEZHA_SERVER
client_secret: $NEZHA_SECRET
uuid: $NEZHA_UUID
report_delay: 1
disable_auto_update: true
disable_force_update: true
disable_command_execute: true
tls: $NEZHA_TLS
insecure_tls: $NEZHA_INSECURE_TLS
EOF

"$NEZHA_AGENT" -c "$config" >"$log" 2>&1 &
agent_pid=$!
cleanup() {
  kill "$agent_pid" 2>/dev/null || true
  wait "$agent_pid" 2>/dev/null || true
  rm -rf "$workdir"
}
trap cleanup EXIT

session_state() {
  python3 - "$NEZHA_DASHBOARD_DB" "$NEZHA_UUID" <<'PY'
import sqlite3
import sys

database, uuid = sys.argv[1], sys.argv[2]
connection = sqlite3.connect(database)
row = connection.execute(
    "SELECT s.id, s.name, o.ongoing FROM servers s "
    "LEFT JOIN online_sessions o ON o.server_id = s.id "
    "WHERE s.uuid = ? ORDER BY o.id DESC LIMIT 1",
    (uuid,),
).fetchone()
if row is None:
    print("pending")
else:
    print(f"{row[0]} {row[1]} {row[2]}")
PY
}

for _ in $(seq 1 30); do
  state="$(session_state)"
  if [[ "$state" == "pending" ]]; then
    sleep 1
    continue
  fi
  read -r server_id server_name ongoing <<<"$state"
  if [[ "$ongoing" == "1" ]]; then
    echo "[nezha-probe] server $server_id ($server_name) is connected; holding ${HOLD_SECONDS}s"
    sleep "$HOLD_SECONDS"
    state="$(session_state)"
    read -r _ _ ongoing <<<"$state"
    if [[ "$ongoing" != "1" ]]; then
      echo "[nezha-probe] FAIL session ended before the hold window elapsed"
      grep -iE 'rpc error|connection (closed|failed)|failed|error' "$log" || true
      exit 1
    fi
    if grep -qiE 'rpc error|connection (closed|failed)|failed to (connect|report)' "$log"; then
      echo "[nezha-probe] FAIL agent reported gRPC errors"
      grep -iE 'rpc error|connection (closed|failed)|failed to (connect|report)' "$log" || true
      exit 1
    fi
    echo "[nezha-probe] PASS server $server_id stayed connected for ${HOLD_SECONDS}s"
    exit 0
  fi
  sleep 1
done

echo "[nezha-probe] FAIL the agent did not register with the dashboard"
printf '%s\n' '--- agent log ---'
cat "$log"
exit 1
