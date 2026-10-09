#![forbid(unsafe_code)]

use std::{error::Error, io::Write, path::PathBuf};

use clap::{Parser, Subcommand};
use micu_image_mcp::{
    app::AppState,
    installer::{self, InstallOptions, ResetOptions},
    mcp_server::MicuServer,
    updates::{self, UpdateChecker},
};
use rmcp::ServiceExt;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "micu-image-mcp",
    version,
    about = "Native Micu Image MCP server"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the MCP STDIO server (the default when no command is provided).
    Serve,
    /// Install the MCP configuration.
    Install {
        #[arg(long)]
        no_codex: bool,
        #[arg(long)]
        no_claude: bool,
        #[arg(long)]
        yes: bool,
        #[arg(long = "baseurl")]
        base_url: Option<String>,
        #[arg(long)]
        save_dir: Option<PathBuf>,
        /// Copy this binary into the stable per-user install directory.
        #[arg(long)]
        binary_path: Option<PathBuf>,
        /// Development mode: point client config directly at --binary-path/current executable.
        #[arg(long)]
        dev: bool,
    },
    /// Remove only the micu-image MCP configuration.
    Reset {
        #[arg(long)]
        no_codex: bool,
        #[arg(long)]
        no_claude: bool,
        #[arg(long)]
        yes: bool,
    },
    /// Diagnose configuration and runtime prerequisites.
    Doctor,
    /// Print the project version.
    Version,
    /// Check the latest stable release without changing files (24-hour cache).
    CheckUpdate {
        /// Bypass the cache or disabled automatic checks for this invocation.
        #[arg(long)]
        force: bool,
    },
    /// Download and verify the latest stable binary, preserving all configuration.
    Update {
        /// Replace this installed binary instead of the currently running executable.
        #[arg(long)]
        binary_path: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        let mut stderr = std::io::stderr().lock();
        let _ignored = writeln!(stderr, "micu-image-mcp: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    initialize_tracing();
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve().await,
        Command::Version => {
            let mut stdout = std::io::stdout().lock();
            writeln!(stdout, "{}", env!("CARGO_PKG_VERSION"))?;
            Ok(())
        }
        Command::Install {
            no_codex,
            no_claude,
            yes,
            base_url,
            save_dir,
            binary_path,
            dev,
        } => installer::install(InstallOptions {
            no_codex,
            no_claude,
            yes,
            base_url,
            save_dir,
            binary_path,
            dev,
        })
        .map_err(Into::into),
        Command::Reset {
            no_codex,
            no_claude,
            yes,
        } => installer::reset(ResetOptions {
            no_codex,
            no_claude,
            yes,
        })
        .map_err(Into::into),
        Command::Doctor => installer::doctor().map_err(Into::into),
        Command::CheckUpdate { force } => {
            let checker = UpdateChecker::system().map_err(|error| error as Box<dyn Error>)?;
            match checker.check(force).await {
                Ok(info) => writeln!(
                    std::io::stdout().lock(),
                    "{}",
                    serde_json::to_string_pretty(&info)?
                )?,
                Err(_) => {
                    writeln!(
                        std::io::stdout().lock(),
                        "{}",
                        serde_json::to_string_pretty(&checker.info())?
                    )?;
                    return Err("无法检查 GitHub Release；现有 MCP 可继续使用，稍后用 check-update --force 重试".into());
                }
            }
            if let Some(hint) = checker.take_hint() {
                writeln!(std::io::stdout().lock(), "{hint}")?;
            }
            Ok(())
        }
        Command::Update { binary_path } => {
            let message = updates::update(binary_path)
                .await
                .map_err(|error| error as Box<dyn Error>)?;
            writeln!(std::io::stdout().lock(), "{message}")?;
            Ok(())
        }
    }
}

async fn serve() -> Result<(), Box<dyn Error>> {
    let state = AppState::load()?;
    let engine = state.tool_engine()?;
    let mut server = MicuServer::new(engine)?;
    if let Ok(checker) = UpdateChecker::system() {
        checker.start();
        server = server.with_updates(checker);
    }
    let running = server
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await?;
    running.waiting().await?;
    Ok(())
}

fn initialize_tracing() {
    // Do not honor broad RUST_LOG=trace: protocol/HTTP dependency traces may contain tool
    // arguments.  Keep third-party transports off and send the remaining diagnostics to stderr.
    let filter = EnvFilter::new("warn,rmcp=off,reqwest=off,hyper=off,h2=off");
    let _ignored = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
