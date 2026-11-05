use crate::project::Project;
use crate::tuner::Tuner;
use crate::validate::validate_project_config;
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
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
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
        /// Path to the project directory (watches `<dir>/trials.sqlite`)
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
    /// Re-run an interrupted trial in its existing artifact dir (no sweep,
    /// no wipe). The trial command re-renders from the stored row and the
    /// training binary resumes from its latest checkpoint. History is
    /// append-only: rows already logged stay, so purge unwanted epochs
    /// first or the new run's rows will sit alongside the old ones.
    ResumeTrial {
        /// Path to the project directory
        #[arg(value_name = "PROJECT_DIR")]
        path: PathBuf,
        /// Trial id to resume (must exist and not be completed)
        #[arg(value_name = "TRIAL_ID")]
        trial_id: usize,
    },
    /// Show what argtuner parsed from the project's config
    Inspect {
        /// Path to the project directory
        #[arg(value_name = "PROJECT_DIR")]
        path: PathBuf,
    },
    /// Recursively locate argtuner projects
    Find {
        /// Directory to search (defaults to the current directory)
        #[arg(value_name = "DIR")]
        dir: Option<PathBuf>,
    },
}

pub fn run() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Run {
            path,
            dry_run,
            allow_config_change,
        } => {
            let project = Project::new(path);
            let tuner = Tuner::new(project);

            let options = crate::tuner::RunOptions {
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
        Commands::Inspect { path } => {
            let project = Project::new(path);
            match crate::inspect::render_inspect(&project) {
                Ok(out) => println!("{out}"),
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::ResumeTrial { path, trial_id } => {
            let project = Project::new(path);
            if let Err(e) = project.ensure_dirs() {
                eprintln!("Error: failed to prepare project dirs: {e}");
                std::process::exit(1);
            }
            let _lock = match project.acquire_lock() {
                Ok(lock) => lock,
                Err(e) => {
                    eprintln!("Error: failed to acquire project lock: {e}");
                    std::process::exit(1);
                }
            };
            // Single read of the unified config: raw text first (for the
            // snapshot comparison), then parse from the same bytes. This
            // avoids re-reading the file for config/template/space/store
            // separately (and the TOCTOU window between those reads);
            // template/space/store construction from the parsed value is
            // infallible.
            let config_text = match std::fs::read_to_string(project.unified_config_path()) {
                Ok(text) => text,
                Err(e) => {
                    eprintln!("Error: failed to load config: {e}");
                    std::process::exit(1);
                }
            };
            let unified: crate::project::UnifiedConfig = match toml::from_str(&config_text) {
                Ok(unified) => unified,
                Err(e) => {
                    eprintln!("Error: failed to load config: {e}");
                    std::process::exit(1);
                }
            };
            let config = crate::project::ProjectConfig::from_sections(
                unified.project,
                unified.sampler,
                unified.scheduler,
            );
            let template = crate::command::CommandTemplate::new(unified.template);
            let space = unified.space;
            if let Err(err) = validate_project_config(
                &config,
                &template,
                &space,
                &template.placeholders().unwrap_or_default(),
            ) {
                eprintln!("Error: {err}");
                std::process::exit(1);
            }
            let store =
                crate::trial::store::TrialStore::new(project.trials_path(), template.clone());
            if let Err(e) = store.ensure_project_config(&config_text, false) {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
            let objective = crate::command::CommandObjective::new(
                store,
                template,
                space,
                project.artifacts_dir(),
                config.metric_key.clone(),
                config.goal,
                config.inject_trial_placeholders,
                *trial_id,
            )
            .with_objectives(config.objectives.clone())
            .with_runner_options(
                if config.scheduler.trial_timeout_s > 0 {
                    Some(std::time::Duration::from_secs(
                        config.scheduler.trial_timeout_s,
                    ))
                } else {
                    None
                },
                None,
            );
            // Deliberately no stale-trial sweep: resume preserves the trial
            // dir (checkpoints, optimizer state). The training binary picks
            // up from its latest checkpoint on its own.
            match objective.resume_trial(*trial_id) {
                Ok(score) => println!("resumed trial {trial_id}: score={score:.6}"),
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Find { dir } => {
            // `dir` is `&Option<PathBuf>` (outer `match &cli.command` borrows);
            // `.as_deref()` flattens to `Option<&Path>` so the Cow only owns
            // the current_dir fallback.
            let root: Cow<Path> = match dir.as_deref() {
                Some(d) => Cow::Borrowed(d),
                None => match std::env::current_dir() {
                    Ok(d) => Cow::Owned(d),
                    Err(e) => {
                        eprintln!("Error: failed to determine current directory: {e}");
                        std::process::exit(1);
                    }
                },
            };
            let projects = crate::find_projects(&root);
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
                    let plan = crate::scheduler::build_plan(&config, *config_id);
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
    fn inspect_requires_project() {
        assert!(
            Cli::try_parse_from(["argtuner", "inspect"]).is_err(),
            "inspect without PROJECT_DIR must fail"
        );
        assert!(
            Cli::try_parse_from(["argtuner", "inspect", "x"]).is_ok(),
            "inspect <dir> must parse"
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
