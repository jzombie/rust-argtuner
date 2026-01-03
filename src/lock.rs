use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

#[derive(Debug)]
pub struct ProjectLock {
    file: std::fs::File,
    path: PathBuf,
}

impl ProjectLock {
    pub fn acquire(lock_path: PathBuf, config_path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&lock_path)?;
        try_lock_exclusive(&file)?;
        let mut guard = Self {
            file,
            path: lock_path,
        };
        guard.write_metadata(config_path)?;
        Ok(guard)
    }

    fn write_metadata(&mut self, config_path: &Path) -> std::io::Result<()> {
        let pid = std::process::id();
        let metadata = format!("pid={pid}\nconfig={}\n", config_path.display());
        self.file.set_len(0)?;
        self.file.write_all(metadata.as_bytes())?;
        self.file.sync_all()?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(unix)]
fn try_lock_exclusive(file: &std::fs::File) -> std::io::Result<()> {
    let fd = file.as_raw_fd();
    let result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn try_lock_exclusive(file: &std::fs::File) -> std::io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    let result = unsafe {
        LockFileEx(
            handle,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if result != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub fn lock_path_for_config(config_path: &Path) -> PathBuf {
    let canonical = config_path
        .canonicalize()
        .unwrap_or_else(|_| config_path.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.to_string_lossy().hash(&mut hasher);
    let hash = hasher.finish();
    std::env::temp_dir()
        .join("argtuner_locks")
        .join(format!("argtuner_{hash}.lock"))
}

#[cfg(test)]
mod tests {
    use super::{ProjectLock, lock_path_for_config};
    use crate::constants::CONFIG_FILENAME;
    use std::path::Path;

    #[test]
    fn lock_prevents_second_acquire() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config_path = temp.path().join(CONFIG_FILENAME);
        std::fs::write(&config_path, "dummy").expect("write");
        let lock_path = lock_path_for_config(&config_path);
        let _first = ProjectLock::acquire(lock_path.clone(), &config_path).expect("lock");
        let second = ProjectLock::acquire(lock_path, Path::new(&config_path));
        assert!(second.is_err());
    }
}
