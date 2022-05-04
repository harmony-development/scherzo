{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nci = {
      url = "github:yusdacra/nix-cargo-integration";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flakeCompat = {
      url = "github:edolstra/flake-compat";
      flake = false;
    };
  };

  outputs = inputs:
    inputs.nci.lib.makeOutputs {
      root = ./.;
      overrides = {
        crates = common: _: {
          mediasoup-sys = prev: let
            pkgs = common.pkgs;
            pythonPkgs = pkgs: with pkgs; [pip];
            pythonWithPkgs = pkgs.python3.withPackages pythonPkgs;
            all = (with pkgs; [cmake gnumake nodejs meson ninja]) ++ [pythonWithPkgs];
          in {
            buildInputs = (prev.buildInputs or []) ++ all;
            nativeBuildInputs = (prev.nativeBuildInputs or []) ++ all;
          };
        };
        shell = common: prev: {
          packages =
            prev.packages
            ++ (with common.pkgs; [
              mold
              mkcert
            ]);
          commands =
            prev.commands
            ++ [
              {
                name = "debug-server";
                command = ''cargo run --no-default-features --features sled -- -d'';
              }
              {
                name = "generate-cert";
                command = ''
                  mkcert localhost 127.0.0.1 ::1
                  mv localhost+2.pem cert.pem
                  mv localhost+2-key.pem key.pem
                '';
              }
            ];
        };
      };
    };
}
