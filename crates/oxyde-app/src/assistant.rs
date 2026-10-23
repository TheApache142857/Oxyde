//! The assistant panel's plumbing, apart from its look (`assistant_ui.rs`).
//!
//! The panel talks to a coding agent installed on this computer (Claude Code or Codex,
//! through their adapters) over the [Agent Client Protocol](https://agentclientprotocol.com):
//! JSON-RPC 2.0, one message per line on the agent's standard input and output. The agent
//! runs under the user's own account with it; Oxyde sells or proxies no model.
//!
//! The agent models through Oxyde's MCP tools. When the session starts, the panel gives it
//! one MCP server to start: this program, as `oxyde --mcp --window <address> <token>`, which
//! only passes messages between the agent and this window over a local socket
//! (`oxyde_mcp::window`). The window answers them on the document open here, with the
//! window's own kernel ([`WindowServer`], `OxydeApp::serve_assistant`).

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use eframe::egui;
use serde_json::{Value, json};

/// The protocol's version this client speaks (ACP's major version).
const PROTOCOL_VERSION: u64 = 1;

/// How much of the agent's standard error is kept, to show why it stopped.
const STDERR_KEPT: usize = 4000;

/// An agent the panel knows how to find and install.
pub struct KnownAgent {
    pub name: &'static str,
    /// Its program, as npm installs it.
    pub command: &'static str,
    pub install: &'static str,
}

pub const KNOWN_AGENTS: [KnownAgent; 2] = [
    KnownAgent {
        name: "Claude Code",
        command: "claude-agent-acp",
        install: "npm install -g @agentclientprotocol/claude-agent-acp",
    },
    KnownAgent {
        name: "Codex",
        command: "codex-acp",
        install: "npm install -g @agentclientprotocol/codex-acp",
    },
];

/// An agent found on this computer.
#[derive(Debug, Clone, PartialEq)]
pub struct FoundAgent {
    pub name: String,
    pub program: PathBuf,
}

/// Looks for the known agents on `PATH` and in npm's global folder (`%APPDATA%\npm`), which
/// a program started from the desktop may not have on its `PATH`.
pub fn find_agents() -> Vec<FoundAgent> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        dirs.push(PathBuf::from(appdata).join("npm"));
    }
    KNOWN_AGENTS
        .iter()
        .filter_map(|agent| {
            find_program(agent.command, &dirs).map(|program| FoundAgent {
                name: agent.name.to_owned(),
                program,
            })
        })
        .collect()
}

/// `name` in the first of `dirs` that has it: on Windows as an `.exe`, or as the `.cmd`
/// script npm makes.
pub fn find_program(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        ["exe", "cmd", "bat"]
            .iter()
            .map(|extension| format!("{name}.{extension}"))
            .collect()
    } else {
        vec![name.to_owned()]
    };
    dirs.iter()
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|path| path.is_file())
}

/// A random token, so that only the agent this window started reaches it: the standard
/// library's hasher keys are random for each process and each `RandomState`.
fn token() -> String {
    let mut out = String::new();
    for _ in 0..2 {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        out.push_str(&format!("{:016x}", hasher.finish()));
    }
    out
}

// ---- The window's end of the MCP bridge ---------------------------------------------------

/// An MCP message from the agent, to answer on the window's document.
pub struct WindowRequest {
    pub message: Value,
    writer: Arc<Mutex<TcpStream>>,
}

impl WindowRequest {
    pub fn answer(&self, answer: &Value) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writeln!(writer, "{answer}");
            let _ = writer.flush();
        }
    }
}

/// The local socket the bridge the agent starts connects to.
pub struct WindowServer {
    pub address: String,
    pub token: String,
    pub requests: Receiver<WindowRequest>,
}

impl WindowServer {
    /// Listens on a free port of the loopback interface. Each message that arrives wakes
    /// the window up through `ctx`.
    pub fn start(ctx: Option<egui::Context>) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?.to_string();
        let token = token();
        let (send, requests) = channel();
        let expected = token.clone();
        std::thread::Builder::new()
            .name("assistant-mcp".to_owned())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let (send, expected, ctx) = (send.clone(), expected.clone(), ctx.clone());
                    let _ = std::thread::Builder::new()
                        .name("assistant-mcp-connection".to_owned())
                        .spawn(move || serve_connection(stream, &expected, &send, ctx.as_ref()));
                }
            })?;
        Ok(Self {
            address,
            token,
            requests,
        })
    }
}

/// Reads one bridge's messages: its token first, then one JSON-RPC message per line.
fn serve_connection(
    stream: TcpStream,
    token: &str,
    send: &Sender<WindowRequest>,
    ctx: Option<&egui::Context>,
) {
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let writer = Arc::new(Mutex::new(writer));
    let mut lines = BufReader::new(stream).lines();
    match lines.next() {
        Some(Ok(hello)) if oxyde_mcp::window::hello_matches(&hello, token) => {}
        _ => return,
    }
    for line in lines {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let message = serde_json::from_str(&line).unwrap_or_else(|e| {
            json!({ "jsonrpc": "2.0", "id": null, "method": "", "_parse_error": e.to_string() })
        });
        let request = WindowRequest {
            message,
            writer: Arc::clone(&writer),
        };
        if send.send(request).is_err() {
            return;
        }
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    }
}

// ---- The agent -----------------------------------------------------------------------------

/// What the agent's reader thread passes on.
enum FromAgent {
    Message(Value),
    /// Its standard output closed: it stopped.
    Ended,
}

/// What a request sent to the agent was, to read its answer.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Sent {
    Initialize,
    NewSession,
    Prompt,
}

/// A running agent.
struct Agent {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    messages: Receiver<FromAgent>,
    stderr: Arc<Mutex<String>>,
    next_id: u64,
    sent: HashMap<u64, Sent>,
}

impl Agent {
    fn start(program: &Path, args: &[String], ctx: Option<egui::Context>) -> Result<Self, String> {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // No console window for the agent (CREATE_NO_WINDOW).
            command.creation_flags(0x0800_0000);
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("{} could not start: {e}", program.display()))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            return Err("the agent's pipes could not be opened".to_owned());
        };
        let (send, messages) = channel();
        std::thread::Builder::new()
            .name("assistant-agent".to_owned())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    // Anything that is not a message (a stray print) is left out.
                    if let Ok(message) = serde_json::from_str::<Value>(&line) {
                        if send.send(FromAgent::Message(message)).is_err() {
                            return;
                        }
                        if let Some(ctx) = &ctx {
                            ctx.request_repaint();
                        }
                    }
                }
                let _ = send.send(FromAgent::Ended);
                if let Some(ctx) = &ctx {
                    ctx.request_repaint();
                }
            })
            .map_err(|e| e.to_string())?;
        let kept = Arc::new(Mutex::new(String::new()));
        let tail = Arc::clone(&kept);
        let _ = std::thread::Builder::new()
            .name("assistant-agent-stderr".to_owned())
            .spawn(move || {
                for line in BufReader::new(stderr).lines() {
                    let Ok(line) = line else { break };
                    if let Ok(mut tail) = tail.lock() {
                        tail.push_str(&line);
                        tail.push('\n');
                        if tail.len() > STDERR_KEPT {
                            let cut = tail.len() - STDERR_KEPT;
                            let cut = (cut..tail.len())
                                .find(|i| tail.is_char_boundary(*i))
                                .unwrap_or(0);
                            tail.replace_range(..cut, "");
                        }
                    }
                }
            });
        Ok(Self {
            child: Some(child),
            stdin: Some(stdin),
            messages,
            stderr: kept,
            next_id: 1,
            sent: HashMap::new(),
        })
    }

    fn write(&mut self, message: &Value) -> Result<(), String> {
        let stdin = self.stdin.as_mut().ok_or("the agent is stopping")?;
        writeln!(stdin, "{message}")
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("the agent stopped listening: {e}"))
    }

    fn request(&mut self, kind: Sent, method: &str, params: Value) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.sent.insert(id, kind);
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        Ok(id)
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.write(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    fn respond(&mut self, id: &Value, result: Result<Value, (i64, String)>) -> Result<(), String> {
        let message = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
            }
        };
        self.write(&message)
    }

    fn stderr_tail(&self) -> String {
        self.stderr
            .lock()
            .map(|tail| tail.trim().to_owned())
            .unwrap_or_default()
    }
}

/// How long a stopped agent has to end by itself before it is killed.
const STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

impl Drop for Agent {
    /// Closing its input asks the agent to end, which lets it end what it started too: on
    /// Windows, killing npm's `.cmd` script would leave Node running. Killed if it has not
    /// ended after [`STOP_GRACE`], off the window's thread.
    fn drop(&mut self) {
        self.stdin = None;
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("assistant-agent-stop".to_owned())
            .spawn(move || {
                let start = std::time::Instant::now();
                while start.elapsed() < STOP_GRACE {
                    if !matches!(child.try_wait(), Ok(None)) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                let _ = child.kill();
                let _ = child.wait();
            });
    }
}

// ---- The conversation ----------------------------------------------------------------------

/// One thing shown in the conversation.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    User(String),
    Agent(String),
    /// The agent's reasoning, as it shares it.
    Thought(String),
    /// A tool the agent calls: its id, what it says it does, and how far it got.
    Tool {
        id: String,
        title: String,
        status: String,
    },
    /// The agent asks before doing something: the options it gives, until one is chosen.
    Permission {
        request: Value,
        title: String,
        options: Vec<PermissionOption>,
        chosen: Option<String>,
    },
    /// Something the panel says: how the agent is doing, a stop.
    Note(String),
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionOption {
    pub id: String,
    pub name: String,
    /// `allow_once`, `allow_always`, `reject_once` or `reject_always`.
    pub kind: String,
}

/// Where the conversation stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum State {
    #[default]
    NotConnected,
    Starting,
    Ready,
    /// A reply is under way.
    Working,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::NotConnected => "Not connected",
            State::Starting => "Starting",
            State::Ready => "Ready",
            State::Working => "Working",
        }
    }
}

/// The assistant panel's state.
#[derive(Default)]
pub struct Assistant {
    pub open: bool,
    /// The agents found, once looked for.
    pub found: Option<Vec<FoundAgent>>,
    /// The agent chosen: an index into `found`, or past its end for `other_command`.
    pub choice: usize,
    /// Any other agent speaking ACP: its program and arguments, separated by spaces.
    pub other_command: String,
    pub entries: Vec<Entry>,
    pub input: String,
    pub state: State,
    agent: Option<Agent>,
    pub(crate) server: Option<WindowServer>,
    session_id: Option<String>,
    /// A message typed before the session was ready, sent once it is.
    queued: Option<String>,
    /// The MCP session that answers the agent's tools on this window's document.
    pub(crate) mcp: Option<oxyde_mcp::Session>,
    /// While a reply is under way: whether it changed the document yet, so that all it
    /// changes is one undo step.
    pub(crate) turn_changed: Option<bool>,
}

impl Assistant {
    /// The program and arguments of the chosen agent.
    fn command(&self) -> Option<(PathBuf, Vec<String>)> {
        match self.found.as_ref().and_then(|found| found.get(self.choice)) {
            Some(agent) => Some((agent.program.clone(), Vec::new())),
            None => {
                let mut words = self.other_command.split_whitespace().map(str::to_owned);
                let program = words.next()?;
                Some((PathBuf::from(program), words.collect()))
            }
        }
    }

    /// Starts the chosen agent; the session with it starts once it answers (in the folder
    /// [`Assistant::poll`] is given). `ctx` wakes the window when the agent says something.
    pub fn connect(&mut self, ctx: Option<egui::Context>) {
        let queued = self.queued.take();
        self.disconnect();
        self.queued = queued;
        let Some((program, args)) = self.command() else {
            self.entries.push(Entry::Error(
                "Choose an agent, or type the command of one".to_owned(),
            ));
            return;
        };
        if self.server.is_none() {
            match WindowServer::start(ctx.clone()) {
                Ok(server) => self.server = Some(server),
                Err(e) => {
                    self.entries.push(Entry::Error(format!(
                        "Oxyde's tools could not be offered: {e}"
                    )));
                    return;
                }
            }
        }
        let mut agent = match Agent::start(&program, &args, ctx) {
            Ok(agent) => agent,
            Err(e) => {
                self.entries.push(Entry::Error(e));
                return;
            }
        };
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "clientCapabilities": {
                "fs": { "readTextFile": false, "writeTextFile": false },
                "terminal": false,
            },
            "clientInfo": { "name": "oxyde", "title": "Oxyde", "version": env!("CARGO_PKG_VERSION") },
        });
        if let Err(e) = agent.request(Sent::Initialize, "initialize", params) {
            self.entries.push(Entry::Error(e));
            return;
        }
        self.agent = Some(agent);
        self.state = State::Starting;
    }

    /// Stops the agent; the conversation stays on screen.
    pub fn disconnect(&mut self) {
        self.agent = None;
        self.session_id = None;
        self.queued = None;
        self.turn_changed = None;
        self.state = State::NotConnected;
        for entry in &mut self.entries {
            if let Entry::Permission { chosen, .. } = entry {
                chosen.get_or_insert_with(|| "cancelled".to_owned());
            }
        }
    }

    /// Stops the agent and clears the conversation. (The window frees the MCP session's
    /// shapes first: `OxydeApp::assistant_header`.)
    pub fn new_chat(&mut self) {
        self.disconnect();
        self.entries.clear();
        self.mcp = None;
    }

    /// Sends what the user typed. Before the session is ready, it waits for it; with no
    /// agent running, the chosen one starts.
    pub fn send(&mut self, text: String, ctx: Option<egui::Context>) {
        let text = text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.entries.push(Entry::User(text.clone()));
        match self.state {
            State::Ready => self.prompt(text),
            State::Working => self.entries.push(Entry::Note(
                "Wait for the reply, or Stop it, before sending".to_owned(),
            )),
            State::Starting => self.queued = Some(text),
            State::NotConnected => {
                self.queued = Some(text);
                self.connect(ctx);
            }
        }
    }

    fn prompt(&mut self, text: String) {
        let (Some(agent), Some(session)) = (self.agent.as_mut(), self.session_id.clone()) else {
            return;
        };
        let params = json!({ "sessionId": session, "prompt": [{ "type": "text", "text": text }] });
        match agent.request(Sent::Prompt, "session/prompt", params) {
            Ok(_) => {
                self.state = State::Working;
                self.turn_changed = Some(false);
            }
            Err(e) => self.fail(e),
        }
    }

    /// Asks the agent to stop its reply.
    pub fn stop(&mut self) {
        let session = self.session_id.clone();
        if let (Some(agent), Some(session)) = (self.agent.as_mut(), session) {
            let _ = agent.notify("session/cancel", json!({ "sessionId": session }));
        }
        // Questions it asked are answered: the turn is cancelled.
        let open: Vec<Value> = self
            .entries
            .iter_mut()
            .filter_map(|entry| match entry {
                Entry::Permission {
                    request, chosen, ..
                } if chosen.is_none() => {
                    *chosen = Some("cancelled".to_owned());
                    Some(request.clone())
                }
                _ => None,
            })
            .collect();
        if let Some(agent) = self.agent.as_mut() {
            for id in open {
                let _ = agent.respond(&id, Ok(json!({ "outcome": { "outcome": "cancelled" } })));
            }
        }
    }

    /// Answers the question of entry `index` with option `option`.
    pub fn choose(&mut self, index: usize, option: &str) {
        let Some(Entry::Permission {
            request,
            options,
            chosen,
            ..
        }) = self.entries.get_mut(index)
        else {
            return;
        };
        if chosen.is_some() {
            return;
        }
        let name = options
            .iter()
            .find(|o| o.id == option)
            .map_or_else(|| option.to_owned(), |o| o.name.clone());
        *chosen = Some(name);
        let request = request.clone();
        let answer = json!({ "outcome": { "outcome": "selected", "optionId": option } });
        if let Some(agent) = self.agent.as_mut()
            && let Err(e) = agent.respond(&request, Ok(answer))
        {
            self.fail(e);
        }
    }

    fn fail(&mut self, message: String) {
        let tail = self
            .agent
            .as_ref()
            .map(Agent::stderr_tail)
            .unwrap_or_default();
        self.entries.push(Entry::Error(if tail.is_empty() {
            message
        } else {
            format!("{message}\n{tail}")
        }));
        self.disconnect();
    }

    /// Reads what the agent said since the last frame.
    pub fn poll(&mut self, folder: &Path) {
        loop {
            let Some(agent) = self.agent.as_ref() else {
                return;
            };
            let Ok(message) = agent.messages.try_recv() else {
                return;
            };
            match message {
                FromAgent::Ended => {
                    self.fail("The agent stopped".to_owned());
                    return;
                }
                FromAgent::Message(message) => self.receive(message, folder),
            }
        }
    }

    fn receive(&mut self, message: Value, folder: &Path) {
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").cloned();
        match (method, id) {
            // An answer to one of ours.
            (None, Some(id)) => {
                let sent = id
                    .as_u64()
                    .and_then(|id| self.agent.as_mut().and_then(|a| a.sent.remove(&id)));
                if let Some(sent) = sent {
                    self.answered(sent, &message, folder);
                }
            }
            // A question of the agent's.
            (Some(method), Some(id)) => self.asked(method, id, &message),
            (Some("session/update"), None) => {
                if let Some(update) = message.pointer("/params/update") {
                    self.update(update);
                }
            }
            _ => {}
        }
    }

    fn answered(&mut self, sent: Sent, message: &Value, folder: &Path) {
        if let Some(error) = message.get("error") {
            let text = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the agent refused");
            let hint = match (sent, error.get("code").and_then(Value::as_i64)) {
                // ACP's "authentication required".
                (Sent::NewSession, Some(-32000)) => {
                    "\nSign in to the agent first (for Claude Code: run `claude` in a \
                     terminal and log in), then send again."
                }
                _ => "",
            };
            match sent {
                Sent::Prompt => {
                    self.entries.push(Entry::Error(format!("{text}{hint}")));
                    self.state = State::Ready;
                    self.turn_changed = None;
                }
                _ => self.fail(format!("{text}{hint}")),
            }
            return;
        }
        let result = message.get("result").cloned().unwrap_or(Value::Null);
        match sent {
            Sent::Initialize => {
                let Some(server) = self.server.as_ref() else {
                    return;
                };
                let program = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("oxyde"));
                let params = json!({
                    "cwd": folder,
                    "mcpServers": [{
                        "name": "oxyde",
                        "command": program,
                        "args": ["--mcp", "--window", server.address, server.token],
                        "env": [],
                    }],
                });
                let sent = self
                    .agent
                    .as_mut()
                    .map(|agent| agent.request(Sent::NewSession, "session/new", params));
                if let Some(Err(e)) = sent {
                    self.fail(e);
                }
            }
            Sent::NewSession => {
                self.session_id = result
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if self.session_id.is_none() {
                    self.fail("the agent gave no session".to_owned());
                    return;
                }
                self.state = State::Ready;
                if let Some(text) = self.queued.take() {
                    self.prompt(text);
                }
            }
            Sent::Prompt => {
                self.state = State::Ready;
                self.turn_changed = None;
                let reason = result
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let note = match reason {
                    "end_turn" | "" => None,
                    "cancelled" => Some("Stopped"),
                    "max_tokens" => Some("The reply ran out of room"),
                    "max_turn_requests" => Some("The agent reached its limit of steps"),
                    "refusal" => Some("The agent refused"),
                    _ => Some("The reply ended"),
                };
                if let Some(note) = note {
                    self.entries.push(Entry::Note(note.to_owned()));
                }
            }
        }
    }

    fn asked(&mut self, method: &str, id: Value, message: &Value) {
        if method == "session/request_permission" {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            let title = params
                .pointer("/toolCall/title")
                .and_then(Value::as_str)
                .unwrap_or("use a tool")
                .to_owned();
            let options = params
                .get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .filter_map(|o| {
                            Some(PermissionOption {
                                id: o.get("optionId")?.as_str()?.to_owned(),
                                name: o.get("name")?.as_str()?.to_owned(),
                                kind: o
                                    .get("kind")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_owned(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            self.entries.push(Entry::Permission {
                request: id,
                title,
                options,
                chosen: None,
            });
            return;
        }
        // Files and terminals stay the agent's own: none was offered at `initialize`.
        let refused = Err((-32601, format!("{method} is not offered by Oxyde")));
        if let Some(agent) = self.agent.as_mut() {
            let _ = agent.respond(&id, refused);
        }
    }

    /// A `session/update`: text as it streams, tools as they run.
    fn update(&mut self, update: &Value) {
        let kind = update.get("sessionUpdate").and_then(Value::as_str);
        let text = || {
            update
                .pointer("/content/text")
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        match kind {
            Some("agent_message_chunk") => {
                if let Some(text) = text() {
                    match self.entries.last_mut() {
                        Some(Entry::Agent(so_far)) => so_far.push_str(&text),
                        _ => self.entries.push(Entry::Agent(text)),
                    }
                }
            }
            Some("agent_thought_chunk") => {
                if let Some(text) = text() {
                    match self.entries.last_mut() {
                        Some(Entry::Thought(so_far)) => so_far.push_str(&text),
                        _ => self.entries.push(Entry::Thought(text)),
                    }
                }
            }
            Some("tool_call") => {
                let field = |key: &str| {
                    update
                        .get(key)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                };
                self.entries.push(Entry::Tool {
                    id: field("toolCallId"),
                    title: field("title"),
                    status: match field("status").as_str() {
                        "" => "pending".to_owned(),
                        status => status.to_owned(),
                    },
                });
            }
            Some("tool_call_update") => {
                let id = update.get("toolCallId").and_then(Value::as_str);
                let found = self.entries.iter_mut().rev().find_map(|entry| match entry {
                    Entry::Tool {
                        id: tool,
                        title,
                        status,
                    } if Some(tool.as_str()) == id => Some((title, status)),
                    _ => None,
                });
                if let Some((title, status)) = found {
                    if let Some(new) = update.get("status").and_then(Value::as_str) {
                        *status = new.to_owned();
                    }
                    if let Some(new) = update.get("title").and_then(Value::as_str) {
                        *title = new.to_owned();
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(update: Value) -> Value {
        json!({ "jsonrpc": "2.0", "method": "session/update",
            "params": { "sessionId": "s", "update": update } })
    }

    fn text(kind: &str, text: &str) -> Value {
        update(json!({ "sessionUpdate": kind, "content": { "type": "text", "text": text } }))
    }

    /// Text streams into one entry until something else comes; tools show and move on.
    #[test]
    fn streamed_text_and_tools_make_the_conversation() {
        let mut a = Assistant::default();
        let folder = Path::new(".");
        a.receive(text("agent_thought_chunk", "Plan"), folder);
        a.receive(text("agent_message_chunk", "Hel"), folder);
        a.receive(text("agent_message_chunk", "lo"), folder);
        a.receive(
            update(json!({ "sessionUpdate": "tool_call", "toolCallId": "t1",
                "title": "extrude", "status": "pending" })),
            folder,
        );
        a.receive(
            update(
                json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1",
                "status": "completed" }),
            ),
            folder,
        );
        a.receive(text("agent_message_chunk", "Done"), folder);
        assert_eq!(
            a.entries,
            vec![
                Entry::Thought("Plan".to_owned()),
                Entry::Agent("Hello".to_owned()),
                Entry::Tool {
                    id: "t1".to_owned(),
                    title: "extrude".to_owned(),
                    status: "completed".to_owned(),
                },
                Entry::Agent("Done".to_owned()),
            ]
        );
    }

    /// A question of the agent's waits in the conversation until one of its options is
    /// chosen, once.
    #[test]
    fn a_permission_request_waits_for_an_answer() {
        let mut a = Assistant::default();
        a.receive(
            json!({ "jsonrpc": "2.0", "id": 7, "method": "session/request_permission",
                "params": { "sessionId": "s", "toolCall": { "toolCallId": "t", "title": "edit Part1" },
                    "options": [
                        { "optionId": "allow", "name": "Allow once", "kind": "allow_once" },
                        { "optionId": "no", "name": "Reject", "kind": "reject_once" },
                    ] } }),
            Path::new("."),
        );
        let Some(Entry::Permission {
            request,
            title,
            options,
            chosen,
        }) = a.entries.first()
        else {
            panic!("{:?}", a.entries);
        };
        assert_eq!((request, title.as_str()), (&json!(7), "edit Part1"));
        assert_eq!(options.len(), 2);
        assert!(chosen.is_none());
        a.choose(0, "allow");
        a.choose(0, "no");
        assert!(
            matches!(a.entries.first(), Some(Entry::Permission { chosen: Some(name), .. }) if name == "Allow once")
        );
    }

    /// The window's socket hears the bridge that has its token, and nobody else; answers go
    /// back through the bridge to the agent.
    #[test]
    fn the_window_server_hears_its_bridge_only() {
        let server = WindowServer::start(None).unwrap();
        // A stranger, with the wrong token.
        {
            let mut stranger = TcpStream::connect(&server.address).unwrap();
            writeln!(
                stranger,
                "{}",
                json!({ oxyde_mcp::window::HELLO_KEY: "wrong" })
            )
            .unwrap();
            writeln!(
                stranger,
                "{}",
                json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })
            )
            .unwrap();
        }
        let (address, token) = (server.address.clone(), server.token.clone());
        let bridge = std::thread::spawn(move || {
            let ping = json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" });
            let mut input = std::io::Cursor::new(
                format!(
                    "{ping}
"
                )
                .into_bytes(),
            );
            let mut output = Vec::new();
            oxyde_mcp::window::bridge(&address, &token, &mut input, &mut output).unwrap();
            String::from_utf8(output).unwrap()
        });
        let request = server
            .requests
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap();
        assert_eq!(request.message["id"], 2);
        let answer = json!({ "jsonrpc": "2.0", "id": 2, "result": {} });
        request.answer(&answer);
        // The connection closes once the window lets go of it.
        drop(request);
        let output = bridge.join().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(output.trim()).unwrap(),
            answer
        );
        assert!(server.requests.try_recv().is_err());
    }

    #[test]
    fn agents_are_found_by_their_program() {
        let dir = std::env::temp_dir().join(format!("oxyde-agents-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let name = if cfg!(windows) {
            "codex-acp.cmd"
        } else {
            "codex-acp"
        };
        std::fs::write(dir.join(name), "").unwrap();
        let dirs = [PathBuf::from("does-not-exist"), dir.clone()];
        assert_eq!(find_program("codex-acp", &dirs), Some(dir.join(name)));
        assert_eq!(find_program("claude-agent-acp", &dirs), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
