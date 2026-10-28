{ lib, rustPlatform }:
let
  manifest = lib.importTOML ../Cargo.toml;
in
rustPlatform.buildRustPackage {
  pname = manifest.package.name;
  inherit (manifest.package) version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../build.rs
      ../src
      ../tests
      ../registry
      ../README.md
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  meta = {
    description = "Seed and sync the writable configuration of AI coding agent harnesses from Nix";
    homepage = "https://github.com/honsunrise/harnesscfg";
    license = lib.licenses.mit;
    mainProgram = "harnesscfg";
    platforms = lib.platforms.unix;
  };
}
