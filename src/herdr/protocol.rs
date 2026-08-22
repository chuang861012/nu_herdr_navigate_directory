//! Typed Herdr CLI JSON envelopes. Transport models stay in this module.

use serde::Deserialize;

use super::cli::{CliOutput, RunError};
use crate::domain::Error;

pub(crate) const MIN_PROTOCOL: u32 = 20;
pub(crate) const MIN_VERSION: (u64, u64, u64) = (0, 8, 2);

const SNAPSHOT: &str = "session_snapshot";
const PANE_CURRENT: &str = "pane_current";
const PROCESS_INFO: &str = "pane_process_info";
const PANE_INFO: &str = "pane_info";
const TAB_CREATED: &str = "tab_created";
const WORKSPACE_CREATED: &str = "workspace_created";

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    result: Option<TaggedResult>,
    #[serde(default)]
    error: Option<ErrorBody>,
}

#[derive(Debug, Deserialize)]
struct TaggedResult {
    #[serde(rename = "type")]
    kind: String,
    #[serde(flatten)]
    fields: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    code: String,
    #[serde(default)]
    message: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct RawSnapshot {
    pub version: String,
    pub protocol: u32,
    #[serde(default)]
    pub focused_workspace_id: Option<String>,
    #[serde(default)]
    pub focused_tab_id: Option<String>,
    pub workspaces: Vec<RawWorkspace>,
    pub tabs: Vec<RawTab>,
    pub panes: Vec<RawPane>,
    pub layouts: Vec<RawLayout>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct RawWorkspace {
    pub workspace_id: String,
    pub number: usize,
    pub label: String,
    pub focused: bool,
    pub pane_count: usize,
    pub tab_count: usize,
    pub active_tab_id: String,
    pub agent_status: RawAgentStatus,
    #[serde(default)]
    pub worktree: Option<RawWorktree>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawWorktree {
    pub checkout_path: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct RawTab {
    pub tab_id: String,
    pub workspace_id: String,
    pub number: usize,
    pub label: String,
    pub focused: bool,
    pub pane_count: usize,
    pub agent_status: RawAgentStatus,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct RawPane {
    pub pane_id: String,
    pub terminal_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub focused: bool,
    pub agent_status: RawAgentStatus,
    pub revision: u64,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct RawLayout {
    pub workspace_id: String,
    pub tab_id: String,
    pub focused_pane_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawProcessInfo {
    pub pane_id: String,
    #[serde(default)]
    pub shell_pid: Option<u32>,
    #[serde(default)]
    pub foreground_process_group_id: Option<u32>,
    #[serde(default)]
    pub foreground_processes: Vec<RawForegroundProcess>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct RawForegroundProcess {
    pub pid: u32,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RawAgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServerErrorStyle {
    Protocol,
    Action,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandResult<T> {
    Ok(T),
    NotFound { code: String, message: String },
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreatedTabBody {
    pub tab: CreatedTabIdentity,
    pub root_pane: CreatedPaneIdentity,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreatedWorkspaceBody {
    pub workspace: CreatedWorkspaceIdentity,
    pub tab: CreatedTabIdentity,
    pub root_pane: CreatedPaneIdentity,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreatedWorkspaceIdentity {
    pub workspace_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreatedTabIdentity {
    pub tab_id: String,
    pub workspace_id: String,
}

#[derive(Debug, Deserialize)]
struct FocusedPaneIdentity {
    pub pane_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreatedPaneIdentity {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
}

pub(crate) fn parse_snapshot(output: &CliOutput) -> Result<CommandResult<RawSnapshot>, RunError> {
    parse_typed(output, SNAPSHOT, "session snapshot", |fields| {
        let snapshot = fields
            .get("snapshot")
            .ok_or_else(|| protocol("session snapshot", "missing snapshot object"))?;
        decode(snapshot, "session snapshot")
    })
}

pub(crate) fn parse_pane_current(output: &CliOutput) -> Result<CommandResult<RawPane>, RunError> {
    parse_typed(output, PANE_CURRENT, "live caller lookup", |fields| {
        let pane = fields
            .get("pane")
            .ok_or_else(|| protocol("live caller lookup", "missing pane object"))?;
        decode(pane, "live caller lookup")
    })
}

pub(crate) fn parse_process_info(
    output: &CliOutput,
) -> Result<CommandResult<RawProcessInfo>, RunError> {
    parse_typed(output, PROCESS_INFO, "pane process inspection", |fields| {
        let info = fields
            .get("process_info")
            .ok_or_else(|| protocol("pane process inspection", "missing process_info object"))?;
        decode(info, "pane process inspection")
    })
}

pub(crate) fn parse_tab_created(
    output: &CliOutput,
) -> Result<CommandResult<CreatedTabBody>, RunError> {
    parse_action(output, TAB_CREATED, "tab creation", |fields| {
        decode(fields, "tab creation")
    })
}

pub(crate) fn parse_workspace_created(
    output: &CliOutput,
) -> Result<CommandResult<CreatedWorkspaceBody>, RunError> {
    parse_action(output, WORKSPACE_CREATED, "workspace creation", |fields| {
        decode(fields, "workspace creation")
    })
}

pub(crate) fn parse_pane_focus_response(
    payload: &[u8],
    request_id: &str,
    pane_id: &str,
) -> Result<CommandResult<()>, RunError> {
    parse_payload(
        payload,
        PANE_INFO,
        Some(request_id),
        "pane focus",
        ServerErrorStyle::Action,
        |fields| {
            let pane = fields
                .get("pane")
                .ok_or_else(|| protocol("pane focus", "missing pane object"))?;
            let focused: FocusedPaneIdentity = decode(pane, "pane focus")?;
            if focused.pane_id != pane_id {
                return Err(protocol("pane focus", "returned a mismatched pane id"));
            }
            if focused.pane_id.is_empty() {
                return Err(protocol("pane focus", "missing pane id"));
            }
            Ok(())
        },
    )
    .map_err(Into::into)
}

fn parse_typed<T>(
    output: &CliOutput,
    expected_kind: &str,
    operation: &str,
    decode_fields: impl FnOnce(&serde_json::Value) -> Result<T, Error>,
) -> Result<CommandResult<T>, RunError> {
    parse_cli(
        output,
        expected_kind,
        operation,
        ServerErrorStyle::Protocol,
        decode_fields,
    )
}

fn parse_action<T>(
    output: &CliOutput,
    expected_kind: &str,
    operation: &str,
    decode_fields: impl FnOnce(&serde_json::Value) -> Result<T, Error>,
) -> Result<CommandResult<T>, RunError> {
    parse_cli(
        output,
        expected_kind,
        operation,
        ServerErrorStyle::Action,
        decode_fields,
    )
}

fn parse_cli<T>(
    output: &CliOutput,
    expected_kind: &str,
    operation: &str,
    style: ServerErrorStyle,
    decode_fields: impl FnOnce(&serde_json::Value) -> Result<T, Error>,
) -> Result<CommandResult<T>, RunError> {
    let payload = if output.status.success() {
        &output.stdout
    } else if !output.stderr.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };

    if payload.is_empty() {
        if output.status.success() {
            return Err(protocol(operation, "empty response").into());
        }
        return Err(failed_status(operation, output).into());
    }

    let envelope: Envelope = match serde_json::from_slice(payload) {
        Ok(envelope) => envelope,
        Err(_) => {
            if output.status.success() {
                return Err(protocol(operation, "response is not valid JSON").into());
            }
            return Err(failed_status(operation, output).into());
        }
    };

    match interpret_envelope(
        envelope,
        expected_kind,
        None,
        operation,
        style,
        decode_fields,
    ) {
        Ok(CommandResult::Ok(_)) if !output.status.success() => {
            Err(failed_status(operation, output).into())
        }
        Ok(result) => Ok(result),
        Err(error) => Err(error.into()),
    }
}

fn parse_payload<T>(
    payload: &[u8],
    expected_kind: &str,
    expected_id: Option<&str>,
    operation: &str,
    style: ServerErrorStyle,
    decode_fields: impl FnOnce(&serde_json::Value) -> Result<T, Error>,
) -> Result<CommandResult<T>, Error> {
    let envelope: Envelope = serde_json::from_slice(payload)
        .map_err(|_| protocol(operation, "response is not valid JSON"))?;
    interpret_envelope(
        envelope,
        expected_kind,
        expected_id,
        operation,
        style,
        decode_fields,
    )
}

fn interpret_envelope<T>(
    envelope: Envelope,
    expected_kind: &str,
    expected_id: Option<&str>,
    operation: &str,
    style: ServerErrorStyle,
    decode_fields: impl FnOnce(&serde_json::Value) -> Result<T, Error>,
) -> Result<CommandResult<T>, Error> {
    if envelope.id.as_deref().is_none_or(str::is_empty) {
        return Err(protocol(operation, "missing response id"));
    }
    if let Some(expected_id) = expected_id
        && envelope.id.as_deref() != Some(expected_id)
    {
        return Err(protocol(
            operation,
            "response id does not match the request",
        ));
    }
    if envelope.result.is_some() && envelope.error.is_some() {
        return Err(protocol(
            operation,
            "response includes both result and error",
        ));
    }

    if let Some(error) = envelope.error {
        if is_not_found_code(&error.code) {
            return Ok(CommandResult::NotFound {
                code: error.code,
                message: super::sanitize_detail(error.message.as_deref().unwrap_or("not found")),
            });
        }
        return Err(server_error(operation, &error, style));
    }

    let Some(result) = envelope.result else {
        return Err(protocol(operation, "missing result"));
    };
    if result.kind != expected_kind {
        return Err(protocol(
            operation,
            &format!(
                "unexpected result kind {}",
                super::sanitize_detail(&result.kind)
            ),
        ));
    }
    Ok(CommandResult::Ok(decode_fields(&result.fields)?))
}

fn is_not_found_code(code: &str) -> bool {
    code == "not_found" || code.ends_with("_not_found")
}

fn decode<T: for<'de> Deserialize<'de>>(
    value: &serde_json::Value,
    operation: &str,
) -> Result<T, Error> {
    serde_json::from_value(value.clone())
        .map_err(|_| protocol(operation, "missing or invalid required fields"))
}

pub(crate) fn require_supported_version(snapshot: &RawSnapshot) -> Result<(), Error> {
    let Some(version) = parse_version(&snapshot.version) else {
        return Err(Error::incompatible_herdr(
            "Herdr snapshot version is missing or unsupported",
        ));
    };
    if snapshot.protocol < MIN_PROTOCOL || version < MIN_VERSION {
        return Err(Error::incompatible_herdr(
            "Herdr version or protocol is below the 0.8.2 baseline",
        ));
    }
    Ok(())
}

fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.trim().trim_start_matches('v');
    let core = value.split(['-', '+']).next().unwrap_or(value);
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

fn protocol(operation: &str, detail: &str) -> Error {
    Error::herdr_protocol(format!("{operation}: {detail}"))
}

fn server_error(operation: &str, error: &ErrorBody, style: ServerErrorStyle) -> Error {
    let code = super::sanitize_detail(&error.code);
    let message = error
        .message
        .as_deref()
        .map(super::sanitize_detail)
        .filter(|message| !message.is_empty());
    let detail = match message {
        Some(message) => format!("{operation} failed ({code}: {message})"),
        None => format!("{operation} failed ({code})"),
    };
    match style {
        ServerErrorStyle::Protocol => Error::herdr_protocol(detail),
        ServerErrorStyle::Action => Error::herdr_action(detail),
    }
}

fn failed_status(operation: &str, output: &CliOutput) -> Error {
    let code = output.status.code().unwrap_or(-1);
    Error::herdr_transport(format!(
        "{operation} exited with status {code}: {}",
        super::cli::utf8_lossy_sanitized(&output.stderr)
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        CommandResult, parse_pane_current, parse_pane_focus_response, parse_process_info,
        parse_snapshot, parse_tab_created, parse_workspace_created, require_supported_version,
    };
    use crate::domain::ErrorKind;
    use crate::herdr::cli::CliOutput;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    fn output(ok: bool, body: &str) -> CliOutput {
        CliOutput {
            stdout: if ok {
                body.as_bytes().to_vec()
            } else {
                Vec::new()
            },
            stderr: if ok {
                Vec::new()
            } else {
                body.as_bytes().to_vec()
            },
            status: if ok {
                ExitStatus::from_raw(0)
            } else {
                ExitStatus::from_raw(256)
            },
        }
    }

    fn snapshot_json(extra_workspace_fields: &str, extra_pane_fields: &str) -> String {
        format!(
            r#"{{
              "id": "cli:session:snapshot",
              "result": {{
                "type": "session_snapshot",
                "snapshot": {{
                  "version": "0.8.2",
                  "protocol": 20,
                  "focused_workspace_id": "w1",
                  "focused_tab_id": "w1:t1",
                  "focused_pane_id": "w1:p1",
                  "workspaces": [{{
                    "workspace_id": "w1",
                    "number": 1,
                    "label": "repo",
                    "focused": true,
                    "pane_count": 1,
                    "tab_count": 1,
                    "active_tab_id": "w1:t1",
                    "agent_status": "idle"{extra_workspace_fields}
                  }}],
                  "tabs": [{{
                    "tab_id": "w1:t1",
                    "workspace_id": "w1",
                    "number": 1,
                    "label": "main",
                    "focused": true,
                    "pane_count": 1,
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
                    "foreground_cwd": "/repo"{extra_pane_fields}
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
                  "agents": [],
                  "ignored_future_field": true
                }}
              }}
            }}"#
        )
    }

    #[test]
    fn snapshot_accepts_unknown_fields_and_required_values() {
        let json = snapshot_json(", \"future_workspace\": 1", r#", "title": "x""#);
        let CommandResult::Ok(snapshot) = parse_snapshot(&output(true, &json)).unwrap() else {
            panic!("expected snapshot");
        };
        assert_eq!(snapshot.version, "0.8.2");
        assert_eq!(snapshot.protocol, 20);
        assert_eq!(snapshot.workspaces[0].workspace_id, "w1");
        assert_eq!(snapshot.panes[0].foreground_cwd.as_deref(), Some("/repo"));
        require_supported_version(&snapshot).unwrap();
    }

    #[test]
    fn snapshot_missing_required_field_is_protocol_error() {
        let json = r#"{"id":"x","result":{"type":"session_snapshot","snapshot":{"version":"0.8.2","protocol":20}}}"#;
        let err = parse_snapshot(&output(true, json)).unwrap_err();
        match err {
            crate::herdr::cli::RunError::Failed(error) => {
                assert_eq!(error.kind(), ErrorKind::HerdrProtocol)
            }
            crate::herdr::cli::RunError::Interrupted => panic!("unexpected interrupt"),
        }
    }

    #[test]
    fn wrong_result_kind_is_protocol_error() {
        let json = r#"{"id":"x","result":{"type":"pane_current","pane":{"pane_id":"w1:p1","terminal_id":"t","workspace_id":"w1","tab_id":"w1:t1","focused":true,"agent_status":"idle","revision":1}}}"#;
        let err = parse_snapshot(&output(true, json)).unwrap_err();
        match err {
            crate::herdr::cli::RunError::Failed(error) => {
                assert!(error.message().contains("unexpected result kind"))
            }
            crate::herdr::cli::RunError::Interrupted => panic!("unexpected interrupt"),
        }
    }

    #[test]
    fn missing_envelope_id_is_a_protocol_error() {
        let success = r#"{"result":{"type":"session_snapshot","snapshot":{"version":"0.8.2","protocol":20,"workspaces":[],"tabs":[],"panes":[],"layouts":[]}}}"#;
        let err = parse_snapshot(&output(true, success)).unwrap_err();
        match err {
            crate::herdr::cli::RunError::Failed(error) => {
                assert_eq!(error.kind(), ErrorKind::HerdrProtocol);
                assert!(error.message().contains("missing response id"));
            }
            crate::herdr::cli::RunError::Interrupted => panic!("unexpected interrupt"),
        }

        let error = r#"{"error":{"code":"not_found","message":"pane not found"}}"#;
        let err = parse_process_info(&output(false, error)).unwrap_err();
        match err {
            crate::herdr::cli::RunError::Failed(failed) => {
                assert_eq!(failed.kind(), ErrorKind::HerdrProtocol);
                assert!(failed.message().contains("missing response id"));
            }
            crate::herdr::cli::RunError::Interrupted => panic!("unexpected interrupt"),
        }
    }

    #[test]
    fn not_found_is_a_distinct_command_result() {
        let json = r#"{"id":"x","error":{"code":"not_found","message":"pane not found"}}"#;
        let CommandResult::NotFound { code, .. } =
            parse_process_info(&output(false, json)).unwrap()
        else {
            panic!("expected not_found");
        };
        assert_eq!(code, "not_found");

        let json =
            r#"{"id":"x","error":{"code":"pane_not_found","message":"pane w1:p9 not found"}}"#;
        let CommandResult::NotFound { code, .. } =
            parse_process_info(&output(false, json)).unwrap()
        else {
            panic!("expected pane_not_found");
        };
        assert_eq!(code, "pane_not_found");
    }

    #[test]
    fn action_errors_use_the_herdr_action_kind() {
        let json = r#"{"id":"x","error":{"code":"tab_create_failed","message":"denied"}}"#;
        let err = parse_tab_created(&output(false, json)).unwrap_err();
        match err {
            crate::herdr::cli::RunError::Failed(error) => {
                assert_eq!(error.kind(), ErrorKind::HerdrAction);
                assert!(error.message().contains("tab_create_failed"));
            }
            crate::herdr::cli::RunError::Interrupted => panic!("unexpected interrupt"),
        }
    }

    #[test]
    fn tab_and_workspace_create_parse_identities() {
        let tab = r#"{
          "id": "cli:tab:create",
          "result": {
            "type": "tab_created",
            "tab": {"tab_id":"w1:t2","workspace_id":"w1","number":2,"label":"src","focused":true,"pane_count":1,"agent_status":"idle"},
            "root_pane": {"pane_id":"w1:p3","terminal_id":"term3","workspace_id":"w1","tab_id":"w1:t2","focused":true,"agent_status":"idle","revision":1}
          }
        }"#;
        let CommandResult::Ok(created) = parse_tab_created(&output(true, tab)).unwrap() else {
            panic!("expected tab");
        };
        assert_eq!(created.tab.tab_id, "w1:t2");
        assert_eq!(created.root_pane.pane_id, "w1:p3");
        assert_eq!(created.root_pane.workspace_id, "w1");
        assert_eq!(created.root_pane.tab_id, "w1:t2");

        let workspace = r#"{
          "id": "cli:workspace:create",
          "result": {
            "type": "workspace_created",
            "workspace": {"workspace_id":"w2","number":2,"label":"other","focused":true,"pane_count":1,"tab_count":1,"active_tab_id":"w2:t1","agent_status":"idle"},
            "tab": {"tab_id":"w2:t1","workspace_id":"w2","number":1,"label":"main","focused":true,"pane_count":1,"agent_status":"idle"},
            "root_pane": {"pane_id":"w2:p1","terminal_id":"term1","workspace_id":"w2","tab_id":"w2:t1","focused":true,"agent_status":"idle","revision":1}
          }
        }"#;
        let CommandResult::Ok(created) = parse_workspace_created(&output(true, workspace)).unwrap()
        else {
            panic!("expected workspace");
        };
        assert_eq!(created.workspace.workspace_id, "w2");
        assert_eq!(created.tab.tab_id, "w2:t1");
        assert_eq!(created.root_pane.pane_id, "w2:p1");
        assert_eq!(created.root_pane.workspace_id, "w2");
        assert_eq!(created.root_pane.tab_id, "w2:t1");
    }

    #[test]
    fn create_root_pane_missing_identity_fields_is_protocol_error() {
        let missing_workspace = r#"{
          "id": "cli:tab:create",
          "result": {
            "type": "tab_created",
            "tab": {"tab_id":"w1:t2","workspace_id":"w1","number":2,"label":"src","focused":true,"pane_count":1,"agent_status":"idle"},
            "root_pane": {"pane_id":"w1:p3","terminal_id":"term3","tab_id":"w1:t2","focused":true,"agent_status":"idle","revision":1}
          }
        }"#;
        let err = parse_tab_created(&output(true, missing_workspace)).unwrap_err();
        match err {
            crate::herdr::cli::RunError::Failed(error) => {
                assert_eq!(error.kind(), ErrorKind::HerdrProtocol);
            }
            crate::herdr::cli::RunError::Interrupted => panic!("unexpected interrupt"),
        }

        let missing_tab = r#"{
          "id": "cli:workspace:create",
          "result": {
            "type": "workspace_created",
            "workspace": {"workspace_id":"w2","number":2,"label":"other","focused":true,"pane_count":1,"tab_count":1,"active_tab_id":"w2:t1","agent_status":"idle"},
            "tab": {"tab_id":"w2:t1","workspace_id":"w2","number":1,"label":"main","focused":true,"pane_count":1,"agent_status":"idle"},
            "root_pane": {"pane_id":"w2:p1","terminal_id":"term1","workspace_id":"w2","focused":true,"agent_status":"idle","revision":1}
          }
        }"#;
        let err = parse_workspace_created(&output(true, missing_tab)).unwrap_err();
        match err {
            crate::herdr::cli::RunError::Failed(error) => {
                assert_eq!(error.kind(), ErrorKind::HerdrProtocol);
            }
            crate::herdr::cli::RunError::Interrupted => panic!("unexpected interrupt"),
        }
    }

    #[test]
    fn pane_focus_matches_request_id_and_pane_id() {
        let json = r#"{"id":"hcd-1","result":{"type":"pane_info","pane":{"pane_id":"w1:p2","ignored":true}}}"#;
        parse_pane_focus_response(json.as_bytes(), "hcd-1", "w1:p2").unwrap();

        let mismatch = r#"{"id":"other","result":{"type":"pane_info","pane":{"pane_id":"w1:p2"}}}"#;
        let err = parse_pane_focus_response(mismatch.as_bytes(), "hcd-1", "w1:p2").unwrap_err();
        match err {
            crate::herdr::cli::RunError::Failed(error) => {
                assert!(error.message().contains("response id does not match"))
            }
            crate::herdr::cli::RunError::Interrupted => panic!("unexpected interrupt"),
        }

        let wrong_pane =
            r#"{"id":"hcd-1","result":{"type":"pane_info","pane":{"pane_id":"w1:p9"}}}"#;
        let err = parse_pane_focus_response(wrong_pane.as_bytes(), "hcd-1", "w1:p2").unwrap_err();
        match err {
            crate::herdr::cli::RunError::Failed(error) => {
                assert!(error.message().contains("mismatched pane id"))
            }
            crate::herdr::cli::RunError::Interrupted => panic!("unexpected interrupt"),
        }

        let not_found =
            r#"{"id":"hcd-1","error":{"code":"pane_not_found","message":"pane w1:p2 not found"}}"#;
        let CommandResult::NotFound { code, .. } =
            parse_pane_focus_response(not_found.as_bytes(), "hcd-1", "w1:p2").unwrap()
        else {
            panic!("expected not found");
        };
        assert_eq!(code, "pane_not_found");
    }

    #[test]
    fn malformed_json_is_protocol_error_on_success() {
        let err = parse_pane_current(&output(true, "not-json")).unwrap_err();
        match err {
            crate::herdr::cli::RunError::Failed(error) => {
                assert_eq!(error.kind(), ErrorKind::HerdrProtocol)
            }
            crate::herdr::cli::RunError::Interrupted => panic!("unexpected interrupt"),
        }
    }

    #[test]
    fn error_details_allow_socket_paths_and_redact_environment_assignments() {
        let stderr = "cannot connect to /tmp/nu-plugin-herdr-cd.sock HERDR_EXTRA=keep";
        let err = parse_snapshot(&output(false, stderr)).unwrap_err();
        match err {
            crate::herdr::cli::RunError::Failed(error) => {
                assert!(error.message().contains("/tmp/nu-plugin-herdr-cd.sock"));
                assert!(!error.message().contains("HERDR_EXTRA=keep"));
                assert!(error.message().contains("<redacted>"));
            }
            crate::herdr::cli::RunError::Interrupted => panic!("unexpected interrupt"),
        }

        let json = r#"{"id":"x","error":{"code":"io","message":"connect /run/herdr failed"}}"#;
        let err = parse_snapshot(&output(false, json)).unwrap_err();
        match err {
            crate::herdr::cli::RunError::Failed(error) => {
                assert!(error.message().contains("/run/herdr"));
            }
            crate::herdr::cli::RunError::Interrupted => panic!("unexpected interrupt"),
        }
    }

    #[test]
    fn older_version_is_incompatible() {
        let json = snapshot_json("", "");
        let CommandResult::Ok(mut snapshot) = parse_snapshot(&output(true, &json)).unwrap() else {
            panic!("expected snapshot");
        };
        snapshot.version = "0.8.1".into();
        let err = require_supported_version(&snapshot).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::IncompatibleHerdr);

        snapshot.version = "0.8.2".into();
        snapshot.protocol = 19;
        let err = require_supported_version(&snapshot).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::IncompatibleHerdr);
    }
}
