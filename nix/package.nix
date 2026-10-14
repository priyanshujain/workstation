{ lib, rustPlatform, src }:
rustPlatform.buildRustPackage {
  pname = "wsctl";
  version = (lib.importTOML ../Cargo.toml).workspace.package.version;
  inherit src;
  cargoLock.lockFile = ../Cargo.lock;
  cargoBuildFlags = [ "--package" "wsctl" ];
  doCheck = false;
  meta = {
    description = "One declarative manager for a developer's whole machine";
    license = lib.licenses.gpl3Plus;
    mainProgram = "wsctl";
  };
}
