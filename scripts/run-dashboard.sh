#!/usr/bin/env bash
# Start the dashboard with local DNS/ACME credentials sourced from
# .secrets/dnsprovider (gitignored). Requires DATABASE_URL (or config.toml).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

# shellcheck source=scripts/source-dnsprovider.sh
. "$root/scripts/source-dnsprovider.sh"

exec "${DASHBOARD_BIN:-./target/debug/dashboard}" "$@"
