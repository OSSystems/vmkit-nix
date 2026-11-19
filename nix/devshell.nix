{ pkgs, ... }:
pkgs.mkShell {
  packages = with pkgs; [
    cargo
    clippy
    rustc
    rustfmt
    rust-analyzer
  ];
  env.RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
}
