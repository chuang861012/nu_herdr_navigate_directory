//! Inspect, decide, recheck, and act for one `hcd` invocation.

use std::time::Instant;

use crate::domain::{
    Action, Caller, CanonicalPath, Error, Occupant, PaneId, ResolvedPaths, Session, decide,
};
use crate::herdr::{
    CommandResult, FocusResult, HerdrMode, InsideContext, ProcessInspection, RunError,
    SessionInspection, apply_shell_evidence, create_tab, create_workspace,
    exact_path_shell_candidates, focus_pane, inspect_process, inspect_session,
};

/// Entire `hcd` invocation deadline, including the one allowed recomputation.
pub(crate) const TOTAL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Side effect visible at the Nushell boundary after Herdr work completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Silent,
    ChangeDirectory { path: CanonicalPath },
}

struct SessionView {
    caller: Caller,
    session: Session,
}

enum EvidenceFill {
    Ready,
    NeedRecompute,
}

enum Executed {
    Done(Outcome),
    NeedRecompute,
}

/// Run the approved outside/inside `hcd` loop. Path resolution is already done.
pub(crate) fn orchestrate(
    paths: &ResolvedPaths,
    mode: &HerdrMode,
    interrupted: &dyn Fn() -> bool,
    deadline: Instant,
) -> Result<Outcome, RunError> {
    check(interrupted, deadline)?;
    match mode {
        HerdrMode::Outside => Ok(Outcome::ChangeDirectory {
            path: paths.target.clone(),
        }),
        HerdrMode::Inside(context) => orchestrate_inside(paths, context, interrupted, deadline),
    }
}

fn orchestrate_inside(
    paths: &ResolvedPaths,
    context: &InsideContext,
    interrupted: &dyn Fn() -> bool,
    deadline: Instant,
) -> Result<Outcome, RunError> {
    let halt = || interrupted() || Instant::now() >= deadline;
    let map = |error| map_halt(error, interrupted);

    let (view, mut recomputed) =
        prepare_view(paths, context, false, &halt, interrupted, deadline).map_err(&map)?;
    let mut action = decide(&view.caller, &view.session, &paths.target);

    if is_create(&action) && !recomputed {
        check(interrupted, deadline)?;
        let (view, _) =
            prepare_view(paths, context, true, &halt, interrupted, deadline).map_err(&map)?;
        recomputed = true;
        action = decide(&view.caller, &view.session, &paths.target);
    }

    check(interrupted, deadline)?;
    match execute_action(context, &action, recomputed, &halt).map_err(&map)? {
        Executed::Done(outcome) => Ok(outcome),
        Executed::NeedRecompute => {
            let (view, _) =
                prepare_view(paths, context, true, &halt, interrupted, deadline).map_err(&map)?;
            let action = decide(&view.caller, &view.session, &paths.target);
            check(interrupted, deadline)?;
            match execute_action(context, &action, true, &halt).map_err(&map)? {
                Executed::Done(outcome) => Ok(outcome),
                Executed::NeedRecompute => {
                    Err(Error::herdr_action("Herdr state changed again after recomputation").into())
                }
            }
        }
    }
}

fn prepare_view(
    paths: &ResolvedPaths,
    context: &InsideContext,
    mut recomputed: bool,
    halt: &impl Fn() -> bool,
    interrupted: &dyn Fn() -> bool,
    deadline: Instant,
) -> Result<(SessionView, bool), RunError> {
    loop {
        check(interrupted, deadline)?;
        let Some(mut view) = load_view(paths, context, halt)? else {
            if recomputed {
                return Err(Error::herdr_protocol(
                    "live caller is absent from the session snapshot",
                )
                .into());
            }
            recomputed = true;
            continue;
        };
        if view.caller.cwd == paths.target {
            return Ok((view, recomputed));
        }
        match fill_shell_evidence(&mut view.session, &paths.target, context, !recomputed, halt)? {
            EvidenceFill::Ready => return Ok((view, recomputed)),
            EvidenceFill::NeedRecompute => {
                recomputed = true;
            }
        }
    }
}

fn load_view(
    paths: &ResolvedPaths,
    context: &InsideContext,
    halt: &impl Fn() -> bool,
) -> Result<Option<SessionView>, RunError> {
    match inspect_session(context, halt)? {
        SessionInspection::Stale => Ok(None),
        SessionInspection::Ready { live, session } => Ok(Some(SessionView {
            caller: Caller {
                cwd: paths.caller_cwd.clone(),
                workspace_id: live.workspace_id,
                tab_id: live.tab_id,
                pane_id: live.pane_id,
            },
            session,
        })),
    }
}

fn fill_shell_evidence(
    session: &mut Session,
    target: &CanonicalPath,
    context: &InsideContext,
    allow_recompute: bool,
    halt: &impl Fn() -> bool,
) -> Result<EvidenceFill, RunError> {
    let pane_ids: Vec<PaneId> = exact_path_shell_candidates(session, target)
        .into_iter()
        .filter(|pane| matches!(pane.occupant, Occupant::Shell(None)))
        .map(|pane| pane.id.clone())
        .collect();

    for pane_id in pane_ids {
        match inspect_process(context, &pane_id, halt)? {
            ProcessInspection::Evidence(evidence) => {
                apply_shell_evidence(session, &pane_id, evidence);
            }
            ProcessInspection::NotFound if allow_recompute => {
                return Ok(EvidenceFill::NeedRecompute);
            }
            ProcessInspection::NotFound => {}
        }
    }
    Ok(EvidenceFill::Ready)
}

fn execute_action(
    context: &InsideContext,
    action: &Action,
    recomputed: bool,
    halt: &impl Fn() -> bool,
) -> Result<Executed, RunError> {
    match action {
        Action::NoOp => Ok(Executed::Done(Outcome::Silent)),
        Action::ChangeDirectory { path } => Ok(Executed::Done(Outcome::ChangeDirectory {
            path: path.clone(),
        })),
        Action::FocusPane { pane_id } => match focus_pane(context, pane_id, halt)? {
            FocusResult::Focused => Ok(Executed::Done(Outcome::Silent)),
            FocusResult::NotFound { .. } if !recomputed => Ok(Executed::NeedRecompute),
            FocusResult::NotFound { code, message } => Err(Error::herdr_action(format!(
                "pane focus failed after recomputation: {code}: {message}"
            ))
            .into()),
        },
        Action::CreateTab { workspace_id, cwd } => {
            match create_tab(context, workspace_id, cwd, halt)? {
                CommandResult::Ok(_) => Ok(Executed::Done(Outcome::Silent)),
                CommandResult::NotFound { .. } if !recomputed => Ok(Executed::NeedRecompute),
                CommandResult::NotFound { code, message } => Err(Error::herdr_action(format!(
                    "tab creation failed after recomputation: {code}: {message}"
                ))
                .into()),
            }
        }
        Action::CreateWorkspace { cwd } => match create_workspace(context, cwd, halt)? {
            CommandResult::Ok(_) => Ok(Executed::Done(Outcome::Silent)),
            CommandResult::NotFound { .. } if !recomputed => Ok(Executed::NeedRecompute),
            CommandResult::NotFound { code, message } => Err(Error::herdr_action(format!(
                "workspace creation failed after recomputation: {code}: {message}"
            ))
            .into()),
        },
    }
}

fn is_create(action: &Action) -> bool {
    matches!(
        action,
        Action::CreateTab { .. } | Action::CreateWorkspace { .. }
    )
}

fn check(interrupted: &dyn Fn() -> bool, deadline: Instant) -> Result<(), RunError> {
    if interrupted() {
        Err(RunError::Interrupted)
    } else if Instant::now() >= deadline {
        Err(RunError::Failed(Error::herdr_timeout(
            "hcd exceeded the 10-second deadline",
        )))
    } else {
        Ok(())
    }
}

fn map_halt(error: RunError, interrupted: &dyn Fn() -> bool) -> RunError {
    match error {
        RunError::Interrupted if interrupted() => RunError::Interrupted,
        RunError::Interrupted => {
            RunError::Failed(Error::herdr_timeout("hcd exceeded the 10-second deadline"))
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::{Outcome, TOTAL_DEADLINE, orchestrate};
    use crate::domain::{CanonicalPath, ErrorKind, ResolvedPaths, resolve_paths};
    use crate::herdr::test_support::{TempDir, lock_cli, write_executable};
    use crate::herdr::{HerdrMode, RunError, inside_context};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::sync::MutexGuard;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    struct World {
        _lock: MutexGuard<'static, ()>,
        dir: TempDir,
        context: crate::herdr::InsideContext,
        repo: PathBuf,
        src: PathBuf,
        docs: PathBuf,
        other: PathBuf,
    }

    enum FocusReply {
        Ok,
        NotFound,
    }

    impl World {
        fn new() -> Self {
            let lock = lock_cli();
            let dir = TempDir::new("orch");
            let repo = dir.path().join("fs/repo");
            let src = repo.join("src");
            let docs = repo.join("docs");
            let other = dir.path().join("fs/other");
            fs::create_dir_all(&src).unwrap();
            fs::create_dir_all(&docs).unwrap();
            fs::create_dir_all(&other).unwrap();
            fs::create_dir_all(dir.path().join("proc")).unwrap();

            let root = dir.path().display().to_string();
            let bin = write_executable(
                dir.path(),
                "herdr",
                &format!(
                    r#"#!/bin/sh
set -eu
ROOT={root}
printf '%s\n' "$*" >> "$ROOT/record"
case "$1 $2" in
  "api snapshot")
    if [ -f "$ROOT/sleep_snapshot" ]; then sleep "$(cat "$ROOT/sleep_snapshot")"; fi
    n=$(cat "$ROOT/snap_count")
    echo $((n+1)) > "$ROOT/snap_count"
    if [ "$n" -ge 1 ] && [ -f "$ROOT/snapshot2.json" ]; then cat "$ROOT/snapshot2.json"; else cat "$ROOT/snapshot1.json"; fi
    ;;
  "pane current")
    n=$(cat "$ROOT/cur_count")
    echo $((n+1)) > "$ROOT/cur_count"
    if [ "$n" -ge 1 ] && [ -f "$ROOT/current2.json" ]; then cat "$ROOT/current2.json"; else cat "$ROOT/current1.json"; fi
    ;;
  "pane process-info")
    pane="$4"
    if [ -f "$ROOT/sleep_process" ]; then sleep "$(cat "$ROOT/sleep_process")"; fi
    if [ -f "$ROOT/proc/${{pane}}.nf" ]; then
      printf '%s\n' '{{"id":"x","error":{{"code":"not_found","message":"pane not found"}}}}' >&2
      exit 1
    fi
    if [ -f "$ROOT/proc/${{pane}}.json" ]; then
      cat "$ROOT/proc/${{pane}}.json"
    else
      printf '%s\n' "{{\"id\":\"cli:pane:process-info\",\"result\":{{\"type\":\"pane_process_info\",\"process_info\":{{\"pane_id\":\"${{pane}}\",\"shell_pid\":42,\"foreground_process_group_id\":42,\"foreground_processes\":[{{\"pid\":42,\"name\":\"zsh\"}}]}}}}}}"
    fi
    ;;
  "tab create")
    cat "$ROOT/tab_created.json"
    ;;
  "workspace create")
    cat "$ROOT/ws_created.json"
    ;;
  *)
    printf 'unexpected %s\n' "$*" >&2
    exit 2
    ;;
esac
"#,
                    root = sh_single(&root),
                ),
            );

            fs::write(dir.path().join("snap_count"), "0\n").unwrap();
            fs::write(dir.path().join("cur_count"), "0\n").unwrap();
            fs::write(dir.path().join("record"), "").unwrap();
            fs::write(dir.path().join("tab_created.json"), tab_created_json("w1")).unwrap();
            fs::write(dir.path().join("ws_created.json"), ws_created_json()).unwrap();

            let socket = dir.path().join("herdr.sock");
            let context = inside_context(
                bin.to_str().unwrap(),
                socket.to_str().unwrap(),
                "w1",
                "w1:t1",
                "w1:p1",
                BTreeMap::new(),
            )
            .unwrap();

            let world = Self {
                _lock: lock,
                dir,
                context,
                repo,
                src,
                docs,
                other,
            };
            world.write_default_session();
            world
        }

        fn write_default_session(&self) {
            let repo = self.repo_str();
            self.write_snapshot1(single_workspace_snapshot(
                &repo,
                &[shell_pane("w1:p1", &repo, &repo, true)],
            ));
            self.write_current1(current_json("w1:p1", "w1:t1", "w1", &repo));
        }

        fn write_snapshot1(&self, json: String) {
            fs::write(self.dir.path().join("snapshot1.json"), json).unwrap();
        }

        fn write_snapshot2(&self, json: String) {
            fs::write(self.dir.path().join("snapshot2.json"), json).unwrap();
        }

        fn write_current1(&self, json: String) {
            fs::write(self.dir.path().join("current1.json"), json).unwrap();
        }

        fn write_current2(&self, json: String) {
            fs::write(self.dir.path().join("current2.json"), json).unwrap();
        }

        fn mark_process_not_found(&self, pane: &str) {
            fs::write(self.dir.path().join("proc").join(format!("{pane}.nf")), "").unwrap();
        }

        fn write_process(&self, pane: &str, body: &str) {
            fs::write(
                self.dir.path().join("proc").join(format!("{pane}.json")),
                body,
            )
            .unwrap();
        }

        fn set_snapshot_sleep(&self, seconds: u64) {
            fs::write(
                self.dir.path().join("sleep_snapshot"),
                format!("{seconds}\n"),
            )
            .unwrap();
        }

        fn repo_str(&self) -> String {
            self.repo.to_str().unwrap().to_string()
        }

        fn src_str(&self) -> String {
            self.src.to_str().unwrap().to_string()
        }

        fn docs_str(&self) -> String {
            self.docs.to_str().unwrap().to_string()
        }

        fn other_str(&self) -> String {
            self.other.to_str().unwrap().to_string()
        }

        fn socket_path(&self) -> PathBuf {
            self.dir.path().join("herdr.sock")
        }

        fn paths_from(&self, cwd: &Path, target: &str) -> ResolvedPaths {
            resolve_paths(cwd, target, None).unwrap()
        }

        fn run_from(&self, cwd: &Path, target: &str) -> Result<Outcome, RunError> {
            self.run_from_with(cwd, target, &|| false, Instant::now() + TOTAL_DEADLINE)
        }

        fn run_from_with(
            &self,
            cwd: &Path,
            target: &str,
            interrupted: &dyn Fn() -> bool,
            deadline: Instant,
        ) -> Result<Outcome, RunError> {
            let paths = self.paths_from(cwd, target);
            orchestrate(
                &paths,
                &HerdrMode::Inside(self.context.clone()),
                interrupted,
                deadline,
            )
        }

        fn commands(&self) -> Vec<String> {
            fs::read_to_string(self.dir.path().join("record"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .filter(|line| !line.is_empty())
                .collect()
        }

        fn count_prefix(&self, prefix: &str) -> usize {
            self.commands()
                .iter()
                .filter(|line| line.starts_with(prefix))
                .count()
        }
    }

    fn sh_single(path: &str) -> String {
        format!("'{}'", path.replace('\'', r#"'"'"'"#))
    }

    fn shell_pane(id: &str, cwd: &str, foreground: &str, focused: bool) -> Value {
        json!({
            "pane_id": id,
            "terminal_id": format!("term-{id}"),
            "workspace_id": "w1",
            "tab_id": "w1:t1",
            "focused": focused,
            "agent_status": "idle",
            "revision": 1,
            "cwd": cwd,
            "foreground_cwd": foreground
        })
    }

    fn agent_pane(id: &str, foreground: &str, status: &str, focused: bool) -> Value {
        json!({
            "pane_id": id,
            "terminal_id": format!("term-{id}"),
            "workspace_id": "w1",
            "tab_id": "w1:t1",
            "focused": focused,
            "agent_status": status,
            "revision": 1,
            "agent": "codex",
            "foreground_cwd": foreground
        })
    }

    fn single_workspace_snapshot(_root: &str, panes: &[Value]) -> String {
        workspace_snapshot(
            "w1",
            &[workspace_record("w1", "w1:t1", None)],
            &[tab_record("w1:t1", "w1")],
            panes,
            &[layout_record(
                "w1",
                "w1:t1",
                panes[0]["pane_id"].as_str().unwrap(),
            )],
        )
    }

    fn workspace_record(id: &str, active_tab: &str, worktree: Option<&str>) -> Value {
        let mut value = json!({
            "workspace_id": id,
            "number": 1,
            "label": id,
            "focused": id == "w1",
            "pane_count": 1,
            "tab_count": 1,
            "active_tab_id": active_tab,
            "agent_status": "idle"
        });
        if let Some(path) = worktree {
            value["worktree"] = json!({
                "repo_key": "k",
                "repo_name": "n",
                "repo_root": path,
                "checkout_path": path,
                "is_linked_worktree": true
            });
        }
        value
    }

    fn tab_record(id: &str, workspace: &str) -> Value {
        json!({
            "tab_id": id,
            "workspace_id": workspace,
            "number": 1,
            "label": id,
            "focused": true,
            "pane_count": 1,
            "agent_status": "idle"
        })
    }

    fn layout_record(workspace: &str, tab: &str, focused_pane: &str) -> Value {
        json!({
            "workspace_id": workspace,
            "tab_id": tab,
            "zoomed": false,
            "area": {"x": 0, "y": 0, "width": 80, "height": 24},
            "focused_pane_id": focused_pane,
            "panes": [],
            "splits": []
        })
    }

    fn workspace_snapshot(
        focused: &str,
        workspaces: &[Value],
        tabs: &[Value],
        panes: &[Value],
        layouts: &[Value],
    ) -> String {
        json!({
            "id": "cli:session:snapshot",
            "result": {
                "type": "session_snapshot",
                "snapshot": {
                    "version": "0.8.2",
                    "protocol": 20,
                    "focused_workspace_id": focused,
                    "workspaces": workspaces,
                    "tabs": tabs,
                    "panes": panes,
                    "layouts": layouts,
                    "agents": []
                }
            }
        })
        .to_string()
    }

    fn current_json(pane_id: &str, tab_id: &str, workspace_id: &str, foreground: &str) -> String {
        json!({
            "id": "cli:pane:current",
            "result": {
                "type": "pane_current",
                "pane": {
                    "pane_id": pane_id,
                    "terminal_id": "term1",
                    "workspace_id": workspace_id,
                    "tab_id": tab_id,
                    "focused": true,
                    "agent_status": "idle",
                    "revision": 1,
                    "foreground_cwd": foreground
                }
            }
        })
        .to_string()
    }

    fn tab_created_json(workspace_id: &str) -> String {
        json!({
            "id": "cli:tab:create",
            "result": {
                "type": "tab_created",
                "tab": {
                    "tab_id": format!("{workspace_id}:t2"),
                    "workspace_id": workspace_id,
                    "number": 2,
                    "label": "src",
                    "focused": true,
                    "pane_count": 1,
                    "agent_status": "idle"
                },
                "root_pane": {
                    "pane_id": format!("{workspace_id}:p3"),
                    "terminal_id": "t",
                    "workspace_id": workspace_id,
                    "tab_id": format!("{workspace_id}:t2"),
                    "focused": true,
                    "agent_status": "idle",
                    "revision": 1
                }
            }
        })
        .to_string()
    }

    fn ws_created_json() -> String {
        json!({
            "id": "cli:workspace:create",
            "result": {
                "type": "workspace_created",
                "workspace": {
                    "workspace_id": "w2",
                    "number": 2,
                    "label": "other",
                    "focused": true,
                    "pane_count": 1,
                    "tab_count": 1,
                    "active_tab_id": "w2:t1",
                    "agent_status": "idle"
                },
                "tab": {
                    "tab_id": "w2:t1",
                    "workspace_id": "w2",
                    "number": 1,
                    "label": "main",
                    "focused": true,
                    "pane_count": 1,
                    "agent_status": "idle"
                },
                "root_pane": {
                    "pane_id": "w2:p1",
                    "terminal_id": "t",
                    "workspace_id": "w2",
                    "tab_id": "w2:t1",
                    "focused": true,
                    "agent_status": "idle",
                    "revision": 1
                }
            }
        })
        .to_string()
    }

    fn busy_process_json(pane: &str) -> String {
        json!({
            "id": "cli:pane:process-info",
            "result": {
                "type": "pane_process_info",
                "process_info": {
                    "pane_id": pane,
                    "shell_pid": 42,
                    "foreground_process_group_id": 99,
                    "foreground_processes": [
                        {"pid": 42, "name": "zsh"},
                        {"pid": 99, "name": "vim"}
                    ]
                }
            }
        })
        .to_string()
    }

    fn serve_focus(path: &Path, replies: Vec<FocusReply>) -> JoinHandle<()> {
        let listener = UnixListener::bind(path).unwrap();
        thread::spawn(move || {
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = Vec::new();
                let mut byte = [0u8; 1];
                loop {
                    stream.read_exact(&mut byte).unwrap();
                    if byte[0] == b'\n' {
                        break;
                    }
                    buf.push(byte[0]);
                }
                let request: Value = serde_json::from_slice(&buf).unwrap();
                let id = request["id"].as_str().unwrap();
                let pane_id = request["params"]["pane_id"].as_str().unwrap();
                let body = match reply {
                    FocusReply::Ok => json!({
                        "id": id,
                        "result": {"type": "pane_info", "pane": {"pane_id": pane_id}}
                    }),
                    FocusReply::NotFound => json!({
                        "id": id,
                        "error": {"code": "pane_not_found", "message": format!("pane {pane_id} not found")}
                    }),
                };
                stream.write_all(body.to_string().as_bytes()).unwrap();
                stream.write_all(b"\n").unwrap();
            }
        })
    }

    fn assert_silent(outcome: Outcome) {
        assert_eq!(outcome, Outcome::Silent);
    }

    fn assert_cd(outcome: Outcome, path: &Path) {
        match outcome {
            Outcome::ChangeDirectory { path: actual } => {
                assert_eq!(actual, CanonicalPath::directory(path).unwrap());
            }
            other => panic!("expected ChangeDirectory, got {other:?}"),
        }
    }

    fn failed(error: RunError) -> crate::domain::Error {
        match error {
            RunError::Failed(error) => error,
            RunError::Interrupted => panic!("unexpected interrupt"),
        }
    }

    #[test]
    fn total_deadline_is_ten_seconds() {
        assert_eq!(TOTAL_DEADLINE, Duration::from_secs(10));
    }

    #[test]
    fn outside_herdr_changes_directory_without_herdr_side_effects() {
        let world = World::new();
        let paths = world.paths_from(&world.repo, "src");
        let outcome = orchestrate(
            &paths,
            &HerdrMode::Outside,
            &|| false,
            Instant::now() + TOTAL_DEADLINE,
        )
        .unwrap();
        assert_cd(outcome, &world.src);
        assert!(world.commands().is_empty());
    }

    #[test]
    fn target_equals_cwd_is_noop_without_process_info() {
        let world = World::new();
        assert_silent(world.run_from(&world.repo, ".").unwrap());
        assert_eq!(world.count_prefix("api snapshot"), 1);
        assert_eq!(world.count_prefix("pane current"), 1);
        assert_eq!(world.count_prefix("pane process-info"), 0);
        assert_eq!(world.count_prefix("tab create"), 0);
        assert_eq!(world.count_prefix("workspace create"), 0);
    }

    #[test]
    fn strict_descendant_without_idle_pane_changes_directory() {
        let world = World::new();
        assert_cd(world.run_from(&world.repo, "src").unwrap(), &world.src);
        assert_eq!(world.count_prefix("tab create"), 0);
        assert_eq!(world.count_prefix("workspace create"), 0);
        assert!(
            !world
                .commands()
                .iter()
                .any(|line| line.contains("pane.focus"))
        );
    }

    #[test]
    fn idle_shell_in_caller_workspace_is_focused_and_does_not_set_pwd() {
        let world = World::new();
        let repo = world.repo_str();
        let src = world.src_str();
        world.write_snapshot1(single_workspace_snapshot(
            &repo,
            &[
                shell_pane("w1:p1", &repo, &repo, true),
                json!({
                    "pane_id": "w1:p2",
                    "terminal_id": "term-p2",
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "focused": false,
                    "agent_status": "idle",
                    "revision": 1,
                    "cwd": src,
                    "foreground_cwd": src
                }),
            ],
        ));
        let server = serve_focus(&world.socket_path(), vec![FocusReply::Ok]);
        assert_silent(world.run_from(&world.repo, "src").unwrap());
        server.join().unwrap();
        assert_eq!(world.count_prefix("pane process-info --pane w1:p2"), 1);
        assert_eq!(world.count_prefix("tab create"), 0);
        assert_eq!(world.count_prefix("api snapshot"), 1);
    }

    #[test]
    fn idle_agent_is_focused_without_process_info() {
        let world = World::new();
        let repo = world.repo_str();
        let src = world.src_str();
        world.write_snapshot1(single_workspace_snapshot(
            &repo,
            &[
                shell_pane("w1:p1", &repo, &repo, true),
                agent_pane("w1:p2", &src, "idle", false),
            ],
        ));
        let server = serve_focus(&world.socket_path(), vec![FocusReply::Ok]);
        assert_silent(world.run_from(&world.repo, "src").unwrap());
        server.join().unwrap();
        assert_eq!(world.count_prefix("pane process-info"), 0);
    }

    #[test]
    fn sibling_path_creates_a_tab_after_one_recompute() {
        let world = World::new();
        let outcome = world
            .run_from(&world.src, world.docs.to_str().unwrap())
            .unwrap();
        assert_silent(outcome);
        assert_eq!(world.count_prefix("api snapshot"), 2);
        assert_eq!(world.count_prefix("tab create"), 1);
        assert_eq!(world.count_prefix("workspace create"), 0);
        let create = world
            .commands()
            .into_iter()
            .find(|line| line.starts_with("tab create"))
            .unwrap();
        assert!(create.contains("--cwd"));
        assert!(create.contains("--focus"));
        assert!(!create.contains("--label"));
    }

    #[test]
    fn unrelated_path_creates_a_workspace_after_one_recompute() {
        let world = World::new();
        assert_silent(
            world
                .run_from(&world.repo, world.other.to_str().unwrap())
                .unwrap(),
        );
        assert_eq!(world.count_prefix("api snapshot"), 2);
        assert_eq!(world.count_prefix("workspace create"), 1);
        assert_eq!(world.count_prefix("tab create"), 0);
    }

    #[test]
    fn recompute_before_create_focuses_a_pane_that_appeared() {
        let world = World::new();
        let repo = world.repo_str();
        let docs = world.docs_str();
        world.write_snapshot1(single_workspace_snapshot(
            &repo,
            &[shell_pane("w1:p1", &repo, &world.src_str(), true)],
        ));
        world.write_snapshot2(single_workspace_snapshot(
            &repo,
            &[
                shell_pane("w1:p1", &repo, &world.src_str(), true),
                json!({
                    "pane_id": "w1:p2",
                    "terminal_id": "term-p2",
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "focused": false,
                    "agent_status": "idle",
                    "revision": 1,
                    "foreground_cwd": docs
                }),
            ],
        ));
        let server = serve_focus(&world.socket_path(), vec![FocusReply::Ok]);
        assert_silent(
            world
                .run_from(&world.src, world.docs.to_str().unwrap())
                .unwrap(),
        );
        server.join().unwrap();
        assert_eq!(world.count_prefix("api snapshot"), 2);
        assert_eq!(world.count_prefix("tab create"), 0);
        assert_eq!(world.count_prefix("pane process-info --pane w1:p2"), 1);
    }

    #[test]
    fn recompute_before_create_uses_a_better_workspace() {
        let world = World::new();
        let repo = world.repo_str();
        let other = world.other_str();
        world.write_snapshot1(single_workspace_snapshot(
            &repo,
            &[shell_pane("w1:p1", &repo, &repo, true)],
        ));
        world.write_snapshot2(workspace_snapshot(
            "w1",
            &[
                workspace_record("w1", "w1:t1", None),
                workspace_record("w2", "w2:t1", Some(&other)),
            ],
            &[tab_record("w1:t1", "w1"), tab_record("w2:t1", "w2")],
            &[
                json!({
                    "pane_id": "w1:p1",
                    "terminal_id": "term-p1",
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "focused": true,
                    "agent_status": "idle",
                    "revision": 1,
                    "cwd": repo,
                    "foreground_cwd": repo
                }),
                json!({
                    "pane_id": "w2:p1",
                    "terminal_id": "term-w2",
                    "workspace_id": "w2",
                    "tab_id": "w2:t1",
                    "focused": true,
                    "agent_status": "working",
                    "revision": 1,
                    "agent": "codex",
                    "cwd": other,
                    "foreground_cwd": other
                }),
            ],
            &[
                layout_record("w1", "w1:t1", "w1:p1"),
                layout_record("w2", "w2:t1", "w2:p1"),
            ],
        ));
        world.write_current2(current_json("w1:p1", "w1:t1", "w1", &repo));
        fs::write(
            world.dir.path().join("tab_created.json"),
            tab_created_json("w2"),
        )
        .unwrap();
        assert_silent(
            world
                .run_from(&world.repo, world.other.to_str().unwrap())
                .unwrap(),
        );
        assert_eq!(world.count_prefix("workspace create"), 0);
        assert_eq!(world.count_prefix("tab create"), 1);
        assert!(
            world
                .commands()
                .iter()
                .any(|line| line.starts_with("tab create --workspace w2"))
        );
    }

    #[test]
    fn stale_caller_recomputes_once_then_succeeds() {
        let world = World::new();
        world.write_current1(current_json("missing:p9", "w1:t1", "w1", &world.repo_str()));
        world.write_current2(current_json("w1:p1", "w1:t1", "w1", &world.repo_str()));
        assert_cd(world.run_from(&world.repo, "src").unwrap(), &world.src);
        assert_eq!(world.count_prefix("api snapshot"), 2);
    }

    #[test]
    fn repeated_stale_caller_is_an_error_without_directory_change() {
        let world = World::new();
        world.write_current1(current_json("missing:p9", "w1:t1", "w1", &world.repo_str()));
        world.write_current2(current_json("missing:p9", "w1:t1", "w1", &world.repo_str()));
        let error = failed(world.run_from(&world.repo, "src").unwrap_err());
        assert_eq!(error.kind(), ErrorKind::HerdrProtocol);
        assert_eq!(world.count_prefix("api snapshot"), 2);
        assert_eq!(world.count_prefix("tab create"), 0);
    }

    #[test]
    fn process_not_found_recomputes_once() {
        let world = World::new();
        let repo = world.repo_str();
        let src = world.src_str();
        world.write_snapshot1(single_workspace_snapshot(
            &repo,
            &[
                shell_pane("w1:p1", &repo, &repo, true),
                json!({
                    "pane_id": "w1:p2",
                    "terminal_id": "term-p2",
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "focused": false,
                    "agent_status": "idle",
                    "revision": 1,
                    "foreground_cwd": src
                }),
            ],
        ));
        world.mark_process_not_found("w1:p2");
        world.write_snapshot2(single_workspace_snapshot(
            &repo,
            &[shell_pane("w1:p1", &repo, &repo, true)],
        ));
        assert_cd(world.run_from(&world.repo, "src").unwrap(), &world.src);
        assert_eq!(world.count_prefix("api snapshot"), 2);
        assert_eq!(world.count_prefix("pane process-info --pane w1:p2"), 1);
    }

    #[test]
    fn focus_not_found_recomputes_once_then_creates() {
        let world = World::new();
        let repo = world.repo_str();
        let docs = world.docs_str();
        let snap = single_workspace_snapshot(
            &repo,
            &[
                shell_pane("w1:p1", &repo, &world.src_str(), true),
                json!({
                    "pane_id": "w1:p2",
                    "terminal_id": "term-p2",
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "focused": false,
                    "agent_status": "idle",
                    "revision": 1,
                    "foreground_cwd": docs
                }),
            ],
        );
        world.write_snapshot1(snap.clone());
        world.write_snapshot2(single_workspace_snapshot(
            &repo,
            &[shell_pane("w1:p1", &repo, &world.src_str(), true)],
        ));
        let server = serve_focus(&world.socket_path(), vec![FocusReply::NotFound]);
        assert_silent(
            world
                .run_from(&world.src, world.docs.to_str().unwrap())
                .unwrap(),
        );
        server.join().unwrap();
        assert_eq!(world.count_prefix("api snapshot"), 2);
        assert_eq!(world.count_prefix("tab create"), 1);
    }

    #[test]
    fn focus_not_found_after_recompute_is_an_error() {
        let world = World::new();
        let repo = world.repo_str();
        let src = world.src_str();
        let snap = single_workspace_snapshot(
            &repo,
            &[
                shell_pane("w1:p1", &repo, &repo, true),
                json!({
                    "pane_id": "w1:p2",
                    "terminal_id": "term-p2",
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "focused": false,
                    "agent_status": "idle",
                    "revision": 1,
                    "foreground_cwd": src
                }),
            ],
        );
        world.write_snapshot1(snap.clone());
        world.write_snapshot2(snap);
        let server = serve_focus(
            &world.socket_path(),
            vec![FocusReply::NotFound, FocusReply::NotFound],
        );
        let error = failed(world.run_from(&world.repo, "src").unwrap_err());
        assert_eq!(error.kind(), ErrorKind::HerdrAction);
        server.join().unwrap();
        assert_eq!(world.count_prefix("api snapshot"), 2);
        assert_eq!(world.count_prefix("tab create"), 0);
    }

    #[test]
    fn create_path_never_inspects_more_than_twice() {
        let world = World::new();
        world
            .run_from(&world.repo, world.other.to_str().unwrap())
            .unwrap();
        assert_eq!(world.count_prefix("api snapshot"), 2);
        assert_eq!(world.count_prefix("pane current"), 2);
    }

    #[test]
    fn busy_exact_path_shell_is_skipped_then_directory_change_proceeds() {
        let world = World::new();
        let repo = world.repo_str();
        let src = world.src_str();
        world.write_snapshot1(single_workspace_snapshot(
            &repo,
            &[
                shell_pane("w1:p1", &repo, &repo, true),
                json!({
                    "pane_id": "w1:p2",
                    "terminal_id": "term-p2",
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "focused": false,
                    "agent_status": "idle",
                    "revision": 1,
                    "foreground_cwd": src
                }),
            ],
        ));
        world.write_process("w1:p2", &busy_process_json("w1:p2"));
        assert_cd(world.run_from(&world.repo, "src").unwrap(), &world.src);
        assert_eq!(world.count_prefix("tab create"), 0);
    }

    #[test]
    fn missing_focus_socket_is_transport_failure_without_directory_change() {
        let world = World::new();
        let repo = world.repo_str();
        let src = world.src_str();
        world.write_snapshot1(single_workspace_snapshot(
            &repo,
            &[
                shell_pane("w1:p1", &repo, &repo, true),
                agent_pane("w1:p2", &src, "idle", false),
            ],
        ));
        let error = failed(world.run_from(&world.repo, "src").unwrap_err());
        assert_eq!(error.kind(), ErrorKind::HerdrTransport);
        assert_eq!(world.count_prefix("tab create"), 0);
        assert_eq!(world.count_prefix("workspace create"), 0);
    }

    #[test]
    fn herdr_protocol_failure_does_not_fall_back_to_directory_change() {
        let world = World::new();
        world.write_snapshot1("{not json".into());
        let error = failed(world.run_from(&world.repo, "src").unwrap_err());
        assert_eq!(error.kind(), ErrorKind::HerdrProtocol);
        assert_eq!(world.count_prefix("tab create"), 0);
        assert_eq!(world.count_prefix("workspace create"), 0);
    }

    #[test]
    fn incompatible_herdr_is_surfaced_without_fallback() {
        let world = World::new();
        let mut snapshot: Value = serde_json::from_str(&single_workspace_snapshot(
            &world.repo_str(),
            &[shell_pane(
                "w1:p1",
                &world.repo_str(),
                &world.repo_str(),
                true,
            )],
        ))
        .unwrap();
        snapshot["result"]["snapshot"]["version"] = json!("0.7.0");
        snapshot["result"]["snapshot"]["protocol"] = json!(19);
        world.write_snapshot1(snapshot.to_string());
        let error = failed(world.run_from(&world.repo, "src").unwrap_err());
        assert_eq!(error.kind(), ErrorKind::IncompatibleHerdr);
    }

    #[test]
    fn total_deadline_maps_halted_work_to_timeout() {
        let world = World::new();
        world.set_snapshot_sleep(1);
        let error = failed(
            world
                .run_from_with(
                    &world.repo,
                    "src",
                    &|| false,
                    Instant::now() + Duration::from_millis(200),
                )
                .unwrap_err(),
        );
        assert_eq!(error.kind(), ErrorKind::HerdrTimeout);
        assert!(
            error.message().contains("10-second deadline")
                || error.message().contains("timed out")
                || error.message().contains("deadline")
        );
        assert_eq!(world.count_prefix("tab create"), 0);
    }

    #[test]
    fn interruption_skips_remaining_herdr_work() {
        let world = World::new();
        world.set_snapshot_sleep(1);
        let stop = AtomicBool::new(false);
        let started = Instant::now();
        let error = world
            .run_from_with(
                &world.repo,
                "src",
                &|| {
                    if started.elapsed() > Duration::from_millis(50) {
                        stop.store(true, Ordering::Relaxed);
                    }
                    stop.load(Ordering::Relaxed)
                },
                Instant::now() + TOTAL_DEADLINE,
            )
            .unwrap_err();
        assert!(matches!(error, RunError::Interrupted));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(world.count_prefix("tab create"), 0);
        assert_eq!(world.count_prefix("workspace create"), 0);
    }

    #[test]
    fn interruption_before_herdr_does_not_start_work() {
        let world = World::new();
        let error = world
            .run_from_with(
                &world.repo,
                "src",
                &|| true,
                Instant::now() + TOTAL_DEADLINE,
            )
            .unwrap_err();
        assert!(matches!(error, RunError::Interrupted));
        assert!(world.commands().is_empty());
    }

    #[test]
    fn two_concurrent_creates_may_both_proceed() {
        let world = World::new();
        let first_paths = world.paths_from(&world.repo, world.other.to_str().unwrap());
        let second_paths = first_paths.clone();
        let first_ctx = world.context.clone();
        let second_ctx = world.context.clone();
        let first = thread::spawn(move || {
            orchestrate(
                &first_paths,
                &HerdrMode::Inside(first_ctx),
                &|| false,
                Instant::now() + TOTAL_DEADLINE,
            )
        });
        let second = thread::spawn(move || {
            orchestrate(
                &second_paths,
                &HerdrMode::Inside(second_ctx),
                &|| false,
                Instant::now() + TOTAL_DEADLINE,
            )
        });
        assert_silent(first.join().unwrap().unwrap());
        assert_silent(second.join().unwrap().unwrap());
        assert_eq!(world.count_prefix("workspace create"), 2);
    }

    #[test]
    fn parent_navigation_does_not_change_directory() {
        let world = World::new();
        let repo = world.repo_str();
        let src = world.src_str();
        world.write_snapshot1(single_workspace_snapshot(
            &repo,
            &[shell_pane("w1:p1", &repo, &src, true)],
        ));
        let outcome = world.run_from(&world.src, "..").unwrap();
        assert!(!matches!(outcome, Outcome::ChangeDirectory { .. }));
        assert_silent(outcome);
        assert_eq!(world.count_prefix("tab create"), 1);
    }

    #[test]
    fn sanitized_herdr_errors_do_not_dump_environment_assignments() {
        let world = World::new();
        world.write_snapshot1("{not json HERDR_SESSION=secret-session".into());
        let error = failed(world.run_from(&world.repo, "src").unwrap_err());
        assert!(!error.message().contains("HERDR_SESSION=secret-session"));
        assert!(!error.message().contains("{not json"));
    }
}
