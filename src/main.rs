use argtuner::project::Project;
use argtuner::tuner::Tuner;
use argtuner::validate::validate_project_config;
use clap::{Parser, Subcommand};
use std::borrow::Cow;
use std::path::{Path, PathBuf};

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
    /// Recursively locate argtuner projects
    Find {
        /// Directory to search (defaults to the current directory)
        #[arg(value_name = "DIR")]
        dir: Option<PathBuf>,
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
            if let Err(e) = tui::run(project, *poll_ms) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        Commands::Find { dir } => {
            // `dir` here is `&Option<PathBuf>` (outer `match &cli.command`
            // borrows), so borrow with `match &dir` and keep the owned
            // `PathBuf` alive in the outer scope.
            let root: Cow<Path> = match &dir {
                Some(d) => Cow::Borrowed(d.as_path()),
                None => match std::env::current_dir() {
                    Ok(d) => Cow::Owned(d),
                    Err(e) => {
                        eprintln!("Error: failed to determine current directory: {e}");
                        std::process::exit(1);
                    }
                },
            };
            let projects = argtuner::find_projects(&root);
            if projects.is_empty() {
                eprintln!("No argtuner projects found under {}", root.display());
            }
            for p in projects {
                println!("{}", p.display());
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

    #[test]
    fn find_dir_is_optional() {
        let cli = Cli::try_parse_from(["argtuner", "find"]).expect("find without dir parses");
        let super::Commands::Find { dir } = cli.command else {
            panic!("expected Find command");
        };
        assert!(dir.is_none());

        let cli =
            Cli::try_parse_from(["argtuner", "find", "/tmp/x"]).expect("find with dir parses");
        let super::Commands::Find { dir } = cli.command else {
            panic!("expected Find command");
        };
        assert_eq!(dir, Some("/tmp/x".into()));
    }
}
