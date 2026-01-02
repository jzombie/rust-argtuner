use argtuner::project::Project;
use argtuner::tuner::Tuner;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod watch_ui;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the tuner on a project
    Run {
        /// Path to the project directory
        #[arg(value_name = "PROJECT_DIR")]
        path: PathBuf,
        /// Run without writing to the project store (uses a temp dir)
        #[arg(long)]
        dry_run: bool,
    },
    /// Rebuild trials.csv from trials.sqlite
    RebuildCsv {
        /// Path to the project directory
        #[arg(value_name = "PROJECT_DIR")]
        path: PathBuf,
    },
    /// Watch trials in a live TUI
    Watch {
        /// Path to the project directory (uses its trials.sqlite)
        #[arg(long, value_name = "PROJECT_DIR")]
        project: Option<PathBuf>,
        /// Path to trials.sqlite (overrides --project)
        #[arg(long, value_name = "PATH")]
        db: Option<PathBuf>,
        /// Polling interval (ms)
        #[arg(long, default_value_t = 500)]
        poll_ms: u64,
    },
}

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Run { path, dry_run } => {
            let project = Project::new(path);
            let tuner = Tuner::new(project);

            let options = argtuner::tuner::RunOptions { dry_run: *dry_run };
            if let Err(e) = tuner.run_with_options(options) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        Commands::RebuildCsv { path } => {
            let project = Project::new(path);
            match project.store().and_then(|store| store.rebuild_csv()) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Watch {
            project,
            db,
            poll_ms,
        } => {
            let db_path = match (project, db) {
                (_, Some(db)) => db.clone(),
                (Some(project_dir), None) => {
                    let project = Project::new(project_dir);
                    project.trials_db_path()
                }
                (None, None) => PathBuf::from("trials.sqlite"),
            };
            if let Err(e) = watch_ui::run(db_path, *poll_ms) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
    }
}
