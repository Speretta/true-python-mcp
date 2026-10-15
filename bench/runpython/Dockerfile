# pydantic/mcp-run-python - RETIRED upstream (archived repo; the retirement
# notice is quoted in COMPARISON.md). WASM sandbox: Pyodide inside Deno.
# Idle tree: python launcher + deno. Deno's version is not pinned upstream,
# so the installed one is recorded in the image (/deno-version.txt) and in
# the run's meta.json. Pin: PyPI 0.0.22 (final release, 2025-12-11).
FROM python:3.13-slim-bookworm
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl ca-certificates unzip \
    && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://deno.land/install.sh | sh -s -- -y
ENV DENO_INSTALL=/root/.deno
ENV PATH=/root/.deno/bin:$PATH
RUN deno --version > /deno-version.txt
RUN pip install --no-cache-dir mcp-run-python==0.0.22
# Pre-warm the Deno environment and the Pyodide download at build time so
# measured runs never pay first-start network costs. The example itself may
# fail without forwarded deps - the cache warm-up is what matters.
RUN mcp-run-python example || true
ENTRYPOINT ["mcp-run-python", "stdio"]
