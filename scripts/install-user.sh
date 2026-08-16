#!/usr/bin/env bash
# Install omarec into ~/.local/bin and the user systemd manager.
# Does not write /usr/bin: that path is Omarchy's legacy dispatcher.
set -euo pipefail

usage() {
  printf '%s\n' \
    "usage: $0 [--skip-build] [--skip-deps] [--no-plugin]" \
    "Install binaries to \${OMAREC_PREFIX:-~/.local}/bin, user units that start those binaries," \
    "and the community.omarec shell plugin. Does not restart the shell." \
    "CARGO_TARGET_DIR defaults to <checkout>/target."
}

skip_build=0
skip_deps=0
no_plugin=0
for arg in "$@"; do
  case "$arg" in
    --skip-build) skip_build=1 ;;
    --skip-deps) skip_deps=1 ;;
    --no-plugin) no_plugin=1 ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'unknown argument: %s\n' "$arg" >&2
      usage >&2
      exit 2
      ;;
  esac
done

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
home="${HOME:?HOME is required}"
prefix="${OMAREC_PREFIX:-$home/.local}"
bindir="$prefix/bin"
target_dir="${CARGO_TARGET_DIR:-$root/target}"
config="${XDG_CONFIG_HOME:-$home/.config}"
systemd_user="$config/systemd/user"
plugin_src="$root/integration/quickshell/community.omarec"
plugin_dst="$config/omarchy/plugins/community.omarec"

if [[ ! -f $root/Cargo.toml ]]; then
  printf 'not an omarec checkout: %s\n' "$root" >&2
  exit 1
fi

need() {
  command -v "$1" >/dev/null 2>&1 || {
    printf 'missing command: %s\n' "$1" >&2
    exit 1
  }
}

need cargo
need install
need systemctl

if (( skip_deps == 0 )) && command -v pacman >/dev/null 2>&1; then
  sudo pacman -S --needed gpu-screen-recorder ffmpeg jq gtk4 gtk4-layer-shell
fi

if (( skip_build == 0 )); then
  cargo build --release --locked --bin omarec --bin omarecd --manifest-path "$root/Cargo.toml"
fi

for binary in omarec omarecd; do
  src="$target_dir/release/$binary"
  if [[ ! -f $src ]]; then
    printf 'missing %s; build first or omit --skip-build\n' "$src" >&2
    exit 1
  fi
  install -Dm755 "$src" "$bindir/$binary"
done

for name in \
  omarchy-capture-screenrecording \
  omarchy-capture-screenrecording-omarec \
  omarchy-capture-screenrecording-legacy \
  omarec-notify
do
  install -Dm755 "$root/integration/omarchy/bin/$name" "$bindir/$name"
done

install -Dm644 "$root/integration/systemd/omarec.service" \
  "$systemd_user/omarec.service"
install -Dm644 "$root/integration/systemd/omarec-notify.service" \
  "$systemd_user/omarec-notify.service"

write_dropin() {
  local unit="$1" exec_start="$2"
  local dir="$systemd_user/${unit}.d"
  mkdir -p "$dir"
  printf '%s\n' \
    '[Service]' \
    'ExecStart=' \
    "ExecStart=${exec_start}" \
    "Environment=PATH=${bindir}:/usr/local/bin:/usr/bin" \
    >"$dir/local.conf"
}

write_dropin omarec.service "$bindir/omarecd"
write_dropin omarec-notify.service "$bindir/omarec-notify"

systemctl --user daemon-reload
systemctl --user enable --now omarec.service omarec-notify.service

if (( no_plugin == 0 )) && [[ -d $plugin_src ]] && command -v omarchy >/dev/null 2>&1; then
  mkdir -p "$(dirname "$plugin_dst")"
  rm -rf "$plugin_dst"
  cp -a "$plugin_src" "$plugin_dst"
  omarchy plugin validate "$plugin_dst"
  omarchy plugin enable community.omarec
  if command -v omarchy-shell >/dev/null 2>&1; then
    omarchy-shell shell rescanPlugins >/dev/null 2>&1 || true
  fi
  printf '%s\n' \
    "plugin installed at $plugin_dst" \
    "bar widgets do not hot-reload; run: omarchy restart shell" \
    "(that command refuses while the session is locked)"
fi

if [[ :$PATH: != *:"$bindir":* ]]; then
  printf 'warning: %s is not on PATH; use %s/omarec until it is\n' "$bindir" "$bindir" >&2
fi

printf '%s\n' \
  "installed to $bindir" \
  "merge $root/integration/omarchy/menu/omarchy-menu.jsonc into" \
  "  $config/omarchy/extensions/omarchy-menu.jsonc if you want capture-menu entries" \
  "rollback: export OMAREC_SCREENRECORDING=0" \
  "next: $bindir/omarec doctor"
