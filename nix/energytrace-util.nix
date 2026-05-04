{ lib
, stdenv
, fetchFromGitHub
, mspds-bin
}:

stdenv.mkDerivation {
  pname = "energytrace-util";
  version = "unstable-2021-10-24";

  src = fetchFromGitHub {
    owner = "carrotIndustries";
    repo = "energytrace-util";
    rev = "d22c86b45e70f9bbc905e7b1fc52a4576e3f61a5";
    hash = "sha256-507YJxSkJwuTGc8IjgtOH2hPNB+/b5xiezisfTSAZRc=";
  };

  buildInputs = [ mspds-bin ];

  buildPhase = ''
    runHook preBuild
    $CC -I${mspds-bin}/include -L${mspds-bin}/lib -lmsp430 -o energytrace energytrace.c
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    install -Dm0755 -t $out/bin energytrace
    runHook postInstall
  '';

  meta = with lib; {
    description = "Utility for reading EnergyTrace data from TI debug hardware";
    homepage = "https://github.com/carrotIndustries/energytrace-util";
    license = licenses.gpl3Only;
    maintainers = [ ];
    platforms = platforms.linux;
  };
}
