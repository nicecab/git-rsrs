use clap::{Parser, Subcommand};
use git_rsrs::{
    config::Config,
    outcome::{Outcome, Trigger},
    sync::sync_once,
};
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
    Watch,
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
    match cli.command {
        Commands::Sync => {
            let report = sync_once(&config, Trigger::Manual);
            render(&report, cli.json);
            std::process::exit(report.outcome.exit_code());
        }
        Commands::Watch => {
            if let Err(error) =
                git_rsrs::watch::watch_with_reports(&config, |report| render(&report, cli.json))
            {
                eprintln!("{error}");
                std::process::exit(
                    if matches!(error, git_rsrs::watch::WatchError::Interrupted) {
                        Outcome::ManualIntervention.exit_code()
                    } else {
                        1
                    },
                );
            }
        }
    }
}
fn render(report: &git_rsrs::outcome::SyncReport, json: bool) {
    for diagnostic in &report.diagnostics {
        eprintln!("{}: {}", diagnostic.phase, diagnostic.message);
    }
    if json {
        println!(
            "{}",
            serde_json::to_string(report).expect("serializable report")
        );
    } else {
        println!("{}", report.outcome.as_str());
        for recovery in &report.recovery {
            eprintln!("recovery: {recovery}");
        }
    }
}
