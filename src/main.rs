#![forbid(unsafe_code)]

use std::{error::Error, io::Write, path::PathBuf};

use clap::{Parser, Subcommand};
use micu_image_mcp::{
    app::AppState,
    installer::{self, InstallOptions, ResetOptions},
    mcp_server::MicuServer,
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
    /// Manage credentials explicitly; never rewrites MCP client configuration.
    Credentials {
        #[command(subcommand)]
        action: CredentialCommand,
    },
    /// Print the project version.
    Version,
}

#[derive(Debug, Subcommand)]
enum CredentialCommand {
    /// Authorize the existing key, then verify that background access works.
    Authorize,
    /// Enter a replacement key without echoing it and save to the system store.
    Set,
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
        Command::Credentials { action } => {
            use micu_image_mcp::{
                config::{ENV_KEYS, EnvironmentSnapshot},
                credentials::*,
            };
            let environment = EnvironmentSnapshot::capture(ENV_KEYS)?;
            let service = environment
                .get("MICU_KEYCHAIN_SERVICE")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(DEFAULT_CREDENTIAL_SERVICE);
            let account = environment
                .get("MICU_KEYCHAIN_ACCOUNT")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(DEFAULT_CREDENTIAL_ACCOUNT);
            if matches!(action, CredentialCommand::Set) {
                use std::io::IsTerminal;
                if !std::io::stdin().is_terminal() {
                    return Err(
                        "credentials set 需要交互终端；Key 不接受 argv 或明文文件输入".into(),
                    );
                }
                let secret = secrecy::SecretString::from(rpassword::prompt_password(
                    "Micu API Key（输入隐藏）: ",
                )?);
                use secrecy::ExposeSecret;
                store_api_key(service, account, secret.expose_secret().trim())?;
            }
            authorize_credentials(&SystemCredentials, service, account)?;
            writeln!(
                std::io::stdout().lock(),
                "凭据已验证可供后台读取；请重连 MCP。客户端配置未修改。"
            )?;
            Ok(())
        }
    }
}

async fn serve() -> Result<(), Box<dyn Error>> {
    let state = AppState::load()?;
    let engine = state.tool_engine()?;
    let server = MicuServer::new(engine)?;
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
