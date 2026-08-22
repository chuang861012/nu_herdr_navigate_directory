use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

#[test]
fn compiled_binary_name_matches_plugin_identity() {
    let path = Path::new(env!("CARGO_BIN_EXE_nu_plugin_herdr_cd"));
    let file_name = path
        .file_name()
        .expect("plugin binary file name")
        .to_string_lossy();
    assert!(
        file_name.starts_with("nu_plugin_herdr_cd"),
        "unexpected plugin binary name: {file_name}"
    );
}

#[test]
fn compiled_binary_serves_msgpack_over_stdio() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_nu_plugin_herdr_cd"))
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn plugin binary");

    let mut stdout = child.stdout.take().expect("plugin stdout");
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

    drop(child.stdin.take());
    let _ = child.wait();
}
