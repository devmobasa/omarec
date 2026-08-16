#!/usr/bin/env bash
# Checks that do not require a Rust toolchain or package registry.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
command -v jq >/dev/null 2>&1 || {
  printf 'static-check: jq is required\n' >&2
  exit 1
}
command -v python3 >/dev/null 2>&1 || {
  printf 'static-check: python3 is required\n' >&2
  exit 1
}

errors=()
err() { errors+=("$1"); }

relpath() {
  local path="$1"
  printf '%s' "${path#"$ROOT"/}"
}

find_files() {
  local name="$1"
  find "$ROOT" \( -path "$ROOT/target" -o -path "$ROOT/.git" \) -prune \
    -o -type f -name "$name" -print
}

if [[ ! -f $ROOT/Cargo.lock ]]; then
  err 'Cargo.lock is required because the workspace ships binaries'
fi

forbidden_crates='^(reqwest|hyper|hyper-util|ureq|isahc|awc)$'
dep_keys_parser="$ROOT/scripts/cargo-dep-keys.py"
while IFS= read -r path; do
  keys=""
  if keys="$(python3 "$dep_keys_parser" "$path" 2>/dev/null)"; then
    leaked="$(printf '%s\n' "$keys" | awk -v re="$forbidden_crates" '$0 ~ re' | sort -u | paste -sd, -)"
    if [[ -n $leaked ]]; then
      err "$(relpath "$path"): network telemetry crate not allowed: [$leaked]"
    fi
  else
    msg="$(python3 "$dep_keys_parser" "$path" 2>&1 >/dev/null || true)"
    err "$(relpath "$path"): invalid TOML: ${msg:-parse failed}"
  fi
done < <(find_files Cargo.toml)

while IFS= read -r path; do
  if ! count="$(jq -s 'length' "$path" 2>&1)"; then
    err "$(relpath "$path"): invalid JSON: $count"
  elif [[ $count != 1 ]]; then
    err "$(relpath "$path"): expected exactly one JSON document, found $count"
  fi
done < <(find_files '*.json')

required_provenance='["kind","source","capture_date","command","sanitization"]'
fixture_root="$ROOT/tests/fixtures"

is_data_file() {
  local path="$1" name ext
  [[ -f $path ]] || return 1
  name="$(basename "$path")"
  [[ $name == provenance.json ]] && return 1
  [[ $name == *omarchy-capture-screenrecording ]] && return 0
  if [[ $name == *.* ]]; then
    ext="${name##*.}"
    [[ $ext == txt || $ext == json ]]
  else
    return 0
  fi
}

has_ancestor_provenance() {
  local dir="$1" parent
  parent="$(dirname "$dir")"
  while [[ $parent == "$fixture_root" || $parent == "$fixture_root"/* ]]; do
    [[ -f $parent/provenance.json ]] && return 0
    [[ $parent == "$fixture_root" ]] && break
    parent="$(dirname "$parent")"
  done
  return 1
}

if [[ -d $fixture_root ]]; then
  while IFS= read -r dir; do
    [[ $(basename "$dir") == parity ]] && continue
    has_data=0
    for child in "$dir"/*; do
      [[ -e $child ]] || continue
      if is_data_file "$child"; then
        has_data=1
        break
      fi
    done
    (( has_data == 1 )) || continue
    provenance="$dir/provenance.json"
    if [[ ! -f $provenance ]]; then
      if has_ancestor_provenance "$dir"; then
        continue
      fi
      err "$(relpath "$dir"): missing provenance.json"
      continue
    fi
    if ! count="$(jq -s 'length' "$provenance" 2>&1)"; then
      err "$(relpath "$provenance"): invalid provenance JSON: $count"
      continue
    fi
    if [[ $count != 1 ]]; then
      err "$(relpath "$provenance"): expected exactly one JSON document, found $count"
      continue
    fi
    missing="$(jq -r --argjson req "$required_provenance" '$req - keys | sort | join(", ")' "$provenance")"
    if [[ -n $missing ]]; then
      err "$(relpath "$provenance"): missing fields [$missing]"
    fi
  done < <(find "$fixture_root" -type d)
fi

parity_cases="$ROOT/tests/fixtures/omarchy/parity/cases.json"
if [[ -f $parity_cases ]]; then
  if ! jq -e '.cases | length > 0' "$parity_cases" >/dev/null; then
    err 'tests/fixtures/omarchy/parity/cases.json: no cases'
  fi
fi

while IFS= read -r path; do
  if grep -Fq 'unsafe {' "$path" || grep -Fq 'unsafe fn' "$path"; then
    err "$(relpath "$path"): unsafe Rust is forbidden in the scaffold"
  fi
  if grep -Fq 'Command::new("sh")' "$path" || grep -Fq 'Command::new("bash")' "$path"; then
    err "$(relpath "$path"): shell execution is not allowed in Rust adapters"
  fi
  if grep -Fq '.peer_cred(' "$path"; then
    err "$(relpath "$path"): std UnixStream::peer_cred is nightly; use rustix SO_PEERCRED"
  fi
done < <(find_files '*.rs')

dispatcher="$ROOT/integration/omarchy/bin"
if [[ -d $dispatcher ]]; then
  for path in "$dispatcher"/*; do
    [[ -f $path ]] || continue
    if grep -Fq pkill "$path"; then
      err "$(relpath "$path"): dispatcher must not pkill"
    fi
    if grep -Fq 'pgrep -f gpu-screen-recorder' "$path"; then
      err "$(relpath "$path"): dispatcher must not broadly match GSR processes"
    fi
  done
fi

plugin="$ROOT/integration/quickshell/community.omarec"
service="$plugin/Service.qml"
if [[ ! -f $service ]]; then
  err 'integration/quickshell/community.omarec/Service.qml is required'
else
  watch_count="$( { grep -o '"--json", "watch"' "$service" || true; } | wc -l )"
  if [[ $watch_count -ne 1 ]]; then
    err 'Service.qml must contain exactly one omarec --json watch process'
  fi
  if ! grep -Fq -- '--session' "$service"; then
    err 'Service.qml mutations must include the displayed session id'
  fi
  if ! grep -Fq '.local/bin/omarec' "$service" || ! grep -Fq '"/usr/bin/omarec"' "$service"; then
    err 'Service.qml must prefer ~/.local/bin/omarec then /usr/bin/omarec'
  fi
fi

widget="$plugin/RecordingWidget.qml"
if [[ ! -f $widget ]]; then
  err 'integration/quickshell/community.omarec/RecordingWidget.qml is required'
else
  if ! grep -Fq 'recorder.stale' "$widget"; then
    err 'RecordingWidget.qml must disable actions while the watch snapshot is stale'
  fi
  if ! grep -Fq 'running: root.phase === "recording"' "$widget"; then
    err 'RecordingWidget.qml must not tick elapsed time while paused'
  fi
fi

if (( ${#errors[@]} > 0 )); then
  printf 'static checks failed:\n' >&2
  for error in "${errors[@]}"; do
    printf -- '- %s\n' "$error" >&2
  done
  exit 1
fi
printf 'static checks passed\n'
