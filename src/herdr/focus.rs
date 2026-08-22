//! Exact-pane focus through one `pane.focus` socket request.

use std::sync::atomic::{AtomicU64, Ordering};

use super::cli::RunError;
use super::context::InsideContext;
use super::protocol::{self, CommandResult};
use super::socket;
use crate::domain::PaneId;

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

/// Outcome of an exact-pane focus attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FocusResult {
    Focused,
    NotFound { code: String, message: String },
}

/// Focus one pane by id. Never falls back to tab or workspace focus.
pub(crate) fn focus_pane(
    context: &InsideContext,
    pane_id: &PaneId,
    interrupted: impl Fn() -> bool,
) -> Result<FocusResult, RunError> {
    let socket_path = socket::validate_socket_path(&context.socket_path)?;
    let request_id = next_request_id();
    let request = serde_json::json!({
        "id": request_id,
        "method": "pane.focus",
        "params": { "pane_id": pane_id.as_str() },
    });
    let payload = socket::roundtrip(&socket_path, &request.to_string(), interrupted)?;
    match protocol::parse_pane_focus_response(&payload, &request_id, pane_id.as_str())? {
        CommandResult::Ok(()) => Ok(FocusResult::Focused),
        CommandResult::NotFound { code, message } => Ok(FocusResult::NotFound { code, message }),
    }
}

fn next_request_id() -> String {
    format!(
        "hcd-{}-{}",
        std::process::id(),
        NEXT_REQUEST.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::{FocusResult, focus_pane, next_request_id};
    use crate::domain::{ErrorKind, PaneId};
    use crate::herdr::cli::RunError;
    use crate::herdr::context::inside_context;
    use crate::herdr::test_support::{TempDir, write_executable};
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    fn context_for(socket: &str) -> (TempDir, crate::herdr::context::InsideContext) {
        let dir = TempDir::new("focus-bin");
        let bin = write_executable(dir.path(), "herdr", "#!/bin/sh\nexit 0\n");
        let context = inside_context(
            bin.to_str().unwrap(),
            socket,
            "w1",
            "w1:t1",
            "w1:p1",
            BTreeMap::new(),
        )
        .unwrap();
        (dir, context)
    }

    fn serve(
        listener: UnixListener,
        respond: impl FnOnce(&str) -> Vec<u8> + Send + 'static,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                stream.read_exact(&mut byte).unwrap();
                if byte[0] == b'\n' {
                    break;
                }
                buf.push(byte[0]);
            }
            let request = String::from_utf8(buf).unwrap();
            let response = respond(&request);
            stream.write_all(&response).unwrap();
            let _ = stream.flush();
        })
    }

    fn success_for(request: &str) -> Vec<u8> {
        let value: Value = serde_json::from_str(request).unwrap();
        let id = value["id"].as_str().unwrap();
        let pane_id = value["params"]["pane_id"].as_str().unwrap();
        format!(
            r#"{{"id":"{id}","result":{{"type":"pane_info","pane":{{"pane_id":"{pane_id}","future":true}}}}}}"#
        )
        .into_bytes()
        .into_iter()
        .chain(std::iter::once(b'\n'))
        .collect()
    }

    #[test]
    fn request_ids_are_unique_and_prefixed() {
        let first = next_request_id();
        let second = next_request_id();
        assert!(first.starts_with("hcd-"));
        assert_ne!(first, second);
    }

    #[test]
    fn sends_exact_pane_focus_and_closes_after_one_response() {
        let dir = TempDir::new("focus-ok");
        let path = dir.path().join("herdr.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, rx) = mpsc::channel();
        let server = serve(listener, move |request| {
            tx.send(request.to_string()).unwrap();
            success_for(request)
        });
        let (_bin, context) = context_for(path.to_str().unwrap());
        let result = focus_pane(&context, &PaneId::new("w1:p2"), || false).unwrap();
        assert_eq!(result, FocusResult::Focused);
        let request: Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(request["method"], "pane.focus");
        assert_eq!(request["params"]["pane_id"], "w1:p2");
        assert!(
            request["id"].as_str().unwrap().starts_with("hcd-"),
            "request id must use the hcd- prefix"
        );
        assert!(request.get("params").unwrap().get("direction").is_none());
        server.join().unwrap();
    }

    #[test]
    fn pane_not_found_is_typed_and_other_errors_are_actions() {
        let dir = TempDir::new("focus-nf");
        let path = dir.path().join("herdr.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = serve(listener, |request| {
            let value: Value = serde_json::from_str(request).unwrap();
            let id = value["id"].as_str().unwrap();
            format!(
                r#"{{"id":"{id}","error":{{"code":"pane_not_found","message":"pane w1:p9 not found"}}}}"#
            )
            .into_bytes()
            .into_iter()
            .chain(std::iter::once(b'\n'))
            .collect()
        });
        let (_bin, context) = context_for(path.to_str().unwrap());
        match focus_pane(&context, &PaneId::new("w1:p9"), || false).unwrap() {
            FocusResult::NotFound { code, .. } => assert_eq!(code, "pane_not_found"),
            FocusResult::Focused => panic!("expected not found"),
        }
        server.join().unwrap();

        let dir = TempDir::new("focus-action");
        let path = dir.path().join("herdr.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = serve(listener, |request| {
            let value: Value = serde_json::from_str(request).unwrap();
            let id = value["id"].as_str().unwrap();
            format!(r#"{{"id":"{id}","error":{{"code":"busy","message":"nope"}}}}"#)
                .into_bytes()
                .into_iter()
                .chain(std::iter::once(b'\n'))
                .collect()
        });
        let (_bin, context) = context_for(path.to_str().unwrap());
        match focus_pane(&context, &PaneId::new("w1:p2"), || false).unwrap_err() {
            RunError::Failed(error) => assert_eq!(error.kind(), ErrorKind::HerdrAction),
            RunError::Interrupted => panic!("unexpected interrupt"),
        }
        server.join().unwrap();
    }

    #[test]
    fn malformed_truncated_and_oversized_responses_are_protocol_errors() {
        let dir = TempDir::new("focus-proto");
        let path = dir.path().join("malformed.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = serve(listener, |_| b"not-json\n".to_vec());
        let (_bin, context) = context_for(path.to_str().unwrap());
        match focus_pane(&context, &PaneId::new("w1:p2"), || false).unwrap_err() {
            RunError::Failed(error) => assert_eq!(error.kind(), ErrorKind::HerdrProtocol),
            RunError::Interrupted => panic!("unexpected interrupt"),
        }
        server.join().unwrap();

        let path = dir.path().join("truncated.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = serve(listener, |_| b"{\"id\":\"x\"".to_vec());
        let (_bin, context) = context_for(path.to_str().unwrap());
        match focus_pane(&context, &PaneId::new("w1:p2"), || false).unwrap_err() {
            RunError::Failed(error) => {
                assert_eq!(error.kind(), ErrorKind::HerdrProtocol);
                assert!(error.message().contains("truncated"));
            }
            RunError::Interrupted => panic!("unexpected interrupt"),
        }
        server.join().unwrap();

        let path = dir.path().join("huge.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = serve(listener, |_| {
            let mut body = vec![b'a'; 4 * 1024 * 1024 + 2];
            body.push(b'\n');
            body
        });
        let (_bin, context) = context_for(path.to_str().unwrap());
        match focus_pane(&context, &PaneId::new("w1:p2"), || false).unwrap_err() {
            RunError::Failed(error) => {
                assert_eq!(error.kind(), ErrorKind::HerdrProtocol);
                assert!(error.message().contains("4 MiB"));
            }
            RunError::Interrupted => panic!("unexpected interrupt"),
        }
        server.join().unwrap();
    }

    #[test]
    fn timeout_and_interruption_close_the_socket() {
        let dir = TempDir::new("focus-wait");
        let path = dir.path().join("timeout.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_secs(3));
        });
        let (_bin, context) = context_for(path.to_str().unwrap());
        let started = Instant::now();
        match focus_pane(&context, &PaneId::new("w1:p2"), || false).unwrap_err() {
            RunError::Failed(error) => assert_eq!(error.kind(), ErrorKind::HerdrTimeout),
            RunError::Interrupted => panic!("expected timeout"),
        }
        assert!(started.elapsed() < Duration::from_secs(3));
        let _ = server.join();

        let path = dir.path().join("interrupt.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_secs(3));
        });
        let (_bin, context) = context_for(path.to_str().unwrap());
        let started = Instant::now();
        match focus_pane(&context, &PaneId::new("w1:p2"), || {
            started.elapsed() > Duration::from_millis(50)
        })
        .unwrap_err()
        {
            RunError::Interrupted => {}
            RunError::Failed(error) => panic!("expected interrupt, got {}", error),
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        let _ = server.join();
    }

    #[test]
    fn regular_file_socket_paths_are_rejected_before_connect() {
        let dir = TempDir::new("focus-file");
        let path = dir.path().join("herdr.sock");
        std::fs::write(&path, "not a socket").unwrap();
        let (_bin, context) = context_for(path.to_str().unwrap());
        match focus_pane(&context, &PaneId::new("w1:p2"), || false).unwrap_err() {
            RunError::Failed(error) => {
                assert_eq!(error.kind(), ErrorKind::HerdrTransport);
                assert!(error.message().contains("not a Unix socket"));
            }
            RunError::Interrupted => panic!("unexpected interrupt"),
        }
    }
}
