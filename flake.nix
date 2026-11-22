{
  description = "Run coding agents in containers on a remote host";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        # On Linux this is wrapped with the pinned devcontainer CLI, for use as
        # a host. Elsewhere it is just the client.
        corral = pkgs.callPackage ./package.nix (
          pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            devcontainer = pkgs.callPackage ./devcontainer.nix { };
          }
        );
        default = corral;
      });

      overlays.default = final: _prev: {
        corral = self.packages.${final.stdenv.hostPlatform.system}.corral;
      };

      formatter = forAllSystems (pkgs: pkgs.nixfmt-rfc-style);
    };
}
