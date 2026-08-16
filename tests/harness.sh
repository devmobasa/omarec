#!/usr/bin/env bash
# Shared assertions for the shell test suites. Source this file.
# run_test must not invoke the case as an `if` condition: bash then ignores
# errexit inside the subshell, so mid-test failures can be reported as ok.

failed=0

write_exe() {
  printf '%s\n' "$2" >"$1"
  chmod u+x "$1"
}

run_test() {
  # Must be a simple command. `if run_test` / `run_test ||` ignore errexit
  # for the whole function, including the inner set -e.
  local name="$1" status=0
  set +e
  (
    set -euo pipefail
    "$2"
  )
  status=$?
  set -euo pipefail
  if (( status == 0 )); then
    printf 'ok %s\n' "$name"
  else
    printf 'not ok %s\n' "$name" >&2
    failed=$((failed + 1))
  fi
}

assert_eq() {
  [[ $1 == "$2" ]] || {
    printf 'expected %q, got %q (%s)\n' "$2" "$1" "${3:-assert_eq}" >&2
    exit 1
  }
}

assert_contains() {
  [[ $1 == *"$2"* ]] || {
    printf 'missing %q in %q (%s)\n' "$2" "$1" "${3:-assert_contains}" >&2
    exit 1
  }
}

assert_not_contains() {
  [[ $1 != *"$2"* ]] || {
    printf 'unexpected %q in %q (%s)\n' "$2" "$1" "${3:-assert_not_contains}" >&2
    exit 1
  }
}

finish_tests() {
  local label="${1:-tests}"
  if (( failed > 0 )); then
    printf '%s tests failed\n' "$failed" >&2
    exit 1
  fi
  printf '%s passed\n' "$label"
}
