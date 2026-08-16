#!/usr/bin/env bash
# Record one packaged-beta hardware row. Does not claim a pass by itself.
set -euo pipefail

row_id="${1:-}"
if [[ -z $row_id ]]; then
  printf 'usage: %s <row-id> [output.json]\n' "$0" >&2
  exit 2
fi

command -v jq >/dev/null 2>&1 || {
  printf 'hardware-qualify: jq is required\n' >&2
  exit 1
}

state_home="${XDG_STATE_HOME:-$HOME/.local/state}"
output="${2:-$state_home/omarec/hardware/${row_id}.json}"
mkdir -p "$(dirname "$output")"

capture() {
  local cmd="$1"
  shift
  if ! command -v "$cmd" >/dev/null 2>&1; then
    printf '%s: missing' "$cmd"
    return
  fi
  local out=""
  if command -v timeout >/dev/null 2>&1; then
    out="$(timeout 8 "$cmd" "$@" 2>&1 || true)"
  else
    out="$("$cmd" "$@" 2>&1 || true)"
  fi
  if [[ -z ${out//[$'\t\n\r ']/} ]]; then
    printf '%s: empty' "$cmd"
    return
  fi
  printf '%s' "$out" | awk 'NR <= 8 { if (NR > 1) printf " "; printf "%s", $0 }'
}

jq -n \
  --arg row_id "$row_id" \
  --arg captured_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg kernel "$(uname -r)" \
  --arg n0 "$(capture uname -a)" \
  --arg n1 "$(capture gpu-screen-recorder --version)" \
  --arg n2 "$(capture hyprctl -j monitors)" \
  --arg n3 "$(capture omarec doctor)" \
  '{
    row_id: $row_id,
    captured_at: $captured_at,
    kernel: $kernel,
    notes: [$n0, $n1, $n2, $n3],
    status: "untested",
    package: "omarec"
  }' >"$output"

printf 'wrote %s\n' "$output"
