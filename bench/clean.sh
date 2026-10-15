#!/usr/bin/env bash
# Remove every image and leftover container this benchmark created.
# Run after measuring to give the disk space back.
set -euo pipefail
for img in bench-tpmcp bench-datalayer bench-block bench-cursor bench-marimo \
    bench-analyzer bench-runpython bench-yzfly bench-jlab-base; do
    docker image rm -f "$img:pin" >/dev/null 2>&1 || true
done
docker image rm -f ghcr.io/anselmoo/mcp-server-analyzer:latest >/dev/null 2>&1 || true
docker container prune -f --filter "name=bench-" >/dev/null 2>&1 || true
echo "bench images and containers removed"
docker system df | head -2
