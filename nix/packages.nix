# All packages of the flake. `default` = bar + every plugin, so all plugins land on $PATH.
{ pkgs, src, py-analogue-clock-src }:
let
  lib = pkgs.lib;
  python = pkgs.python314 or pkgs.python313;
  ps = python.pkgs;

  py-analogue-clock = ps.buildPythonPackage {
    pname = "py-analogue-clock";
    version = "0.4.0";
    src = py-analogue-clock-src;
    format = "pyproject";
    # Upstream pyproject.toml has no [build-system]; add one (same trick as hypr-analogue).
    postPatch = ''
      cat >> pyproject.toml <<'TOML'

      [build-system]
      requires = ["setuptools>=61", "wheel"]
      build-backend = "setuptools.build_meta"

      [tool.setuptools.packages.find]
      include = ["analogue_clock*"]
      TOML
    '';
    nativeBuildInputs = [ ps.setuptools ps.wheel ];
    propagatedBuildInputs = [ ps.svgwrite ps.typer ];
    doCheck = false;
    pythonImportsCheck = [ "analogue_clock" ];
  };

  python-env = python.withPackages (p: [ py-analogue-clock p.pytest ]);

  # GTK4 + the wlr-layer-shell binding used by the bar.
  runtime-libs = with pkgs; [ gtk4 gtk4-layer-shell ];

  version = (lib.importTOML (src + "/Cargo.toml")).workspace.package.version;

  # Only what the builds need, so edits to docs or the NixOS module don't trigger rebuilds.
  buildSrc = lib.fileset.toSource {
    root = src;
    fileset = lib.fileset.unions [
      (src + "/Cargo.toml")
      (src + "/Cargo.lock")
      (src + "/crates")
      (src + "/plugins")
      (src + "/plugin-sdk-python")
      (src + "/systemd")
      (src + "/examples")
    ];
  };

  # One cargo build for the bar and all Rust plugins.
  rust = pkgs.rustPlatform.buildRustPackage {
    pname = "days-of-our-lives";
    inherit version;
    src = buildSrc;
    cargoLock.lockFile = buildSrc + "/Cargo.lock";
    nativeBuildInputs = [ pkgs.pkg-config pkgs.wrapGAppsHook4 ];
    buildInputs = runtime-libs;
    doCheck = true;
    postInstall = ''
      install -Dm644 systemd/days-of-our-lives.service \
        $out/share/systemd/user/days-of-our-lives.service
      substituteInPlace $out/share/systemd/user/days-of-our-lives.service \
        --replace-fail /usr/bin/days-of-our-lives $out/bin/days-of-our-lives
      mkdir -p $out/share/days-of-our-lives
      cp -r examples $out/share/days-of-our-lives/examples
      install -Dm755 plugins/examples/hello.sh $out/share/days-of-our-lives/examples/plugins/dool-plugin-hello
    '';
    meta.mainProgram = "days-of-our-lives";
  };

  plugin-clock = pkgs.stdenvNoCC.mkDerivation {
    pname = "dool-plugin-clock";
    inherit version;
    src = buildSrc;
    nativeBuildInputs = [ pkgs.makeWrapper ];
    dontBuild = true;
    installPhase = ''
      mkdir -p $out/lib/dool $out/bin $out/share/days-of-our-lives/plugins/clock
      cp plugin-sdk-python/dool_plugin.py plugins/clock/clock.py $out/lib/dool/
      cp -r plugins/clock/faces $out/share/days-of-our-lives/plugins/clock/
      makeWrapper ${python-env}/bin/python $out/bin/dool-plugin-clock \
        --add-flags $out/lib/dool/clock.py
      cp plugins/examples/mouse-echo $out/lib/dool/
      makeWrapper ${python-env}/bin/python $out/bin/dool-plugin-mouse-echo \
        --add-flags $out/lib/dool/mouse-echo
    '';
  };
in
{
  inherit python-env runtime-libs plugin-clock;
  bar = rust;
  default = pkgs.symlinkJoin {
    name = "days-of-our-lives-${version}";
    paths = [ rust plugin-clock ];
    meta.mainProgram = "days-of-our-lives";
  };
}
