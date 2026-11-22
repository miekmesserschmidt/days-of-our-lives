{
  description = "days-of-our-lives — a simple, plugin based desktop bar for Hyprland";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    py-analogue-clock-src = {
      url = "github:miekmesserschmidt/py-analogue-clock";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, py-analogue-clock-src }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: import ./nix/packages.nix { inherit pkgs py-analogue-clock-src; src = ./.; });

      devShells = forAllSystems (pkgs:
        let p = self.packages.${pkgs.system}; in {
          default = pkgs.mkShell {
            packages = [
              pkgs.cargo pkgs.rustc pkgs.clippy pkgs.rustfmt pkgs.rust-analyzer pkgs.pkg-config
              p.python-env
            ] ++ p.runtime-libs;
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath p.runtime-libs;
          };
        });

      nixosModules.default = import ./nix/module.nix self;
    };
}
