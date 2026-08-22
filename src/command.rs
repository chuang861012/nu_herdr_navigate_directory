//! Nushell plugin identity, `hcd` signature, and command-boundary errors.

use nu_plugin::{EngineInterface, EvaluatedCall, Plugin, PluginCommand, SimplePluginCommand};
use nu_protocol::{Category, LabeledError, Signature, Span, SyntaxShape, Type, Value};

use crate::domain::ErrorKind;

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
    use super::{COMMAND_NAME, Hcd, HerdrCdPlugin, labeled_error, not_implemented};
    use crate::PLUGIN_IDENTITY;
    use crate::domain::ErrorKind;
    use nu_plugin::{Plugin, PluginCommand};
    use nu_protocol::{Span, SyntaxShape, Type};

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
}
