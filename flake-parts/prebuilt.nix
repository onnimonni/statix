let
  # Release binaries from https://github.com/onnimonni/statix/releases.
  # `prebuilt.json` is rewritten by the `prebuilt-hashes` job in
  # `.github/workflows/release.yaml` after each `v*` release.
  inherit (builtins.fromJSON (builtins.readFile ./prebuilt.json)) version hashes;
  # Linux binaries are static musl, so no autoPatchelfHook is needed
  targets = {
    x86_64-linux = "x86_64-unknown-linux-musl";
    aarch64-linux = "aarch64-unknown-linux-musl";
    x86_64-darwin = "x86_64-apple-darwin";
    aarch64-darwin = "aarch64-apple-darwin";
  };
in
{
  perSystem =
    { pkgs, system, ... }:
    let
      target = targets.${system};
    in
    {
      # `nix run github:onnimonni/statix#prebuilt`: no compiling and no cachix needed
      packages.prebuilt = pkgs.stdenvNoCC.mkDerivation {
        pname = "statix-prebuilt";
        inherit version;
        src = pkgs.fetchurl {
          url = "https://github.com/onnimonni/statix/releases/download/v${version}/statix-${target}.tar.gz";
          hash = hashes.${target};
        };
        sourceRoot = ".";
        nativeBuildInputs = [ pkgs.installShellFiles ];
        dontConfigure = true;
        dontBuild = true;
        installPhase = ''
          runHook preInstall
          installBin statix
          runHook postInstall
        '';
        meta = {
          description = "Lints and suggestions for the Nix programming language (prebuilt release binary)";
          homepage = "https://github.com/onnimonni/statix";
          license = pkgs.lib.licenses.mit;
          mainProgram = "statix";
          platforms = builtins.attrNames targets;
          sourceProvenance = [ pkgs.lib.sourceTypes.binaryNativeCode ];
        };
      };
    };
}
