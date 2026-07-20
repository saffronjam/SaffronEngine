//! Socket-level integration tests: the framing, the flush loop, and the path
//! resolution. These drive a real bound `ControlServer` with a std `UnixStream`
//! client and the synchronous `drain` seam, so the wire mechanics are exercised
//! end to end without the un-headless-constructible engine subsystems.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;

use saffron_control::start_control_server;
use serde_json::Value;

/// A unique short socket path for one test. Unix-domain socket paths have a small fixed limit, and
/// macOS's per-user temporary directory already consumes most of it.
fn temp_socket(tag: &str) -> String {
    let dir = std::path::Path::new("/tmp");
    let unique = format!(
        "saffron-control-{tag}-{}-{}.sock",
        std::process::id(),
        // A monotonically-distinct suffix per call.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    );
    dir.join(unique).to_string_lossy().into_owned()
}

#[test]
fn round_trip_reads_one_newline_terminated_reply() {
    let path = temp_socket("round-trip");
    let mut server = start_control_server(path.clone()).expect("bind");

    let mut client = UnixStream::connect(&path).expect("connect");
    client
        .write_all(b"{\"id\":1,\"cmd\":\"ping\"}\n")
        .expect("write request");

    // Echo back a canned ping-shaped reply so the framing is what is under test.
    server.drain(|line| {
        let request: Value = serde_json::from_str(line).expect("valid json");
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        serde_json::to_string(&serde_json::json!({
            "id": id,
            "ok": true,
            "result": { "pong": true },
        }))
        .unwrap()
    });

    let mut reader = BufReader::new(&mut client);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read reply");
    assert!(line.ends_with('\n'), "reply is newline-terminated");
    let reply: Value = serde_json::from_str(line.trim_end()).expect("reply parses");
    assert_eq!(reply["id"], serde_json::json!(1));
    assert_eq!(reply["ok"], serde_json::json!(true));
    assert_eq!(reply["result"]["pong"], serde_json::json!(true));
}

#[test]
fn invalid_json_request_returns_the_frozen_error_envelope() {
    let path = temp_socket("invalid");
    let mut server = start_control_server(path.clone()).expect("bind");

    let mut client = UnixStream::connect(&path).expect("connect");
    client.write_all(b"{not json\n").expect("write");

    server.drain(|line| match serde_json::from_str::<Value>(line) {
        Ok(_) => unreachable!("the line is not valid json"),
        Err(_) => r#"{"ok":false,"error":"invalid JSON request"}"#.to_owned(),
    });

    let mut reader = BufReader::new(&mut client);
    let mut reply = String::new();
    reader.read_line(&mut reply).expect("read");
    let parsed: Value = serde_json::from_str(reply.trim_end()).unwrap();
    assert_eq!(parsed["ok"], serde_json::json!(false));
    assert_eq!(parsed["error"], serde_json::json!("invalid JSON request"));
}

#[test]
fn flush_loop_delivers_a_reply_larger_than_the_socket_buffer() {
    let path = temp_socket("flush");
    let mut server = start_control_server(path.clone()).expect("bind");

    let mut client = UnixStream::connect(&path).expect("connect");
    client.write_all(b"{\"cmd\":\"big\"}\n").expect("write");

    // A reply far larger than any socket send buffer: a single send() would
    // short-write and drop the tail without the flush loop. The payload is one
    // JSON string of a megabyte of 'x'.
    const PAYLOAD_LEN: usize = 1 << 20;
    let big = "x".repeat(PAYLOAD_LEN);
    let reply = serde_json::to_string(&serde_json::json!({ "ok": true, "blob": big })).unwrap();
    let expected = reply.clone();

    // The client must drain the socket concurrently or the server's send blocks
    // forever on a full buffer; read on a background thread.
    let mut read_side = client.try_clone().expect("clone fd");
    let reader_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 65536];
        loop {
            let n = read_side.read(&mut chunk).expect("read");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.last() == Some(&b'\n') {
                break;
            }
        }
        buf
    });

    server.drain(move |_line| reply.clone());

    // Close the server side so the reader's loop terminates if it has the '\n'.
    drop(server);
    let got = reader_handle.join().expect("reader thread");
    assert_eq!(
        got.last(),
        Some(&b'\n'),
        "terminated by exactly one newline"
    );
    let body = &got[..got.len() - 1];
    assert_eq!(body.len(), expected.len(), "whole payload arrived");
    assert_eq!(body, expected.as_bytes());
}
