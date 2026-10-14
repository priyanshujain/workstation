{ lib, stdenv, rustPlatform, makeWrapper, src, switcher }:
rustPlatform.buildRustPackage {
  pname = "wsctl";
  version = (lib.importTOML ../Cargo.toml).workspace.package.version;
  inherit src;
  cargoLock.lockFile = ../Cargo.lock;
  cargoBuildFlags = [ "--package" "wsctl" ];
  doCheck = false;
  nativeBuildInputs = [ makeWrapper ];
  postInstall = ''
    wrapProgram $out/bin/wsctl --suffix PATH : ${lib.makeBinPath [ switcher ]}
  '';
  meta = {
    description = "One declarative manager for a developer's whole machine";
    license = lib.licenses.gpl3Plus;
    mainProgram = "wsctl";
  };
}
