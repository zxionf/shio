//! `relay` 命令行入口。

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use shio_relay::{server, Config};

#[derive(Debug, Parser)]
#[command(name = "relay", version, about = "shio content-agnostic relay")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 启动中继服务器。
    Serve {
        /// `relay.toml` 路径。
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    init_tracing();
    if let Err(err) = run().await {
        tracing::error!(%err, "relay failed");
        std::process::exit(1);
    }
}

async fn run() -> shio_relay::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve { config } => {
            let config = Config::load(config)?;
            server::serve(config).await
        }
    }
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
