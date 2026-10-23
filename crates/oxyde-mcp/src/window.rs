//! The server inside an Oxyde window: the window's assistant panel starts an agent, which
//! starts `oxyde --mcp --window <address> <token>`, a bridge that passes the agent's MCP
//! messages to the window over a local socket ([`bridge`]). The window answers them with a
//! [`Session`] that works on the document open there ([`Session::handle_in_window`]): the
//! part or assembly on screen and the window's kernel are lent to the session for each
//! message, so what the agent does shows at once, without a copy to keep in step.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};

use oxyde_assembly::Assembly;
use oxyde_format::{AssemblyFile, Properties};
use oxyde_model::{Part, PartDefinition};
use oxyde_occt::OcctKernel;
use serde_json::{Value, json};

use crate::assembly::AssemblyDoc;
use crate::session::{PartDoc, Session};

/// The first line the bridge sends: the window's token, so that only the agent the window
/// started reaches it.
pub const HELLO_KEY: &str = "oxyde_window";

/// Tools that choose, replace or write the document. In a window the document is the one
/// open there, and saving it is the user's to do.
pub(crate) const DOCUMENT_TOOLS: [&str; 6] = [
    "new_part",
    "open_part",
    "save_part",
    "new_assembly",
    "open_assembly",
    "save_assembly",
];

/// Why a document tool is refused in a window.
pub(crate) const DOCUMENT_TOOL_REFUSED: &str = "In the Oxyde window you work on the document open \
there, which the user saves: new, open and save are theirs. Ask the user to open another \
document if one is needed.";

/// What the server says at `initialize` in a window, instead of its usual instructions.
pub(crate) const WINDOW_INSTRUCTIONS: &str = "You are working inside the Oxyde window, on the \
part or assembly the user has open there: every change shows at once, and each of your replies \
is one undo step for the user. Oxyde is a parametric, feature-based CAD modeller in the manner \
of SolidWorks. Model as a person would: sketches on the Front, Top or Right plane (or on a flat \
face) described by shapes that come out fully dimensioned, then features (extrude, revolve, \
fillet, chamfer, shell, patterns, mirror, hole). Units are millimetres and degrees; the world is \
Y up and the Front plane is XY. Faces and edges are picked with selectors and kept by \
persistent name. Start with part_info or assembly_info to see what is there. Do not save: the \
user does.";

/// The document of a window, lent to the session for one message.
pub enum WindowDocument {
    Part {
        /// Boxed: a part is far bigger than an assembly's description.
        part: Box<Part>,
        path: Option<PathBuf>,
        document_id: String,
        properties: Properties,
    },
    Assembly {
        assembly: Assembly,
        path: Option<PathBuf>,
        document_id: String,
        properties: Properties,
    },
}

/// What the session keeps of a window's part between messages, to tell whether the part it
/// is lent next is still the one it gave back.
pub(crate) struct WindowState {
    document_id: String,
    definition: PartDefinition,
}

impl Session {
    /// A session for a window: document tools are refused and not listed, and `initialize`
    /// gives [`WINDOW_INSTRUCTIONS`].
    pub fn for_window() -> Self {
        let mut session = Self::new();
        session.window = true;
        session
    }

    /// Answers one MCP message on the window's document: `document` and the window's
    /// `kernel` (which owns its shapes) are lent for the message and handed back as the tools
    /// left them. The undo snapshots and sketch labels the session keeps for a part go on
    /// while the part comes back as the session left it; a part changed in the window since
    /// starts them afresh.
    pub fn handle_in_window(
        &mut self,
        kernel: &mut OcctKernel,
        document: &mut WindowDocument,
        message: Value,
    ) -> Option<Value> {
        let id = message.get("id").cloned();
        std::mem::swap(&mut self.kernel, kernel);
        // The tools catch their own panics; this is for the lending around them, so that the
        // window gets its kernel and its part back whatever happens.
        let answer = match oxyde_kernel::catch_panic(|| self.answer_lent(document, message)) {
            Ok(answer) => answer,
            Err(panic) => {
                // A part still lent goes back as the session holds it (the window's undo has
                // its state from before), and what the session kept of the document goes.
                if let (WindowDocument::Part { part, .. }, Some(doc)) =
                    (&mut *document, self.part.take())
                    && part.features().is_empty()
                {
                    **part = doc.part;
                }
                self.window_part = None;
                id.map(|id| {
                    json!({ "jsonrpc": "2.0", "id": id,
                        "error": { "code": -32603, "message": format!("internal error: {panic}") } })
                })
            }
        };
        std::mem::swap(&mut self.kernel, kernel);
        answer
    }

    /// [`Session::handle_in_window`], with the window's kernel in the session.
    fn answer_lent(&mut self, document: &mut WindowDocument, message: Value) -> Option<Value> {
        match document {
            WindowDocument::Part {
                part,
                path,
                document_id,
                properties,
            } => {
                // An assembly the window showed before is gone from it.
                if let Some(old) = self.assembly.take() {
                    old.release(&mut self.kernel);
                }
                let lent = std::mem::replace(part.as_mut(), Part::new());
                self.lend_part(lent, path.clone(), document_id, properties.clone());
                let answer = crate::handle(self, message);
                if let Some(doc) = self.part.as_mut() {
                    **part = std::mem::replace(&mut doc.part, Part::new());
                    *properties = doc.properties.clone();
                    self.window_part = Some(WindowState {
                        document_id: doc.document_id.clone(),
                        definition: part.definition(),
                    });
                }
                answer
            }
            WindowDocument::Assembly {
                assembly,
                path,
                document_id,
                properties,
            } => {
                // The part the window showed before is the window's again.
                self.part = None;
                self.window_part = None;
                self.lend_assembly(assembly, path.as_deref(), document_id, properties);
                let answer = crate::handle(self, message);
                if let Some(doc) = self.assembly.as_ref() {
                    *assembly = doc.file.assembly.clone();
                    *properties = doc.file.properties.clone();
                }
                answer
            }
        }
    }

    /// Frees what the session made with the window's kernel (an assembly's parts, a
    /// reference's solids): before the session is dropped, with the kernel it was lent.
    pub fn release_in_window(&mut self, kernel: &mut OcctKernel) {
        std::mem::swap(&mut self.kernel, kernel);
        if let Some(doc) = self.assembly.take() {
            doc.release(&mut self.kernel);
        }
        self.close_reference();
        std::mem::swap(&mut self.kernel, kernel);
    }

    fn lend_part(
        &mut self,
        part: Part,
        path: Option<PathBuf>,
        document_id: &str,
        properties: Properties,
    ) {
        let same = self.window_part.as_ref().is_some_and(|kept| {
            kept.document_id == document_id && kept.definition == part.definition()
        });
        let (undo, labels) = match self.part.take() {
            Some(doc) if same => (doc.undo, doc.labels),
            _ => (Vec::new(), BTreeMap::new()),
        };
        self.part = Some(PartDoc {
            path,
            document_id: document_id.to_owned(),
            properties,
            part,
            undo,
            labels,
        });
    }

    /// The window's assembly, as the session's own: its parts are loaded once (with the
    /// window's kernel) and kept while the window shows the same document.
    fn lend_assembly(
        &mut self,
        assembly: &Assembly,
        path: Option<&Path>,
        document_id: &str,
        properties: &Properties,
    ) {
        let same = self
            .assembly
            .as_ref()
            .is_some_and(|doc| doc.file.document_id == document_id);
        if !same {
            if let Some(old) = self.assembly.take() {
                old.release(&mut self.kernel);
            }
            let dir = path
                .and_then(Path::parent)
                .map(Path::to_path_buf)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default();
            self.assembly = Some(AssemblyDoc {
                path: path.map(Path::to_path_buf),
                file: AssemblyFile {
                    document_id: document_id.to_owned(),
                    assembly: Assembly::new(),
                    properties: properties.clone(),
                },
                dir,
                loaded: BTreeMap::new(),
                equation_results: Default::default(),
                changed_parts: Default::default(),
            });
        }
        if let Some(doc) = self.assembly.as_mut() {
            doc.file.assembly = assembly.clone();
            doc.file.properties = properties.clone();
            doc.path = path.map(Path::to_path_buf);
            // Components the window added since: loaded now, as the tools expect them.
            let _ = doc.load_all(&mut self.kernel);
        }
    }
}

/// Passes MCP messages between the agent (on `input` and `output`) and the window listening
/// at `address`: the token first, then every line both ways, until either side ends.
pub fn bridge(
    address: &str,
    token: &str,
    input: &mut dyn BufRead,
    output: &mut (dyn Write + Send),
) -> std::io::Result<()> {
    let mut socket = TcpStream::connect(address)?;
    socket.set_nodelay(true)?;
    writeln!(socket, "{}", json!({ HELLO_KEY: token }))?;
    let from_window = BufReader::new(socket.try_clone()?);
    std::thread::scope(|scope| {
        scope.spawn(move || -> std::io::Result<()> {
            for line in from_window.lines() {
                writeln!(output, "{}", line?)?;
                output.flush()?;
            }
            Ok(())
        });
        let mut line = String::new();
        loop {
            line.clear();
            if input.read_line(&mut line)? == 0 {
                break;
            }
            socket.write_all(line.as_bytes())?;
            if !line.ends_with('\n') {
                socket.write_all(b"\n")?;
            }
        }
        // The agent is gone: the window sees the connection end, and closes it in turn,
        // which ends the thread above once the last answers are through.
        socket.shutdown(std::net::Shutdown::Write)
    })
}

/// Whether `line`, the first a window receives on a connection, carries `token`.
pub fn hello_matches(line: &str, token: &str) -> bool {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v.get(HELLO_KEY).and_then(Value::as_str).map(str::to_owned))
        .is_some_and(|t| t == token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(
        session: &mut Session,
        kernel: &mut OcctKernel,
        doc: &mut WindowDocument,
        id: u64,
        name: &str,
        arguments: Value,
    ) -> Value {
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments } });
        session.handle_in_window(kernel, doc, message).unwrap()
    }

    fn part_of(doc: &WindowDocument) -> &Part {
        match doc {
            WindowDocument::Part { part, .. } => part,
            WindowDocument::Assembly { .. } => panic!("not a part"),
        }
    }

    /// The agent models on the window's part, which comes back with the feature, built with
    /// the window's kernel; the session keeps nothing of it between messages.
    #[test]
    fn tools_work_on_the_window_part() {
        let mut kernel = OcctKernel::new();
        let mut session = Session::for_window();
        let mut doc = WindowDocument::Part {
            part: Box::new(Part::new()),
            path: None,
            document_id: "window-part".to_owned(),
            properties: Properties::default(),
        };
        let answer = call(
            &mut session,
            &mut kernel,
            &mut doc,
            1,
            "sketch",
            json!({
                "plane": "front",
                "items": [{ "type": "rect", "center": [0, 0], "size": [40, 20] }],
            }),
        );
        assert_eq!(answer["result"]["isError"], false, "{answer}");
        let answer = call(
            &mut session,
            &mut kernel,
            &mut doc,
            2,
            "extrude",
            json!({ "sketch": "Sketch1", "depth": 10 }),
        );
        assert_eq!(answer["result"]["isError"], false, "{answer}");
        let part = part_of(&doc);
        assert_eq!(part.features().len(), 2);
        let body = part.body().expect("a body");
        let volume: f64 = body
            .solids
            .iter()
            .map(|s| oxyde_kernel::Kernel::volume(&kernel, s.shape).unwrap())
            .sum();
        assert!((volume - 8000.0).abs() < 1e-6, "{volume}");
        assert!(
            session
                .part
                .as_ref()
                .is_some_and(|d| d.part.features().is_empty())
        );

        // The session's undo goes on while the part comes back as it left it.
        let answer = call(&mut session, &mut kernel, &mut doc, 3, "undo", json!({}));
        assert_eq!(answer["result"]["isError"], false, "{answer}");
        assert_eq!(part_of(&doc).features().len(), 1);
    }

    /// New, open and save are the window's, not the agent's: refused, and not listed.
    #[test]
    fn document_tools_are_refused_in_a_window() {
        let mut kernel = OcctKernel::new();
        let mut session = Session::for_window();
        let mut doc = WindowDocument::Part {
            part: Box::new(Part::new()),
            path: None,
            document_id: "window-part".to_owned(),
            properties: Properties::default(),
        };
        let answer = call(
            &mut session,
            &mut kernel,
            &mut doc,
            1,
            "new_part",
            json!({}),
        );
        assert_eq!(answer["result"]["isError"], true, "{answer}");
        let list = session
            .handle_in_window(
                &mut kernel,
                &mut doc,
                json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            )
            .unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(names.contains(&"extrude"));
        for tool in DOCUMENT_TOOLS {
            assert!(!names.contains(&tool), "{tool}");
        }
        let init = session
            .handle_in_window(
                &mut kernel,
                &mut doc,
                json!({ "jsonrpc": "2.0", "id": 3, "method": "initialize", "params": {} }),
            )
            .unwrap();
        assert_eq!(init["result"]["instructions"], WINDOW_INSTRUCTIONS);
    }

    /// The bridge passes the token, then lines both ways.
    #[test]
    fn the_bridge_passes_lines_both_ways() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let window = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut hello = String::new();
            reader.read_line(&mut hello).unwrap();
            assert!(hello_matches(&hello, "secret"));
            assert!(!hello_matches(&hello, "other"));
            let mut message = String::new();
            reader.read_line(&mut message).unwrap();
            writeln!(writer, "answer to {}", message.trim()).unwrap();
            writer.shutdown(std::net::Shutdown::Write).unwrap();
        });
        let mut input = std::io::Cursor::new(b"ping\n".to_vec());
        let mut output: Vec<u8> = Vec::new();
        bridge(&address, "secret", &mut input, &mut output).unwrap();
        window.join().unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "answer to ping\n");
    }
}
