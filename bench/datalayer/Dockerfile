FROM bench-jlab-base:pin
RUN pip install --no-cache-dir jupyter-mcp-tools==0.1.8 jupyter-mcp-server==2.2.4
COPY --chmod=755 entrypoint.sh /entrypoint.sh
ENTRYPOINT ["/entrypoint.sh"]
