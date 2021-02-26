{ release ? false
, doCheck ? false
, doDoc ? false
, common
,
}:
with common;
let
  meta = with pkgs.stdenv.lib; {
    homepage = "https://github.com/yusdacra/scherzo";
    license = licenses.agpl3;
  };

  package = with pkgs; naersk.buildPackage {
    root = ../.;
    nativeBuildInputs = crateDeps.nativeBuildInputs;
    buildInputs = crateDeps.buildInputs;
    # WORKAROUND because doctests fail to compile (they compile with nightly cargo but then rustdoc fails)
    cargoTestOptions = def: def ++ [ "--lib" "--tests" "--bins" "--examples" ];
    override = (_: {
      PROTOC = "${protobuf}/bin/protoc";
      PROTOC_INCLUDE = "${protobuf}/include";
    });
    overrideMain = (prev: {
      inherit meta;
      nativeBuildInputs = prev.nativeBuildInputs ++ [ rustfmt ];
    });
    inherit release doCheck doDoc;
  };
in
package
