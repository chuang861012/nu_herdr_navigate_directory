//! Deterministic inside-Herdr navigation decision tree.

use super::path::CanonicalPath;
use super::types::{
    Action, AgentIdlePolicy, Caller, Pane, Session, Tab, TabId, Workspace, WorkspaceId,
};

/// Choose the next navigation action from already-canonical paths and typed evidence.
///
/// The function is free of I/O, environment reads, clocks, process execution, and
/// global state. Recheck-before-create belongs to command orchestration.
pub(crate) fn decide(
    caller: &Caller,
    session: &Session,
    target: &CanonicalPath,
    policy: &AgentIdlePolicy,
) -> Action {
    if &caller.cwd == target {
        return Action::NoOp;
    }

    if let Some(workspace) = find_workspace(session, &caller.workspace_id)
        && let Some(pane) = select_eligible_pane(workspace, target, Some(&caller.tab_id), policy)
    {
        return Action::FocusPane {
            pane_id: pane.id.clone(),
        };
    }

    if caller.cwd.is_strict_ancestor_of(target) {
        return Action::ChangeDirectory {
            path: target.clone(),
        };
    }

    match nearest_containing_workspace(session, &caller.workspace_id, target) {
        Some(workspace) => {
            if let Some(pane) = select_eligible_pane(workspace, target, None, policy) {
                Action::FocusPane {
                    pane_id: pane.id.clone(),
                }
            } else {
                Action::CreateTab {
                    workspace_id: workspace.id.clone(),
                    cwd: target.clone(),
                }
            }
        }
        None => Action::CreateWorkspace {
            cwd: target.clone(),
        },
    }
}

fn find_workspace<'a>(session: &'a Session, id: &WorkspaceId) -> Option<&'a Workspace> {
    session
        .workspaces
        .iter()
        .find(|workspace| &workspace.id == id)
}

pub(crate) fn nearest_containing_workspace<'a>(
    session: &'a Session,
    caller_workspace_id: &WorkspaceId,
    target: &CanonicalPath,
) -> Option<&'a Workspace> {
    session
        .workspaces
        .iter()
        .enumerate()
        .filter(|(_, workspace)| {
            workspace
                .root
                .as_ref()
                .is_some_and(|root| root.contains(target))
        })
        .max_by_key(|(index, workspace)| {
            let depth = workspace
                .root
                .as_ref()
                .map(CanonicalPath::depth)
                .expect("containing workspaces have a canonical root");
            let is_caller = &workspace.id == caller_workspace_id;
            let is_focused = session.focused_workspace_id.as_ref() == Some(&workspace.id);
            (depth, is_caller, is_focused, std::cmp::Reverse(*index))
        })
        .map(|(_, workspace)| workspace)
}

fn select_eligible_pane<'a>(
    workspace: &'a Workspace,
    target: &CanonicalPath,
    caller_tab_id: Option<&TabId>,
    policy: &AgentIdlePolicy,
) -> Option<&'a Pane> {
    workspace
        .tabs
        .iter()
        .enumerate()
        .flat_map(|(tab_index, tab)| {
            tab.panes
                .iter()
                .enumerate()
                .filter_map(move |(pane_index, pane)| {
                    pane.is_eligible_at(target, policy).then_some((
                        pane_rank(workspace, tab, tab_index, pane, pane_index, caller_tab_id),
                        pane,
                    ))
                })
        })
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, pane)| pane)
}

fn pane_rank(
    workspace: &Workspace,
    tab: &Tab,
    tab_index: usize,
    pane: &Pane,
    pane_index: usize,
    caller_tab_id: Option<&TabId>,
) -> (u8, u8, usize, usize) {
    let tab_tier = if caller_tab_id.is_some_and(|id| id == &tab.id) {
        0
    } else if workspace.focused_tab_id.as_ref() == Some(&tab.id) {
        1
    } else {
        2
    };
    let pane_tier = if tab.focused_pane_id.as_ref() == Some(&pane.id) {
        0
    } else {
        1
    };
    (tab_tier, pane_tier, tab_index, pane_index)
}

#[cfg(test)]
mod tests {
    use super::decide as decide_with_policy;
    use crate::domain::path::CanonicalPath;
    use crate::domain::types::{
        Action, AgentIdlePolicy, AgentStatus, Caller, ForegroundProcess, Occupant, Pane, PaneId,
        Session, ShellProcessEvidence, Tab, TabId, Workspace, WorkspaceId,
    };

    fn cp(path: &str) -> CanonicalPath {
        CanonicalPath::from_parts_for_test(path)
    }

    fn decide(caller: &Caller, session: &Session, target: &CanonicalPath) -> Action {
        decide_with_policy(caller, session, target, &AgentIdlePolicy::default())
    }

    fn caller(cwd: &str, workspace: &str, tab: &str, pane: &str) -> Caller {
        Caller {
            cwd: cp(cwd),
            workspace_id: WorkspaceId::new(workspace),
            tab_id: TabId::new(tab),
            pane_id: PaneId::new(pane),
        }
    }

    fn session(focused: Option<&str>, workspaces: Vec<Workspace>) -> Session {
        Session {
            focused_workspace_id: focused.map(WorkspaceId::new),
            workspaces,
        }
    }

    fn workspace(
        id: &str,
        root: Option<&str>,
        focused_tab: Option<&str>,
        tabs: Vec<Tab>,
    ) -> Workspace {
        Workspace {
            id: WorkspaceId::new(id),
            root: root.map(cp),
            focused_tab_id: focused_tab.map(TabId::new),
            tabs,
            label: id.to_string(),
            number: 1,
        }
    }

    fn tab(id: &str, focused_pane: Option<&str>, panes: Vec<Pane>) -> Tab {
        Tab {
            id: TabId::new(id),
            focused_pane_id: focused_pane.map(PaneId::new),
            panes,
        }
    }

    fn pane(id: &str, cwd: Option<&str>, occupant: Occupant) -> Pane {
        Pane {
            id: PaneId::new(id),
            foreground_cwd: cwd.map(cp),
            occupant,
        }
    }

    fn idle_shell() -> Occupant {
        Occupant::Shell(Some(ShellProcessEvidence {
            shell_pid: Some(42),
            foreground_process_group_id: Some(42),
            foreground_processes: vec![ForegroundProcess { pid: 42 }],
        }))
    }

    fn busy_shell() -> Occupant {
        Occupant::Shell(Some(ShellProcessEvidence {
            shell_pid: Some(42),
            foreground_process_group_id: Some(99),
            foreground_processes: vec![
                ForegroundProcess { pid: 42 },
                ForegroundProcess { pid: 99 },
            ],
        }))
    }

    fn unproven_shell() -> Occupant {
        Occupant::Shell(None)
    }

    fn idle_agent() -> Occupant {
        Occupant::Agent(AgentStatus::Idle)
    }

    fn done_agent() -> Occupant {
        Occupant::Agent(AgentStatus::Done)
    }

    fn working_agent() -> Occupant {
        Occupant::Agent(AgentStatus::Working)
    }

    fn blocked_agent() -> Occupant {
        Occupant::Agent(AgentStatus::Blocked)
    }

    fn unknown_agent() -> Occupant {
        Occupant::Agent(AgentStatus::Unknown)
    }

    fn focus(pane_id: &str) -> Action {
        Action::FocusPane {
            pane_id: PaneId::new(pane_id),
        }
    }

    fn cd(path: &str) -> Action {
        Action::ChangeDirectory { path: cp(path) }
    }

    fn create_tab(workspace_id: &str, cwd: &str) -> Action {
        Action::CreateTab {
            workspace_id: WorkspaceId::new(workspace_id),
            cwd: cp(cwd),
        }
    }

    fn create_workspace(cwd: &str) -> Action {
        Action::CreateWorkspace { cwd: cp(cwd) }
    }

    fn caller_repo_workspace(occupant: Occupant) -> Vec<Workspace> {
        vec![workspace(
            "ws-a",
            Some("/repo"),
            Some("tab-a"),
            vec![tab(
                "tab-a",
                Some("pane-a"),
                vec![pane("pane-a", Some("/repo/src"), occupant)],
            )],
        )]
    }

    #[test]
    fn decision_table_covers_approved_branches() {
        struct Case {
            name: &'static str,
            caller: Caller,
            target: &'static str,
            session: Session,
            expected: Action,
        }

        let cases = [
            Case {
                name: "target equals cwd is NoOp without idle evidence",
                caller: caller("/repo/src", "ws-a", "tab-a", "pane-a"),
                target: "/repo/src",
                session: session(Some("ws-a"), caller_repo_workspace(unproven_shell())),
                expected: Action::NoOp,
            },
            Case {
                name: "target equals cwd is NoOp even when another idle pane exists",
                caller: caller("/repo/src", "ws-a", "tab-a", "pane-a"),
                target: "/repo/src",
                session: session(
                    Some("ws-a"),
                    vec![workspace(
                        "ws-a",
                        Some("/repo"),
                        Some("tab-a"),
                        vec![tab(
                            "tab-a",
                            Some("pane-a"),
                            vec![
                                pane("pane-a", Some("/repo/src"), unproven_shell()),
                                pane("pane-b", Some("/repo/src"), idle_shell()),
                            ],
                        )],
                    )],
                ),
                expected: Action::NoOp,
            },
            Case {
                name: "strict descendant without idle pane changes directory",
                caller: caller("/repo", "ws-a", "tab-a", "pane-a"),
                target: "/repo/src",
                session: session(
                    Some("ws-a"),
                    vec![workspace(
                        "ws-a",
                        Some("/repo"),
                        Some("tab-a"),
                        vec![tab(
                            "tab-a",
                            Some("pane-a"),
                            vec![pane("pane-a", Some("/repo"), idle_shell())],
                        )],
                    )],
                ),
                expected: cd("/repo/src"),
            },
            Case {
                name: "same-workspace idle pane beats cwd descent",
                caller: caller("/repo", "ws-a", "tab-a", "pane-a"),
                target: "/repo/src",
                session: session(
                    Some("ws-a"),
                    vec![workspace(
                        "ws-a",
                        Some("/repo"),
                        Some("tab-a"),
                        vec![tab(
                            "tab-a",
                            Some("pane-a"),
                            vec![
                                pane("pane-a", Some("/repo"), idle_shell()),
                                pane("pane-src", Some("/repo/src"), idle_shell()),
                            ],
                        )],
                    )],
                ),
                expected: focus("pane-src"),
            },
            Case {
                name: "parent path from nested cwd does not change directory",
                caller: caller("/repo/src", "ws-a", "tab-a", "pane-a"),
                target: "/repo",
                session: session(Some("ws-a"), caller_repo_workspace(idle_shell())),
                expected: create_tab("ws-a", "/repo"),
            },
            Case {
                name: "sibling path creates a tab in the containing workspace",
                caller: caller("/repo/src", "ws-a", "tab-a", "pane-a"),
                target: "/repo/docs",
                session: session(Some("ws-a"), caller_repo_workspace(idle_shell())),
                expected: create_tab("ws-a", "/repo/docs"),
            },
            Case {
                name: "unrelated path without a containing workspace creates a workspace",
                caller: caller("/repo/src", "ws-a", "tab-a", "pane-a"),
                target: "/other",
                session: session(Some("ws-a"), caller_repo_workspace(idle_shell())),
                expected: create_workspace("/other"),
            },
            Case {
                name: "nearest containing workspace is the deepest root",
                caller: caller("/home/src", "ws-home", "tab-home", "pane-home"),
                target: "/repo/src",
                session: session(
                    Some("ws-root"),
                    vec![
                        workspace(
                            "ws-root",
                            Some("/"),
                            Some("tab-root"),
                            vec![tab("tab-root", None, vec![])],
                        ),
                        workspace(
                            "ws-home",
                            Some("/home"),
                            Some("tab-home"),
                            vec![tab(
                                "tab-home",
                                Some("pane-home"),
                                vec![pane("pane-home", Some("/home/src"), idle_shell())],
                            )],
                        ),
                        workspace(
                            "ws-repo",
                            Some("/repo"),
                            Some("tab-repo"),
                            vec![tab("tab-repo", None, vec![])],
                        ),
                    ],
                ),
                expected: create_tab("ws-repo", "/repo/src"),
            },
            Case {
                name: "equal-depth duplicate roots prefer the caller workspace",
                caller: caller("/tmp", "ws-a", "tab-a", "pane-a"),
                target: "/repo/src",
                session: session(
                    Some("ws-b"),
                    vec![
                        workspace(
                            "ws-b",
                            Some("/repo"),
                            Some("tab-b"),
                            vec![tab("tab-b", None, vec![])],
                        ),
                        workspace(
                            "ws-a",
                            Some("/repo"),
                            Some("tab-a"),
                            vec![tab(
                                "tab-a",
                                Some("pane-a"),
                                vec![pane("pane-a", Some("/tmp"), idle_shell())],
                            )],
                        ),
                    ],
                ),
                expected: create_tab("ws-a", "/repo/src"),
            },
            Case {
                name: "equal-depth duplicate roots prefer the focused workspace when caller does not contain",
                caller: caller("/tmp", "ws-caller", "tab-c", "pane-c"),
                target: "/repo/src",
                session: session(
                    Some("ws-b"),
                    vec![
                        workspace(
                            "ws-caller",
                            Some("/tmp"),
                            Some("tab-c"),
                            vec![tab(
                                "tab-c",
                                Some("pane-c"),
                                vec![pane("pane-c", Some("/tmp"), idle_shell())],
                            )],
                        ),
                        workspace(
                            "ws-a",
                            Some("/repo"),
                            Some("tab-a"),
                            vec![tab("tab-a", None, vec![])],
                        ),
                        workspace(
                            "ws-b",
                            Some("/repo"),
                            Some("tab-b"),
                            vec![tab("tab-b", None, vec![])],
                        ),
                    ],
                ),
                expected: create_tab("ws-b", "/repo/src"),
            },
            Case {
                name: "equal-depth duplicate roots fall back to list order",
                caller: caller("/tmp", "ws-caller", "tab-c", "pane-c"),
                target: "/repo/src",
                session: session(
                    Some("ws-caller"),
                    vec![
                        workspace(
                            "ws-caller",
                            Some("/tmp"),
                            Some("tab-c"),
                            vec![tab(
                                "tab-c",
                                Some("pane-c"),
                                vec![pane("pane-c", Some("/tmp"), idle_shell())],
                            )],
                        ),
                        workspace(
                            "ws-a",
                            Some("/repo"),
                            Some("tab-a"),
                            vec![tab("tab-a", None, vec![])],
                        ),
                        workspace(
                            "ws-b",
                            Some("/repo"),
                            Some("tab-b"),
                            vec![tab("tab-b", None, vec![])],
                        ),
                    ],
                ),
                expected: create_tab("ws-a", "/repo/src"),
            },
            Case {
                name: "exact-path pane in a non-containing workspace is ignored",
                caller: caller("/repo/src", "ws-a", "tab-a", "pane-a"),
                target: "/other",
                session: session(
                    Some("ws-a"),
                    vec![
                        workspace(
                            "ws-a",
                            Some("/repo"),
                            Some("tab-a"),
                            vec![tab(
                                "tab-a",
                                Some("pane-a"),
                                vec![pane("pane-a", Some("/repo/src"), idle_shell())],
                            )],
                        ),
                        workspace(
                            "ws-b",
                            Some("/unrelated"),
                            Some("tab-b"),
                            vec![tab(
                                "tab-b",
                                Some("pane-b"),
                                vec![pane("pane-b", Some("/other"), idle_shell())],
                            )],
                        ),
                    ],
                ),
                expected: create_workspace("/other"),
            },
            Case {
                name: "idle shell pane in the caller workspace is focused",
                caller: caller("/repo/src", "ws-a", "tab-a", "pane-a"),
                target: "/repo/docs",
                session: session(
                    Some("ws-a"),
                    vec![workspace(
                        "ws-a",
                        Some("/repo"),
                        Some("tab-a"),
                        vec![tab(
                            "tab-a",
                            Some("pane-a"),
                            vec![
                                pane("pane-a", Some("/repo/src"), idle_shell()),
                                pane("pane-docs", Some("/repo/docs"), idle_shell()),
                            ],
                        )],
                    )],
                ),
                expected: focus("pane-docs"),
            },
            Case {
                name: "idle agent pane is eligible",
                caller: caller("/repo/src", "ws-a", "tab-a", "pane-a"),
                target: "/repo/docs",
                session: session(
                    Some("ws-a"),
                    vec![workspace(
                        "ws-a",
                        Some("/repo"),
                        Some("tab-a"),
                        vec![tab(
                            "tab-a",
                            Some("pane-a"),
                            vec![
                                pane("pane-a", Some("/repo/src"), idle_shell()),
                                pane("pane-docs", Some("/repo/docs"), idle_agent()),
                            ],
                        )],
                    )],
                ),
                expected: focus("pane-docs"),
            },
            Case {
                name: "done agent pane is eligible",
                caller: caller("/repo/src", "ws-a", "tab-a", "pane-a"),
                target: "/repo/docs",
                session: session(
                    Some("ws-a"),
                    vec![workspace(
                        "ws-a",
                        Some("/repo"),
                        Some("tab-a"),
                        vec![tab(
                            "tab-a",
                            Some("pane-a"),
                            vec![pane("pane-docs", Some("/repo/docs"), done_agent())],
                        )],
                    )],
                ),
                expected: focus("pane-docs"),
            },
            Case {
                name: "working agent is skipped then directory change proceeds",
                caller: caller("/repo", "ws-a", "tab-a", "pane-a"),
                target: "/repo/src",
                session: session(
                    Some("ws-a"),
                    vec![workspace(
                        "ws-a",
                        Some("/repo"),
                        Some("tab-a"),
                        vec![tab(
                            "tab-a",
                            Some("pane-busy"),
                            vec![pane("pane-busy", Some("/repo/src"), working_agent())],
                        )],
                    )],
                ),
                expected: cd("/repo/src"),
            },
            Case {
                name: "blocked agent is skipped then a tab is created",
                caller: caller("/repo/src", "ws-a", "tab-a", "pane-a"),
                target: "/repo/docs",
                session: session(
                    Some("ws-a"),
                    vec![workspace(
                        "ws-a",
                        Some("/repo"),
                        Some("tab-a"),
                        vec![tab(
                            "tab-a",
                            Some("pane-a"),
                            vec![
                                pane("pane-a", Some("/repo/src"), idle_shell()),
                                pane("pane-docs", Some("/repo/docs"), blocked_agent()),
                            ],
                        )],
                    )],
                ),
                expected: create_tab("ws-a", "/repo/docs"),
            },
            Case {
                name: "unknown agent and unproven shell are skipped",
                caller: caller("/repo/src", "ws-a", "tab-a", "pane-a"),
                target: "/repo/docs",
                session: session(
                    Some("ws-a"),
                    vec![workspace(
                        "ws-a",
                        Some("/repo"),
                        Some("tab-a"),
                        vec![tab(
                            "tab-a",
                            Some("pane-a"),
                            vec![
                                pane("pane-unknown", Some("/repo/docs"), unknown_agent()),
                                pane("pane-unproven", Some("/repo/docs"), unproven_shell()),
                            ],
                        )],
                    )],
                ),
                expected: create_tab("ws-a", "/repo/docs"),
            },
            Case {
                name: "busy shell at the exact path is skipped then a workspace is created",
                caller: caller("/repo/src", "ws-a", "tab-a", "pane-a"),
                target: "/other",
                session: session(
                    Some("ws-a"),
                    vec![workspace(
                        "ws-a",
                        Some("/repo"),
                        Some("tab-a"),
                        vec![tab(
                            "tab-a",
                            Some("pane-a"),
                            vec![
                                pane("pane-a", Some("/repo/src"), idle_shell()),
                                pane("pane-other", Some("/other"), busy_shell()),
                            ],
                        )],
                    )],
                ),
                expected: create_workspace("/other"),
            },
            Case {
                name: "focused pane in the caller tab wins over earlier list order",
                caller: caller("/repo", "ws-a", "tab-a", "pane-a"),
                target: "/repo/src",
                session: session(
                    Some("ws-a"),
                    vec![workspace(
                        "ws-a",
                        Some("/repo"),
                        Some("tab-b"),
                        vec![tab(
                            "tab-a",
                            Some("pane-focus"),
                            vec![
                                pane("pane-first", Some("/repo/src"), idle_shell()),
                                pane("pane-focus", Some("/repo/src"), idle_agent()),
                            ],
                        )],
                    )],
                ),
                expected: focus("pane-focus"),
            },
            Case {
                name: "caller tab wins over the workspace focused tab",
                caller: caller("/repo", "ws-a", "tab-a", "pane-a"),
                target: "/repo/src",
                session: session(
                    Some("ws-a"),
                    vec![workspace(
                        "ws-a",
                        Some("/repo"),
                        Some("tab-b"),
                        vec![
                            tab(
                                "tab-b",
                                Some("pane-focused-tab"),
                                vec![pane("pane-focused-tab", Some("/repo/src"), idle_shell())],
                            ),
                            tab(
                                "tab-a",
                                Some("pane-a"),
                                vec![pane("pane-caller-tab", Some("/repo/src"), done_agent())],
                            ),
                        ],
                    )],
                ),
                expected: focus("pane-caller-tab"),
            },
            Case {
                name: "focused tab in another workspace wins over list order",
                caller: caller("/tmp", "ws-caller", "tab-c", "pane-c"),
                target: "/repo/src",
                session: session(
                    Some("ws-repo"),
                    vec![
                        workspace(
                            "ws-caller",
                            Some("/tmp"),
                            Some("tab-c"),
                            vec![tab(
                                "tab-c",
                                Some("pane-c"),
                                vec![pane("pane-c", Some("/tmp"), idle_shell())],
                            )],
                        ),
                        workspace(
                            "ws-repo",
                            Some("/repo"),
                            Some("tab-b"),
                            vec![
                                tab(
                                    "tab-a",
                                    Some("pane-a"),
                                    vec![pane("pane-a", Some("/repo/src"), idle_shell())],
                                ),
                                tab(
                                    "tab-b",
                                    Some("pane-b"),
                                    vec![pane("pane-b", Some("/repo/src"), idle_agent())],
                                ),
                            ],
                        ),
                    ],
                ),
                expected: focus("pane-b"),
            },
            Case {
                name: "occupant type does not outrank focus",
                caller: caller("/repo", "ws-a", "tab-a", "pane-a"),
                target: "/repo/src",
                session: session(
                    Some("ws-a"),
                    vec![workspace(
                        "ws-a",
                        Some("/repo"),
                        Some("tab-a"),
                        vec![tab(
                            "tab-a",
                            Some("pane-shell"),
                            vec![
                                pane("pane-agent", Some("/repo/src"), idle_agent()),
                                pane("pane-shell", Some("/repo/src"), idle_shell()),
                            ],
                        )],
                    )],
                ),
                expected: focus("pane-shell"),
            },
            Case {
                name: "stable list order is used when focus does not distinguish panes",
                caller: caller("/tmp", "ws-caller", "tab-c", "pane-c"),
                target: "/repo/src",
                session: session(
                    Some("ws-repo"),
                    vec![
                        workspace(
                            "ws-caller",
                            Some("/tmp"),
                            Some("tab-c"),
                            vec![tab(
                                "tab-c",
                                Some("pane-c"),
                                vec![pane("pane-c", Some("/tmp"), idle_shell())],
                            )],
                        ),
                        workspace(
                            "ws-repo",
                            Some("/repo"),
                            None,
                            vec![
                                tab(
                                    "tab-a",
                                    None,
                                    vec![
                                        pane("pane-a", Some("/repo/src"), idle_shell()),
                                        pane("pane-b", Some("/repo/src"), idle_agent()),
                                    ],
                                ),
                                tab(
                                    "tab-b",
                                    None,
                                    vec![pane("pane-c2", Some("/repo/src"), done_agent())],
                                ),
                            ],
                        ),
                    ],
                ),
                expected: focus("pane-a"),
            },
            Case {
                name: "invalid workspace root is excluded from containment",
                caller: caller("/repo/src", "ws-a", "tab-a", "pane-a"),
                target: "/other",
                session: session(
                    Some("ws-a"),
                    vec![
                        workspace(
                            "ws-a",
                            Some("/repo"),
                            Some("tab-a"),
                            vec![tab(
                                "tab-a",
                                Some("pane-a"),
                                vec![pane("pane-a", Some("/repo/src"), idle_shell())],
                            )],
                        ),
                        workspace(
                            "ws-broken",
                            None,
                            Some("tab-b"),
                            vec![tab("tab-b", None, vec![])],
                        ),
                    ],
                ),
                expected: create_workspace("/other"),
            },
            Case {
                name: "invalid pane foreground cwd cannot match the target",
                caller: caller("/repo", "ws-a", "tab-a", "pane-a"),
                target: "/repo/src",
                session: session(
                    Some("ws-a"),
                    vec![workspace(
                        "ws-a",
                        Some("/repo"),
                        Some("tab-a"),
                        vec![tab(
                            "tab-a",
                            Some("pane-a"),
                            vec![pane("pane-missing-cwd", None, idle_shell())],
                        )],
                    )],
                ),
                expected: cd("/repo/src"),
            },
            Case {
                name: "workspace rooted at / contains an unrelated target",
                caller: caller("/repo/src", "ws-a", "tab-a", "pane-a"),
                target: "/other",
                session: session(
                    Some("ws-a"),
                    vec![
                        workspace(
                            "ws-a",
                            Some("/repo"),
                            Some("tab-a"),
                            vec![tab(
                                "tab-a",
                                Some("pane-a"),
                                vec![pane("pane-a", Some("/repo/src"), idle_shell())],
                            )],
                        ),
                        workspace(
                            "ws-root",
                            Some("/"),
                            Some("tab-root"),
                            vec![tab("tab-root", None, vec![])],
                        ),
                    ],
                ),
                expected: create_tab("ws-root", "/other"),
            },
            Case {
                name: "component-prefix workspace root does not contain a neighbor path",
                caller: caller("/repo-a/src", "ws-a", "tab-a", "pane-a"),
                target: "/repo-ab",
                session: session(
                    Some("ws-a"),
                    vec![workspace(
                        "ws-a",
                        Some("/repo-a"),
                        Some("tab-a"),
                        vec![tab(
                            "tab-a",
                            Some("pane-a"),
                            vec![pane("pane-a", Some("/repo-a/src"), idle_shell())],
                        )],
                    )],
                ),
                expected: create_workspace("/repo-ab"),
            },
            Case {
                name: "idle pane in a shallower containing workspace is ignored after a deeper workspace is selected",
                caller: caller("/home", "ws-home", "tab-home", "pane-home"),
                target: "/repo/src",
                session: session(
                    Some("ws-root"),
                    vec![
                        workspace(
                            "ws-root",
                            Some("/"),
                            Some("tab-root"),
                            vec![tab(
                                "tab-root",
                                Some("pane-root"),
                                vec![pane("pane-root", Some("/repo/src"), idle_shell())],
                            )],
                        ),
                        workspace(
                            "ws-home",
                            Some("/home"),
                            Some("tab-home"),
                            vec![tab(
                                "tab-home",
                                Some("pane-home"),
                                vec![pane("pane-home", Some("/home"), idle_shell())],
                            )],
                        ),
                        workspace(
                            "ws-repo",
                            Some("/repo"),
                            Some("tab-repo"),
                            vec![tab("tab-repo", None, vec![])],
                        ),
                    ],
                ),
                expected: create_tab("ws-repo", "/repo/src"),
            },
            Case {
                name: "same-workspace exact pane is used before root containment",
                caller: caller("/repo", "ws-a", "tab-a", "pane-a"),
                target: "/other",
                session: session(
                    Some("ws-a"),
                    vec![workspace(
                        "ws-a",
                        Some("/repo"),
                        Some("tab-a"),
                        vec![tab(
                            "tab-a",
                            Some("pane-a"),
                            vec![
                                pane("pane-a", Some("/repo"), idle_shell()),
                                pane("pane-other", Some("/other"), idle_shell()),
                            ],
                        )],
                    )],
                ),
                expected: focus("pane-other"),
            },
        ];

        for case in cases {
            let action = decide(&case.caller, &case.session, &cp(case.target));
            assert_eq!(action, case.expected, "{}", case.name);
        }
    }

    #[test]
    fn parent_navigation_never_returns_change_directory_without_a_same_workspace_match() {
        let action = decide(
            &caller("/repo/src", "ws-a", "tab-a", "pane-a"),
            &session(Some("ws-a"), caller_repo_workspace(busy_shell())),
            &cp("/repo"),
        );
        assert!(
            !matches!(action, Action::ChangeDirectory { .. }),
            "hnd .. must not change the current pane directory, got {action:?}"
        );
        assert_eq!(action, create_tab("ws-a", "/repo"));
    }

    #[test]
    fn agent_policy_controls_only_agent_eligibility() {
        let caller_view = caller("/repo", "ws-a", "tab-a", "pane-a");
        let session_with = |occupant| {
            session(
                Some("ws-a"),
                vec![workspace(
                    "ws-a",
                    Some("/repo"),
                    Some("tab-a"),
                    vec![tab(
                        "tab-a",
                        Some("pane-target"),
                        vec![pane("pane-target", Some("/repo/src"), occupant)],
                    )],
                )],
            )
        };
        let cases = [
            (
                "default idle",
                idle_agent(),
                AgentIdlePolicy::default(),
                focus("pane-target"),
            ),
            (
                "default done",
                done_agent(),
                AgentIdlePolicy::default(),
                focus("pane-target"),
            ),
            (
                "done removed",
                done_agent(),
                AgentIdlePolicy::from_statuses([AgentStatus::Idle]),
                cd("/repo/src"),
            ),
            (
                "configured blocked",
                blocked_agent(),
                AgentIdlePolicy::from_statuses([AgentStatus::Blocked]),
                focus("pane-target"),
            ),
            (
                "configured working",
                working_agent(),
                AgentIdlePolicy::from_statuses([AgentStatus::Working]),
                focus("pane-target"),
            ),
            (
                "unknown remains ineligible",
                unknown_agent(),
                AgentIdlePolicy::from_statuses([AgentStatus::Unknown]),
                cd("/repo/src"),
            ),
            (
                "empty agent policy preserves idle shell",
                idle_shell(),
                AgentIdlePolicy::from_statuses([]),
                focus("pane-target"),
            ),
        ];
        for (name, occupant, policy, expected) in cases {
            assert_eq!(
                decide_with_policy(
                    &caller_view,
                    &session_with(occupant),
                    &cp("/repo/src"),
                    &policy,
                ),
                expected,
                "{name}"
            );
        }

        let equal_weight = session(
            Some("ws-a"),
            vec![workspace(
                "ws-a",
                Some("/repo"),
                Some("tab-a"),
                vec![tab(
                    "tab-a",
                    Some("shell"),
                    vec![
                        pane("blocked", Some("/repo/src"), blocked_agent()),
                        pane("shell", Some("/repo/src"), idle_shell()),
                    ],
                )],
            )],
        );
        assert_eq!(
            decide_with_policy(
                &caller_view,
                &equal_weight,
                &cp("/repo/src"),
                &AgentIdlePolicy::from_statuses([AgentStatus::Blocked]),
            ),
            focus("shell"),
            "focused-pane ranking must outrank occupant type"
        );

        assert_eq!(
            decide_with_policy(
                &caller("/repo/src", "ws-a", "tab-a", "pane-a"),
                &session_with(working_agent()),
                &cp("/repo/src"),
                &AgentIdlePolicy::from_statuses([]),
            ),
            Action::NoOp
        );
    }
}
