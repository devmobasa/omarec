# Contributing and implementation rules

This repository is structured for incremental implementation. Keep changes reviewable and preserve the separation between policy, lifecycle, backend adapters, desktop integration, and presentation.

## Development setup

On Arch Linux:

```bash
sudo pacman -S --needed base-devel rustup gpu-screen-recorder systemd gtk4 gtk4-layer-shell jq python
rustup toolchain install 1.97.1 --component rustfmt,clippy
rustup override set 1.97.1
cargo generate-lockfile
make check
```

Additional packages are needed for real integration/hardware tests; see [DRIVERS_AND_BACKENDS.md](DRIVERS_AND_BACKENDS.md).

## Toolchain policy

- `rust-toolchain.toml` pins the normal development/release compiler.
- `rust-version = "1.95"` is the MSRV contract.
- Source and dependencies must compile on the MSRV job.
- Do not use a newer language/library feature without intentionally raising MSRV and documenting the decision.
- Commit `Cargo.lock` because the workspace produces applications.

## Architecture rules

1. `omarec-core` remains deterministic and independent of Tokio, systemd, GSR, Hyprland, and QML.
2. External commands are invoked with argv arrays; never construct a shell command.
3. State transitions occur through one session service/state machine.
4. Backend stderr is diagnostic evidence, not a public API parsed by QML.
5. The protocol uses stable error/event codes; user-facing prose can change.
6. One session has one exact unit, PID/cgroup, runtime directory, IPC socket, and staging path.
7. The final file is not reported successful until acknowledgement, validation, and promotion finish.
8. Capability policy is pure and fixture-tested before external side effects.
9. Quattro is a client, not the state authority.
10. New extension mechanisms require a security/compatibility ADR.

## Coding style

Run:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

Guidelines:

- avoid `unwrap`/`expect` outside tests and provably infallible build-time constants;
- use typed errors internally and stable codes at the protocol boundary;
- include context without leaking sensitive paths/devices;
- prefer exhaustive enums for lifecycle/domain concepts;
- keep subprocess adapters narrow and injectable;
- use bounded channels/buffers;
- avoid long critical sections across `.await`;
- document invariants near the owning type;
- do not add `unsafe` without first changing the workspace policy through an ADR/security review.

## Testing expectations

Every behavioral change needs:

- a success-path test;
- at least one failure-path test;
- state/event assertions;
- protocol fixture updates when externally visible;
- command-plan golden updates for backend changes;
- hardware evidence when changing driver/capture behavior.

Do not replace deterministic barriers with arbitrary sleeps in async/process tests.

## Commit and pull-request scope

Prefer pull requests that change one architectural concern:

- domain/protocol;
- backend planning;
- supervisor/lifecycle;
- finalization/recovery;
- Omarchy adapter;
- Quattro UI;
- packaging/dependency updates.

A pull request should state:

```text
problem
behavioral change
non-goals
failure/recovery behavior
tests
security/privacy impact
migration/rollback impact
```

Update an ADR when changing a foundational decision.

## Dependency policy

Before adding a crate, document:

- why the standard library/current dependencies are insufficient;
- maintenance and release activity;
- MSRV;
- license;
- transitive dependency/build-script/proc-macro impact;
- whether the dependency handles untrusted input or privileged boundaries.

Prefer a small explicit adapter over a broad framework. Do not add an async D-Bus, PipeWire, or compositor stack until a concrete milestone requires it.

## Protocol changes

Protocol v1 is newline-delimited JSON over a local Unix socket.

- additive optional response fields are preferred;
- request behavior cannot be silently reinterpreted;
- enum/error/event codes are compatibility surface;
- breaking changes require a new protocol version or negotiated feature;
- update [PROTOCOL.md](PROTOCOL.md), fixtures, CLI, and QML together;
- old clients must receive an explicit incompatibility response.

## Security-sensitive review areas

Request additional review for:

- output path and filesystem operations;
- socket ownership/peer credentials;
- process identity/signaling;
- environment/credential handling;
- external hooks or shared-object plugins;
- support-bundle redaction;
- systemd hardening;
- portal/camera/microphone behavior.

See [SECURITY.md](SECURITY.md).

## Release process outline

1. pass pinned stable and MSRV CI;
2. run dependency/license/advisory checks;
3. build an Arch package in a clean chroot;
4. run packaged smoke/integration tests;
5. complete required hardware matrix rows;
6. verify upgrade, downgrade, and legacy rollback;
7. update changelog/support matrix/protocol notes;
8. tag and sign according to project policy;
9. publish source and checksums.
