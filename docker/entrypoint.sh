#!/bin/sh
set -eu
command="${1:-dashboard}"
if [ "$#" -gt 0 ]; then
  shift
fi
case "$command" in
  dashboard|gateway)
    exec "/app/bin/$command" "$@"
    ;;
  mnt)
    exec /app/bin/webgateway-mnt "$@"
    ;;
  *)
    exec "$command" "$@"
    ;;
esac
