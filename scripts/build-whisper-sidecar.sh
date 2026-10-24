#!/usr/bin/env bash
#
# Build a self-contained whisper.cpp `whisper-cli` for macOS, Linux or
# Windows (Vulkan GPU build) and package it exactly the way `models.rs`
# expects a managed tool artifact — then print the SHA-256, byte size, and a
# ready-to-paste `Artifact { .. }`.
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
#   scripts/build-whisper-sidecar.sh windows-vulkan   # Git Bash + MSVC + Vulkan SDK
#
# Requirements: git, cmake (>=3.14), a C/C++ toolchain. macOS also needs the
# Xcode command line tools (for Metal). windows-vulkan needs Visual Studio
# (MSVC), the LunarG Vulkan SDK (VULKAN_SDK set, glslc on PATH) and python
# (for packaging). Output lands in ./dist/.
#
# macOS/Linux builds are STATIC (BUILD_SHARED_LIBS=OFF) so the archive is a
# single self-contained binary — no dylibs to bundle, no rpath surprises.
# macOS embeds the Metal shader library into the binary; Linux is a portable
# CPU build (GGML_NATIVE=OFF so it runs on machines other than the builder's).
# windows-vulkan is a SHARED build laid out like upstream's whisper-bin-x64.zip
# (Release/whisper-cli.exe + DLLs), using upstream's own Windows x64 release
# flags plus GGML_VULKAN: the CPU backend ships every x64 variant and the best
# one is picked at runtime (GGML_BACKEND_DL + GGML_CPU_ALL_VARIANTS).

set -euo pipefail

# Pinned to match the Windows CPU whisper artifact in src-tauri/src/models.rs
# (upstream's b5454 build of this same commit), so transcription behaves
# identically across platforms. Bump both together. The COMMIT is the source
# of truth (tags can be moved; commits can't) — v1.9.5's commit (d1be6fd).
# v1.9.5 fixes CVE-2026-110810 (OOB read in whisper_model_load).
# build-whisper-sidecar.yml reads WHISPER_TAG from this line for release tags.
WHISPER_TAG="v1.9.5"
WHISPER_COMMIT="d1be6fde11ac6e0407606b4e42fe72d34add8037"
REPO="https://github.com/ggml-org/whisper.cpp.git"

# Oldest macOS the app supports (tauri.conf.json bundle.macOS
# minimumSystemVersion). Without this the binary inherits the BUILD machine's
# OS as its minimum — a CI runner on macOS 15 would produce a whisper-cli
# that refuses to launch on the very Intel-era Machines this build targets.
MACOS_DEPLOYMENT_TARGET="12.0"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="${ROOT}/.whisper-build"
DIST="${ROOT}/dist"
SRC="${WORK}/whisper.cpp"

target="${1:-}"
if [[ -z "${target}" ]]; then
  case "$(uname -s)" in
    Darwin) target="macos" ;;
    Linux)  target="linux" ;;
    *) echo "unsupported OS: $(uname -s) — pass 'macos', 'linux' or 'windows-vulkan'" >&2; exit 1 ;;
  esac
fi

for tool in git cmake; do
  command -v "${tool}" >/dev/null 2>&1 || {
    echo "error: '${tool}' is required but not installed." >&2
    [[ "${tool}" == "cmake" ]] && echo "  macOS: 'brew install cmake' or download from https://cmake.org/download/" >&2
    exit 1
  }
done

echo ">> target=${target}  whisper.cpp=${WHISPER_TAG} (${WHISPER_COMMIT})"
mkdir -p "${WORK}" "${DIST}"

# Clone (shallow, by tag) or reuse an existing checkout. Either way the build
# proceeds only if HEAD is exactly the pinned COMMIT — a moved/reused tag or a
# stale checkout fails loudly instead of silently building something else.
if [[ ! -d "${SRC}/.git" ]]; then
  git clone --depth 1 --branch "${WHISPER_TAG}" "${REPO}" "${SRC}"
else
  # A shallow tag fetch doesn't create a local tag ref, so check out
  # FETCH_HEAD rather than the tag name (which fails on a reused checkout).
  git -C "${SRC}" fetch --depth 1 origin "refs/tags/${WHISPER_TAG}"
  git -C "${SRC}" checkout -q FETCH_HEAD
fi
HEAD_COMMIT="$(git -C "${SRC}" rev-parse HEAD)"
if [[ "${HEAD_COMMIT}" != "${WHISPER_COMMIT}" ]]; then
  echo "error: checkout is ${HEAD_COMMIT}, expected pinned ${WHISPER_COMMIT}" >&2
  echo "       (tag ${WHISPER_TAG} no longer points at the pinned commit?)" >&2
  exit 1
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
    # Universal so one archive covers Intel + Apple Silicon (like sherpa's
    # osx-universal2 build). Metal is on by default; embed its shader library
    # so the binary needs no sidecar .metal file at runtime. GGML_NATIVE=OFF:
    # -march=native poisons a universal/cross build — the x86_64 slice must
    # run on any Intel Mac, not just CPUs like the build machine's.
    CMAKE_FLAGS+=(
      -DCMAKE_OSX_ARCHITECTURES="x86_64;arm64"
      -DCMAKE_OSX_DEPLOYMENT_TARGET="${MACOS_DEPLOYMENT_TARGET}"
      -DGGML_NATIVE=OFF
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
  windows-vulkan)
    [[ -n "${VULKAN_SDK:-}" ]] || { echo "error: VULKAN_SDK is not set (install the LunarG Vulkan SDK)" >&2; exit 1; }
    command -v python >/dev/null 2>&1 || { echo "error: 'python' is required for packaging" >&2; exit 1; }
    # Shared libs + dynamically loaded backends, exactly like upstream's
    # whisper-bin-x64.zip, with the Vulkan backend added.
    CMAKE_FLAGS=(
      -A x64
      -DCMAKE_BUILD_TYPE=Release
      -DBUILD_SHARED_LIBS=ON
      -DWHISPER_BUILD_TESTS=OFF
      -DWHISPER_BUILD_SERVER=OFF
      -DWHISPER_BUILD_EXAMPLES=ON
      -DGGML_NATIVE=OFF
      -DGGML_BACKEND_DL=ON
      -DGGML_CPU_ALL_VARIANTS=ON
      -DGGML_VULKAN=ON
    )
    ASSET="whisper-bin-x64-vulkan-${WHISPER_TAG}.zip"
    ;;
  *) echo "unknown target '${target}'" >&2; exit 1 ;;
esac

rm -rf "${SRC}/build"
cmake -S "${SRC}" -B "${SRC}/build" "${CMAKE_FLAGS[@]}"

if [[ "${target}" == "windows-vulkan" ]]; then
  # Backends are MODULE libraries under GGML_BACKEND_DL — nothing links them,
  # so a whisper-cli-only build would skip them. Build everything.
  cmake --build "${SRC}/build" --config Release -j
  REL="${SRC}/build/bin/Release"
  [[ -f "${REL}/whisper-cli.exe" ]] || { echo "error: whisper-cli.exe not found in ${REL}" >&2; exit 1; }
  [[ -f "${REL}/ggml-vulkan.dll" ]] || { echo "error: ggml-vulkan.dll not built — Vulkan backend missing" >&2; exit 1; }
  compgen -G "${REL}/ggml-cpu-*.dll" >/dev/null || { echo "error: no ggml-cpu-*.dll variants built" >&2; exit 1; }

  # Package as Release/<files> — the same layout as upstream's CPU zip, so the
  # artifact's probe_rel ends in Release/whisper-cli.exe.
  STAGE="${WORK}/stage"
  rm -rf "${STAGE}"; mkdir -p "${STAGE}/Release"
  cp "${REL}/whisper-cli.exe" "${REL}"/*.dll "${STAGE}/Release/"
  echo ">> packaged files:"
  ls -l "${STAGE}/Release"
  # Launch check: proves every DLL the exe links resolves from its own dir.
  "${STAGE}/Release/whisper-cli.exe" --help >/dev/null

  OUT="${DIST}/${ASSET}"
  rm -f "${OUT}"
  # python's zipfile always writes '/' separators (zip spec); some Windows
  # archivers store backslashes instead.
  python -I -c '
import os, sys, zipfile
stage, out = sys.argv[1], sys.argv[2]
rel = os.path.join(stage, "Release")
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
    z.write(rel, "Release/")
    for name in sorted(os.listdir(rel)):
        z.write(os.path.join(rel, name), "Release/" + name)
' "${STAGE}" "${OUT}"
  SHA="$(sha256sum "${OUT}" | awk '{print $1}')"
  BYTES="$(wc -c < "${OUT}" | tr -d ' ')"
  echo
  echo "=================================================================="
  echo " Built: ${OUT}"
  echo " SHA-256: ${SHA}"
  echo " Bytes:   ${BYTES}"
  echo "=================================================================="
  echo
  cat <<EOF
    Artifact {
        id: "whisper-bin-vulkan",
        display: "whisper.cpp CLI (Vulkan GPU, ${WHISPER_TAG})",
        url: "https://github.com/swarnavspujari/fly-on-the-wall/releases/download/tools-whisper-vulkan-${WHISPER_TAG}/${ASSET}",
        sha256: "${SHA}",
        bytes: ${BYTES},
        kind: ArtifactKind::Archive,
        dest_rel: "bin/whisper-vulkan/${WHISPER_TAG}",
        probe_rel: "bin/whisper-vulkan/${WHISPER_TAG}/Release/whisper-cli.exe",
    },
EOF
  exit 0
fi

cmake --build "${SRC}/build" --config Release --target whisper-cli -j

BIN="${SRC}/build/bin/whisper-cli"
[[ -f "${BIN}" ]] || { echo "error: whisper-cli not found at ${BIN}" >&2; exit 1; }

# Sanity: show what the binary dynamically links. It should reference only
# system libraries/frameworks — anything else means we'd have to bundle it.
echo ">> dynamic dependencies (expect only system libs):"
if [[ "${target}" == "macos" ]]; then
  otool -L "${BIN}" || true
  # HARD assert both slices exist. A single-arch binary here means the
  # universal flags were dropped (or cmake cached an old configure) and the
  # archive would brick whichever architecture is missing — the arm64 slice
  # in particular has never been exercised by anyone yet.
  ARCHS="$(lipo -archs "${BIN}")"
  echo ">> architectures: ${ARCHS}"
  if [[ "${ARCHS}" != "x86_64 arm64" ]]; then
    echo "error: expected a universal binary ('x86_64 arm64'), got '${ARCHS}'" >&2
    exit 1
  fi
  echo ">> minimum macOS per slice (expect ${MACOS_DEPLOYMENT_TARGET}):"
  otool -l "${BIN}" | grep -A2 'LC_BUILD_VERSION\|LC_VERSION_MIN' | grep -i 'minos\|version' || true
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
# Strip owner metadata from the archive. The flags differ by tar flavor:
# macOS ships bsdtar (libarchive 3.5.x), which has --uid/--gid but rejects
# GNU tar's --owner/--group outright — using those kills the script (set -e)
# right after a successful compile. Note the tarball is NOT bit-reproducible
# across rebuilds (the staged file's mtime lands in the header); integrity
# is anchored by the SHA-256 pin of the one hosted artifact, not by rebuild
# equality.
if [[ "${target}" == "macos" ]]; then
  tar --numeric-owner --uid 0 --gid 0 -cjf "${OUT}" -C "${STAGE}" whisper-cli
else
  tar --numeric-owner --owner=0 --group=0 -cjf "${OUT}" -C "${STAGE}" whisper-cli
fi

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
echo " Next steps (normally done by .github/workflows/build-whisper-sidecar.yml):"
echo "  1. Upload ${ASSET} to a GitHub release on THIS repo,"
echo "     tag: tools-whisper-${WHISPER_TAG}"
echo "  2. Paste this into the matching TOOLS array in src-tauri/src/models.rs:"
echo
cat <<EOF
    Artifact {
        id: "whisper-bin",
        display: "whisper.cpp CLI (${target}, ${WHISPER_TAG})",
        url: "https://github.com/swarnavspujari/fly-on-the-wall/releases/download/tools-whisper-${WHISPER_TAG}/${ASSET}",
        sha256: "${SHA}",
        bytes: ${BYTES},
        kind: ArtifactKind::Archive,
        dest_rel: "bin/whisper/${WHISPER_TAG}",
        probe_rel: "bin/whisper/${WHISPER_TAG}/whisper-cli",
    },
EOF
