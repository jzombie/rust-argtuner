use argtuner::project::Project;
use argtuner::tuner::Tuner;
use argtuner::validate::validate_project_config;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod tui;

#[derive(Parser)]
#[command(
    author,
    version,
    about = concat!("Repository: ", env!("CARGO_PKG_REPOSITORY")),
    long_about = None,
    arg_required_else_help = true,
    help_template = "{name} {version}\n{about}\n\n{usage-heading} {usage}\n\n{all-args}\n"
)]
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
        /// Allow resuming even if the config changed (not recommended; use at your own risk)
        #[arg(long)]
        allow_config_change: bool,
    },
    /// Rebuild trials.csv from trials.sqlite
    RebuildCsv {
        /// Path to the project directory
        #[arg(value_name = "PROJECT_DIR")]
        path: PathBuf,
    },
    /// Watch trials in a live TUI
    Watch {
        /// Path to the project directory (watches <dir>/trials.sqlite)
        #[arg(long, value_name = "PROJECT_DIR")]
        project: PathBuf,
        /// Polling interval (ms)
        #[arg(long, default_value_t = 5000)]
        poll_ms: u64,
    },
    /// Show the scheduler plan for a project
    Plan {
        /// Path to the project directory
        #[arg(value_name = "PROJECT_DIR")]
        path: PathBuf,
        /// Optional config id to visualize within the plan
        #[arg(long)]
        config_id: Option<usize>,
    },
}

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Run {
            path,
            dry_run,
            allow_config_change,
        } => {
            let project = Project::new(path);
            let tuner = Tuner::new(project);

            let options = argtuner::tuner::RunOptions {
                dry_run: *dry_run,
                allow_config_change: *allow_config_change,
            };
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
        Commands::Watch { project, poll_ms } => {
            let project = Project::new(project);
            if let Err(e) = tui::run(project.trials_db_path(), *poll_ms) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        Commands::Plan { path, config_id } => {
            let project = Project::new(path);
            match project.load_config() {
                Ok(config) => {
                    let template = match project.read_template() {
                        Ok(template) => template,
                        Err(err) => {
                            eprintln!("Error: failed to read template: {err}");
                            std::process::exit(1);
                        }
                    };
                    let template_placeholders = template.placeholders().unwrap_or_default();
                    let space = match project.read_space() {
                        Ok(space) => space,
                        Err(err) => {
                            eprintln!("Error: failed to read search space: {err}");
                            std::process::exit(1);
                        }
                    };
                    if let Err(err) =
                        validate_project_config(&config, &template, &space, &template_placeholders)
                    {
                        eprintln!("Error: {}", err);
                        std::process::exit(1);
                    }
                    let plan = argtuner::scheduler::build_plan(&config, *config_id);
                    println!("{}", plan.render(*config_id));
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }
}

#[cfg(test)]
mod cli_tests {
    use super::Cli;
    use clap::Parser;

    #[test]
    fn watch_requires_project() {
        assert!(
            Cli::try_parse_from(["argtuner", "watch"]).is_err(),
            "watch without --project must fail"
        );
        assert!(
            Cli::try_parse_from(["argtuner", "watch", "--project", "x"]).is_ok(),
            "watch --project <dir> must parse"
        );
        assert!(
            Cli::try_parse_from(["argtuner", "watch", "--db", "x"]).is_err(),
            "--db flag no longer exists"
        );
    }

    #[test]
    fn watch_poll_ms_defaults_to_5000() {
        let cli = Cli::try_parse_from(["argtuner", "watch", "--project", "x"]).expect("parses");
        let super::Commands::Watch { poll_ms, .. } = cli.command else {
            panic!("expected Watch command");
        };
        assert_eq!(poll_ms, 5000);
    }
}
