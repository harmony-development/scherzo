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
    buildPlatform = "crate2nix";
    overrides = {
      crateOverrides = common: _: {
        mediasoup-sys = prev: {
          nativeBuildInputs = (prev.nativeBuildInputs or [ ]) ++ (with common.pkgs; [ python3 gnumake nodejs ]);
        };
      };
      shell = common: prev: {
        packages = prev.packages ++ [
          common.pkgs.musl.dev
          (common.lib.buildCrate {
            memberName = "tokio-console";
            defaultCrateOverrides = (common.lib.removePropagatedEnv common.crateOverrides) // {
              tokio-console = _: {
                CARGO_PKG_REPOSITORY = "https://github.com/tokio-rs/console";
              };
            };
            root = builtins.fetchGit {
              url = "https://github.com/tokio-rs/console.git";
              rev = "3d80c4b68b97db9c20cb496a2e3df0ccc1336b38";
              ref = "main";
            };
          })
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
          {
            name = "run-with-console";
            command = ''RUSTFLAGS="--cfg tokio_unstable" cargo r --features console'';
          }
        ];
      };
    };
  };
}
