FROM bench-jlab-base:pin
# mcp-jupyter 2.0.2 declares jupyter-kernel-client>=0.8.0 with no upper
# bound; the 1.x releases removed the KernelClient export it imports. It
# also uses mcp.server.fastmcp, which the mcp SDK 2.x removed, and mcp
# 1.15 needs pydantic internals that pydantic 2.14 dropped. Three manual
# pins to the versions current at its 2025-09-30 release are needed before
# the server starts at all; COMPARISON.md notes this as a maintenance
# signal.
RUN pip install --no-cache-dir mcp-jupyter==2.0.2 \
    jupyter-kernel-client==0.8.0 mcp==1.15.0 pydantic==2.11.7
COPY --chmod=755 entrypoint.sh /entrypoint.sh
ENTRYPOINT ["/entrypoint.sh"]
