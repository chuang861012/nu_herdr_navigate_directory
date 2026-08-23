use std::collections::HashMap;
use std::fs;
use std::io::{BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use nu_plugin_core::{Encoder, MsgPackSerializer};
use nu_plugin_protocol::{
    DynamicCompletionCall, EngineCall, EngineCallResponse, GetCompletionArgType, GetCompletionInfo,
    PipelineDataHeader, PluginCall, PluginCallResponse, PluginInput, PluginOutput, ProtocolInfo,
};
use nu_protocol::{
    DynamicSuggestion, Span, SuggestionKind, SyntaxShape, Type, Value,
    ast::{Call, Expr, Expression},
    record,
};

#[test]
fn compiled_binary_name_matches_plugin_identity() {
    let path = Path::new(env!("CARGO_BIN_EXE_nu_plugin_herdr_navigate_directory"));
    let file_name = path
        .file_name()
        .expect("plugin binary file name")
        .to_string_lossy();
    assert!(
        file_name.starts_with("nu_plugin_herdr_navigate_directory"),
        "unexpected plugin binary name: {file_name}"
    );
}

#[test]
fn compiled_binary_exposes_metadata_and_hnd_signature() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_nu_plugin_herdr_navigate_directory"))
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn plugin binary");

    let mut stdin = child.stdin.take().expect("plugin stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("plugin stdout"));

    let mut length = [0_u8; 1];
    stdout
        .read_exact(&mut length)
        .expect("read encoding length");
    let encoding_len = usize::from(length[0]);
    let mut encoding = vec![0_u8; encoding_len];
    stdout
        .read_exact(&mut encoding)
        .expect("read encoding name");
    assert_eq!(encoding, b"msgpack");

    let encoder = MsgPackSerializer;
    match encoder
        .decode(&mut stdout)
        .expect("decode plugin hello")
        .expect("plugin hello")
    {
        PluginOutput::Hello(_) => {}
        other => panic!("expected plugin Hello, got {other:?}"),
    }

    encode(
        &encoder,
        &mut stdin,
        &PluginInput::Hello(ProtocolInfo::default()),
    );
    encode(
        &encoder,
        &mut stdin,
        &PluginInput::Call(0, PluginCall::Metadata),
    );
    encode(
        &encoder,
        &mut stdin,
        &PluginInput::Call(1, PluginCall::Signature),
    );
    encode(&encoder, &mut stdin, &PluginInput::Goodbye);
    stdin.flush().expect("flush plugin stdin");
    drop(stdin);

    let mut metadata = None;
    let mut signatures = None;
    while metadata.is_none() || signatures.is_none() {
        let Some(message) = encoder.decode(&mut stdout).expect("decode plugin output") else {
            break;
        };
        match message {
            PluginOutput::CallResponse(0, PluginCallResponse::Metadata(value)) => {
                metadata = Some(value);
            }
            PluginOutput::CallResponse(1, PluginCallResponse::Signature(value)) => {
                signatures = Some(value);
            }
            PluginOutput::Hello(_) | PluginOutput::Option(_) => {}
            other => panic!("unexpected plugin output: {other:?}"),
        }
    }

    let metadata = metadata.expect("plugin metadata");
    assert_eq!(metadata.version.as_deref(), Some(env!("CARGO_PKG_VERSION")));

    let signatures = signatures.expect("plugin signatures");
    assert_eq!(signatures.len(), 1, "plugin must advertise only hnd");
    let signature = &signatures[0].sig;
    assert_eq!(signature.name, "hnd");
    assert_eq!(signature.required_positional.len(), 1);
    assert_eq!(signature.required_positional[0].name, "path");
    assert_eq!(
        signature.required_positional[0].shape,
        SyntaxShape::Directory
    );
    assert!(signature.optional_positional.is_empty());
    assert!(signature.rest_positional.is_none());
    assert!(
        signature
            .named
            .iter()
            .all(|flag| flag.long == "help" && flag.arg.is_none()),
        "hnd must not declare command-specific flags, found {:?}",
        signature
            .named
            .iter()
            .map(|flag| &flag.long)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        signature.input_output_types,
        vec![(Type::Nothing, Type::Nothing)]
    );
    assert!(!signature.creates_scope);
    assert!(!signature.allows_unknown_args);

    let status = child.wait().expect("wait for plugin");
    assert!(status.success(), "plugin exited with {status}");
}

#[test]
fn compiled_binary_dynamic_completion_falls_back_when_disabled() {
    let suggestions = request_completion(None);
    assert!(
        suggestions.is_none(),
        "disabled completion must return None"
    );
}

#[test]
fn compiled_binary_dynamic_completion_falls_back_when_enabled_outside_herdr() {
    let config = Value::test_record(record! {
        "dynamic_completion" => Value::test_bool(true),
    });
    let suggestions = request_completion(Some(config));
    assert!(
        suggestions.is_none(),
        "outside Herdr, enabled completion still falls back to native directory completion"
    );
}

#[test]
fn compiled_binary_dynamic_completion_returns_directory_items_when_enabled() {
    let root = unique_temp("enabled-sdk");
    let src = root.join("src");
    fs::create_dir(&src).expect("create src pane directory");
    let src_utf8 = src.to_str().expect("src is UTF-8");
    let root_utf8 = root.to_str().expect("root is UTF-8");

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
                  "repo_root": "{root_utf8}",
                  "checkout_path": "{root_utf8}",
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
                "cwd": "{root_utf8}",
                "foreground_cwd": "{root_utf8}"
              }}, {{
                "pane_id": "w1:p2",
                "terminal_id": "term2",
                "workspace_id": "w1",
                "tab_id": "w1:t1",
                "focused": false,
                "agent_status": "idle",
                "revision": 2,
                "agent": "codex",
                "foreground_cwd": "{src_utf8}"
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
              "foreground_cwd": "{root_utf8}"
            }}
          }}
        }}"#
    );

    let snapshot_path = root.join("snapshot.json");
    let current_path = root.join("current.json");
    let record_path = root.join("record");
    fs::write(&snapshot_path, snapshot).expect("write snapshot");
    fs::write(&current_path, current).expect("write current pane");
    let bin = write_executable(
        &root,
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
            record = sh_single(&record_path.display().to_string()),
            snapshot = sh_single(&snapshot_path.display().to_string()),
            current = sh_single(&current_path.display().to_string()),
        ),
    );
    let _ = Command::new(&bin).args(["api", "snapshot"]).output();

    let config = Value::test_record(record! {
        "dynamic_completion" => Value::test_bool(true),
    });
    let mut env = HashMap::new();
    env.insert("HERDR_ENV".into(), Value::test_string("1"));
    env.insert(
        "HERDR_BIN_PATH".into(),
        Value::test_string(bin.to_str().expect("bin is UTF-8")),
    );
    env.insert(
        "HERDR_SOCKET_PATH".into(),
        Value::test_string("/tmp/nu-plugin-herdr-navigate-directory.sock"),
    );
    env.insert("HERDR_WORKSPACE_ID".into(), Value::test_string("w1"));
    env.insert("HERDR_TAB_ID".into(), Value::test_string("w1:t1"));
    env.insert("HERDR_PANE_ID".into(), Value::test_string("w1:p1"));

    let suggestions = request_completion_with(CompletionHarness {
        plugin_config: Some(config),
        env,
        cwd: root_utf8.to_string(),
    })
    .expect("enabled completion must return items");
    assert!(
        !suggestions.is_empty(),
        "enabled completion must return structured directory suggestions"
    );
    assert!(suggestions.iter().all(|item| {
        item.kind == Some(SuggestionKind::Directory)
            && item.value.ends_with('/')
            && !item.append_whitespace
    }));
    assert!(
        suggestions.iter().any(|item| {
            item.value.contains("src")
                && item
                    .description
                    .as_deref()
                    .is_some_and(|text| text.contains("agent idle"))
        }),
        "expected a Herdr semantic directory suggestion, got {suggestions:?}"
    );
    let recorded = fs::read_to_string(&record_path).unwrap_or_default();
    assert!(recorded.contains("api snapshot"));
    assert!(recorded.contains("pane current --current"));
    assert!(!recorded.contains("process-info"));
    let _ = fs::remove_dir_all(&root);
}

struct CompletionHarness {
    plugin_config: Option<Value>,
    env: HashMap<String, Value>,
    cwd: String,
}

fn request_completion(plugin_config: Option<Value>) -> Option<Vec<DynamicSuggestion>> {
    request_completion_with(CompletionHarness {
        plugin_config,
        env: HashMap::new(),
        cwd: "/tmp".into(),
    })
}

fn request_completion_with(harness: CompletionHarness) -> Option<Vec<DynamicSuggestion>> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_nu_plugin_herdr_navigate_directory"))
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn plugin binary");

    let mut stdin = child.stdin.take().expect("plugin stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("plugin stdout"));
    let encoder = handshake(&mut stdin, &mut stdout);

    let mut call = Call::new(Span::test_data());
    call.add_positional(Expression {
        expr: Expr::Directory(String::new(), false),
        span: Span::test_data(),
        span_id: nu_protocol::SpanId::new(0),
        ty: Type::String,
    });
    encode(
        &encoder,
        &mut stdin,
        &PluginInput::Call(
            0,
            PluginCall::GetCompletion(GetCompletionInfo {
                name: "hnd".into(),
                arg_type: GetCompletionArgType::Positional(0),
                call: DynamicCompletionCall {
                    call,
                    strip: false,
                    pos: 0,
                },
            }),
        ),
    );
    stdin.flush().expect("flush completion call");

    let mut completion = None;
    loop {
        let Some(message) = encoder.decode(&mut stdout).expect("decode plugin output") else {
            break;
        };
        match message {
            PluginOutput::CallResponse(0, PluginCallResponse::CompletionItems(items)) => {
                completion = Some(items);
                break;
            }
            PluginOutput::EngineCall { context, id, call } => {
                encode(
                    &encoder,
                    &mut stdin,
                    &PluginInput::EngineCallResponse(id, respond_engine_call(call, &harness)),
                );
                stdin.flush().expect("flush engine response");
                let _ = context;
            }
            PluginOutput::Hello(_) | PluginOutput::Option(_) => {}
            other => panic!("unexpected plugin output: {other:?}"),
        }
    }

    encode(&encoder, &mut stdin, &PluginInput::Goodbye);
    drop(stdin);
    let status = child.wait().expect("wait for plugin");
    assert!(status.success(), "plugin exited with {status}");
    completion.expect("completion response")
}

fn handshake(stdin: &mut ChildStdin, stdout: &mut BufReader<impl Read>) -> MsgPackSerializer {
    let mut length = [0_u8; 1];
    stdout
        .read_exact(&mut length)
        .expect("read encoding length");
    let encoding_len = usize::from(length[0]);
    let mut encoding = vec![0_u8; encoding_len];
    stdout
        .read_exact(&mut encoding)
        .expect("read encoding name");
    assert_eq!(encoding, b"msgpack");

    let encoder = MsgPackSerializer;
    match encoder
        .decode(stdout)
        .expect("decode plugin hello")
        .expect("plugin hello")
    {
        PluginOutput::Hello(_) => {}
        other => panic!("expected plugin Hello, got {other:?}"),
    }
    encode(
        &encoder,
        stdin,
        &PluginInput::Hello(ProtocolInfo::default()),
    );
    encoder
}

fn respond_engine_call(
    call: EngineCall<PipelineDataHeader>,
    harness: &CompletionHarness,
) -> EngineCallResponse<PipelineDataHeader> {
    match call {
        EngineCall::GetPluginConfig => match &harness.plugin_config {
            Some(value) => {
                EngineCallResponse::PipelineData(PipelineDataHeader::Value(value.clone(), None))
            }
            None => EngineCallResponse::PipelineData(PipelineDataHeader::Empty),
        },
        EngineCall::GetEnvVar(name) => match harness.env.get(&name) {
            Some(value) => {
                EngineCallResponse::PipelineData(PipelineDataHeader::Value(value.clone(), None))
            }
            None => EngineCallResponse::PipelineData(PipelineDataHeader::Empty),
        },
        EngineCall::GetEnvVars => EngineCallResponse::ValueMap(harness.env.clone()),
        EngineCall::GetCurrentDir => EngineCallResponse::PipelineData(PipelineDataHeader::Value(
            Value::test_string(&harness.cwd),
            None,
        )),
        other => panic!("unexpected engine call: {}", other.name()),
    }
}

fn unique_temp(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "hnd-proto-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&path).expect("create protocol fixture");
    path
}

fn write_executable(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).expect("write fake herdr");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod fake herdr");
    path
}

fn sh_single(path: &str) -> String {
    format!("'{}'", path.replace('\'', r#"'"'"'"#))
}

fn encode(encoder: &MsgPackSerializer, writer: &mut impl Write, input: &PluginInput) {
    encoder
        .encode(input, writer)
        .unwrap_or_else(|error| panic!("encode {input:?}: {error:?}"));
}
