{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    pkg-config
  ];

  buildInputs = with pkgs; [
    rustc
    cargo
    clippy
    rustfmt
    rust-analyzer
    rustPlatform.rustLibSrc

    gcc

    wayland
    libxkbcommon
    vulkan-loader
    libGL
    libX11
    libXcursor
    libXi
    libXrandr
    alsa-lib
    udev
    openssl
    dbus
    fontconfig
    freetype

    gtk3
    glib
    pango
    cairo
    gdk-pixbuf
    atk
  ];

  shellHook = ''
    export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath [
      pkgs.wayland
      pkgs.libxkbcommon
      pkgs.vulkan-loader
      pkgs.libGL
      pkgs.libX11
      pkgs.libXcursor
      pkgs.libXi
      pkgs.libXrandr
      pkgs.alsa-lib
      pkgs.udev
      pkgs.openssl
      pkgs.dbus
      pkgs.fontconfig
      pkgs.freetype
      pkgs.gtk3
      pkgs.glib
      pkgs.pango
      pkgs.cairo
      pkgs.gdk-pixbuf
      pkgs.atk
      pkgs.expat
    ]}:$LD_LIBRARY_PATH"
    export RUST_SRC_PATH="${pkgs.rustPlatform.rustLibSrc}"
    # Prefer nix cargo in this shell even if ~/.cargo/bin shadows it.
    export PATH="${pkgs.cargo}/bin:${pkgs.rustc}/bin:$PATH"
    echo "mcsc dev shell: $(cargo --version), $(rustc --version)"
  '';
}
