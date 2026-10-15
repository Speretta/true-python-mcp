#!/bin/sh
# Start JupyterLab the way the project README describes (bound to loopback
# instead of 0.0.0.0 for safety - the only deviation, and it cannot affect
# memory), wait until it answers, then run the MCP server on stdio.
set -e
jupyter lab --port 8888 --IdentityProvider.token MY_TOKEN --ip 127.0.0.1 \
    --no-browser --allow-root --ServerApp.root_dir=/work >/tmp/jlab.log 2>&1 &
i=0
until curl -sf -H "Authorization: token MY_TOKEN" \
    http://127.0.0.1:8888/api/status >/dev/null 2>&1; do
    i=$((i + 1))
    if [ "$i" -gt 90 ]; then
        echo "jupyter lab did not become ready" >&2
        cat /tmp/jlab.log >&2
        exit 1
    fi
    sleep 1
done
export JUPYTER_URL="http://127.0.0.1:8888"
export JUPYTER_TOKEN="MY_TOKEN"
export ALLOW_IMG_OUTPUT="false"
exec jupyter-mcp-server
