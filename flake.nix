{
  description = "pgtop — TUI activity monitor for PostgreSQL";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane = {
      url = "github:ipetkov/crane";
    };
  };

  outputs = { self, nixpkgs, flake-utils, crane }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        craneLib = crane.mkLib pkgs;
        src = craneLib.cleanCargoSource ./.;

        commonArgs = {
          inherit src;
          strictDeps = true;

          # rustls / ring need cmake on some platforms; libiconv on Darwin.
          nativeBuildInputs = [ pkgs.cmake ];
          buildInputs = pkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.libiconv
          ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        pgtop = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = "pgtop";
        });
      in
      {
        packages.default = pgtop;
        packages.pgtop = pgtop;

        apps.default = flake-utils.lib.mkApp { drv = pgtop; };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ pgtop ];
          packages = with pkgs; [
            cargo
            rustc
            rustfmt
            clippy
            rust-analyzer
          ];
        };
      });
}
