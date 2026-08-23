//! Canonical physical paths and caller-relative target resolution.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use super::{Error, ErrorKind};

/// Absolute UTF-8 physical path after `.`, `..`, and symlink resolution.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CanonicalPath(PathBuf);

/// Canonical caller cwd and target used by later decision and command layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedPaths {
    pub caller_cwd: CanonicalPath,
    pub target: CanonicalPath,
}

impl CanonicalPath {
    /// Canonicalize an absolute existing enterable directory to a UTF-8 path.
    pub(crate) fn directory(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(Error::invalid_path(format!(
                "path must be absolute: {}",
                path.display()
            )));
        }

        let canonical = fs::canonicalize(path).map_err(|err| map_io_error(path, err))?;
        if !canonical.is_dir() {
            return Err(Error::invalid_path(format!(
                "path is not a directory: {}",
                canonical.display()
            )));
        }
        require_enterable(&canonical)?;
        require_utf8(&canonical)?;
        Ok(Self(canonical))
    }

    /// Canonicalize an optional workspace root or pane cwd.
    ///
    /// Any failure excludes the path from matching instead of failing the command.
    pub(crate) fn try_directory(path: impl AsRef<Path>) -> Option<Self> {
        Self::directory(path).ok()
    }

    pub(crate) fn as_path(&self) -> &Path {
        &self.0
    }

    pub(crate) fn as_str(&self) -> &str {
        self.0.to_str().expect("canonical paths are UTF-8")
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn into_path_buf(self) -> PathBuf {
        self.0
    }

    /// Component-aware containment, including equality.
    pub(crate) fn contains(&self, other: &Self) -> bool {
        let ancestor: Vec<_> = self.0.components().collect();
        let descendant: Vec<_> = other.0.components().collect();
        descendant.starts_with(&ancestor)
    }

    /// True when `other` is inside this path but is not this path.
    pub(crate) fn is_strict_ancestor_of(&self, other: &Self) -> bool {
        self.contains(other) && self != other
    }

    /// Workspace-root depth: component count after the filesystem root.
    pub(crate) fn depth(&self) -> usize {
        self.0
            .components()
            .filter(|component| !matches!(component, Component::RootDir | Component::Prefix(_)))
            .count()
    }

    #[cfg(test)]
    pub(crate) fn from_parts_for_test(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        assert!(
            path.is_absolute(),
            "test canonical paths must be absolute: {}",
            path.display()
        );
        require_utf8(&path).expect("test canonical paths must be UTF-8");
        Self(path)
    }
}

impl AsRef<Path> for CanonicalPath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl std::fmt::Display for CanonicalPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Expand only `~` and a leading `~/`, resolve against the caller cwd, then canonicalize.
pub(crate) fn resolve_paths(
    caller_cwd: &Path,
    target: &str,
    home: Option<&str>,
) -> Result<ResolvedPaths, Error> {
    if target.is_empty() {
        return Err(Error::invalid_path("target path is empty"));
    }
    if !caller_cwd.is_absolute() {
        return Err(Error::invalid_path(format!(
            "caller working directory must be an absolute path: {}",
            caller_cwd.display()
        )));
    }

    let caller_cwd = CanonicalPath::directory(caller_cwd)?;
    let expanded = expand_leading_home(target, home)?;
    let absolute_target = if expanded.is_absolute() {
        expanded
    } else {
        caller_cwd.as_path().join(expanded)
    };
    let target = CanonicalPath::directory(&absolute_target)?;

    Ok(ResolvedPaths { caller_cwd, target })
}

fn expand_leading_home(target: &str, home: Option<&str>) -> Result<PathBuf, Error> {
    if target != "~" && !target.starts_with("~/") {
        return Ok(PathBuf::from(target));
    }

    let Some(home) = home.filter(|value| !value.is_empty()) else {
        return Err(Error::invalid_path(
            "home directory is unavailable for ~ expansion",
        ));
    };
    let home = Path::new(home);
    if !home.is_absolute() {
        return Err(Error::invalid_path(format!(
            "home directory must be an absolute path: {home}",
            home = home.display()
        )));
    }

    if target == "~" {
        return Ok(home.to_path_buf());
    }

    Ok(home.join(&target["~/".len()..]))
}

fn require_utf8(path: &Path) -> Result<(), Error> {
    if path.to_str().is_none() {
        return Err(Error::invalid_path(format!(
            "path is not valid UTF-8: {}",
            path.display()
        )));
    }
    Ok(())
}

fn require_enterable(path: &Path) -> Result<(), Error> {
    if can_search_directory(path) {
        Ok(())
    } else {
        Err(Error::invalid_path(format!(
            "path is not enterable: {}",
            path.display()
        )))
    }
}

/// Search (execute) permission is the POSIX `chdir` requirement.
#[cfg(unix)]
fn can_search_directory(path: &Path) -> bool {
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
fn can_search_directory(_path: &Path) -> bool {
    false
}

fn map_io_error(path: &Path, err: io::Error) -> Error {
    let message = match err.kind() {
        io::ErrorKind::NotFound => format!("path does not exist: {}", path.display()),
        io::ErrorKind::NotADirectory => {
            format!("path is not a directory: {}", path.display())
        }
        io::ErrorKind::PermissionDenied => {
            format!("path is not enterable: {}", path.display())
        }
        _ => format!("path cannot be resolved: {}", path.display()),
    };
    Error::new(ErrorKind::InvalidPath, message)
}

#[cfg(test)]
mod tests {
    use super::{CanonicalPath, expand_leading_home, require_utf8, resolve_paths};
    use crate::domain::ErrorKind;
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
                "hnd-path-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).expect("create path fixture");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn utf8(&self) -> &str {
            self.path.to_str().expect("temp path is UTF-8")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn cp(path: &str) -> CanonicalPath {
        CanonicalPath::from_parts_for_test(path)
    }

    #[test]
    fn containment_is_component_aware_and_includes_equality() {
        let repo = cp("/repo");
        let src = cp("/repo/src");
        let repo_a = cp("/repo-a");
        let repo_ab = cp("/repo-ab");
        let root = cp("/");

        assert!(repo.contains(&repo));
        assert!(repo.contains(&src));
        assert!(repo.is_strict_ancestor_of(&src));
        assert!(!src.is_strict_ancestor_of(&repo));
        assert!(!repo.is_strict_ancestor_of(&repo));
        assert!(!repo_a.contains(&repo_ab));
        assert!(!repo.contains(&repo_a));
        assert!(root.contains(&repo));
        assert!(root.is_strict_ancestor_of(&src));
        assert_eq!(root.depth(), 0);
        assert_eq!(repo.depth(), 1);
        assert_eq!(src.depth(), 2);
    }

    #[test]
    fn expands_only_tilde_and_leading_tilde_slash() {
        let home = "/Users/example";
        assert_eq!(
            expand_leading_home("~", Some(home)).unwrap(),
            PathBuf::from(home)
        );
        assert_eq!(
            expand_leading_home("~/", Some(home)).unwrap(),
            PathBuf::from(home)
        );
        assert_eq!(
            expand_leading_home("~/src", Some(home)).unwrap(),
            PathBuf::from("/Users/example/src")
        );
        assert_eq!(
            expand_leading_home("~otheruser", Some(home)).unwrap(),
            PathBuf::from("~otheruser")
        );
        assert_eq!(
            expand_leading_home("~otheruser/src", Some(home)).unwrap(),
            PathBuf::from("~otheruser/src")
        );
        assert_eq!(
            expand_leading_home("/abs", Some(home)).unwrap(),
            PathBuf::from("/abs")
        );

        let err = expand_leading_home("~", None).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidPath);
    }

    #[test]
    fn resolve_relative_dot_and_parent_against_caller_cwd() {
        let root = TempDir::new();
        let nested = root.path().join("repo").join("src");
        fs::create_dir_all(&nested).unwrap();
        let sibling = root.path().join("repo").join("docs");
        fs::create_dir_all(&sibling).unwrap();

        let same = resolve_paths(&nested, ".", None).unwrap();
        assert_eq!(same.caller_cwd, same.target);
        assert_eq!(same.target, CanonicalPath::directory(&nested).unwrap());

        let parent = resolve_paths(&nested, "..", None).unwrap();
        assert_eq!(
            parent.target,
            CanonicalPath::directory(root.path().join("repo")).unwrap()
        );
        assert!(!parent.caller_cwd.is_strict_ancestor_of(&parent.target));
        assert!(parent.target.is_strict_ancestor_of(&parent.caller_cwd));

        let rel = resolve_paths(&nested, "../docs", None).unwrap();
        assert_eq!(rel.target, CanonicalPath::directory(&sibling).unwrap());
    }

    #[test]
    fn resolve_absolute_and_home_targets() {
        let cwd = TempDir::new();
        let home = TempDir::new();
        let project = home.path().join("project");
        fs::create_dir(&project).unwrap();

        let absolute = resolve_paths(cwd.path(), home.utf8(), None).unwrap();
        assert_eq!(
            absolute.target,
            CanonicalPath::directory(home.path()).unwrap()
        );

        let from_home = resolve_paths(cwd.path(), "~/project", Some(home.utf8())).unwrap();
        assert_eq!(
            from_home.target,
            CanonicalPath::directory(&project).unwrap()
        );

        let home_only = resolve_paths(cwd.path(), "~", Some(home.utf8())).unwrap();
        assert_eq!(
            home_only.target,
            CanonicalPath::directory(home.path()).unwrap()
        );
    }

    #[test]
    fn does_not_expand_other_user_tilde_and_resolves_it_relative_to_cwd() {
        let cwd = TempDir::new();
        let named = cwd.path().join("~otheruser");
        fs::create_dir(&named).unwrap();
        let home = TempDir::new();

        let resolved = resolve_paths(cwd.path(), "~otheruser", Some(home.utf8())).unwrap();
        assert_eq!(resolved.target, CanonicalPath::directory(&named).unwrap());
    }

    #[test]
    fn canonicalizes_symbolic_link_identity() {
        let root = TempDir::new();
        let real = root.path().join("real");
        let linked = root.path().join("link");
        fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &linked).unwrap();

        let via_link = resolve_paths(root.path(), "link", None).unwrap();
        let via_real = resolve_paths(root.path(), "real", None).unwrap();
        assert_eq!(via_link.target, via_real.target);
        assert_eq!(via_link.target, CanonicalPath::directory(&real).unwrap());
    }

    #[test]
    fn rejects_missing_file_and_non_directory_targets() {
        let root = TempDir::new();
        let file = root.path().join("file.txt");
        fs::write(&file, b"data").unwrap();

        let missing = resolve_paths(root.path(), "absent", None).unwrap_err();
        assert_eq!(missing.kind(), ErrorKind::InvalidPath);
        assert!(missing.message().contains("does not exist"));

        let not_dir = resolve_paths(root.path(), "file.txt", None).unwrap_err();
        assert_eq!(not_dir.kind(), ErrorKind::InvalidPath);
        assert!(not_dir.message().contains("not a directory"));
    }

    #[cfg(unix)]
    struct RestoreMode<'a> {
        path: &'a Path,
    }

    #[cfg(unix)]
    impl Drop for RestoreMode<'_> {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(self.path, fs::Permissions::from_mode(0o755));
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_enterable_directory() {
        use std::os::unix::fs::PermissionsExt;

        let root = TempDir::new();
        let locked = root.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let _restore = RestoreMode { path: &locked };

        let err = resolve_paths(root.path(), "locked", None).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidPath);
        assert!(err.message().contains("not enterable"));
    }

    #[cfg(unix)]
    #[test]
    fn accepts_execute_only_directory() {
        use std::os::unix::fs::PermissionsExt;

        let root = TempDir::new();
        let searchable = root.path().join("searchable");
        fs::create_dir(&searchable).unwrap();
        fs::set_permissions(&searchable, fs::Permissions::from_mode(0o100)).unwrap();
        let _restore = RestoreMode { path: &searchable };

        let resolved = resolve_paths(root.path(), "searchable", None).unwrap();
        assert!(resolved.target.as_path().ends_with("searchable"));
    }

    #[test]
    fn optional_directory_excludes_invalid_paths_without_failing() {
        let root = TempDir::new();
        let file = root.path().join("file.txt");
        fs::write(&file, b"data").unwrap();

        assert!(CanonicalPath::try_directory(root.path().join("missing")).is_none());
        assert!(CanonicalPath::try_directory(&file).is_none());
        assert!(CanonicalPath::try_directory("relative").is_none());
        assert!(CanonicalPath::try_directory(root.path()).is_some());
    }

    #[test]
    fn require_utf8_rejects_non_utf8_bytes() {
        use std::os::unix::ffi::OsStrExt;

        let valid = Path::new("/tmp");
        assert!(require_utf8(valid).is_ok());

        let invalid = Path::new(std::ffi::OsStr::from_bytes(b"/\xff"));
        let err = require_utf8(invalid).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidPath);
        assert!(err.message().contains("not valid UTF-8"));
    }

    #[test]
    fn empty_target_and_relative_caller_cwd_are_invalid() {
        let root = TempDir::new();
        let empty = resolve_paths(root.path(), "", None).unwrap_err();
        assert_eq!(empty.kind(), ErrorKind::InvalidPath);

        let relative = resolve_paths(Path::new("relative-cwd"), ".", None).unwrap_err();
        assert_eq!(relative.kind(), ErrorKind::InvalidPath);
    }

    #[test]
    fn resolves_paths_with_spaces() {
        let root = TempDir::new();
        let spaced = root.path().join("my dir");
        fs::create_dir(&spaced).unwrap();

        let resolved = resolve_paths(root.path(), "my dir", None).unwrap();
        let expected = CanonicalPath::directory(&spaced).unwrap();
        assert_eq!(resolved.target, expected);
        assert_eq!(resolved.target.clone().into_path_buf(), expected.as_path());
        assert_eq!(resolved.target.as_str(), expected.as_str());
    }
}
