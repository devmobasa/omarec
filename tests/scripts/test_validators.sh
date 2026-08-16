#!/usr/bin/env bash
# Regression coverage for the shell validators and test harness.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=../harness.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/harness.sh"
MATRIX="$ROOT/scripts/hardware-matrix.sh"
DEP_KEYS="$ROOT/scripts/cargo-dep-keys.py"
FIXTURE="$ROOT/tests/fixtures/hardware/matrix.json"

probe_mid_fail() {
  false
  true
}

out_file="$(mktemp)"
run_test probe_mid_fail probe_mid_fail >"$out_file" 2>&1
if (( failed != 1 )); then
  printf 'harness did not count a mid-function false as failure\n' >&2
  cat "$out_file" >&2
  rm -f "$out_file"
  exit 1
fi
assert_contains "$(cat "$out_file")" 'not ok probe_mid_fail'
failed=0
rm -f "$out_file"
printf 'ok harness_rejects_mid_function_failure\n'

test_empty_matrix_is_rejected() {
  local tmp
  tmp="$(mktemp)"
  : >"$tmp"
  set +e
  out="$("$MATRIX" --check --path "$tmp" 2>&1)"
  status=$?
  set -e
  rm -f "$tmp"
  assert_eq "$status" 1
  assert_contains "$out" 'exactly one JSON document'
}

test_concatenated_matrices_are_rejected() {
  local tmp
  tmp="$(mktemp)"
  cat "$FIXTURE" "$FIXTURE" >"$tmp"
  set +e
  out="$("$MATRIX" --check --path "$tmp" 2>&1)"
  status=$?
  set -e
  rm -f "$tmp"
  assert_eq "$status" 1
  assert_contains "$out" 'exactly one JSON document'
}

test_passed_row_requires_nonempty_evidence() {
  local tmp ev out status
  tmp="$(mktemp)"
  for ev in 'null' 'false' '0' '""' '[]' '{}'; do
    jq --argjson evidence "$ev" \
      '.rows[0].status = "passed" | .rows[0].evidence = $evidence' \
      "$FIXTURE" >"$tmp"
    set +e
    out="$("$MATRIX" --check --path "$tmp" 2>&1)"
    status=$?
    set -e
    assert_eq "$status" 1 "evidence=$ev"
    assert_contains "$out" 'passed rows must point at evidence' "evidence=$ev"
  done
  jq '.rows[0].status = "passed" | .rows[0].evidence = "hardware/amd.json"' \
    "$FIXTURE" >"$tmp"
  "$MATRIX" --check --path "$tmp" >/dev/null
  rm -f "$tmp"
}

keys_from() {
  python3 "$DEP_KEYS" /dev/stdin
}

test_dotted_dependency_table_is_detected() {
  local keys
  keys="$(keys_from <<'EOF'
[package]
name = "x"

[dependencies.reqwest]
version = "2"
EOF
)"
  assert_eq "$keys" reqwest
}

test_quoted_dependency_key_is_detected() {
  local keys
  keys="$(keys_from <<'EOF'
[dependencies]
"ureq" = "2.9"
hyper-util = "0.1"
EOF
)"
  printf '%s\n' "$keys" | awk '$0 == "ureq" { found = 1 } END { exit found ? 0 : 1 }'
  printf '%s\n' "$keys" | awk '$0 == "hyper-util" { found = 1 } END { exit found ? 0 : 1 }'
}

test_dotted_key_under_dependencies_is_detected() {
  local keys
  keys="$(keys_from <<'EOF'
[dev-dependencies]
isahc.workspace = true
EOF
)"
  assert_eq "$keys" isahc
}

test_root_dotted_dependency_is_detected() {
  local keys
  keys="$(keys_from <<'EOF'
dependencies.reqwest = "0.12"
EOF
)"
  assert_eq "$keys" reqwest
}

test_root_inline_dependency_is_detected() {
  local keys
  keys="$(keys_from <<'EOF'
dependencies = { hyper = "1" }
EOF
)"
  assert_eq "$keys" hyper
}

test_escaped_quoted_dependency_key_is_detected() {
  local keys
  keys="$(keys_from <<'EOF'
[dependencies]
"req\u0077est" = "0.12"
EOF
)"
  assert_eq "$keys" reqwest
}

test_malformed_manifest_is_rejected() {
  local status=0
  set +e
  keys_from <<'EOF' 2>/dev/null
[dependencies
reqwest = "2"
EOF
  status=$?
  set -e
  assert_eq "$status" 2
}

test_malformed_value_is_rejected() {
  local status=0
  set +e
  keys_from <<'EOF' 2>/dev/null
[package]
name = [
EOF
  status=$?
  set -e
  assert_eq "$status" 2
}

test_workspace_dependencies_are_not_scanned() {
  local keys
  keys="$(keys_from <<'EOF'
[workspace.dependencies]
reqwest = "2"
EOF
)"
  assert_eq "$keys" ""
}

run_test empty_matrix_is_rejected test_empty_matrix_is_rejected
run_test concatenated_matrices_are_rejected test_concatenated_matrices_are_rejected
run_test passed_row_requires_nonempty_evidence test_passed_row_requires_nonempty_evidence
run_test dotted_dependency_table_is_detected test_dotted_dependency_table_is_detected
run_test quoted_dependency_key_is_detected test_quoted_dependency_key_is_detected
run_test dotted_key_under_dependencies_is_detected test_dotted_key_under_dependencies_is_detected
run_test root_dotted_dependency_is_detected test_root_dotted_dependency_is_detected
run_test root_inline_dependency_is_detected test_root_inline_dependency_is_detected
run_test escaped_quoted_dependency_key_is_detected test_escaped_quoted_dependency_key_is_detected
run_test malformed_manifest_is_rejected test_malformed_manifest_is_rejected
run_test malformed_value_is_rejected test_malformed_value_is_rejected
run_test workspace_dependencies_are_not_scanned test_workspace_dependencies_are_not_scanned

finish_tests 'validator tests'
