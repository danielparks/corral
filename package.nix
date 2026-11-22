# corral, plus (when `devcontainer` is given) what it needs to run as a host.
{
  lib,
  rustPlatform,
  makeBinaryWrapper,
  git,
  gnutar,
  devcontainer ? null,
}:

rustPlatform.buildRustPackage {
  pname = "corral";
  version = (lib.importTOML ./Cargo.toml).package.version;

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./src
    ];
  };
  cargoLock.lockFile = ./Cargo.lock;

  nativeBuildInputs = [ makeBinaryWrapper ];

  # `podman` is deliberately not pinned here: rootless podman needs the
  # setuid helpers that the NixOS `virtualisation.podman` module provides.
  postInstall = lib.optionalString (devcontainer != null) ''
    wrapProgram $out/bin/corral \
      --prefix PATH : ${lib.makeBinPath [ git gnutar ]} \
      --set-default CORRAL_DEVCONTAINER ${lib.getExe devcontainer}
  '';

  meta = {
    description = "Run coding agents in containers on a remote host";
    mainProgram = "corral";
    platforms = lib.platforms.unix;
  };
}
