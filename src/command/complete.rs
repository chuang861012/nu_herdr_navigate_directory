//! Opt-in experimental dynamic completion for the `hnd` directory argument.

use std::collections::HashMap;
use std::fs;
use std::time::{Duration, Instant};

use nu_plugin::DynamicCompletionCall;
use nu_protocol::{DynamicSuggestion, Span, Value, ast::Expr, engine::ArgType};

use super::display::{FilesystemAlias, to_suggestion};
use super::prefix::{TypedPrefix, parse_typed_prefix, resolve_bound};
use super::{CallerEngine, platform_is_supported, read_herdr_mode, read_home};
use crate::domain::{
    CanonicalPath, Evidence, PrefixBound, Session, filesystem_path_allowed, merge_candidates,
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
    let halt_herdr = || engine.interrupted() || Instant::now() >= herdr_deadline;
    if !platform_is_supported() || halt_overall() {
        return None;
    }
    let engine_worker = engine.clone();
    let context = match run_bounded(&halt_herdr, move || {
        if !dynamic_completion_enabled(engine_worker.plugin_config()) {
            return None;
        }
        match read_herdr_mode(&engine_worker) {
            Ok(HerdrMode::Inside(context)) => Some(context),
            _ => None,
        }
    }) {
        Ok(Some(context)) => context,
        _ => return None,
    };
    if engine.interrupted() {
        return None;
    }
    let remaining = herdr_deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return None;
    }
    let (live, session) = match inspect_session_concurrent(&context, remaining, halt_herdr) {
        Ok(SessionInspection::Ready { live, session }) => (live, session),
        Ok(SessionInspection::Stale) | Err(_) => return None,
    };
    if engine.interrupted() {
        return None;
    }
    complete_from_ready(
        engine,
        typed,
        span,
        &live,
        &session,
        halt_herdr,
        halt_overall,
    )
}

struct PreparedCompletion {
    caller_cwd: CanonicalPath,
    home: Option<CanonicalPath>,
    prefix: TypedPrefix,
    bound: Option<PrefixBound>,
    semantic: Vec<(CanonicalPath, Evidence)>,
}

fn prepare_semantic(
    engine: &impl CallerEngine,
    typed: &str,
    live: &LiveCaller,
    session: &Session,
) -> Option<PreparedCompletion> {
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
    Some(PreparedCompletion {
        caller_cwd,
        home,
        prefix,
        bound,
        semantic,
    })
}

fn complete_from_ready(
    engine: &impl CallerEngine,
    typed: &str,
    span: Option<Span>,
    live: &LiveCaller,
    session: &Session,
    halt_herdr: impl Fn() -> bool,
    halt_overall: impl Fn() -> bool,
) -> Option<Vec<DynamicSuggestion>> {
    if engine.interrupted() || halt_overall() {
        return None;
    }
    let engine_worker = engine.clone();
    let typed_owned = typed.to_string();
    let live = live.clone();
    let session = session.clone();
    let (prepared, semantic_suggestions) = match run_bounded(&halt_herdr, move || {
        let prepared = prepare_semantic(&engine_worker, &typed_owned, &live, &session)?;
        let suggestions = suggestions_from(&prepared, &[], span)?;
        Some((prepared, suggestions))
    }) {
        Ok(Some(ready)) => ready,
        _ => return None,
    };
    if engine.interrupted() {
        return None;
    }

    let filesystem = filesystem_candidates(
        &prepared.prefix,
        prepared.bound.as_ref(),
        &prepared.caller_cwd,
        &halt_overall,
    );
    if engine.interrupted() {
        return None;
    }
    if filesystem.is_empty() {
        return Some(semantic_suggestions);
    }

    match run_bounded(&halt_overall, move || {
        suggestions_from(&prepared, &filesystem, span)
    }) {
        Ok(Some(suggestions)) => {
            if engine.interrupted() {
                None
            } else {
                Some(suggestions)
            }
        }
        Ok(None) => None,
        Err(_) if engine.interrupted() => None,
        Err(_) => Some(semantic_suggestions),
    }
}

fn suggestions_from(
    prepared: &PreparedCompletion,
    filesystem: &[(CanonicalPath, FilesystemAlias)],
    span: Option<Span>,
) -> Option<Vec<DynamicSuggestion>> {
    let mut aliases: HashMap<CanonicalPath, Vec<FilesystemAlias>> = HashMap::new();
    for (path, alias) in filesystem {
        aliases.entry(path.clone()).or_default().push(alias.clone());
    }
    let fs_paths = filesystem.iter().map(|(path, _)| path.clone());
    let candidates = merge_candidates(prepared.semantic.clone(), fs_paths, &prepared.caller_cwd)?;
    Some(
        candidates
            .into_iter()
            .map(|candidate| {
                let names = aliases.get(&candidate.path).cloned().unwrap_or_default();
                to_suggestion(
                    candidate,
                    &prepared.prefix,
                    prepared.bound.as_ref(),
                    prepared.home.as_ref(),
                    span,
                    &names,
                )
            })
            .collect(),
    )
}

fn filesystem_candidates(
    prefix: &TypedPrefix,
    bound: Option<&PrefixBound>,
    caller_cwd: &CanonicalPath,
    halt: impl Fn() -> bool,
) -> Vec<(CanonicalPath, FilesystemAlias)> {
    if halt() {
        return Vec::new();
    }
    let remaining = prefix.remaining.clone();
    let bound = bound.cloned();
    let caller = caller_cwd.clone();
    run_bounded(&halt, move || {
        let fs_bound = bound.unwrap_or(PrefixBound {
            base: caller.clone(),
            remaining,
        });
        read_direct_children(fs_bound, caller)
    })
    .unwrap_or_default()
}

fn read_direct_children(
    bound: PrefixBound,
    caller_cwd: CanonicalPath,
) -> Vec<(CanonicalPath, FilesystemAlias)> {
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
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let Some(path) = CanonicalPath::try_directory(entry.path()) else {
            continue;
        };
        if filesystem_path_allowed(&path, name, &caller_cwd, &bound) {
            paths.push((
                path,
                FilesystemAlias {
                    name: name.to_string(),
                    symlink: file_type.is_symlink(),
                },
            ));
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    #[derive(Clone)]
    struct FakeEngine {
        cwd: String,
        env: HashMap<String, Value>,
        config: Result<Option<Value>, Error>,
        interrupted: Arc<AtomicBool>,
        config_delay: Duration,
        cwd_delay: Duration,
        env_delay: Duration,
    }

    impl FakeEngine {
        fn outside(cwd: &str) -> Self {
            Self {
                cwd: cwd.to_string(),
                env: HashMap::new(),
                config: Ok(None),
                interrupted: Arc::new(AtomicBool::new(false)),
                config_delay: Duration::ZERO,
                cwd_delay: Duration::ZERO,
                env_delay: Duration::ZERO,
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
            if !self.cwd_delay.is_zero() {
                thread::sleep(self.cwd_delay);
            }
            Ok(self.cwd.clone())
        }

        fn env_var(&self, name: &str) -> Result<Option<Value>, Error> {
            if !self.env_delay.is_zero() {
                thread::sleep(self.env_delay);
            }
            Ok(self.env.get(name).cloned())
        }

        fn env_vars(&self) -> Result<HashMap<String, Value>, Error> {
            if !self.env_delay.is_zero() {
                thread::sleep(self.env_delay);
            }
            Ok(self.env.clone())
        }

        fn add_env_var(&self, _name: &str, _value: Value) -> Result<(), Error> {
            Ok(())
        }

        fn plugin_config(&self) -> Result<Option<Value>, Error> {
            if !self.config_delay.is_zero() {
                thread::sleep(self.config_delay);
            }
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

    fn warmup_herdr(bin: &str) {
        let _ = std::process::Command::new(bin)
            .args(["api", "snapshot"])
            .output();
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
    fn complete_directory_returns_suggestions_when_enabled() {
        let _cli = lock_cli();
        let root = TempDir::new("complete-dir");
        fs::create_dir(root.path().join("src")).unwrap();
        let (snapshot, current) = snapshot_and_current(root.path().to_str().unwrap(), "");
        let (_dir, bin) = install_fake(&snapshot, &current);
        let engine = enabled_inside(root.path().to_str().unwrap(), &bin);
        warmup_herdr(&bin);
        let suggestions = complete_directory(&engine, "", None, Instant::now())
            .expect("enabled in-process completion must return items");
        assert!(suggestions.iter().any(|item| {
            item.description
                .as_deref()
                .is_some_and(|text| text.contains("agent idle"))
        }));
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
            complete_from_ready(&engine, "", None, &live, &session, || false, || false).unwrap();
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
        let docs = CanonicalPath::directory(&child).unwrap();
        assert!(
            suggestions
                .iter()
                .all(|item| item.value.trim_end_matches('/') != caller.as_str())
        );
        assert!(
            suggestions
                .iter()
                .any(|item| item.value == format!("{}/", docs.as_str())),
            "ordinary empty-prefix children must stay home-relative or absolute, got {:?}",
            suggestions
                .iter()
                .map(|item| item.value.clone())
                .collect::<Vec<_>>()
        );
        assert!(
            suggestions.iter().all(|item| item.value != "docs/"),
            "ordinary empty-prefix children must not collapse to a relative name"
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
        let canonical_real = CanonicalPath::directory(&real).unwrap();
        assert!(children.iter().any(|(path, alias)| {
            path == &canonical_real && alias.name == "real" && !alias.symlink
        }));
        assert!(children.iter().any(|(path, alias)| {
            path == &canonical_real && alias.name == "link" && alias.symlink
        }));
        assert!(!children.iter().any(|(_, alias)| alias.name == "broken"));
        assert!(!children.iter().any(|(_, alias)| alias.name == "file"));
    }

    #[test]
    fn filesystem_symlink_outside_prefix_stays_a_lexical_candidate() {
        let root = TempDir::new("outside-link");
        let outside = TempDir::new("outside-target");
        let linked = root.path().join("link");
        std::os::unix::fs::symlink(outside.path(), &linked).unwrap();
        let bound = PrefixBound {
            base: CanonicalPath::directory(root.path()).unwrap(),
            remaining: "l".into(),
        };
        let caller = CanonicalPath::directory(root.path()).unwrap();
        let children = read_direct_children(bound, caller);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].1.name, "link");
        assert!(children[0].1.symlink);
        assert_eq!(
            children[0].0,
            CanonicalPath::directory(outside.path()).unwrap()
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

    #[test]
    fn herdr_deadline_covers_plugin_config_lookup() {
        let _cli = lock_cli();
        let root = TempDir::new("slow-config");
        let (snapshot, current) = snapshot_and_current(root.path().to_str().unwrap(), "");
        let (dir, bin) = install_fake(&snapshot, &current);
        let mut engine = enabled_inside(root.path().to_str().unwrap(), &bin);
        engine.config_delay = Duration::from_millis(500);
        let started = Instant::now();
        assert!(complete_directory(&engine, "", None, Instant::now()).is_none());
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "config lookup must honor the 200 ms Herdr deadline, elapsed {:?}",
            started.elapsed()
        );
        assert!(
            !fs::read_to_string(dir.path().join("record"))
                .unwrap_or_default()
                .contains("snapshot")
        );
    }

    #[test]
    fn herdr_deadline_covers_caller_environment_lookup() {
        let _cli = lock_cli();
        let root = TempDir::new("slow-env");
        let (snapshot, current) = snapshot_and_current(root.path().to_str().unwrap(), "");
        let (dir, bin) = install_fake(&snapshot, &current);
        let mut engine = enabled_inside(root.path().to_str().unwrap(), &bin);
        engine.env_delay = Duration::from_millis(500);
        let started = Instant::now();
        assert!(complete_directory(&engine, "", None, Instant::now()).is_none());
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "Herdr environment lookup must honor the 200 ms Herdr deadline, elapsed {:?}",
            started.elapsed()
        );
        assert!(
            !fs::read_to_string(dir.path().join("record"))
                .unwrap_or_default()
                .contains("snapshot")
        );
    }

    #[test]
    fn herdr_deadline_covers_caller_cwd_canonicalization() {
        let _cli = lock_cli();
        let root = TempDir::new("slow-cwd");
        fs::create_dir(root.path().join("src")).unwrap();
        let (snapshot, current) = snapshot_and_current(root.path().to_str().unwrap(), "");
        let (_dir, bin) = install_fake(&snapshot, &current);
        let mut engine = enabled_inside(root.path().to_str().unwrap(), &bin);
        engine.cwd_delay = Duration::from_millis(500);
        let started = Instant::now();
        assert!(complete_directory(&engine, "", None, Instant::now()).is_none());
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "cwd canonicalization must honor the 200 ms Herdr deadline, elapsed {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn interruption_after_inspection_discards_suggestions() {
        let _cli = lock_cli();
        let root = TempDir::new("interrupt-ready");
        fs::create_dir(root.path().join("src")).unwrap();
        let (snapshot, current) = snapshot_and_current(root.path().to_str().unwrap(), "");
        let (_dir, bin) = install_fake(&snapshot, &current);
        let engine = enabled_inside(root.path().to_str().unwrap(), &bin);
        engine.interrupted.store(true, Ordering::Relaxed);
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
        assert!(
            complete_from_ready(&engine, "", None, &live, &session, || false, || false).is_none()
        );
    }

    #[test]
    fn overall_deadline_omits_filesystem_but_keeps_semantic() {
        let _cli = lock_cli();
        let root = TempDir::new("overall-fs");
        fs::create_dir(root.path().join("src")).unwrap();
        fs::create_dir(root.path().join("docs")).unwrap();
        let (snapshot, current) = snapshot_and_current(root.path().to_str().unwrap(), "");
        let (_dir, bin) = install_fake(&snapshot, &current);
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
        let overall_checks = AtomicUsize::new(0);
        let halt_overall = || overall_checks.fetch_add(1, Ordering::SeqCst) > 0;
        let suggestions =
            complete_from_ready(&engine, "", None, &live, &session, || false, halt_overall)
                .unwrap();
        assert!(suggestions.iter().any(|item| {
            item.description
                .as_deref()
                .is_some_and(|text| text.contains("agent idle"))
        }));
        assert!(
            suggestions
                .iter()
                .all(|item| item.value.trim_end_matches('/') != "docs")
        );
    }

    #[test]
    fn expired_overall_deadline_after_inspection_returns_none() {
        let _cli = lock_cli();
        let root = TempDir::new("overall-expired");
        fs::create_dir(root.path().join("src")).unwrap();
        let (snapshot, current) = snapshot_and_current(root.path().to_str().unwrap(), "");
        let (_dir, bin) = install_fake(&snapshot, &current);
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
        assert!(
            complete_from_ready(&engine, "", None, &live, &session, || false, || true).is_none()
        );
    }

    #[test]
    fn symlink_alias_is_inserted_instead_of_canonical_target() {
        let _cli = lock_cli();
        let root = TempDir::new("alias-insert");
        let outside = TempDir::new("alias-target");
        fs::create_dir(root.path().join("src")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        let (snapshot, current) = snapshot_and_current(root.path().to_str().unwrap(), "");
        let (_dir, bin) = install_fake(&snapshot, &current);
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
            complete_from_ready(&engine, "", None, &live, &session, || false, || false).unwrap();
        assert!(
            suggestions.iter().any(|item| item.value == "link/"),
            "expected lexical symlink insertion, got {:?}",
            suggestions
                .iter()
                .map(|item| item.value.clone())
                .collect::<Vec<_>>()
        );
        let outside_path = CanonicalPath::directory(outside.path()).unwrap();
        assert!(
            suggestions
                .iter()
                .all(|item| { item.value.trim_end_matches('/') != outside_path.as_str() })
        );
    }

    #[test]
    fn symlink_prefix_match_uses_lexical_name_not_physical_suffix() {
        let _cli = lock_cli();
        let root = TempDir::new("alias-prefix");
        fs::create_dir(root.path().join("src")).unwrap();
        fs::create_dir(root.path().join("lib")).unwrap();
        fs::create_dir(root.path().join("real")).unwrap();
        std::os::unix::fs::symlink(root.path().join("real"), root.path().join("link")).unwrap();
        let extra = format!(
            r#"}}, {{
                    "pane_id": "w1:p3",
                    "terminal_id": "term3",
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "focused": false,
                    "agent_status": "idle",
                    "revision": 3,
                    "agent": "codex",
                    "foreground_cwd": "{}/lib""#,
            root.path().to_str().unwrap()
        );
        let (snapshot, current) = snapshot_and_current(root.path().to_str().unwrap(), &extra);
        let (_dir, bin) = install_fake(&snapshot, &current);
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
            complete_from_ready(&engine, "l", None, &live, &session, || false, || false).unwrap();
        let values: Vec<_> = suggestions.iter().map(|item| item.value.as_str()).collect();
        assert!(values.contains(&"lib/"), "got {values:?}");
        assert!(values.contains(&"link/"), "got {values:?}");
        assert!(!values.contains(&"real/"), "got {values:?}");
    }

    #[test]
    fn same_basename_symlink_alias_is_kept_on_empty_argument() {
        let _cli = lock_cli();
        let root = TempDir::new("same-base");
        let outside = TempDir::new("project");
        fs::create_dir(root.path().join("src")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("project")).unwrap();
        let (snapshot, current) = snapshot_and_current(root.path().to_str().unwrap(), "");
        let (_dir, bin) = install_fake(&snapshot, &current);
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
            complete_from_ready(&engine, "", None, &live, &session, || false, || false).unwrap();
        assert!(
            suggestions.iter().any(|item| item.value == "project/"),
            "same-basename symlink alias must stay selectable, got {:?}",
            suggestions
                .iter()
                .map(|item| item.value.clone())
                .collect::<Vec<_>>()
        );
        let outside_path = CanonicalPath::directory(outside.path()).unwrap();
        assert!(
            suggestions
                .iter()
                .all(|item| { item.value.trim_end_matches('/') != outside_path.as_str() })
        );
    }
}
