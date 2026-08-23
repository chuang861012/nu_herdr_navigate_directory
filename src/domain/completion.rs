//! Pure completion evidence, prefix matching, and provenance-safe descriptions.

use super::path::CanonicalPath;
use super::types::{AgentStatus, Occupant, Session, TabId, WorkspaceId};

/// Merged candidate set may not exceed this count after validation and dedup.
pub(crate) const CANDIDATE_CEILING: usize = 1_000;

/// Supporting evidence for one canonical directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Evidence {
    WorkspaceRoot {
        workspace_id: WorkspaceId,
        label: String,
        number: usize,
        is_current: bool,
    },
    Pane {
        workspace_id: WorkspaceId,
        tab_id: TabId,
        occupant: OccupantKind,
        label: String,
        number: usize,
        is_current_workspace: bool,
        is_current_tab: bool,
    },
    Filesystem,
}

/// Pane occupant as observed from the snapshot, without process inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OccupantKind {
    Shell,
    Agent(AgentStatus),
}

/// Strongest source shown in the first description segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceLabel {
    AgentIdle,
    AgentDone,
    AgentWorking,
    AgentBlocked,
    AgentUnknown,
    Workspace,
    ShellPane,
    Directory,
}

/// Closest truthful scope coupled to the winning evidence record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScopeLabel {
    None,
    CurrentTab,
    CurrentWorkspace,
    Workspace { label: String, number: usize },
    MultipleWorkspaces { count: usize },
}

/// Provenance-safe description data before terminal escaping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DescriptionData {
    pub source: SourceLabel,
    pub scope: ScopeLabel,
    pub pane_count: usize,
}

/// One deduplicated completion candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompletionCandidate {
    pub path: CanonicalPath,
    pub description: DescriptionData,
}

/// Hard physical prefix used for non-empty completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrefixBound {
    pub base: CanonicalPath,
    pub remaining: String,
}

/// Collect workspace-root and pane-foreground evidence from a typed session.
pub(crate) fn session_evidence(
    session: &Session,
    current_workspace: &WorkspaceId,
    current_tab: &TabId,
) -> Vec<(CanonicalPath, Evidence)> {
    let mut evidence = Vec::new();
    for workspace in &session.workspaces {
        let is_current = &workspace.id == current_workspace;
        if let Some(root) = &workspace.root {
            evidence.push((
                root.clone(),
                Evidence::WorkspaceRoot {
                    workspace_id: workspace.id.clone(),
                    label: workspace.label.clone(),
                    number: workspace.number,
                    is_current,
                },
            ));
        }
        for tab in &workspace.tabs {
            let is_current_tab = is_current && &tab.id == current_tab;
            for pane in &tab.panes {
                let Some(path) = &pane.foreground_cwd else {
                    continue;
                };
                evidence.push((
                    path.clone(),
                    Evidence::Pane {
                        workspace_id: workspace.id.clone(),
                        tab_id: tab.id.clone(),
                        occupant: occupant_kind(&pane.occupant),
                        label: workspace.label.clone(),
                        number: workspace.number,
                        is_current_workspace: is_current,
                        is_current_tab,
                    },
                ));
            }
        }
    }
    evidence
}

/// Keep a Herdr semantic path that passes prefix, hidden, and caller-cwd rules.
pub(crate) fn semantic_path_allowed(
    path: &CanonicalPath,
    caller_cwd: &CanonicalPath,
    bound: Option<&PrefixBound>,
) -> bool {
    if path == caller_cwd {
        return false;
    }
    match bound {
        None => !has_untyped_hidden_component(path),
        Some(bound) => matches_bound(path, bound, false),
    }
}

/// Keep a filesystem child that is a direct child of the prefix base.
pub(crate) fn filesystem_path_allowed(
    path: &CanonicalPath,
    caller_cwd: &CanonicalPath,
    bound: &PrefixBound,
) -> bool {
    path != caller_cwd && matches_bound(path, bound, true)
}

/// Merge semantic and filesystem evidence. `None` means native fallback.
pub(crate) fn merge_candidates(
    semantic: impl IntoIterator<Item = (CanonicalPath, Evidence)>,
    filesystem: impl IntoIterator<Item = CanonicalPath>,
    caller_cwd: &CanonicalPath,
) -> Option<Vec<CompletionCandidate>> {
    let mut groups: Vec<(CanonicalPath, Vec<Evidence>)> = Vec::new();
    let mut semantic_count = 0usize;
    for (path, evidence) in semantic {
        if &path == caller_cwd {
            continue;
        }
        if insert_evidence(&mut groups, path, evidence) {
            semantic_count += 1;
        }
    }
    if semantic_count == 0 {
        return None;
    }
    for path in filesystem {
        if &path == caller_cwd {
            continue;
        }
        insert_evidence(&mut groups, path, Evidence::Filesystem);
    }
    if groups.len() > CANDIDATE_CEILING {
        return None;
    }
    Some(
        groups
            .into_iter()
            .map(|(path, evidence)| CompletionCandidate {
                description: describe(&evidence),
                path,
            })
            .collect(),
    )
}

pub(crate) fn is_hidden_component(name: &str) -> bool {
    name.starts_with('.') && name != "." && name != ".."
}

fn occupant_kind(occupant: &Occupant) -> OccupantKind {
    match occupant {
        Occupant::Shell(_) => OccupantKind::Shell,
        Occupant::Agent(status) => OccupantKind::Agent(*status),
    }
}

fn has_untyped_hidden_component(path: &CanonicalPath) -> bool {
    path.as_path().components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(is_hidden_component)
    })
}

fn matches_bound(path: &CanonicalPath, bound: &PrefixBound, filesystem: bool) -> bool {
    let Some(suffix) = path.relative_components(&bound.base) else {
        return false;
    };
    if filesystem {
        if suffix.len() != 1 {
            return false;
        }
    } else if suffix.is_empty() {
        return bound.remaining.is_empty();
    }
    let Some(first) = suffix.first() else {
        return true;
    };
    if is_hidden_component(first) && !bound.remaining.starts_with('.') {
        return false;
    }
    if !first.starts_with(&bound.remaining) {
        return false;
    }
    suffix[1..]
        .iter()
        .all(|component| !is_hidden_component(component))
}

fn insert_evidence(
    groups: &mut Vec<(CanonicalPath, Vec<Evidence>)>,
    path: CanonicalPath,
    evidence: Evidence,
) -> bool {
    if let Some((_, existing)) = groups.iter_mut().find(|(known, _)| known == &path) {
        existing.push(evidence);
        false
    } else {
        groups.push((path, vec![evidence]));
        true
    }
}

fn describe(evidence: &[Evidence]) -> DescriptionData {
    let pane_count = evidence
        .iter()
        .filter(|item| matches!(item, Evidence::Pane { .. }))
        .count();
    let Some(winner) = evidence
        .iter()
        .min_by_key(|item| (strength(item), scope_rank(item)))
    else {
        return DescriptionData {
            source: SourceLabel::Directory,
            scope: ScopeLabel::None,
            pane_count: 0,
        };
    };
    match winner {
        Evidence::Filesystem => DescriptionData {
            source: SourceLabel::Directory,
            scope: ScopeLabel::None,
            pane_count,
        },
        Evidence::WorkspaceRoot { .. } => DescriptionData {
            source: SourceLabel::Workspace,
            scope: workspace_scope(evidence),
            pane_count,
        },
        Evidence::Pane {
            occupant,
            label,
            number,
            is_current_workspace,
            is_current_tab,
            ..
        } => DescriptionData {
            source: pane_source(*occupant),
            scope: pane_scope(*is_current_tab, *is_current_workspace, label, *number),
            pane_count,
        },
    }
}

fn strength(evidence: &Evidence) -> u8 {
    match evidence {
        Evidence::Pane {
            occupant: OccupantKind::Agent(AgentStatus::Idle | AgentStatus::Done),
            ..
        } => 0,
        Evidence::WorkspaceRoot { .. } => 1,
        Evidence::Pane { .. } => 2,
        Evidence::Filesystem => 3,
    }
}

fn scope_rank(evidence: &Evidence) -> u8 {
    match evidence {
        Evidence::Pane {
            is_current_tab: true,
            ..
        } => 0,
        Evidence::Pane {
            is_current_workspace: true,
            ..
        }
        | Evidence::WorkspaceRoot {
            is_current: true, ..
        } => 1,
        _ => 2,
    }
}

fn pane_source(occupant: OccupantKind) -> SourceLabel {
    match occupant {
        OccupantKind::Shell => SourceLabel::ShellPane,
        OccupantKind::Agent(AgentStatus::Idle) => SourceLabel::AgentIdle,
        OccupantKind::Agent(AgentStatus::Done) => SourceLabel::AgentDone,
        OccupantKind::Agent(AgentStatus::Working) => SourceLabel::AgentWorking,
        OccupantKind::Agent(AgentStatus::Blocked) => SourceLabel::AgentBlocked,
        OccupantKind::Agent(AgentStatus::Unknown) => SourceLabel::AgentUnknown,
    }
}

fn pane_scope(
    is_current_tab: bool,
    is_current_workspace: bool,
    label: &str,
    number: usize,
) -> ScopeLabel {
    if is_current_tab {
        ScopeLabel::CurrentTab
    } else if is_current_workspace {
        ScopeLabel::CurrentWorkspace
    } else {
        ScopeLabel::Workspace {
            label: label.to_string(),
            number,
        }
    }
}

fn workspace_scope(evidence: &[Evidence]) -> ScopeLabel {
    let mut unique: Vec<(WorkspaceId, String, usize, bool)> = Vec::new();
    for item in evidence {
        let (id, label, number, is_current) = match item {
            Evidence::WorkspaceRoot {
                workspace_id,
                label,
                number,
                is_current,
            } => (workspace_id.clone(), label.clone(), *number, *is_current),
            Evidence::Pane {
                workspace_id,
                label,
                number,
                is_current_workspace,
                ..
            } => (
                workspace_id.clone(),
                label.clone(),
                *number,
                *is_current_workspace,
            ),
            Evidence::Filesystem => continue,
        };
        if let Some(existing) = unique.iter_mut().find(|(known, _, _, _)| known == &id) {
            existing.3 |= is_current;
        } else {
            unique.push((id, label, number, is_current));
        }
    }
    if unique.iter().any(|(_, _, _, is_current)| *is_current) {
        ScopeLabel::CurrentWorkspace
    } else if unique.len() == 1 {
        let (_, label, number, _) = unique.remove(0);
        ScopeLabel::Workspace { label, number }
    } else if unique.len() > 1 {
        ScopeLabel::MultipleWorkspaces {
            count: unique.len(),
        }
    } else {
        ScopeLabel::None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CANDIDATE_CEILING, CompletionCandidate, DescriptionData, Evidence, OccupantKind,
        PrefixBound, ScopeLabel, SourceLabel, filesystem_path_allowed, merge_candidates,
        semantic_path_allowed, session_evidence,
    };
    use crate::domain::path::CanonicalPath;
    use crate::domain::types::{
        AgentStatus, Occupant, Pane, PaneId, Session, Tab, TabId, Workspace, WorkspaceId,
    };

    fn cp(path: &str) -> CanonicalPath {
        CanonicalPath::from_parts_for_test(path)
    }

    fn bound(base: &str, remaining: &str) -> PrefixBound {
        PrefixBound {
            base: cp(base),
            remaining: remaining.to_string(),
        }
    }

    fn idle_agent() -> OccupantKind {
        OccupantKind::Agent(AgentStatus::Idle)
    }

    fn workspace(
        id: &str,
        label: &str,
        number: usize,
        root: Option<&str>,
        tabs: Vec<Tab>,
    ) -> Workspace {
        Workspace {
            id: WorkspaceId::new(id),
            root: root.map(cp),
            focused_tab_id: tabs.first().map(|tab| tab.id.clone()),
            tabs,
            label: label.to_string(),
            number,
        }
    }

    fn tab(id: &str, panes: Vec<Pane>) -> Tab {
        Tab {
            id: TabId::new(id),
            focused_pane_id: panes.first().map(|pane| pane.id.clone()),
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

    fn candidate_at<'a>(
        candidates: &'a [CompletionCandidate],
        path: &str,
    ) -> &'a CompletionCandidate {
        candidates
            .iter()
            .find(|candidate| candidate.path.as_str() == path)
            .unwrap_or_else(|| panic!("missing candidate {path}"))
    }

    #[test]
    fn aggregates_workspace_pane_and_filesystem_evidence() {
        let semantic = vec![
            (
                cp("/repo"),
                Evidence::WorkspaceRoot {
                    workspace_id: WorkspaceId::new("w1"),
                    label: "repo".into(),
                    number: 1,
                    is_current: true,
                },
            ),
            (
                cp("/repo"),
                Evidence::Pane {
                    workspace_id: WorkspaceId::new("w1"),
                    tab_id: TabId::new("t1"),
                    occupant: OccupantKind::Shell,
                    label: "repo".into(),
                    number: 1,
                    is_current_workspace: true,
                    is_current_tab: true,
                },
            ),
        ];
        let merged = merge_candidates(semantic, [cp("/repo")], &cp("/cwd")).unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].description,
            DescriptionData {
                source: SourceLabel::Workspace,
                scope: ScopeLabel::CurrentWorkspace,
                pane_count: 1,
            }
        );
    }

    #[test]
    fn canonical_dedup_keeps_exact_pane_counts() {
        let semantic = vec![
            (
                cp("/repo/src"),
                Evidence::Pane {
                    workspace_id: WorkspaceId::new("w1"),
                    tab_id: TabId::new("t1"),
                    occupant: idle_agent(),
                    label: "repo".into(),
                    number: 1,
                    is_current_workspace: true,
                    is_current_tab: true,
                },
            ),
            (
                cp("/repo/src"),
                Evidence::Pane {
                    workspace_id: WorkspaceId::new("w1"),
                    tab_id: TabId::new("t2"),
                    occupant: OccupantKind::Shell,
                    label: "repo".into(),
                    number: 1,
                    is_current_workspace: true,
                    is_current_tab: false,
                },
            ),
        ];
        let merged = merge_candidates(semantic, [], &cp("/cwd")).unwrap();
        assert_eq!(merged[0].description.pane_count, 2);
        assert_eq!(merged[0].description.source, SourceLabel::AgentIdle);
        assert_eq!(merged[0].description.scope, ScopeLabel::CurrentTab);
    }

    #[test]
    fn provenance_does_not_mix_idle_status_with_another_pane_scope() {
        let semantic = vec![
            (
                cp("/repo"),
                Evidence::Pane {
                    workspace_id: WorkspaceId::new("w-other"),
                    tab_id: TabId::new("t-other"),
                    occupant: idle_agent(),
                    label: "other".into(),
                    number: 2,
                    is_current_workspace: false,
                    is_current_tab: false,
                },
            ),
            (
                cp("/repo"),
                Evidence::Pane {
                    workspace_id: WorkspaceId::new("w1"),
                    tab_id: TabId::new("t1"),
                    occupant: OccupantKind::Shell,
                    label: "repo".into(),
                    number: 1,
                    is_current_workspace: true,
                    is_current_tab: true,
                },
            ),
        ];
        let description = &merge_candidates(semantic, [], &cp("/cwd")).unwrap()[0].description;
        assert_eq!(description.source, SourceLabel::AgentIdle);
        assert_eq!(
            description.scope,
            ScopeLabel::Workspace {
                label: "other".into(),
                number: 2,
            }
        );
        assert_eq!(description.pane_count, 2);
    }

    #[test]
    fn prefers_current_tab_then_current_workspace_for_equal_idle_agents() {
        let semantic = vec![
            (
                cp("/repo"),
                Evidence::Pane {
                    workspace_id: WorkspaceId::new("w2"),
                    tab_id: TabId::new("t-other"),
                    occupant: OccupantKind::Agent(AgentStatus::Done),
                    label: "other".into(),
                    number: 2,
                    is_current_workspace: false,
                    is_current_tab: false,
                },
            ),
            (
                cp("/repo"),
                Evidence::Pane {
                    workspace_id: WorkspaceId::new("w1"),
                    tab_id: TabId::new("t1"),
                    occupant: idle_agent(),
                    label: "repo".into(),
                    number: 1,
                    is_current_workspace: true,
                    is_current_tab: true,
                },
            ),
        ];
        let description = &merge_candidates(semantic, [], &cp("/cwd")).unwrap()[0].description;
        assert_eq!(description.source, SourceLabel::AgentIdle);
        assert_eq!(description.scope, ScopeLabel::CurrentTab);
    }

    #[test]
    fn multi_workspace_paths_use_current_label_or_count() {
        let current_and_other = vec![
            (
                cp("/shared"),
                Evidence::WorkspaceRoot {
                    workspace_id: WorkspaceId::new("w1"),
                    label: "one".into(),
                    number: 1,
                    is_current: true,
                },
            ),
            (
                cp("/shared"),
                Evidence::WorkspaceRoot {
                    workspace_id: WorkspaceId::new("w2"),
                    label: "two".into(),
                    number: 2,
                    is_current: false,
                },
            ),
        ];
        assert_eq!(
            merge_candidates(current_and_other, [], &cp("/cwd")).unwrap()[0]
                .description
                .scope,
            ScopeLabel::CurrentWorkspace
        );

        let two_others = vec![
            (
                cp("/shared"),
                Evidence::WorkspaceRoot {
                    workspace_id: WorkspaceId::new("w2"),
                    label: "two".into(),
                    number: 2,
                    is_current: false,
                },
            ),
            (
                cp("/shared"),
                Evidence::Pane {
                    workspace_id: WorkspaceId::new("w3"),
                    tab_id: TabId::new("t3"),
                    occupant: OccupantKind::Shell,
                    label: "three".into(),
                    number: 3,
                    is_current_workspace: false,
                    is_current_tab: false,
                },
            ),
        ];
        assert_eq!(
            merge_candidates(two_others, [], &cp("/cwd")).unwrap()[0]
                .description
                .scope,
            ScopeLabel::MultipleWorkspaces { count: 2 }
        );
    }

    #[test]
    fn caller_cwd_is_excluded_from_every_source() {
        let semantic = vec![
            (
                cp("/cwd"),
                Evidence::WorkspaceRoot {
                    workspace_id: WorkspaceId::new("w1"),
                    label: "cwd".into(),
                    number: 1,
                    is_current: true,
                },
            ),
            (
                cp("/cwd"),
                Evidence::Pane {
                    workspace_id: WorkspaceId::new("w1"),
                    tab_id: TabId::new("t1"),
                    occupant: idle_agent(),
                    label: "cwd".into(),
                    number: 1,
                    is_current_workspace: true,
                    is_current_tab: true,
                },
            ),
            (
                cp("/other"),
                Evidence::Pane {
                    workspace_id: WorkspaceId::new("w1"),
                    tab_id: TabId::new("t1"),
                    occupant: OccupantKind::Shell,
                    label: "cwd".into(),
                    number: 1,
                    is_current_workspace: true,
                    is_current_tab: true,
                },
            ),
        ];
        let merged = merge_candidates(semantic, [cp("/cwd"), cp("/fs")], &cp("/cwd")).unwrap();
        let paths: Vec<_> = merged
            .iter()
            .map(|candidate| candidate.path.as_str())
            .collect();
        assert!(paths.contains(&"/other"));
        assert!(paths.contains(&"/fs"));
        assert!(!paths.contains(&"/cwd"));
    }

    #[test]
    fn idle_and_done_outrank_workspace_and_other_panes() {
        let cases = [
            (AgentStatus::Idle, SourceLabel::AgentIdle),
            (AgentStatus::Done, SourceLabel::AgentDone),
        ];
        for (status, source) in cases {
            let semantic = vec![
                (
                    cp("/repo"),
                    Evidence::WorkspaceRoot {
                        workspace_id: WorkspaceId::new("w1"),
                        label: "repo".into(),
                        number: 1,
                        is_current: true,
                    },
                ),
                (
                    cp("/repo"),
                    Evidence::Pane {
                        workspace_id: WorkspaceId::new("w1"),
                        tab_id: TabId::new("t1"),
                        occupant: OccupantKind::Agent(status),
                        label: "repo".into(),
                        number: 1,
                        is_current_workspace: true,
                        is_current_tab: false,
                    },
                ),
            ];
            assert_eq!(
                merge_candidates(semantic, [], &cp("/cwd")).unwrap()[0]
                    .description
                    .source,
                source
            );
        }
    }

    #[test]
    fn non_reusable_agent_and_shell_labels_are_truthful() {
        let cases = [
            (
                OccupantKind::Agent(AgentStatus::Working),
                SourceLabel::AgentWorking,
            ),
            (
                OccupantKind::Agent(AgentStatus::Blocked),
                SourceLabel::AgentBlocked,
            ),
            (
                OccupantKind::Agent(AgentStatus::Unknown),
                SourceLabel::AgentUnknown,
            ),
            (OccupantKind::Shell, SourceLabel::ShellPane),
        ];
        for (occupant, source) in cases {
            let semantic = [(
                cp("/repo"),
                Evidence::Pane {
                    workspace_id: WorkspaceId::new("w1"),
                    tab_id: TabId::new("t1"),
                    occupant,
                    label: "repo".into(),
                    number: 1,
                    is_current_workspace: true,
                    is_current_tab: false,
                },
            )];
            let description = &merge_candidates(semantic, [], &cp("/cwd")).unwrap()[0].description;
            assert_eq!(description.source, source);
            assert_eq!(description.scope, ScopeLabel::CurrentWorkspace);
        }
    }

    #[test]
    fn empty_discovery_keeps_session_paths_and_hides_untyped_dot_components() {
        let caller = cp("/home/me/downloads");
        assert!(semantic_path_allowed(&cp("/home/me/src"), &caller, None));
        assert!(!semantic_path_allowed(
            &cp("/home/me/.config"),
            &caller,
            None
        ));
        assert!(!semantic_path_allowed(
            &cp("/home/me/src/.git"),
            &caller,
            None
        ));
        assert!(!semantic_path_allowed(&caller, &caller, None));
    }

    #[test]
    fn non_empty_prefix_is_physical_and_case_sensitive() {
        let prefix = bound("/home/me/src", "hc");
        let caller = cp("/home/me/downloads");
        assert!(semantic_path_allowed(
            &cp("/home/me/src/hc-v2"),
            &caller,
            Some(&prefix)
        ));
        assert!(semantic_path_allowed(
            &cp("/home/me/src/hc-v2/crates/api"),
            &caller,
            Some(&prefix)
        ));
        assert!(!semantic_path_allowed(
            &cp("/home/me/src/HC-v2"),
            &caller,
            Some(&prefix)
        ));
        assert!(!semantic_path_allowed(
            &cp("/home/me/other"),
            &caller,
            Some(&prefix)
        ));
        assert!(!filesystem_path_allowed(
            &cp("/home/me/src/hc-v2/crates"),
            &caller,
            &prefix
        ));
        assert!(filesystem_path_allowed(
            &cp("/home/me/src/hc-v2"),
            &caller,
            &prefix
        ));
    }

    #[test]
    fn hidden_components_need_an_explicit_dot_prefix() {
        let caller = cp("/home/me");
        let without_dot = bound("/home/me/src", "");
        let with_dot = bound("/home/me/src", ".g");
        assert!(!semantic_path_allowed(
            &cp("/home/me/src/.git"),
            &caller,
            Some(&without_dot)
        ));
        assert!(semantic_path_allowed(
            &cp("/home/me/src/.git"),
            &caller,
            Some(&with_dot)
        ));
        assert!(!semantic_path_allowed(
            &cp("/home/me/src/lib/.hidden"),
            &caller,
            Some(&without_dot)
        ));
        let typed_hidden_parent = bound("/home/me/src/.hidden", "a");
        assert!(semantic_path_allowed(
            &cp("/home/me/src/.hidden/api"),
            &caller,
            Some(&typed_hidden_parent)
        ));
    }

    #[test]
    fn prefix_directory_itself_matches_only_with_empty_remaining() {
        let caller = cp("/home/me");
        assert!(semantic_path_allowed(
            &cp("/home/me/src"),
            &caller,
            Some(&bound("/home/me/src", ""))
        ));
        assert!(!semantic_path_allowed(
            &cp("/home/me/src"),
            &caller,
            Some(&bound("/home/me/src", "h"))
        ));
        assert!(!filesystem_path_allowed(
            &cp("/home/me/src"),
            &caller,
            &bound("/home/me/src", "")
        ));
    }

    #[test]
    fn zero_semantic_candidates_fall_back_even_with_filesystem_paths() {
        assert!(merge_candidates([], [cp("/tmp/dir")], &cp("/cwd")).is_none());
    }

    #[test]
    fn ceiling_rejects_the_whole_set() {
        let semantic = (0..=CANDIDATE_CEILING).map(|index| {
            (
                cp(&format!("/repo/{index}")),
                Evidence::WorkspaceRoot {
                    workspace_id: WorkspaceId::new(format!("w{index}")),
                    label: format!("w{index}"),
                    number: index + 1,
                    is_current: false,
                },
            )
        });
        assert!(merge_candidates(semantic, [], &cp("/cwd")).is_none());
    }

    #[test]
    fn session_evidence_includes_out_of_root_panes_and_skips_invalid_cwd() {
        let session = Session {
            focused_workspace_id: Some(WorkspaceId::new("w1")),
            workspaces: vec![workspace(
                "w1",
                "repo",
                1,
                Some("/repo"),
                vec![tab(
                    "t1",
                    vec![
                        pane("p1", Some("/repo"), Occupant::Shell(None)),
                        pane(
                            "p2",
                            Some("/outside"),
                            Occupant::Agent(AgentStatus::Working),
                        ),
                        pane("p3", None, Occupant::Agent(AgentStatus::Idle)),
                    ],
                )],
            )],
        };
        let evidence = session_evidence(&session, &WorkspaceId::new("w1"), &TabId::new("t1"));
        let paths: Vec<_> = evidence.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(paths, ["/repo", "/repo", "/outside"]);
        assert!(matches!(
            evidence[2].1,
            Evidence::Pane {
                occupant: OccupantKind::Agent(AgentStatus::Working),
                is_current_tab: true,
                ..
            }
        ));
    }

    #[test]
    fn filesystem_only_description_is_directory() {
        let semantic = [(
            cp("/tmp/dir"),
            Evidence::Pane {
                workspace_id: WorkspaceId::new("w1"),
                tab_id: TabId::new("t1"),
                occupant: OccupantKind::Shell,
                label: "tmp".into(),
                number: 1,
                is_current_workspace: false,
                is_current_tab: false,
            },
        )];
        let merged = merge_candidates(semantic, [cp("/tmp/other")], &cp("/cwd")).unwrap();
        let other = candidate_at(&merged, "/tmp/other");
        assert_eq!(other.description.source, SourceLabel::Directory);
        assert_eq!(other.description.scope, ScopeLabel::None);
    }
}
