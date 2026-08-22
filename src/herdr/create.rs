//! Focused tab and workspace creation through the injected Herdr CLI.

use std::time::Duration;

use super::cli::{self, CREATE_TIMEOUT, RunError, START_FAILED};
use super::context::InsideContext;
use super::protocol::{self, CommandResult, CreatedTabBody, CreatedWorkspaceBody};
use crate::domain::{CanonicalPath, Error, ErrorKind, PaneId, TabId, WorkspaceId};

/// Identities returned by a successful focused tab create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreatedTab {
    pub tab_id: TabId,
    pub pane_id: PaneId,
}

/// Identities returned by a successful focused workspace create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreatedWorkspace {
    pub workspace_id: WorkspaceId,
    pub tab_id: TabId,
    pub pane_id: PaneId,
}

/// Create a tab in an existing workspace at the canonical target and focus it.
pub(crate) fn create_tab(
    context: &InsideContext,
    workspace_id: &WorkspaceId,
    cwd: &CanonicalPath,
    interrupted: impl Fn() -> bool,
) -> Result<CommandResult<CreatedTab>, RunError> {
    create_tab_with_timeout(context, workspace_id, cwd, interrupted, CREATE_TIMEOUT)
}

fn create_tab_with_timeout(
    context: &InsideContext,
    workspace_id: &WorkspaceId,
    cwd: &CanonicalPath,
    interrupted: impl Fn() -> bool,
    timeout: Duration,
) -> Result<CommandResult<CreatedTab>, RunError> {
    let output = match cli::run(
        context,
        &[
            "tab",
            "create",
            "--workspace",
            workspace_id.as_str(),
            "--cwd",
            cwd.as_str(),
            "--focus",
        ],
        timeout,
        interrupted,
    ) {
        Ok(output) => output,
        Err(error) => return Err(ambiguous_after_dispatch("tab creation", error)),
    };

    match protocol::parse_tab_created(&output) {
        Ok(CommandResult::Ok(body)) => match validate_tab(body, workspace_id.as_str()) {
            Ok(created) => Ok(CommandResult::Ok(created)),
            Err(error) => Err(ambiguous_after_dispatch(
                "tab creation",
                RunError::Failed(error),
            )),
        },
        Ok(CommandResult::NotFound { code, message }) => {
            Ok(CommandResult::NotFound { code, message })
        }
        Err(error) => Err(keep_or_mark_partial("tab creation", error)),
    }
}

/// Create a workspace at the canonical target, with its first tab focused.
pub(crate) fn create_workspace(
    context: &InsideContext,
    cwd: &CanonicalPath,
    interrupted: impl Fn() -> bool,
) -> Result<CommandResult<CreatedWorkspace>, RunError> {
    let output = match cli::run(
        context,
        &["workspace", "create", "--cwd", cwd.as_str(), "--focus"],
        CREATE_TIMEOUT,
        interrupted,
    ) {
        Ok(output) => output,
        Err(error) => return Err(ambiguous_after_dispatch("workspace creation", error)),
    };

    match protocol::parse_workspace_created(&output) {
        Ok(CommandResult::Ok(body)) => match validate_workspace(body) {
            Ok(created) => Ok(CommandResult::Ok(created)),
            Err(error) => Err(ambiguous_after_dispatch(
                "workspace creation",
                RunError::Failed(error),
            )),
        },
        Ok(CommandResult::NotFound { code, message }) => {
            Ok(CommandResult::NotFound { code, message })
        }
        Err(error) => Err(keep_or_mark_partial("workspace creation", error)),
    }
}

fn validate_tab(body: CreatedTabBody, workspace_id: &str) -> Result<CreatedTab, Error> {
    require_id("tab creation", "tab", &body.tab.tab_id)?;
    require_id("tab creation", "tab workspace", &body.tab.workspace_id)?;
    require_id("tab creation", "root pane", &body.root_pane.pane_id)?;
    require_id(
        "tab creation",
        "root pane workspace",
        &body.root_pane.workspace_id,
    )?;
    require_id("tab creation", "root pane tab", &body.root_pane.tab_id)?;
    if body.tab.workspace_id != workspace_id {
        return Err(Error::herdr_protocol(
            "tab creation: created tab workspace does not match the request",
        ));
    }
    if body.root_pane.workspace_id != workspace_id {
        return Err(Error::herdr_protocol(
            "tab creation: created root pane workspace does not match the request",
        ));
    }
    if body.root_pane.tab_id != body.tab.tab_id {
        return Err(Error::herdr_protocol(
            "tab creation: created root pane tab does not match the created tab",
        ));
    }
    Ok(CreatedTab {
        tab_id: TabId::new(body.tab.tab_id),
        pane_id: PaneId::new(body.root_pane.pane_id),
    })
}

fn validate_workspace(body: CreatedWorkspaceBody) -> Result<CreatedWorkspace, Error> {
    require_id(
        "workspace creation",
        "workspace",
        &body.workspace.workspace_id,
    )?;
    require_id("workspace creation", "tab", &body.tab.tab_id)?;
    require_id(
        "workspace creation",
        "tab workspace",
        &body.tab.workspace_id,
    )?;
    require_id("workspace creation", "root pane", &body.root_pane.pane_id)?;
    require_id(
        "workspace creation",
        "root pane workspace",
        &body.root_pane.workspace_id,
    )?;
    require_id(
        "workspace creation",
        "root pane tab",
        &body.root_pane.tab_id,
    )?;
    if body.tab.workspace_id != body.workspace.workspace_id {
        return Err(Error::herdr_protocol(
            "workspace creation: created tab workspace does not match the workspace",
        ));
    }
    if body.root_pane.workspace_id != body.workspace.workspace_id {
        return Err(Error::herdr_protocol(
            "workspace creation: created root pane workspace does not match the workspace",
        ));
    }
    if body.root_pane.tab_id != body.tab.tab_id {
        return Err(Error::herdr_protocol(
            "workspace creation: created root pane tab does not match the created tab",
        ));
    }
    Ok(CreatedWorkspace {
        workspace_id: WorkspaceId::new(body.workspace.workspace_id),
        tab_id: TabId::new(body.tab.tab_id),
        pane_id: PaneId::new(body.root_pane.pane_id),
    })
}

fn require_id(operation: &str, field: &str, id: &str) -> Result<(), Error> {
    if id.is_empty() {
        Err(Error::herdr_protocol(format!(
            "{operation}: missing created {field} id"
        )))
    } else {
        Ok(())
    }
}

fn keep_or_mark_partial(operation: &str, error: RunError) -> RunError {
    match &error {
        RunError::Failed(failed) if failed.kind() == ErrorKind::HerdrAction => error,
        _ => ambiguous_after_dispatch(operation, error),
    }
}

fn ambiguous_after_dispatch(operation: &str, error: RunError) -> RunError {
    match error {
        RunError::Interrupted => RunError::Interrupted,
        RunError::Failed(failed) if is_undispatched_start_failure(&failed) => {
            RunError::Failed(failed)
        }
        RunError::Failed(failed) => RunError::Failed(Error::new(
            failed.kind(),
            format!(
                "{}; {operation} may have partially completed and was not rolled back",
                failed.message()
            ),
        )),
    }
}

fn is_undispatched_start_failure(error: &Error) -> bool {
    error.kind() == ErrorKind::HerdrTransport && error.message() == START_FAILED
}

#[cfg(test)]
mod tests {
    use super::{create_tab, create_tab_with_timeout, create_workspace};
    use crate::domain::{CanonicalPath, ErrorKind, WorkspaceId};
    use crate::herdr::cli::{CREATE_TIMEOUT, RunError};
    use crate::herdr::context::inside_context;
    use crate::herdr::protocol::CommandResult;
    use crate::herdr::test_support::{TempDir, lock_cli, write_executable};
    use std::collections::BTreeMap;
    use std::fs;
    use std::time::{Duration, Instant};

    fn cwd_fixture() -> (TempDir, CanonicalPath) {
        let dir = TempDir::new("create-cwd");
        let cwd = CanonicalPath::directory(dir.path()).unwrap();
        (dir, cwd)
    }

    fn context_for(bin: &std::path::Path) -> crate::herdr::context::InsideContext {
        inside_context(
            bin.to_str().unwrap(),
            "/tmp/nu-plugin-herdr-cd.sock",
            "w1",
            "w1:t1",
            "w1:p1",
            BTreeMap::new(),
        )
        .unwrap()
    }

    fn sh_single(path: &str) -> String {
        format!("'{}'", path.replace('\'', r#"'"'"'"#))
    }

    fn tab_json() -> &'static str {
        r#"{"id":"cli:tab:create","result":{"type":"tab_created","tab":{"tab_id":"w1:t2","workspace_id":"w1","number":2,"label":"src","focused":true,"pane_count":1,"agent_status":"idle"},"root_pane":{"pane_id":"w1:p3","terminal_id":"t","workspace_id":"w1","tab_id":"w1:t2","focused":true,"agent_status":"idle","revision":1}}}"#
    }

    fn workspace_json() -> &'static str {
        r#"{"id":"cli:workspace:create","result":{"type":"workspace_created","workspace":{"workspace_id":"w2","number":2,"label":"other","focused":true,"pane_count":1,"tab_count":1,"active_tab_id":"w2:t1","agent_status":"idle"},"tab":{"tab_id":"w2:t1","workspace_id":"w2","number":1,"label":"main","focused":true,"pane_count":1,"agent_status":"idle"},"root_pane":{"pane_id":"w2:p1","terminal_id":"t","workspace_id":"w2","tab_id":"w2:t1","focused":true,"agent_status":"idle","revision":1}}}"#
    }

    #[test]
    fn creation_timeout_is_five_seconds() {
        assert_eq!(CREATE_TIMEOUT, Duration::from_secs(5));
    }

    #[test]
    fn tab_create_uses_exact_argv_focus_and_omits_labels() {
        let _cli = lock_cli();
        let (_cwd_dir, cwd) = cwd_fixture();
        let dir = TempDir::new("tab-argv");
        let record = dir.path().join("record");
        let bin = write_executable(
            dir.path(),
            "herdr",
            &format!(
                "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$*\" > {record}\nprintf '%s\\n' {json}\n",
                record = sh_single(&record.display().to_string()),
                json = sh_single(tab_json()),
            ),
        );
        let context = context_for(&bin);
        let CommandResult::Ok(created) =
            create_tab(&context, &WorkspaceId::new("w1"), &cwd, || false).unwrap()
        else {
            panic!("expected created tab");
        };
        assert_eq!(created.tab_id.as_str(), "w1:t2");
        assert_eq!(created.pane_id.as_str(), "w1:p3");
        let argv = fs::read_to_string(&record).unwrap();
        assert_eq!(
            argv.trim(),
            format!("tab create --workspace w1 --cwd {} --focus", cwd.as_str())
        );
        assert!(!argv.contains("--label"));
        assert!(!argv.contains("close"));
        assert!(!argv.contains("send-"));
    }

    #[test]
    fn workspace_create_uses_exact_argv_focus_and_omits_labels() {
        let _cli = lock_cli();
        let (_cwd_dir, cwd) = cwd_fixture();
        let dir = TempDir::new("ws-argv");
        let record = dir.path().join("record");
        let bin = write_executable(
            dir.path(),
            "herdr",
            &format!(
                "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$*\" > {record}\nprintf '%s\\n' {json}\n",
                record = sh_single(&record.display().to_string()),
                json = sh_single(workspace_json()),
            ),
        );
        let context = context_for(&bin);
        let CommandResult::Ok(created) = create_workspace(&context, &cwd, || false).unwrap() else {
            panic!("expected created workspace");
        };
        assert_eq!(created.workspace_id.as_str(), "w2");
        assert_eq!(created.tab_id.as_str(), "w2:t1");
        assert_eq!(created.pane_id.as_str(), "w2:p1");
        let argv = fs::read_to_string(&record).unwrap();
        assert_eq!(
            argv.trim(),
            format!("workspace create --cwd {} --focus", cwd.as_str())
        );
        assert!(!argv.contains("--label"));
        assert!(!argv.contains("close"));
    }

    #[test]
    fn timeout_and_invalid_success_are_ambiguous_and_never_rolled_back() {
        let _cli = lock_cli();
        let (_cwd_dir, cwd) = cwd_fixture();
        let dir = TempDir::new("create-timeout");
        let bin = write_executable(dir.path(), "herdr", "#!/bin/sh\nexec sleep 5\n");
        let context = context_for(&bin);
        let started = Instant::now();
        match create_tab_with_timeout(
            &context,
            &WorkspaceId::new("w1"),
            &cwd,
            || false,
            Duration::from_millis(200),
        )
        .unwrap_err()
        {
            RunError::Failed(error) => {
                assert_eq!(error.kind(), ErrorKind::HerdrTimeout);
                assert!(error.message().contains("partially completed"));
                assert!(error.message().contains("not rolled back"));
            }
            RunError::Interrupted => panic!("expected timeout"),
        }
        assert!(started.elapsed() < Duration::from_secs(3));

        let bin = write_executable(
            dir.path(),
            "herdr-bad",
            "#!/bin/sh\nprintf '{\"id\":\"x\",\"result\":{\"type\":\"tab_created\"}}\\n'\n",
        );
        let context = context_for(&bin);
        match create_tab(&context, &WorkspaceId::new("w1"), &cwd, || false).unwrap_err() {
            RunError::Failed(error) => {
                assert_eq!(error.kind(), ErrorKind::HerdrProtocol);
                assert!(error.message().contains("partially completed"));
                assert!(error.message().contains("not rolled back"));
            }
            RunError::Interrupted => panic!("expected protocol error"),
        }
    }

    #[test]
    fn start_failure_is_not_reported_as_partial() {
        let _cli = lock_cli();
        let (_cwd_dir, cwd) = cwd_fixture();
        let dir = TempDir::new("create-start");
        let bin = write_executable(dir.path(), "herdr", "#!/bin/sh\nexit 0\n");
        let context = context_for(&bin);
        crate::herdr::test_support::chmod(&bin, 0o644);
        match create_tab(&context, &WorkspaceId::new("w1"), &cwd, || false).unwrap_err() {
            RunError::Failed(error) => {
                assert_eq!(error.kind(), ErrorKind::HerdrTransport);
                assert!(!error.message().contains("partially completed"));
            }
            RunError::Interrupted => panic!("unexpected interrupt"),
        }
    }

    #[test]
    fn herdr_action_rejection_is_not_partial() {
        let _cli = lock_cli();
        let (_cwd_dir, cwd) = cwd_fixture();
        let dir = TempDir::new("create-action");
        let bin = write_executable(
            dir.path(),
            "herdr",
            r#"#!/bin/sh
printf '{"id":"x","error":{"code":"tab_create_failed","message":"denied"}}\n' >&2
exit 1
"#,
        );
        let context = context_for(&bin);
        match create_tab(&context, &WorkspaceId::new("w1"), &cwd, || false).unwrap_err() {
            RunError::Failed(error) => {
                assert_eq!(error.kind(), ErrorKind::HerdrAction);
                assert!(!error.message().contains("partially completed"));
            }
            RunError::Interrupted => panic!("unexpected interrupt"),
        }
    }

    #[test]
    fn mismatched_create_identities_are_protocol_errors() {
        let _cli = lock_cli();
        let (_cwd_dir, cwd) = cwd_fixture();
        let dir = TempDir::new("create-mismatch");

        let cases = [
            (
                "tab-other-tab",
                r#"{"id":"cli:tab:create","result":{"type":"tab_created","tab":{"tab_id":"w1:t2","workspace_id":"w1","number":2,"label":"src","focused":true,"pane_count":1,"agent_status":"idle"},"root_pane":{"pane_id":"w1:p3","terminal_id":"t","workspace_id":"w1","tab_id":"w1:t9","focused":true,"agent_status":"idle","revision":1}}}"#,
                true,
                "root pane tab does not match",
            ),
            (
                "tab-other-workspace",
                r#"{"id":"cli:tab:create","result":{"type":"tab_created","tab":{"tab_id":"w1:t2","workspace_id":"w1","number":2,"label":"src","focused":true,"pane_count":1,"agent_status":"idle"},"root_pane":{"pane_id":"w1:p3","terminal_id":"t","workspace_id":"w9","tab_id":"w1:t2","focused":true,"agent_status":"idle","revision":1}}}"#,
                true,
                "root pane workspace does not match",
            ),
            (
                "ws-other-tab",
                r#"{"id":"cli:workspace:create","result":{"type":"workspace_created","workspace":{"workspace_id":"w2","number":2,"label":"other","focused":true,"pane_count":1,"tab_count":1,"active_tab_id":"w2:t1","agent_status":"idle"},"tab":{"tab_id":"w2:t1","workspace_id":"w2","number":1,"label":"main","focused":true,"pane_count":1,"agent_status":"idle"},"root_pane":{"pane_id":"w2:p1","terminal_id":"t","workspace_id":"w2","tab_id":"w2:t9","focused":true,"agent_status":"idle","revision":1}}}"#,
                false,
                "root pane tab does not match",
            ),
        ];

        for (name, json, is_tab, detail) in cases {
            let bin = write_executable(
                dir.path(),
                name,
                &format!("#!/bin/sh\nprintf '%s\\n' {json}\n", json = sh_single(json),),
            );
            let context = context_for(&bin);
            let err = if is_tab {
                create_tab(&context, &WorkspaceId::new("w1"), &cwd, || false).unwrap_err()
            } else {
                create_workspace(&context, &cwd, || false).unwrap_err()
            };
            match err {
                RunError::Failed(error) => {
                    assert_eq!(error.kind(), ErrorKind::HerdrProtocol, "{name}");
                    assert!(
                        error.message().contains(detail),
                        "{name}: expected {detail:?} in {}",
                        error.message()
                    );
                    assert!(
                        error.message().contains("partially completed"),
                        "{name}: mismatched identities are ambiguous after dispatch"
                    );
                }
                RunError::Interrupted => panic!("{name}: unexpected interrupt"),
            }
        }
    }

    #[test]
    fn workspace_not_found_is_typed() {
        let _cli = lock_cli();
        let (_cwd_dir, cwd) = cwd_fixture();
        let dir = TempDir::new("create-nf");
        let bin = write_executable(
            dir.path(),
            "herdr",
            r#"#!/bin/sh
printf '{"id":"x","error":{"code":"workspace_not_found","message":"workspace w9 not found"}}\n' >&2
exit 1
"#,
        );
        let context = context_for(&bin);
        let CommandResult::NotFound { code, .. } =
            create_tab(&context, &WorkspaceId::new("w9"), &cwd, || false).unwrap()
        else {
            panic!("expected not found");
        };
        assert_eq!(code, "workspace_not_found");
    }
}
