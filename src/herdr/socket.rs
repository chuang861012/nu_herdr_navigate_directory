//! One-shot Unix-socket transport for exact-pane focus.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use super::cli::{MAX_RESPONSE_BYTES, POLL_INTERVAL, READ_TIMEOUT, RunError};
use crate::domain::Error;

pub(crate) fn validate_socket_path(path: &str) -> Result<PathBuf, Error> {
    let path = path.trim();
    if path.is_empty() || !Path::new(path).is_absolute() {
        return Err(Error::herdr_transport(
            "Herdr socket path must be an absolute Unix socket",
        ));
    }

    let metadata = fs::metadata(path).map_err(|_| {
        Error::herdr_transport("Herdr socket path does not exist or cannot be read")
    })?;
    if !metadata.file_type().is_socket() {
        return Err(Error::herdr_transport(
            "Herdr socket path is not a Unix socket",
        ));
    }
    if metadata.uid() != euid() {
        return Err(Error::herdr_transport(
            "Herdr socket is not owned by the current user",
        ));
    }
    Ok(PathBuf::from(path))
}

pub(crate) fn roundtrip(
    path: &Path,
    request_line: &str,
    interrupted: impl Fn() -> bool,
) -> Result<Vec<u8>, RunError> {
    let deadline = Instant::now() + READ_TIMEOUT;
    let mut stream = connect(path, deadline, &interrupted)?;
    if let Err(err) = stream.set_nonblocking(true) {
        return Err(
            Error::herdr_transport(format!("failed to configure the Herdr socket: {err}")).into(),
        );
    }

    write_request(&mut stream, request_line, deadline, &interrupted)?;
    read_response(&mut stream, deadline, &interrupted)
}

fn connect(
    path: &Path,
    deadline: Instant,
    interrupted: &impl Fn() -> bool,
) -> Result<UnixStream, RunError> {
    let path = path.to_path_buf();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(UnixStream::connect(path));
    });

    loop {
        if interrupted() {
            return Err(RunError::Interrupted);
        }
        if Instant::now() >= deadline {
            return Err(Error::herdr_timeout("pane focus timed out").into());
        }
        match rx.try_recv() {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(_)) => {
                return Err(Error::herdr_transport("failed to connect to the Herdr socket").into());
            }
            Err(mpsc::TryRecvError::Empty) => thread::sleep(POLL_INTERVAL),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(Error::herdr_transport("failed to connect to the Herdr socket").into());
            }
        }
    }
}

fn write_request(
    stream: &mut UnixStream,
    request_line: &str,
    deadline: Instant,
    interrupted: &impl Fn() -> bool,
) -> Result<(), RunError> {
    let mut bytes = request_line.as_bytes().to_vec();
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    let mut written = 0;
    while written < bytes.len() {
        if interrupted() {
            return Err(RunError::Interrupted);
        }
        if Instant::now() >= deadline {
            return Err(Error::herdr_timeout("pane focus timed out").into());
        }
        match stream.write(&bytes[written..]) {
            Ok(0) => {
                return Err(Error::herdr_transport("Herdr socket closed while sending").into());
            }
            Ok(n) => written += n,
            Err(err) if would_block(&err) => thread::sleep(POLL_INTERVAL),
            Err(_) => {
                return Err(
                    Error::herdr_transport("failed to write the pane focus request").into(),
                );
            }
        }
    }
    Ok(())
}

fn read_response(
    stream: &mut UnixStream,
    deadline: Instant,
    interrupted: &impl Fn() -> bool,
) -> Result<Vec<u8>, RunError> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if interrupted() {
            return Err(RunError::Interrupted);
        }
        if Instant::now() >= deadline {
            return Err(Error::herdr_timeout("pane focus timed out").into());
        }
        match stream.read(&mut chunk) {
            Ok(0) => {
                if bytes.is_empty() {
                    return Err(protocol("empty response").into());
                }
                return Err(protocol("truncated response").into());
            }
            Ok(n) => {
                let data = &chunk[..n];
                if let Some(newline) = data.iter().position(|b| *b == b'\n') {
                    bytes.extend_from_slice(&data[..newline]);
                    if bytes.len() > MAX_RESPONSE_BYTES {
                        return Err(Error::herdr_protocol(
                            "Herdr response exceeded the 4 MiB limit",
                        )
                        .into());
                    }
                    return Ok(bytes);
                }
                bytes.extend_from_slice(data);
                if bytes.len() > MAX_RESPONSE_BYTES {
                    return Err(
                        Error::herdr_protocol("Herdr response exceeded the 4 MiB limit").into(),
                    );
                }
            }
            Err(err) if would_block(&err) => thread::sleep(POLL_INTERVAL),
            Err(_) => {
                return Err(
                    Error::herdr_transport("failed to read the pane focus response").into(),
                );
            }
        }
    }
}

fn would_block(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

fn protocol(detail: &str) -> Error {
    Error::herdr_protocol(format!("pane focus: {detail}"))
}

fn euid() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: geteuid has no preconditions and only returns the effective user id.
    unsafe { geteuid() }
}

pub(crate) fn socket_owned_by_current_user(uid: u32) -> bool {
    uid == euid()
}

#[cfg(test)]
mod tests {
    use super::{socket_owned_by_current_user, validate_socket_path};
    use crate::domain::ErrorKind;
    use crate::herdr::test_support::TempDir;
    use std::os::unix::net::UnixListener;

    #[test]
    fn current_user_ownership_is_required() {
        assert!(socket_owned_by_current_user(super::euid()));
        assert!(!socket_owned_by_current_user(super::euid().wrapping_add(1)));
    }

    #[test]
    fn rejects_relative_missing_and_regular_file_paths() {
        let relative = validate_socket_path("herdr.sock").unwrap_err();
        assert_eq!(relative.kind(), ErrorKind::HerdrTransport);
        assert!(relative.message().contains("absolute"));

        let missing = validate_socket_path("/tmp/nu-plugin-herdr-cd-missing.sock").unwrap_err();
        assert_eq!(missing.kind(), ErrorKind::HerdrTransport);

        let dir = TempDir::new("socket-file");
        let file = dir.path().join("not-a-socket");
        std::fs::write(&file, "nope").unwrap();
        let regular = validate_socket_path(file.to_str().unwrap()).unwrap_err();
        assert_eq!(regular.kind(), ErrorKind::HerdrTransport);
        assert!(regular.message().contains("not a Unix socket"));
    }

    #[test]
    fn accepts_an_absolute_user_owned_unix_socket() {
        let dir = TempDir::new("socket-ok");
        let path = dir.path().join("herdr.sock");
        let _listener = UnixListener::bind(&path).unwrap();
        let accepted = validate_socket_path(path.to_str().unwrap()).unwrap();
        assert_eq!(accepted, path);
    }
}
