# File-based notebook editor, closest architectural analog to tpmcp
# (single Python process, no kernel). Exact pin resolved 2026-10-15.
# Note: PyPI version is 0.3.1 while the project README claims 0.3.2.
FROM python:3.12-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends procps \
    && rm -rf /var/lib/apt/lists/*
RUN pip install --no-cache-dir cursor-notebook-mcp==0.3.1 pydantic==2.11.10
# The 0.3.1 sdist does not declare pydantic-settings but imports it at
# startup ("Error importing package components"), so the server cannot
# start as published on PyPI. Its fastmcp<2.11 pin also breaks on current
# pydantic ("cannot specify both default and default_factory"), hence the
# pydantic pin above. Both fixes are noted in COMPARISON.md as maintenance
# signals: a year-old pin set no longer starts with today's resolver.
RUN pip install --no-cache-dir pydantic-settings==2.16.0
ENTRYPOINT ["cursor-notebook-mcp"]
