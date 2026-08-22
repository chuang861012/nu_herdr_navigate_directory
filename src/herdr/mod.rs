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
const MIN_SECRET_CHARS: usize = 4;

fn sanitize_detail(text: &str) -> String {
    sanitize_untrusted(text, &[])
}

fn sanitize_untrusted(text: &str, secrets: &[String]) -> String {
    let cleaned: String = text.chars().filter(|ch| !ch.is_control()).collect();
    let redacted = redact_secrets(&cleaned, secrets);
    redacted.chars().take(MAX_ERROR_DETAIL_CHARS).collect()
}

fn redact_secrets(text: &str, secrets: &[String]) -> String {
    let socket_redacted = redact_socket_paths(text);
    let mut out = redact_herdr_assignments(&socket_redacted);
    let mut secrets: Vec<&str> = secrets
        .iter()
        .map(String::as_str)
        .filter(|secret| secret.chars().count() >= MIN_SECRET_CHARS)
        .collect();
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.chars().count()));
    secrets.dedup();
    for secret in secrets {
        if out.contains(secret) {
            out = out.replace(secret, REDACTED);
        }
    }
    out
}

fn redact_socket_paths(text: &str) -> String {
    let mut out = String::new();
    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find(".sock") {
        let idx = search_from + rel;
        let after_sock = idx + ".sock".len();
        let after = &text[after_sock..];
        if !is_socket_token_end(after) {
            out.push_str(&text[search_from..after_sock]);
            search_from = after_sock;
            continue;
        }
        let prefix = &text[search_from..idx];
        let token_start = prefix
            .rfind(char::is_whitespace)
            .map(|offset| search_from + offset + 1)
            .unwrap_or(search_from);
        out.push_str(&text[search_from..token_start]);
        out.push_str(REDACTED);
        search_from = after_sock;
    }
    out.push_str(&text[search_from..]);
    out
}

fn is_socket_token_end(after: &str) -> bool {
    after.chars().next().is_none_or(|ch| {
        ch.is_whitespace() || matches!(ch, ':' | ',' | ';' | ')' | ']' | '"' | '\'')
    })
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
    use super::{REDACTED, sanitize_detail, sanitize_untrusted};

    #[test]
    fn sanitizes_control_characters_and_length() {
        let cleaned = sanitize_detail("pane\nnot\u{7}found");
        assert_eq!(cleaned, "panenotfound");
        let long = "a".repeat(500);
        assert_eq!(sanitize_detail(&long).len(), 200);
    }

    #[test]
    fn redacts_socket_paths_environment_assignments_and_secrets() {
        let text = "cannot connect to /tmp/nu-plugin-herdr-cd.sock HERDR_SOCKET_PATH=/run/herdr also /run/herdr";
        let cleaned = sanitize_untrusted(text, &["/run/herdr".into()]);
        assert!(!cleaned.contains("/tmp/nu-plugin-herdr-cd.sock"));
        assert!(!cleaned.contains("/run/herdr"));
        assert!(!cleaned.contains("HERDR_SOCKET_PATH=/run/herdr"));
        assert!(cleaned.contains(REDACTED));
        assert!(cleaned.contains("cannot connect to"));
        assert!(
            sanitize_untrusted("pane w1:p1 not found", &[]).contains("w1:p1"),
            "resource ids must remain visible"
        );
    }
}
