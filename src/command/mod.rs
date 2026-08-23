//! Nushell plugin identity, `hnd` signature, and command-boundary orchestration.

mod complete;
mod display;
mod orchestrate;
mod prefix;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Instant;

use nu_plugin::{
    DynamicCompletionCall, EngineInterface, EvaluatedCall, Plugin, PluginCommand,
    SimplePluginCommand,
};
use nu_protocol::{
    Category, DynamicSuggestion, LabeledError, ShellError, Signature, Span, SyntaxShape, Type,
    Value, engine::ArgType,
};

use crate::domain::{Error, ErrorKind, resolve_paths};
use crate::herdr::{
    EnvValue, HerdrMode, RunError, classify_herdr_env, inside_context, run_bounded,
};

use orchestrate::{Outcome, TOTAL_DEADLINE, check, map_halt, orchestrate};

/// Caller-side Nushell engine operations used by one `hnd` invocation.
trait CallerEngine: Clone + Send + 'static {
    fn interrupted(&self) -> bool;
    fn current_dir(&self) -> Result<String, Error>;
    fn env_var(&self, name: &str) -> Result<Option<Value>, Error>;
    fn env_vars(&self) -> Result<HashMap<String, Value>, Error>;
    fn add_env_var(&self, name: &str, value: Value) -> Result<(), Error>;
    fn plugin_config(&self) -> Result<Option<Value>, Error>;
}

impl CallerEngine for EngineInterface {
    fn interrupted(&self) -> bool {
        self.signals().interrupted()
    }

    fn current_dir(&self) -> Result<String, Error> {
        self.get_current_dir()
            .map_err(|_| Error::invalid_path("caller working directory is unavailable"))
    }

    fn env_var(&self, name: &str) -> Result<Option<Value>, Error> {
        self.get_env_var(name)
            .map_err(|_| Error::invalid_herdr_context("caller environment is unavailable"))
    }

    fn env_vars(&self) -> Result<HashMap<String, Value>, Error> {
        self.get_env_vars()
            .map_err(|_| Error::invalid_herdr_context("caller environment is unavailable"))
    }

    fn add_env_var(&self, name: &str, value: Value) -> Result<(), Error> {
        EngineInterface::add_env_var(self, name, value)
            .map_err(|_| Error::invalid_path("failed to update the caller working directory"))
    }

    fn plugin_config(&self) -> Result<Option<Value>, Error> {
        self.get_plugin_config()
            .map_err(|_| Error::invalid_herdr_context("plugin configuration is unavailable"))
    }
}

const COMMAND_NAME: &str = "hnd";

/// Plugin process that registers exactly one public command, `hnd`.
pub struct HerdrNavigateDirectoryPlugin;

struct Hnd;

impl Plugin for HerdrNavigateDirectoryPlugin {
    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").into()
    }

    fn commands(&self) -> Vec<Box<dyn PluginCommand<Plugin = Self>>> {
        vec![Box::new(Hnd)]
    }
}

impl SimplePluginCommand for Hnd {
    type Plugin = HerdrNavigateDirectoryPlugin;

    fn name(&self) -> &str {
        COMMAND_NAME
    }

    fn description(&self) -> &str {
        "Change directory, reusing an idle Herdr pane when possible"
    }

    fn extra_description(&self) -> &str {
        "Outside Herdr, hnd updates $env.PWD. Inside Herdr, it reuses an idle pane, changes directory only for downward navigation, or creates a focused tab or workspace. Successful calls return nothing. Experimental opt-in dynamic completion can enrich the directory argument with live Herdr workspace and pane paths; it never changes execution behavior."
    }

    fn signature(&self) -> Signature {
        Signature::build(COMMAND_NAME)
            .required("path", SyntaxShape::Directory, "Directory to navigate to")
            .input_output_type(Type::Nothing, Type::Nothing)
            .allow_variants_without_examples(true)
            .category(Category::FileSystem)
    }

    #[expect(deprecated, reason = "forwarding experimental status")]
    fn get_dynamic_completion(
        &self,
        _plugin: &HerdrNavigateDirectoryPlugin,
        engine: &EngineInterface,
        call: DynamicCompletionCall,
        arg_type: ArgType,
        _experimental: nu_protocol::engine::ExperimentalMarker,
    ) -> Option<Vec<DynamicSuggestion>> {
        complete::complete_path_argument(engine, call, arg_type)
    }

    fn run(
        &self,
        _plugin: &HerdrNavigateDirectoryPlugin,
        engine: &EngineInterface,
        call: &EvaluatedCall,
        _input: &Value,
    ) -> Result<Value, LabeledError> {
        run_hnd(engine, call, Instant::now() + TOTAL_DEADLINE)
    }
}

fn run_hnd(
    engine: &impl CallerEngine,
    call: &EvaluatedCall,
    deadline: Instant,
) -> Result<Value, LabeledError> {
    let interrupted = || engine.interrupted();
    let fail = |error: RunError| map_run_error(error, call);
    check(&interrupted, deadline).map_err(&fail)?;
    if !platform_is_supported() {
        return Err(labeled_error(
            &Error::unsupported_platform("hnd supports Linux and macOS only"),
            call.head,
            call.head,
        ));
    }

    let target: String = call.req(0)?;
    let path_span = call.nth(0).map(|value| value.span()).unwrap_or(call.head);
    let to_labeled = |error: Error| labeled_error(&error, path_span, call.head);
    let halt = || interrupted() || Instant::now() >= deadline;

    let engine_worker = engine.clone();
    let target_worker = target;
    let (paths, mode) = match run_bounded(&halt, move || {
        let caller_cwd = engine_worker.current_dir()?;
        let home = read_home(&engine_worker)?;
        let paths = resolve_paths(Path::new(&caller_cwd), &target_worker, home.as_deref())?;
        let mode = read_herdr_mode(&engine_worker)?;
        Ok::<_, Error>((paths, mode))
    }) {
        Ok(Ok(pair)) => pair,
        Ok(Err(error)) => return Err(to_labeled(error)),
        Err(error) => return Err(fail(map_halt(error, &interrupted))),
    };
    check(&interrupted, deadline).map_err(&fail)?;

    match orchestrate(&paths, &mode, &interrupted, deadline) {
        Ok(outcome) => apply_outcome(engine, outcome, call.head, path_span, &halt, &interrupted),
        Err(error) => Err(fail(error)),
    }
}

fn apply_outcome(
    engine: &impl CallerEngine,
    outcome: Outcome,
    head: Span,
    path_span: Span,
    halt: &dyn Fn() -> bool,
    interrupted: &dyn Fn() -> bool,
) -> Result<Value, LabeledError> {
    match outcome {
        Outcome::Silent => Ok(Value::nothing(head)),
        Outcome::ChangeDirectory { path } => {
            if halt() {
                return Err(map_run_error_parts(
                    map_halt(RunError::Interrupted, interrupted),
                    path_span,
                    head,
                ));
            }
            engine
                .add_env_var("PWD", Value::string(path.as_str(), path_span))
                .map_err(|error| labeled_error(&error, path_span, head))?;
            Ok(Value::nothing(head))
        }
    }
}

fn map_run_error_parts(error: RunError, path_span: Span, head: Span) -> LabeledError {
    match error {
        RunError::Interrupted => interrupt_error(head),
        RunError::Failed(error) => labeled_error(&error, path_span, head),
    }
}

fn map_run_error(error: RunError, call: &EvaluatedCall) -> LabeledError {
    let path_span = call.nth(0).map(|value| value.span()).unwrap_or(call.head);
    map_run_error_parts(error, path_span, call.head)
}

fn platform_is_supported() -> bool {
    cfg!(target_os = "linux") || cfg!(target_os = "macos")
}

/// Read Herdr mode from the caller's environment, never from the plugin process.
fn read_herdr_mode(engine: &impl CallerEngine) -> Result<HerdrMode, Error> {
    let herdr_env = engine.env_var("HERDR_ENV")?.as_ref().map(nu_value_to_env);
    if !classify_herdr_env(herdr_env.as_ref())? {
        return Ok(HerdrMode::Outside);
    }

    let vars = engine.env_vars()?;
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

fn read_home(engine: &impl CallerEngine) -> Result<Option<String>, Error> {
    match engine.env_var("HOME")? {
        Some(Value::String { val, .. }) if !val.is_empty() => Ok(Some(val)),
        _ => Ok(None),
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
        .with_code(format!("herdr_navigate_directory::{}", error.kind()))
        .with_label(error.kind().as_str(), span)
}

#[cfg(test)]
mod tests {
    use super::{
        COMMAND_NAME, CallerEngine, HerdrNavigateDirectoryPlugin, Hnd, apply_outcome,
        labeled_error, nu_value_to_env, platform_is_supported, run_hnd,
    };
    use crate::PLUGIN_IDENTITY;
    use crate::command::orchestrate::{Outcome, TOTAL_DEADLINE};
    use crate::domain::{CanonicalPath, Error, ErrorKind};
    use crate::herdr::{EnvValue, classify_herdr_env};
    use nu_plugin::{EvaluatedCall, Plugin, PluginCommand};
    use nu_protocol::{Span, SyntaxShape, Type, Value};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn plugin_identity_and_version_match_the_package() {
        let plugin = HerdrNavigateDirectoryPlugin;
        assert_eq!(PLUGIN_IDENTITY, "herdr_navigate_directory");
        assert_eq!(plugin.version(), env!("CARGO_PKG_VERSION"));
        assert_eq!(plugin.commands().len(), 1);
        assert_eq!(plugin.commands()[0].name(), COMMAND_NAME);
    }

    #[test]
    fn registers_only_hnd_with_the_approved_signature() {
        let commands = HerdrNavigateDirectoryPlugin.commands();
        assert_eq!(commands.len(), 1);

        let command = &commands[0];
        assert_eq!(command.name(), COMMAND_NAME);

        let signature = command.signature();
        assert_eq!(signature.name, COMMAND_NAME);
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
    }

    #[derive(Clone)]
    struct FakeEngine {
        cwd: String,
        env: HashMap<String, Value>,
        interrupted: Arc<AtomicBool>,
        interrupt_after: Option<Instant>,
        cwd_delay: Duration,
        pwd_delay: Duration,
        env_writes: Arc<Mutex<Vec<(String, String)>>>,
        cwd_calls: Arc<AtomicUsize>,
        plugin_config: Result<Option<Value>, Error>,
    }

    impl FakeEngine {
        fn outside(cwd: &str) -> Self {
            Self {
                cwd: cwd.to_string(),
                env: HashMap::new(),
                interrupted: Arc::new(AtomicBool::new(false)),
                interrupt_after: None,
                cwd_delay: Duration::ZERO,
                pwd_delay: Duration::ZERO,
                env_writes: Arc::new(Mutex::new(Vec::new())),
                cwd_calls: Arc::new(AtomicUsize::new(0)),
                plugin_config: Ok(None),
            }
        }

        fn writes(&self) -> Vec<(String, String)> {
            self.env_writes.lock().expect("env writes").clone()
        }
    }

    impl CallerEngine for FakeEngine {
        fn interrupted(&self) -> bool {
            if self.interrupted.load(Ordering::Relaxed) {
                return true;
            }
            if self.interrupt_after.is_some_and(|at| Instant::now() >= at) {
                self.interrupted.store(true, Ordering::Relaxed);
                true
            } else {
                false
            }
        }

        fn current_dir(&self) -> Result<String, Error> {
            self.cwd_calls.fetch_add(1, Ordering::SeqCst);
            if !self.cwd_delay.is_zero() {
                thread::sleep(self.cwd_delay);
            }
            Ok(self.cwd.clone())
        }

        fn env_var(&self, name: &str) -> Result<Option<Value>, Error> {
            Ok(self.env.get(name).cloned())
        }

        fn env_vars(&self) -> Result<HashMap<String, Value>, Error> {
            Ok(self.env.clone())
        }

        fn add_env_var(&self, name: &str, value: Value) -> Result<(), Error> {
            if !self.pwd_delay.is_zero() {
                thread::sleep(self.pwd_delay);
            }
            let rendered = match &value {
                Value::String { val, .. } => val.clone(),
                other => format!("{other:?}"),
            };
            self.env_writes
                .lock()
                .expect("env writes")
                .push((name.to_string(), rendered));
            Ok(())
        }

        fn plugin_config(&self) -> Result<Option<Value>, Error> {
            match &self.plugin_config {
                Ok(value) => Ok(value.clone()),
                Err(error) => Err(error.clone()),
            }
        }
    }

    fn test_call(path: &str) -> EvaluatedCall {
        EvaluatedCall::new(Span::test_data()).with_positional(Value::test_string(path))
    }

    #[test]
    fn successful_output_is_nothing_and_change_directory_is_the_only_pwd_mutation() {
        let cwd = std::env::temp_dir();
        let cwd_str = cwd.to_str().expect("temp dir is UTF-8");
        let path = CanonicalPath::directory(&cwd).unwrap();

        let silent = FakeEngine::outside(cwd_str);
        let value = apply_outcome(
            &silent,
            Outcome::Silent,
            Span::test_data(),
            Span::test_data(),
            &|| false,
            &|| false,
        )
        .unwrap();
        assert!(value.is_nothing());
        assert!(silent.writes().is_empty());

        let changed = FakeEngine::outside(cwd_str);
        let value = apply_outcome(
            &changed,
            Outcome::ChangeDirectory { path: path.clone() },
            Span::test_data(),
            Span::test_data(),
            &|| false,
            &|| false,
        )
        .unwrap();
        assert!(value.is_nothing());
        assert_eq!(
            changed.writes().as_slice(),
            [("PWD".to_string(), path.as_str().to_string())]
        );

        let engine = FakeEngine::outside(cwd_str);
        let value = run_hnd(
            &engine,
            &test_call(cwd_str),
            Instant::now() + TOTAL_DEADLINE,
        )
        .unwrap();
        assert!(value.is_nothing());
        assert_eq!(
            engine.writes().as_slice(),
            [("PWD".to_string(), path.as_str().to_string())]
        );
    }

    #[test]
    fn total_deadline_is_a_hard_maximum_for_blocking_lookup() {
        let cwd = std::env::temp_dir();
        let cwd_str = cwd.to_str().expect("temp dir is UTF-8");

        let expired = FakeEngine::outside(cwd_str);
        let deadline = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("clock has elapsed at least one second");
        let error = run_hnd(&expired, &test_call(cwd_str), deadline).unwrap_err();
        assert_eq!(
            error.code.as_deref(),
            Some("herdr_navigate_directory::herdr_timeout")
        );
        assert_eq!(expired.cwd_calls.load(Ordering::SeqCst), 0);
        assert!(expired.writes().is_empty());

        let mut slow = FakeEngine::outside(cwd_str);
        slow.cwd_delay = Duration::from_millis(500);
        let started = Instant::now();
        let error = run_hnd(
            &slow,
            &test_call(cwd_str),
            Instant::now() + Duration::from_millis(30),
        )
        .unwrap_err();
        assert_eq!(
            error.code.as_deref(),
            Some("herdr_navigate_directory::herdr_timeout")
        );
        // Hosted macOS timers can overshoot a 10ms poll by tens of milliseconds.
        // The bound still proves the 500ms lookup was not waited out.
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "deadline must abort before the blocking lookup returns, elapsed {:?}",
            started.elapsed()
        );
        assert!(slow.writes().is_empty());

        let mut blocked = FakeEngine::outside(cwd_str);
        blocked.cwd_delay = Duration::from_millis(500);
        blocked.interrupt_after = Some(Instant::now() + Duration::from_millis(30));
        let started = Instant::now();
        let error = run_hnd(
            &blocked,
            &test_call(cwd_str),
            Instant::now() + TOTAL_DEADLINE,
        )
        .unwrap_err();
        assert!(
            error.msg.to_lowercase().contains("interrupt"),
            "expected interruption, got {error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "interruption must abort before the blocking lookup returns, elapsed {:?}",
            started.elapsed()
        );
        assert!(blocked.writes().is_empty());
    }

    #[test]
    fn halted_change_directory_does_not_late_write_pwd() {
        let cwd = std::env::temp_dir();
        let cwd_str = cwd.to_str().expect("temp dir is UTF-8");
        let path = CanonicalPath::directory(&cwd).unwrap();
        let mut engine = FakeEngine::outside(cwd_str);
        engine.pwd_delay = Duration::from_millis(80);

        let started = Instant::now();
        let error = apply_outcome(
            &engine,
            Outcome::ChangeDirectory { path },
            Span::test_data(),
            Span::test_data(),
            &|| true,
            &|| false,
        )
        .unwrap_err();
        assert_eq!(
            error.code.as_deref(),
            Some("herdr_navigate_directory::herdr_timeout")
        );
        assert!(
            started.elapsed() < Duration::from_millis(40),
            "PWD mutation must not be dispatched after halt, elapsed {:?}",
            started.elapsed()
        );
        thread::sleep(Duration::from_millis(120));
        assert!(
            engine.writes().is_empty(),
            "abandoned timeout must not complete a delayed PWD write, got {:?}",
            engine.writes()
        );
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
        assert_eq!(
            path_error.code.as_deref(),
            Some("herdr_navigate_directory::invalid_path")
        );
        assert_eq!(path_error.msg, "path is not a directory");
        assert_eq!(path_error.labels[0].text, "invalid_path");
        assert_eq!(path_error.labels[0].span, path_span);

        let herdr_kinds = [
            Error::unsupported_platform("hnd supports Linux and macOS only"),
            Error::invalid_herdr_context(
                "HERDR_SOCKET_PATH is missing from the Herdr caller context",
            ),
            Error::incompatible_herdr("Herdr version or protocol is below the 0.8.2 baseline"),
            Error::herdr_timeout("hnd exceeded the 10-second deadline"),
            Error::herdr_transport("failed to start the Herdr command"),
            Error::herdr_protocol("session snapshot: missing snapshot object"),
            Error::herdr_action("pane focus failed after recomputation: pane_not_found: gone"),
        ];
        for error in herdr_kinds {
            let labeled = labeled_error(&error, path_span, head);
            let expected = format!("herdr_navigate_directory::{}", error.kind());
            assert_eq!(labeled.code.as_deref(), Some(expected.as_str()));
            assert_eq!(labeled.labels[0].span, head, "{}", error.kind());
            assert_eq!(labeled.labels[0].text, error.kind().as_str());
        }
    }

    #[test]
    fn directory_argument_is_required_and_decoded_as_a_string() {
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
    fn hnd_command_name_is_stable() {
        assert_eq!(PluginCommand::name(&Hnd), COMMAND_NAME);
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
