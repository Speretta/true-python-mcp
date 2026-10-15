#!/bin/sh
# marimo edit with the built-in MCP endpoint (docs: editor_features/mcp.md),
# headless, no token, HTTP on 2718 inside the container. --host 0.0.0.0 is
# required for docker port mapping to reach it; the host side of the mapping
# is bound to 127.0.0.1 by measure.sh, so it stays loopback-only.
set -e
exec marimo edit /work/bench.py --mcp --no-token --headless --port 2718 --host 0.0.0.0
