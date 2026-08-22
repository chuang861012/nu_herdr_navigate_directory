//! Bounded synchronous Herdr CLI execution.

use std::io::{self, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::context::InsideContext;
use crate::domain::Error;

/// Per-read-operation deadline for snapshot, caller lookup, pane inspection, and focus.
pub(crate) const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// Tab or workspace creation deadline.
pub(crate) const CREATE_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum accepted CLI stdout or stderr payload.
pub(crate) const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(crate) const START_FAILED: &str = "failed to start the Herdr command";

/// CLI runner failure, including interruption that must surface as Nushell's interrupt error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunError {
    Failed(Error),
    Interrupted,
}

impl From<Error> for RunError {
    fn from(error: Error) -> Self {
        Self::Failed(error)
    }
}

#[derive(Debug)]
pub(crate) struct CliOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: ExitStatus,
}

/// Run the validated Herdr binary with separate argv values and no shell.
pub(crate) fn run(
    context: &InsideContext,
    args: &[&str],
    timeout: Duration,
    interrupted: impl Fn() -> bool,
) -> Result<CliOutput, RunError> {
    let mut command = Command::new(&context.bin);
    command.args(args);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    apply_herdr_env(&mut command, context);
    // Put the child in its own process group so timeout/interrupt can reap
    // descendants that still hold stdout/stderr (for example `sh` + `sleep`).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command
        .spawn()
        .map_err(|_| Error::herdr_transport(START_FAILED))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::herdr_transport("Herdr command stdout was not captured"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::herdr_transport("Herdr command stderr was not captured"))?;

    let stdout_rx = spawn_reader(stdout);
    let stderr_rx = spawn_reader(stderr);

    let deadline = Instant::now() + timeout;
    let mut status = None;
    let mut stdout_read = None;
    let mut stderr_read = None;
    loop {
        if interrupted() {
            terminate(&mut child);
            return Err(RunError::Interrupted);
        }
        if Instant::now() >= deadline {
            terminate(&mut child);
            return Err(Error::herdr_timeout("Herdr command timed out").into());
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(exit)) => status = Some(exit),
                Ok(None) => {}
                Err(_) => {
                    terminate(&mut child);
                    return Err(
                        Error::herdr_transport("failed to wait for the Herdr command").into(),
                    );
                }
            }
        }
        take_reader(&stdout_rx, &mut stdout_read)?;
        take_reader(&stderr_rx, &mut stderr_read)?;
        if status.is_some() && stdout_read.is_some() && stderr_read.is_some() {
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }

    let stdout = stdout_read.expect("stdout reader completed");
    let stderr = stderr_read.expect("stderr reader completed");
    let status = status.expect("child exited");
    if stdout.exceeded || stderr.exceeded {
        return Err(Error::herdr_protocol("Herdr response exceeded the 4 MiB limit").into());
    }

    Ok(CliOutput {
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        status,
    })
}

/// Run read-only blocking work on a helper thread so the caller can honor halt.
///
/// The helper is abandoned if halt becomes true before it finishes. The syscall
/// may continue until the kernel returns; `hcd` itself does not wait. Do not use
/// this for caller mutations such as `$env.PWD`; an abandoned worker must not
/// be able to complete a write after the command has already failed.
pub(crate) fn run_bounded<T: Send + 'static>(
    halt: &dyn Fn() -> bool,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, RunError> {
    if halt() {
        return Err(RunError::Interrupted);
    }
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(work());
    });
    loop {
        match rx.recv_timeout(POLL_INTERVAL) {
            Ok(value) => return Ok(value),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if halt() {
                    return Err(RunError::Interrupted);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Error::herdr_transport("bounded work ended without a result").into());
            }
        }
    }
}

fn apply_herdr_env(command: &mut Command, context: &InsideContext) {
    for (key, _) in std::env::vars() {
        if key.starts_with("HERDR_") {
            command.env_remove(key);
        }
    }
    for (key, value) in &context.herdr_vars {
        if key != "HERDR_SESSION" {
            command.env(key, value);
        }
    }
    command.env("HERDR_SOCKET_PATH", &context.socket_path);
    command.env("HERDR_BIN_PATH", &context.bin);
    command.env_remove("HERDR_SESSION");
}

struct BoundedRead {
    bytes: Vec<u8>,
    exceeded: bool,
}

fn read_bounded(reader: impl Read, max_bytes: usize) -> io::Result<BoundedRead> {
    let mut bytes = Vec::new();
    reader
        .take(u64::try_from(max_bytes).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut bytes)?;
    let exceeded = bytes.len() > max_bytes;
    if exceeded {
        bytes.truncate(max_bytes);
    }
    Ok(BoundedRead { bytes, exceeded })
}

fn terminate(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = i32::try_from(child.id()).unwrap_or(0);
        if pid > 0 {
            // SAFETY: `run` spawns with `process_group(0)`, so the child's PGID
            // equals its PID. A negative PID targets that whole group.
            unsafe {
                kill(-pid, SIGKILL);
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
const SIGKILL: i32 = 9;

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

fn spawn_reader(reader: impl Read + Send + 'static) -> mpsc::Receiver<io::Result<BoundedRead>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(read_bounded(reader, MAX_RESPONSE_BYTES));
    });
    rx
}

fn take_reader(
    rx: &mpsc::Receiver<io::Result<BoundedRead>>,
    slot: &mut Option<BoundedRead>,
) -> Result<(), RunError> {
    if slot.is_some() {
        return Ok(());
    }
    match rx.try_recv() {
        Ok(Ok(read)) => {
            *slot = Some(read);
            Ok(())
        }
        Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
            Err(Error::herdr_transport("failed to read the Herdr command output").into())
        }
        Err(mpsc::TryRecvError::Empty) => Ok(()),
    }
}

pub(crate) fn utf8_lossy_sanitized(bytes: &[u8]) -> String {
    super::sanitize_detail(&String::from_utf8_lossy(bytes))
}

#[cfg(test)]
mod tests {
    use super::{READ_TIMEOUT, RunError, run, run_bounded};
    use crate::domain::ErrorKind;
    use crate::herdr::context::inside_context;
    use crate::herdr::test_support::{TempDir, chmod, lock_cli, write_executable};
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    fn context_for(bin: &std::path::Path) -> crate::herdr::context::InsideContext {
        let mut vars = BTreeMap::new();
        vars.insert("HERDR_SESSION".into(), "other".into());
        vars.insert("HERDR_EXTRA".into(), "keep".into());
        inside_context(
            bin.to_str().unwrap(),
            "/tmp/nu-plugin-herdr-cd.sock",
            "w1",
            "w1:t1",
            "w1:p1",
            vars,
        )
        .unwrap()
    }

    fn fake_script(body: &str) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new("cli");
        let bin = write_executable(
            dir.path(),
            "herdr",
            &format!("#!/bin/sh\nset -eu\n{body}\n"),
        );
        (dir, bin)
    }

    #[test]
    fn run_bounded_returns_before_blocking_work_finishes() {
        let deadline = Instant::now() + Duration::from_millis(30);
        let halt = || Instant::now() >= deadline;
        let started = Instant::now();
        let error = run_bounded(&halt, || {
            thread::sleep(Duration::from_millis(500));
            1
        })
        .unwrap_err();
        assert!(matches!(error, RunError::Interrupted));
        // Hosted macOS timers can overshoot a 10ms poll by tens of milliseconds.
        // The bound still proves the helper's 500ms sleep was not waited out.
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "bounded wait must return at halt, elapsed {:?}",
            started.elapsed()
        );
        assert_eq!(run_bounded(&|| false, || 7).unwrap(), 7);
    }

    #[test]
    fn records_argv_and_strips_session_via_wrapper_env() {
        let _cli = lock_cli();
        let dir = TempDir::new("cli-env");
        let record = dir.path().join("record");
        let bin = write_executable(
            dir.path(),
            "herdr",
            &format!(
                "#!/bin/sh\nset -eu\n{{\n  printf 'argv0=%s\\n' \"$0\"\n  printf 'args=%s\\n' \"$*\"\n  printf 'HERDR_SOCKET_PATH=%s\\n' \"${{HERDR_SOCKET_PATH-}}\"\n  if [ -n \"${{HERDR_SESSION+x}}\" ]; then printf 'HERDR_SESSION=%s\\n' \"$HERDR_SESSION\"; else printf 'HERDR_SESSION=<unset>\\n'; fi\n  printf 'HERDR_EXTRA=%s\\n' \"${{HERDR_EXTRA-}}\"\n  printf 'HERDR_PANE_ID=%s\\n' \"${{HERDR_PANE_ID-}}\"\n}} > {}\nprintf '{{}}\\n'\n",
                sh_single(&record.display().to_string()),
            ),
        );
        let context = context_for(&bin);
        let output = run(
            &context,
            &["pane", "current", "--current"],
            READ_TIMEOUT,
            || false,
        )
        .unwrap();
        assert!(output.status.success());
        let recorded = fs::read_to_string(&record).unwrap();
        assert!(recorded.contains(&format!("argv0={}", context.bin.display())));
        assert!(recorded.contains("args=pane current --current"));
        assert!(recorded.contains("HERDR_SOCKET_PATH=/tmp/nu-plugin-herdr-cd.sock"));
        assert!(recorded.contains("HERDR_SESSION=<unset>"));
        assert!(recorded.contains("HERDR_EXTRA=keep"));
        assert!(recorded.contains("HERDR_PANE_ID=w1:p1"));
    }

    fn sh_single(path: &str) -> String {
        format!("'{}'", path.replace('\'', r#"'"'"'"#))
    }

    #[test]
    fn invokes_the_injected_absolute_binary_not_a_path_lookup() {
        let _cli = lock_cli();
        let dir = TempDir::new("cli-path");
        let record = dir.path().join("record");
        let injected = write_executable(
            dir.path(),
            "herdr-injected",
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$0\" > {}\nprintf '{{}}\\n'\n",
                sh_single(&record.display().to_string()),
            ),
        );
        let context = context_for(&injected);
        run(&context, &["api", "snapshot"], READ_TIMEOUT, || false).unwrap();
        assert_eq!(
            fs::read_to_string(&record).unwrap().trim(),
            context.bin.to_str().unwrap()
        );
        assert_ne!(injected.file_name().unwrap(), "herdr");
    }

    #[test]
    fn timeout_terminates_and_reaps_the_child() {
        let _cli = lock_cli();
        // `exec` replaces the wrapper shell so the timed-out PID is `sleep`
        // itself. Process-group teardown still covers a non-exec wrapper.
        let (_dir, bin) = fake_script("exec sleep 5\n");
        let context = context_for(&bin);
        let started = Instant::now();
        let err = run(
            &context,
            &["api", "snapshot"],
            Duration::from_millis(200),
            || false,
        )
        .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout should kill the child promptly, elapsed {:?}",
            started.elapsed()
        );
        match err {
            RunError::Failed(error) => assert_eq!(error.kind(), ErrorKind::HerdrTimeout),
            RunError::Interrupted => panic!("expected timeout"),
        }
    }

    #[test]
    fn timeout_kills_a_wrapper_shell_and_its_sleep_child() {
        let _cli = lock_cli();
        let (_dir, bin) = fake_script("sleep 5\nprintf '{}\\n'\n");
        let context = context_for(&bin);
        let started = Instant::now();
        let err = run(
            &context,
            &["api", "snapshot"],
            Duration::from_millis(200),
            || false,
        )
        .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "killing the wrapper must also reap sleep, elapsed {:?}",
            started.elapsed()
        );
        match err {
            RunError::Failed(error) => assert_eq!(error.kind(), ErrorKind::HerdrTimeout),
            RunError::Interrupted => panic!("expected timeout"),
        }
    }

    #[test]
    fn timeout_reaps_descendants_that_keep_pipes_open_after_the_child_exits() {
        let _cli = lock_cli();
        // The wrapper exits immediately; the background sleep keeps stdout open.
        let (_dir, bin) = fake_script("sleep 5 &\nprintf '{}\\n'\n");
        let context = context_for(&bin);
        let started = Instant::now();
        let err = run(
            &context,
            &["api", "snapshot"],
            Duration::from_millis(200),
            || false,
        )
        .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "reader wait after child exit must stay inside the deadline, elapsed {:?}",
            started.elapsed()
        );
        match err {
            RunError::Failed(error) => assert_eq!(error.kind(), ErrorKind::HerdrTimeout),
            RunError::Interrupted => panic!("expected timeout"),
        }
    }

    #[test]
    fn interruption_terminates_descendants_after_the_child_exits() {
        let _cli = lock_cli();
        let (_dir, bin) = fake_script("sleep 5 &\nprintf '{}\\n'\n");
        let context = context_for(&bin);
        let started = Instant::now();
        let err = run(&context, &["api", "snapshot"], READ_TIMEOUT, || {
            started.elapsed() > Duration::from_millis(50)
        })
        .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "interrupt must be observed while waiting for pipe-holding descendants, elapsed {:?}",
            started.elapsed()
        );
        assert!(matches!(err, RunError::Interrupted));
    }

    #[test]
    fn interruption_terminates_the_child() {
        let _cli = lock_cli();
        let (_dir, bin) = fake_script("exec sleep 5\n");
        let context = context_for(&bin);
        let stop = AtomicBool::new(true);
        let err = run(&context, &["api", "snapshot"], READ_TIMEOUT, || {
            stop.load(Ordering::Relaxed)
        })
        .unwrap_err();
        assert!(matches!(err, RunError::Interrupted));
    }

    #[test]
    fn oversized_output_is_a_protocol_error() {
        let _cli = lock_cli();
        let (_dir, bin) = fake_script("head -c 4194312 /dev/zero\n");
        let context = context_for(&bin);
        let err = run(&context, &["api", "snapshot"], READ_TIMEOUT, || false).unwrap_err();
        match err {
            RunError::Failed(error) => assert_eq!(error.kind(), ErrorKind::HerdrProtocol),
            RunError::Interrupted => panic!("expected protocol error"),
        }
    }

    #[test]
    fn nonzero_exit_is_returned_to_the_caller() {
        let _cli = lock_cli();
        let (_dir, bin) = fake_script("printf 'failed\\n' >&2\nexit 1\n");
        let context = context_for(&bin);
        let output = run(&context, &["api", "snapshot"], READ_TIMEOUT, || false).unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            std::str::from_utf8(&output.stderr).unwrap().trim(),
            "failed"
        );
    }

    #[test]
    fn error_details_may_include_the_socket_path_from_stderr() {
        let _cli = lock_cli();
        let (_dir, bin) =
            fake_script("printf 'cannot connect to %s\\n' \"$HERDR_SOCKET_PATH\" >&2\nexit 1\n");
        let context = context_for(&bin);
        let output = run(&context, &["api", "snapshot"], READ_TIMEOUT, || false).unwrap();
        assert!(
            std::str::from_utf8(&output.stderr)
                .unwrap()
                .contains(&context.socket_path),
            "raw capture still contains the path"
        );
        let err = crate::herdr::protocol::parse_snapshot(&output).unwrap_err();
        match err {
            RunError::Failed(error) => {
                assert!(error.message().contains(&context.socket_path));
            }
            RunError::Interrupted => panic!("unexpected interrupt"),
        }
    }

    #[test]
    fn rejects_a_non_executable_injected_binary_before_spawn() {
        let dir = TempDir::new("cli-mode");
        let bin = dir.path().join("herdr");
        fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        chmod(&bin, 0o644);
        let err = inside_context(
            bin.to_str().unwrap(),
            "/tmp/herdr.sock",
            "w1",
            "w1:t1",
            "w1:p1",
            BTreeMap::new(),
        )
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidHerdrContext);
    }
}
