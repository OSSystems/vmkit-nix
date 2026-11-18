{ pkgs, ... }:

pkgs.buildGoModule {
  pname = "vmkit-tests";
  version = "0.1.0";
  src = pkgs.lib.fileset.toSource {
    root = ../..;
    fileset = pkgs.lib.fileset.fileFilter (f: f.hasExt "go" || f.name == "go.mod") ../..;
  };
  vendorHash = null;
  env.CGO_ENABLED = 0;
  postInstall = "touch $out/ok";
}
