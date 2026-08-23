//! Opt-in experimental dynamic completion for the `hnd` directory argument.

use std::fs;
use std::time::{Duration, Instant};

use nu_plugin::DynamicCompletionCall;
use nu_protocol::{DynamicSuggestion, Span, Value, ast::Expr, engine::ArgType};

use super::display::to_suggestion;
use super::orchestrate::check;
use super::prefix::{TypedPrefix, parse_typed_prefix, resolve_bound};
use super::{CallerEngine, platform_is_supported, read_herdr_mode, read_home};
use crate::domain::{
    CanonicalPath, PrefixBound, Session, filesystem_path_allowed, merge_candidates,
    semantic_path_allowed, session_evidence,
};
use crate::herdr::{
    HerdrMode, LiveCaller, SessionInspection, inspect_session_concurrent, run_bounded,
};

/// Herdr enrichment sub-deadline from completion entry.
pub(crate) const HERDR_COMPLETION_DEADLINE: Duration = Duration::from_millis(200);

/// Overall merged completion deadline from completion entry.
pub(crate) const TOTAL_COMPLETION_DEADLINE: Duration = Duration::from_millis(250);

pub(crate) fn complete_path_argument(
    engine: &impl CallerEngine,
    call: DynamicCompletionCall,
    arg_type: ArgType<'_>,
) -> Option<Vec<DynamicSuggestion>> {
    if !matches!(arg_type, ArgType::Positional(0)) {
        return None;
    }
    let started = Instant::now();
    let (typed, span) = typed_argument(&call)?;
    complete_directory(engine, &typed, span, started)
}

pub(crate) fn dynamic_completion_enabled(
    config: Result<Option<Value>, crate::domain::Error>,
) -> bool {
    match config {
        Ok(Some(Value::Record { val, .. })) => matches!(
            val.get("dynamic_completion"),
            Some(Value::Bool { val: true, .. })
        ),
        _ => false,
    }
}

fn typed_argument(call: &DynamicCompletionCall) -> Option<(String, Option<Span>)> {
    let Some(expr) = call.call.positional_iter().next() else {
        return Some((String::new(), None));
    };
    let typed = match &expr.expr {
        Expr::Directory(value, _)
        | Expr::Filepath(value, _)
        | Expr::GlobPattern(value, _)
        | Expr::String(value)
        | Expr::RawString(value) => value.clone(),
        _ => return None,
    };
    Some((typed, Some(expr.span)))
}

fn complete_directory(
    engine: &impl CallerEngine,
    typed: &str,
    span: Option<Span>,
    started: Instant,
) -> Option<Vec<DynamicSuggestion>> {
    let overall_deadline = started + TOTAL_COMPLETION_DEADLINE;
    let herdr_deadline = started + HERDR_COMPLETION_DEADLINE;
    let halt_overall = || engine.interrupted() || Instant::now() >= overall_deadline;
    if !platform_is_supported() || halt_overall() {
        return None;
    }
    if !dynamic_completion_enabled(engine.plugin_config()) {
        return None;
    }

    let halt_herdr = || engine.interrupted() || Instant::now() >= herdr_deadline;
    if check(&halt_herdr, herdr_deadline).is_err() {
        return None;
    }
    let HerdrMode::Inside(context) = read_herdr_mode(engine).ok()? else {
        return None;
    };
    let remaining = herdr_deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return None;
    }
    let (live, session) = match inspect_session_concurrent(&context, remaining, halt_herdr) {
        Ok(SessionInspection::Ready { live, session }) => (live, session),
        Ok(SessionInspection::Stale) | Err(_) => return None,
    };
    if halt_herdr() {
        return None;
    }
    complete_from_ready(engine, typed, span, &live, &session, halt_overall)
}

fn complete_from_ready(
    engine: &impl CallerEngine,
    typed: &str,
    span: Option<Span>,
    live: &LiveCaller,
    session: &Session,
    halt_overall: impl Fn() -> bool,
) -> Option<Vec<DynamicSuggestion>> {
    let caller_cwd = CanonicalPath::directory(engine.current_dir().ok()?).ok()?;
    let home_raw = read_home(engine).ok()?;
    let home = home_raw.as_deref().and_then(CanonicalPath::try_directory);
    let prefix = parse_typed_prefix(typed);
    let bound = if prefix.empty {
        None
    } else {
        Some(resolve_bound(
            &prefix,
            caller_cwd.as_path(),
            home_raw.as_deref(),
        )?)
    };

    let mut semantic = Vec::new();
    for (path, evidence) in session_evidence(session, &live.workspace_id, &live.tab_id) {
        if semantic_path_allowed(&path, &caller_cwd, bound.as_ref()) {
            semantic.push((path, evidence));
        }
    }

    let filesystem = filesystem_candidates(&prefix, bound.as_ref(), &caller_cwd, halt_overall);
    let candidates = merge_candidates(semantic, filesystem, &caller_cwd)?;
    Some(
        candidates
            .into_iter()
            .map(|candidate| to_suggestion(candidate, &prefix, bound.as_ref(), home.as_ref(), span))
            .collect(),
    )
}

fn filesystem_candidates(
    prefix: &TypedPrefix,
    bound: Option<&PrefixBound>,
    caller_cwd: &CanonicalPath,
    halt: impl Fn() -> bool,
) -> Vec<CanonicalPath> {
    if halt() {
        return Vec::new();
    }
    let fs_bound = bound.cloned().unwrap_or(PrefixBound {
        base: caller_cwd.clone(),
        remaining: prefix.remaining.clone(),
    });
    let caller = caller_cwd.clone();
    run_bounded(&halt, move || read_direct_children(fs_bound, caller)).unwrap_or_default()
}

fn read_direct_children(bound: PrefixBound, caller_cwd: CanonicalPath) -> Vec<CanonicalPath> {
    let Ok(entries) = fs::read_dir(bound.base.as_path()) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == "." || name == ".." {
            continue;
        }
        let Some(path) = CanonicalPath::try_directory(entry.path()) else {
            continue;
        };
        if filesystem_path_allowed(&path, &caller_cwd, &bound) {
            paths.push(path);
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::{
        HERDR_COMPLETION_DEADLINE, complete_directory, complete_from_ready,
        dynamic_completion_enabled, read_direct_children,
    };
    use crate::command::CallerEngine;
    use crate::domain::{CanonicalPath, Error, PrefixBound};
    use crate::herdr::test_support::{TempDir, lock_cli, write_executable};
    use crate::herdr::{SessionInspection, inspect_session_concurrent};
    use nu_protocol::{SuggestionKind, Value, record};
    use std::collections::{BTreeMap, HashMap};
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    #[derive(Clone)]
    struct FakeEngine {
        cwd: String,
        env: HashMap<String, Value>,
        config: Result<Option<Value>, Error>,
        interrupted: Arc<AtomicBool>,
    }

    impl FakeEngine {
        fn outside(cwd: &str) -> Self {
            Self {
                cwd: cwd.to_string(),
                env: HashMap::new(),
                config: Ok(None),
                interrupted: Arc::new(AtomicBool::new(false)),
            }
        }

        fn enabled(mut self) -> Self {
            self.config = Ok(Some(Value::test_record(record! {
                "dynamic_completion" => Value::test_bool(true),
            })));
            self
        }

        fn with_herdr(mut self, bin: &str, socket: &str) -> Self {
            self.env.insert("HERDR_ENV".into(), Value::test_string("1"));
            self.env
                .insert("HERDR_BIN_PATH".into(), Value::test_string(bin));
            self.env
                .insert("HERDR_SOCKET_PATH".into(), Value::test_string(socket));
            self.env
                .insert("HERDR_WORKSPACE_ID".into(), Value::test_string("w1"));
            self.env
                .insert("HERDR_TAB_ID".into(), Value::test_string("w1:t1"));
            self.env
                .insert("HERDR_PANE_ID".into(), Value::test_string("w1:p1"));
            self
        }
    }

    impl CallerEngine for FakeEngine {
        fn interrupted(&self) -> bool {
            self.interrupted.load(Ordering::Relaxed)
        }

        fn current_dir(&self) -> Result<String, Error> {
            Ok(self.cwd.clone())
        }

        fn env_var(&self, name: &str) -> Result<Option<Value>, Error> {
            Ok(self.env.get(name).cloned())
        }

        fn env_vars(&self) -> Result<HashMap<String, Value>, Error> {
            Ok(self.env.clone())
        }

        fn add_env_var(&self, _name: &str, _value: Value) -> Result<(), Error> {
            Ok(())
        }

        fn plugin_config(&self) -> Result<Option<Value>, Error> {
            match &self.config {
                Ok(value) => Ok(value.clone()),
                Err(error) => Err(error.clone()),
            }
        }
    }

    fn snapshot_and_current(root: &str, extra_pane: &str) -> (String, String) {
        let snapshot = format!(
            r#"{{
              "id": "cli:session:snapshot",
              "result": {{
                "type": "session_snapshot",
                "snapshot": {{
                  "version": "0.8.2",
                  "protocol": 20,
                  "focused_workspace_id": "w1",
                  "workspaces": [{{
                    "workspace_id": "w1",
                    "number": 1,
                    "label": "repo",
                    "focused": true,
                    "pane_count": 2,
                    "tab_count": 1,
                    "active_tab_id": "w1:t1",
                    "agent_status": "idle",
                    "worktree": {{
                      "repo_key": "k",
                      "repo_name": "n",
                      "repo_root": "{root}",
                      "checkout_path": "{root}",
                      "is_linked_worktree": true
                    }}
                  }}],
                  "tabs": [{{
                    "tab_id": "w1:t1",
                    "workspace_id": "w1",
                    "number": 1,
                    "label": "main",
                    "focused": true,
                    "pane_count": 2,
                    "agent_status": "idle"
                  }}],
                  "panes": [{{
                    "pane_id": "w1:p1",
                    "terminal_id": "term1",
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "focused": true,
                    "agent_status": "idle",
                    "revision": 1,
                    "cwd": "{root}",
                    "foreground_cwd": "{root}"
                  }}, {{
                    "pane_id": "w1:p2",
                    "terminal_id": "term2",
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "focused": false,
                    "agent_status": "idle",
                    "revision": 2,
                    "agent": "codex",
                    "foreground_cwd": "{root}/src"{extra_pane}
                  }}],
                  "layouts": [{{
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "zoomed": false,
                    "area": {{"x":0,"y":0,"width":80,"height":24}},
                    "focused_pane_id": "w1:p1",
                    "panes": [],
                    "splits": []
                  }}],
                  "agents": []
                }}
              }}
            }}"#
        );
        let current = format!(
            r#"{{
              "id": "cli:pane:current",
              "result": {{
                "type": "pane_current",
                "pane": {{
                  "pane_id": "w1:p1",
                  "terminal_id": "term1",
                  "workspace_id": "w1",
                  "tab_id": "w1:t1",
                  "focused": true,
                  "agent_status": "idle",
                  "revision": 1,
                  "foreground_cwd": "{root}"
                }}
              }}
            }}"#
        );
        (snapshot, current)
    }

    fn install_fake(snapshot: &str, current: &str) -> (TempDir, String) {
        let dir = TempDir::new("complete");
        let snapshot_path = dir.path().join("snapshot.json");
        let current_path = dir.path().join("current.json");
        let record = dir.path().join("record");
        fs::write(&snapshot_path, snapshot).unwrap();
        fs::write(&current_path, current).unwrap();
        let bin = write_executable(
            dir.path(),
            "herdr",
            &format!(
                r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {record}
case "$1 $2" in
  "api snapshot") cat {snapshot} ;;
  "pane current") cat {current} ;;
  *) echo "unexpected $*" >&2; exit 2 ;;
esac
"#,
                record = sh_single(&record.display().to_string()),
                snapshot = sh_single(&snapshot_path.display().to_string()),
                current = sh_single(&current_path.display().to_string()),
            ),
        );
        (dir, bin.to_str().unwrap().to_string())
    }

    fn sh_single(path: &str) -> String {
        format!("'{}'", path.replace('\'', r#"'"'"'"#))
    }

    fn enabled_inside(cwd: &str, bin: &str) -> FakeEngine {
        FakeEngine::outside(cwd)
            .enabled()
            .with_herdr(bin, "/tmp/nu-plugin-herdr-navigate-directory.sock")
    }

    #[test]
    fn config_is_strict_and_disabled_by_default() {
        assert!(!dynamic_completion_enabled(Ok(None)));
        assert!(!dynamic_completion_enabled(Ok(Some(Value::test_record(
            record! { "other" => Value::test_bool(true) }
        )))));
        assert!(!dynamic_completion_enabled(Ok(Some(Value::test_record(
            record! { "dynamic_completion" => Value::test_bool(false) }
        )))));
        assert!(!dynamic_completion_enabled(Ok(Some(Value::test_record(
            record! { "dynamic_completion" => Value::test_string("true") }
        )))));
        assert!(!dynamic_completion_enabled(Ok(Some(Value::test_bool(
            true
        )))));
        assert!(!dynamic_completion_enabled(Err(
            Error::invalid_herdr_context("unreadable")
        )));
        assert!(dynamic_completion_enabled(Ok(Some(Value::test_record(
            record! { "dynamic_completion" => Value::test_bool(true) }
        )))));
    }

    #[test]
    fn disabled_config_does_not_inspect_herdr() {
        let _cli = lock_cli();
        let root = TempDir::new("disabled-root");
        let (snapshot, current) = snapshot_and_current(root.path().to_str().unwrap(), "");
        let (dir, bin) = install_fake(&snapshot, &current);
        let engine = FakeEngine::outside(root.path().to_str().unwrap())
            .with_herdr(&bin, "/tmp/nu-plugin-herdr-navigate-directory.sock");
        assert!(complete_directory(&engine, "", None, Instant::now()).is_none());
        assert!(
            !fs::read_to_string(dir.path().join("record"))
                .unwrap_or_default()
                .contains("snapshot")
        );
    }

    #[test]
    fn outside_herdr_falls_back_when_enabled() {
        let cwd = TempDir::new("outside");
        let engine = FakeEngine::outside(cwd.path().to_str().unwrap()).enabled();
        assert!(complete_directory(&engine, "", None, Instant::now()).is_none());
    }

    #[test]
    fn enabled_completion_returns_directory_suggestions() {
        let _cli = lock_cli();
        let root = TempDir::new("enabled-root");
        let src = root.path().join("src");
        fs::create_dir(&src).unwrap();
        let child = root.path().join("docs");
        fs::create_dir(&child).unwrap();
        let (snapshot, current) = snapshot_and_current(root.path().to_str().unwrap(), "");
        let (dir, bin) = install_fake(&snapshot, &current);
        let engine = enabled_inside(root.path().to_str().unwrap(), &bin);
        let context = crate::herdr::inside_context(
            &bin,
            "/tmp/nu-plugin-herdr-navigate-directory.sock",
            "w1",
            "w1:t1",
            "w1:p1",
            BTreeMap::new(),
        )
        .unwrap();
        let SessionInspection::Ready { live, session } =
            inspect_session_concurrent(&context, Duration::from_secs(2), || false).unwrap()
        else {
            panic!("expected ready inspection");
        };
        let suggestions =
            complete_from_ready(&engine, "", None, &live, &session, || false).unwrap();
        assert!(
            suggestions
                .iter()
                .all(|item| item.kind == Some(SuggestionKind::Directory)
                    && item.value.ends_with('/')
                    && !item.append_whitespace)
        );
        assert!(suggestions.iter().any(|item| {
            item.description
                .as_deref()
                .is_some_and(|text| text.contains("agent idle"))
        }));
        let caller = CanonicalPath::directory(root.path()).unwrap();
        assert!(
            suggestions
                .iter()
                .all(|item| item.value.trim_end_matches('/') != caller.as_str())
        );
        let recorded = fs::read_to_string(dir.path().join("record")).unwrap();
        assert!(recorded.contains("api snapshot"));
        assert!(recorded.contains("pane current --current"));
        assert!(!recorded.contains("process-info"));
    }

    #[test]
    fn relative_prefix_does_not_search_globally() {
        let _cli = lock_cli();
        let cwd = TempDir::new("cwd-rel");
        let sibling = TempDir::new("other-ws");
        fs::create_dir(cwd.path().join("src")).unwrap();
        let (snapshot, current) = snapshot_and_current(sibling.path().to_str().unwrap(), "");
        let (_dir, bin) = install_fake(&snapshot, &current);
        let engine = enabled_inside(cwd.path().to_str().unwrap(), &bin);
        assert!(complete_directory(&engine, "s", None, Instant::now()).is_none());
    }

    #[test]
    fn symlink_children_are_included_and_broken_links_are_not() {
        let root = TempDir::new("links");
        let real = root.path().join("real");
        let linked = root.path().join("link");
        let broken = root.path().join("broken");
        let file = root.path().join("file");
        fs::create_dir(&real).unwrap();
        fs::write(&file, b"data").unwrap();
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        std::os::unix::fs::symlink(root.path().join("missing"), &broken).unwrap();
        let bound = PrefixBound {
            base: CanonicalPath::directory(root.path()).unwrap(),
            remaining: String::new(),
        };
        let caller = CanonicalPath::directory(root.path()).unwrap();
        let children = read_direct_children(bound, caller);
        let unique: std::collections::HashSet<_> = children.iter().cloned().collect();
        assert_eq!(unique.len(), 1);
        assert_eq!(
            unique.into_iter().next().unwrap(),
            CanonicalPath::directory(&real).unwrap()
        );
    }

    #[test]
    fn herdr_timeout_falls_back_silently() {
        let _cli = lock_cli();
        let root = TempDir::new("slow");
        let dir = TempDir::new("slow-bin");
        let bin = write_executable(dir.path(), "herdr", "#!/bin/sh\nexec sleep 5\n");
        let engine = enabled_inside(root.path().to_str().unwrap(), bin.to_str().unwrap());
        let started = Instant::now();
        assert!(complete_directory(&engine, "", None, Instant::now()).is_none());
        assert!(started.elapsed() < Duration::from_millis(400));
        assert!(HERDR_COMPLETION_DEADLINE < Duration::from_secs(1));
    }

    #[test]
    fn interruption_returns_none() {
        let cwd = TempDir::new("interrupt");
        let engine = FakeEngine::outside(cwd.path().to_str().unwrap()).enabled();
        engine.interrupted.store(true, Ordering::Relaxed);
        assert!(complete_directory(&engine, "", None, Instant::now()).is_none());
    }

    #[test]
    fn expired_overall_deadline_does_not_start_work() {
        let cwd = TempDir::new("expired");
        let engine = FakeEngine::outside(cwd.path().to_str().unwrap()).enabled();
        let started = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("clock");
        assert!(complete_directory(&engine, "", None, started).is_none());
    }
}
