//! Owns only the short-lived collection child and enforces its overall budget.

use super::protocol::{Frame, Ledger, MAX_BUFFERED, MAX_FRAME};
use serde::{Deserialize, Serialize};
use spool_shared_types::inspection::{Issue, ReadRequest};
use std::{
    io::{self, Read, Write},
    os::fd::{AsRawFd, RawFd},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[derive(Serialize, Deserialize)]
pub(crate) struct WorkerRequest {
    pub capture_id: String,
    pub request: ReadRequest,
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
fn poll(fd: RawFd, events: i16, remaining: Duration) -> io::Result<()> {
    let mut descriptor = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    let wait = i32::try_from(remaining.as_millis().min(50)).unwrap_or(50);
    if unsafe { libc::poll(&raw mut descriptor, 1, wait) } < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}
fn problem(ledger: &mut Ledger, code: &str, message: impl Into<String>) {
    ledger.issues.push(Issue {
        source: "helper".into(),
        operation: "collection".into(),
        target: None,
        code: code.into(),
        message: message.into(),
    });
}

fn decode(buffer: &mut Vec<u8>, ledger: &mut Ledger) -> Result<(), String> {
    while buffer.len() >= 4 {
        let length = u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
        if length == 0 || length > MAX_FRAME {
            buffer.clear();
            return Err("invalid helper frame length".into());
        }
        if buffer.len() < length + 4 {
            break;
        }
        let decoded = serde_json::from_slice::<Frame>(&buffer[4..4 + length])
            .map_err(|error| error.to_string());
        buffer.drain(..4 + length);
        match decoded.and_then(|frame| ledger.accept(frame)) {
            Ok(()) => {}
            Err(error) => {
                buffer.clear();
                return Err(error);
            }
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "exhaustive command/source branches share one admission or capture boundary"
)]
pub(crate) fn supervise(
    command: &mut Command,
    capture_id: String,
    request: &ReadRequest,
    cancelled: &AtomicBool,
) -> Ledger {
    let started = Instant::now();
    let budget = Duration::from_millis(request.timeout_ms);
    let mut ledger = Ledger::new(capture_id.clone());
    let input = match serde_json::to_vec(&WorkerRequest {
        capture_id,
        request: request.clone(),
    }) {
        Ok(input) if input.len() <= MAX_FRAME => input,
        _ => {
            problem(
                &mut ledger,
                "invalid_request",
                "helper request exceeds limit",
            );
            return ledger;
        }
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = match command.spawn() {
        Ok(child) => OwnedChild(child),
        Err(error) => {
            problem(&mut ledger, "helper_unavailable", error.to_string());
            return ledger;
        }
    };
    let Some(mut stdin) = child.0.stdin.take() else {
        problem(&mut ledger, "helper_io", "missing input pipe");
        return ledger;
    };
    let Some(mut stdout) = child.0.stdout.take() else {
        problem(&mut ledger, "helper_io", "missing output pipe");
        return ledger;
    };
    if let Err(error) =
        nonblocking(stdin.as_raw_fd()).and_then(|()| nonblocking(stdout.as_raw_fd()))
    {
        problem(&mut ledger, "helper_io", error.to_string());
        return ledger;
    }
    let mut request_frame = u32::try_from(input.len())
        .expect("request size was bounded")
        .to_be_bytes()
        .to_vec();
    request_frame.extend(input);
    let mut written = 0;
    while written < request_frame.len() {
        if cancelled.load(Ordering::Acquire) || started.elapsed() >= budget {
            let reason = if cancelled.load(Ordering::Acquire) {
                "interrupted"
            } else {
                "timed_out"
            };
            problem(
                &mut ledger,
                reason,
                "helper request was not fully delivered",
            );
            ledger.interrupt(reason);
            return ledger;
        }
        match stdin.write(&request_frame[written..]) {
            Ok(0) => {
                problem(&mut ledger, "helper_io", "input pipe closed");
                return ledger;
            }
            Ok(count) => written += count,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let _ = poll(
                    stdin.as_raw_fd(),
                    libc::POLLOUT,
                    budget.saturating_sub(started.elapsed()),
                );
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                problem(&mut ledger, "helper_io", error.to_string());
                return ledger;
            }
        }
    }
    drop(stdin);
    let mut buffer = Vec::new();
    let mut total = 0usize;
    let mut chunk = vec![0u8; 32768];
    let mut termination = None;
    let mut protocol_failed = false;
    loop {
        if cancelled.load(Ordering::Acquire) {
            termination = Some("interrupted");
            break;
        }
        if started.elapsed() >= budget {
            termination = Some("timed_out");
            break;
        }
        match stdout.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => {
                total = total.saturating_add(count);
                if total > MAX_BUFFERED {
                    problem(
                        &mut ledger,
                        "truncated",
                        "helper output exceeds capture limit",
                    );
                    termination = Some("interrupted");
                    break;
                }
                buffer.extend_from_slice(&chunk[..count]);
                if let Err(error) = decode(&mut buffer, &mut ledger) {
                    problem(&mut ledger, "protocol_error", error);
                    protocol_failed = true;
                    termination = Some("interrupted");
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if let Err(error) = poll(
                    stdout.as_raw_fd(),
                    libc::POLLIN,
                    budget.saturating_sub(started.elapsed()),
                ) {
                    problem(&mut ledger, "helper_io", error.to_string());
                    termination = Some("interrupted");
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                problem(&mut ledger, "helper_io", error.to_string());
                termination = Some("interrupted");
                break;
            }
        }
    }
    if let Some(reason) = termination {
        let _ = child.0.kill();
        // Only drain bytes already available; never extend the deadline by
        // waiting for a frame that the terminated worker cannot finish.
        for _ in 0..if protocol_failed { 0 } else { 4 } {
            match stdout.read(&mut chunk) {
                Ok(count) if count > 0 && total.saturating_add(count) <= MAX_BUFFERED => {
                    total += count;
                    buffer.extend_from_slice(&chunk[..count]);
                    if let Err(error) = decode(&mut buffer, &mut ledger) {
                        problem(&mut ledger, "protocol_error", error);
                        break;
                    }
                }
                _ => break,
            }
        }
        problem(
            &mut ledger,
            reason,
            "native collection stopped before the helper exited",
        );
    }
    if !buffer.is_empty() {
        problem(
            &mut ledger,
            "truncated_frame",
            "incomplete helper frame was discarded",
        );
    }
    // EOF is not permission to wait forever for a child that closed stdout.
    let status = loop {
        match child.0.try_wait() {
            Ok(Some(status)) => break Some(status),
            Err(error) => {
                problem(&mut ledger, "helper_wait", error.to_string());
                break None;
            }
            Ok(None) => {
                if termination.is_some()
                    || started.elapsed() >= budget
                    || cancelled.load(Ordering::Acquire)
                {
                    if termination.is_none() {
                        let reason = if cancelled.load(Ordering::Acquire) {
                            "interrupted"
                        } else {
                            "timed_out"
                        };
                        termination = Some(reason);
                        problem(
                            &mut ledger,
                            reason,
                            "helper closed output but did not exit within budget",
                        );
                    }
                    let _ = child.0.kill();
                    break child.0.wait().ok();
                }
                std::thread::park_timeout(Duration::from_millis(1));
            }
        }
    };
    if status.is_none_or(|status| !status.success()) && termination.is_none() {
        problem(
            &mut ledger,
            "helper_failed",
            "collection worker exited unsuccessfully",
        );
    }
    if termination.is_some() || !ledger.finished {
        ledger.interrupt(termination.unwrap_or("interrupted"));
    }
    ledger
}

#[cfg(test)]
mod tests {
    use super::super::protocol::{Frame, Message, Object, Outcome, VERSION, Work, write_frame};
    use super::*;
    use serde_json::json;
    use spool_shared_types::inspection::{ReadRequest, Resource, Source};

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "explicit protocol transcript verifies five retained fields and a blocked sixth"
    )]
    fn a_stuck_helper_is_reaped_without_losing_complete_field_frames() {
        let mut bytes = Vec::new();
        let mut messages = vec![
            Message::WorkDeclared(Work {
                id: "discover".into(),
                target: "session".into(),
                source: "cg".into(),
                scope: "windows".into(),
                dependencies: vec![],
                inventory: true,
            }),
            Message::OperationStarted {
                id: "discover".into(),
                at: "start".into(),
                offset_ms: 0,
            },
            Message::InventoryChunk {
                id: "discover".into(),
                objects: vec![Object {
                    reference: "cg:1".into(),
                    source: "cg".into(),
                    resource: Resource::Window,
                    id: Some(42),
                    pid: Some(100),
                    data: json!({}),
                }],
            },
            Message::WorkDeclared(Work {
                id: "title".into(),
                target: "cg:1".into(),
                source: "cg".into(),
                scope: "title".into(),
                dependencies: vec!["discover".into()],
                inventory: false,
            }),
            Message::OperationStarted {
                id: "title".into(),
                at: "start".into(),
                offset_ms: 1,
            },
            Message::EvidenceResult {
                id: "title".into(),
                outcome: Outcome::value("CFString", json!("preserved")),
                at: "end".into(),
                offset_ms: 2,
            },
        ];
        for index in 1..=4 {
            let id = format!("field:{index}");
            messages.push(Message::WorkDeclared(Work {
                id: id.clone(),
                target: "cg:1".into(),
                source: "cg".into(),
                scope: id.clone(),
                dependencies: vec!["discover".into()],
                inventory: false,
            }));
            messages.push(Message::OperationStarted {
                id: id.clone(),
                at: "start".into(),
                offset_ms: 2 + index * 2,
            });
            messages.push(Message::EvidenceResult {
                id: id.clone(),
                outcome: Outcome::value("Boolean", json!(false)),
                at: "end".into(),
                offset_ms: 3 + index * 2,
            });
            messages.push(Message::OperationFinished {
                id,
                at: "end".into(),
                offset_ms: 3 + index * 2,
                outcome: "value".into(),
                enumeration_complete: true,
            });
        }
        messages.push(Message::WorkDeclared(Work {
            id: "blocked-sixth".into(),
            target: "cg:1".into(),
            source: "cg".into(),
            scope: "blocked".into(),
            dependencies: vec!["discover".into()],
            inventory: false,
        }));
        messages.push(Message::OperationStarted {
            id: "blocked-sixth".into(),
            at: "start".into(),
            offset_ms: 12,
        });
        for (sequence, message) in messages.into_iter().enumerate() {
            write_frame(
                &mut bytes,
                &Frame {
                    version: VERSION,
                    capture_id: "test".into(),
                    sequence: sequence as u64,
                    message,
                },
            )
            .unwrap();
        }
        let hex = super::super::hex(&bytes);
        let mut command = std::process::Command::new("/usr/bin/python3");
        command.args(["-c","import sys,time;sys.stdout.buffer.write(bytes.fromhex(sys.argv[1]));sys.stdout.buffer.flush();time.sleep(30)",&hex]);
        let mut request = ReadRequest::detail(Resource::Window, Source::Native, Some(42));
        request.timeout_ms = 2000;
        let started = std::time::Instant::now();
        let ledger = supervise(
            &mut command,
            "test".into(),
            &request,
            &std::sync::atomic::AtomicBool::new(false),
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(ledger.objects["cg:1"].data["title"]["value"], "preserved");
        for index in 1..=4 {
            assert_eq!(
                ledger.objects["cg:1"].data[format!("field:{index}")]["value"],
                false
            );
        }
        assert!(ledger.operations["blocked-sixth"].result.is_none());
        assert!(ledger.issues.iter().any(|issue| issue.code == "timed_out"));
        assert!(!ledger.complete());
    }
    #[test]
    fn malformed_partial_and_oversized_frames_never_become_successful_empty_results() {
        for bytes in [vec![0, 0], vec![0, 0, 0, 1, b'{'], vec![255, 255, 255, 255]] {
            let mut command = Command::new("/usr/bin/python3");
            command.args(["-c", "import sys;sys.stdin.buffer.read();sys.stdout.buffer.write(bytes.fromhex(sys.argv[1]))", &super::super::hex(&bytes)]);
            let request = ReadRequest::detail(Resource::Window, Source::Native, Some(42));
            let ledger = supervise(
                &mut command,
                "test".into(),
                &request,
                &AtomicBool::new(false),
            );
            assert!(!ledger.complete());
            assert!(!ledger.issues.is_empty());
        }
    }

    #[test]
    fn cancellation_reaps_a_helper_before_the_total_deadline() {
        let mut command = Command::new("/usr/bin/python3");
        command.args(["-c", "import time;time.sleep(30)"]);
        let request = ReadRequest::detail(Resource::Window, Source::Native, Some(42));
        let started = Instant::now();
        let ledger = supervise(
            &mut command,
            "test".into(),
            &request,
            &AtomicBool::new(true),
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(
            ledger
                .issues
                .iter()
                .any(|issue| issue.code == "interrupted")
        );
    }
}
