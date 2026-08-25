//! Pure decision types, path model, and internal error categories.
//!
//! This layer must stay free of Nushell and Herdr side effects.
//! Command and Herdr layers consume the crate-internal API in later phases.

mod completion;
mod decision;
mod path;
mod types;

pub(crate) use completion::{
    CompletionCandidate, DescriptionData, Evidence, PrefixBound, ScopeLabel, SourceLabel,
    filesystem_path_allowed, merge_candidates, semantic_path_allowed, session_evidence,
};
pub(crate) use decision::{decide, nearest_containing_workspace};
pub(crate) use path::{CanonicalPath, ResolvedPaths, expand_leading_home, resolve_paths};
pub(crate) use types::{
    Action, AgentIdlePolicy, AgentStatus, Caller, ForegroundProcess, Occupant, Pane, PaneId,
    Session, ShellProcessEvidence, Tab, TabId, Workspace, WorkspaceId,
};

/// Internal failure category before conversion to a Nushell `LabeledError`.
///
/// These names are internal and are not a stable machine-readable public API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorKind {
    InvalidPath,
    UnsupportedPlatform,
    InvalidHerdrContext,
    InvalidConfiguration,
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
            Self::InvalidConfiguration => "invalid_configuration",
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

    pub(crate) fn unsupported_platform(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::UnsupportedPlatform, message)
    }

    pub(crate) fn invalid_herdr_context(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidHerdrContext, message)
    }

    pub(crate) fn invalid_configuration(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidConfiguration, message)
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
            (ErrorKind::InvalidConfiguration, "invalid_configuration"),
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
