# The Java doclet classes as a real Nix package.
#
# `build.rs` compiles these into `$OUT_DIR/classes` for an in-tree cargo
# build. That path is baked into the binary and does not exist inside a
# shipped `lindsey.app`. `NUDOX_JAVA_ORACLE_CLASSES` is the override the
# producer already consults; this derivation is what the cargo-bundle
# wrapper points it at.
{
  pkgs,
  jdk,
  src,
}:

pkgs.stdenv.mkDerivation {
  pname = "nudox-java-oracle";
  version = "0.0.0";

  inherit src;

  nativeBuildInputs = [ jdk ];

  dontConfigure = true;

  buildPhase = ''
    runHook preBuild
    mkdir -p classes
    javac -encoding UTF-8 -d classes *.java
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p "$out"
    cp -R classes/. "$out/"
    # The producer hands this directory to javadoc as `-docletpath`. The
    # package layout (`nudox/oracle/*.class`) must sit at the root.
    if [ ! -e "$out/nudox/oracle/Extractor.class" ]; then
      echo "java oracle install is missing nudox.oracle.Extractor" >&2
      find "$out" -type f >&2
      exit 1
    fi
    runHook postInstall
  '';

  meta = {
    description = "Java doclet classes spawned by the Java producer";
  };
}
