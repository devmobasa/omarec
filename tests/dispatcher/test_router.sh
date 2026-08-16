#!/usr/bin/env bash
# Golden routing cases for the ownership-safe Omarchy dispatcher.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=../harness.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/harness.sh"
BIN="$ROOT/integration/omarchy/bin"
DISPATCHER="$BIN/omarchy-capture-screenrecording"
ADAPTER="$BIN/omarchy-capture-screenrecording-omarec"
IDLE_STATUS='{"response":{"snapshot":{"phase":"idle","session_id":null}}}'
RECORDING_STATUS='{"response":{"snapshot":{"phase":"recording","session_id":"sess-1"}}}'
ACCEPTED_START='{"response":{"session_id":"sess-new"}}'

setup_dispatcher() {
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  runtime="$tmp/run"
  bindir="$tmp/bin"
  log="$tmp/route.log"
  mkdir -p "$runtime" "$bindir"
  write_exe "$bindir/omarchy-capture-screenrecording-omarec" \
    "#!/bin/sh
echo native >>'$log'
exit 0"
  write_exe "$bindir/omarchy-capture-screenrecording-legacy" \
    "#!/bin/sh
echo legacy >>'$log'
exit 0"
  write_exe "$bindir/pgrep" $'#!/bin/sh\nexit 1'
  export XDG_RUNTIME_DIR="$runtime"
  export PATH="$bindir:$BIN:$PATH"
}

run_dispatcher() {
  "$DISPATCHER" "$@"
}

marker() {
  printf '%s' "$runtime/omarec/owner"
}

test_idle_defaults_to_native() {
  setup_dispatcher
  run_dispatcher
  assert_eq "$(cat "$log")" native
  assert_eq "$(head -n1 "$(marker)")" native
}

test_idle_rollout_off_uses_legacy() {
  setup_dispatcher
  OMAREC_SCREENRECORDING=0 run_dispatcher
  assert_eq "$(cat "$log")" legacy
  assert_eq "$(head -n1 "$(marker)")" legacy
}

test_idle_native_rollout() {
  setup_dispatcher
  OMAREC_SCREENRECORDING=1 run_dispatcher
  assert_eq "$(cat "$log")" native
}

test_owned_native_ignores_rollout_off() {
  setup_dispatcher
  mkdir -p "$(dirname "$(marker)")"
  printf 'native\nsession-1\n' >"$(marker)"
  OMAREC_SCREENRECORDING=0 run_dispatcher
  assert_eq "$(cat "$log")" native
}

test_owned_legacy_ignores_rollout_on() {
  setup_dispatcher
  mkdir -p "$(dirname "$(marker)")"
  printf 'legacy\n' >"$(marker)"
  OMAREC_SCREENRECORDING=1 run_dispatcher
  assert_eq "$(cat "$log")" legacy
}

test_stale_symlink_marker_is_refused() {
  setup_dispatcher
  mkdir -p "$(dirname "$(marker)")"
  ln -s /tmp/omarec-owner "$(marker)"
  set +e
  stderr="$(run_dispatcher 2>&1)"
  status=$?
  set -e
  assert_eq "$status" 3
  assert_contains "$stderr" symlink
}

test_contradictory_marker_is_refused() {
  setup_dispatcher
  mkdir -p "$(dirname "$(marker)")"
  printf 'both\n' >"$(marker)"
  set +e
  stderr="$(run_dispatcher 2>&1)"
  status=$?
  set -e
  assert_eq "$status" 3
  assert_contains "$stderr" contradictory
}

test_native_preflight_failure_falls_back_when_provisional() {
  setup_dispatcher
  write_exe "$bindir/omarchy-capture-screenrecording-omarec" $'#!/bin/sh\nexit 1'
  set +e
  stderr="$(run_dispatcher 2>&1)"
  status=$?
  set -e
  assert_eq "$status" 0 "$stderr"
  assert_eq "$(cat "$log")" legacy
  assert_contains "$stderr" preflight
}

test_dispatcher_does_not_pkill() {
  awk 'BEGIN { found = 0 } /pkill/ { found = 1 } END { exit found }' "$DISPATCHER"
  awk 'BEGIN { found = 0 } /pkill/ || /pgrep -f gpu-screen-recorder/ { found = 1 } END { exit found }' \
    "$BIN/omarchy-capture-screenrecording-omarec"
}

setup_adapter() {
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  runtime="$tmp/run"
  bindir="$tmp/bin"
  videos="$tmp/videos"
  log="$tmp/calls.log"
  status_file="$tmp/status.json"
  start_file="$tmp/start.json"
  mkdir -p "$runtime" "$bindir"
  printf '%s\n' "$ACCEPTED_START" >"$start_file"
  write_exe "$bindir/omarec" \
    "#!/bin/bash
echo \"omarec \$*\" >>'$log'
for arg in \"\$@\"; do
  case \"\$arg\" in
    status) cat '$status_file'; exit 0 ;;
    stop) exit 0 ;;
    start) cat '$start_file'; exit 0 ;;
  esac
done
exit 0"
  write_picker '10,20 640x480'
  write_exe "$bindir/omarchy-shell" \
    "#!/bin/bash
echo \"omarchy-shell \$*\" >>'$log'"
  write_exe "$bindir/pgrep" $'#!/bin/sh\nexit 1'
  export XDG_RUNTIME_DIR="$runtime"
  export OMARCHY_SCREENRECORD_DIR="$videos"
  export PATH="$bindir:$BIN:$PATH"
}

write_picker() {
  local output="$1" exit_code="${2:-0}"
  write_exe "$bindir/omarchy-capture-region" \
    "#!/bin/bash
echo \"picker \$*\" >>'$log'
printf '%s\\n' '$output'
exit $exit_code"
}

run_adapter() {
  "$ADAPTER" "$@"
}

adapter_marker() {
  mkdir -p "$runtime/omarec"
  printf '%s' "$runtime/omarec/owner"
}

calls() {
  if [[ -f $log ]]; then
    cat "$log"
  fi
}

test_start_request_during_active_session_stops_it() {
  setup_adapter
  printf '%s\n' "$RECORDING_STATUS" >"$status_file"
  marker_path="$(adapter_marker)"
  printf 'native\nsess-1\n' >"$marker_path"
  run_adapter
  assert_contains "$(calls)" 'omarec stop --session sess-1'
  assert_not_contains "$(calls)" picker
  [[ ! -e $marker_path ]]
  assert_contains "$(calls)" 'omarchy-shell -q omarchy.indicators refresh'
}

test_stop_when_idle_cleans_marker_without_daemon_stop() {
  setup_adapter
  printf '%s\n' "$IDLE_STATUS" >"$status_file"
  marker_path="$(adapter_marker)"
  printf 'native\nsess-old\n' >"$marker_path"
  run_adapter --stop
  assert_not_contains "$(calls)" 'omarec stop'
  [[ ! -e $marker_path ]]
}

test_idle_region_selection_maps_slurp_geometry() {
  setup_adapter
  printf '%s\n' "$IDLE_STATUS" >"$status_file"
  run_adapter
  assert_contains "$(calls)" '--region 640x480+10+20 --coordinate-space logical'
  assert_eq "$(cat "$(adapter_marker)")" $'native\nsess-new'
  assert_contains "$(calls)" 'omarchy-shell -q omarchy.indicators refresh'
}

test_idle_monitor_match_maps_to_monitor_argument() {
  setup_adapter
  printf '%s\n' "$IDLE_STATUS" >"$status_file"
  write_picker 'monitor:DP-1'
  run_adapter
  assert_contains "$(calls)" '--monitor DP-1'
  assert_not_contains "$(calls)" '--region'
}

test_negative_offsets_survive_geometry_mapping() {
  setup_adapter
  printf '%s\n' "$IDLE_STATUS" >"$status_file"
  write_picker '-1920,0 1920x1080'
  run_adapter
  assert_contains "$(calls)" '--region 1920x1080+-1920+0'
}

test_unrecognized_selection_is_rejected() {
  setup_adapter
  printf '%s\n' "$IDLE_STATUS" >"$status_file"
  write_picker 'garbage output'
  set +e
  stderr="$(run_adapter 2>&1)"
  status=$?
  set -e
  assert_eq "$status" 1
  assert_contains "$stderr" Unrecognized
}

test_stale_marker_cleared_when_picker_cancelled() {
  setup_adapter
  printf '%s\n' "$IDLE_STATUS" >"$status_file"
  write_picker '' 1
  marker_path="$(adapter_marker)"
  printf 'native\nsess-old\n' >"$marker_path"
  run_adapter
  [[ ! -e $marker_path ]]
}

test_provisional_marker_preserved_when_picker_cancelled() {
  setup_adapter
  printf '%s\n' "$IDLE_STATUS" >"$status_file"
  write_picker '' 1
  marker_path="$(adapter_marker)"
  printf 'native\nprovisional\n' >"$marker_path"
  run_adapter
  assert_eq "$(cat "$marker_path")" $'native\nprovisional'
}

test_unreachable_daemon_is_treated_as_idle() {
  setup_adapter
  write_exe "$bindir/omarec" \
    "#!/bin/bash
echo \"omarec \$*\" >>'$log'
for arg in \"\$@\"; do
  case \"\$arg\" in
    status) echo 'connection refused' >&2; exit 1 ;;
    start) cat '$start_file'; exit 0 ;;
  esac
done
exit 0"
  run_adapter
  assert_contains "$(calls)" '--region 640x480+10+20'
}

run_test idle_defaults_to_native test_idle_defaults_to_native
run_test idle_rollout_off_uses_legacy test_idle_rollout_off_uses_legacy
run_test idle_native_rollout test_idle_native_rollout
run_test owned_native_ignores_rollout_off test_owned_native_ignores_rollout_off
run_test owned_legacy_ignores_rollout_on test_owned_legacy_ignores_rollout_on
run_test stale_symlink_marker_is_refused test_stale_symlink_marker_is_refused
run_test contradictory_marker_is_refused test_contradictory_marker_is_refused
run_test native_preflight_failure_falls_back_when_provisional test_native_preflight_failure_falls_back_when_provisional
run_test dispatcher_does_not_pkill test_dispatcher_does_not_pkill
run_test start_request_during_active_session_stops_it test_start_request_during_active_session_stops_it
run_test stop_when_idle_cleans_marker_without_daemon_stop test_stop_when_idle_cleans_marker_without_daemon_stop
run_test idle_region_selection_maps_slurp_geometry test_idle_region_selection_maps_slurp_geometry
run_test idle_monitor_match_maps_to_monitor_argument test_idle_monitor_match_maps_to_monitor_argument
run_test negative_offsets_survive_geometry_mapping test_negative_offsets_survive_geometry_mapping
run_test unrecognized_selection_is_rejected test_unrecognized_selection_is_rejected
run_test stale_marker_cleared_when_picker_cancelled test_stale_marker_cleared_when_picker_cancelled
run_test provisional_marker_preserved_when_picker_cancelled test_provisional_marker_preserved_when_picker_cancelled
run_test unreachable_daemon_is_treated_as_idle test_unreachable_daemon_is_treated_as_idle

finish_tests 'dispatcher tests'
