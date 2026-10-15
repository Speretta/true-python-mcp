# yzfly/mcp-python-interpreter - subprocess execution in the selected
# environment (here: the container's own Python). Pin: PyPI 1.2.3,
# resolved 2026-10-15. Note: the GitHub license field shows NOASSERTION;
# COMPARISON.md flags this. We only run and measure it, we redistribute
# nothing.
FROM python:3.12-slim
# mcp-python-interpreter 1.2.3 imports mcp.server.fastmcp, which the mcp
# SDK 2.x removed, so it does not start with today's resolver. Pinning the
# newest 1.x line fixes it; COMPARISON.md notes this as the third stack
# (after cursor and block) needing a manual pin to start at all.
RUN pip install --no-cache-dir mcp-python-interpreter==1.2.3 mcp==1.30.0
ENTRYPOINT ["mcp-python-interpreter"]
