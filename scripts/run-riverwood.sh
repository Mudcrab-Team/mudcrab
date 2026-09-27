#!/bin/sh
# Fiji Riverwood package launcher (schema 4, terrain physics, V walk, T tankard, E pickup).
# Bundled models and textures cover a six-cell radius around grid (5, -12).
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
export LD_LIBRARY_PATH="$dir/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export VK_ICD_FILENAMES=/run/opengl-driver/share/vulkan/icd.d/radeon_icd.x86_64.json
# Terminals opened through SSH or tmux can lack the graphical session's display
# variables even while the same user has an active Wayland compositor.
if [ -z "${XDG_RUNTIME_DIR:-}" ] && [ -d "/run/user/$(id -u)" ]; then
  XDG_RUNTIME_DIR="/run/user/$(id -u)"
  export XDG_RUNTIME_DIR
fi
if [ -z "${WAYLAND_DISPLAY:-}" ] && [ -z "${WAYLAND_SOCKET:-}" ] && [ -z "${DISPLAY:-}" ]; then
  for socket in "${XDG_RUNTIME_DIR:-/nonexistent}"/wayland-*; do
    [ -S "$socket" ] || continue
    name=${socket##*/}
    number=${name#wayland-}
    case "$number" in ''|*[!0-9]*) continue ;; esac
    WAYLAND_DISPLAY=$name
    export WAYLAND_DISPLAY
    break
  done
  if [ -z "${WAYLAND_DISPLAY:-}" ]; then
    echo 'No Wayland display found; start an active Fiji desktop session or set WAYLAND_DISPLAY.' >&2
    exit 1
  fi
fi
# Bundled libxkbcommon bakes in the build host's xkeyboard-config path; fiji
# ships a different version, so point at its real xkb data.
if [ -z "$XKB_CONFIG_ROOT" ]; then
  for candidate in /nix/store/*-xkeyboard-config-*/share/X11/xkb; do
    if [ -d "$candidate" ]; then
      XKB_CONFIG_ROOT="$candidate"
      break
    fi
  done
  export XKB_CONFIG_ROOT
fi
exec "$dir/lib/ld-linux-x86-64.so.2" --library-path "$dir/lib" "$dir/bin/engine" \
  --assets "$dir/assets" --worldspace 60 --grid-x 5 --grid-y -12 --stream-radius 2 "$@"
