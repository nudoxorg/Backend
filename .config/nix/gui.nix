# Materializes the GUI test contract and its hermetic native capture closure.
# Keeps display, fonts, media capture, and lock tooling pinned to nixpkgs so a
# GUI lane never inherits a developer's host display or font search path.
{
  pkgs,
  control,
  workspaceRoot ? null,
  resolvedSourceHashes ? { },
}:
let
  workspaceAvailable = workspaceRoot != null && builtins.pathExists (workspaceRoot + "/Cargo.lock");
  workspaceManifest =
    if workspaceAvailable && builtins.pathExists (workspaceRoot + "/Cargo.toml") then
      builtins.readFile (workspaceRoot + "/Cargo.toml")
    else
      "unavailable";
  cargoLock =
    if workspaceAvailable then builtins.readFile (workspaceRoot + "/Cargo.lock") else "unavailable";
  cargoLockBlocks = pkgs.lib.splitString "\n\n" cargoLock;
  blockLines = block: pkgs.lib.splitString "\n" block;
  gpuiPackageBlocks = builtins.filter (
    block: builtins.any (line: builtins.match "name = \"gpui[^\"]*\"" line != null) (blockLines block)
  ) cargoLockBlocks;
  componentBlocks = builtins.filter (
    block:
    builtins.any (line: builtins.match "name = \"gpui_ce_components\"" line != null) (blockLines block)
  ) cargoLockBlocks;
  componentChecksumLines =
    if builtins.length componentBlocks == 1 then
      builtins.filter (line: builtins.match "checksum = \"[^\"]+\"" line != null) (
        blockLines (builtins.head componentBlocks)
      )
    else
      [ ];
  vendoredComponentPath =
    if workspaceAvailable && builtins.pathExists (workspaceRoot + "/vendor/gpui_ce_components") then
      builtins.path {
        path = workspaceRoot + "/vendor/gpui_ce_components";
        name = "nudox-gpui-ce-components-source";
      }
    else
      null;
  componentSourceIdentity =
    if builtins.length componentChecksumLines == 1 then
      builtins.head componentChecksumLines
    else if vendoredComponentPath != null then
      "vendored-source = \"${toString vendoredComponentPath}\""
    else
      null;
  packageProofLines = builtins.concatLists (
    map (
      block:
      let
        lines = blockLines block;
        nameLine = builtins.head (
          builtins.filter (line: builtins.match "name = \"gpui[^\"]*\"" line != null) lines
        );
        versionLine = builtins.head (
          builtins.filter (line: builtins.match "version = \"[^\"]+\"" line != null) lines
        );
        sourceLine =
          let
            values = builtins.filter (line: builtins.match "source = \"[^\"]+\"" line != null) lines;
          in
          if values == [ ] then "source = \"workspace\"" else builtins.head values;
        name = builtins.elemAt (builtins.match "name = \"([^\"]+)\"" nameLine) 0;
        version = builtins.elemAt (builtins.match "version = \"([^\"]+)\"" versionLine) 0;
        key = "${name}-${version}";
        checksumValues = builtins.filter (line: builtins.match "checksum = \"[^\"]+\"" line != null) lines;
        checksumLine = if checksumValues == [ ] then "" else builtins.head checksumValues;
        checksum =
          if checksumLine == "" then
            "none"
          else
            builtins.elemAt (builtins.match "checksum = \"([^\"]+)\"" checksumLine) 0;
        sourceHash =
          if builtins.hasAttr key resolvedSourceHashes then
            "nix-output-hash=${builtins.getAttr key resolvedSourceHashes}"
          else
            "cargo-checksum=${checksum}";
      in
      [ "package=${name}@${version};${sourceLine};${sourceHash}" ]
    ) gpuiPackageBlocks
  );
  gpuiLockLines = builtins.filter (
    line: builtins.match ".*(gpui|GPUI|gpui-ce-component).*" line != null
  ) (pkgs.lib.splitString "\n" cargoLock);
  resolvedSourceLines = map (name: "resolved-source/${name}=${resolvedSourceHashes.${name}}") (
    builtins.attrNames resolvedSourceHashes
  );
  strictComponentContract =
    if workspaceAvailable && builtins.length (builtins.attrNames resolvedSourceHashes) > 0 then
      assert builtins.length componentBlocks == 1;
      assert componentSourceIdentity != null;
      true
    else
      true;
  componentSourceLines =
    if builtins.length componentBlocks == 1 then blockLines (builtins.head componentBlocks) else [ ];
  componentSourceProofLines =
    componentSourceLines ++ pkgs.lib.optional (componentSourceIdentity != null) componentSourceIdentity;
  gpuiResolvedSourceLines = builtins.filter (
    line: builtins.match "resolved-source/gpui.*" line != null
  ) resolvedSourceLines;
  gpuiSourceProofLines = packageProofLines ++ gpuiResolvedSourceLines;
  gpuiSourceDigest = builtins.hashString "sha256" (
    builtins.concatStringsSep "\n" gpuiSourceProofLines
  );
  gpuiComponentSourceDigest =
    if builtins.length componentBlocks == 1 && componentSourceIdentity != null then
      builtins.hashString "sha256" (builtins.concatStringsSep "\n" componentSourceProofLines)
    else
      null;
  dependencyGraphDigest = builtins.hashString "sha256" (
    cargoLock + builtins.concatStringsSep "\n" gpuiSourceProofLines
  );
  gpuiSourceManifest = pkgs.writeText "nudox-gui-gpui-source-manifest.txt" (
    assert strictComponentContract;
    builtins.concatStringsSep "\n" (
      [
        "policy=single-pinned-type-universe"
        "workspace-cargo-toml-sha256=${builtins.hashString "sha256" workspaceManifest}"
        "cargo-lock-sha256=${builtins.hashString "sha256" cargoLock}"
        "gpui-source-proof-sha256=${gpuiSourceDigest}"
        "gpui-component-source-sha256=${
          if gpuiComponentSourceDigest == null then "unresolved" else gpuiComponentSourceDigest
        }"
        "dependency-graph-sha256=${dependencyGraphDigest}"
        "gpui-package-proof-sha256=${builtins.hashString "sha256" (builtins.concatStringsSep "\n" packageProofLines)}"
      ]
      ++ packageProofLines
      ++ componentSourceProofLines
      ++ gpuiResolvedSourceLines
      ++ gpuiLockLines
    )
  );
  fontPackages = [
    pkgs.dejavu_fonts
    pkgs.liberation_ttf
    pkgs.noto-fonts
    pkgs.noto-fonts-color-emoji
  ];
  linuxDisplayPackages = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
    pkgs.cage
    pkgs.dbus
    pkgs.fontconfig
    pkgs.grim
    pkgs.weston
    pkgs.xvfb
  ];
  linuxGpuPackages = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
    pkgs.gawk
    pkgs.gnugrep
    pkgs.mesa-demos
    pkgs.vulkan-tools
  ];
  capturePackages = [
    pkgs.coreutils
    pkgs.ffmpeg
    pkgs.imagemagick
    pkgs.jq
    pkgs.pngcheck
    pkgs.procps
    pkgs.util-linux
  ];
  lockPackages = [
    pkgs.coreutils
    pkgs.git
    pkgs.jq
    pkgs.util-linux
  ];
  gpuProbe = pkgs.writeShellScriptBin "nudox-gui-gpu-probe" (
    if pkgs.stdenv.hostPlatform.isLinux then
      ''
        set -eu
        renderer="$(${pkgs.mesa-demos}/bin/glxinfo -B 2>/dev/null | ${pkgs.gawk}/bin/awk -F: '/OpenGL renderer string/ {sub(/^[[:space:]]+/, "", $2); print $2; exit}')"
        if [ -n "$renderer" ]; then
          if printf '%s' "$renderer" | ${pkgs.gnugrep}/bin/grep -Eiq 'llvmpipe|softpipe|swiftshader'; then
            backend=software-pinned
          else
            backend=opengl-hardware
          fi
          ${pkgs.jq}/bin/jq -cn --arg backend "$backend" --arg device "$renderer" '{backend:$backend,device:$device}'
          exit 0
        fi
        device="$(${pkgs.vulkan-tools}/bin/vulkaninfo --summary 2>/dev/null | ${pkgs.gawk}/bin/awk -F= '/deviceName/ {gsub(/^ +| +$/, "", $2); print $2; exit}')"
        if [ -z "$device" ]; then
          echo "no pinned GPUI GPU backend/device was detectable" >&2
          exit 78
        fi
        if printf '%s' "$device" | ${pkgs.gnugrep}/bin/grep -Eiq 'llvmpipe|softpipe|swiftshader'; then
          backend=software-pinned
        else
          backend=vulkan-hardware
        fi
        ${pkgs.jq}/bin/jq -cn --arg backend "$backend" --arg device "$device" '{backend:$backend,device:$device}'
      ''
    else
      ''
        echo "native GPU probing is required from the real quartz GPUI driver on this host" >&2
        exit 78
      ''
  );
  allPackages =
    fontPackages ++ linuxDisplayPackages ++ linuxGpuPackages ++ capturePackages ++ lockPackages;
  toolsBundle = pkgs.buildEnv {
    name = "nudox-gui-tools";
    paths = allPackages ++ [
      fontManifest
      gpuProbe
    ];
    pathsToLink = [
      "/bin"
      "/share"
    ];
  };
  configFile = pkgs.writeTextFile {
    name = "nudox-gui-control-plane";
    destination = "/share/nudox/gui-control-plane.json";
    text = builtins.toJSON control.gui;
  };
  fontConfig = pkgs.makeFontsConf { fontDirectories = fontPackages; };
  fontManifest =
    pkgs.runCommand "nudox-gui-font-manifest"
      {
        nativeBuildInputs = [
          pkgs.coreutils
          pkgs.findutils
        ];
      }
      ''
        set -eu
        mkdir -p "$out/share/nudox"
        files=$(find ${
          pkgs.lib.concatStringsSep " " (map (font: "${font}/share/fonts") fontPackages)
        } -type f -print)
        if [ -z "$files" ]; then
          echo "GUI font closure contains no font files" >&2
          exit 1
        fi
        printf '%s\n' "$files" | sort | xargs sha256sum > "$out/share/nudox/fonts.sha256"
      '';
in
{
  inherit
    allPackages
    configFile
    dependencyGraphDigest
    fontConfig
    fontManifest
    fontPackages
    gpuProbe
    gpuiComponentSourceDigest
    gpuiSourceDigest
    gpuiSourceManifest
    toolsBundle
    ;
  displayPackages = linuxDisplayPackages;
  capturePackages = capturePackages;
  lockPackages = lockPackages;
}
