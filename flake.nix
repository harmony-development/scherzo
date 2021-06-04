{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nixCargoIntegration = {
      url = "github:yusdacra/nix-cargo-integration";
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
      pkgs = common: prev: {
        overlays = [
          (final: prev: {
            llvmPackages_12 = prev.llvmPackages_12 // {
              clang = prev.lib.hiPrio prev.llvmPackages_12.clang;
              bintools = prev.lib.setPrio (-20) prev.llvmPackages_12.bintools;
            };
          })
        ] ++ prev.overlays;
      };
      shell = common: prev: {
        packages = prev.packages ++ [ common.pkgs.mkcert ];
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
