//! Pure decision types, path model, and internal error categories.
//!
//! This layer must stay free of Nushell and Herdr side effects.

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
