#!/usr/bin/env bash
# Consume omarec watch NDJSON the same way the Quattro service reducer does.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=../harness.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/harness.sh"
STREAM="$ROOT/tests/fixtures/quattro/watch-stream.ndjson"

command -v jq >/dev/null 2>&1 || {
  printf 'test_watch_consumer: jq is required\n' >&2
  exit 1
}

consume() {
  local state="$1" line="$2"
  jq -c --argjson envelope "$line" '
    . as $state
    | if $envelope.protocol != 1 then
        $state + {protocol_error: $envelope.protocol}
      else
        ($envelope.daemon_lifetime_id // "") as $lifetime
        | (if $lifetime != "" and $lifetime != ($state.daemon_lifetime_id // "") then
            $state + {daemon_lifetime_id: $lifetime, stale: false, watermark: 0}
          else $state end) as $s
        | ($envelope.event // {}) as $event
        | ($event.type) as $kind
        | if $kind == "lag" then
            $s + {stale: true}
          elif $kind == "snapshot" then
            $s
            + {
                watermark: (if $event.watermark then $event.watermark else $s.watermark end),
                snapshot: ($event.snapshot // {}),
                stale: false
              }
          elif (($envelope.sequence // 0) != 0)
            and (($envelope.sequence // 0) <= ($s.watermark // 0)) then
            $s
          elif $kind == "state_changed" or $kind == "snapshot" then
            $s + {snapshot: ($event.snapshot // {})}
          else
            $s
          end
      end
  ' <<<"$state"
}

test_lag_then_lifetime_snapshot_clears_stale() {
  local state='{"snapshot":{"phase":"idle"},"stale":false,"daemon_lifetime_id":"","watermark":0}'
  while IFS= read -r line; do
    [[ -n $line ]] || continue
    state="$(consume "$state" "$line")"
  done <"$STREAM"
  assert_eq "$(jq -r '.stale' <<<"$state")" false
  assert_eq "$(jq -r '.snapshot.phase' <<<"$state")" recording
  assert_eq "$(jq -r '.daemon_lifetime_id' <<<"$state")" \
    '22222222-2222-2222-2222-222222222222'
  assert_eq "$(jq -r '.watermark' <<<"$state")" 8
}

test_unknown_protocol_is_rejected() {
  local state='{"daemon_lifetime_id":"","watermark":0,"stale":false,"snapshot":{}}'
  state="$(consume "$state" '{"protocol":2,"event":{"type":"snapshot"}}')"
  assert_eq "$(jq -r '.protocol_error' <<<"$state")" 2
}

run_test lag_then_lifetime_snapshot_clears_stale test_lag_then_lifetime_snapshot_clears_stale
run_test unknown_protocol_is_rejected test_unknown_protocol_is_rejected

finish_tests 'watch consumer tests'
