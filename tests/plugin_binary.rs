use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{ChildStdin, Command, Stdio};

use nu_plugin_core::{Encoder, MsgPackSerializer};
use nu_plugin_protocol::{
    DynamicCompletionCall, EngineCall, EngineCallResponse, GetCompletionArgType, GetCompletionInfo,
    PipelineDataHeader, PluginCall, PluginCallResponse, PluginInput, PluginOutput, ProtocolInfo,
};
use nu_protocol::{
    Span, SyntaxShape, Type, Value,
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
fn compiled_binary_dynamic_completion_returns_directory_items_when_enabled_outside_herdr_still_falls_back()
 {
    let config = Value::test_record(record! {
        "dynamic_completion" => Value::test_bool(true),
    });
    let suggestions = request_completion(Some(config));
    assert!(
        suggestions.is_none(),
        "outside Herdr, enabled completion still falls back to native directory completion"
    );
}

fn request_completion(plugin_config: Option<Value>) -> Option<Vec<nu_protocol::DynamicSuggestion>> {
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
                    &PluginInput::EngineCallResponse(id, respond_engine_call(call, &plugin_config)),
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
    plugin_config: &Option<Value>,
) -> EngineCallResponse<PipelineDataHeader> {
    match call {
        EngineCall::GetPluginConfig => match plugin_config {
            Some(value) => {
                EngineCallResponse::PipelineData(PipelineDataHeader::Value(value.clone(), None))
            }
            None => EngineCallResponse::PipelineData(PipelineDataHeader::Empty),
        },
        EngineCall::GetEnvVar(_) => EngineCallResponse::PipelineData(PipelineDataHeader::Empty),
        EngineCall::GetEnvVars => EngineCallResponse::ValueMap(HashMap::new()),
        EngineCall::GetCurrentDir => EngineCallResponse::PipelineData(PipelineDataHeader::Value(
            Value::test_string("/tmp"),
            None,
        )),
        other => panic!("unexpected engine call: {}", other.name()),
    }
}

fn encode(encoder: &MsgPackSerializer, writer: &mut impl Write, input: &PluginInput) {
    encoder
        .encode(input, writer)
        .unwrap_or_else(|error| panic!("encode {input:?}: {error:?}"));
}
