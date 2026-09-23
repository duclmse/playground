//! Native DAP (Debug Adapter Protocol) server over `lua_vm::DebugSession` -
//! the same debug engine the browser's web debugger drives via wasm, here
//! linked as a plain native `rlib` dependency (see docs/dap-server.md for
//! why `#[wasm_bindgen]` on `DebugSession` doesn't stand in the way of
//! that). A wholly separate consumer of `lua-vm`/`vm` from `apps/web` -
//! nothing here talks to the browser worker protocol, and nothing in
//! `apps/web` needs to change for this to exist.
//!
//! v1 scope (see docs/dap-server.md for the full design and what's
//! deferred): `initialize`, `launch` (single local `.lua` file, no
//! `require()`/multi-file projects), `setBreakpoints` (including
//! conditional/hit-count/logpoint variants), `configurationDone`,
//! `threads`, `stackTrace`, `scopes`, `variables`, `evaluate`,
//! `setVariable`, `continue`, `next`, `stepIn`, `stepOut`, `pause`,
//! `disconnect`/`terminate`.
//!
//! Threading model: one reader thread does blocking stdio reads and posts
//! parsed requests to a channel; the main thread owns the `DebugSession`
//! (piccolo/gc-arena's GC types are `!Send`, so it must never leave one
//! thread) and both handles requests and writes every response/event
//! directly to stdout - no separate writer thread is needed since only this
//! one thread ever produces outgoing messages. A `continue` request is
//! acknowledged immediately, then driven in `BURST_INSTRUCTIONS`-sized
//! bursts (mirroring `apps/web/src/debug-session.ts`'s `continue()`); the
//! non-blocking channel poll between bursts is what makes `pause` (and any
//! other request arriving mid-run, such as a live `setBreakpoints`)
//! responsive without a second thread touching `DebugSession`.

mod convert;
mod framing;
mod session;

use std::io::{self, BufReader, Write};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc;
use std::thread;

use serde_json::{json, Value};

use session::{dispatch_query, AdapterSession};

/// Instructions per `continue_burst` call - small enough that `pause`/an
/// interleaved request takes effect within roughly one burst, large enough
/// that a normal run isn't dominated by loop overhead. Matches
/// `debug-session.ts`'s `CONTINUE_BURST_SIZE`.
const BURST_INSTRUCTIONS: u32 = 200_000;
/// Matches `debug-session.ts`'s `MAX_CONTINUE_INSTRUCTIONS`: a client-side
/// cumulative cap across bursts, since each `continue_burst` call only
/// enforces its own per-call budget, not a running total.
const MAX_CONTINUE_INSTRUCTIONS: u64 = 50_000_000;

struct IncomingRequest {
    seq: i64,
    command: String,
    arguments: Value,
}

static OUT_SEQ: AtomicI64 = AtomicI64::new(1);

fn next_seq() -> i64 {
    OUT_SEQ.fetch_add(1, Ordering::SeqCst)
}

fn write_response(
    out: &mut impl Write,
    request_seq: i64,
    command: &str,
    result: &Result<Value, String>,
) {
    let msg = match result {
        Ok(body) => json!({
            "seq": next_seq(),
            "type": "response",
            "request_seq": request_seq,
            "success": true,
            "command": command,
            "body": body,
        }),
        Err(message) => json!({
            "seq": next_seq(),
            "type": "response",
            "request_seq": request_seq,
            "success": false,
            "command": command,
            "message": message,
        }),
    };
    let _ = framing::write_message(out, &msg);
}

fn write_event(out: &mut impl Write, event: &str, body: Value) {
    let msg = json!({ "seq": next_seq(), "type": "event", "event": event, "body": body });
    let _ = framing::write_message(out, &msg);
}

fn flush_output_event(session: &mut AdapterSession, out: &mut impl Write) {
    let text = session.take_output();
    if !text.is_empty() {
        write_event(
            out,
            "output",
            json!({ "category": "stdout", "output": text }),
        );
    }
}

/// Outcome of the interruptible `continue` loop, so the caller (the main
/// dispatch loop) knows whether to keep reading requests or shut down.
enum DriveOutcome {
    /// A real stop happened (breakpoint/step/exception/pause) or the
    /// program terminated - either way, the appropriate DAP event was
    /// already emitted, and the caller should go back to reading requests.
    Stopped,
    /// A `disconnect`/`terminate` request arrived mid-run.
    Disconnect,
}

/// Drives `continue` in bursts until a real stop, termination, or an
/// interleaved `pause`/`disconnect` request arrives - polling `rx`
/// non-blockingly between bursts is what lets those interrupt a run without
/// a second thread ever touching `DebugSession` (see the module doc
/// comment). Any *other* request that arrives mid-run (e.g. `evaluate`,
/// `variables`, a live `setBreakpoints`) is handled immediately, right
/// here, since the paused-between-bursts state is exactly as valid a
/// snapshot as a real breakpoint stop; there's no reason to make the
/// client wait.
fn drive_continue(
    session: &mut AdapterSession,
    rx: &mpsc::Receiver<IncomingRequest>,
    out: &mut impl Write,
) -> DriveOutcome {
    let mut total_instructions: u64 = 0;
    loop {
        let burst = match session.continue_burst(BURST_INSTRUCTIONS) {
            Ok(b) => b,
            Err(message) => {
                write_event(
                    out,
                    "output",
                    json!({ "category": "stderr", "output": format!("{message}\n") }),
                );
                return DriveOutcome::Stopped;
            }
        };
        flush_output_event(session, out);

        if burst.stopped() {
            let stop = burst
                .stop()
                .expect("stopped burst always carries a StopEvent");
            let thread_id = session.current_thread_id();
            if stop.reason() == "terminated" {
                write_event(out, "terminated", json!({}));
                write_event(out, "exited", json!({ "exitCode": 0 }));
            } else {
                write_event(
                    out,
                    "stopped",
                    convert::stopped_event_body(&stop, thread_id),
                );
            }
            return DriveOutcome::Stopped;
        }

        total_instructions += BURST_INSTRUCTIONS as u64;
        if total_instructions >= MAX_CONTINUE_INSTRUCTIONS {
            write_event(
                out,
                "output",
                json!({ "category": "stderr", "output": "Execution exceeded instruction limit\n" }),
            );
            write_event(out, "terminated", json!({}));
            return DriveOutcome::Stopped;
        }

        // Drain and handle anything that arrived while we were bursting.
        while let Ok(req) = rx.try_recv() {
            match req.command.as_str() {
                "pause" => {
                    write_response(out, req.seq, &req.command, &Ok(json!({})));
                    let thread_id = session.current_thread_id();
                    write_event(
                        out,
                        "stopped",
                        json!({ "reason": "pause", "threadId": thread_id, "allThreadsStopped": true }),
                    );
                    return DriveOutcome::Stopped;
                }
                "disconnect" | "terminate" => {
                    write_response(out, req.seq, &req.command, &Ok(json!({})));
                    return DriveOutcome::Disconnect;
                }
                _ => {
                    let result = dispatch_query(session, &req.command, req.arguments)
                        .unwrap_or_else(|| {
                            Err(format!("'{}' is not valid while continuing", req.command))
                        });
                    write_response(out, req.seq, &req.command, &result);
                }
            }
        }
    }
}

fn main() -> io::Result<()> {
    let (tx, rx) = mpsc::channel::<IncomingRequest>();

    let reader_handle = thread::spawn(move || {
        let stdin = io::stdin();
        let mut reader = BufReader::new(stdin.lock());
        loop {
            match framing::read_message(&mut reader) {
                Ok(Some(value)) => {
                    let seq = value.get("seq").and_then(Value::as_i64).unwrap_or(0);
                    let command = value
                        .get("command")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let arguments = value.get("arguments").cloned().unwrap_or(Value::Null);
                    if tx
                        .send(IncomingRequest {
                            seq,
                            command,
                            arguments,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(None) => return, // EOF
                Err(_) => return,
            }
        }
    });

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut session = AdapterSession::new();

    for req in &rx {
        match req.command.as_str() {
            "configurationDone" => {
                write_response(&mut out, req.seq, &req.command, &Ok(json!({})));
                if let DriveOutcome::Disconnect = run_if_ready(&mut session, &rx, &mut out) {
                    break;
                }
            }
            "continue" => {
                write_response(
                    &mut out,
                    req.seq,
                    &req.command,
                    &Ok(json!({ "allThreadsContinued": true })),
                );
                if let DriveOutcome::Disconnect = drive_continue(&mut session, &rx, &mut out) {
                    break;
                }
            }
            "next" | "stepIn" | "stepOut" => {
                let step_result = match req.command.as_str() {
                    "next" => session.step_over(),
                    "stepIn" => session.step_into(),
                    _ => session.step_out(),
                };
                write_response(
                    &mut out,
                    req.seq,
                    &req.command,
                    &step_result
                        .as_ref()
                        .map(|_| json!({}))
                        .map_err(|e| e.clone()),
                );
                flush_output_event(&mut session, &mut out);
                if let Ok(stop) = step_result {
                    let thread_id = session.current_thread_id();
                    if stop.reason() == "terminated" {
                        write_event(&mut out, "terminated", json!({}));
                        write_event(&mut out, "exited", json!({ "exitCode": 0 }));
                    } else {
                        write_event(
                            &mut out,
                            "stopped",
                            convert::stopped_event_body(&stop, thread_id),
                        );
                    }
                }
            }
            "pause" => {
                // Nothing is running (a `continue` in flight is handled
                // inside `drive_continue`'s own poll loop) - matches
                // `debug-session.ts`'s `pause()`: a no-op outside a run.
                write_response(&mut out, req.seq, &req.command, &Ok(json!({})));
            }
            "disconnect" | "terminate" => {
                write_response(&mut out, req.seq, &req.command, &Ok(json!({})));
                break;
            }
            _ => {
                let result = dispatch_query(&mut session, &req.command, req.arguments)
                    .unwrap_or_else(|| Err(format!("unknown command '{}'", req.command)));
                let is_init = req.command == "initialize";
                write_response(&mut out, req.seq, &req.command, &result);
                if is_init && result.is_ok() {
                    write_event(&mut out, "initialized", json!({}));
                }
            }
        }
    }

    drop(rx);
    let _ = reader_handle.join();
    Ok(())
}

/// `configurationDone` starts the program - equivalent to an implicit
/// first `continue`, per the DAP launch sequence
/// (`initialize` -> `initialized` event -> `setBreakpoints`* ->
/// `configurationDone` -> the program runs).
fn run_if_ready(
    session: &mut AdapterSession,
    rx: &mpsc::Receiver<IncomingRequest>,
    out: &mut impl Write,
) -> DriveOutcome {
    if !session.is_launched() {
        return DriveOutcome::Stopped;
    }
    drive_continue(session, rx, out)
}
