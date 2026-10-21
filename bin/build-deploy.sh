#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
if [ "$CURRENT_BRANCH" != "main" ]; then
    echo "WARNING: deploying from branch '$CURRENT_BRANCH', not 'main'." >&2
    read -r -p "Continue anyway? [y/N] " reply
    case "$reply" in
        [yY] | [yY][eE][sS]) ;;
        *)
            echo "Aborted." >&2
            exit 1
            ;;
    esac
fi

TAG="${1:-$(git rev-parse --short HEAD)}"
HQ_DOKKU_REMOTE="${HQ_DOKKU_REMOTE:?HQ_DOKKU_REMOTE must be set (e.g. dokku@your-server.com)}"
HQ_DOKKU_APP="${HQ_DOKKU_APP:?HQ_DOKKU_APP must be set}"
IMAGE_NAME="hq:${TAG}"

# 1. Rebuild Tailwind CSS
echo "==> Rebuilding Tailwind CSS..."
cd web-ui
"$REPO_ROOT/bin/tailwindcss" -i ./src/input.css -o ./src/output.css -m
cd "$REPO_ROOT"

# 2. Build a buildx-enabled builder image. Colima's docker CLI lacks the
#    buildx plugin, and the legacy builder ignores the Dockerfile's
#    --platform=linux/amd64 on the runner stage, silently producing an arm64
#    image. BuildKit is required to honor --platform while the builder stage
#    cross-compiles natively on arm64. We get buildx via docker/buildx-bin
#    inside a builder container rather than installing a host plugin.
echo "==> Building buildx builder image..."
docker build -t hq-buildx:local - <<'EOF' >/dev/null
# syntax=docker/dockerfile:1
FROM docker:latest
COPY --from=docker/buildx-bin /buildx /usr/libexec/docker/cli-plugins/docker-buildx
EOF

# 3. Build the Docker image for linux/amd64 via BuildKit (cross-compiles hq).
#    --load makes the amd64 image available to the host daemon (shared socket)
#    so it can be streamed to Dokku in the next step.
echo "==> Building Docker image for linux/amd64..."
docker run --rm \
    -v /var/run/docker.sock:/var/run/docker.sock \
    -v "${REPO_ROOT}":/workspace \
    -w /workspace \
    hq-buildx:local \
    docker buildx build --load --no-cache-filter runner -t "${IMAGE_NAME}" -t "hq:latest" .

# 4. Stream the image to Dokku via SSH
echo "==> Streaming image to Dokku..."
docker image save "${IMAGE_NAME}" | ssh "${HQ_DOKKU_REMOTE}" git:load-image "${HQ_DOKKU_APP}" "${IMAGE_NAME}"

# 5. Cleanup
echo "==> Cleaning up..."
docker rmi "${IMAGE_NAME}" 2>/dev/null || true

echo "==> Done! Deployed ${TAG}"
