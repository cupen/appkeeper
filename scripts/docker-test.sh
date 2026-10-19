#!/usr/bin/env bash
# Build and run the appkeeper one-shot acceptance image (docker-test-env).
#
#   scripts/docker-test.sh                    # build (global line) + full suite
#   scripts/docker-test.sh --region cn        # mainland mirrors
#   scripts/docker-test.sh --no-build         # reuse the existing image
#   scripts/docker-test.sh --stages e2e       # run a subset
#
# Outputs (stage logs + bench JSON) land in docker-out/<timestamp>/ and the
# container exit code is propagated unchanged.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="${AK_TEST_IMAGE:-appkeeper-test:latest}"
REGION="${AK_TEST_REGION:-global}"
STAGES="${AK_TEST_STAGES:-all}"
OUT_DIR="${AK_TEST_OUT:-}"
DO_BUILD=1

usage() {
    sed -n '2,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --region)   REGION="${2:?--region needs global|cn}"; shift 2 ;;
        --stages)   STAGES="${2:?--stages needs a comma-separated list}"; shift 2 ;;
        --out)      OUT_DIR="${2:?--out needs a directory}"; shift 2 ;;
        --no-build) DO_BUILD=0; shift ;;
        -h|--help)  usage 0 ;;
        *)          echo "unknown argument: $1" >&2; usage 2 ;;
    esac
done

if [ "$DO_BUILD" -eq 1 ]; then
    build_args=(--build-arg "REGION=$REGION")
    for var in RUST_VERSION NODE_VERSION PNPM_VERSION PLAYWRIGHT_VERSION PW_DOWNLOAD_HOST; do
        if [ -n "${!var:-}" ]; then
            build_args+=(--build-arg "$var=${!var}")
        fi
    done
    echo "[ak-test] building $IMAGE (REGION=$REGION)"
    docker build -f "$REPO_ROOT/docker/Dockerfile.test" \
        "${build_args[@]}" -t "$IMAGE" "$REPO_ROOT"
fi

if [ -z "$OUT_DIR" ]; then
    OUT_DIR="$REPO_ROOT/docker-out/$(date +%Y%m%d-%H%M%S)"
fi
mkdir -p "$OUT_DIR"

# Mask the host's target/ with a per-repo named volume. e2e hardcodes
# target/debug/<bin>, and reusing host artifacts built on another distro can
# yield binaries that need a newer glibc than the container has. The volume
# keeps the host checkout untouched and still caches builds across runs.
TARGET_VOLUME="${AK_TEST_TARGET_VOLUME:-appkeeper-test-target-$(printf '%s' "$REPO_ROOT" | sha256sum | cut -c1-12)}"

echo "[ak-test] running $IMAGE (stages=$STAGES, out=$OUT_DIR, target=$TARGET_VOLUME)"
exec docker run --rm \
    -v "$REPO_ROOT:/work" \
    -v "$TARGET_VOLUME:/work/target" \
    -v "$OUT_DIR:/out" \
    -e "HOST_UID=$(id -u)" \
    -e "HOST_GID=$(id -g)" \
    -e "AK_TEST_STAGES=$STAGES" \
    "$IMAGE"
