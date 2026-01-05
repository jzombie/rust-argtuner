use std::path::{Path, PathBuf};

fn command_path(bin: &str) -> Option<PathBuf> {
    let env_key = format!("CARGO_BIN_EXE_{bin}");
    if let Ok(path) = std::env::var(env_key) {
        return Some(PathBuf::from(path));
    }

    let mut path = crate::workspace_root()
        .join("target")
        .join("debug")
        .join(bin);
    if cfg!(windows) {
        path.set_extension("exe");
    }
    if path.exists() {
        return Some(path);
    }
    None
}

fn quote_path(path: &Path) -> String {
    shell_words::quote(&path.to_string_lossy()).to_string()
}

pub fn bin_command(bin: &str) -> String {
    if let Some(path) = command_path(bin) {
        return quote_path(&path);
    }
    let manifest = crate::workspace_root().join("Cargo.toml");
    let manifest_text = manifest.to_string_lossy();
    let manifest_arg = shell_words::quote(&manifest_text);
    format!("cargo run -q --manifest-path {manifest_arg} -p argtuner --bin {bin} --")
}
