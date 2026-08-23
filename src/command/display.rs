//! Quoting, terminal-safe display, and suggestion rendering.

use nu_protocol::{DynamicSuggestion, Span, SuggestionKind};

use super::prefix::{TypedPrefix, reconstruct};
use crate::domain::{
    CanonicalPath, CompletionCandidate, DescriptionData, PrefixBound, ScopeLabel, SourceLabel,
};

pub(crate) fn to_suggestion(
    candidate: CompletionCandidate,
    prefix: &TypedPrefix,
    bound: Option<&PrefixBound>,
    home: Option<&CanonicalPath>,
    span: Option<Span>,
) -> DynamicSuggestion {
    let lexical = reconstruct(prefix, &candidate.path, bound, home);
    let display = escape_display(lexical.trim_end_matches('/'));
    let value = if needs_quoting(&lexical) {
        quote_nu(&lexical)
    } else {
        lexical
    };
    DynamicSuggestion {
        value,
        display_override: Some(display),
        description: Some(render_description(&candidate.description)),
        extra: None,
        append_whitespace: false,
        match_indices: None,
        span,
        kind: Some(SuggestionKind::Directory),
    }
}

pub(crate) fn render_description(data: &DescriptionData) -> String {
    let source = match data.source {
        SourceLabel::AgentIdle => "agent idle",
        SourceLabel::AgentDone => "agent done",
        SourceLabel::AgentWorking => "agent working",
        SourceLabel::AgentBlocked => "agent blocked",
        SourceLabel::AgentUnknown => "agent unknown",
        SourceLabel::Workspace => "workspace",
        SourceLabel::ShellPane => "shell pane",
        SourceLabel::Directory => return "directory".into(),
    };
    let scope = match &data.scope {
        ScopeLabel::None => return source.to_string(),
        ScopeLabel::CurrentTab => "current tab".to_string(),
        ScopeLabel::CurrentWorkspace => "current workspace".to_string(),
        ScopeLabel::Workspace { label, number } => {
            let safe = escape_display(label);
            if safe.trim().is_empty() {
                format!("workspace #{number}")
            } else {
                format!("workspace {safe}")
            }
        }
        ScopeLabel::MultipleWorkspaces { count } => format!("{count} workspaces"),
    };
    if data.pane_count > 1 {
        format!("{source} · {scope} · {} panes", data.pane_count)
    } else {
        format!("{source} · {scope}")
    }
}

pub(crate) fn escape_display(input: &str) -> String {
    let mut out = String::new();
    for c in input.chars() {
        if is_unsafe_display_char(c) {
            out.push_str(&format!("\\u{{{:x}}}", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    out
}

fn needs_quoting(path: &str) -> bool {
    path.chars().any(|c| {
        c.is_whitespace()
            || c.is_control()
            || matches!(
                c,
                '\'' | '"'
                    | '`'
                    | '$'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | ';'
                    | '|'
                    | '&'
                    | '<'
                    | '>'
                    | '#'
                    | '\\'
                    | '*'
                    | '?'
                    | '!'
                    | ','
            )
    })
}

fn quote_nu(path: &str) -> String {
    format!("'{}'", path.replace('\'', "''"))
}

fn is_unsafe_display_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00ad}'
                | '\u{061c}'
                | '\u{180e}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'
                | '\u{2066}'..='\u{2069}'
                | '\u{feff}'
        )
}

#[cfg(test)]
mod tests {
    use super::{escape_display, needs_quoting, quote_nu, render_description, to_suggestion};
    use crate::command::prefix::parse_typed_prefix;
    use crate::domain::{
        CanonicalPath, CompletionCandidate, DescriptionData, ScopeLabel, SourceLabel,
    };
    use nu_protocol::{Span, SuggestionKind};

    fn cp(path: &str) -> CanonicalPath {
        CanonicalPath::from_parts_for_test(path)
    }

    #[test]
    fn quotes_spaces_and_syntax_sensitive_names() {
        assert!(needs_quoting("foo bar/"));
        assert!(needs_quoting("foo$bar/"));
        assert!(!needs_quoting("~/src/"));
        assert_eq!(quote_nu("foo bar/"), "'foo bar/'");
        assert_eq!(quote_nu("it's/"), "'it''s/'");
    }

    #[test]
    fn escapes_control_ansi_and_bidi_in_display_only() {
        let dirty = "repo\u{1b}[31m\u{202e}evil";
        let escaped = escape_display(dirty);
        assert!(escaped.contains("\\u{1b}"));
        assert!(escaped.contains("\\u{202e}"));
        assert!(!escaped.contains('\u{1b}'));
        assert!(!escaped.contains('\u{202e}'));
        assert!(escaped.contains("[31m"));
    }

    #[test]
    fn renders_three_segment_descriptions_without_opaque_ids() {
        assert_eq!(
            render_description(&DescriptionData {
                source: SourceLabel::AgentIdle,
                scope: ScopeLabel::CurrentTab,
                pane_count: 1,
            }),
            "agent idle · current tab"
        );
        assert_eq!(
            render_description(&DescriptionData {
                source: SourceLabel::AgentDone,
                scope: ScopeLabel::CurrentWorkspace,
                pane_count: 2,
            }),
            "agent done · current workspace · 2 panes"
        );
        assert_eq!(
            render_description(&DescriptionData {
                source: SourceLabel::Workspace,
                scope: ScopeLabel::Workspace {
                    label: "hc-v2".into(),
                    number: 3,
                },
                pane_count: 3,
            }),
            "workspace · workspace hc-v2 · 3 panes"
        );
        assert_eq!(
            render_description(&DescriptionData {
                source: SourceLabel::ShellPane,
                scope: ScopeLabel::Workspace {
                    label: "hc-v2".into(),
                    number: 3,
                },
                pane_count: 1,
            }),
            "shell pane · workspace hc-v2"
        );
        assert_eq!(
            render_description(&DescriptionData {
                source: SourceLabel::AgentWorking,
                scope: ScopeLabel::Workspace {
                    label: String::new(),
                    number: 4,
                },
                pane_count: 1,
            }),
            "agent working · workspace #4"
        );
        assert_eq!(
            render_description(&DescriptionData {
                source: SourceLabel::Directory,
                scope: ScopeLabel::None,
                pane_count: 0,
            }),
            "directory"
        );
        assert_eq!(
            render_description(&DescriptionData {
                source: SourceLabel::Workspace,
                scope: ScopeLabel::MultipleWorkspaces { count: 2 },
                pane_count: 4,
            }),
            "workspace · 2 workspaces · 4 panes"
        );
        assert_eq!(
            render_description(&DescriptionData {
                source: SourceLabel::Workspace,
                scope: ScopeLabel::Workspace {
                    label: "ok\u{1b}id".into(),
                    number: 1,
                },
                pane_count: 1,
            }),
            "workspace · workspace ok\\u{1b}id"
        );
    }

    #[test]
    fn suggestion_kind_trailing_slash_and_no_whitespace() {
        let suggestion = to_suggestion(
            CompletionCandidate {
                path: cp("/Users/me/src"),
                description: DescriptionData {
                    source: SourceLabel::Directory,
                    scope: ScopeLabel::None,
                    pane_count: 0,
                },
            },
            &parse_typed_prefix(""),
            None,
            Some(&cp("/Users/me")),
            Some(Span::test_data()),
        );
        assert_eq!(suggestion.kind, Some(SuggestionKind::Directory));
        assert!(suggestion.value.ends_with('/'));
        assert!(!suggestion.append_whitespace);
        assert_eq!(suggestion.display_override.as_deref(), Some("~/src"));
        assert_eq!(suggestion.span, Some(Span::test_data()));
        assert_eq!(suggestion.description.as_deref(), Some("directory"));
    }

    #[test]
    fn quoted_replacement_keeps_unquoted_display() {
        let suggestion = to_suggestion(
            CompletionCandidate {
                path: cp("/Users/me/my dir"),
                description: DescriptionData {
                    source: SourceLabel::Directory,
                    scope: ScopeLabel::None,
                    pane_count: 0,
                },
            },
            &parse_typed_prefix(""),
            None,
            Some(&cp("/Users/me")),
            None,
        );
        assert_eq!(suggestion.value, "'~/my dir/'");
        assert_eq!(suggestion.display_override.as_deref(), Some("~/my dir"));
    }
}
