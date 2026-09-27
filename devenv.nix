{ pkgs, ... }:

{
  languages.rust = {
    enable = true;
    channel = "stable";
    components = [ "rustc" "cargo" "clippy" "rustfmt" ];
  };

  packages = with pkgs; [
    pkg-config
    cmake

    # Bevy link-time system dependencies (headers + libs).
    alsa-lib.dev
    alsa-lib
    systemd.dev
    systemd
    wayland.dev
    wayland
    libxkbcommon.dev
    libxkbcommon
    libX11.dev
    libX11
    xorg.libxcb

    # Headless world-viewer runs: virtual display plus software rendering.
    xorg.libXcursor
    xorg.libXi
    xorg.libXrandr
    xorg.libXfixes
    xorg.libXrender
    xorg.libXext
    xvfb-run
    xorg.xorgserver
    mesa
    vulkan-loader
    vulkan-tools
    vulkan-validation-layers
  ];
}
