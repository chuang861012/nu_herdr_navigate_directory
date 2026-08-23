//! Lexical prefix splitting and reconstruction for dynamic completion.

use std::path::Path;

use crate::domain::{CanonicalPath, PrefixBound, expand_leading_home};

/// User-typed directory argument before physical resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypedPrefix {
    pub raw: String,
    pub lexical_parent: String,
    pub remaining: String,
    pub empty: bool,
}

pub(crate) fn parse_typed_prefix(raw: &str) -> TypedPrefix {
    if raw.is_empty() {
        return TypedPrefix {
            raw: String::new(),
            lexical_parent: String::new(),
            remaining: String::new(),
            empty: true,
        };
    }
    if raw == "~" || raw == "~/" {
        return TypedPrefix {
            raw: raw.to_string(),
            lexical_parent: "~".into(),
            remaining: String::new(),
            empty: false,
        };
    }
    if raw.ends_with('/') {
        return TypedPrefix {
            raw: raw.to_string(),
            lexical_parent: trim_trailing_slash(raw),
            remaining: String::new(),
            empty: false,
        };
    }
    match raw.rsplit_once('/') {
        Some(("", rest)) => TypedPrefix {
            raw: raw.to_string(),
            lexical_parent: "/".into(),
            remaining: rest.to_string(),
            empty: false,
        },
        Some((parent, rest)) => TypedPrefix {
            raw: raw.to_string(),
            lexical_parent: parent.to_string(),
            remaining: rest.to_string(),
            empty: false,
        },
        None => TypedPrefix {
            raw: raw.to_string(),
            lexical_parent: String::new(),
            remaining: raw.to_string(),
            empty: false,
        },
    }
}

pub(crate) fn resolve_bound(
    prefix: &TypedPrefix,
    caller_cwd: &Path,
    home: Option<&str>,
) -> Option<PrefixBound> {
    let parent = if prefix.lexical_parent.is_empty() {
        caller_cwd.to_path_buf()
    } else {
        let expanded = expand_leading_home(&prefix.lexical_parent, home).ok()?;
        if expanded.is_absolute() {
            expanded
        } else {
            caller_cwd.join(expanded)
        }
    };
    Some(PrefixBound {
        base: CanonicalPath::try_directory(parent)?,
        remaining: prefix.remaining.clone(),
    })
}

pub(crate) fn reconstruct(
    prefix: &TypedPrefix,
    candidate: &CanonicalPath,
    bound: Option<&PrefixBound>,
    home: Option<&CanonicalPath>,
) -> String {
    if prefix.empty {
        return empty_display(candidate, home);
    }
    let Some(bound) = bound else {
        return empty_display(candidate, home);
    };
    let suffix = candidate
        .relative_components(&bound.base)
        .unwrap_or_default();
    join_lexical(&prefix.lexical_parent, &suffix)
}

fn empty_display(path: &CanonicalPath, home: Option<&CanonicalPath>) -> String {
    if let Some(home) = home
        && let Some(suffix) = path.relative_components(home)
    {
        if suffix.is_empty() {
            return "~/".into();
        }
        return format!("~/{}/", suffix.join("/"));
    }
    if path.as_str() == "/" {
        "/".into()
    } else {
        format!("{}/", path.as_str())
    }
}

fn join_lexical(parent: &str, parts: &[String]) -> String {
    let mut out = if parent.is_empty() {
        parts.join("/")
    } else if parent == "/" {
        if parts.is_empty() {
            "/".into()
        } else {
            format!("/{}", parts.join("/"))
        }
    } else if parts.is_empty() {
        parent.to_string()
    } else {
        format!("{parent}/{}", parts.join("/"))
    };
    if out.is_empty() {
        out.push('.');
    }
    if !out.ends_with('/') {
        out.push('/');
    }
    out
}

fn trim_trailing_slash(raw: &str) -> String {
    if raw == "/" {
        "/".into()
    } else {
        raw.strip_suffix('/').unwrap_or(raw).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_typed_prefix, reconstruct};
    use crate::domain::CanonicalPath;
    use crate::domain::PrefixBound;

    fn cp(path: &str) -> CanonicalPath {
        CanonicalPath::from_parts_for_test(path)
    }

    #[test]
    fn splits_empty_home_absolute_and_relative_prefixes() {
        let empty = parse_typed_prefix("");
        assert!(empty.empty);
        assert_eq!(empty.remaining, "");

        let home = parse_typed_prefix("~/src");
        assert_eq!(home.lexical_parent, "~");
        assert_eq!(home.remaining, "src");

        let home_dir = parse_typed_prefix("~/src/");
        assert_eq!(home_dir.lexical_parent, "~/src");
        assert_eq!(home_dir.remaining, "");

        let tilde = parse_typed_prefix("~");
        assert_eq!(tilde.lexical_parent, "~");
        assert_eq!(tilde.remaining, "");

        let absolute = parse_typed_prefix("/tmp/foo");
        assert_eq!(absolute.lexical_parent, "/tmp");
        assert_eq!(absolute.remaining, "foo");

        let root_child = parse_typed_prefix("/tmp");
        assert_eq!(root_child.lexical_parent, "/");
        assert_eq!(root_child.remaining, "tmp");

        let relative = parse_typed_prefix("src");
        assert_eq!(relative.lexical_parent, "");
        assert_eq!(relative.remaining, "src");

        let dot = parse_typed_prefix("./src");
        assert_eq!(dot.lexical_parent, ".");
        assert_eq!(dot.remaining, "src");

        let parent = parse_typed_prefix("../docs/");
        assert_eq!(parent.lexical_parent, "../docs");
        assert_eq!(parent.remaining, "");
    }

    #[test]
    fn reconstructs_through_a_symlinked_lexical_prefix() {
        let prefix = parse_typed_prefix("~/src-link/");
        let bound = PrefixBound {
            base: cp("/Volumes/work/src"),
            remaining: String::new(),
        };
        assert_eq!(
            reconstruct(
                &prefix,
                &cp("/Volumes/work/src/hc-v2"),
                Some(&bound),
                Some(&cp("/Users/me"))
            ),
            "~/src-link/hc-v2/"
        );
    }

    #[test]
    fn empty_argument_abbreviates_home_and_keeps_other_paths_absolute() {
        let prefix = parse_typed_prefix("");
        let home = cp("/Users/me");
        assert_eq!(
            reconstruct(&prefix, &cp("/Users/me/src"), None, Some(&home)),
            "~/src/"
        );
        assert_eq!(reconstruct(&prefix, &home, None, Some(&home)), "~/");
        assert_eq!(
            reconstruct(&prefix, &cp("/tmp/other"), None, Some(&home)),
            "/tmp/other/"
        );
    }

    #[test]
    fn preserves_relative_and_absolute_insertion_style() {
        let relative = parse_typed_prefix("./src");
        let bound = PrefixBound {
            base: cp("/cwd"),
            remaining: "src".into(),
        };
        assert_eq!(
            reconstruct(&relative, &cp("/cwd/src"), Some(&bound), None),
            "./src/"
        );

        let absolute = parse_typed_prefix("/tmp/");
        let bound = PrefixBound {
            base: cp("/tmp"),
            remaining: String::new(),
        };
        assert_eq!(
            reconstruct(&absolute, &cp("/tmp/foo"), Some(&bound), None),
            "/tmp/foo/"
        );
    }
}
