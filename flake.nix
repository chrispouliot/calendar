{
  description = "Rust GTK4/libadwaita development shell using Fenix";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    nixpkgs,
    fenix,
    ...
  }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      forAllSystems = nixpkgs.lib.genAttrs systems;
    in {
      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          fenixPkgs = fenix.packages.${system};

          rustToolchain = fenixPkgs.stable.withComponents [
            "cargo"
            "clippy"
            "rust-src"
            "rustc"
            "rustfmt"
          ];

          xdgDataDirs = pkgs.lib.makeSearchPath "share" [
            pkgs.adwaita-icon-theme
            pkgs.hicolor-icon-theme
            pkgs.shared-mime-info
            pkgs.gsettings-desktop-schemas
            pkgs.gtk4
            pkgs.libadwaita
          ];

          gtkRuntimeLibraries = [
            pkgs.glib
            pkgs.gdk-pixbuf
            pkgs.gtk4
            pkgs.cairo
            pkgs.graphene
            pkgs.pango
            pkgs.harfbuzz
            pkgs.vulkan-loader
            pkgs.libadwaita
          ];
        in {
          default = pkgs.mkShell {
            strictDeps = true;

            nativeBuildInputs = [
              # Rust toolchain and editor support
              rustToolchain
              fenixPkgs.rust-analyzer

              # Rust development helpers
              pkgs.cargo-edit
              pkgs.cargo-watch
              pkgs.git
              pkgs.radicale

              # Native build system and pkg-config discovery
              pkgs.pkg-config
              pkgs.meson
              pkgs.ninja
              pkgs.gcc

              # GNOME application tooling
              pkgs.glib.dev
              pkgs.gobject-introspection
              pkgs.blueprint-compiler
              pkgs.desktop-file-utils
              pkgs.appstream-glib
              pkgs.gettext
            ];

            buildInputs = gtkRuntimeLibraries ++ [
              # Runtime data commonly expected by GTK/GNOME apps
              pkgs.gsettings-desktop-schemas
              pkgs.adwaita-icon-theme
              pkgs.hicolor-icon-theme
              pkgs.shared-mime-info
            ];

            RUST_BACKTRACE = "1";

            RUST_SRC_PATH =
              "${fenixPkgs.stable.rust-src}/lib/rustlib/src/rust/library";

            GSETTINGS_SCHEMA_DIR =
              pkgs.glib.getSchemaPath pkgs.gsettings-desktop-schemas;

            GI_TYPELIB_PATH =
              pkgs.lib.makeSearchPath "lib/girepository-1.0" [
                pkgs.glib
                pkgs.gtk4
                pkgs.libadwaita
              ];

            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath gtkRuntimeLibraries;

            shellHook = ''
              export XDG_DATA_DIRS="${xdgDataDirs}''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"

              echo "Rust GTK4/libadwaita development shell"
              echo "  rustc:          $(rustc --version)"
              echo "  cargo:          $(cargo --version)"
              echo "  GTK4:           $(pkg-config --modversion gtk4)"
              echo "  libadwaita:     $(pkg-config --modversion libadwaita-1)"
              echo "  Blueprint:      $(blueprint-compiler --version 2>/dev/null || true)"
            '';
          };
        }
      );
    };
}
