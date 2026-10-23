//! `oxyde-mcp --window`: the bridge an Oxyde window's agent starts, run as a process. Its
//! answers go out from a second thread while `main` holds the standard output locked, which
//! once meant nothing ever reached the agent: this checks a message both ways through the
//! real program, with a time limit rather than a hang.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::mpsc::channel;
use std::time::Duration;

#[test]
fn the_bridge_process_passes_messages_both_ways() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_oxyde-mcp"))
        .args(["--window", &address, "secret"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    // The window's side: the token, then the agent's message, answered.
    let window = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut from_bridge = BufReader::new(stream.try_clone().unwrap());
        let mut to_bridge = stream;
        let mut hello = String::new();
        from_bridge.read_line(&mut hello).unwrap();
        assert!(
            oxyde_mcp::window::hello_matches(&hello, "secret"),
            "{hello}"
        );
        let mut message = String::new();
        from_bridge.read_line(&mut message).unwrap();
        assert!(message.contains("\"ping\""), "{message}");
        writeln!(to_bridge, r#"{{"jsonrpc":"2.0","id":1,"result":{{}}}}"#).unwrap();
        // Then the agent goes, and the window closes its side.
        let mut rest = String::new();
        while from_bridge.read_line(&mut rest).unwrap_or(0) > 0 {}
    });

    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"ping"}}"#).unwrap();
    stdin.flush().unwrap();
    let (send, answer) = channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = send.send(line);
    });
    let Ok(line) = answer.recv_timeout(Duration::from_secs(30)) else {
        let _ = child.kill();
        panic!("no answer from the bridge in 30 s");
    };
    assert_eq!(line.trim(), r#"{"jsonrpc":"2.0","id":1,"result":{}}"#);

    drop(stdin);
    window.join().unwrap();
    assert!(child.wait().unwrap().success());
}
