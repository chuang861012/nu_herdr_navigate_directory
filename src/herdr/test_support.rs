//! Shared filesystem fixtures for Herdr transport tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

static CLI_LOCK: Mutex<()> = Mutex::new(());

/// Serialize process-spawning tests so 4 MiB and timeout cases cannot starve siblings.
pub(crate) fn lock_cli() -> MutexGuard<'static, ()> {
    CLI_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

pub(crate) struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub(crate) fn new(label: &str) -> Self {
        // Keep names short: macOS Unix socket paths must fit in sockaddr_un.sun_path.
        let path = std::env::temp_dir().join(format!(
            "hnd-{label}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create herdr fixture");
        Self { path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub(crate) fn write_executable(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).expect("write fake herdr");
    chmod(&path, 0o755);
    path
}

#[cfg(unix)]
pub(crate) fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod fake herdr");
}
