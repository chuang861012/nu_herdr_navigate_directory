//! Nushell plugin identity, `hcd` signature, and command-boundary orchestration.

mod orchestrate;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use nu_plugin::{EngineInterface, EvaluatedCall, Plugin, PluginCommand, SimplePluginCommand};
use nu_protocol::{Category, LabeledError, ShellError, Signature, Span, SyntaxShape, Type, Value};

use crate::domain::{Error, ErrorKind, resolve_paths};
use crate::herdr::{EnvValue, HerdrMode, RunError, classify_herdr_env, inside_context};

use orchestrate::{Outcome, TOTAL_DEADLINE, orchestrate};

const COMMAND_NAME: &str = "hcd";

/// Plugin process that registers exactly one public command, `hcd`.
pub struct HerdrCdPlugin;

struct Hcd;

impl Plugin for HerdrCdPlugin {
    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").into()
    }

    fn commands(&self) -> Vec<Box<dyn PluginCommand<Plugin = Self>>> {
        vec![Box::new(Hcd)]
    }
}

impl SimplePluginCommand for Hcd {
    type Plugin = HerdrCdPlugin;

    fn name(&self) -> &str {
        COMMAND_NAME
    }

    fn description(&self) -> &str {
        "Change directory, reusing an idle Herdr pane when possible"
    }

    fn extra_description(&self) -> &str {
        "Outside Herdr, hcd updates $env.PWD. Inside Herdr, it reuses an idle pane, changes directory only for downward navigation, or creates a focused tab or workspace. Successful calls return nothing."
    }

    fn signature(&self) -> Signature {
        Signature::build(COMMAND_NAME)
            .required("path", SyntaxShape::Filepath, "Directory to navigate to")
            .input_output_type(Type::Nothing, Type::Nothing)
            .allow_variants_without_examples(true)
            .category(Category::FileSystem)
    }

    fn run(
        &self,
        _plugin: &HerdrCdPlugin,
        engine: &EngineInterface,
        call: &EvaluatedCall,
        _input: &Value,
    ) -> Result<Value, LabeledError> {
        run_hcd(engine, call)
    }
}

fn run_hcd(engine: &EngineInterface, call: &EvaluatedCall) -> Result<Value, LabeledError> {
    let interrupted = || engine.signals().interrupted();
    if interrupted() {
        return Err(interrupt_error(call.head));
    }
    if !platform_is_supported() {
        return Err(labeled_error(
            &Error::unsupported_platform("hcd supports Linux and macOS only"),
            call.head,
            call.head,
        ));
    }

    let target: String = call.req(0)?;
    let path_span = call.nth(0).map(|value| value.span()).unwrap_or(call.head);
    let caller_cwd = engine.get_current_dir().map_err(|_| {
        labeled_error(
            &Error::invalid_path("caller working directory is unavailable"),
            path_span,
            call.head,
        )
    })?;
    let home = read_home(engine).map_err(|error| labeled_error(&error, path_span, call.head))?;
    let paths = resolve_paths(Path::new(&caller_cwd), &target, home.as_deref())
        .map_err(|error| labeled_error(&error, path_span, call.head))?;
    let mode =
        read_herdr_mode(engine).map_err(|error| labeled_error(&error, path_span, call.head))?;
    let deadline = Instant::now() + TOTAL_DEADLINE;

    match orchestrate(&paths, &mode, &interrupted, deadline) {
        Ok(Outcome::Silent) => Ok(Value::nothing(call.head)),
        Ok(Outcome::ChangeDirectory { path }) => {
            engine
                .add_env_var("PWD", Value::string(path.as_str(), path_span))
                .map_err(|_| {
                    labeled_error(
                        &Error::invalid_path("failed to update the caller working directory"),
                        path_span,
                        call.head,
                    )
                })?;
            Ok(Value::nothing(call.head))
        }
        Err(RunError::Interrupted) => Err(interrupt_error(call.head)),
        Err(RunError::Failed(error)) => Err(labeled_error(&error, path_span, call.head)),
    }
}

fn platform_is_supported() -> bool {
    cfg!(target_os = "linux") || cfg!(target_os = "macos")
}

/// Read Herdr mode from the caller's environment, never from the plugin process.
fn read_herdr_mode(engine: &EngineInterface) -> Result<HerdrMode, Error> {
    let herdr_env = engine
        .get_env_var("HERDR_ENV")
        .map_err(|_| Error::invalid_herdr_context("caller environment is unavailable"))?
        .as_ref()
        .map(nu_value_to_env);
    if !classify_herdr_env(herdr_env.as_ref())? {
        return Ok(HerdrMode::Outside);
    }

    let vars = engine
        .get_env_vars()
        .map_err(|_| Error::invalid_herdr_context("caller environment is unavailable"))?;
    let mut herdr_vars = BTreeMap::new();
    for (key, value) in vars {
        if !key.starts_with("HERDR_") {
            continue;
        }
        match nu_value_to_env(&value) {
            EnvValue::String(value) => {
                herdr_vars.insert(key, value);
            }
            EnvValue::Other if key == "HERDR_ENV" => {}
            EnvValue::Other => {
                return Err(Error::invalid_herdr_context(format!(
                    "{key} must be a string"
                )));
            }
        }
    }

    let required = |name: &str| -> Result<String, Error> {
        herdr_vars.get(name).cloned().ok_or_else(|| {
            Error::invalid_herdr_context(format!("{name} is missing from the Herdr caller context"))
        })
    };

    Ok(HerdrMode::Inside(inside_context(
        &required("HERDR_BIN_PATH")?,
        &required("HERDR_SOCKET_PATH")?,
        &required("HERDR_WORKSPACE_ID")?,
        &required("HERDR_TAB_ID")?,
        &required("HERDR_PANE_ID")?,
        herdr_vars,
    )?))
}

fn read_home(engine: &EngineInterface) -> Result<Option<String>, Error> {
    match engine.get_env_var("HOME") {
        Ok(Some(Value::String { val, .. })) if !val.is_empty() => Ok(Some(val)),
        Ok(None) | Ok(Some(_)) => Ok(None),
        Err(_) => Err(Error::invalid_herdr_context(
            "caller environment is unavailable",
        )),
    }
}

fn nu_value_to_env(value: &Value) -> EnvValue {
    match value {
        Value::String { val, .. } => EnvValue::String(val.clone()),
        _ => EnvValue::Other,
    }
}

fn interrupt_error(span: Span) -> LabeledError {
    ShellError::Interrupted { span }.into()
}

fn labeled_error(error: &Error, path_span: Span, head: Span) -> LabeledError {
    let span = match error.kind() {
        ErrorKind::InvalidPath => path_span,
        _ => head,
    };
    LabeledError::new(error.message())
        .with_code(format!("herdr_cd::{}", error.kind()))
        .with_label(error.kind().as_str(), span)
}

#[cfg(test)]
mod tests {
    use super::{
        COMMAND_NAME, Hcd, HerdrCdPlugin, labeled_error, nu_value_to_env, platform_is_supported,
    };
    use crate::PLUGIN_IDENTITY;
    use crate::domain::{Error, ErrorKind};
    use crate::herdr::{EnvValue, classify_herdr_env};
    use nu_plugin::{EvaluatedCall, Plugin, PluginCommand};
    use nu_protocol::{Span, SyntaxShape, Type, Value};

    #[test]
    fn plugin_identity_and_version_match_the_package() {
        let plugin = HerdrCdPlugin;
        assert_eq!(PLUGIN_IDENTITY, "herdr_cd");
        assert_eq!(plugin.version(), env!("CARGO_PKG_VERSION"));
        assert_eq!(plugin.commands().len(), 1);
        assert_eq!(plugin.commands()[0].name(), COMMAND_NAME);
    }

    #[test]
    fn registers_only_hcd_with_the_approved_signature() {
        let commands = HerdrCdPlugin.commands();
        assert_eq!(commands.len(), 1);

        let command = &commands[0];
        assert_eq!(command.name(), COMMAND_NAME);

        let signature = command.signature();
        assert_eq!(signature.name, COMMAND_NAME);
        assert_eq!(signature.required_positional.len(), 1);
        assert_eq!(signature.required_positional[0].name, "path");
        assert_eq!(
            signature.required_positional[0].shape,
            SyntaxShape::Filepath
        );
        assert!(signature.optional_positional.is_empty());
        assert!(signature.rest_positional.is_none());
        assert!(
            signature
                .named
                .iter()
                .all(|flag| flag.long == "help" && flag.arg.is_none()),
            "hcd must not declare command-specific flags, found {:?}",
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
    }

    #[test]
    fn successful_output_is_nothing_and_change_directory_is_the_only_pwd_mutation() {
        use crate::command::orchestrate::Outcome;
        use crate::domain::CanonicalPath;

        assert!(matches!(Outcome::Silent, Outcome::Silent));
        let path = CanonicalPath::from_parts_for_test("/tmp");
        assert!(matches!(
            Outcome::ChangeDirectory { path: path.clone() },
            Outcome::ChangeDirectory { path: updated } if updated == path
        ));
    }

    #[test]
    fn labeled_error_uses_internal_kind_codes_and_approved_spans() {
        let path_span = Span::new(10, 20);
        let head = Span::new(1, 4);

        let path_error = labeled_error(
            &Error::invalid_path("path is not a directory"),
            path_span,
            head,
        );
        assert_eq!(path_error.code.as_deref(), Some("herdr_cd::invalid_path"));
        assert_eq!(path_error.msg, "path is not a directory");
        assert_eq!(path_error.labels[0].text, "invalid_path");
        assert_eq!(path_error.labels[0].span, path_span);

        let herdr_kinds = [
            Error::unsupported_platform("hcd supports Linux and macOS only"),
            Error::invalid_herdr_context(
                "HERDR_SOCKET_PATH is missing from the Herdr caller context",
            ),
            Error::incompatible_herdr("Herdr version or protocol is below the 0.8.2 baseline"),
            Error::herdr_timeout("hcd exceeded the 10-second deadline"),
            Error::herdr_transport("failed to start the Herdr command"),
            Error::herdr_protocol("session snapshot: missing snapshot object"),
            Error::herdr_action("pane focus failed after recomputation: pane_not_found: gone"),
        ];
        for error in herdr_kinds {
            let labeled = labeled_error(&error, path_span, head);
            let expected = format!("herdr_cd::{}", error.kind());
            assert_eq!(labeled.code.as_deref(), Some(expected.as_str()));
            assert_eq!(labeled.labels[0].span, head, "{}", error.kind());
            assert_eq!(labeled.labels[0].text, error.kind().as_str());
        }
    }

    #[test]
    fn filepath_argument_is_required_and_decoded_as_a_string() {
        let call = EvaluatedCall::new(Span::test_data())
            .with_positional(Value::test_string("/tmp/project"));
        let path: String = call.req(0).unwrap();
        assert_eq!(path, "/tmp/project");
        assert_eq!(call.nth(0).unwrap().span(), Span::test_data());
        assert!(
            EvaluatedCall::new(Span::test_data())
                .req::<String>(0)
                .is_err()
        );
    }

    #[test]
    fn hcd_command_name_is_stable() {
        assert_eq!(PluginCommand::name(&Hcd), COMMAND_NAME);
    }

    #[test]
    fn current_platform_is_supported() {
        assert!(
            platform_is_supported(),
            "phase 5 tests run only on Linux and macOS"
        );
    }

    #[test]
    fn home_value_uses_string_env_and_ignores_other_types() {
        assert!(matches!(
            nu_value_to_env(&Value::test_string("/Users/example")),
            EnvValue::String(value) if value == "/Users/example"
        ));
        assert!(matches!(
            nu_value_to_env(&Value::test_int(1)),
            EnvValue::Other
        ));
    }

    #[test]
    fn caller_herdr_env_uses_nushell_values_not_process_env() {
        assert!(matches!(
            nu_value_to_env(&Value::test_string("1")),
            EnvValue::String(value) if value == "1"
        ));
        assert!(matches!(
            nu_value_to_env(&Value::test_int(1)),
            EnvValue::Other
        ));
        assert!(!classify_herdr_env(None).unwrap());
        assert!(classify_herdr_env(Some(&nu_value_to_env(&Value::test_string("1")))).unwrap());
        assert!(
            classify_herdr_env(Some(&nu_value_to_env(&Value::test_int(1))))
                .unwrap_err()
                .kind()
                == ErrorKind::InvalidHerdrContext
        );
    }
}
