mod mcp;
mod transport;

use clap::{Args, Parser, Subcommand};
use recall_ledger::{models, runtime::Runtime, Store};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Local, revision-aware memory for agents")]
struct Cli {
    #[arg(long, global = true, default_value = "recall-ledger.db")]
    db: PathBuf,
    #[arg(long, global = true, default_value = "default")]
    namespace: String,
    /// Explicit loopback model provider configuration for CLI operations or serving.
    #[arg(long, global = true)]
    models: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Execute a trusted local administrative operation directly against SQLite.
    Call {
        method: String,
        #[arg(long, default_value = "{}")]
        params: String,
    },
    /// Serve one fixed namespace over a private Unix-domain socket.
    Serve {
        #[arg(long)]
        socket: PathBuf,
        /// Disable background indexing, summarization, and repair.
        #[arg(long)]
        no_worker: bool,
        #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u64).range(100..))]
        worker_interval_ms: u64,
        #[command(flatten)]
        summary: SummaryOptions,
    },
    /// Run one bounded model maintenance batch directly; no daemon is required.
    Maintain {
        #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u64).range(0..=32))]
        max_embeddings: u64,
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u64).range(0..=8))]
        max_summaries: u64,
        #[command(flatten)]
        summary: SummaryOptions,
    },
    /// Provide MCP stdio tools connected to an existing daemon.
    ///
    /// Namespace and models are pinned by that daemon.
    /// --db/--namespace/--models are not used in this mode.
    Mcp {
        #[arg(long)]
        socket: PathBuf,
    },
}

#[derive(Args)]
struct SummaryOptions {
    /// Minimum unassigned inputs before creating a summary recipe; 0 disables grouping.
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u64).range(0..=64))]
    auto_summary_min: u64,
    #[arg(long, default_value = "fixed", value_parser = ["individual", "fixed", "topic", "volatility"])]
    summary_strategy: String,
    #[arg(long, default_value_t = 12_000, value_parser = clap::value_parser!(u64).range(1..=16_000))]
    max_group_chars: u64,
    #[arg(long, default_value_t = 2000, value_parser = clap::value_parser!(u64).range(1..=16_384))]
    max_summary_chars: u64,
}

impl SummaryOptions {
    fn params(&self, max_embeddings: u64, max_summaries: u64) -> Value {
        json!({
            "max_embeddings":max_embeddings,"max_summaries":max_summaries,
            "auto_summary_min":self.auto_summary_min,"strategy":self.summary_strategy,
            "max_group_chars":self.max_group_chars,"max_summary_chars":self.max_summary_chars
        })
    }
}

fn config(path: Option<PathBuf>) -> Result<Option<models::Config>, recall_ledger::Error> {
    path.map(models::Config::load).transpose()
}

fn print_result(result: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(result).expect("JSON value serialization")
    );
}

fn run(cli: Cli) -> Result<(), recall_ledger::Error> {
    match cli.command {
        Command::Call { method, params } => {
            let params: Value = serde_json::from_str(&params)
                .map_err(|err| transport::failure("invalid_json", err.to_string(), false))?;
            let mut runtime = Runtime::new(Store::open(cli.db)?, config(cli.models)?)?;
            let result = runtime.execute(&cli.namespace, &method, params)?;
            print_result(&result);
            Ok(())
        }
        Command::Serve {
            socket,
            no_worker,
            worker_interval_ms,
            summary,
        } => {
            let models = config(cli.models)?;
            let worker = if !no_worker
                && models
                    .as_ref()
                    .is_some_and(|c| c.embedding.is_some() || c.summary.is_some())
            {
                Some(transport::WorkerOptions {
                    interval_ms: worker_interval_ms,
                    params: summary.params(4, 1),
                })
            } else {
                None
            };
            transport::serve(&cli.db, &cli.namespace, &socket, models, worker)
        }
        Command::Maintain {
            max_embeddings,
            max_summaries,
            summary,
        } => {
            let mut runtime = Runtime::new(Store::open(cli.db)?, config(cli.models)?)?;
            let result = runtime.execute(
                &cli.namespace,
                "maintain",
                summary.params(max_embeddings, max_summaries),
            )?;
            print_result(&result);
            Ok(())
        }
        Command::Mcp { socket } => mcp::run(&socket)
            .map_err(|err| transport::failure("mcp_transport_error", err.to_string(), false)),
    }
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("{}", json!({"error": error}));
        std::process::exit(1);
    }
}
