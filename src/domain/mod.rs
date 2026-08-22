//! Pure decision types, path model, and internal error categories.
//!
//! This layer must stay free of Nushell and Herdr side effects.
//! Command and Herdr layers consume the crate-internal API in later phases.

#![cfg_attr(not(test), allow(dead_code))]

mod decision;
mod path;
mod types;

#[allow(unused_imports)]
pub(crate) use decision::decide;
#[allow(unused_imports)]
pub(crate) use path::{CanonicalPath, ResolvedPaths, resolve_paths};
#[allow(unused_imports)]
pub(crate) use types::{
    Action, AgentStatus, Caller, ForegroundProcess, Occupant, Pane, PaneId, Session,
    ShellProcessEvidence, Tab, TabId, Workspace, WorkspaceId,
};

/// Internal failure category before conversion to a Nushell `LabeledError`.
///
/// These names are internal in 0.1.0 and are not a stable machine-readable
/// public API. Construction is added as the corresponding failure paths are
/// implemented.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorKind {
    InvalidPath,
    UnsupportedPlatform,
    InvalidHerdrContext,
    IncompatibleHerdr,
    HerdrTimeout,
    HerdrTransport,
    HerdrProtocol,
    HerdrAction,
}

impl ErrorKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::InvalidPath => "invalid_path",
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::InvalidHerdrContext => "invalid_herdr_context",
            Self::IncompatibleHerdr => "incompatible_herdr",
            Self::HerdrTimeout => "herdr_timeout",
            Self::HerdrTransport => "herdr_transport",
            Self::HerdrProtocol => "herdr_protocol",
            Self::HerdrAction => "herdr_action",
        }
    }
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Domain-layer failure with an internal kind and a bounded human-readable message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Error {
    kind: ErrorKind,
    message: String,
}

impl Error {
    pub(crate) fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub(crate) fn invalid_path(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidPath, message)
    }

    pub(crate) fn invalid_herdr_context(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidHerdrContext, message)
    }

    pub(crate) fn incompatible_herdr(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::IncompatibleHerdr, message)
    }

    pub(crate) fn herdr_timeout(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::HerdrTimeout, message)
    }

    pub(crate) fn herdr_transport(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::HerdrTransport, message)
    }

    pub(crate) fn herdr_protocol(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::HerdrProtocol, message)
    }

    #[allow(dead_code)]
    pub(crate) fn herdr_action(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::HerdrAction, message)
    }

    pub(crate) fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::ErrorKind;

    #[test]
    fn error_kind_names_match_the_design() {
        let cases = [
            (ErrorKind::InvalidPath, "invalid_path"),
            (ErrorKind::UnsupportedPlatform, "unsupported_platform"),
            (ErrorKind::InvalidHerdrContext, "invalid_herdr_context"),
            (ErrorKind::IncompatibleHerdr, "incompatible_herdr"),
            (ErrorKind::HerdrTimeout, "herdr_timeout"),
            (ErrorKind::HerdrTransport, "herdr_transport"),
            (ErrorKind::HerdrProtocol, "herdr_protocol"),
            (ErrorKind::HerdrAction, "herdr_action"),
        ];

        for (kind, expected) in cases {
            assert_eq!(kind.as_str(), expected);
            assert_eq!(kind.to_string(), expected);
        }
    }
}
