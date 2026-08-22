//! Nushell plugin identity, `hcd` signature, and command-boundary errors.

use std::collections::BTreeMap;

use nu_plugin::{EngineInterface, EvaluatedCall, Plugin, PluginCommand, SimplePluginCommand};
use nu_protocol::{Category, LabeledError, Signature, Span, SyntaxShape, Type, Value};

use crate::domain::{Error, ErrorKind};
use crate::herdr::{EnvValue, HerdrMode, classify_herdr_env, inside_context};

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
        "Navigation is not implemented yet. Successful calls will later return nothing."
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
        _engine: &EngineInterface,
        call: &EvaluatedCall,
        _input: &Value,
    ) -> Result<Value, LabeledError> {
        Err(not_implemented(call.head))
    }
}

/// Read Herdr mode from the caller's environment, never from the plugin process.
#[allow(dead_code)]
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

fn nu_value_to_env(value: &Value) -> EnvValue {
    match value {
        Value::String { val, .. } => EnvValue::String(val.clone()),
        _ => EnvValue::Other,
    }
}

fn not_implemented(span: Span) -> LabeledError {
    LabeledError::new("hcd is not implemented yet")
        .with_label("navigation behavior will be added in later phases", span)
}

#[cfg_attr(not(test), allow(dead_code))]
fn labeled_error(kind: ErrorKind, message: impl Into<String>, span: Span) -> LabeledError {
    LabeledError::new(message)
        .with_code(format!("herdr_cd::{kind}"))
        .with_label(kind.as_str(), span)
}

#[cfg(test)]
mod tests {
    use super::{
        COMMAND_NAME, Hcd, HerdrCdPlugin, labeled_error, not_implemented, nu_value_to_env,
    };
    use crate::PLUGIN_IDENTITY;
    use crate::domain::ErrorKind;
    use crate::herdr::{EnvValue, classify_herdr_env};
    use nu_plugin::{Plugin, PluginCommand};
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
    fn hcd_reports_not_implemented_without_navigation() {
        let error = not_implemented(Span::test_data());
        assert_eq!(error.msg, "hcd is not implemented yet");
        assert!(error.code.is_none());
    }

    #[test]
    fn labeled_error_uses_internal_kind_codes() {
        let error = labeled_error(
            ErrorKind::InvalidPath,
            "path is not a directory",
            Span::test_data(),
        );
        assert_eq!(error.code.as_deref(), Some("herdr_cd::invalid_path"));
        assert_eq!(error.msg, "path is not a directory");
        assert_eq!(error.labels[0].text, "invalid_path");
    }

    #[test]
    fn hcd_command_name_is_stable() {
        assert_eq!(PluginCommand::name(&Hcd), COMMAND_NAME);
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
