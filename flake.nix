{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    naersk = {
      url = "github:yusdacra/naersk/feat/cargolock-git-deps";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rustOverlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nixCargoIntegration = {
      url = "github:yusdacra/nix-cargo-integration";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.naersk.follows = "naersk";
      inputs.rustOverlay.follows = "rustOverlay";
    };
  };

  outputs = inputs: inputs.nixCargoIntegration.lib.makeOutputs {
    root = ./.;
    overrides = {
      build = common: prevb: {
        allRefs = true;
        gitSubmodules = true;
        nativeBuildInputs = prevb.nativeBuildInputs ++ [ common.pkgs.rustfmt ];
      };
    };
  };
}
