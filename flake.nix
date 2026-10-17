{
  description = "mcsc — Minecraft Server World Converter (Purpur/Paper/Spigot/Fabric → Vanilla, and back)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
  };

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs { inherit system; }));
      guiLibs = pkgs: with pkgs; [
        xorg.libX11
        xorg.libXcursor
        xorg.libXi
        xorg.libXrandr
        wayland
        libxkbcommon
        vulkan-loader
        libGL
        fontconfig
      ];
    in
    {
      packages = forAll (pkgs: {
        mcsc = pkgs.rustPlatform.buildRustPackage {
          pname = "mcsc";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = with pkgs; [
              pkg-config
              makeWrapper
            ];
            buildInputs = guiLibs pkgs;
            postFixup = ''
              wrapProgram $out/bin/mcsc \
                --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath (guiLibs pkgs)}" \
                --suffix XDG_DATA_DIRS : "/run/opengl-driver/share"
            '';
            meta = with pkgs.lib; {
              description = "Convert Minecraft server worlds between layouts (Paper/Purpur/Spigot/Fabric ↔ Vanilla)";
              license = licenses.mit;
              platforms = platforms.linux;
              mainProgram = "mcsc";
            };
          };
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.mcsc;
      });

      apps = forAll (pkgs: {
        mcsc = {
          type = "app";
          program = nixpkgs.lib.getExe self.packages.${pkgs.stdenv.hostPlatform.system}.mcsc;
        };
        default = self.apps.${pkgs.stdenv.hostPlatform.system}.mcsc;
      });
    };
}
