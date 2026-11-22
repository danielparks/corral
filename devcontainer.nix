# The devcontainer CLI, pinned to the version corral is tested with.
#
# nixpkgs' package pulls in `docker` (currently marked insecure). Corral points
# the CLI at its own filter via `--docker-path`, so the docker on its PATH is
# never used; podman stands in.
{
  devcontainer,
  fetchFromGitHub,
  fetchYarnDeps,
  podman,
  podman-compose,
}:

let
  version = "0.91.0";
  src = fetchFromGitHub {
    owner = "devcontainers";
    repo = "cli";
    tag = "v${version}";
    hash = "sha256-QarNe2+122909q4io0WfMRqg2ppD3f6Q+NzTSkFVGzc=";
  };
in
(devcontainer.override {
  docker = podman;
  docker-compose = podman-compose;
}).overrideAttrs
  {
    inherit version src;
    yarnOfflineCache = fetchYarnDeps {
      yarnLock = "${src}/yarn.lock";
      hash = "sha256-uShY5UJXgV+07jMulTmngJ5U4cVm7exKtS/79YIhe7A=";
    };
  }
