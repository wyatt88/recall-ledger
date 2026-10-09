mod mcp;
mod transport;

use clap::{Parser, Subcommand};
use recall_ledger::Store;
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Local, revision-aware memory for agents")]
struct Cli {
    #[arg(long, global = true, default_value = "recall-ledger.db")]
    db: PathBuf,
    #[arg(long, global = true, default_value = "default")]
    namespace: String,
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
    },
    /// Provide MCP stdio tools connected to an existing daemon.
    ///
    /// Namespace is pinned by that daemon. --db/--namespace do not select an
    /// alternative daemon namespace in this mode.
    Mcp {
        #[arg(long)]
        socket: PathBuf,
    },
}

fn run(cli: Cli) -> Result<(), recall_ledger::Error> {
    match cli.command {
        Command::Call { method, params } => {
            let params: Value = serde_json::from_str(&params)
                .map_err(|err| transport::failure("invalid_json", err.to_string(), false))?;
            let mut store = Store::open(cli.db)?;
            let result = store.execute(&cli.namespace, &method, params)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&result).expect("JSON value serialization")
            );
            Ok(())
        }
        Command::Serve { socket } => transport::serve(&cli.db, &cli.namespace, &socket),
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
