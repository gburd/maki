{
  description = "Maki - AI coding agent";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      lib = nixpkgs.lib;
      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
      packageName = cargoToml.package.name;
      version = cargoToml.workspace.package.version;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forEachSystem =
        f:
        lib.genAttrs systems (
          system:
          f system (import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          })
        );
    in
    {
      packages = forEachSystem (
        system: pkgs:
        let
          maki = pkgs.rustPlatform.buildRustPackage {
            pname = packageName;
            inherit version;
            src = ./.;
            cargoLock = {
              lockFile = ./Cargo.lock;
              # NOTE: these are cargo git dependencies; set hash to "" and
              # rebuild to get the correct value.
              outputHashes = {
                "monty-0.0.11" = "sha256-PRP8XcgeNVnc+2dWHxpizjvAtSjfqtkEXckXjPCRoJI=";
                "ruff_python_ast-0.0.0" = "sha256-nVQC4ZaLWiZBUEReLqzpXKxXVxCdUW6b+mda9J8JSA0=";
                "ruff_python_parser-0.0.0" = "sha256-nVQC4ZaLWiZBUEReLqzpXKxXVxCdUW6b+mda9J8JSA0=";
                "ruff_python_trivia-0.0.0" = "sha256-nVQC4ZaLWiZBUEReLqzpXKxXVxCdUW6b+mda9J8JSA0=";
                "ruff_source_file-0.0.0" = "sha256-nVQC4ZaLWiZBUEReLqzpXKxXVxCdUW6b+mda9J8JSA0=";
                "ruff_text_size-0.0.0" = "sha256-nVQC4ZaLWiZBUEReLqzpXKxXVxCdUW6b+mda9J8JSA0=";
              };
            };
            cargoBuildFlags = [
              "--package"
              packageName
            ];
            nativeBuildInputs = with pkgs; [
              pkg-config
              perl
              python3
            ];
            # TODO: Upstream monty includes a relative README path that doesn't
            # survive nix vendoring. Remove this once `monty` stops including
            # the relative path
            postPatch = ''
              for f in "$cargoDepsCopy"/monty-*/src/lib.rs; do
                substituteInPlace "$f" \
                  --replace-fail '#![doc = include_str!("../../../README.md")]' \
                                 '#![doc = "Monty Python bridge."]'
              done
            '';
            buildInputs = with pkgs; [ openssl ];
            doCheck = false;
          };
        in
        {
          default = maki;
        }
      );

      devShells = forEachSystem (
        _: pkgs:
        let
          certs = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
          rustToolchain = pkgs.rust-bin.stable.latest.default.override {
            targets = lib.optionals pkgs.stdenv.isLinux [
              "x86_64-unknown-linux-musl"
              "aarch64-unknown-linux-musl"
            ];
          };
          # Full GCC cross-compiler targeting musl. Provides musl-compatible
          # gcc, g++, and libstdc++.a (unlike musl-gcc which only wraps the
          # host GCC and links against glibc's libstdc++).
          muslCross = pkgs.pkgsCross.musl64.stdenv.cc;
          muslCC = "${muslCross}/bin/x86_64-unknown-linux-musl-gcc";
          muslCXX = "${muslCross}/bin/x86_64-unknown-linux-musl-g++";
        in
        {
          default = pkgs.mkShell {
            packages =
              [ rustToolchain ]
              ++ (with pkgs; [
                cargo-nextest
                git
                just
                lld # Rust 1.85+ defaults to lld on x86_64-linux-gnu
                openssl
                perl
                pkg-config
                python3
                ripgrep
                ruff
                stylua
                ty
              ]);

            SSL_CERT_FILE = certs;
            NIX_SSL_CERT_FILE = certs;

            # Musl static build configuration (Linux only).
            # Uses the musl cross-compiler so ALL C/C++ code (tree-sitter
            # grammars, Luau, curl, openssl) is compiled against musl with
            # a musl-compatible libstdc++.
            CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER =
              lib.optionalString pkgs.stdenv.isLinux muslCC;
            CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS =
              lib.optionalString pkgs.stdenv.isLinux "-C target-feature=+crt-static";
            CC_x86_64_unknown_linux_musl =
              lib.optionalString pkgs.stdenv.isLinux muslCC;
            CXX_x86_64_unknown_linux_musl =
              lib.optionalString pkgs.stdenv.isLinux muslCXX;
          };
        }
      );

      formatter = forEachSystem (_: pkgs: pkgs.nixfmt-rfc-style);
    };
}
