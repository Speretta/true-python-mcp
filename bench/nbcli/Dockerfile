# jupyter-ai-contrib/nb-cli: the `nb` notebook CLI (Rust, BSD-3), aimed at
# humans and AI agents. Unlike the MCP stacks there is no server process:
# every operation is one `nb` invocation, so the bench runs this image with
# `--entrypoint sh` and feeds it commands (see measure.sh / driver.py).
#
# The binary is the project's own published release asset (static-pie,
# verified against the SHA256SUMS the release ships), pinned by version and
# sha256 here. ipykernel provides the `python3` kernelspec that `nb execute`
# uses for its native kernel execution over ZeroMQ - that install is the
# whole reason this image differs from a bare `nb` download. Exact pins
# resolved 2026-10-27 (nb-cli v0.0.10 released 2026-07-31; ipykernel pin
# matches bench/jlab-base.Dockerfile).
FROM python:3.12-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl ca-certificates procps \
    && rm -rf /var/lib/apt/lists/*
ARG NB_VERSION=0.0.10
ARG NB_SHA256=0910daf3032da5a3e52951072d3ddb0eb2a0db13adf9bacb9e1b8af74593fd9e
RUN curl -fsSL -o /usr/local/bin/nb \
        "https://github.com/jupyter-ai-contrib/nb-cli/releases/download/v${NB_VERSION}/nb-linux-amd64" \
    && echo "${NB_SHA256}  /usr/local/bin/nb" | sha256sum -c - \
    && chmod +x /usr/local/bin/nb
RUN nb --version > /nb-version.txt
RUN pip install --no-cache-dir ipykernel==7.4.0
ENTRYPOINT ["nb"]
