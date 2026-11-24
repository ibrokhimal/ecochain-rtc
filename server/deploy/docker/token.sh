#!/usr/bin/env bash
# Opens ROOM (if it is not running) and prints a room token for IDENTITY.
# Usage: token.sh ROOM IDENTITY [participant|host] [TTL_SECS]
# An empty room closes after rooms.empty_room_ttl_secs: join right after.
set -euo pipefail
cd "$(dirname "$0")"
[ $# -ge 2 ] || { sed -n 2,4p "$0" >&2; exit 2; }
room=$1 id=$2 role=${3:-participant} ttl=${4:-86400}
for v in "$room" "$id" "$role" "$ttl"; do
  [[ $v =~ ^[A-Za-z0-9_-]+$ ]] || { echo "invalid argument: $v" >&2; exit 2; }
done
set -a; . ./.env; set +a
base=${ECOCHAIN_RTC_BASE:-https://rtc.tauapp.uz:7443}
# The admin key goes through a config on fd 3, not the process list.
api() {
  curl -sS -m 15 -K /dev/fd/3 -H 'content-type: application/json' "$@" \
    3< <(printf 'header = "Authorization: Bearer %s"\n' "$ECOCHAIN_RTC_ADMIN_KEY")
}
code=$(api -o /dev/null -w '%{http_code}' -d "{\"room_id\":\"$room\"}" "$base/v1/rooms")
case $code in 201|409) ;; *) echo "room: HTTP $code" >&2; exit 1 ;; esac
api -f -d "{\"room\":\"$room\",\"identity\":\"$id\",\"name\":\"$id\",\"role\":\"$role\",\"ttl_secs\":$ttl}" \
  "$base/v1/tokens" | sed -E 's/.*"token":"([^"]+)".*/\1/' | tr -d '\n'
