{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nixCargoIntegration = {
      url = "github:yusdacra/nix-cargo-integration/master";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flakeCompat = {
      url = "github:edolstra/flake-compat";
      flake = false;
    };
  };

  outputs = inputs: inputs.nixCargoIntegration.lib.makeOutputs {
    root = ./.;
    buildPlatform = "naersk";
    overrides = {
      crateOverrides = common: _: {
        mediasoup-sys = prev:
          let
            pkgs = common.pkgs;
            pythonPkgs = pkgs: with pkgs; [
              pip
            ];
            pythonWithPkgs = pkgs.python3.withPackages pythonPkgs;
            all = (with pkgs; [ cmake gnumake nodejs meson ninja ]) ++ [ pythonWithPkgs ];
          in
          {
            buildInputs = (prev.buildInputs or [ ]) ++ all;
            nativeBuildInputs = (prev.nativeBuildInputs or [ ]) ++ all;
          };
      };
      shell = common: prev: {
        packages = prev.packages ++ [
          common.pkgs.musl.dev
          /*(common.lib.buildCrate {
            memberName = "tokio-console";

            root = builtins.fetchGit {
            url = "https://github.com/tokio-rs/console.git";
            rev = "a30264e0b5469ea596430b846b05e6e3541915d1";
            ref = "main";
            };

            inherit (common) nativeBuildInputs buildInputs;
            CARGO_PKG_REPOSITORY = "https://github.com/tokio-rs/console";
            })*/
        ];
        commands = prev.commands ++ [
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
