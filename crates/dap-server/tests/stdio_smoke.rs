// End-to-end proof the adapter speaks real DAP over stdio: spawns the built
// binary as a subprocess, drives it through the full launch sequence
// (initialize -> initialized event -> setBreakpoints -> configurationDone
// -> stopped-at-breakpoint -> introspect -> continue -> terminated), and
// asserts on the framed JSON read back - real process boundaries, real
// wire framing, nothing mocked.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

use serde_json::{json, Value};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_seq: i64,
}

impl Client {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_dap-server"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn dap-server binary");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Client { child, stdin, stdout, next_seq: 1 }
    }

    fn write_message(&mut self, value: &Value) {
        let body = serde_json::to_vec(value).unwrap();
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        self.stdin.write_all(&body).unwrap();
        self.stdin.flush().unwrap();
    }

    fn read_message(&mut self) -> Value {
        let mut content_length = None;
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).expect("failed to read a header line from dap-server");
            assert_ne!(n, 0, "dap-server closed stdout unexpectedly (process likely exited/panicked)");
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length:") {
                content_length = Some(v.trim().parse::<usize>().unwrap());
            }
        }
        let content_length = content_length.expect("missing Content-Length header");
        let mut body = vec![0u8; content_length];
        self.stdout.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    /// Sends a request, then reads messages until the matching response
    /// arrives, returning it along with any events observed along the way
    /// (in order) - `configurationDone`/`continue` can emit `stopped`/
    /// `output`/`terminated` events interleaved before their own response
    /// in principle, though in this adapter the response is always written
    /// first; collecting events defensively either way.
    fn request(&mut self, command: &str, arguments: Value) -> (Value, Vec<Value>) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.write_message(&json!({ "seq": seq, "type": "request", "command": command, "arguments": arguments }));
        let mut events = Vec::new();
        loop {
            let msg = self.read_message();
            if msg.get("type").and_then(Value::as_str) == Some("response")
                && msg.get("request_seq").and_then(Value::as_i64) == Some(seq)
            {
                return (msg, events);
            }
            events.push(msg);
        }
    }

    /// Reads messages (which may include leftover events from a prior
    /// `request` call, if any were buffered - none are here) until an event
    /// of the given name is seen.
    fn wait_for_event(&mut self, name: &str) -> Value {
        loop {
            let msg = self.read_message();
            if msg.get("type").and_then(Value::as_str) == Some("event")
                && msg.get("event").and_then(Value::as_str) == Some(name)
            {
                return msg;
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn full_dap_lifecycle_over_stdio() {
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/breakpoint.lua");
    let mut client = Client::spawn();

    let (resp, _) = client.request("initialize", json!({}));
    assert_eq!(resp["success"], true, "{resp}");
    assert_eq!(resp["body"]["supportsConditionalBreakpoints"], true);
    let _ = client.wait_for_event("initialized");

    let (resp, _) = client.request("launch", json!({ "program": fixture }));
    assert_eq!(resp["success"], true, "{resp}");

    let (resp, _) = client.request(
        "setBreakpoints",
        json!({ "source": { "path": fixture }, "breakpoints": [{ "line": 4 }] }),
    );
    assert_eq!(resp["success"], true, "{resp}");
    let breakpoints = resp["body"]["breakpoints"].as_array().unwrap();
    assert_eq!(breakpoints.len(), 1);
    assert_eq!(breakpoints[0]["verified"], true);
    assert_eq!(breakpoints[0]["line"], 4);

    let (resp, _) = client.request("configurationDone", json!({}));
    assert_eq!(resp["success"], true, "{resp}");
    let stopped = client.wait_for_event("stopped");
    assert_eq!(stopped["body"]["reason"], "breakpoint");
    let thread_id = stopped["body"]["threadId"].as_i64().unwrap();

    let (resp, _) = client.request("threads", json!({}));
    assert_eq!(resp["success"], true, "{resp}");
    assert_eq!(resp["body"]["threads"][0]["id"], thread_id);

    let (resp, _) = client.request("stackTrace", json!({ "threadId": thread_id }));
    assert_eq!(resp["success"], true, "{resp}");
    let frames = resp["body"]["stackFrames"].as_array().unwrap();
    assert!(!frames.is_empty(), "{resp}");
    assert_eq!(frames[0]["line"], 4);
    assert_eq!(
        frames[0]["source"]["path"], fixture,
        "source.path must be the absolute launch path, not just the chunk basename, or VS Code can't navigate to it: {resp}"
    );
    let frame_id = frames[0]["id"].as_i64().unwrap();

    let (resp, _) = client.request("scopes", json!({ "frameId": frame_id }));
    assert_eq!(resp["success"], true, "{resp}");
    let scopes = resp["body"]["scopes"].as_array().unwrap();
    let locals_ref = scopes.iter().find(|s| s["name"] == "Locals").unwrap()["variablesReference"]
        .as_i64()
        .unwrap();

    let (resp, _) = client.request("variables", json!({ "variablesReference": locals_ref }));
    assert_eq!(resp["success"], true, "{resp}");
    let vars = resp["body"]["variables"].as_array().unwrap();
    let sum = vars.iter().find(|v| v["name"] == "sum").expect(&format!("no 'sum' local in {vars:?}"));
    assert_eq!(sum["value"], "3");
    let x = vars.iter().find(|v| v["name"] == "x").unwrap();
    assert_eq!(x["value"], "1");

    let (resp, _) = client.request("evaluate", json!({ "expression": "x + y", "frameId": frame_id }));
    assert_eq!(resp["success"], true, "{resp}");
    assert_eq!(resp["body"]["result"], "3");

    let (resp, _) = client.request(
        "setVariable",
        json!({ "variablesReference": locals_ref, "name": "sum", "value": "42" }),
    );
    assert_eq!(resp["success"], true, "{resp}");
    assert_eq!(resp["body"]["value"], "42");

    let (resp, _) = client.request("continue", json!({ "threadId": thread_id }));
    assert_eq!(resp["success"], true, "{resp}");
    assert_eq!(resp["body"]["allThreadsContinued"], true);

    let output = client.wait_for_event("output");
    assert_eq!(output["body"]["output"], "42\n", "setVariable's edit should reach the actual print() call");
    let _ = client.wait_for_event("terminated");

    let (resp, _) = client.request("disconnect", json!({}));
    assert_eq!(resp["success"], true, "{resp}");
}

#[test]
fn set_breakpoints_reconciles_a_second_call_against_the_first() {
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/breakpoint.lua");
    let mut client = Client::spawn();

    let (resp, _) = client.request("initialize", json!({}));
    assert_eq!(resp["success"], true, "{resp}");
    let _ = client.wait_for_event("initialized");
    let (resp, _) = client.request("launch", json!({ "program": fixture }));
    assert_eq!(resp["success"], true, "{resp}");

    let (resp, _) = client.request(
        "setBreakpoints",
        json!({ "source": { "path": fixture }, "breakpoints": [{ "line": 2 }, { "line": 4 }] }),
    );
    assert_eq!(resp["body"]["breakpoints"].as_array().unwrap().len(), 2, "{resp}");

    // Bulk-replace with just line 4 - line 2's breakpoint must be gone, not
    // left stale (this is what `set_breakpoints` reconciling against the
    // tracked list, rather than blindly re-adding, is actually for).
    let (resp, _) = client.request(
        "setBreakpoints",
        json!({ "source": { "path": fixture }, "breakpoints": [{ "line": 4 }] }),
    );
    assert_eq!(resp["body"]["breakpoints"].as_array().unwrap().len(), 1, "{resp}");

    let (resp, _) = client.request("configurationDone", json!({}));
    assert_eq!(resp["success"], true, "{resp}");
    let stopped = client.wait_for_event("stopped");
    // If line 2's breakpoint were still active, this would stop at line 2
    // first instead.
    assert_eq!(stopped["body"]["reason"], "breakpoint");

    let thread_id = stopped["body"]["threadId"].as_i64().unwrap();
    let (resp, _) = client.request("stackTrace", json!({ "threadId": thread_id }));
    let frames = resp["body"]["stackFrames"].as_array().unwrap();
    assert_eq!(frames[0]["line"], 4, "expected the remaining breakpoint (line 4) to be the one hit: {resp}");

    let (resp, _) = client.request("disconnect", json!({}));
    assert_eq!(resp["success"], true, "{resp}");
}

#[test]
fn pause_interrupts_a_tight_loop() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("dap_pause_test_{}.lua", std::process::id()));
    std::fs::write(&path, "local i = 0\nwhile true do\n  i = i + 1\nend\n").unwrap();

    let mut client = Client::spawn();
    let (resp, _) = client.request("initialize", json!({}));
    assert_eq!(resp["success"], true, "{resp}");
    let _ = client.wait_for_event("initialized");
    let (resp, _) = client.request("launch", json!({ "program": path.to_str().unwrap() }));
    assert_eq!(resp["success"], true, "{resp}");
    let (resp, _) = client.request("configurationDone", json!({}));
    assert_eq!(resp["success"], true, "{resp}");

    // The program is now looping forever inside `drive_continue`'s burst
    // loop - `pause` must still get a timely response and a `stopped`
    // event, proving the non-blocking channel poll between bursts works.
    let (resp, _) = client.request("pause", json!({}));
    assert_eq!(resp["success"], true, "{resp}");
    let stopped = client.wait_for_event("stopped");
    assert_eq!(stopped["body"]["reason"], "pause", "{stopped}");

    let (resp, _) = client.request("disconnect", json!({}));
    assert_eq!(resp["success"], true, "{resp}");
    let _ = std::fs::remove_file(&path);
}
