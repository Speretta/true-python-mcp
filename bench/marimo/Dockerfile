# marimo's built-in MCP server (experimental; read-only tools over HTTP).
# Exact pin resolved 2026-10-15.
FROM python:3.12-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl procps \
    && rm -rf /var/lib/apt/lists/*
RUN pip install --no-cache-dir "marimo[mcp]==0.25.1"
COPY --chmod=755 entrypoint.sh /entrypoint.sh
ENTRYPOINT ["/entrypoint.sh"]
