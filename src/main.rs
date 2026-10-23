use clap::Parser;
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
}
fn main() {
    let cli = Cli::parse();
    if let Err(error) = git_rsrs::config::Config::load(&cli.config) {
        eprintln!("{error}");
        std::process::exit(2);
    }
}
