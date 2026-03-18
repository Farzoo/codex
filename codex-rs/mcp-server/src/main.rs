use clap::Parser;
use codex_arg0::Arg0DispatchPaths;
use codex_arg0::arg0_dispatch_or_else;
use codex_mcp_server::run_main;
use codex_mcp_server::tool_response_format::ToolResponseFormat;
use codex_utils_cli::CliConfigOverrides;

#[derive(Debug, Parser)]
struct McpServerArgs {
    /// Format used for codex tool-call responses.
    #[arg(long = "tool-response-format", default_value = "dual")]
    tool_response_format: ToolResponseFormat,
}

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|arg0_paths: Arg0DispatchPaths| async move {
        let args = McpServerArgs::parse();
        run_main(
            arg0_paths,
            CliConfigOverrides::default(),
            args.tool_response_format,
        )
        .await?;
        Ok(())
    })
}
