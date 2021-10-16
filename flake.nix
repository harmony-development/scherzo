{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nixCargoIntegration = {
      url = "github:yusdacra/nix-cargo-integration/fix/devshell-attrs";
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
        packages = prev.packages ++ [ common.pkgs.musl.dev ];
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
