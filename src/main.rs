use anyhow::Context;
use clap::Parser;
use rmcp::{ServiceExt, transport::stdio};
use tpmcp::server::Tpmcp;

/// Python and Jupyter notebook MCP server.
#[derive(Parser)]
#[command(name = "tpmcp", version, about)]
struct Cli {
    /// Also expose generic Python/text file tools (py_read, py_write, py_edit).
    /// Off by default: coding hosts (opencode, Claude Code, VS Code, Cursor)
    /// already provide native file tools.
    #[arg(long)]
    file_tools: bool,
}

// Single-threaded runtime: the stdio pump and tool routing share one worker,
// while tool bodies run on the blocking pool (tools::blocking), so long
// lint/format passes and cross-process lock waits never stall the
// connection. Also drops the ~12 idle multi-thread workers from the
// footprint.
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // The stdio transport owns stdout, so all logging must go to stderr.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let service = Tpmcp::new(cli.file_tools)
        .serve(stdio())
        .await
        .context("failed to start MCP server on stdio")?;
    service.waiting().await.context("MCP server stopped")?;
    Ok(())
}
