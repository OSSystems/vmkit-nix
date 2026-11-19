{ pkgs, ... }:

pkgs.rustPlatform.buildRustPackage {
  pname = "vmkit-rust-tests";
  version = "0.2.0";
  src = pkgs.lib.fileset.toSource {
    root = ../..;
    fileset = pkgs.lib.fileset.unions [
      ../../Cargo.toml
      ../../Cargo.lock
      ../../rustfmt.toml
      ../../src
      ../../testdata
    ];
  };
  cargoLock.lockFile = ../../Cargo.lock;
  nativeBuildInputs = [
    pkgs.clippy
    pkgs.rustfmt
  ];
  preCheck = ''
    cargo fmt --check
    cargo clippy --all-targets --offline -- -D warnings
  '';
  installPhase = "touch $out";
}
