#!/bin/sh
# V56: an incomplete portable package reports its Vulkan loader dependency.
set -eu

fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT HUP INT TERM
cp "$(dirname "$0")/run-riverwood.sh" "$fixture/run-riverwood.sh"
mkdir "$fixture/lib"
if "$fixture/run-riverwood.sh" >"$fixture/stdout" 2>"$fixture/stderr"; then
  echo 'V56 failed: launcher accepted a package without libdl.so.2' >&2
  exit 1
fi
if ! grep -Fq 'missing lib/libdl.so.2 required for Vulkan loading' "$fixture/stderr"; then
  echo 'V56 failed: launcher did not name the missing Vulkan dependency' >&2
  cat "$fixture/stderr" >&2
  exit 1
fi
echo 'V56 launcher preflight passed'

# V59: the package must use the host Vulkan ICD and resolve its ALSA hook.
libdl=$(ldd /bin/sh | awk '$1 == "libdl.so.2" { print $3; exit }')
cp "$libdl" "$fixture/lib/libdl.so.2"
cat > "$fixture/lib/ld-linux-x86-64.so.2" <<'EOF'
#!/bin/sh
printf 'VK_ICD_FILENAMES=%s\n' "${VK_ICD_FILENAMES-unset}"
printf 'ALSA_PLUGIN_DIR=%s\n' "${ALSA_PLUGIN_DIR-unset}"
EOF
chmod +x "$fixture/lib/ld-linux-x86-64.so.2"
env -u VK_ICD_FILENAMES DISPLAY=:0 XKB_CONFIG_ROOT=/nonexistent \
  "$fixture/run-riverwood.sh" >"$fixture/default-env"
grep -Fxq 'VK_ICD_FILENAMES=unset' "$fixture/default-env"
grep -Fxq "ALSA_PLUGIN_DIR=$fixture" "$fixture/default-env"
VK_ICD_FILENAMES=/tmp/test-intel-icd.json DISPLAY=:0 XKB_CONFIG_ROOT=/nonexistent \
  "$fixture/run-riverwood.sh" >"$fixture/override-env"
grep -Fxq 'VK_ICD_FILENAMES=/tmp/test-intel-icd.json' "$fixture/override-env"
echo 'V59 host Vulkan and bundled ALSA environment passed'
