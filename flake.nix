{
  description = "Neovide (obmutescences fork) — No Nonsense Neovim GUI, built from this source tree";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      # skia 源码：本仓库 Cargo.lock 里 skia-bindings 的版本决定用哪份 skia
      # （0.153.3 → skia-bindings/Cargo.toml [package.metadata] skia = "m153-0.101.2"）。
      # nixpkgs 的 neovide 包写死了它自己 release 对应的 tag（当前 m145-0.92.0），
      # 版本不符时 ninja 找不到 third_party/externals 下的文件（例如 freetype）。
      # 升级 skia-bindings 时：改 tag，并按该 tag 的 skia/DEPS 更新 externals
      # （skia 自带的 freetype/harfbuzz/icu 等源码不在 skia 仓库里，要单独取）。
      skiaSourceDir =
        pkgs:
        let
          repo = pkgs.fetchFromGitHub {
            owner = "rust-skia";
            repo = "skia";
            tag = "m153-0.101.2";
            hash = "sha256-xKCkvQoTAMlLvKdVzzSxTST6kER4kRUIDjvTRZ9z72o=";
          };

          externals = pkgs.linkFarm "skia-externals" (
            map (e: {
              inherit (e) name;
              path = pkgs.fetchgit {
                inherit (e) url rev;
                sha256 = e.hash;
              };
            }) [
              {
                name = "expat";
                url = "https://chromium.googlesource.com/external/github.com/libexpat/libexpat.git";
                rev = "6154446fccefbf3ca644894f598969113b0c7bcd";
                hash = "sha256-ifMDdEWu9H8iJ/VSKuXlisJKx1s1F8ecT74bwXnBWcA=";
              }
              {
                name = "freetype";
                url = "https://chromium.googlesource.com/chromium/src/third_party/freetype2.git";
                rev = "264b5fbf5b912b39f98d038bf75d39be0a73f21b";
                hash = "sha256-RRoTXV063rJBTL/JkVXuRZiNbPZLjtUP/kvt6XybG/k=";
              }
              {
                name = "harfbuzz";
                url = "https://chromium.googlesource.com/external/github.com/harfbuzz/harfbuzz.git";
                rev = "9cb1fee51069b206effb4736e443b038d230789d";
                hash = "sha256-EYItQuWvB4q2Q2iJfMyEtX9MTm3rEFlcwZ+NQEAVy0M=";
              }
              {
                name = "icu";
                url = "https://chromium.googlesource.com/chromium/deps/icu.git";
                rev = "d578f2e8b7bd5938e21cfb6bf15c079e0aa5b738";
                hash = "sha256-DsktUjKFFbeSqlFZTBqbZgvuJLOKvssCym2M3tcOJtw=";
              }
              {
                name = "libjpeg-turbo";
                url = "https://chromium.googlesource.com/chromium/deps/libjpeg_turbo.git";
                rev = "e14cbfaa85529d47f9f55b0f104a579c1061f9ad";
                hash = "sha256-Ig+tmprZDvlf/M72/DTar2pbxat9ZElgSqdXdoM0lPs=";
              }
              {
                name = "libpng";
                url = "https://skia.googlesource.com/third_party/libpng.git";
                rev = "d5515b5b8be3901aac04e5bd8bd5c89f287bcd33";
                hash = "sha256-N507gFyjXXsJ3dyQN0SlVy80WXlbNlZs/cnqLaCmFTk=";
              }
              {
                name = "wuffs";
                url = "https://skia.googlesource.com/external/github.com/google/wuffs-mirror-release-c.git";
                rev = "e3f919ccfe3ef542cfc983a82146070258fb57f8";
                hash = "sha256-373d2F/STcgCHEq+PO+SCHrKVOo6uO1rqqwRN5eeBCw=";
              }
              {
                name = "zlib";
                url = "https://chromium.googlesource.com/chromium/src/third_party/zlib";
                rev = "646b7f569718921d7d4b5b8e22572ff6c76f2596";
                hash = "sha256-jNj6SuTZ5/a7crtYhxW3Q/TlfRMNMfYIVxDlr7bYdzQ=";
              }
            ]
          );
        in
        pkgs.runCommand "skia-source" { } ''
          cp -R ${repo} $out
          chmod -R +w $out
          ln -s ${externals} $out/third_party/externals
        '';

      # 复用 nixpkgs 的 neovide derivation（clangStdenv、图标/desktop、wrapProgram
      # LD_LIBRARY_PATH 都在里面），只换成这份源码、按本仓库 Cargo.lock 取依赖、
      # 并给出与 skia-bindings 版本匹配的 skia 源码。
      mkNeovide =
        pkgs:
        pkgs.neovide.overrideAttrs (old: {
          src = self;
          cargoDeps = pkgs.rustPlatform.importCargoLock {
            lockFile = self + "/Cargo.lock";
          };
          env = old.env // {
            SKIA_SOURCE_DIR = skiaSourceDir pkgs;
          };
          meta = old.meta // {
            description = old.meta.description + " (obmutescences fork)";
            homepage = "https://github.com/obmutescences/neovide";
          };
        });
    in
    {
      # 给 NixOS / home-manager 用：nixpkgs.overlays = [ inputs.neovide.overlays.default ];
      # 这样 pkgs.neovide 就是这份源码编译的二进制。
      overlays.default = _final: prev: {
        neovide = mkNeovide prev;
      };

      packages = nixpkgs.lib.genAttrs systems (system: {
        default = mkNeovide nixpkgs.legacyPackages.${system};
        neovide = mkNeovide nixpkgs.legacyPackages.${system};
      });
    };
}
