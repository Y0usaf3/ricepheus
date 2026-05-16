{
  lib,
  rustPlatform,
  fetchFromGitHub,
  nix-update-script,
}:
rustPlatform.buildRustPackage (finalAttrs: {
  pname = "ricepheus";
  version = "0.0-unstable";
  __structuredAttrs = true;

  src = fetchFromGitHub {
    owner = "Y0usaf3";
    repo = "ricepheus";
    rev = "08b7e4df151114fb9e6c24f08334f98572a224be";
    hash = "sha256-U7LL0k3r/JNbSDXK63jsqDp06zuFg8s3yDGIc6Vz2/c=";
  };

  cargoHash = "sha256-twQQcK9ZuqCeeXUFLfn3TEJYKmjwp13VFIF/8lv3Cwo=";

  passthru.updateScript = nix-update-script {};

  meta = {
    description = "";
    homepage = "https://github.com/Y0usaf3/ricepheus";
    license = lib.licenses.mit; # FIXME: nix-init did not find a license
    maintainers = with lib.maintainers; [];
    mainProgram = "ricepheus";
  };
})
