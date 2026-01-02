use std::path::PathBuf;

/// Find the workspace root directory by searching upward from this crate's manifest
/// directory for a directory containing `Cargo.toml`.
pub fn workspace_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if dir.join("Cargo.toml").exists() {
            return dir;
        }
        if !dir.pop() {
            panic!("workspace root not found");
        }
    }
}
