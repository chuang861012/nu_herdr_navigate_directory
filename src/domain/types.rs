//! Typed session views, idle evidence, and domain actions.

use super::path::CanonicalPath;

/// Live caller identity used by the inside-Herdr decision function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Caller {
    pub cwd: CanonicalPath,
    pub workspace_id: WorkspaceId,
    pub tab_id: TabId,
    pub pane_id: PaneId,
}

/// Authoritative session snapshot already mapped away from transport JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Session {
    pub focused_workspace_id: Option<WorkspaceId>,
    pub workspaces: Vec<Workspace>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Workspace {
    pub id: WorkspaceId,
    pub root: Option<CanonicalPath>,
    pub focused_tab_id: Option<TabId>,
    pub tabs: Vec<Tab>,
    pub label: String,
    pub number: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tab {
    pub id: TabId,
    pub focused_pane_id: Option<PaneId>,
    pub panes: Vec<Pane>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pane {
    pub id: PaneId,
    pub foreground_cwd: Option<CanonicalPath>,
    pub occupant: Occupant,
}

/// Pane occupant after agent detection. Missing evidence is ineligible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Occupant {
    Shell(Option<ShellProcessEvidence>),
    Agent(AgentStatus),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentStatus {
    Idle,
    Done,
    Working,
    Blocked,
    Unknown,
}

/// Already-collected `pane process-info` fields used to prove an idle shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellProcessEvidence {
    pub shell_pid: Option<u32>,
    pub foreground_process_group_id: Option<u32>,
    pub foreground_processes: Vec<ForegroundProcess>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForegroundProcess {
    pub pid: u32,
}

/// Navigation action produced by the pure decision function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    NoOp,
    ChangeDirectory {
        path: CanonicalPath,
    },
    FocusPane {
        pane_id: PaneId,
    },
    CreateTab {
        workspace_id: WorkspaceId,
        cwd: CanonicalPath,
    },
    CreateWorkspace {
        cwd: CanonicalPath,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct WorkspaceId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct TabId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PaneId(String);

impl WorkspaceId {
    pub(crate) fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl TabId {
    pub(crate) fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl PaneId {
    pub(crate) fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl Occupant {
    pub(crate) fn is_idle(&self) -> bool {
        match self {
            Self::Shell(None) => false,
            Self::Shell(Some(evidence)) => evidence.proves_idle_interactive_shell(),
            Self::Agent(status) => matches!(status, AgentStatus::Idle | AgentStatus::Done),
        }
    }
}

impl ShellProcessEvidence {
    fn proves_idle_interactive_shell(&self) -> bool {
        let Some(shell_pid) = self.shell_pid else {
            return false;
        };
        if self.foreground_process_group_id.is_none() {
            return false;
        }
        matches!(
            self.foreground_processes.as_slice(),
            [process] if process.pid == shell_pid
        )
    }
}

impl Pane {
    pub(crate) fn is_eligible_at(&self, target: &CanonicalPath) -> bool {
        self.foreground_cwd.as_ref() == Some(target) && self.occupant.is_idle()
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentStatus, ForegroundProcess, Occupant, Pane, PaneId, ShellProcessEvidence};
    use crate::domain::path::CanonicalPath;

    fn shell(shell_pid: Option<u32>, pgid: Option<u32>, pids: &[u32]) -> Occupant {
        Occupant::Shell(Some(ShellProcessEvidence {
            shell_pid,
            foreground_process_group_id: pgid,
            foreground_processes: pids
                .iter()
                .copied()
                .map(|pid| ForegroundProcess { pid })
                .collect(),
        }))
    }

    #[test]
    fn agent_idle_and_done_are_eligible_other_states_are_not() {
        assert!(Occupant::Agent(AgentStatus::Idle).is_idle());
        assert!(Occupant::Agent(AgentStatus::Done).is_idle());
        assert!(!Occupant::Agent(AgentStatus::Working).is_idle());
        assert!(!Occupant::Agent(AgentStatus::Blocked).is_idle());
        assert!(!Occupant::Agent(AgentStatus::Unknown).is_idle());
    }

    #[test]
    fn shell_is_idle_only_with_complete_foreground_proof() {
        assert!(shell(Some(7), Some(7), &[7]).is_idle());
        assert!(shell(Some(7), Some(99), &[7]).is_idle());
        assert!(!Occupant::Shell(None).is_idle());
        assert!(!shell(None, Some(7), &[7]).is_idle());
        assert!(!shell(Some(7), None, &[7]).is_idle());
        assert!(!shell(Some(7), Some(7), &[]).is_idle());
        assert!(!shell(Some(7), Some(7), &[7, 8]).is_idle());
        assert!(!shell(Some(7), Some(8), &[8]).is_idle());
    }

    #[test]
    fn pane_without_canonical_foreground_cwd_cannot_match() {
        let target = CanonicalPath::from_parts_for_test("/repo");
        let pane = Pane {
            id: PaneId::new("p1"),
            foreground_cwd: None,
            occupant: Occupant::Agent(AgentStatus::Idle),
        };
        assert!(!pane.is_eligible_at(&target));
    }

    #[test]
    fn resource_ids_preserve_their_string_values() {
        assert_eq!(super::WorkspaceId::new("ws-a").as_str(), "ws-a");
        assert_eq!(super::TabId::new("tab-a").as_str(), "tab-a");
        assert_eq!(PaneId::new("pane-a").as_str(), "pane-a");
    }
}
