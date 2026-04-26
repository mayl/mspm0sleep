{ lib
, stdenv
, fetchurl
, writeShellScript
, unzip
, patchelf
, buildFHSEnv
, bash
, coreutils
, gawk
, gnused
, gnugrep
, which
, gtk3
, glib
, nss
, nspr
, atk
, at-spi2-atk
, at-spi2-core
, cairo
, pango
, cups
, dbus
, expat
, libdrm
, libxkbcommon
, mesa
, libgbm
, alsa-lib
, xorg
, libnotify
, libsecret
, libxshmfence
, libusb1
, systemd
, zlib
, libGL
}:

let
  version = "1.5.1.00003";
  installFile = "CCSTheia${version}_linux-x64.zip";
  topDir = "CCSTheia${version}_linux-x64";

  # Minimal env for the BitRock installer. Its bundled scripts have
  # #!/bin/bash shebangs that don't resolve on NixOS; the installer
  # itself doesn't need GUI/Electron libs. Keeping this list tight
  # means iterating on runtime libs below doesn't trigger a full
  # 5-minute reinstall.
  installerTargetPkgs = pkgs: (with pkgs; [
    bash coreutils gawk gnused gnugrep which
    zlib stdenv.cc.cc.lib
  ]);

  installerFHS = buildFHSEnv {
    name = "ccs-theia-install-env";
    targetPkgs = installerTargetPkgs;
    runScript = "bash";
  };

  # Runtime env for the launcher: everything Electron / Chromium /
  # bundled JRE / Node / dslite / libusb need.
  runtimeTargetPkgs = pkgs: (with pkgs; [
    bash coreutils
    gtk3 glib nss nspr atk at-spi2-atk at-spi2-core
    cairo pango cups dbus expat libdrm libxkbcommon mesa libgbm alsa-lib
    libnotify libsecret libxshmfence libGL
    xorg.libX11 xorg.libXcomposite xorg.libXdamage xorg.libXext
    xorg.libXfixes xorg.libXrandr xorg.libxcb xorg.libXtst
    xorg.libXScrnSaver xorg.libXi xorg.libXcursor xorg.libXrender
    zlib stdenv.cc.cc.lib
    libusb1 systemd
  ]);

  unwrapped = stdenv.mkDerivation {
    pname = "ccs-theia-unwrapped";
    inherit version;

    src = fetchurl {
      url = "https://dr-download.ti.com/software-development/ide-configuration-compiler-or-debugger/MD-63JH5Zr6eq/1.5.1/${installFile}";
      hash = "sha256-+qwpetmm19QBwZdYe6bfP81Tdhe6ygclbsR8plhEloI=";
    };

    nativeBuildInputs = [ unzip patchelf ];

    unpackPhase = ''
      runHook preUnpack
      unzip -q $src
      runHook postUnpack
    '';

    dontConfigure = true;
    dontBuild = true;

    # Patch the installer's interpreter (same trick as ti_c2000_nix),
    # then run it inside an FHS env so /bin/bash etc. resolve.
    #
    # The bundled BLACKHAWK_EMUPACK sub-installer is mandatory and runs
    # bh_driver_install.sh as a post-install step; that script bails
    # with "must be run as root" and propagates a fatal exit. We race
    # the installer: watch for the file to appear under $out and
    # overwrite it with a no-op before bh_emupack invokes it. The
    # other root-required scripts (ti_permissions_install.sh, etc.)
    # also fail but their failures are non-fatal to the parent.
    installPhase = ''
      runHook preInstall
      cd ${topDir}
      patchelf --set-interpreter ${stdenv.cc.bintools.dynamicLinker} \
        ./ccs_theia_setup_${version}.run

      mkdir -p $out
      bhScript="$out/ccs/ccs_base/emulation/Blackhawk/Install/bh_driver_install.sh"
      (
        for _ in $(seq 1 6000); do
          if [ -f "$bhScript" ]; then
            printf '#!/bin/sh\nexit 0\n' > "$bhScript"
            break
          fi
          sleep 0.05
        done
      ) &
      watcherPid=$!

      ${installerFHS}/bin/ccs-theia-install-env -c "
        ./ccs_theia_setup_${version}.run \
          --mode unattended \
          --unattendedmodeui none \
          --prefix $out \
          --enable-components PF_MSPM0
      "
      kill "$watcherPid" 2>/dev/null || true
      wait "$watcherPid" 2>/dev/null || true

      runHook postInstall
    '';

    # TI binaries are pre-built and self-contained.
    dontStrip = true;
    dontPatchELF = true;
    dontFixup = true;

    meta = with lib; {
      description = "TI Code Composer Studio Theia (MSPM0 only) — installed tree";
      homepage = "https://www.ti.com/tool/CCSTUDIO-THEIA";
      license = licenses.unfree;
      platforms = [ "x86_64-linux" ];
    };
  };

in
buildFHSEnv {
  name = "ccs-theia";

  targetPkgs = runtimeTargetPkgs;

  runScript = writeShellScript "ccs-theia-launch" ''
    exec ${unwrapped}/ccs/theia/ccstudio "$@"
  '';

  meta = with lib; {
    description = "TI Code Composer Studio Theia (MSPM0 only) — FHS-wrapped launcher";
    homepage = "https://www.ti.com/tool/CCSTUDIO-THEIA";
    license = licenses.unfree;
    platforms = [ "x86_64-linux" ];
    mainProgram = "ccs-theia";
  };
}
