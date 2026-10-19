//! Oxyde as a [Model Context Protocol](https://modelcontextprotocol.io) server: an AI agent
//! (or any MCP client) models parts and assemblies through it the way a person does in the
//! application, feature by feature, and looks at what it made.
//!
//! The server holds a session: one part being modelled, optionally a *reference* (an
//! existing part or STEP file to reproduce, read-only), and optionally an assembly. Its
//! tools, listed by `tools/list` with their JSON schemas (see [`tools`]), fall in five
//! groups:
//!
//! - **documents**: new, open and save parts and assemblies;
//! - **sketches and features**: sketches described by shapes (rectangles, circles, slots,
//!   polygons, polylines with fillets) that come out dimensioned and related as Oxyde's
//!   own sketch tools make them, then extrusions, revolves, fillets, chamfers, shells,
//!   patterns, mirrors, holes, reference geometry, and any feature in its `model.json` form;
//! - **inspection**: faces and edges picked by geometric selectors (near a point, by normal,
//!   radius, box…), exact sections through a plane with the shapes of their loops
//!   recognised and grouped into patterns, measures;
//! - **comparison**: the part against the reference, by exact booleans (common volume,
//!   material too much and too little), and pictures of both overlaid;
//! - **assemblies**: components placed or mated, and pictures of the whole.
//!
//! Every change is rebuilt at once through the same engine as the application
//! ([`oxyde_model::Part::rebuild`] with an [`oxyde_occt::OcctKernel`]), and a tool reports
//! the features that failed, so the agent sees what it did as a user sees the tree.
//!
//! Transport: JSON-RPC 2.0 messages, one per line, on standard input and output (MCP's
//! stdio transport). `--batch <file>` runs tool calls from a file of JSON lines instead,
//! one session for the whole file, for scripts and tests.

// Never crash: a panic in a modelling path would take the session with it.
// Return an error or fall back to a safe default instead (docs/ARCHITECTURE.md, "Never crash").
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented
)]
#![warn(clippy::indexing_slicing)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing
    )
)]

mod args;
mod assembling;
mod assembly;
mod font;
mod geometry;
mod inspect;
mod looking;
mod modelling;
mod picture;
mod select;
mod session;
mod sketch;
mod stdio;
pub mod tools;

use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};

pub use session::Session;
pub use tools::{Image, ToolOutput};

/// The newest protocol revision this server speaks; it answers a client asking for an
/// older one with that one, which the tools work the same under.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Protocol revisions a client may ask for.
const KNOWN_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];

/// What the server tells the agent once, at `initialize`: how to go about modelling here.
const INSTRUCTIONS: &str = "Oxyde is a parametric, feature-based CAD modeller in the manner of \
SolidWorks. Model a part as a person would: new_part, then sketches on the Front, Top or Right \
plane (or on a flat face) described by shapes that come out fully dimensioned, then features \
(extrude, revolve, fillet, chamfer, shell, patterns, mirror, hole). Units are millimetres and \
degrees; the world is Y up and the Front plane is XY. Centre parts on the origin and use \
mid-plane extrusions, mirrors and patterns where the part is symmetric or repetitive. Faces and \
edges are picked with selectors (near a point, by normal, radius, box, the feature that made \
them) and kept by persistent name. To reproduce an existing part, open it with open_reference \
(optionally moved so it is centred), read it with section, faces and edges, and check your part \
against it with compare and render. Save often; every tool reports features that failed.";

/// Runs the server with the command line `args` (the program name first): stdio MCP by
/// default, `--batch <file>` for a file of tool calls, `--tools` to print the tool list.
/// Returns the exit code.
pub fn run<I, T>(
    args: I,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    err: &mut dyn Write,
) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).skip(1).collect();
    let words: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    match words
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => {
            // Messages go out on a duplicate of the standard output, which itself now leads
            // to the standard error (see `stdio`); as they are when that cannot be done.
            let served = match stdio::protocol_channel() {
                Some(mut channel) => serve(input, &mut channel),
                None => serve(input, output),
            };
            match served {
                Ok(()) => 0,
                Err(e) => {
                    let _ = writeln!(err, "oxyde-mcp: {e}");
                    2
                }
            }
        }
        ["--tools"] => {
            let list = Value::Array(tools::list());
            let _ = writeln!(
                output,
                "{}",
                serde_json::to_string_pretty(&list).unwrap_or_default()
            );
            0
        }
        ["--batch", _] => {
            let path = args.get(1).map(PathBuf::from).unwrap_or_default();
            batch(&path, output, err)
        }
        ["--help" | "-h"] => {
            let _ = writeln!(output, "{USAGE}");
            0
        }
        _ => {
            let _ = writeln!(err, "{USAGE}");
            2
        }
    }
}

const USAGE: &str = "oxyde-mcp: Oxyde as a Model Context Protocol server.

usage:
  oxyde-mcp                 serve MCP (JSON-RPC 2.0, one message per line) on stdin/stdout
  oxyde-mcp --batch <file>  run the tool calls of a file, one JSON object per line:
                            {\"tool\": \"<name>\", \"arguments\": {...}}; lines starting
                            with // are skipped
  oxyde-mcp --tools         print the tools and their input schemas as JSON";

/// Serves MCP on `input` and `output` until the input ends.
pub fn serve(input: &mut dyn BufRead, output: &mut dyn Write) -> std::io::Result<()> {
    let mut session = Session::new();
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle(&mut session, message),
            Err(e) => Some(error_response(
                Value::Null,
                -32700,
                &format!("parse error: {e}"),
            )),
        };
        if let Some(response) = response {
            writeln!(output, "{response}")?;
            output.flush()?;
        }
    }
}

/// Answers one JSON-RPC message (or batch of messages); `None` for notifications.
pub fn handle(session: &mut Session, message: Value) -> Option<Value> {
    if let Value::Array(messages) = message {
        let answers: Vec<Value> = messages
            .into_iter()
            .filter_map(|m| handle(session, m))
            .collect();
        return (!answers.is_empty()).then_some(Value::Array(answers));
    }
    // A notification (no id) gets no answer, whatever it is.
    let id = message.get("id").cloned()?;
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => Ok(initialize(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools::list() })),
        "tools/call" => tools_call(session, &params),
        "resources/list" => Ok(json!({ "resources": [] })),
        "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        "" => Err((-32600, "invalid request: no method".to_owned())),
        other => Err((-32601, format!("method not found: {other}"))),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => error_response(id, code, &message),
    })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn initialize(params: &Value) -> Value {
    let asked = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL_VERSION);
    let version = if KNOWN_VERSIONS.contains(&asked) {
        asked
    } else {
        PROTOCOL_VERSION
    };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "oxyde", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

fn tools_call(session: &mut Session, params: &Value) -> Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or((-32602, "tools/call needs a tool name".to_owned()))?;
    if !tools::exists(name) {
        return Err((-32602, format!("unknown tool: {name}")));
    }
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    let (output, failed) = match tools::call(session, name, &arguments) {
        Ok(output) => (output, false),
        Err(message) => (ToolOutput::text(message), true),
    };
    let mut content = vec![json!({ "type": "text", "text": output.text })];
    for image in &output.images {
        content.push(json!({
            "type": "image",
            "data": base64(&image.png),
            "mimeType": "image/png",
        }));
    }
    Ok(json!({ "content": content, "isError": failed }))
}

/// Runs the tool calls of a file of JSON lines in one session, printing each result.
fn batch(path: &std::path::Path, output: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => {
            let _ = writeln!(err, "oxyde-mcp: {}: {e}", path.display());
            return 2;
        }
    };
    let mut session = Session::new();
    let mut failures = 0;
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let call: Value = match serde_json::from_str(line) {
            Ok(call) => call,
            Err(e) => {
                let _ = writeln!(err, "line {}: {e}", number + 1);
                return 2;
            }
        };
        let name = call.get("tool").and_then(Value::as_str).unwrap_or("");
        let arguments = call.get("arguments").cloned().unwrap_or(json!({}));
        let _ = writeln!(output, "== {name} (line {})", number + 1);
        match tools::call(&mut session, name, &arguments) {
            Ok(result) => {
                let _ = writeln!(output, "{}", result.text);
                for image in &result.images {
                    if let Some(path) = &image.path {
                        let _ = writeln!(output, "[image {}]", path.display());
                    }
                }
            }
            Err(message) => {
                failures += 1;
                let _ = writeln!(output, "ERROR: {message}");
            }
        }
    }
    if failures > 0 { 1 } else { 0 }
}

/// Standard base64 with padding, for image content.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let symbol =
        |index: u32| char::from(ALPHABET.get((index & 63) as usize).copied().unwrap_or(b'A'));
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk.first().copied().unwrap_or(0));
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(symbol(n >> 18));
        out.push(symbol(n >> 12));
        out.push(if chunk.len() > 1 { symbol(n >> 6) } else { '=' });
        out.push(if chunk.len() > 2 { symbol(n) } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn notifications_get_no_answer_and_unknown_methods_an_error() {
        let mut session = Session::new();
        assert!(
            handle(
                &mut session,
                json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
            )
            .is_none()
        );
        let answer = handle(
            &mut session,
            json!({"jsonrpc": "2.0", "id": 7, "method": "frobnicate"}),
        )
        .unwrap();
        assert_eq!(answer["id"], 7);
        assert_eq!(answer["error"]["code"], -32601);
    }

    #[test]
    fn initialize_echoes_a_known_protocol_version() {
        let mut session = Session::new();
        let answer = handle(
            &mut session,
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        )
        .unwrap();
        assert_eq!(answer["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(answer["result"]["serverInfo"]["name"], "oxyde");
        let answer = handle(
            &mut session,
            json!({"jsonrpc": "2.0", "id": 2, "method": "initialize",
                   "params": {"protocolVersion": "1999-01-01"}}),
        )
        .unwrap();
        assert_eq!(answer["result"]["protocolVersion"], PROTOCOL_VERSION);
    }

    #[test]
    fn every_tool_has_a_schema_and_a_description() {
        let list = tools::list();
        assert!(list.len() > 20);
        for tool in &list {
            let name = tool["name"].as_str().unwrap();
            assert!(tools::exists(name), "{name}");
            assert!(!tool["description"].as_str().unwrap().is_empty(), "{name}");
            assert_eq!(tool["inputSchema"]["type"], "object", "{name}");
        }
    }
}
