#!/usr/bin/env bash
#
# Free self-signing for macOS so system-audio capture actually works.
#
# WHY THIS EXISTS
#   A Core Audio process tap (how we record the other participants' sound on
#   macOS 14.2+) only receives real audio if the app has a STABLE code-signing
#   identity that macOS can attach the "System Audio Recording" consent to.
#   An unsigned or ad-hoc-signed build gets a tap that silently returns all
#   zeros — the recording succeeds but the far end is missing. A **self-signed
#   certificate** is a free, stable identity: enough to prove capture works on
#   your own machine (and Ian's). It is NOT a substitute for a paid Apple
#   Developer ID when distributing to other people — a self-signed cert isn't
#   trusted on machines that don't hold it. See README "For developers →
#   macOS: recording system audio from a self-signed build".
#
# WHAT IT DOES
#   1. Ensures a self-signed code-signing identity exists (creates one, free,
#      if absent — override with SIGN_IDENTITY to use your own cert or a real
#      Developer ID).
#   2. Signs the app bundle with it (hardened runtime, stable identity).
#   3. Verifies the signature and prints the one-time consent step.
#
# USAGE
#   bash scripts/macos-selfsign.sh                      # signs the built bundle
#   bash scripts/macos-selfsign.sh "/Applications/Fly on the Wall.app"
#   SIGN_IDENTITY="Developer ID Application: You (TEAMID)" bash scripts/macos-selfsign.sh
#
set -euo pipefail

if [[ "$(uname)" != "Darwin" ]]; then
  echo "This script only runs on macOS (it uses codesign/security)." >&2
  exit 1
fi

# --- locate the app bundle -------------------------------------------------
APP_PATH="${1:-}"
if [[ -z "$APP_PATH" ]]; then
  # Default to the bundle `tauri build` produces.
  for cand in \
    "target/release/bundle/macos/Fly on the Wall.app" \
    "src-tauri/target/release/bundle/macos/Fly on the Wall.app"; do
    if [[ -d "$cand" ]]; then APP_PATH="$cand"; break; fi
  done
fi
if [[ -z "$APP_PATH" || ! -d "$APP_PATH" ]]; then
  echo "Could not find the app bundle. Build it first (npm run tauri build)," >&2
  echo "or pass the path explicitly, e.g.:" >&2
  echo "  bash scripts/macos-selfsign.sh \"/Applications/Fly on the Wall.app\"" >&2
  exit 1
fi

CERT_NAME="${SIGN_CERT_NAME:-Fly on the Wall Dev (self-signed)}"
IDENTITY="${SIGN_IDENTITY:-}"

# --- ensure a signing identity exists --------------------------------------
if [[ -z "$IDENTITY" ]]; then
  if security find-identity -v -p codesigning | grep -qF "$CERT_NAME"; then
    IDENTITY="$CERT_NAME"
    echo "Using existing self-signed identity: $IDENTITY"
  else
    echo "Creating a free self-signed code-signing certificate: $CERT_NAME"
    workdir="$(mktemp -d)"
    trap 'rm -rf "$workdir"' EXIT
    # A code-signing leaf: digitalSignature + codeSigning EKU is what codesign
    # and TCC require. Self-signed = its own root, so the identity is stable.
    openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
      -keyout "$workdir/key.pem" -out "$workdir/cert.pem" \
      -subj "/CN=$CERT_NAME" \
      -addext "keyUsage=critical,digitalSignature" \
      -addext "extendedKeyUsage=critical,codeSigning" >/dev/null 2>&1
    openssl pkcs12 -export -inkey "$workdir/key.pem" -in "$workdir/cert.pem" \
      -out "$workdir/id.p12" -passout pass: >/dev/null 2>&1
    # Import into the login keychain and let codesign use it without prompting.
    security import "$workdir/id.p12" -k "$HOME/Library/Keychains/login.keychain-db" \
      -P "" -T /usr/bin/codesign >/dev/null
    # Trust the cert for code signing so the signature verifies locally. This
    # needs admin (sudo prompt); if it fails, signing still works for capture —
    # you'll just also see the Gatekeeper "unknown developer" prompt on launch.
    if ! sudo security add-trusted-cert -d -r trustRoot -p codeSign \
        -k /Library/Keychains/System.keychain "$workdir/cert.pem" >/dev/null 2>&1; then
      echo "  (could not add system trust — continuing; capture still works," >&2
      echo "   you'll just get the usual first-launch Gatekeeper prompt.)" >&2
    fi
    IDENTITY="$CERT_NAME"
  fi
fi

# --- sign ------------------------------------------------------------------
echo "Signing: $APP_PATH"
echo "     as: $IDENTITY"
# --force replaces the release build's (absent/ad-hoc) signature; --options
# runtime gives a hardened, stable identity; --deep covers the bundled
# sidecars (whisper, sherpa, ollama, ffmpeg).
codesign --force --deep --options runtime --sign "$IDENTITY" "$APP_PATH"
codesign --verify --deep --strict --verbose=2 "$APP_PATH"

cat <<EOF

Signed. To capture the other participants' audio:
  1. Launch the app (right-click → Open the first time if Gatekeeper warns).
  2. Start a recording; macOS will ask to allow System Audio Recording — Allow.
     (Change later under System Settings → Privacy & Security →
      Screen & System Audio Recording.)
  3. The recording bar warns if the tap is silent, so you'll know immediately
     whether real system audio is being captured.
EOF
