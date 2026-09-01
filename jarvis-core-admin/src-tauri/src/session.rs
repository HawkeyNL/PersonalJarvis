use std::{
    ffi::OsStr,
    io::{self, BufRead, BufReader, BufWriter, Read, Write},
    os::fd::AsRawFd,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use wait_timeout::ChildExt;

use crate::admin::{self, LogQuery, ModelMutation, ProgramOutput, UpdateMutation};

const PKEXEC: &str = "/usr/bin/pkexec";
const INSTALLED_ADMIN_APP: &str = "/usr/bin/jarvis-core-admin";
const BROKER_ARGUMENT: &str = "--jarvis-privileged-broker";
// Vue locks at exactly five inactive minutes. The broker gets a small heartbeat
// grace so a last-moment pointer event cannot race its poll timeout; closed
// pipes still terminate it immediately when the application exits.
const IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60 + 15);
const REQUEST_LIMIT: usize = 64 * 1024;
const RESPONSE_LIMIT: usize = 2 * 1_048_576 + 64 * 1024;

type SessionResult<T> = Result<T, String>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub(crate) enum BrokerRequest {
    Touch,
    Shutdown,
    Status,
    Health,
    UpdateStatus { check: bool },
    UpdateMutation { request: UpdateMutation },
    AgentsStatus,
    AgentManifest,
    AgentAction { update: bool },
    Models,
    Usage,
    ModelMutation { request: ModelMutation },
    Credentials,
    Logs { query: LogQuery },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum BrokerResponse {
    Ready,
    Ack,
    Output {
        success: bool,
        stdout: String,
        stderr: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Serialize)]
pub struct SessionStatus {
    pub authenticated: bool,
    pub expires_in_seconds: u64,
}

pub struct SessionManager {
    broker: Mutex<Option<BrokerSession>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            broker: Mutex::new(None),
        }
    }

    pub fn authenticate(&self) -> SessionResult<SessionStatus> {
        let mut slot = self.lock_slot()?;
        if slot.as_ref().is_some_and(BrokerSession::expired) {
            close_slot(&mut slot);
        }
        if slot.is_none() {
            *slot = Some(BrokerSession::start()?);
        }
        let broker = slot.as_mut().expect("authenticated broker exists");
        broker.touch()?;
        Ok(broker.status())
    }

    pub fn touch(&self) -> SessionResult<SessionStatus> {
        let mut slot = self.lock_slot()?;
        let broker = active_broker(&mut slot)?;
        broker.touch()?;
        Ok(broker.status())
    }

    pub fn lock(&self) -> SessionResult<SessionStatus> {
        let mut slot = self.lock_slot()?;
        close_slot(&mut slot);
        Ok(SessionStatus {
            authenticated: false,
            expires_in_seconds: 0,
        })
    }

    pub fn require_active(&self) -> SessionResult<SessionStatus> {
        let mut slot = self.lock_slot()?;
        Ok(active_broker(&mut slot)?.status())
    }

    pub(crate) fn run(&self, request: BrokerRequest) -> SessionResult<ProgramOutput> {
        let mut slot = self.lock_slot()?;
        active_broker(&mut slot)?.request(request)
    }

    fn lock_slot(&self) -> SessionResult<std::sync::MutexGuard<'_, Option<BrokerSession>>> {
        self.broker
            .lock()
            .map_err(|_| "administration session state is unavailable".to_owned())
    }
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        if let Ok(slot) = self.broker.get_mut() {
            close_slot(slot);
        }
    }
}

fn active_broker(slot: &mut Option<BrokerSession>) -> SessionResult<&mut BrokerSession> {
    if slot.as_ref().is_some_and(BrokerSession::expired) {
        close_slot(slot);
    }
    slot.as_mut()
        .ok_or_else(|| "Jarvis Core Administration is locked; authenticate to continue".to_owned())
}

fn close_slot(slot: &mut Option<BrokerSession>) {
    if let Some(mut broker) = slot.take() {
        broker.close();
    }
}

struct BrokerSession {
    child: Child,
    input: BufWriter<ChildStdin>,
    output: BufReader<ChildStdout>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_reader: Option<JoinHandle<()>>,
    last_activity: Instant,
    closed: bool,
}

impl BrokerSession {
    fn start() -> SessionResult<Self> {
        admin::root_guard()?;
        admin::verify_root_executable(PKEXEC)?;
        admin::verify_root_executable(INSTALLED_ADMIN_APP)?;

        let mut child = Command::new(PKEXEC)
            .arg(INSTALLED_ADMIN_APP)
            .arg(BROKER_ARGUMENT)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("LANG", "C.UTF-8")
            .spawn()
            .map_err(|_| "could not start the system authorization boundary".to_owned())?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| "missing protected broker input".to_owned())?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| "missing protected broker output".to_owned())?;
        let stderr_pipe = child
            .stderr
            .take()
            .ok_or_else(|| "missing protected broker error channel".to_owned())?;
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let error_buffer = Arc::clone(&stderr);
        let stderr_reader = thread::spawn(move || {
            let mut pipe = stderr_pipe;
            let mut buffer = [0_u8; 4096];
            while let Ok(count) = pipe.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                let Ok(mut destination) = error_buffer.lock() else {
                    break;
                };
                let remaining = REQUEST_LIMIT.saturating_sub(destination.len());
                destination.extend_from_slice(&buffer[..count.min(remaining)]);
            }
        });
        let mut session = Self {
            child,
            input: BufWriter::new(input),
            output: BufReader::new(output),
            stderr,
            stderr_reader: Some(stderr_reader),
            last_activity: Instant::now(),
            closed: false,
        };
        match session.read_response() {
            Ok(BrokerResponse::Ready) => Ok(session),
            Ok(_) => {
                session.close();
                Err("trusted administration broker returned an invalid greeting".to_owned())
            }
            Err(_) => {
                session.close();
                let detail = session.stderr_text();
                Err(if detail.is_empty() {
                    "administrator authentication was cancelled or failed".to_owned()
                } else {
                    detail
                })
            }
        }
    }

    fn expired(&self) -> bool {
        self.last_activity.elapsed() >= IDLE_TIMEOUT
    }

    fn status(&self) -> SessionStatus {
        SessionStatus {
            authenticated: !self.expired(),
            expires_in_seconds: IDLE_TIMEOUT
                .saturating_sub(self.last_activity.elapsed())
                .as_secs(),
        }
    }

    fn touch(&mut self) -> SessionResult<()> {
        match self.exchange(&BrokerRequest::Touch)? {
            BrokerResponse::Ack => {
                self.last_activity = Instant::now();
                Ok(())
            }
            BrokerResponse::Error { message } => Err(message),
            _ => Err("trusted administration broker returned an invalid response".to_owned()),
        }
    }

    fn request(&mut self, request: BrokerRequest) -> SessionResult<ProgramOutput> {
        match self.exchange(&request)? {
            BrokerResponse::Output {
                success,
                stdout,
                stderr,
            } => {
                self.last_activity = Instant::now();
                Ok(ProgramOutput {
                    success,
                    stdout,
                    stderr,
                })
            }
            BrokerResponse::Error { message } => {
                self.last_activity = Instant::now();
                Err(message)
            }
            _ => Err("trusted administration broker returned an invalid response".to_owned()),
        }
    }

    fn exchange(&mut self, request: &BrokerRequest) -> SessionResult<BrokerResponse> {
        serde_json::to_writer(&mut self.input, request)
            .map_err(|_| "could not encode typed administration request".to_owned())?;
        self.input
            .write_all(b"\n")
            .and_then(|_| self.input.flush())
            .map_err(|_| "trusted administration session ended".to_owned())?;
        self.read_response()
    }

    fn read_response(&mut self) -> SessionResult<BrokerResponse> {
        let line = read_limited_line(&mut self.output, RESPONSE_LIMIT)?;
        if line.is_empty() {
            return Err("trusted administration session ended".to_owned());
        }
        serde_json::from_slice(&line)
            .map_err(|_| "trusted administration broker returned invalid data".to_owned())
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let _ = serde_json::to_writer(&mut self.input, &BrokerRequest::Shutdown);
        let _ = self.input.write_all(b"\n");
        let _ = self.input.flush();
        drop(self.child.stdin.take());
        if self
            .child
            .wait_timeout(Duration::from_secs(2))
            .ok()
            .flatten()
            .is_none()
        {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
    }

    fn stderr_text(&self) -> String {
        self.stderr
            .lock()
            .ok()
            .map(|bytes| admin::safe_text(&bytes))
            .unwrap_or_default()
    }
}

impl Drop for BrokerSession {
    fn drop(&mut self) {
        self.close();
    }
}

pub fn broker_requested() -> bool {
    let mut args = std::env::args_os();
    let _program = args.next();
    args.next().as_deref() == Some(OsStr::new(BROKER_ARGUMENT)) && args.next().is_none()
}

pub fn run_broker() -> SessionResult<()> {
    if unsafe { libc::geteuid() } != 0 || std::env::var_os("PKEXEC_UID").is_none() {
        return Err(
            "privileged broker must be started by the system authorization boundary".to_owned(),
        );
    }
    let stdin = io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut output = BufWriter::new(stdout.lock());
    write_response(&mut output, &BrokerResponse::Ready)?;
    let mut last_activity = Instant::now();

    loop {
        if !wait_for_input(
            input.get_ref().as_raw_fd(),
            IDLE_TIMEOUT.saturating_sub(last_activity.elapsed()),
        )? {
            break;
        }
        let line = read_limited_line(&mut input, REQUEST_LIMIT)?;
        if line.is_empty() {
            break;
        }
        let request = match serde_json::from_slice::<BrokerRequest>(&line) {
            Ok(request) => request,
            Err(_) => {
                write_response(
                    &mut output,
                    &BrokerResponse::Error {
                        message: "invalid typed administration request".to_owned(),
                    },
                )?;
                continue;
            }
        };
        last_activity = Instant::now();
        match request {
            BrokerRequest::Touch => write_response(&mut output, &BrokerResponse::Ack)?,
            BrokerRequest::Shutdown => {
                write_response(&mut output, &BrokerResponse::Ack)?;
                break;
            }
            request => {
                let response = match admin::run_broker_request(request) {
                    Ok(result) => BrokerResponse::Output {
                        success: result.success,
                        stdout: result.stdout,
                        stderr: result.stderr,
                    },
                    Err(message) => BrokerResponse::Error {
                        message: admin::sanitize_error(&message),
                    },
                };
                write_response(&mut output, &response)?;
                // A bounded trusted operation may legitimately run longer
                // than the idle window. Completion starts a fresh window;
                // transactional children are never killed just to lock UI.
                last_activity = Instant::now();
            }
        }
    }
    Ok(())
}

fn wait_for_input(fd: i32, timeout: Duration) -> SessionResult<bool> {
    let milliseconds = timeout.as_millis().min(i32::MAX as u128) as i32;
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut descriptor, 1, milliseconds) };
    if result < 0 {
        Err("could not monitor privileged broker input".to_owned())
    } else {
        Ok(result > 0 && descriptor.revents & (libc::POLLIN | libc::POLLHUP) != 0)
    }
}

fn write_response(writer: &mut impl Write, response: &BrokerResponse) -> SessionResult<()> {
    serde_json::to_writer(&mut *writer, response)
        .map_err(|_| "could not encode privileged broker response".to_owned())?;
    writer
        .write_all(b"\n")
        .and_then(|_| writer.flush())
        .map_err(|_| "could not write privileged broker response".to_owned())
}

fn read_limited_line(reader: &mut impl BufRead, limit: usize) -> SessionResult<Vec<u8>> {
    let mut result = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|_| "could not read administration protocol".to_owned())?;
        if available.is_empty() {
            return Ok(result);
        }
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if result.len().saturating_add(count) > limit {
            return Err("administration protocol message exceeded its limit".to_owned());
        }
        result.extend_from_slice(&available[..count]);
        reader.consume(count);
        if result.last() == Some(&b'\n') {
            result.pop();
            return Ok(result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_rejects_unbounded_lines() {
        let mut input = BufReader::new(&b"123456\n"[..]);
        assert!(read_limited_line(&mut input, 5).is_err());
    }

    #[test]
    fn broker_protocol_has_no_arbitrary_command_variant() {
        let request =
            serde_json::from_str::<BrokerRequest>(r#"{"operation":"shell","command":"id"}"#);
        assert!(request.is_err());
    }
}
