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

fn sanitize_detail(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control())
        .take(MAX_ERROR_DETAIL_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::sanitize_detail;

    #[test]
    fn sanitizes_control_characters_and_length() {
        let cleaned = sanitize_detail("pane\nnot\u{7}found");
        assert_eq!(cleaned, "panenotfound");
        let long = "a".repeat(500);
        assert_eq!(sanitize_detail(&long).len(), 200);
    }
}
