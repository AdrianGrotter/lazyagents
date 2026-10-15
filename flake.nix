{
  description = "lazyagents: a lazygit-style TUI for managing coding agents";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, flake-utils, rust-overlay, ... }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            toolchain
            pkgs.just
            pkgs.cargo-nextest
            pkgs.cargo-insta
            pkgs.cargo-deny
            pkgs.jq
            pkgs.tmux
            pkgs.git
          ];
          RUST_BACKTRACE = "1";
          shellHook = ''
            if git rev-parse --git-dir >/dev/null 2>&1; then
              git config core.hooksPath .githooks
            fi
          '';
        };
        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
