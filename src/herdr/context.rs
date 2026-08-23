//! Caller Herdr environment classification and injected-binary validation.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::domain::Error;

/// Environment value as observed from the caller, not the plugin process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnvValue {
    String(String),
    Other,
}

/// Top-level Herdr mode derived from `HERDR_ENV`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HerdrMode {
    Outside,
    Inside(InsideContext),
}

/// Validated inside-Herdr caller context used for CLI inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InsideContext {
    pub bin: PathBuf,
    pub socket_path: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub pane_id: String,
    pub herdr_vars: BTreeMap<String, String>,
}

/// Classify `HERDR_ENV`: absent, exactly the string `1`, or malformed.
pub(crate) fn classify_herdr_env(value: Option<&EnvValue>) -> Result<bool, Error> {
    match value {
        None => Ok(false),
        Some(EnvValue::String(value)) if value == "1" => Ok(true),
        Some(_) => Err(Error::invalid_herdr_context(
            "HERDR_ENV must be absent or exactly the string 1",
        )),
    }
}

/// Build a validated inside-Herdr context from caller environment values.
pub(crate) fn inside_context(
    bin_path: &str,
    socket_path: &str,
    workspace_id: &str,
    tab_id: &str,
    pane_id: &str,
    herdr_vars: BTreeMap<String, String>,
) -> Result<InsideContext, Error> {
    let socket_path = require_non_empty("HERDR_SOCKET_PATH", socket_path)?;
    let workspace_id = require_non_empty("HERDR_WORKSPACE_ID", workspace_id)?;
    let tab_id = require_non_empty("HERDR_TAB_ID", tab_id)?;
    let pane_id = require_non_empty("HERDR_PANE_ID", pane_id)?;
    let bin = validate_bin_path(bin_path)?;

    let mut herdr_vars = herdr_vars;
    herdr_vars.remove("HERDR_SESSION");
    herdr_vars.insert("HERDR_ENV".into(), "1".into());
    herdr_vars.insert("HERDR_SOCKET_PATH".into(), socket_path.clone());
    herdr_vars.insert("HERDR_BIN_PATH".into(), bin.display().to_string());
    herdr_vars.insert("HERDR_WORKSPACE_ID".into(), workspace_id.clone());
    herdr_vars.insert("HERDR_TAB_ID".into(), tab_id.clone());
    herdr_vars.insert("HERDR_PANE_ID".into(), pane_id.clone());

    Ok(InsideContext {
        bin,
        socket_path,
        workspace_id,
        tab_id,
        pane_id,
        herdr_vars,
    })
}

fn require_non_empty(name: &str, value: &str) -> Result<String, Error> {
    let value = value.trim();
    if value.is_empty() {
        return Err(Error::invalid_herdr_context(format!(
            "{name} is missing from the Herdr caller context"
        )));
    }
    Ok(value.to_string())
}

fn validate_bin_path(bin_path: &str) -> Result<PathBuf, Error> {
    let bin_path = bin_path.trim();
    if bin_path.is_empty() {
        return Err(Error::invalid_herdr_context(
            "HERDR_BIN_PATH is missing from the Herdr caller context",
        ));
    }

    let path = Path::new(bin_path);
    if !path.is_absolute() {
        return Err(Error::invalid_herdr_context(
            "HERDR_BIN_PATH must be an absolute path",
        ));
    }

    let canonical = fs::canonicalize(path).map_err(|_| invalid_binary())?;
    if !canonical.is_absolute() {
        return Err(invalid_binary());
    }
    let metadata = fs::metadata(&canonical).map_err(|_| invalid_binary())?;
    if !metadata.is_file() {
        return Err(invalid_binary());
    }
    if !is_executable(&canonical) {
        return Err(invalid_binary());
    }
    Ok(canonical)
}

fn invalid_binary() -> Error {
    Error::invalid_herdr_context("Herdr binary is missing, invalid, or not executable")
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    const X_OK: i32 = 1;
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c_path` is a valid C string that lives for the duration of the call.
    unsafe { access(c_path.as_ptr(), X_OK) == 0 }
}

#[cfg(unix)]
unsafe extern "C" {
    fn access(path: *const std::ffi::c_char, amode: i32) -> i32;
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::{EnvValue, classify_herdr_env, inside_context};
    use crate::domain::ErrorKind;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "hnd-herdr-ctx-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).expect("create context fixture");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_executable(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        set_mode(&path, 0o755);
        path
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn herdr_env_absent_is_outside() {
        assert!(!classify_herdr_env(None).unwrap());
    }

    #[test]
    fn herdr_env_string_one_is_inside() {
        assert!(classify_herdr_env(Some(&EnvValue::String("1".into()))).unwrap());
    }

    #[test]
    fn herdr_env_any_other_value_or_type_is_invalid() {
        let cases = [
            EnvValue::String(String::new()),
            EnvValue::String("true".into()),
            EnvValue::String("0".into()),
            EnvValue::String("1 ".into()),
            EnvValue::Other,
        ];
        for value in cases {
            let err = classify_herdr_env(Some(&value)).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::InvalidHerdrContext);
        }
    }

    #[test]
    fn inside_context_accepts_symlink_to_an_executable() {
        let dir = TempDir::new();
        let target = write_executable(dir.path(), "herdr-real");
        let link = dir.path().join("herdr-link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let ctx = inside_context(
            link.to_str().unwrap(),
            "/tmp/herdr.sock",
            "w1",
            "w1:t1",
            "w1:p1",
            BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(ctx.bin, fs::canonicalize(&target).unwrap());
        assert_eq!(ctx.socket_path, "/tmp/herdr.sock");
        assert_eq!(
            ctx.herdr_vars.get("HERDR_BIN_PATH").unwrap(),
            ctx.bin.to_str().unwrap()
        );
        assert!(!ctx.herdr_vars.contains_key("HERDR_SESSION"));
    }

    #[test]
    fn inside_context_rejects_relative_and_non_executable_binaries() {
        let dir = TempDir::new();
        let relative = inside_context(
            "herdr",
            "/tmp/herdr.sock",
            "w1",
            "w1:t1",
            "w1:p1",
            BTreeMap::new(),
        )
        .unwrap_err();
        assert_eq!(relative.kind(), ErrorKind::InvalidHerdrContext);
        assert!(!relative.message().contains("herdr"));

        let not_exec = write_executable(dir.path(), "herdr");
        set_mode(&not_exec, 0o644);
        let err = inside_context(
            not_exec.to_str().unwrap(),
            "/tmp/herdr.sock",
            "w1",
            "w1:t1",
            "w1:p1",
            BTreeMap::new(),
        )
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidHerdrContext);

        let nested = dir.path().join("nested");
        fs::create_dir(&nested).unwrap();
        let dir_err = inside_context(
            nested.to_str().unwrap(),
            "/tmp/herdr.sock",
            "w1",
            "w1:t1",
            "w1:p1",
            BTreeMap::new(),
        )
        .unwrap_err();
        assert_eq!(dir_err.kind(), ErrorKind::InvalidHerdrContext);
    }

    #[test]
    fn missing_required_ids_are_malformed_context() {
        let dir = TempDir::new();
        let bin = write_executable(dir.path(), "herdr");
        let err = inside_context(
            bin.to_str().unwrap(),
            " ",
            "w1",
            "w1:t1",
            "w1:p1",
            BTreeMap::new(),
        )
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidHerdrContext);
        assert!(err.message().contains("HERDR_SOCKET_PATH"));
    }
}
