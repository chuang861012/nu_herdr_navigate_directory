//! Read-only Herdr inspection mapped onto phase-2 domain types.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::cli::{self, POLL_INTERVAL, READ_TIMEOUT, RunError};
use super::context::InsideContext;
use super::protocol::{
    self, CommandResult, RawAgentStatus, RawPane, RawProcessInfo, RawSnapshot, RawTab, RawWorkspace,
};
use crate::domain::{
    AgentStatus, Caller, CanonicalPath, Error, ForegroundProcess, Occupant, Pane, PaneId, Session,
    ShellProcessEvidence, Tab, TabId, Workspace, WorkspaceId, nearest_containing_workspace,
};

/// Live caller identity from `herdr pane current --current`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveCaller {
    pub workspace_id: WorkspaceId,
    pub tab_id: TabId,
    pub pane_id: PaneId,
}

/// Snapshot plus live caller, or a stale identity that later phases may recompute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionInspection {
    Ready { live: LiveCaller, session: Session },
    Stale,
}

/// `pane process-info` outcome. Incomplete evidence is ineligible, not an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProcessInspection {
    Evidence(ShellProcessEvidence),
    NotFound,
}

/// Read one snapshot and the live caller. This never focuses or creates resources.
pub(crate) fn inspect_session(
    context: &InsideContext,
    interrupted: impl Fn() -> bool,
) -> Result<SessionInspection, RunError> {
    inspect_session_sequential(context, READ_TIMEOUT, interrupted)
}

/// Concurrent snapshot and live-caller reads that share one wall-clock deadline.
///
/// A stale caller/snapshot mismatch is returned immediately. Completion must not
/// perform the execution path's bounded recomputation.
pub(crate) fn inspect_session_concurrent(
    context: &InsideContext,
    timeout: Duration,
    interrupted: impl Fn() -> bool,
) -> Result<SessionInspection, RunError> {
    inspect_session_parallel(context, timeout, interrupted)
}

fn inspect_session_sequential(
    context: &InsideContext,
    timeout: Duration,
    interrupted: impl Fn() -> bool,
) -> Result<SessionInspection, RunError> {
    let snapshot = match snapshot(context, timeout, &interrupted)? {
        CommandResult::Ok(snapshot) => snapshot,
        CommandResult::NotFound { .. } => return Ok(SessionInspection::Stale),
    };
    protocol::require_supported_version(&snapshot)?;
    let session = super::cli::run_bounded(&interrupted, move || map_session(&snapshot))??;

    let live_pane = match current_pane(context, timeout, &interrupted)? {
        CommandResult::Ok(pane) => pane,
        CommandResult::NotFound { .. } => return Ok(SessionInspection::Stale),
    };
    finish_inspection(session, live_pane)
}

fn inspect_session_parallel(
    context: &InsideContext,
    timeout: Duration,
    interrupted: impl Fn() -> bool,
) -> Result<SessionInspection, RunError> {
    let halt = Arc::new(AtomicBool::new(false));
    let (snapshot_tx, snapshot_rx) = mpsc::channel();
    let (current_tx, current_rx) = mpsc::channel();
    let snapshot_context = context.clone();
    let current_context = context.clone();
    let snapshot_halt = halt.clone();
    let current_halt = halt.clone();

    thread::spawn(move || {
        let _ = snapshot_tx.send(snapshot(&snapshot_context, timeout, &|| {
            snapshot_halt.load(Ordering::Relaxed)
        }));
    });
    thread::spawn(move || {
        let _ = current_tx.send(current_pane(&current_context, timeout, &|| {
            current_halt.load(Ordering::Relaxed)
        }));
    });

    let deadline = Instant::now() + timeout;
    let snapshot_result = recv_cli_result(&snapshot_rx, &halt, &interrupted, deadline)?;
    let current_result = recv_cli_result(&current_rx, &halt, &interrupted, deadline)?;

    let snapshot = match snapshot_result? {
        CommandResult::Ok(snapshot) => snapshot,
        CommandResult::NotFound { .. } => return Ok(SessionInspection::Stale),
    };
    protocol::require_supported_version(&snapshot)?;
    let session = super::cli::run_bounded(&interrupted, move || map_session(&snapshot))??;
    let live_pane = match current_result? {
        CommandResult::Ok(pane) => pane,
        CommandResult::NotFound { .. } => return Ok(SessionInspection::Stale),
    };
    finish_inspection(session, live_pane)
}

fn recv_cli_result<T>(
    rx: &mpsc::Receiver<Result<T, RunError>>,
    halt: &AtomicBool,
    interrupted: impl Fn() -> bool,
    deadline: Instant,
) -> Result<Result<T, RunError>, RunError> {
    loop {
        if interrupted() {
            halt.store(true, Ordering::Relaxed);
            return Err(RunError::Interrupted);
        }
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(POLL_INTERVAL);
        if wait.is_zero() {
            halt.store(true, Ordering::Relaxed);
            return Err(Error::herdr_timeout("Herdr command timed out").into());
        }
        match rx.recv_timeout(wait) {
            Ok(result) => return Ok(result),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                halt.store(true, Ordering::Relaxed);
                return Err(
                    Error::herdr_transport("Herdr inspection ended without a result").into(),
                );
            }
        }
    }
}

fn finish_inspection(session: Session, live_pane: RawPane) -> Result<SessionInspection, RunError> {
    let live = live_caller(&live_pane)?;
    if !session_contains(&session, &live) {
        return Ok(SessionInspection::Stale);
    }
    Ok(SessionInspection::Ready { live, session })
}

/// Inspect one shell pane's foreground process evidence.
pub(crate) fn inspect_process(
    context: &InsideContext,
    pane_id: &PaneId,
    interrupted: impl Fn() -> bool,
) -> Result<ProcessInspection, RunError> {
    let output = cli::run(
        context,
        &["pane", "process-info", "--pane", pane_id.as_str()],
        READ_TIMEOUT,
        interrupted,
    )?;
    match protocol::parse_process_info(&output)? {
        CommandResult::Ok(info) => {
            if info.pane_id != pane_id.as_str() {
                return Err(Error::herdr_protocol(
                    "pane process inspection returned a mismatched pane id",
                )
                .into());
            }
            Ok(ProcessInspection::Evidence(map_process_evidence(&info)))
        }
        CommandResult::NotFound { .. } => Ok(ProcessInspection::NotFound),
    }
}

/// Exact-path shell panes the decision tree is allowed to consider.
///
/// The caller's workspace is always searched. Other workspaces are included
/// only when cwd descent would not already win, and then only the unique
/// nearest containing workspace.
pub(crate) fn exact_path_shell_candidates<'a>(
    session: &'a Session,
    caller: &Caller,
    target: &CanonicalPath,
) -> Vec<&'a Pane> {
    let caller_workspace = session
        .workspaces
        .iter()
        .find(|workspace| workspace.id == caller.workspace_id);
    let nearest = (!caller.cwd.is_strict_ancestor_of(target))
        .then(|| nearest_containing_workspace(session, &caller.workspace_id, target))
        .flatten()
        .filter(|workspace| workspace.id != caller.workspace_id);

    caller_workspace
        .into_iter()
        .chain(nearest)
        .flat_map(|workspace| workspace.tabs.iter())
        .flat_map(|tab| tab.panes.iter())
        .filter(|pane| {
            pane.foreground_cwd.as_ref() == Some(target)
                && matches!(pane.occupant, Occupant::Shell(_))
        })
        .collect()
}

pub(crate) fn apply_shell_evidence(
    session: &mut Session,
    pane_id: &PaneId,
    evidence: ShellProcessEvidence,
) {
    for workspace in &mut session.workspaces {
        for tab in &mut workspace.tabs {
            for pane in &mut tab.panes {
                if &pane.id == pane_id {
                    if matches!(pane.occupant, Occupant::Shell(_)) {
                        pane.occupant = Occupant::Shell(Some(evidence));
                    }
                    return;
                }
            }
        }
    }
}

fn snapshot(
    context: &InsideContext,
    timeout: Duration,
    interrupted: &impl Fn() -> bool,
) -> Result<CommandResult<RawSnapshot>, RunError> {
    let output = cli::run(context, &["api", "snapshot"], timeout, interrupted)?;
    protocol::parse_snapshot(&output)
}

fn current_pane(
    context: &InsideContext,
    timeout: Duration,
    interrupted: &impl Fn() -> bool,
) -> Result<CommandResult<RawPane>, RunError> {
    let output = cli::run(
        context,
        &["pane", "current", "--current"],
        timeout,
        interrupted,
    )?;
    protocol::parse_pane_current(&output)
}

fn live_caller(pane: &RawPane) -> Result<LiveCaller, Error> {
    if pane.pane_id.is_empty() || pane.tab_id.is_empty() || pane.workspace_id.is_empty() {
        return Err(Error::herdr_protocol(
            "live caller lookup is missing required identity fields",
        ));
    }
    Ok(LiveCaller {
        workspace_id: WorkspaceId::new(pane.workspace_id.clone()),
        tab_id: TabId::new(pane.tab_id.clone()),
        pane_id: PaneId::new(pane.pane_id.clone()),
    })
}

fn session_contains(session: &Session, live: &LiveCaller) -> bool {
    session.workspaces.iter().any(|workspace| {
        workspace.id == live.workspace_id
            && workspace.tabs.iter().any(|tab| {
                tab.id == live.tab_id && tab.panes.iter().any(|pane| pane.id == live.pane_id)
            })
    })
}

fn map_session(snapshot: &RawSnapshot) -> Result<Session, Error> {
    let mut seen_workspaces = Vec::new();
    for workspace in &snapshot.workspaces {
        require_id("workspace", &workspace.workspace_id)?;
        if seen_workspaces.contains(&workspace.workspace_id) {
            return Err(Error::herdr_protocol(
                "session snapshot contains a duplicate workspace id",
            ));
        }
        seen_workspaces.push(workspace.workspace_id.clone());
    }
    let mut seen_tabs = Vec::new();
    for tab in &snapshot.tabs {
        require_id("tab", &tab.tab_id)?;
        if seen_tabs.contains(&tab.tab_id) {
            return Err(Error::herdr_protocol(
                "session snapshot contains a duplicate tab id",
            ));
        }
        if !seen_workspaces.iter().any(|id| id == &tab.workspace_id) {
            return Err(Error::herdr_protocol(
                "session snapshot tab references an unknown workspace",
            ));
        }
        seen_tabs.push(tab.tab_id.clone());
    }
    let mut seen_panes = Vec::new();
    for pane in &snapshot.panes {
        require_id("pane", &pane.pane_id)?;
        if seen_panes.contains(&pane.pane_id) {
            return Err(Error::herdr_protocol(
                "session snapshot contains a duplicate pane id",
            ));
        }
        let Some(tab) = snapshot.tabs.iter().find(|tab| tab.tab_id == pane.tab_id) else {
            return Err(Error::herdr_protocol(
                "session snapshot pane references an unknown tab",
            ));
        };
        if pane.workspace_id != tab.workspace_id {
            return Err(Error::herdr_protocol(
                "session snapshot pane workspace does not match its tab",
            ));
        }
        seen_panes.push(pane.pane_id.clone());
    }
    if let Some(id) = snapshot
        .focused_workspace_id
        .as_deref()
        .filter(|id| !id.is_empty())
        && !seen_workspaces.iter().any(|known| known == id)
    {
        return Err(Error::herdr_protocol(
            "session snapshot focused workspace is unknown",
        ));
    }
    for layout in &snapshot.layouts {
        require_id("layout tab", &layout.tab_id)?;
        let Some(tab) = snapshot.tabs.iter().find(|tab| tab.tab_id == layout.tab_id) else {
            return Err(Error::herdr_protocol(
                "session snapshot layout references an unknown tab",
            ));
        };
        if layout.workspace_id != tab.workspace_id {
            return Err(Error::herdr_protocol(
                "session snapshot layout workspace does not match its tab",
            ));
        }
        require_id("layout focused pane", &layout.focused_pane_id)?;
        if !snapshot
            .panes
            .iter()
            .any(|pane| pane.pane_id == layout.focused_pane_id && pane.tab_id == layout.tab_id)
        {
            return Err(Error::herdr_protocol(
                "session snapshot layout focused pane is unknown or not in this tab",
            ));
        }
    }

    let workspaces = snapshot
        .workspaces
        .iter()
        .map(|workspace| map_workspace(workspace, snapshot))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Session {
        focused_workspace_id: snapshot
            .focused_workspace_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(WorkspaceId::new),
        workspaces,
    })
}

fn map_workspace(workspace: &RawWorkspace, snapshot: &RawSnapshot) -> Result<Workspace, Error> {
    let tabs = snapshot
        .tabs
        .iter()
        .filter(|tab| tab.workspace_id == workspace.workspace_id)
        .map(|tab| map_tab(tab, snapshot))
        .collect::<Result<Vec<_>, _>>()?;
    require_id("workspace active tab", &workspace.active_tab_id)?;
    if !tabs
        .iter()
        .any(|tab| tab.id.as_str() == workspace.active_tab_id)
    {
        return Err(Error::herdr_protocol(
            "session snapshot workspace active tab is unknown or not in this workspace",
        ));
    }

    Ok(Workspace {
        id: WorkspaceId::new(workspace.workspace_id.clone()),
        root: workspace_root(workspace, &tabs, snapshot),
        focused_tab_id: Some(TabId::new(workspace.active_tab_id.clone())),
        tabs,
        label: workspace.label.clone(),
        number: workspace.number,
    })
}

fn workspace_root(
    workspace: &RawWorkspace,
    tabs: &[Tab],
    snapshot: &RawSnapshot,
) -> Option<CanonicalPath> {
    if let Some(worktree) = &workspace.worktree {
        return CanonicalPath::try_directory(&worktree.checkout_path);
    }
    let first_tab = tabs.first()?;
    let first_pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.tab_id == first_tab.id.as_str())?;
    first_pane
        .cwd
        .as_deref()
        .and_then(CanonicalPath::try_directory)
}

fn map_tab(tab: &RawTab, snapshot: &RawSnapshot) -> Result<Tab, Error> {
    let panes = snapshot
        .panes
        .iter()
        .filter(|pane| pane.tab_id == tab.tab_id)
        .map(map_pane)
        .collect::<Result<Vec<_>, _>>()?;
    let focused_pane_id = snapshot
        .layouts
        .iter()
        .find(|layout| layout.tab_id == tab.tab_id)
        .map(|layout| PaneId::new(layout.focused_pane_id.clone()))
        .or_else(|| {
            snapshot
                .panes
                .iter()
                .find(|pane| pane.tab_id == tab.tab_id && pane.focused)
                .map(|pane| PaneId::new(pane.pane_id.clone()))
        });

    Ok(Tab {
        id: TabId::new(tab.tab_id.clone()),
        focused_pane_id,
        panes,
    })
}

fn map_pane(pane: &RawPane) -> Result<Pane, Error> {
    let occupant = match pane.agent.as_deref().filter(|agent| !agent.is_empty()) {
        Some(_) => Occupant::Agent(map_agent_status(pane.agent_status)),
        None => Occupant::Shell(None),
    };
    Ok(Pane {
        id: PaneId::new(pane.pane_id.clone()),
        foreground_cwd: pane
            .foreground_cwd
            .as_deref()
            .and_then(CanonicalPath::try_directory),
        occupant,
    })
}

fn map_agent_status(status: RawAgentStatus) -> AgentStatus {
    match status {
        RawAgentStatus::Idle => AgentStatus::Idle,
        RawAgentStatus::Working => AgentStatus::Working,
        RawAgentStatus::Blocked => AgentStatus::Blocked,
        RawAgentStatus::Done => AgentStatus::Done,
        RawAgentStatus::Unknown => AgentStatus::Unknown,
    }
}

fn map_process_evidence(info: &RawProcessInfo) -> ShellProcessEvidence {
    ShellProcessEvidence {
        shell_pid: info.shell_pid,
        foreground_process_group_id: info.foreground_process_group_id,
        foreground_processes: info
            .foreground_processes
            .iter()
            .map(|process| ForegroundProcess { pid: process.pid })
            .collect(),
    }
}

fn require_id(kind: &str, id: &str) -> Result<(), Error> {
    if id.is_empty() {
        Err(Error::herdr_protocol(format!(
            "session snapshot {kind} is missing an id"
        )))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LiveCaller, ProcessInspection, SessionInspection, apply_shell_evidence,
        exact_path_shell_candidates, inspect_process, inspect_session, inspect_session_concurrent,
        map_session,
    };
    use crate::domain::CanonicalPath;
    use crate::domain::{AgentStatus, Caller, Occupant, PaneId, TabId, WorkspaceId};
    use crate::herdr::context::inside_context;
    use crate::herdr::protocol::{CommandResult, RawSnapshot, parse_snapshot};
    use crate::herdr::test_support::{TempDir, lock_cli, write_executable};
    use std::collections::BTreeMap;
    use std::fs;

    fn snapshot_json(root: &str, extra_pane: &str, extra_workspace: &str) -> String {
        snapshot_json_extended(root, extra_pane, extra_workspace, "", "", "")
    }

    fn snapshot_json_extended(
        root: &str,
        extra_pane: &str,
        extra_workspace: &str,
        extra_workspaces: &str,
        extra_tabs: &str,
        extra_panes: &str,
    ) -> String {
        format!(
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
                    "agent_status": "idle"{extra_workspace}
                  }}{extra_workspaces}],
                  "tabs": [{{
                    "tab_id": "w1:t1",
                    "workspace_id": "w1",
                    "number": 1,
                    "label": "main",
                    "focused": true,
                    "pane_count": 2,
                    "agent_status": "idle"
                  }}{extra_tabs}],
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
                    "agent_status": "working",
                    "revision": 2,
                    "agent": "codex",
                    "foreground_cwd": "{root}/src"{extra_pane}
                  }}{extra_panes}],
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
        )
    }

    fn map_session_from_json(json: &str) -> Result<crate::domain::Session, crate::domain::Error> {
        map_session(&raw_snapshot(json))
    }

    fn raw_snapshot(json: &str) -> RawSnapshot {
        let output = crate::herdr::cli::CliOutput {
            stdout: json.as_bytes().to_vec(),
            stderr: Vec::new(),
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
        };
        let CommandResult::Ok(raw) = parse_snapshot(&output).unwrap() else {
            panic!("snapshot");
        };
        raw
    }

    fn assert_protocol_error(snapshot: &RawSnapshot, detail: &str) {
        let err = map_session(snapshot).unwrap_err();
        assert_eq!(err.kind(), crate::domain::ErrorKind::HerdrProtocol);
        assert!(
            err.message().contains(detail),
            "expected {detail:?} in {}",
            err.message()
        );
    }

    fn current_json(root: &str, pane_id: &str) -> String {
        format!(
            r#"{{
              "id": "cli:pane:current",
              "result": {{
                "type": "pane_current",
                "pane": {{
                  "pane_id": "{pane_id}",
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
        )
    }

    fn process_json(complete: bool) -> String {
        if complete {
            r#"{
              "id": "cli:pane:process-info",
              "result": {
                "type": "pane_process_info",
                "process_info": {
                  "pane_id": "w1:p1",
                  "shell_pid": 42,
                  "foreground_process_group_id": 42,
                  "foreground_processes": [{"pid": 42, "name": "zsh"}]
                }
              }
            }"#
            .into()
        } else {
            r#"{
              "id": "cli:pane:process-info",
              "result": {
                "type": "pane_process_info",
                "process_info": {
                  "pane_id": "w1:p1",
                  "foreground_processes": []
                }
              }
            }"#
            .into()
        }
    }

    fn install_fake(
        snapshot: &str,
        current: &str,
        process: &str,
    ) -> (TempDir, crate::herdr::context::InsideContext) {
        let dir = TempDir::new("inspect");
        let snapshot_path = dir.path().join("snapshot.json");
        let current_path = dir.path().join("current.json");
        let process_path = dir.path().join("process.json");
        fs::write(&snapshot_path, snapshot).unwrap();
        fs::write(&current_path, current).unwrap();
        fs::write(&process_path, process).unwrap();
        let record = dir.path().join("record");
        let bin = write_executable(
            dir.path(),
            "herdr",
            &format!(
                r#"#!/bin/sh
set -eu
{{
  printf 'argv0=%s\n' "$0"
  printf 'args=%s\n' "$*"
  printf 'HERDR_SOCKET_PATH=%s\n' "${{HERDR_SOCKET_PATH-}}"
  if [ -n "${{HERDR_SESSION+x}}" ]; then printf 'HERDR_SESSION=%s\n' "$HERDR_SESSION"; else printf 'HERDR_SESSION=<unset>\n'; fi
}} >> {record}
case "$1 $2" in
  "api snapshot") cat {snapshot} ;;
  "pane current") cat {current} ;;
  "pane process-info") cat {process} ;;
  *) echo "unexpected $*" >&2; exit 2 ;;
esac
"#,
                record = sh_single(&record.display().to_string()),
                snapshot = sh_single(&snapshot_path.display().to_string()),
                current = sh_single(&current_path.display().to_string()),
                process = sh_single(&process_path.display().to_string()),
            ),
        );
        let context = inside_context(
            bin.to_str().unwrap(),
            "/tmp/nu-plugin-herdr-navigate-directory.sock",
            "w1",
            "w1:t1",
            "w1:p1",
            BTreeMap::new(),
        )
        .unwrap();
        (dir, context)
    }

    fn sh_single(path: &str) -> String {
        format!("'{}'", path.replace('\'', r#"'"'"'"#))
    }

    fn not_found_json() -> &'static str {
        r#"{"id":"x","error":{"code":"not_found","message":"pane not found"}}"#
    }

    #[test]
    fn maps_snapshot_into_domain_types_without_transport_json() {
        let root = TempDir::new("root");
        let src = root.path().join("src");
        fs::create_dir(&src).unwrap();
        let json = snapshot_json(root.path().to_str().unwrap(), "", "");
        let output = crate::herdr::cli::CliOutput {
            stdout: json.into_bytes(),
            stderr: Vec::new(),
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
        };
        let CommandResult::Ok(raw) = parse_snapshot(&output).unwrap() else {
            panic!("snapshot");
        };
        let session = map_session(&raw).unwrap();
        assert_eq!(session.focused_workspace_id, Some(WorkspaceId::new("w1")));
        assert_eq!(
            session.workspaces[0].root.as_ref(),
            Some(&CanonicalPath::directory(root.path()).unwrap())
        );
        assert!(matches!(
            session.workspaces[0].tabs[0].panes[0].occupant,
            Occupant::Shell(None)
        ));
        assert!(matches!(
            session.workspaces[0].tabs[0].panes[1].occupant,
            Occupant::Agent(AgentStatus::Working)
        ));
        let target = CanonicalPath::directory(root.path()).unwrap();
        let caller = Caller {
            cwd: target.clone(),
            workspace_id: WorkspaceId::new("w1"),
            tab_id: TabId::new("w1:t1"),
            pane_id: PaneId::new("w1:p1"),
        };
        let shells = exact_path_shell_candidates(&session, &caller, &target);
        assert_eq!(shells.len(), 1);
        assert_eq!(shells[0].id, PaneId::new("w1:p1"));
    }

    #[test]
    fn exact_path_shells_in_non_containing_workspaces_are_not_candidates() {
        let caller_root = TempDir::new("caller-root");
        let other_root = TempDir::new("other-root");
        let target_dir = TempDir::new("unrelated-target");
        let extra_workspace = format!(
            r#", {{
              "workspace_id": "w2",
              "number": 2,
              "label": "other",
              "focused": false,
              "pane_count": 1,
              "tab_count": 1,
              "active_tab_id": "w2:t1",
              "agent_status": "idle",
              "worktree": {{
                "repo_key": "k",
                "repo_name": "n",
                "repo_root": "{path}",
                "checkout_path": "{path}",
                "is_linked_worktree": true
              }}
            }}"#,
            path = other_root.path().to_str().unwrap()
        );
        let extra_tab = r#", {
          "tab_id": "w2:t1",
          "workspace_id": "w2",
          "number": 1,
          "label": "other",
          "focused": false,
          "pane_count": 1,
          "agent_status": "idle"
        }"#;
        let extra_pane = format!(
            r#", {{
              "pane_id": "w2:p9",
              "terminal_id": "term9",
              "workspace_id": "w2",
              "tab_id": "w2:t1",
              "focused": true,
              "agent_status": "idle",
              "revision": 1,
              "cwd": "{other}",
              "foreground_cwd": "{target}"
            }}"#,
            other = other_root.path().to_str().unwrap(),
            target = target_dir.path().to_str().unwrap()
        );
        let json = snapshot_json_extended(
            caller_root.path().to_str().unwrap(),
            "",
            "",
            &extra_workspace,
            extra_tab,
            &extra_pane,
        );
        let session = map_session_from_json(&json).unwrap();
        let target = CanonicalPath::directory(target_dir.path()).unwrap();
        let caller = Caller {
            cwd: CanonicalPath::directory(caller_root.path()).unwrap(),
            workspace_id: WorkspaceId::new("w1"),
            tab_id: TabId::new("w1:t1"),
            pane_id: PaneId::new("w1:p1"),
        };
        assert!(exact_path_shell_candidates(&session, &caller, &target).is_empty());
    }

    #[test]
    fn descendant_targets_do_not_inspect_other_containing_workspaces() {
        let root = TempDir::new("desc-root");
        let src = root.path().join("src");
        fs::create_dir(&src).unwrap();
        let extra_workspace = format!(
            r#", {{
              "workspace_id": "w2",
              "number": 2,
              "label": "nested",
              "focused": false,
              "pane_count": 1,
              "tab_count": 1,
              "active_tab_id": "w2:t1",
              "agent_status": "idle",
              "worktree": {{
                "repo_key": "k",
                "repo_name": "n",
                "repo_root": "{path}",
                "checkout_path": "{path}",
                "is_linked_worktree": true
              }}
            }}"#,
            path = src.to_str().unwrap()
        );
        let extra_tab = r#", {
          "tab_id": "w2:t1",
          "workspace_id": "w2",
          "number": 1,
          "label": "src",
          "focused": false,
          "pane_count": 1,
          "agent_status": "idle"
        }"#;
        let extra_pane = format!(
            r#", {{
              "pane_id": "w2:p9",
              "terminal_id": "term9",
              "workspace_id": "w2",
              "tab_id": "w2:t1",
              "focused": true,
              "agent_status": "idle",
              "revision": 1,
              "cwd": "{src}",
              "foreground_cwd": "{src}"
            }}"#,
            src = src.to_str().unwrap()
        );
        let json = snapshot_json_extended(
            root.path().to_str().unwrap(),
            "",
            "",
            &extra_workspace,
            extra_tab,
            &extra_pane,
        );
        let session = map_session_from_json(&json).unwrap();
        let caller = Caller {
            cwd: CanonicalPath::directory(root.path()).unwrap(),
            workspace_id: WorkspaceId::new("w1"),
            tab_id: TabId::new("w1:t1"),
            pane_id: PaneId::new("w1:p1"),
        };
        let target = CanonicalPath::directory(&src).unwrap();
        assert!(exact_path_shell_candidates(&session, &caller, &target).is_empty());
    }

    #[test]
    fn only_nearest_containing_workspace_shells_are_candidates() {
        let caller_root = TempDir::new("caller-root");
        let repo = TempDir::new("repo");
        let src = repo.path().join("src");
        fs::create_dir(&src).unwrap();
        let extra_workspaces = format!(
            r#", {{
              "workspace_id": "w2",
              "number": 2,
              "label": "repo",
              "focused": false,
              "pane_count": 1,
              "tab_count": 1,
              "active_tab_id": "w2:t1",
              "agent_status": "idle",
              "worktree": {{
                "repo_key": "k",
                "repo_name": "n",
                "repo_root": "{repo}",
                "checkout_path": "{repo}",
                "is_linked_worktree": true
              }}
            }}, {{
              "workspace_id": "w3",
              "number": 3,
              "label": "src",
              "focused": false,
              "pane_count": 1,
              "tab_count": 1,
              "active_tab_id": "w3:t1",
              "agent_status": "idle",
              "worktree": {{
                "repo_key": "k",
                "repo_name": "n",
                "repo_root": "{src}",
                "checkout_path": "{src}",
                "is_linked_worktree": true
              }}
            }}"#,
            repo = repo.path().to_str().unwrap(),
            src = src.to_str().unwrap()
        );
        let extra_tabs = r#", {
          "tab_id": "w2:t1",
          "workspace_id": "w2",
          "number": 1,
          "label": "repo",
          "focused": false,
          "pane_count": 1,
          "agent_status": "idle"
        }, {
          "tab_id": "w3:t1",
          "workspace_id": "w3",
          "number": 1,
          "label": "src",
          "focused": false,
          "pane_count": 1,
          "agent_status": "idle"
        }"#;
        let extra_panes = format!(
            r#", {{
              "pane_id": "w2:p9",
              "terminal_id": "term-w2",
              "workspace_id": "w2",
              "tab_id": "w2:t1",
              "focused": true,
              "agent_status": "idle",
              "revision": 1,
              "cwd": "{repo}",
              "foreground_cwd": "{src}"
            }}, {{
              "pane_id": "w3:p1",
              "terminal_id": "term-w3",
              "workspace_id": "w3",
              "tab_id": "w3:t1",
              "focused": true,
              "agent_status": "idle",
              "revision": 1,
              "cwd": "{src}",
              "foreground_cwd": "{src}"
            }}"#,
            repo = repo.path().to_str().unwrap(),
            src = src.to_str().unwrap()
        );
        let json = snapshot_json_extended(
            caller_root.path().to_str().unwrap(),
            "",
            "",
            &extra_workspaces,
            extra_tabs,
            &extra_panes,
        );
        let session = map_session_from_json(&json).unwrap();
        let caller = Caller {
            cwd: CanonicalPath::directory(caller_root.path()).unwrap(),
            workspace_id: WorkspaceId::new("w1"),
            tab_id: TabId::new("w1:t1"),
            pane_id: PaneId::new("w1:p1"),
        };
        let target = CanonicalPath::directory(&src).unwrap();
        let shells = exact_path_shell_candidates(&session, &caller, &target);
        assert_eq!(
            shells
                .iter()
                .map(|pane| pane.id.as_str())
                .collect::<Vec<_>>(),
            ["w3:p1"]
        );
    }

    #[test]
    fn inspect_session_uses_live_ids_and_rejects_stale_identity() {
        let _cli = lock_cli();
        let root = TempDir::new("live");
        let snapshot = snapshot_json(root.path().to_str().unwrap(), "", "");
        let (dir, context) = install_fake(
            &snapshot,
            &current_json(root.path().to_str().unwrap(), "w1:p1"),
            &process_json(true),
        );
        let _ = dir;
        let SessionInspection::Ready { live, session } =
            inspect_session(&context, || false).unwrap()
        else {
            panic!("expected ready inspection");
        };
        assert_eq!(
            live,
            LiveCaller {
                workspace_id: WorkspaceId::new("w1"),
                tab_id: crate::domain::TabId::new("w1:t1"),
                pane_id: PaneId::new("w1:p1"),
            }
        );
        assert_eq!(session.workspaces[0].id, WorkspaceId::new("w1"));

        let (_dir, stale_ctx) = install_fake(
            &snapshot,
            &current_json(root.path().to_str().unwrap(), "w-missing:p9"),
            &process_json(true),
        );
        assert!(matches!(
            inspect_session(&stale_ctx, || false).unwrap(),
            SessionInspection::Stale
        ));
        assert!(matches!(
            inspect_session_concurrent(&stale_ctx, super::READ_TIMEOUT, || false).unwrap(),
            SessionInspection::Stale
        ));
    }

    #[test]
    fn concurrent_inspection_overlaps_snapshot_and_live_caller() {
        let _cli = lock_cli();
        let root = TempDir::new("overlap");
        let snapshot = snapshot_json(root.path().to_str().unwrap(), "", "");
        let dir = TempDir::new("overlap-bin");
        let snapshot_path = dir.path().join("snapshot.json");
        let current_path = dir.path().join("current.json");
        let record = dir.path().join("record");
        fs::write(&snapshot_path, snapshot).unwrap();
        fs::write(
            &current_path,
            current_json(root.path().to_str().unwrap(), "w1:p1"),
        )
        .unwrap();
        let bin = write_executable(
            dir.path(),
            "herdr",
            &format!(
                r#"#!/bin/sh
set -eu
printf 'start %s %s\n' "$1" "$2" >> {record}
sleep 0.12
printf 'end %s %s\n' "$1" "$2" >> {record}
case "$1 $2" in
  "api snapshot") cat {snapshot} ;;
  "pane current") cat {current} ;;
  *) printf 'unexpected %s\n' "$*" >> {record}; echo "unexpected $*" >&2; exit 2 ;;
esac
"#,
                record = sh_single(&record.display().to_string()),
                snapshot = sh_single(&snapshot_path.display().to_string()),
                current = sh_single(&current_path.display().to_string()),
            ),
        );
        let context = inside_context(
            bin.to_str().unwrap(),
            "/tmp/nu-plugin-herdr-navigate-directory.sock",
            "w1",
            "w1:t1",
            "w1:p1",
            BTreeMap::new(),
        )
        .unwrap();
        let SessionInspection::Ready { .. } =
            inspect_session_concurrent(&context, std::time::Duration::from_secs(2), || false)
                .unwrap()
        else {
            panic!("expected ready concurrent inspection");
        };
        let recorded = fs::read_to_string(&record).unwrap();
        let snapshot_start = recorded.find("start api snapshot").expect("snapshot start");
        let current_start = recorded.find("start pane current").expect("current start");
        let first_end = recorded
            .find("end api snapshot")
            .min(recorded.find("end pane current"))
            .expect("first end");
        assert!(
            snapshot_start.min(current_start) < first_end
                && snapshot_start.max(current_start) < first_end,
            "snapshot and live-caller must overlap, record:\n{recorded}"
        );
        assert!(!recorded.contains("process-info"));
        assert!(!recorded.contains("unexpected"));
    }

    #[test]
    fn concurrent_inspection_never_calls_process_info_or_mutations() {
        let _cli = lock_cli();
        let root = TempDir::new("readonly");
        let snapshot = snapshot_json(root.path().to_str().unwrap(), "", "");
        let (dir, context) = install_fake(
            &snapshot,
            &current_json(root.path().to_str().unwrap(), "w1:p1"),
            &process_json(true),
        );
        inspect_session_concurrent(&context, super::READ_TIMEOUT, || false).unwrap();
        let recorded = fs::read_to_string(dir.path().join("record")).unwrap();
        assert!(recorded.contains("args=api snapshot"));
        assert!(recorded.contains("args=pane current --current"));
        assert!(!recorded.contains("process-info"));
        assert!(!recorded.contains("tab create"));
        assert!(!recorded.contains("workspace create"));
        assert!(!recorded.contains("pane focus"));
    }

    #[test]
    fn process_info_incomplete_and_not_found_are_distinct() {
        let _cli = lock_cli();
        let root = TempDir::new("proc");
        let snapshot = snapshot_json(root.path().to_str().unwrap(), "", "");
        let (dir, context) = install_fake(
            &snapshot,
            &current_json(root.path().to_str().unwrap(), "w1:p1"),
            &process_json(false),
        );
        let _ = dir;
        let ProcessInspection::Evidence(evidence) =
            inspect_process(&context, &PaneId::new("w1:p1"), || false).unwrap()
        else {
            panic!("expected evidence");
        };
        assert!(evidence.shell_pid.is_none());
        assert!(!crate::domain::Occupant::Shell(Some(evidence.clone())).is_idle());

        let mut session = match inspect_session(&context, || false).unwrap() {
            SessionInspection::Ready { session, .. } => session,
            SessionInspection::Stale => panic!("session"),
        };
        apply_shell_evidence(&mut session, &PaneId::new("w1:p1"), evidence);
        assert!(!session.workspaces[0].tabs[0].panes[0].occupant.is_idle());

        let err_script = TempDir::new("not-found");
        let bin = write_executable(
            err_script.path(),
            "herdr",
            &format!(
                "#!/bin/sh\nprintf '%s\\n' {body} >&2\nexit 1\n",
                body = sh_single(not_found_json())
            ),
        );
        let context = inside_context(
            bin.to_str().unwrap(),
            "/tmp/x.sock",
            "w1",
            "w1:t1",
            "w1:p1",
            BTreeMap::new(),
        )
        .unwrap();
        assert!(matches!(
            inspect_process(&context, &PaneId::new("w1:p1"), || false).unwrap(),
            ProcessInspection::NotFound
        ));

        let mismatch = TempDir::new("mismatch");
        let bin = write_executable(
            mismatch.path(),
            "herdr",
            r#"#!/bin/sh
printf '{"id":"x","result":{"type":"pane_process_info","process_info":{"pane_id":"other","shell_pid":1,"foreground_process_group_id":1,"foreground_processes":[{"pid":1,"name":"zsh"}]}}}\n'
"#,
        );
        let context = inside_context(
            bin.to_str().unwrap(),
            "/tmp/x.sock",
            "w1",
            "w1:t1",
            "w1:p1",
            BTreeMap::new(),
        )
        .unwrap();
        let err = inspect_process(&context, &PaneId::new("w1:p1"), || false).unwrap_err();
        match err {
            crate::herdr::cli::RunError::Failed(error) => {
                assert_eq!(error.kind(), crate::domain::ErrorKind::HerdrProtocol);
                assert!(error.message().contains("mismatched pane id"));
            }
            crate::herdr::cli::RunError::Interrupted => panic!("unexpected interrupt"),
        }
    }

    #[test]
    fn worktree_checkout_path_is_the_workspace_root() {
        let checkout = TempDir::new("checkout");
        let json = snapshot_json(
            "/missing-pane-cwd",
            "",
            &format!(
                r#", "worktree": {{"repo_key":"k","repo_name":"n","repo_root":"{path}","checkout_path":"{path}","is_linked_worktree":true}}"#,
                path = checkout.path().to_str().unwrap()
            ),
        );
        let output = crate::herdr::cli::CliOutput {
            stdout: json.into_bytes(),
            stderr: Vec::new(),
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
        };
        let CommandResult::Ok(raw) = parse_snapshot(&output).unwrap() else {
            panic!("snapshot");
        };
        let session = map_session(&raw).unwrap();
        assert_eq!(
            session.workspaces[0].root.as_ref(),
            Some(&CanonicalPath::directory(checkout.path()).unwrap())
        );
    }

    #[test]
    fn snapshot_rejects_duplicate_tab_and_pane_ids() {
        let root = TempDir::new("dup-ids");
        let path = root.path().to_str().unwrap();
        let duplicate_tab = r#", {
          "tab_id": "w1:t1",
          "workspace_id": "w1",
          "number": 2,
          "label": "dup",
          "focused": false,
          "pane_count": 0,
          "agent_status": "idle"
        }"#;
        let err =
            map_session_from_json(&snapshot_json_extended(path, "", "", "", duplicate_tab, ""))
                .unwrap_err();
        assert_eq!(err.kind(), crate::domain::ErrorKind::HerdrProtocol);
        assert!(err.message().contains("duplicate tab id"));

        let duplicate_pane = r#", {
          "pane_id": "w1:p1",
          "terminal_id": "term9",
          "workspace_id": "w1",
          "tab_id": "w1:t1",
          "focused": false,
          "agent_status": "idle",
          "revision": 9
        }"#;
        let err = map_session_from_json(&snapshot_json_extended(
            path,
            "",
            "",
            "",
            "",
            duplicate_pane,
        ))
        .unwrap_err();
        assert_eq!(err.kind(), crate::domain::ErrorKind::HerdrProtocol);
        assert!(err.message().contains("duplicate pane id"));
    }

    #[test]
    fn snapshot_rejects_inconsistent_focus_ids() {
        let root = TempDir::new("focus-ids");
        let json = snapshot_json(root.path().to_str().unwrap(), "", "");

        let mut unknown_workspace = raw_snapshot(&json);
        unknown_workspace.focused_workspace_id = Some("w-missing".into());
        assert_protocol_error(&unknown_workspace, "focused workspace is unknown");

        let mut empty_workspace = raw_snapshot(&json);
        empty_workspace.focused_workspace_id = Some(String::new());
        assert!(
            map_session(&empty_workspace)
                .unwrap()
                .focused_workspace_id
                .is_none()
        );

        let extra_workspace = r#", {
          "workspace_id": "w2",
          "number": 2,
          "label": "other",
          "focused": false,
          "pane_count": 0,
          "tab_count": 1,
          "active_tab_id": "w2:t1",
          "agent_status": "idle"
        }"#;
        let extra_tab = r#", {
          "tab_id": "w2:t1",
          "workspace_id": "w2",
          "number": 1,
          "label": "other",
          "focused": false,
          "pane_count": 0,
          "agent_status": "idle"
        }"#;
        let mut foreign_tab = raw_snapshot(&snapshot_json_extended(
            root.path().to_str().unwrap(),
            "",
            "",
            extra_workspace,
            extra_tab,
            "",
        ));
        foreign_tab.workspaces[0].active_tab_id = "w2:t1".into();
        assert_protocol_error(
            &foreign_tab,
            "active tab is unknown or not in this workspace",
        );

        let mut layout_workspace = raw_snapshot(&json);
        layout_workspace.layouts[0].workspace_id = "w2".into();
        assert_protocol_error(&layout_workspace, "layout workspace does not match its tab");

        let mut layout_pane = raw_snapshot(&json);
        layout_pane.layouts[0].focused_pane_id = "w1:missing".into();
        assert_protocol_error(
            &layout_pane,
            "layout focused pane is unknown or not in this tab",
        );
    }

    #[test]
    fn snapshot_rejects_pane_workspace_that_does_not_match_its_tab() {
        let root = TempDir::new("mismatch-ids");
        let extra_workspace = r#", {
          "workspace_id": "w2",
          "number": 2,
          "label": "other",
          "focused": false,
          "pane_count": 0,
          "tab_count": 0,
          "active_tab_id": "w2:t1",
          "agent_status": "idle"
        }"#;
        let mismatched_pane = r#", {
          "pane_id": "w2:p9",
          "terminal_id": "term9",
          "workspace_id": "w2",
          "tab_id": "w1:t1",
          "focused": false,
          "agent_status": "idle",
          "revision": 9
        }"#;
        let err = map_session_from_json(&snapshot_json_extended(
            root.path().to_str().unwrap(),
            "",
            "",
            extra_workspace,
            "",
            mismatched_pane,
        ))
        .unwrap_err();
        assert_eq!(err.kind(), crate::domain::ErrorKind::HerdrProtocol);
        assert!(err.message().contains("workspace does not match its tab"));
    }

    #[test]
    fn invalid_workspace_root_is_excluded_without_failing() {
        let json = snapshot_json("/missing-root-does-not-exist", "", "");
        let output = crate::herdr::cli::CliOutput {
            stdout: json.into_bytes(),
            stderr: Vec::new(),
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
        };
        let CommandResult::Ok(raw) = parse_snapshot(&output).unwrap() else {
            panic!("snapshot");
        };
        let session = map_session(&raw).unwrap();
        assert!(session.workspaces[0].root.is_none());
        assert!(
            session.workspaces[0].tabs[0].panes[0]
                .foreground_cwd
                .is_none()
        );
    }
}
