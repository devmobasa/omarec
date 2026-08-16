#!/usr/bin/env bash
# Behavioral tests for the omarec-notify watch consumer.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=../harness.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/harness.sh"
NOTIFIER="$ROOT/integration/omarchy/bin/omarec-notify"

link_tool() {
  local name="$1" target
  target="$(command -v "$name")"
  [[ -n $target ]] || {
    printf '%s is required to run these tests\n' "$name" >&2
    exit 1
  }
  ln -s "$target" "$sysdir/$name"
}

event_line() {
  jq -c -n --argjson sequence "$1" --argjson event "$2" \
    '{protocol: 1, sequence: $sequence, event: $event}'
}

setup_notifier() {
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  runtime="$tmp/run"
  bindir="$tmp/bin"
  sysdir="$tmp/sys"
  log="$tmp/calls.log"
  stream="$tmp/watch.ndjson"
  recording="$tmp/videos/rec.mp4"
  mkdir -p "$runtime" "$bindir" "$sysdir" "$(dirname "$recording")"
  printf 'not a real video' >"$recording"
  write_exe "$bindir/omarec" "#!/bin/bash
cat '$stream'"
  write_exe "$bindir/omarchy-notification-send" \
    "#!/bin/bash
echo \"notify \$*\" >>'$log'"
  write_exe "$bindir/ffmpeg" \
    "#!/bin/bash
echo \"ffmpeg \$*\" >>'$log'
for last in \"\$@\"; do :; done
printf thumb >\"\$last\""
  write_exe "$bindir/omarchy-shell" \
    "#!/bin/bash
echo \"omarchy-shell \$*\" >>'$log'"
  for name in env bash jq basename cat mkdir chmod rm sleep timeout; do
    link_tool "$name"
  done
  export XDG_RUNTIME_DIR="$runtime"
  export OMAREC_NOTIFY_ONCE=1
  export PATH="$bindir:$sysdir"
}

run_notifier() {
  local status=0
  timeout 30 "$NOTIFIER" || status=$?
  if (( status == 124 )); then
    printf 'notifier timed out after 30s\n' >&2
    exit 1
  fi
  return "$status"
}

notifications() {
  if [[ ! -f $log ]]; then
    return 0
  fi
  while IFS= read -r line; do
    [[ $line == notify\ * ]] && printf '%s\n' "$line"
  done <"$log"
}

test_file_saved_notifies_once_with_thumbnail_and_open_action() {
  setup_notifier
  saved="$(jq -nc --arg output "$recording" \
    '{type:"file_saved",session_id:"sess-1",output:$output}')"
  {
    event_line 1 '{"type":"heartbeat"}'
    event_line 2 "$saved"
    event_line 3 "$saved"
  } >"$stream"
  run_notifier
  mapfile -t notes < <(notifications)
  assert_eq "${#notes[@]}" 1 "${notes[*]-}"
  assert_contains "${notes[0]}" 'Screen recording saved'
  assert_contains "${notes[0]}" rec.mp4
  thumbnail="$runtime/omarec/thumbnails/rec.png"
  assert_contains "${notes[0]}" "--image $thumbnail"
  [[ -f $thumbnail ]]
  assert_contains "${notes[0]}" "--exec xdg-open $recording"
  assert_contains "$(cat "$log")" 'omarchy-shell -q omarchy.indicators refresh'
}

test_error_event_sends_critical_notification() {
  setup_notifier
  event_line 1 '{"type":"error","session_id":"sess-1","code":"gsr_exit","message":"recorder exited unexpectedly"}' \
    >"$stream"
  run_notifier
  mapfile -t notes < <(notifications)
  assert_eq "${#notes[@]}" 1 "${notes[*]-}"
  assert_contains "${notes[0]}" '-u critical'
  assert_contains "${notes[0]}" 'Screen recording error'
  assert_contains "${notes[0]}" 'recorder exited unexpectedly'
}

test_thumbnail_failure_still_notifies_without_image() {
  setup_notifier
  write_exe "$bindir/ffmpeg" $'#!/bin/bash\nexit 1'
  saved="$(jq -nc --arg output "$recording" \
    '{type:"file_saved",session_id:"sess-1",output:$output}')"
  event_line 1 "$saved" >"$stream"
  run_notifier
  mapfile -t notes < <(notifications)
  assert_eq "${#notes[@]}" 1 "${notes[*]-}"
  assert_not_contains "${notes[0]}" '--image'
  assert_contains "${notes[0]}" 'Screen recording saved'
}

test_falls_back_to_notify_send() {
  setup_notifier
  rm -f "$bindir/omarchy-notification-send"
  write_exe "$bindir/notify-send" \
    "#!/bin/bash
echo \"notify \$*\" >>'$log'"
  saved="$(jq -nc --arg output "$recording" \
    '{type:"file_saved",session_id:"sess-1",output:$output}')"
  event_line 1 "$saved" >"$stream"
  run_notifier
  mapfile -t notes < <(notifications)
  assert_eq "${#notes[@]}" 1 "${notes[*]-}"
  assert_contains "${notes[0]}" '-a omarec'
  assert_contains "${notes[0]}" '-i '
}

run_test file_saved_notifies_once_with_thumbnail_and_open_action \
  test_file_saved_notifies_once_with_thumbnail_and_open_action
run_test error_event_sends_critical_notification \
  test_error_event_sends_critical_notification
run_test thumbnail_failure_still_notifies_without_image \
  test_thumbnail_failure_still_notifies_without_image
run_test falls_back_to_notify_send test_falls_back_to_notify_send

finish_tests 'notify tests'
