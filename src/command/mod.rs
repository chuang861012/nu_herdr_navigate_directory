//! Nushell plugin identity, `hnd` signature, and command-boundary orchestration.

mod complete;
mod config;
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
    Category, DynamicSuggestion, Example, LabeledError, ShellError, Signature, Span, SyntaxShape,
    Type, Value, engine::ArgType,
};

use crate::domain::{CanonicalPath, Error, ErrorKind, ResolvedPaths, resolve_target};
use crate::herdr::{
    EnvValue, HerdrMode, RunError, classify_herdr_env, inside_context, run_bounded,
};

use config::read_command_config;
use orchestrate::{Outcome, TOTAL_DEADLINE, check, map_halt, orchestrate};

/// Caller-side Nushell engine operations used by one `hnd` invocation.
trait CallerEngine: Clone + Send + 'static {
    fn interrupted(&self) -> bool;
    fn current_dir(&self) -> Result<String, Error>;
    fn env_var(&self, name: &str) -> Result<Option<Value>, Error>;
    fn path_env_var(&self, name: &str) -> Result<Option<Value>, Error>;
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

    fn path_env_var(&self, name: &str) -> Result<Option<Value>, Error> {
        self.get_env_var(name)
            .map_err(|_| Error::invalid_path(format!("caller {name} is unavailable")))
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
            .map_err(|_| Error::invalid_configuration("plugin configuration is unavailable"))
    }
}

const COMMAND_NAME: &str = "hnd";

#[derive(Debug, Clone, PartialEq, Eq)]
enum TargetRequest {
    Home,
    Previous,
    Explicit(String),
}

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
        "With no path, hnd navigates to the caller's $env.HOME. A bare - selects $env.OLDPWD, falling back to the current directory when OLDPWD is absent; use ./- for a literal directory named -. Every directory change writes the canonical old directory to $env.OLDPWD before updating $env.PWD. Outside Herdr, hnd changes the calling pane's directory. Inside Herdr, it reuses an eligible pane, changes directory only for downward navigation, or creates a focused tab or workspace; focus and create actions leave PWD and OLDPWD unchanged. Configure reusable agent states with $env.config.plugins.herdr_navigate_directory.idle_agent_statuses; the default is [idle done]. Successful calls return nothing. Experimental opt-in dynamic completion can enrich the directory argument with live Herdr workspace and pane paths; it is not an action preview."
    }

    fn signature(&self) -> Signature {
        Signature::build(COMMAND_NAME)
            .optional(
                "path",
                SyntaxShape::Directory,
                "Directory to navigate to; omit for home or use - for the previous directory",
            )
            .input_output_type(Type::Nothing, Type::Nothing)
            .allow_variants_without_examples(true)
            .category(Category::FileSystem)
    }

    fn examples(&self) -> Vec<Example<'_>> {
        vec![
            Example {
                example: "hnd",
                description: "Navigate to the caller's home directory",
                result: None,
            },
            Example {
                example: "hnd -",
                description: "Navigate to the caller's previous directory",
                result: None,
            },
            Example {
                example: "hnd ./-",
                description: "Navigate to a literal child directory named -",
                result: None,
            },
        ]
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

    let request = decode_target_request(call)?;
    let path_span = call.nth(0).map(|value| value.span()).unwrap_or(call.head);
    let to_labeled = |error: Error| labeled_error(&error, path_span, call.head);
    let halt = || interrupted() || Instant::now() >= deadline;

    let engine_worker = engine.clone();
    let request_worker = request;
    let (paths, mode) = match run_bounded(&halt, move || {
        let paths = resolve_target_request(&engine_worker, &request_worker)?;
        let mode = read_herdr_mode(&engine_worker)?;
        Ok::<_, Error>((paths, mode))
    }) {
        Ok(Ok(pair)) => pair,
        Ok(Err(error)) => return Err(to_labeled(error)),
        Err(error) => return Err(fail(map_halt(error, &interrupted))),
    };
    check(&interrupted, deadline).map_err(&fail)?;

    let policy = match mode {
        HerdrMode::Outside => crate::domain::AgentIdlePolicy::default(),
        HerdrMode::Inside(_) => {
            let engine_worker = engine.clone();
            let head = call.head;
            match run_bounded(&halt, move || read_command_config(&engine_worker, head)) {
                Ok(Ok(config)) => config.idle_agent_policy,
                Ok(Err(error)) => {
                    return Err(labeled_error_at(&error.error, error.span));
                }
                Err(error) => return Err(fail(map_halt(error, &interrupted))),
            }
        }
    };
    check(&interrupted, deadline).map_err(&fail)?;

    match orchestrate(&paths, &mode, &policy, &interrupted, deadline) {
        Ok(outcome) => apply_outcome(
            engine,
            outcome,
            &paths,
            call.head,
            path_span,
            &halt,
            &interrupted,
        ),
        Err(error) => Err(fail(error)),
    }
}

fn decode_target_request(call: &EvaluatedCall) -> Result<TargetRequest, LabeledError> {
    match call.opt::<String>(0)? {
        None => Ok(TargetRequest::Home),
        Some(target) if target == "-" => Ok(TargetRequest::Previous),
        Some(target) => Ok(TargetRequest::Explicit(target)),
    }
}

fn resolve_target_request(
    engine: &impl CallerEngine,
    request: &TargetRequest,
) -> Result<ResolvedPaths, Error> {
    let caller_cwd_raw = engine.current_dir()?;
    let caller_cwd = CanonicalPath::directory(Path::new(&caller_cwd_raw))?;
    let target = match request {
        TargetRequest::Home => {
            let home = required_absolute_path_env(engine, "HOME")?;
            resolve_target(&caller_cwd, &home, None)?
        }
        TargetRequest::Previous => match optional_absolute_path_env(engine, "OLDPWD")? {
            Some(previous) => resolve_target(&caller_cwd, &previous, None)?,
            None => caller_cwd.clone(),
        },
        TargetRequest::Explicit(target) => {
            let home = if target == "~" || target.starts_with("~/") {
                read_home(engine)?
            } else {
                None
            };
            resolve_target(&caller_cwd, target, home.as_deref())?
        }
    };
    Ok(ResolvedPaths { caller_cwd, target })
}

fn apply_outcome(
    engine: &impl CallerEngine,
    outcome: Outcome,
    paths: &ResolvedPaths,
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
                .add_env_var(
                    "OLDPWD",
                    Value::string(paths.caller_cwd.as_str(), path_span),
                )
                .map_err(|error| labeled_error(&error, path_span, head))?;
            engine
                .add_env_var("PWD", Value::string(path.as_str(), path_span))
                .map_err(|_| {
                    labeled_error(
                        &Error::invalid_path(
                            "failed to update the caller working directory; OLDPWD may already have changed",
                        ),
                        path_span,
                        head,
                    )
                })?;
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
    match engine.path_env_var("HOME")? {
        Some(Value::String { val, .. }) if !val.is_empty() => Ok(Some(val)),
        _ => Ok(None),
    }
}

fn required_absolute_path_env(engine: &impl CallerEngine, name: &str) -> Result<String, Error> {
    optional_absolute_path_env(engine, name)?.ok_or_else(|| {
        Error::invalid_path(format!("{name} is missing from the caller environment"))
    })
}

fn optional_absolute_path_env(
    engine: &impl CallerEngine,
    name: &str,
) -> Result<Option<String>, Error> {
    let Some(value) = engine.path_env_var(name)? else {
        return Ok(None);
    };
    let Value::String { val, .. } = value else {
        return Err(Error::invalid_path(format!("{name} must be a string")));
    };
    if val.is_empty() {
        return Err(Error::invalid_path(format!("{name} must not be empty")));
    }
    if !Path::new(&val).is_absolute() {
        return Err(Error::invalid_path(format!(
            "{name} must be an absolute path"
        )));
    }
    Ok(Some(val))
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
    labeled_error_at(error, span)
}

fn labeled_error_at(error: &Error, span: Span) -> LabeledError {
    LabeledError::new(error.message())
        .with_code(format!("herdr_navigate_directory::{}", error.kind()))
        .with_label(error.kind().as_str(), span)
}

#[cfg(test)]
mod tests {
    use super::{
        COMMAND_NAME, CallerEngine, HerdrNavigateDirectoryPlugin, Hnd, TargetRequest,
        apply_outcome, decode_target_request, labeled_error, nu_value_to_env,
        platform_is_supported, resolve_target_request, run_hnd,
    };
    use crate::PLUGIN_IDENTITY;
    use crate::command::orchestrate::{Outcome, TOTAL_DEADLINE};
    use crate::domain::{CanonicalPath, Error, ErrorKind, ResolvedPaths};
    use crate::herdr::test_support::{TempDir, lock_cli, write_executable};
    use crate::herdr::{EnvValue, classify_herdr_env};
    use nu_plugin::{EvaluatedCall, Plugin, PluginCommand};
    use nu_protocol::{Span, SyntaxShape, Type, Value};
    use serde_json::json;
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
        assert!(signature.required_positional.is_empty());
        assert_eq!(signature.optional_positional.len(), 1);
        assert_eq!(signature.optional_positional[0].name, "path");
        assert_eq!(
            signature.optional_positional[0].shape,
            SyntaxShape::Directory
        );
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
        path_env_delays: HashMap<String, Duration>,
        path_env_failures: HashMap<String, Error>,
        pwd_delay: Duration,
        oldpwd_delay: Duration,
        write_failures: HashMap<String, Error>,
        env_writes: Arc<Mutex<Vec<(String, String)>>>,
        cwd_calls: Arc<AtomicUsize>,
        plugin_config: Result<Option<Value>, Error>,
        config_calls: Arc<AtomicUsize>,
        config_delay: Duration,
    }

    impl FakeEngine {
        fn outside(cwd: &str) -> Self {
            Self {
                cwd: cwd.to_string(),
                env: HashMap::new(),
                interrupted: Arc::new(AtomicBool::new(false)),
                interrupt_after: None,
                cwd_delay: Duration::ZERO,
                path_env_delays: HashMap::new(),
                path_env_failures: HashMap::new(),
                pwd_delay: Duration::ZERO,
                oldpwd_delay: Duration::ZERO,
                write_failures: HashMap::new(),
                env_writes: Arc::new(Mutex::new(Vec::new())),
                cwd_calls: Arc::new(AtomicUsize::new(0)),
                plugin_config: Ok(None),
                config_calls: Arc::new(AtomicUsize::new(0)),
                config_delay: Duration::ZERO,
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

        fn path_env_var(&self, name: &str) -> Result<Option<Value>, Error> {
            if let Some(delay) = self.path_env_delays.get(name) {
                thread::sleep(*delay);
            }
            if let Some(error) = self.path_env_failures.get(name) {
                return Err(error.clone());
            }
            Ok(self.env.get(name).cloned())
        }

        fn env_vars(&self) -> Result<HashMap<String, Value>, Error> {
            Ok(self.env.clone())
        }

        fn add_env_var(&self, name: &str, value: Value) -> Result<(), Error> {
            let delay = match name {
                "OLDPWD" => self.oldpwd_delay,
                "PWD" => self.pwd_delay,
                _ => Duration::ZERO,
            };
            if !delay.is_zero() {
                thread::sleep(delay);
            }
            if let Some(error) = self.write_failures.get(name) {
                return Err(error.clone());
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
            self.config_calls.fetch_add(1, Ordering::SeqCst);
            if !self.config_delay.is_zero() {
                thread::sleep(self.config_delay);
            }
            match &self.plugin_config {
                Ok(value) => Ok(value.clone()),
                Err(error) => Err(error.clone()),
            }
        }
    }

    fn test_call(path: &str) -> EvaluatedCall {
        EvaluatedCall::new(Span::test_data()).with_positional(Value::test_string(path))
    }

    fn test_call_without_path() -> EvaluatedCall {
        EvaluatedCall::new(Span::test_data())
    }

    fn inside_engine(cwd: &str, bin: &str, socket: &str) -> FakeEngine {
        let mut engine = FakeEngine::outside(cwd);
        for (name, value) in [
            ("HERDR_ENV", "1"),
            ("HERDR_BIN_PATH", bin),
            ("HERDR_SOCKET_PATH", socket),
            ("HERDR_WORKSPACE_ID", "w1"),
            ("HERDR_TAB_ID", "w1:t1"),
            ("HERDR_PANE_ID", "w1:p1"),
        ] {
            engine
                .env
                .insert(name.to_string(), Value::test_string(value));
        }
        engine
    }

    #[test]
    fn successful_output_is_nothing_and_only_change_directory_mutates_history() {
        let cwd = std::env::temp_dir();
        let cwd_str = cwd.to_str().expect("temp dir is UTF-8");
        let path = CanonicalPath::directory(&cwd).unwrap();
        let paths = ResolvedPaths {
            caller_cwd: path.clone(),
            target: path.clone(),
        };

        let silent = FakeEngine::outside(cwd_str);
        let value = apply_outcome(
            &silent,
            Outcome::Silent,
            &paths,
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
            &paths,
            Span::test_data(),
            Span::test_data(),
            &|| false,
            &|| false,
        )
        .unwrap();
        assert!(value.is_nothing());
        assert_eq!(
            changed.writes().as_slice(),
            [
                ("OLDPWD".to_string(), path.as_str().to_string()),
                ("PWD".to_string(), path.as_str().to_string())
            ]
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
            [
                ("OLDPWD".to_string(), path.as_str().to_string()),
                ("PWD".to_string(), path.as_str().to_string())
            ]
        );
    }

    #[test]
    fn target_requests_use_caller_home_previous_and_literal_dash_paths() {
        let dir = TempDir::new("target-request");
        let cwd = dir.path().join("cwd");
        let home = dir.path().join("home");
        let previous = dir.path().join("previous");
        let literal_dash = cwd.join("-");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&previous).unwrap();
        std::fs::create_dir_all(&literal_dash).unwrap();
        let home_link = dir.path().join("home-link");
        std::os::unix::fs::symlink(&home, &home_link).unwrap();

        let mut engine = FakeEngine::outside(cwd.to_str().unwrap());
        engine.env.insert(
            "HOME".into(),
            Value::test_string(home_link.to_str().unwrap()),
        );
        engine.env.insert(
            "OLDPWD".into(),
            Value::test_string(previous.to_str().unwrap()),
        );

        let cases = [
            (
                TargetRequest::Home,
                CanonicalPath::directory(&home).unwrap(),
            ),
            (
                TargetRequest::Previous,
                CanonicalPath::directory(&previous).unwrap(),
            ),
            (
                TargetRequest::Explicit("./-".into()),
                CanonicalPath::directory(&literal_dash).unwrap(),
            ),
            (
                TargetRequest::Explicit(literal_dash.to_str().unwrap().into()),
                CanonicalPath::directory(&literal_dash).unwrap(),
            ),
        ];
        for (request, expected) in cases {
            let paths = resolve_target_request(&engine, &request).unwrap();
            assert_eq!(paths.target, expected, "request {request:?}");
            assert_eq!(paths.caller_cwd, CanonicalPath::directory(&cwd).unwrap());
        }

        let implicit_home = resolve_target_request(&engine, &TargetRequest::Home).unwrap();
        let explicit_home =
            resolve_target_request(&engine, &TargetRequest::Explicit("~".into())).unwrap();
        assert_eq!(implicit_home, explicit_home);

        engine.env.remove("OLDPWD");
        let paths = resolve_target_request(&engine, &TargetRequest::Previous).unwrap();
        assert_eq!(paths.target, paths.caller_cwd);
    }

    #[test]
    fn implicit_and_previous_targets_fail_closed_with_their_approved_spans() {
        let dir = TempDir::new("target-errors");
        let cwd = dir.path().to_str().unwrap();
        let head = Span::new(1, 4);
        let argument = Span::new(10, 11);

        let invalid_values = [
            Value::string("", Span::test_data()),
            Value::string("relative", Span::test_data()),
            Value::test_int(7),
            Value::string(
                dir.path().join("missing").to_str().unwrap(),
                Span::test_data(),
            ),
            Value::string(
                dir.path().join("not-a-directory").to_str().unwrap(),
                Span::test_data(),
            ),
        ];
        std::fs::write(dir.path().join("not-a-directory"), "file").unwrap();
        for value in invalid_values {
            let mut home = FakeEngine::outside(cwd);
            home.env.insert("HOME".into(), value.clone());
            let error = run_hnd(
                &home,
                &EvaluatedCall::new(head),
                Instant::now() + TOTAL_DEADLINE,
            )
            .unwrap_err();
            assert_eq!(
                error.code.as_deref(),
                Some("herdr_navigate_directory::invalid_path")
            );
            assert_eq!(error.labels[0].span, head);
            assert!(home.writes().is_empty());

            let mut previous = FakeEngine::outside(cwd);
            previous.env.insert("OLDPWD".into(), value);
            let call = EvaluatedCall::new(head).with_positional(Value::string("-", argument));
            let error = run_hnd(&previous, &call, Instant::now() + TOTAL_DEADLINE).unwrap_err();
            assert_eq!(
                error.code.as_deref(),
                Some("herdr_navigate_directory::invalid_path")
            );
            assert_eq!(error.labels[0].span, argument);
            assert!(previous.writes().is_empty());
        }

        let missing_home = FakeEngine::outside(cwd);
        let error = run_hnd(
            &missing_home,
            &EvaluatedCall::new(head),
            Instant::now() + TOTAL_DEADLINE,
        )
        .unwrap_err();
        assert_eq!(
            error.code.as_deref(),
            Some("herdr_navigate_directory::invalid_path")
        );
        assert_eq!(error.labels[0].span, head);

        for (name, call) in [
            ("HOME", EvaluatedCall::new(head)),
            ("OLDPWD", {
                EvaluatedCall::new(head).with_positional(Value::string("-", argument))
            }),
        ] {
            let mut lookup_failure = FakeEngine::outside(cwd);
            lookup_failure.path_env_failures.insert(
                name.into(),
                Error::invalid_path("caller environment lookup failed"),
            );
            let error =
                run_hnd(&lookup_failure, &call, Instant::now() + TOTAL_DEADLINE).unwrap_err();
            assert_eq!(
                error.code.as_deref(),
                Some("herdr_navigate_directory::invalid_path")
            );
        }
    }

    #[test]
    fn no_path_and_previous_navigation_update_and_toggle_history() {
        let dir = TempDir::new("target-history");
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let first_path = CanonicalPath::directory(&first).unwrap();
        let second_path = CanonicalPath::directory(&second).unwrap();

        let mut home = FakeEngine::outside(first.to_str().unwrap());
        home.env
            .insert("HOME".into(), Value::test_string(second.to_str().unwrap()));
        run_hnd(
            &home,
            &test_call_without_path(),
            Instant::now() + TOTAL_DEADLINE,
        )
        .unwrap();
        assert_eq!(
            home.writes(),
            vec![
                ("OLDPWD".into(), first_path.as_str().into()),
                ("PWD".into(), second_path.as_str().into()),
            ]
        );

        let mut backward = FakeEngine::outside(second.to_str().unwrap());
        backward
            .env
            .insert("OLDPWD".into(), Value::test_string(first.to_str().unwrap()));
        run_hnd(&backward, &test_call("-"), Instant::now() + TOTAL_DEADLINE).unwrap();
        assert_eq!(
            backward.writes(),
            vec![
                ("OLDPWD".into(), second_path.as_str().into()),
                ("PWD".into(), first_path.as_str().into()),
            ]
        );
    }

    #[test]
    fn inside_herdr_home_and_missing_previous_use_the_existing_decision_tree() {
        let _cli = lock_cli();
        let dir = TempDir::new("inside-target-sources");
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let cwd = dir.path().to_str().unwrap();
        let snapshot = json!({
            "id": "cli:session:snapshot",
            "result": {
                "type": "session_snapshot",
                "snapshot": {
                    "version": "0.8.2",
                    "protocol": 20,
                    "focused_workspace_id": "w1",
                    "workspaces": [{
                        "workspace_id": "w1",
                        "number": 1,
                        "label": "main",
                        "focused": true,
                        "pane_count": 1,
                        "tab_count": 1,
                        "active_tab_id": "w1:t1",
                        "agent_status": "idle",
                        "worktree": {
                            "repo_key": "k",
                            "repo_name": "n",
                            "repo_root": cwd,
                            "checkout_path": cwd,
                            "is_linked_worktree": true
                        }
                    }],
                    "tabs": [{
                        "tab_id": "w1:t1",
                        "workspace_id": "w1",
                        "number": 1,
                        "label": "main",
                        "focused": true,
                        "pane_count": 1,
                        "agent_status": "idle"
                    }],
                    "panes": [{
                        "pane_id": "w1:p1",
                        "terminal_id": "term1",
                        "workspace_id": "w1",
                        "tab_id": "w1:t1",
                        "focused": true,
                        "agent_status": "idle",
                        "revision": 1,
                        "cwd": cwd,
                        "foreground_cwd": cwd
                    }],
                    "layouts": [{
                        "workspace_id": "w1",
                        "tab_id": "w1:t1",
                        "zoomed": false,
                        "area": {"x": 0, "y": 0, "width": 80, "height": 24},
                        "focused_pane_id": "w1:p1",
                        "panes": [],
                        "splits": []
                    }],
                    "agents": []
                }
            }
        });
        let current = json!({
            "id": "cli:pane:current",
            "result": {
                "type": "pane_current",
                "pane": {
                    "pane_id": "w1:p1",
                    "terminal_id": "term1",
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "focused": true,
                    "agent_status": "idle",
                    "revision": 1,
                    "foreground_cwd": cwd
                }
            }
        });
        let bin = write_executable(
            dir.path(),
            "herdr",
            &format!(
                "#!/bin/sh\nset -eu\ncase \"$1 $2\" in\n  \"api snapshot\") printf '%s\\n' '{}' ;;\n  \"pane current\") printf '%s\\n' '{}' ;;\n  *) exit 99 ;;\nesac\n",
                snapshot, current
            ),
        );

        let mut home_engine = inside_engine(cwd, bin.to_str().unwrap(), "/tmp/hnd-unused.sock");
        home_engine
            .env
            .insert("HOME".into(), Value::test_string(home.to_str().unwrap()));
        run_hnd(
            &home_engine,
            &test_call_without_path(),
            Instant::now() + TOTAL_DEADLINE,
        )
        .unwrap();
        assert_eq!(
            home_engine.writes(),
            vec![
                (
                    "OLDPWD".into(),
                    CanonicalPath::directory(dir.path())
                        .unwrap()
                        .as_str()
                        .into(),
                ),
                (
                    "PWD".into(),
                    CanonicalPath::directory(&home).unwrap().as_str().into(),
                ),
            ]
        );

        let previous_engine = inside_engine(cwd, bin.to_str().unwrap(), "/tmp/hnd-unused.sock");
        let value = run_hnd(
            &previous_engine,
            &test_call("-"),
            Instant::now() + TOTAL_DEADLINE,
        )
        .unwrap();
        assert!(value.is_nothing());
        assert!(previous_engine.writes().is_empty());
    }

    #[test]
    fn home_and_previous_reads_honor_deadline_and_interruption() {
        let dir = TempDir::new("target-read-halt");
        let cwd = dir.path().to_str().unwrap();
        for (name, call) in [
            ("HOME", test_call_without_path()),
            ("OLDPWD", test_call("-")),
        ] {
            let mut engine = FakeEngine::outside(cwd);
            engine
                .path_env_delays
                .insert(name.into(), Duration::from_millis(500));
            let started = Instant::now();
            let error =
                run_hnd(&engine, &call, Instant::now() + Duration::from_millis(30)).unwrap_err();
            assert_eq!(
                error.code.as_deref(),
                Some("herdr_navigate_directory::herdr_timeout")
            );
            assert!(started.elapsed() < Duration::from_millis(250));
            assert!(engine.writes().is_empty());

            let mut interrupted = FakeEngine::outside(cwd);
            interrupted
                .path_env_delays
                .insert(name.into(), Duration::from_millis(500));
            interrupted.interrupt_after = Some(Instant::now() + Duration::from_millis(30));
            let started = Instant::now();
            let error = run_hnd(&interrupted, &call, Instant::now() + TOTAL_DEADLINE).unwrap_err();
            assert!(error.msg.to_lowercase().contains("interrupt"));
            assert!(started.elapsed() < Duration::from_millis(250));
            assert!(interrupted.writes().is_empty());
        }
    }

    #[test]
    fn history_writes_are_one_ordered_critical_section_with_disclosed_partial_failure() {
        let dir = TempDir::new("history-mutation");
        let target_dir = dir.path().join("target");
        std::fs::create_dir(&target_dir).unwrap();
        let caller_cwd = CanonicalPath::directory(dir.path()).unwrap();
        let target = CanonicalPath::directory(&target_dir).unwrap();
        let paths = ResolvedPaths {
            caller_cwd: caller_cwd.clone(),
            target: target.clone(),
        };

        let halt_calls = AtomicUsize::new(0);
        let engine = FakeEngine::outside(dir.path().to_str().unwrap());
        apply_outcome(
            &engine,
            Outcome::ChangeDirectory {
                path: target.clone(),
            },
            &paths,
            Span::test_data(),
            Span::test_data(),
            &|| halt_calls.fetch_add(1, Ordering::SeqCst) > 0,
            &|| false,
        )
        .unwrap();
        assert_eq!(halt_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            engine.writes(),
            vec![
                ("OLDPWD".into(), caller_cwd.as_str().into()),
                ("PWD".into(), target.as_str().into()),
            ]
        );

        let mut oldpwd_failure = FakeEngine::outside(dir.path().to_str().unwrap());
        oldpwd_failure.write_failures.insert(
            "OLDPWD".into(),
            Error::invalid_path("failed to update the caller working directory"),
        );
        assert!(
            apply_outcome(
                &oldpwd_failure,
                Outcome::ChangeDirectory {
                    path: target.clone()
                },
                &paths,
                Span::test_data(),
                Span::test_data(),
                &|| false,
                &|| false,
            )
            .is_err()
        );
        assert!(oldpwd_failure.writes().is_empty());

        let mut pwd_failure = FakeEngine::outside(dir.path().to_str().unwrap());
        pwd_failure.write_failures.insert(
            "PWD".into(),
            Error::invalid_path("failed to update the caller working directory"),
        );
        let error = apply_outcome(
            &pwd_failure,
            Outcome::ChangeDirectory { path: target },
            &paths,
            Span::test_data(),
            Span::test_data(),
            &|| false,
            &|| false,
        )
        .unwrap_err();
        assert!(error.msg.contains("OLDPWD may already have changed"));
        assert_eq!(
            pwd_failure.writes(),
            vec![("OLDPWD".into(), caller_cwd.as_str().into())]
        );
    }

    #[test]
    fn outside_herdr_never_reads_or_validates_plugin_configuration() {
        let cwd = std::env::temp_dir();
        let cwd_str = cwd.to_str().expect("temp dir is UTF-8");
        let mut engine = FakeEngine::outside(cwd_str);
        engine.plugin_config = Err(Error::invalid_configuration("unreadable"));
        engine.config_delay = Duration::from_millis(500);
        let started = Instant::now();
        let value = run_hnd(
            &engine,
            &test_call(cwd_str),
            Instant::now() + TOTAL_DEADLINE,
        )
        .unwrap();
        assert!(value.is_nothing());
        assert_eq!(engine.config_calls.load(Ordering::SeqCst), 0);
        assert!(started.elapsed() < Duration::from_millis(250));
    }

    #[test]
    fn path_and_herdr_context_errors_precede_invalid_configuration() {
        let cwd = std::env::temp_dir();
        let cwd_str = cwd.to_str().expect("temp dir is UTF-8");
        let mut invalid_path = FakeEngine::outside(cwd_str);
        invalid_path.plugin_config = Ok(Some(Value::test_bool(true)));
        let missing = cwd.join("hnd-path-that-must-not-exist");
        let error = run_hnd(
            &invalid_path,
            &test_call(missing.to_str().unwrap()),
            Instant::now() + TOTAL_DEADLINE,
        )
        .unwrap_err();
        assert_eq!(
            error.code.as_deref(),
            Some("herdr_navigate_directory::invalid_path")
        );
        assert_eq!(invalid_path.config_calls.load(Ordering::SeqCst), 0);

        let mut invalid_context = FakeEngine::outside(cwd_str);
        invalid_context
            .env
            .insert("HERDR_ENV".into(), Value::test_string("1"));
        invalid_context.plugin_config = Ok(Some(Value::test_bool(true)));
        let error = run_hnd(
            &invalid_context,
            &test_call(cwd_str),
            Instant::now() + TOTAL_DEADLINE,
        )
        .unwrap_err();
        assert_eq!(
            error.code.as_deref(),
            Some("herdr_navigate_directory::invalid_herdr_context")
        );
        assert_eq!(invalid_context.config_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn invalid_inside_configuration_is_read_once_and_uses_its_own_span() {
        let _cli = lock_cli();
        let dir = TempDir::new("command-config");
        let bin = write_executable(dir.path(), "herdr", "#!/bin/sh\nexit 99\n");
        let cwd = dir.path().to_str().unwrap();
        let mut engine = inside_engine(cwd, bin.to_str().unwrap(), "/tmp/hnd-unused.sock");
        let config_span = Span::new(40, 50);
        engine.plugin_config = Ok(Some(Value::bool(true, config_span)));
        let head = Span::new(1, 4);
        let call = EvaluatedCall::new(head).with_positional(Value::string(cwd, Span::new(10, 20)));
        let error = run_hnd(&engine, &call, Instant::now() + TOTAL_DEADLINE).unwrap_err();
        assert_eq!(
            error.code.as_deref(),
            Some("herdr_navigate_directory::invalid_configuration")
        );
        assert_eq!(error.labels[0].span, config_span);
        assert_eq!(engine.config_calls.load(Ordering::SeqCst), 1);
        assert!(engine.writes().is_empty());

        let mut unreadable = inside_engine(cwd, bin.to_str().unwrap(), "/tmp/hnd-unused.sock");
        unreadable.plugin_config = Err(Error::invalid_herdr_context("lookup failed"));
        let error = run_hnd(&unreadable, &call, Instant::now() + TOTAL_DEADLINE).unwrap_err();
        assert_eq!(
            error.code.as_deref(),
            Some("herdr_navigate_directory::invalid_configuration")
        );
        assert_eq!(error.labels[0].span, head);
        assert_eq!(unreadable.config_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn config_lookup_honors_total_deadline_and_interruption() {
        let _cli = lock_cli();
        let dir = TempDir::new("command-config-halt");
        let bin = write_executable(dir.path(), "herdr", "#!/bin/sh\nexit 99\n");
        let cwd = dir.path().to_str().unwrap();

        let mut timeout = inside_engine(cwd, bin.to_str().unwrap(), "/tmp/hnd-unused.sock");
        timeout.config_delay = Duration::from_millis(500);
        let started = Instant::now();
        let error = run_hnd(
            &timeout,
            &test_call(cwd),
            Instant::now() + Duration::from_millis(30),
        )
        .unwrap_err();
        assert_eq!(
            error.code.as_deref(),
            Some("herdr_navigate_directory::herdr_timeout")
        );
        assert!(started.elapsed() < Duration::from_millis(250));
        assert_eq!(timeout.config_calls.load(Ordering::SeqCst), 1);

        let mut interrupted = inside_engine(cwd, bin.to_str().unwrap(), "/tmp/hnd-unused.sock");
        interrupted.config_delay = Duration::from_millis(500);
        interrupted.interrupt_after = Some(Instant::now() + Duration::from_millis(30));
        let started = Instant::now();
        let error = run_hnd(
            &interrupted,
            &test_call(cwd),
            Instant::now() + TOTAL_DEADLINE,
        )
        .unwrap_err();
        assert!(error.msg.to_lowercase().contains("interrupt"));
        assert!(started.elapsed() < Duration::from_millis(250));
        assert_eq!(interrupted.config_calls.load(Ordering::SeqCst), 1);
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
    fn halted_change_directory_does_not_late_write_history() {
        let cwd = std::env::temp_dir();
        let cwd_str = cwd.to_str().expect("temp dir is UTF-8");
        let path = CanonicalPath::directory(&cwd).unwrap();
        let paths = ResolvedPaths {
            caller_cwd: path.clone(),
            target: path.clone(),
        };
        let mut engine = FakeEngine::outside(cwd_str);
        engine.pwd_delay = Duration::from_millis(80);

        let started = Instant::now();
        let error = apply_outcome(
            &engine,
            Outcome::ChangeDirectory { path },
            &paths,
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
    fn directory_argument_decodes_home_previous_and_explicit_requests() {
        let call = EvaluatedCall::new(Span::test_data())
            .with_positional(Value::test_string("/tmp/project"));
        assert_eq!(
            decode_target_request(&call).unwrap(),
            TargetRequest::Explicit("/tmp/project".into())
        );
        assert_eq!(call.nth(0).unwrap().span(), Span::test_data());
        assert_eq!(
            decode_target_request(&EvaluatedCall::new(Span::test_data())).unwrap(),
            TargetRequest::Home
        );
        assert_eq!(
            decode_target_request(
                &EvaluatedCall::new(Span::test_data()).with_positional(Value::test_string("-"))
            )
            .unwrap(),
            TargetRequest::Previous
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
    fn environment_value_conversion_distinguishes_strings() {
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
