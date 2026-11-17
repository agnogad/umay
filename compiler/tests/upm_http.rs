//! Fetching a package from an HTTP registry.
//!
//! A local listener serves one USTAR archive; the client uses `curl` and the
//! same extraction path a hosted registry does. HTTPS is carried by curl, so a
//! plain listener is enough to pin the protocol.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::thread;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_http_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

fn octal(value: usize, width: usize) -> Vec<u8> {
    let text = format!("{value:0width$o}");
    let mut bytes = text.into_bytes();
    bytes.push(0);
    bytes
}

fn tar_entry(name: &str, data: &[u8]) -> Vec<u8> {
    let mut header = vec![0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    header[100..108].copy_from_slice(b"0000644\0");
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    let size = octal(data.len(), 11);
    header[124..124 + size.len()].copy_from_slice(&size);
    header[136..148].copy_from_slice(b"00000000000\0");
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let mut out = header;
    out.extend_from_slice(data);
    out.resize(512 + data.len().div_ceil(512) * 512, 0);
    out
}

fn package_tar() -> Vec<u8> {
    let mut tar = Vec::new();
    tar.extend(tar_entry(
        "umay.toml",
        b"[package]\nname = \"lib\"\nversion = \"1.0.0\"\nedition = \"2026\"\n",
    ));
    tar.extend(tar_entry(
        "src/lib.um",
        b"pub fn answer() -> i64 { return 42 }\n",
    ));
    tar.extend(vec![0u8; 1024]);
    tar
}

#[test]
fn a_package_is_fetched_over_http_and_unpacked() {
    let curl = Command::new("curl")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !curl {
        eprintln!("skipping: curl is not available");
        return;
    }

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let body = package_tar();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept");
        let mut request = [0u8; 2048];
        let _ = socket.read(&mut request);
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Type: application/x-tar\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .expect("headers");
        socket.write_all(&body).expect("body");
    });

    let project = scratch("project");
    std::env::set_var("UMAY_REGISTRY", format!("http://127.0.0.1:{port}"));
    let fetched = umayc::upm::fetch(&project, "lib", "1.0.0").expect("fetch");
    std::env::remove_var("UMAY_REGISTRY");
    server.join().expect("server");

    assert_eq!(
        std::fs::read_to_string(fetched.join("src").join("lib.um")).unwrap(),
        "pub fn answer() -> i64 { return 42 }\n"
    );
    assert!(fetched.join("umay.toml").is_file());
}
