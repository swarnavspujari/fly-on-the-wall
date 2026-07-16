#!/usr/bin/env bash
#
# Build a self-contained whisper.cpp `whisper-cli` for macOS or Linux and
# package it exactly the way `models.rs` expects a managed tool artifact —
# then print the SHA-256, byte size, and a ready-to-paste `Artifact { .. }`.
#
# Why this exists: upstream whisper.cpp publishes binaries only for Windows.
# macOS and Linux otherwise fall back to a `whisper-cli` on PATH (see
# `models::ensure_tool`), which most users don't have — the usability cliff
# this whole feature fixes. We build the same pinned version the Windows
# artifact uses and host the archive as a tools release on the fork, so
# `ensure_tool` auto-downloads it on first transcribe just like on Windows.
#
# Usage:
#   scripts/build-whisper-sidecar.sh            # auto-detect this OS
#   scripts/build-whisper-sidecar.sh macos      # force target
#   scripts/build-whisper-sidecar.sh linux
#
# Requirements: git, cmake (>=3.14), a C/C++ toolchain. macOS also needs the
# Xcode command line tools (for Metal). Output lands in ./dist/.
#
# The build is STATIC (BUILD_SHARED_LIBS=OFF) so the archive is a single
# self-contained binary — no dylibs to bundle, no rpath surprises. macOS
# embeds the Metal shader library into the binary; Linux is a portable CPU
# build (GGML_NATIVE=OFF so it runs on machines other than the builder's).

set -euo pipefail

# Pinned to match the Windows whisper artifact in src-tauri/src/models.rs, so
# transcription behaves identically across platforms. Bump both together.
WHISPER_TAG="v1.9.1"
REPO="https://github.com/ggml-org/whisper.cpp.git"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="${ROOT}/.whisper-build"
DIST="${ROOT}/dist"
SRC="${WORK}/whisper.cpp"

target="${1:-}"
if [[ -z "${target}" ]]; then
  case "$(uname -s)" in
    Darwin) target="macos" ;;
    Linux)  target="linux" ;;
    *) echo "unsupported OS: $(uname -s) — pass 'macos' or 'linux'" >&2; exit 1 ;;
  esac
fi

for tool in git cmake; do
  command -v "${tool}" >/dev/null 2>&1 || {
    echo "error: '${tool}' is required but not installed." >&2
    [[ "${tool}" == "cmake" ]] && echo "  macOS: 'brew install cmake' or download from https://cmake.org/download/" >&2
    exit 1
  }
done

echo ">> target=${target}  whisper.cpp=${WHISPER_TAG}"
mkdir -p "${WORK}" "${DIST}"

# Clone (shallow, pinned) or reuse an existing checkout at the right tag.
if [[ ! -d "${SRC}/.git" ]]; then
  git clone --depth 1 --branch "${WHISPER_TAG}" "${REPO}" "${SRC}"
else
  git -C "${SRC}" fetch --depth 1 origin "${WHISPER_TAG}"
  git -C "${SRC}" checkout -q "${WHISPER_TAG}"
fi

# Common cmake flags: static libs, only the CLI (no server/tests/examples we
# don't ship), Release. This yields build/bin/whisper-cli.
CMAKE_FLAGS=(
  -DCMAKE_BUILD_TYPE=Release
  -DBUILD_SHARED_LIBS=OFF
  -DWHISPER_BUILD_TESTS=OFF
  -DWHISPER_BUILD_SERVER=OFF
  -DWHISPER_BUILD_EXAMPLES=ON   # whisper-cli lives under examples/
)

case "${target}" in
  macos)
    # Universal2 so one archive covers Intel + Apple Silicon (like sherpa's
    # osx-universal2 build). Metal is on by default; embed its shader library
    # so the binary needs no sidecar .metal file at runtime.
    CMAKE_FLAGS+=(
      -DCMAKE_OSX_ARCHITECTURES="arm64;x86_64"
      -DGGML_METAL=ON
      -DGGML_METAL_EMBED_LIBRARY=ON
    )
    ASSET="whisper-bin-macos-universal2-${WHISPER_TAG}.tar.bz2"
    ;;
  linux)
    # GGML_NATIVE=OFF avoids -march=native so the binary runs on CPUs other
    # than the build machine's (portability > a few % speed).
    CMAKE_FLAGS+=(
      -DGGML_NATIVE=OFF
    )
    ASSET="whisper-bin-linux-x64-${WHISPER_TAG}.tar.bz2"
    ;;
  *) echo "unknown target '${target}'" >&2; exit 1 ;;
esac

rm -rf "${SRC}/build"
cmake -S "${SRC}" -B "${SRC}/build" "${CMAKE_FLAGS[@]}"
cmake --build "${SRC}/build" --config Release --target whisper-cli -j

BIN="${SRC}/build/bin/whisper-cli"
[[ -f "${BIN}" ]] || { echo "error: whisper-cli not found at ${BIN}" >&2; exit 1; }

# Sanity: show what the binary dynamically links. It should reference only
# system libraries/frameworks — anything else means we'd have to bundle it.
echo ">> dynamic dependencies (expect only system libs):"
if [[ "${target}" == "macos" ]]; then
  otool -L "${BIN}" || true
  echo ">> architectures:"; lipo -info "${BIN}" || true
else
  ldd "${BIN}" || true
fi

# Package FLAT: the archive contains just `whisper-cli`, so it extracts to
# <dest_rel>/whisper-cli — that path is the artifact's probe_rel.
STAGE="${WORK}/stage"
rm -rf "${STAGE}"; mkdir -p "${STAGE}"
cp "${BIN}" "${STAGE}/whisper-cli"
chmod +x "${STAGE}/whisper-cli"

OUT="${DIST}/${ASSET}"
rm -f "${OUT}"
# Deterministic-ish tar (sorted, no owner metadata) so rebuilds match.
tar --numeric-owner --owner=0 --group=0 -cjf "${OUT}" -C "${STAGE}" whisper-cli

# Emit the pins.
if command -v sha256sum >/dev/null 2>&1; then
  SHA="$(sha256sum "${OUT}" | awk '{print $1}')"
else
  SHA="$(shasum -a 256 "${OUT}" | awk '{print $1}')"
fi
BYTES="$(wc -c < "${OUT}" | tr -d ' ')"

echo
echo "=================================================================="
echo " Built: ${OUT}"
echo " SHA-256: ${SHA}"
echo " Bytes:   ${BYTES}"
echo "=================================================================="
echo
echo " Next steps:"
echo "  1. Upload ${ASSET} to a GitHub release on the fork,"
echo "     tag: tools-whisper-${WHISPER_TAG}"
echo "  2. Paste this into the matching TOOLS array in src-tauri/src/models.rs:"
echo
cat <<EOF
    Artifact {
        id: "whisper-bin",
        display: "whisper.cpp CLI (${target}, ${WHISPER_TAG})",
        url: "https://github.com/<owner>/fly-on-the-wall/releases/download/tools-whisper-${WHISPER_TAG}/${ASSET}",
        sha256: "${SHA}",
        bytes: ${BYTES},
        kind: ArtifactKind::Archive,
        dest_rel: "bin/whisper",
        probe_rel: "bin/whisper/whisper-cli",
    },
EOF
