{
  inputs = {
    nixCargoIntegration = {
      url = "github:yusdacra/nix-cargo-integration";
    };
  };

  outputs = inputs: inputs.nixCargoIntegration.lib.makeOutputs {
    root = ./.;
    buildPlatform = "crate2nix";
  };
}
