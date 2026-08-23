//! One-shot Unix-socket transport for exact-pane focus.

use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
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
            Ok(Ok(stream)) => {
                require_peer_current_user(&stream)?;
                return Ok(stream);
            }
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

fn require_peer_current_user(stream: &UnixStream) -> Result<(), RunError> {
    let uid = peer_uid(stream)
        .map_err(|_| Error::herdr_transport("failed to authenticate the Herdr socket peer"))?;
    if !socket_owned_by_current_user(uid) {
        return Err(Error::herdr_transport("Herdr socket peer is not the current user").into());
    }
    Ok(())
}

fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    #[cfg(target_os = "linux")]
    {
        linux_peer_uid(stream)
    }
    #[cfg(not(target_os = "linux"))]
    {
        getpeereid_uid(stream)
    }
}

#[cfg(target_os = "linux")]
fn linux_peer_uid(stream: &UnixStream) -> io::Result<u32> {
    #[repr(C)]
    struct Ucred {
        pid: i32,
        uid: u32,
        gid: u32,
    }

    const SOL_SOCKET: i32 = 1;
    const SO_PEERCRED: i32 = 17;

    unsafe extern "C" {
        fn getsockopt(
            sockfd: i32,
            level: i32,
            optname: i32,
            optval: *mut std::ffi::c_void,
            optlen: *mut u32,
        ) -> i32;
    }

    let mut cred = Ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<Ucred>() as u32;
    // SAFETY: `stream` is a connected Unix socket. `cred` and `len` match the
    // Linux SO_PEERCRED ucred layout, and getsockopt writes at most *optlen bytes.
    let rc = unsafe {
        getsockopt(
            stream.as_raw_fd(),
            SOL_SOCKET,
            SO_PEERCRED,
            (&raw mut cred).cast(),
            &raw mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    if usize::try_from(len).unwrap_or(0) < std::mem::size_of::<Ucred>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incomplete peer credentials",
        ));
    }
    Ok(cred.uid)
}

#[cfg(not(target_os = "linux"))]
fn getpeereid_uid(stream: &UnixStream) -> io::Result<u32> {
    unsafe extern "C" {
        fn getpeereid(s: i32, euid: *mut u32, egid: *mut u32) -> i32;
    }

    let mut uid = 0_u32;
    let mut gid = 0_u32;
    // SAFETY: `stream` is a connected Unix socket. `uid` and `gid` are valid
    // out-pointers for getpeereid.
    let rc = unsafe { getpeereid(stream.as_raw_fd(), &raw mut uid, &raw mut gid) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

fn euid() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: geteuid has no preconditions and only returns the effective user id.
    unsafe { geteuid() }
}

fn socket_owned_by_current_user(uid: u32) -> bool {
    uid == euid()
}

#[cfg(test)]
mod tests {
    use super::{require_peer_current_user, socket_owned_by_current_user, validate_socket_path};
    use crate::domain::ErrorKind;
    use crate::herdr::test_support::TempDir;
    use std::os::unix::net::{UnixListener, UnixStream};

    #[test]
    fn current_user_ownership_is_required() {
        assert!(socket_owned_by_current_user(super::euid()));
        assert!(!socket_owned_by_current_user(super::euid().wrapping_add(1)));
    }

    #[test]
    fn connected_same_user_peer_is_accepted() {
        let dir = TempDir::new("socket-peer");
        let path = dir.path().join("herdr.sock");
        let _listener = UnixListener::bind(&path).unwrap();
        let stream = UnixStream::connect(&path).unwrap();
        assert_eq!(super::peer_uid(&stream).unwrap(), super::euid());
        require_peer_current_user(&stream).unwrap();
    }

    #[test]
    fn rejects_relative_missing_and_regular_file_paths() {
        let relative = validate_socket_path("herdr.sock").unwrap_err();
        assert_eq!(relative.kind(), ErrorKind::HerdrTransport);
        assert!(relative.message().contains("absolute"));

        let missing = validate_socket_path("/tmp/nu-plugin-herdr-navigate-directory-missing.sock")
            .unwrap_err();
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
