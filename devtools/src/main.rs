use clap::{Parser, Subcommand};
use micu_devtools::{Result, audit, benchmark, clients, engineering, harness, live, repository};
use serde_json::Value;
use std::path::PathBuf;

fn default_binary() -> PathBuf {
    PathBuf::from("target/debug").join(format!("micu-image-mcp{}", std::env::consts::EXE_SUFFIX))
}

#[derive(Parser)]
#[command(about = "Rust verification and repository engineering tools")]
struct Cli {
    #[command(subcommand)]
    command: Task,
}

#[derive(Subcommand)]
enum Task {
    Smoke {
        #[arg(long,default_value_os_t=default_binary())]
        binary: PathBuf,
    },
    Stress {
        #[arg(long,default_value_os_t=default_binary())]
        binary: PathBuf,
        #[arg(long, default_value_t = 4)]
        processes: usize,
        #[arg(long, default_value_t = 1)]
        rounds: usize,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    SessionBenchmark {
        #[arg(long,default_value_os_t=default_binary())]
        binary: PathBuf,
        #[arg(long, default_value_t = 3)]
        repeat: usize,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    RuntimeBenchmark {
        #[arg(long,default_value_os_t=default_binary())]
        binary: PathBuf,
        #[arg(long, default_value_t = 3)]
        repeat: usize,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    AuditProxy {
        #[arg(long)]
        binary: PathBuf,
        #[arg(long)]
        audit: PathBuf,
        #[arg(long)]
        omit_cache: bool,
        #[arg(long)]
        catalog: Option<PathBuf>,
    },
    CatalogProbe {
        #[arg(long)]
        cli: PathBuf,
        #[arg(long,default_value_os_t=default_binary())]
        binary: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    NativeHarness {
        #[arg(long)]
        cli: PathBuf,
        #[arg(long, default_value = "claude")]
        kind: String,
        #[arg(long,default_value_os_t=default_binary())]
        binary: PathBuf,
        #[arg(long)]
        model: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value_t = 120)]
        timeout: u64,
        #[arg(long)]
        live_images: bool,
        #[arg(long)]
        catalog: Option<PathBuf>,
    },
    QualityMatrix {
        #[arg(long,default_value_os_t=default_binary())]
        binary: PathBuf,
        #[arg(
            long,
            value_delimiter = ',',
            default_value = "gpt-image-2.5-flare,gpt-image-2.5-sunburst,gpt-image-2,gpt-image-2-openai"
        )]
        models: Vec<String>,
        #[arg(long, value_delimiter = ',', default_value = "1024x1024")]
        sizes: Vec<String>,
        #[arg(long)]
        live: bool,
        #[arg(long, default_value_t = 1)]
        limit: usize,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    SourceAudit {
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    ReleaseCheck {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        github_output: Option<PathBuf>,
    },
    MaintainBranches {
        #[arg(long, default_value = "Subaru486desuwa/micu-image-mcp")]
        repo: String,
        #[arg(long)]
        event: Option<PathBuf>,
        #[arg(long)]
        apply: bool,
    },
}

fn report(value: Value, output: Option<PathBuf>) -> Result<Option<Value>> {
    if let Some(output) = output {
        engineering::write_report(&output, &value)?;
    }
    Ok(Some(value))
}
fn run() -> Result<Option<Value>> {
    let value = match Cli::parse().command {
        Task::Smoke { binary } => engineering::smoke(&binary.canonicalize()?)?,
        Task::Stress {
            binary,
            processes,
            rounds,
            output,
        } => {
            return report(
                engineering::stress(&binary.canonicalize()?, processes, rounds)?,
                output,
            );
        }
        Task::SessionBenchmark {
            binary,
            repeat,
            output,
        } => {
            return report(
                engineering::session_concurrency(&binary.canonicalize()?, repeat)?,
                output,
            );
        }
        Task::RuntimeBenchmark {
            binary,
            repeat,
            output,
        } => return report(benchmark::runtime(&binary.canonicalize()?, repeat)?, output),
        Task::AuditProxy {
            binary,
            audit,
            omit_cache,
            catalog,
        } => {
            audit::relay(
                &binary.canonicalize()?,
                &audit,
                omit_cache,
                catalog.as_deref(),
            )?;
            return Ok(None);
        }
        Task::CatalogProbe {
            cli,
            binary,
            output,
        } => clients::catalog_probe(&cli.canonicalize()?, &binary.canonicalize()?, &output)?,
        Task::NativeHarness {
            cli,
            kind,
            binary,
            model,
            output,
            timeout,
            live_images,
            catalog,
        } => {
            clients::gated("MICU_RUN_HARNESS_TESTS")?;
            harness::native(harness::Options {
                cli: &cli.canonicalize()?,
                kind: &kind,
                binary: &binary.canonicalize()?,
                model: &model,
                output: &output,
                timeout,
                live_images,
                catalog: catalog.as_deref(),
            })?
        }
        Task::QualityMatrix {
            binary,
            models,
            sizes,
            live,
            limit,
            output,
        } => {
            if live {
                clients::gated("MICU_RUN_LIVE_TESTS")?;
            }
            let binary = if live { binary.canonicalize()? } else { binary };
            live::quality(&binary, &models, &sizes, live, limit, output.as_deref())?
        }
        Task::SourceAudit { root } => repository::source_audit(&root)?,
        Task::ReleaseCheck {
            root,
            tag,
            github_output,
        } => repository::release_check(&root, tag.as_deref(), github_output.as_deref())?,
        Task::MaintainBranches { repo, event, apply } => {
            repository::maintain_branches(&repo, event.as_deref(), apply)?
        }
    };
    Ok(Some(value))
}

fn main() {
    match run() {
        Ok(Some(value)) => println!("{value}"),
        Ok(None) => {}
        Err(error) => {
            eprintln!("micu-devtools: {error}");
            std::process::exit(1);
        }
    }
}
