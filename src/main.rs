use clap::{Parser, Subcommand};
use git_rsrs::{config::Config, outcome::Trigger, sync::sync_once};
use std::path::PathBuf;
#[derive(Parser)]
#[command(
    version,
    about = "Git repository synchronization with external command hooks"
)]
struct Cli {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    json: bool,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    Sync,
}
fn main() {
    if let Err(error) = git_rsrs::process::install_signal_handlers() {
        eprintln!("cannot install signal handlers: {error}");
        std::process::exit(1);
    }
    let cli = Cli::parse();
    let mut config = match Config::load(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2)
        }
    };
    config.repo = match cli.repo.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cannot resolve repository: {e}");
            std::process::exit(2)
        }
    };
    let report = match cli.command {
        Commands::Sync => sync_once(&config, Trigger::Manual),
    };
    for diagnostic in &report.diagnostics {
        eprintln!("{}: {}", diagnostic.phase, diagnostic.message);
    }
    if cli.json {
        println!(
            "{}",
            serde_json::to_string(&report).expect("serializable report")
        );
    } else {
        println!("{}", report.outcome.as_str());
        for recovery in &report.recovery {
            eprintln!("recovery: {recovery}");
        }
    }
    std::process::exit(report.outcome.exit_code());
}
