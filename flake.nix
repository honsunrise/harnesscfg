{
  description = "Seed and sync the writable configuration of AI coding agent harnesses from Nix";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: rec {
        harnesscfg = pkgs.callPackage ./nix/package.nix { };
        default = harnesscfg;
      });

      overlays.default = final: _: { harnesscfg = final.callPackage ./nix/package.nix { }; };

      homeModules.default = import ./nix/hm-module.nix self;

      checks = forAll (pkgs: {
        package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        format =
          pkgs.runCommand "harnesscfg-format"
            {
              nativeBuildInputs = [
                pkgs.cargo
                pkgs.rustfmt
                pkgs.nixfmt
              ];
            }
            ''
              cd ${self}
              cargo fmt --check
              nixfmt --check flake.nix nix/*.nix
              touch $out
            '';
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.default ];
          packages = [
            pkgs.clippy
            pkgs.rust-analyzer
            pkgs.rustfmt
            pkgs.nixfmt
          ];
        };
      });

      formatter = forAll (pkgs: pkgs.nixfmt);
    };
}
