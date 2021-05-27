{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nixCargoIntegration = {
      url = "github:yusdacra/nix-cargo-integration";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs: inputs.nixCargoIntegration.lib.makeOutputs {
    root = ./.;
    buildPlatform = "crate2nix";
    overrides = {
      shell = common: prev:
        let
          sledtool = common.lib.buildCrate {
            root = common.pkgs.stdenv.mkDerivation {
              name = "patch-src";
              src = builtins.fetchGit {
                url = "https://github.com/vi/sledtool.git";
                ref = "main";
                rev = "a872f66a32dc2ff4cc4eb056b56d043cdfa53b30";
              };
              patches = [ ./nix/sledtool.patch ];
              installPhase = "cp -r . $out";
            };
          };
        in
        {
          packages = prev.packages ++ [ common.pkgs.mkcert sledtool ];
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
