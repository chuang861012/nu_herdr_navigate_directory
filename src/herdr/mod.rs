//! Herdr binary selection, CLI transport, JSON protocol, and read-only inspection.
//!
//! This module is the side-effect boundary for talking to Herdr. Focus and create
//! actions are added in a later phase. Transport JSON types do not leak into
//! `domain` or `command`.

#![cfg_attr(not(test), allow(dead_code))]

mod cli;
mod context;
mod inspect;
mod protocol;

#[cfg(test)]
mod test_support;

#[allow(unused_imports)]
pub(crate) use cli::{READ_TIMEOUT, RunError};
#[allow(unused_imports)]
pub(crate) use context::{EnvValue, HerdrMode, InsideContext, classify_herdr_env, inside_context};
#[allow(unused_imports)]
pub(crate) use inspect::{
    LiveCaller, ProcessInspection, SessionInspection, apply_shell_evidence,
    exact_path_shell_candidates, inspect_process, inspect_session,
};

const MAX_ERROR_DETAIL_CHARS: usize = 200;
const REDACTED: &str = "<redacted>";

fn sanitize_detail(text: &str) -> String {
    let cleaned: String = text.chars().filter(|ch| !ch.is_control()).collect();
    let redacted = redact_herdr_assignments(&cleaned);
    redacted.chars().take(MAX_ERROR_DETAIL_CHARS).collect()
}

fn redact_herdr_assignments(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(idx) = rest.find("HERDR_") {
        out.push_str(&rest[..idx]);
        let after_prefix = &rest[idx + "HERDR_".len()..];
        let key_len = after_prefix
            .find(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
            .unwrap_or(after_prefix.len());
        if key_len == 0 {
            out.push_str("HERDR_");
            rest = after_prefix;
            continue;
        }
        let after_key = &after_prefix[key_len..];
        if !after_key.starts_with('=') {
            out.push_str("HERDR_");
            out.push_str(&after_prefix[..key_len]);
            rest = after_key;
            continue;
        }
        let value = &after_key[1..];
        let value_len = value.find(char::is_whitespace).unwrap_or(value.len());
        out.push_str("HERDR_");
        out.push_str(&after_prefix[..key_len]);
        out.push('=');
        out.push_str(REDACTED);
        rest = &value[value_len..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::{REDACTED, sanitize_detail};

    #[test]
    fn sanitizes_control_characters_and_length() {
        let cleaned = sanitize_detail("pane\nnot\u{7}found");
        assert_eq!(cleaned, "panenotfound");
        let long = "a".repeat(500);
        assert_eq!(sanitize_detail(&long).len(), 200);
    }

    #[test]
    fn redacts_environment_assignments_without_hiding_paths_or_resource_ids() {
        let text = "cannot connect to /tmp/nu-plugin-herdr-cd.sock HERDR_SOCKET_PATH=/run/herdr also /run/herdr";
        let cleaned = sanitize_detail(text);
        assert!(cleaned.contains("/tmp/nu-plugin-herdr-cd.sock"));
        assert!(cleaned.contains("also /run/herdr"));
        assert!(!cleaned.contains("HERDR_SOCKET_PATH=/run/herdr"));
        assert!(cleaned.contains(REDACTED));
        assert!(cleaned.contains("cannot connect to"));
        assert!(
            sanitize_detail("pane w1:p1 not found").contains("w1:p1"),
            "resource ids must remain visible"
        );
    }
}
