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
      shell = common: prev: {
        packages = prev.packages ++ [ common.pkgs.mkcert ];
        commands = prev.commands ++ [
          {
            name = "generate-cert";
            command = "mkcert 127.0.0.1 ::1 localhost";
          }
        ];
      };
    };
  };
}
