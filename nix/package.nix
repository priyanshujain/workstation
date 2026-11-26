{ lib, stdenv, rustPlatform, makeWrapper, pinentry_mac, pinentry-qt, withPinentry ? true, src, switcher }:
let
  pinentry = if stdenv.hostPlatform.isDarwin then pinentry_mac else pinentry-qt;
in
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
  '' + lib.optionalString withPinentry ''
    wrapProgram $out/bin/wsctl-askpass --set-default WSCTL_PINENTRY ${lib.getExe pinentry}
  '';
  meta = {
    description = "One declarative manager for a developer's whole machine";
    license = lib.licenses.gpl3Plus;
    mainProgram = "wsctl";
  };
}
