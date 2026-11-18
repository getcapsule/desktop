{
  description = "Capsule desktop music player";

  inputs.nixpkgs.url = "https://channels.nixos.org/nixos-26.05/nixexprs.tar.xz";

  outputs = { nixpkgs, ... }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      pname = "capsule";
      release = builtins.fromJSON (builtins.readFile ./appimage.json);
      inherit (release) version;
      src = pkgs.fetchurl {
        inherit (release) url hash;
      };
      appimage = pkgs.appimageTools.extractType2 { inherit pname version src; };
    in
    {
      packages.${system}.default = pkgs.appimageTools.wrapType2 {
        inherit pname version src;

        extraInstallCommands = ''
          install -Dm644 ${appimage}/usr/share/applications/capsule.desktop \
            "$out/share/applications/capsule.desktop"
          cp -r ${appimage}/usr/share/icons "$out/share/icons"
        '';

        meta = {
          description = "Desktop music player";
          homepage = "https://github.com/getcapsule/desktop";
          mainProgram = pname;
        };
      };
    };
}
