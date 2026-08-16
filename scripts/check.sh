#!/usr/bin/env bash
set -euo pipefail

required_minor=95
version="$(rustc --version | awk '{print $2}')"
minor="$(cut -d. -f2 <<<"$version")"
if (( minor < required_minor )); then
  printf 'Rust 1.%s+ is required; found %s\n' "$required_minor" "$version" >&2
  exit 1
fi

cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --locked
jq -e -s 'length == 1' integration/quickshell/community.omarec/manifest.json >/dev/null
bash -n integration/omarchy/bin/omarchy-capture-screenrecording
bash -n integration/omarchy/bin/omarchy-capture-screenrecording-omarec
bash -n integration/omarchy/bin/omarchy-capture-screenrecording-legacy
bash -n integration/omarchy/bin/omarec-notify
bash -n scripts/hardware-qualify.sh
bash -n scripts/hardware-matrix.sh
bash -n scripts/install-user.sh
bash -n scripts/static-check.sh
bash -n install-omarchy
bash -n tests/harness.sh
bash -n tests/scripts/test_validators.sh
bash tests/dispatcher/test_router.sh
bash tests/notify/test_notifier.sh
bash tests/quattro/test_watch_consumer.sh
bash tests/scripts/test_validators.sh
bash scripts/hardware-matrix.sh --check
bash scripts/static-check.sh
