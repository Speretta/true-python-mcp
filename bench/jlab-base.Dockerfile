# Shared base for the two JupyterLab-based stacks (datalayer, block):
# python:3.12-slim plus the JupyterLab server stack both projects require.
# Exact pins resolved from PyPI on 2026-10-15.
FROM python:3.12-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl procps \
    && rm -rf /var/lib/apt/lists/*
RUN pip install --no-cache-dir \
    jupyterlab==4.6.4 \
    jupyter-collaboration==5.0.4 \
    ipykernel==7.4.0
