# ABOUTME: Selects one minimum macOS version for Rust, C, and Swift builds.
# ABOUTME: Rejects invalid targets before the native build starts.
configure_macos_target() {
  local arch=$1
  case "$arch" in
    arm64|x86_64) ;;
    *) printf 'error: invalid macOS architecture: %s\n' "$arch" >&2; return 2;;
  esac
  export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-26.0}
  if [[ ! "$MACOSX_DEPLOYMENT_TARGET" =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ ]]; then
    printf 'error: invalid macOS deployment version: %s\n' "$MACOSX_DEPLOYMENT_TARGET" >&2
    return 2
  fi
  export swift_target="$arch-apple-macosx$MACOSX_DEPLOYMENT_TARGET"
}
