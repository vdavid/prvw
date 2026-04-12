#!/bin/bash
set -e

# Deploy website script
# Triggered by GitHub Actions via webhook after CI passes

LOG_FILE="/var/log/prvw-deploy.log"

exec > >(tee -a "$LOG_FILE") 2>&1

echo ""
echo "=== Starting Prvw website deployment ==="
echo "Time: $(date --iso-8601=seconds)"

cd /home/david/prvw

echo "Fetching and resetting to origin/main..."
git fetch origin main
git reset --hard origin/main

echo "Building new image (old site stays up during build)..."
cd apps/website
docker compose build --no-cache

echo "Swapping containers..."
docker compose down
docker compose up -d

echo "Verifying container is running..."
sleep 2
if docker compose ps --status running | grep -q getprvw-static; then
    echo "=== Deployment succeeded ==="
else
    echo "=== ERROR: Container not running after deploy ==="
    docker compose logs --tail 20
    exit 1
fi

echo "Time: $(date --iso-8601=seconds)"
