#!/usr/bin/env bash
# Validate the M8 hardware qualification matrix schema.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
path="$ROOT/tests/fixtures/hardware/matrix.json"

usage() {
  printf 'usage: %s [--check] [--path FILE]\n' "$0" >&2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --check) shift ;;
    --path)
      path="${2:?--path requires a file}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'unknown argument: %s\n' "$1" >&2
      usage
      exit 2
      ;;
  esac
done

command -v jq >/dev/null 2>&1 || {
  printf 'hardware-matrix: jq is required\n' >&2
  exit 1
}

if [[ ! -f $path ]]; then
  printf 'hardware matrix is invalid:\n- missing file %s\n' "$path" >&2
  exit 1
fi
if [[ ! -s $path ]]; then
  printf 'hardware matrix is invalid:\n- expected exactly one JSON document, found 0\n' >&2
  exit 1
fi

if ! matrix="$(jq -ce -s 'if length == 1 then .[0] else error("expected exactly one JSON document") end' "$path" 2>&1)"; then
  printf 'hardware matrix is invalid:\n- %s\n' "$matrix" >&2
  exit 1
fi

required_ids='[
  "amd-single-gpu",
  "intel-recent",
  "intel-legacy",
  "nvidia-proprietary",
  "intel-igpu-nvidia-dgpu",
  "amd-igpu-nvidia-dgpu",
  "rotated-secondary",
  "fractional-scale-multi-monitor",
  "external-display-other-gpu"
]'
required_fields='[
  "id",
  "topology",
  "status",
  "direct_monitor",
  "region",
  "portal",
  "audio",
  "camera",
  "pause",
  "restart_recovery",
  "evidence"
]'

errors="$(printf '%s\n' "$matrix" | jq -r --argjson required_ids "$required_ids" --argjson required_fields "$required_fields" '
  def statuses: ["untested", "passed", "failed", "unsupported"];
  . as $m
  | [
      (if $m.schema_version != 1 then "schema_version must be 1" else empty end),
      (if $m.gsr_floor != "6.0.0" then "gsr_floor must be 6.0.0" else empty end),
      (if ($m.rows | type != "array" or length == 0) then "rows must be a non-empty list" else empty end)
    ]
  + (
      if ($m.rows | type != "array" or ($m.rows | length) == 0) then []
      else
        (
          $m.rows
          | to_entries
          | map(
              .value as $r
              | if ($r | type != "object") then ["each row must be an object"]
                else
                  ($required_fields - ($r | keys)) as $missing
                  | [
                      (if $missing != [] then "\($r.id // "<unknown>"): missing \($missing | sort)" else empty end),
                      (if ($r.status | IN(statuses[])) | not then "\($r.id): invalid status \($r.status)" else empty end),
                      (if ($r.id | type != "string" or $r.id == "") then "row id is required" else empty end),
                      (if $r.status == "passed" and (
                          $r.evidence == null
                          or $r.evidence == false
                          or $r.evidence == 0
                          or $r.evidence == ""
                          or $r.evidence == []
                          or $r.evidence == {}
                        ) then "\($r.id): passed rows must point at evidence" else empty end)
                    ]
                end
            )
          | add // []
        )
        + (
            $m.rows
            | map(select(.id | type == "string" and . != "") | .id)
            | group_by(.)
            | map(select(length > 1) | "duplicate row id \(.[0])")
          )
      end
    )
  + (
      if ($m.rows | type != "array" or ($m.rows | length) == 0) then []
      else
        ($required_ids - ($m.rows | map(.id))) as $missing_ids
        | if $missing_ids != [] then ["missing required topologies: \($missing_ids | sort)"] else [] end
      end
    )
  | .[]
')"

if [[ -n $errors ]]; then
  printf 'hardware matrix is invalid:\n' >&2
  while IFS= read -r error; do
    printf -- '- %s\n' "$error" >&2
  done <<<"$errors"
  exit 1
fi

rows="$(jq '.rows | length' <<<"$matrix")"
printf 'hardware matrix ok (%s rows)\n' "$rows"
