{ sources, system }:
let
  pkgz = import sources.nixpkgs { inherit system; overlays = [ sources.rustOverlay.overlay ]; };
  rust = pkgz.rust-bin."stable".latest.rust.override {
    extensions = [ "rust-src" ];
  };

  pkgs = import sources.nixpkgs {
    inherit system;
    overlays = [
      sources.rustOverlay.overlay
      sources.devshell.overlay
      (final: prev: {
        rustc = rust;
      })
      (final: prev: {
        naersk = prev.callPackage sources.naersk { };
      })
    ];
  };
in
{
  inherit pkgs;

  # Dependencies listed here will be passed to Nix build and development shell
  crateDeps =
    with pkgs;
    {
      buildInputs = [ protobuf binutils ];
      nativeBuildInputs = [ /* Add compile time dependencies here */ ];
    };

  env = with pkgs;
    let
      nv = lib.nameValuePair;
    in
    [
      (nv "PROTOC" "${protobuf}/bin/protoc")
      (nv "PROTOC_INCLUDE" "${protobuf}/include")
    ];
}
