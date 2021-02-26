{
  description = "Flake for scherzo";

  inputs = {
    devshell.url = "github:numtide/devshell";
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    naersk = {
      url = "github:nmattia/naersk";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flakeUtils.url = "github:numtide/flake-utils";
    rustOverlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs: with inputs;
    with flakeUtils.lib;
    eachSystem defaultSystems (system:
      let
        common = import ./nix/common.nix {
          sources = { inherit devshell naersk nixpkgs rustOverlay; };
          inherit system;
        };

        packages = {
          # Compiles slower but has tests and faster executable
          "scherzo" = import ./nix/build.nix {
            inherit common;
            doCheck = false;
            release = true;
          };
          # Compiles faster but no tests and slower executable
          "scherzo-debug" = import ./nix/build.nix { inherit common; };
          # Compiles faster but has tests and slower executable
          "scherzo-tests" = import ./nix/build.nix { inherit common; doCheck = true; };
        };
        apps = builtins.mapAttrs (n: v: mkApp { name = n; drv = v; exePath = "/bin/scherzo"; }) packages;
      in
      {
        inherit packages apps;

        # Release build is the default package
        defaultPackage = packages."scherzo";

        # Release build is the default app
        defaultApp = apps."scherzo";

        devShell = import ./nix/devShell.nix { inherit common; };
      }
    );
}
