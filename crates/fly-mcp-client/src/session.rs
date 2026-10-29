//! Talking to one MCP server with rmcp.
//!
//! FOTW declares no client capabilities: no sampling (a server can't make
//! FOTW's model write anything), no roots (no local paths are shared) and no
//! elicitation. Requests for any of them are answered with "method not
//! found". Multi-round `input_required` results and task results are refused
//! too.
//!
//! Stdio servers start with a cleared environment plus a short allowlist
//! (PATH, home and temp folders, locale) and the variables the user stored
//! for that connector; their stderr is discarded and no console window
//! opens. Messages are capped at [`MAX_MESSAGE_BYTES`] on every transport.

// rmcp marks sampling and roots deprecated (SEP-2577); FOTW only refuses them.
#![allow(deprecated)]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ClientCapabilities, ClientConfig,
    CreateMessageRequestParams, CreateMessageResult, ElicitRequestParams, ElicitResult, ErrorCode,
    Implementation, ListRootsResult,
};
use rmcp::service::{NotificationContext, RequestContext, RoleClient, RunningService};
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::{ClientHandler, ErrorData as McpError, ServiceExt};
use serde_json::{Map, Value};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::config::{ServerConfig, TransportConfig};
use crate::fence::ToolResult;
use crate::netguard::{guarded_client, NetPolicy, Redirects};
use crate::pin::{LiveTool, MAX_TOOLS_PER_SERVER};

/// Largest single message accepted from a server.
pub const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
pub const LIST_TIMEOUT: Duration = Duration::from_secs(20);
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("couldn't start the connector program: {0}")]
    Spawn(String),
    #[error("couldn't find {0}")]
    ProgramNotFound(String),
    /// The program stopped right away; the end of its error output.
    #[error("the program stopped right away")]
    Exited(String),
    #[error("couldn't reach the server: {0}")]
    Connect(String),
    #[error("the server asked FOTW to sign in again")]
    AuthRequired,
    #[error("the server took too long to answer")]
    Timeout,
    #[error("the server's tool list was invalid: {0}")]
    Listing(String),
    #[error("the call failed: {0}")]
    Call(String),
    #[error("{0}")]
    Unsupported(String),
}

/// Counts what the server tried that FOTW refused (for tests and logs).
#[derive(Debug, Default)]
pub struct Refusals {
    pub sampling: AtomicU32,
    pub roots: AtomicU32,
    pub elicitation: AtomicU32,
}

#[derive(Clone)]
struct FotwClient {
    tools_changed: Arc<AtomicBool>,
    refusals: Arc<Refusals>,
}

impl ClientHandler for FotwClient {
    fn get_info(&self) -> ClientConfig {
        // No capabilities at all: no sampling, roots or elicitation. The
        // legacy `initialize` handshake works with every hosted server today.
        let mut cfg = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("fly-on-the-wall", env!("CARGO_PKG_VERSION")),
        );
        cfg.protocol_version = rmcp::model::ProtocolVersion::LATEST_WITH_INITIALIZE;
        cfg
    }

    async fn create_message(
        &self,
        _params: CreateMessageRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<CreateMessageResult, McpError> {
        self.refusals.sampling.fetch_add(1, Ordering::Relaxed);
        tracing::warn!("a connector asked for sampling; refused");
        Err(McpError::new(
            ErrorCode::METHOD_NOT_FOUND,
            "sampling is not supported",
            None,
        ))
    }

    async fn list_roots(
        &self,
        _context: RequestContext<RoleClient>,
    ) -> Result<ListRootsResult, McpError> {
        self.refusals.roots.fetch_add(1, Ordering::Relaxed);
        tracing::warn!("a connector asked for roots; refused");
        Err(McpError::new(
            ErrorCode::METHOD_NOT_FOUND,
            "roots are not supported",
            None,
        ))
    }

    async fn create_elicitation(
        &self,
        _request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, McpError> {
        self.refusals.elicitation.fetch_add(1, Ordering::Relaxed);
        Err(McpError::new(
            ErrorCode::METHOD_NOT_FOUND,
            "elicitation is not supported",
            None,
        ))
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.tools_changed.store(true, Ordering::Relaxed);
    }
}

/// A live connection. Dropping it closes the session (and stops a stdio
/// program).
pub struct Connection {
    service: RunningService<RoleClient, FotwClient>,
    tools_changed: Arc<AtomicBool>,
    refusals: Arc<Refusals>,
    _child: Option<tokio::process::Child>,
    /// Dropped last: stops every process the program started.
    _tree: Option<ProcessTree>,
}

/// A local connector's process tree. Dropping it kills every process in it,
/// so a runner like `npx.cmd` can't leave `node` behind.
pub struct ProcessTree {
    #[cfg(windows)]
    job: windows_sys::Win32::Foundation::HANDLE,
    #[cfg(unix)]
    pgid: i32,
}

// The job handle is owned and only closed once, on drop.
#[cfg(windows)]
unsafe impl Send for ProcessTree {}
#[cfg(windows)]
unsafe impl Sync for ProcessTree {}

impl ProcessTree {
    /// Put `child` (and everything it starts later) under one handle.
    #[cfg(windows)]
    fn adopt(child: &tokio::process::Child) -> Option<Self> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        let process = child.raw_handle()?;
        // SAFETY: plain Win32 calls on handles we own; the job handle is
        // closed exactly once (here on failure, or in Drop).
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set == 0 || AssignProcessToJobObject(job, process as _) == 0 {
                CloseHandle(job);
                return None;
            }
            Some(Self { job })
        }
    }

    #[cfg(unix)]
    fn adopt(child: &tokio::process::Child) -> Option<Self> {
        // The child leads its own process group (set before spawn).
        child.id().map(|pid| Self { pgid: pid as i32 })
    }

    #[cfg(not(any(windows, unix)))]
    fn adopt(_child: &tokio::process::Child) -> Option<Self> {
        None
    }
}

/// Resume a process started with `CREATE_SUSPENDED` (its only thread).
#[cfg(windows)]
fn resume_suspended(child: &tokio::process::Child) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};
    let pid = child.id().ok_or("the program stopped before it started")?;
    let mut resumed = 0;
    // SAFETY: a thread snapshot we own and close; threads of our own child,
    // opened only to resume them and closed again.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snap == INVALID_HANDLE_VALUE {
            return Err("couldn't start the program".into());
        }
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut ok = Thread32First(snap, &mut entry) != 0;
        while ok {
            if entry.th32OwnerProcessID == pid {
                let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                if !thread.is_null() {
                    if ResumeThread(thread) != u32::MAX {
                        resumed += 1;
                    }
                    CloseHandle(thread);
                }
            }
            ok = Thread32Next(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
    }
    if resumed == 0 {
        return Err("couldn't start the program".into());
    }
    Ok(())
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        // SAFETY: closing our own job handle (KILL_ON_JOB_CLOSE ends every
        // process in it) / signalling our own child's process group.
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.job);
        }
        #[cfg(unix)]
        unsafe {
            libc::killpg(self.pgid, libc::SIGKILL);
        }
    }
}

/// Inputs for [`connect`] the configuration doesn't hold: resolved values.
#[derive(Default)]
pub struct ConnectOptions {
    /// Sent as `Authorization: Bearer …` (OAuth access token or API key).
    pub bearer: Option<String>,
    /// Extra request headers for a web connector (config values and secret
    /// values from the keychain). Only names in the config are sent.
    pub headers: Vec<(String, String)>,
    /// Variables for a local connector (config values and secret values from
    /// the keychain). Only names in the config are set.
    pub env: Vec<(String, String)>,
}

/// How much of a local program's error output is kept for "Show details".
pub const STDERR_TAIL_BYTES: usize = 4096;

/// Find a program the way a shell would: a path as given, otherwise each
/// folder on PATH (with PATHEXT's extensions on Windows, so `npx` finds
/// `npx.cmd`).
pub fn resolve_program(command: &str) -> Result<std::path::PathBuf, SessionError> {
    use std::path::{Path, PathBuf};
    let given = Path::new(command);
    if given.is_absolute() || command.contains(['/', '\\']) {
        return if given.is_file() {
            Ok(given.to_path_buf())
        } else {
            Err(SessionError::ProgramNotFound(command.to_string()))
        };
    }
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        // A relative PATH entry (like `.`) depends on the working directory.
        if !dir.is_absolute() {
            continue;
        }
        for ext in &exts {
            let candidate: PathBuf = dir.join(format!("{command}{ext}"));
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(SessionError::ProgramNotFound(command.to_string()))
}

pub async fn connect(
    config: &ServerConfig,
    policy: NetPolicy,
    opts: ConnectOptions,
) -> Result<Connection, SessionError> {
    match &config.transport {
        TransportConfig::Stdio { command, args, env } => {
            // The program was pinned to a full path when the user approved it
            // (`resolve_program`); a bare name here was never shown in full.
            let program = std::path::PathBuf::from(command);
            if !program.is_absolute() {
                return Err(SessionError::Spawn(
                    "the program isn't pinned to a full path; edit the connector to approve it"
                        .into(),
                ));
            }
            if !program.is_file() {
                return Err(SessionError::ProgramNotFound(command.clone()));
            }
            let mut cmd = tokio::process::Command::new(&program);
            cmd.args(args).env_clear();
            for name in crate::config::INHERITED_ENV {
                if let Some(v) = std::env::var_os(name) {
                    cmd.env(name, v);
                }
            }
            for (k, v) in &opts.env {
                if env.iter().any(|e| &e.name == k) {
                    cmd.env(k, v);
                }
            }
            cmd.stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true);
            #[cfg(windows)]
            {
                // Started suspended, put in its Job Object, then resumed: it
                // can't start anything outside the job first.
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                const CREATE_SUSPENDED: u32 = 0x0000_0004;
                cmd.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
            }
            #[cfg(unix)]
            cmd.process_group(0);
            let mut child = cmd
                .spawn()
                .map_err(|e| SessionError::Spawn(e.to_string()))?;
            let tree = ProcessTree::adopt(&child);
            if tree.is_none() {
                tracing::warn!(
                    "couldn't group a connector's processes; only the first will be stopped"
                );
            }
            #[cfg(windows)]
            if let Err(e) = resume_suspended(&child) {
                let _ = child.start_kill();
                return Err(SessionError::Spawn(e));
            }
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| SessionError::Spawn("no stdout".into()))?;
            let stdin = child
                .stdin
                .take()
                .ok_or_else(|| SessionError::Spawn("no stdin".into()))?;
            let tail = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
            if let Some(mut err) = child.stderr.take() {
                let tail = tail.clone();
                tokio::spawn(async move {
                    use tokio::io::AsyncReadExt;
                    let mut buf = [0u8; 1024];
                    while let Ok(n) = err.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        let mut t = tail.lock().unwrap();
                        t.extend(&buf[..n]);
                        while t.len() > STDERR_TAIL_BYTES {
                            t.pop_front();
                        }
                    }
                });
            }
            match connect_io(stdout, stdin).await {
                Ok(mut conn) => {
                    conn._child = Some(child);
                    conn._tree = tree;
                    Ok(conn)
                }
                Err(e) => {
                    // Let the program's last words arrive, then report them.
                    let exited = tokio::time::timeout(Duration::from_millis(500), child.wait())
                        .await
                        .is_ok();
                    let text = {
                        let t = tail.lock().unwrap();
                        let bytes: Vec<u8> = t.iter().copied().collect();
                        String::from_utf8_lossy(&bytes).into_owned()
                    };
                    if exited || !text.trim().is_empty() {
                        Err(SessionError::Exited(text))
                    } else {
                        Err(e)
                    }
                }
            }
        }
        TransportConfig::Http { url, headers } => {
            let url = policy
                .parse_and_check(url)
                .map_err(|e| SessionError::Connect(e.to_string()))?;
            let client = guarded_client(policy, Redirects::Stop)
                .map_err(|e| SessionError::Connect(e.to_string()))?;
            let mut extra = std::collections::HashMap::new();
            for (k, v) in &opts.headers {
                if !headers.iter().any(|h| h.name.eq_ignore_ascii_case(k)) {
                    continue;
                }
                let name = http::HeaderName::from_bytes(k.as_bytes())
                    .map_err(|_| SessionError::Connect(format!("invalid header {k}")))?;
                let mut value = http::HeaderValue::from_str(v)
                    .map_err(|_| SessionError::Connect(format!("invalid value for {k}")))?;
                value.set_sensitive(true);
                extra.insert(name, value);
            }
            let mut cfg = StreamableHttpClientTransportConfig::with_uri(url.as_str())
                .max_sse_event_size(MAX_MESSAGE_BYTES)
                .custom_headers(extra);
            if let Some(t) = opts.bearer {
                cfg = cfg.auth_header(t);
            }
            let transport = StreamableHttpClientTransport::with_client(client, cfg);
            let (handler, tools_changed, refusals) = handler();
            let service = tokio::time::timeout(CONNECT_TIMEOUT, handler.serve(transport))
                .await
                .map_err(|_| SessionError::Timeout)?
                .map_err(|e| classify_init_error(e.to_string()))?;
            Ok(Connection {
                service,
                tools_changed,
                refusals,
                _child: None,
                _tree: None,
            })
        }
    }
}

fn handler() -> (FotwClient, Arc<AtomicBool>, Arc<Refusals>) {
    let tools_changed = Arc::new(AtomicBool::new(false));
    let refusals = Arc::new(Refusals::default());
    (
        FotwClient {
            tools_changed: tools_changed.clone(),
            refusals: refusals.clone(),
        },
        tools_changed,
        refusals,
    )
}

fn classify_init_error(msg: String) -> SessionError {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("401") || lower.contains("unauthorized") || lower.contains("auth required") {
        SessionError::AuthRequired
    } else {
        SessionError::Connect(msg)
    }
}

/// Connect over any byte stream (stdio pipes, or an in-memory pipe in tests).
pub async fn connect_io<R, W>(read: R, write: W) -> Result<Connection, SessionError>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let transport = AsyncRwTransport::<RoleClient, R, W>::new_client(read, write)
        .with_max_line_length(MAX_MESSAGE_BYTES);
    let (handler, tools_changed, refusals) = handler();
    let service = tokio::time::timeout(CONNECT_TIMEOUT, handler.serve(transport))
        .await
        .map_err(|_| SessionError::Timeout)?
        .map_err(|e| SessionError::Connect(e.to_string()))?;
    Ok(Connection {
        service,
        tools_changed,
        refusals,
        _child: None,
        _tree: None,
    })
}

impl Connection {
    /// Every tool the server lists (paginated, bounded).
    pub async fn list_tools(&self) -> Result<Vec<LiveTool>, SessionError> {
        self.tools_changed.store(false, Ordering::Relaxed);
        let fut = async {
            let mut tools = Vec::new();
            let mut cursor = None;
            for _page in 0..32 {
                let page = self
                    .service
                    .list_tools(Some(
                        rmcp::model::PaginatedRequestParams::default().with_cursor(cursor),
                    ))
                    .await
                    .map_err(|e| SessionError::Listing(e.to_string()))?;
                tools.extend(page.tools.iter().map(LiveTool::from));
                if tools.len() > MAX_TOOLS_PER_SERVER {
                    return Err(SessionError::Listing("too many tools".into()));
                }
                cursor = page.next_cursor;
                if cursor.is_none() {
                    return Ok(tools);
                }
            }
            Err(SessionError::Listing("too many pages".into()))
        };
        tokio::time::timeout(LIST_TIMEOUT, fut)
            .await
            .map_err(|_| SessionError::Timeout)?
    }

    /// Call one tool. One round only: a server asking for more input gets an
    /// error, not an answer.
    pub async fn call_tool(
        &self,
        name: &str,
        args: Map<String, Value>,
        timeout: Duration,
    ) -> Result<ToolResult, SessionError> {
        let params = CallToolRequestParams::new(name.to_string()).with_arguments(args);
        let resp = tokio::time::timeout(timeout, self.service.call_tool_once(params))
            .await
            .map_err(|_| SessionError::Timeout)?
            .map_err(|e| SessionError::Call(e.to_string()))?;
        match resp {
            CallToolResponse::Complete(r) => Ok(ToolResult::from(&r)),
            _ => Err(SessionError::Unsupported(
                "The service asked for more input or started a background task, which FOTW \
                 doesn't support."
                    .into(),
            )),
        }
    }

    /// The server announced its tool list changed since the last listing.
    pub fn tools_changed(&self) -> bool {
        self.tools_changed.load(Ordering::Relaxed)
    }

    pub fn refusals(&self) -> &Refusals {
        &self.refusals
    }

    pub async fn close(self) {
        let _ = self.service.cancel().await;
    }
}
