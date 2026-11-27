//! The MCP server: lets an AI agent (Claude Code or any MCP client) open, read
//! and drive panes and write on the scratchpad.
//!
//! Streamable HTTP transport (JSON responses only, no SSE stream) on
//! `127.0.0.1:<port>/mcp`, behind a bearer token, off unless enabled in
//! Settings → MCP. Plain std threads: one listener, one thread per request.
//!
//! Terminal tools (`run_command`, `send_text`, `read_pane`) work on the hub
//! from the request thread. Anything that touches layout or the scratchpad is
//! sent to the UI thread as [`UiEvent::Mcp`] and answered from there
//! (`App::mcp_call`), like every other layout change (CLAUDE.md §2.5).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sv_core::layout::{Direction, PaneId};
use sv_core::scratch::{Item, Shape, bounds, next_free_at};

use crate::app::{App, Focus};
use crate::hub::{Hub, UiEvent};

pub const DEFAULT_PORT: u16 = 7690;
const MAX_HEAD: usize = 32 * 1024;
const MAX_BODY: usize = 1 << 20;
/// How long a request waits for the UI thread.
const UI_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_WAIT_MS: u64 = 120_000;
const MAX_LINES: u64 = 5_000;
const PROTOCOLS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "SplitView is the terminal emulator the user is looking at. \
Pane ids come from list_panes. If you run inside SplitView, $SPLITVIEW_PANE is your own pane: \
never type into it or close it. run_command types a command, waits for the prompt to come back \
and returns the output; send_text sends raw keys to interactive programs (\\u0003 = Ctrl+C). \
The scratchpad is the user's canvas for brainstorming and plans, one per project (the git root \
of `dir`, default: the focused pane's directory); keep notes short and lay them out top to bottom.";

/// A tool call the UI thread must answer.
#[derive(Debug)]
pub struct Call {
    pub tool: String,
    pub args: Value,
    pub reply: mpsc::SyncSender<Result<Value, String>>,
}

/// A running server; dropping it stops it (and frees the port before returning).
pub struct Server {
    pub port: u16,
    pub token: String,
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    pub fn start(hub: Arc<Hub>, port: u16, token: String) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (stop, token) = (stop.clone(), token.clone());
            thread::Builder::new()
                .name("sv-mcp".into())
                .spawn(move || {
                    for stream in listener.incoming() {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let Ok(stream) = stream else { continue };
                        let (hub, token) = (hub.clone(), token.clone());
                        let _ = thread::Builder::new()
                            .name("sv-mcp-req".into())
                            .spawn(move || {
                                let _ = serve(stream, &hub, &token);
                            });
                    }
                })?
        };
        Ok(Self {
            port: addr.port(),
            token,
            addr,
            stop,
            thread: Some(thread),
        })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.port)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the blocking accept; join only if that worked, never hang.
        if TcpStream::connect_timeout(&self.addr, Duration::from_millis(500)).is_ok()
            && let Some(t) = self.thread.take()
        {
            let _ = t.join();
        }
    }
}

/// A fresh 256-bit bearer token, hex.
pub fn new_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The command that registers this server with Claude Code.
pub fn claude_command(url: &str, token: &str) -> String {
    format!(
        // User scope: the default (local) only works in the folder it was run in.
        "claude mcp add --scope user --transport http splitview {url} --header \"Authorization: Bearer {token}\""
    )
}

fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Browsers send `Origin`; only a page served from this machine may get through
/// (DNS rebinding). Agents send none.
fn local_origin(o: &str) -> bool {
    let Some(rest) = o
        .strip_prefix("http://")
        .or_else(|| o.strip_prefix("https://"))
    else {
        return false;
    };
    let host = if rest.starts_with("[::1]") {
        "[::1]"
    } else {
        rest.split([':', '/']).next().unwrap_or("")
    };
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

fn respond(w: &mut impl Write, status: &str, body: &str) -> std::io::Result<()> {
    let ctype = if body.is_empty() {
        ""
    } else {
        "Content-Type: application/json; charset=utf-8\r\n"
    };
    write!(
        w,
        "HTTP/1.1 {status}\r\n{ctype}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    w.flush()
}

/// One HTTP request → one response, then the connection closes.
fn serve(stream: TcpStream, hub: &Arc<Hub>, token: &str) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut w = stream.try_clone()?;
    let mut r = BufReader::new(stream);
    let mut head = Vec::new();
    let mut total = 0;
    loop {
        let mut line = String::new();
        let n = (&mut r).take(8192).read_line(&mut line)?;
        total += n;
        if n == 0 || total > MAX_HEAD {
            return Ok(());
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        head.push(line.to_owned());
    }
    let mut start = head.first().map(|l| l.split(' ')).into_iter().flatten();
    let (method, target) = (start.next().unwrap_or(""), start.next().unwrap_or(""));
    let headers: Vec<(String, &str)> = head[1..]
        .iter()
        .filter_map(|h| h.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim()))
        .collect();
    let header = |n: &str| headers.iter().find(|(k, _)| k == n).map(|(_, v)| *v);

    if target.split('?').next() != Some("/mcp") {
        return respond(&mut w, "404 Not Found", "");
    }
    if header("origin").is_some_and(|o| !local_origin(o)) {
        return respond(&mut w, "403 Forbidden", "");
    }
    let presented = header("authorization")
        .and_then(|a| a.strip_prefix("Bearer "))
        .unwrap_or("");
    if !same(presented.trim().as_bytes(), token.as_bytes()) {
        return respond(&mut w, "401 Unauthorized", "");
    }
    if method != "POST" {
        // No server-initiated stream, no sessions to delete.
        return respond(&mut w, "405 Method Not Allowed", "");
    }
    let len: usize = header("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if len > MAX_BODY {
        return respond(&mut w, "413 Payload Too Large", "");
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    let msg = match serde_json::from_slice::<Value>(&body) {
        Ok(v) if v.is_object() => v,
        _ => {
            let e = rpc_error(Value::Null, -32700, "expected one JSON-RPC message");
            return respond(&mut w, "400 Bad Request", &e.to_string());
        }
    };
    // Notifications and responses get no body.
    if msg.get("id").is_none() || msg.get("method").is_none() {
        return respond(&mut w, "202 Accepted", "");
    }
    respond(&mut w, "200 OK", &rpc(hub, &msg).to_string())
}

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn rpc(hub: &Arc<Hub>, msg: &Value) -> Value {
    let id = msg["id"].clone();
    let params = &msg["params"];
    let result = match msg["method"].as_str().unwrap_or("") {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or("");
            let version = PROTOCOLS
                .iter()
                .find(|v| **v == asked)
                .unwrap_or(&PROTOCOLS[1]);
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "splitview", "version": crate::theme::VERSION},
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let (text, is_error) = match call_tool(hub, name, args) {
                Ok(Value::String(s)) => (s, false),
                Ok(v) => (serde_json::to_string_pretty(&v).unwrap_or_default(), false),
                Err(e) => (e, true),
            };
            json!({"content": [{"type": "text", "text": text}], "isError": is_error})
        }
        _ => return rpc_error(id, -32601, "method not found"),
    };
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn tools() -> Value {
    let pane = json!({"type": "integer", "description": "Pane id from list_panes"});
    let dir = json!({"type": "string", "description": "A directory in the project whose pad to use (default: the focused pane's directory)"});
    let point = json!({"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2, "description": "[x, y] in canvas points; y grows downward"});
    let color = json!({"type": "integer", "minimum": 0, "maximum": 5, "description": "0 text, 1 teal, 2 blue, 3 amber, 4 red, 5 purple"});
    let lines = json!({"type": "integer", "description": "How many lines of output to return (scrollback included)"});
    let tool = |name: &str, description: &str, properties: Value, required: &[&str]| {
        json!({"name": name, "description": description,
               "inputSchema": {"type": "object", "properties": properties, "required": required}})
    };
    json!([
        tool(
            "list_panes",
            "List every terminal pane in every SplitView window: id, window, title, shell, cwd, size, whether it is focused in its window, whether it is backgrounded.",
            json!({}),
            &[]
        ),
        tool(
            "open_pane",
            "Open a new terminal pane by splitting a pane (default: the focused one of the active window). Optionally run a command in it. Returns its id.",
            json!({
                "cwd": {"type": "string", "description": "Working directory (default: that of the pane it splits)"},
                "command": {"type": "string", "description": "A command to run once the shell starts"},
                "beside": pane,
                "direction": {"type": "string", "enum": ["right", "down"], "description": "Side by side (right, default) or stacked (down)"},
                "focus": {"type": "boolean", "description": "Move the keyboard focus to it (default false)"}
            }),
            &[]
        ),
        tool(
            "close_pane",
            "Close a pane and kill its shell.",
            json!({"pane": pane}),
            &["pane"]
        ),
        tool(
            "focus_pane",
            "Give a pane the keyboard focus (brings a backgrounded pane back).",
            json!({"pane": pane}),
            &["pane"]
        ),
        tool(
            "run_command",
            "Type a command into a pane's shell, press Enter, wait until the prompt returns (or wait_ms passes) and return the last lines of output. For programs that never return to the prompt, use a small wait_ms and read_pane later.",
            json!({
                "pane": pane,
                "command": {"type": "string"},
                "wait_ms": {"type": "integer", "description": "Longest wait for the prompt, default 10000, max 120000"},
                "lines": lines
            }),
            &["pane", "command"]
        ),
        tool(
            "send_text",
            "Send raw text/keys to a pane with no Enter added (\\r is Enter, \\u0003 Ctrl+C, \\u001b Esc, \\u001b[A arrow up). Returns the screen after wait_ms.",
            json!({
                "pane": pane,
                "text": {"type": "string"},
                "wait_ms": {"type": "integer", "description": "Default 300"},
                "lines": lines
            }),
            &["pane", "text"]
        ),
        tool(
            "read_pane",
            "Read a pane's recent output as plain text (scrollback + screen).",
            json!({"pane": pane, "lines": lines}),
            &["pane"]
        ),
        tool(
            "scratchpad_read",
            "Read a project's scratchpad: notes (text) and shapes, each with an id. Ids are positions in the list and shift when items are deleted.",
            json!({"dir": dir}),
            &[]
        ),
        tool(
            "scratchpad_add_note",
            "Add a text note to the scratchpad (placed under everything unless `at` is given). Returns its id.",
            json!({"text": {"type": "string"}, "at": point, "color": color, "dir": dir}),
            &["text"]
        ),
        tool(
            "scratchpad_add_shape",
            "Draw a line, arrow, rectangle or ellipse from one corner/end to the other (e.g. a box around notes, an arrow between them). Returns its id.",
            json!({
                "shape": {"type": "string", "enum": ["line", "arrow", "rect", "ellipse"]},
                "from": point, "to": point, "color": color, "dir": dir
            }),
            &["shape", "from", "to"]
        ),
        tool(
            "scratchpad_update_note",
            "Change a note's text, position or color.",
            json!({"id": {"type": "integer"}, "text": {"type": "string"}, "at": point, "color": color, "dir": dir}),
            &["id"]
        ),
        tool(
            "scratchpad_delete",
            "Delete items (notes or shapes) by id. The user can undo it with Ctrl+Z on the pad.",
            json!({"ids": {"type": "array", "items": {"type": "integer"}}, "dir": dir}),
            &["ids"]
        ),
        tool(
            "scratchpad_open",
            "Show the scratchpad panel in the active window so the user sees it.",
            json!({}),
            &[]
        ),
    ])
}

fn pane_arg(args: &Value) -> Result<PaneId, String> {
    args["pane"]
        .as_u64()
        .ok_or_else(|| "`pane` is required: an id from list_panes".into())
}

fn str_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    args[name]
        .as_str()
        .ok_or_else(|| format!("`{name}` is required"))
}

fn lines_arg(args: &Value, default: u64) -> usize {
    args["lines"]
        .as_u64()
        .unwrap_or(default)
        .clamp(1, MAX_LINES) as usize
}

fn wait_arg(args: &Value, default: u64) -> Duration {
    Duration::from_millis(args["wait_ms"].as_u64().unwrap_or(default).min(MAX_WAIT_MS))
}

fn call_tool(hub: &Arc<Hub>, name: &str, args: Value) -> Result<Value, String> {
    let gone = |id| format!("no pane {id} (see list_panes)");
    match name {
        "run_command" => {
            let id = pane_arg(&args)?;
            let cmd = str_arg(&args, "command")?.trim_end_matches(['\r', '\n']);
            let target = hub.type_line(id, cmd).map_err(|_| gone(id))?;
            let deadline = Instant::now() + wait_arg(&args, 10_000);
            let done = loop {
                thread::sleep(Duration::from_millis(50));
                match hub.prompts(id) {
                    Some(n) if n >= target => break true,
                    None => return Err(format!("pane {id} closed while the command ran")),
                    _ if Instant::now() >= deadline => break false,
                    _ => {}
                }
            };
            // The prompt's own bytes land in the same or the next read.
            thread::sleep(Duration::from_millis(60));
            let out = read(hub, id, lines_arg(&args, 60))?;
            Ok(Value::String(if done {
                out
            } else {
                format!("[still running — the prompt has not come back; read_pane later]\n{out}")
            }))
        }
        "send_text" => {
            let id = pane_arg(&args)?;
            hub.write(id, str_arg(&args, "text")?.as_bytes())
                .map_err(|_| gone(id))?;
            thread::sleep(wait_arg(&args, 300));
            read(hub, id, lines_arg(&args, 40)).map(Value::String)
        }
        "read_pane" => {
            let id = pane_arg(&args)?;
            read(hub, id, lines_arg(&args, 100)).map(Value::String)
        }
        _ => {
            let (reply, rx) = mpsc::sync_channel(1);
            let call = Call {
                tool: name.to_owned(),
                args,
                reply,
            };
            if !hub.emit(UiEvent::Mcp(call)) {
                return Err("SplitView is closing".into());
            }
            rx.recv_timeout(UI_TIMEOUT)
                .map_err(|_| "SplitView's window did not answer in time".to_owned())?
        }
    }
}

fn read(hub: &Hub, id: PaneId, lines: usize) -> Result<String, String> {
    hub.terminal(id)
        .map(|t| t.tail_text(lines))
        .ok_or_else(|| format!("no pane {id} (see list_panes)"))
}

fn point(v: &Value) -> Option<[f32; 2]> {
    match v.as_array()?.as_slice() {
        [x, y] => Some([x.as_f64()? as f32, y.as_f64()? as f32]),
        _ => None,
    }
}

fn color_arg(args: &Value) -> Result<Option<u8>, String> {
    match args.get("color") {
        None | Some(Value::Null) => Ok(None),
        Some(c) => c
            .as_u64()
            .filter(|c| (*c as usize) < crate::theme::INK.len())
            .map(|c| Some(c as u8))
            .ok_or_else(|| "`color` must be 0–5".into()),
    }
}

/// An item as an agent reads it (pen strokes as their bounds, not every point).
fn item_json(id: usize, item: &Item) -> Value {
    match item {
        Item::Text { at, text, color } => {
            json!({"id": id, "kind": "note", "text": text, "at": at, "color": color})
        }
        Item::Ink {
            shape: Shape::Pen,
            points,
            color,
        } => {
            let b = bounds(points).map(|(lo, hi)| [lo, hi]);
            json!({"id": id, "kind": "shape", "shape": "pen", "bounds": b, "color": color})
        }
        Item::Ink {
            shape,
            points,
            color,
        } => {
            json!({"id": id, "kind": "shape", "shape": shape, "from": points.first(), "to": points.get(1), "color": color})
        }
    }
}

impl App {
    /// Answer a tool call that needs the UI thread.
    pub fn mcp_call(&mut self, tool: &str, args: &Value) -> Result<Value, String> {
        match tool {
            "list_panes" => {
                let mut out = Vec::new();
                for w in &self.windows {
                    let shown = w.layout.leaves().into_iter().map(|id| (id, false));
                    let bg = w.background.iter().map(|b| (b.0, true));
                    for (id, backgrounded) in shown.chain(bg) {
                        let Some(p) = self.panes.get(&id) else {
                            continue;
                        };
                        let size = p.term.size();
                        out.push(json!({
                            "pane": id, "window": w.label, "title": p.title(), "shell": p.shell,
                            "cwd": p.cwd, "cols": size.cols, "rows": size.rows,
                            "focused": w.layout.focused == Some(id), "backgrounded": backgrounded,
                        }));
                    }
                }
                Ok(Value::Array(out))
            }
            "open_pane" => {
                let dir = match args["direction"].as_str() {
                    None | Some("right") => Direction::Right,
                    Some("down") => Direction::Down,
                    Some(d) => return Err(format!("unknown direction '{d}'")),
                };
                let beside = args["beside"].as_u64();
                let mut wi = match beside {
                    Some(b) => self.window_of(b).ok_or_else(|| format!("no pane {b}"))?,
                    None => self.active_window(),
                };
                if self.windows[wi].detached_from.is_some() {
                    wi = 0; // a detached pane's window holds one pane
                }
                let w = &mut self.windows[wi];
                let (prev, prev_focus) = (w.layout.focused, w.focus);
                if let Some(b) = beside
                    && w.layout.has_pane(b)
                {
                    w.layout = w.layout.focus(b);
                }
                let cwd = args["cwd"].as_str().map(str::to_owned);
                let id = self.split_into(wi, dir, cwd, None, true)?;
                let w = &mut self.windows[wi];
                if !args["focus"].as_bool().unwrap_or(false) {
                    if let Some(p) = prev {
                        w.layout = w.layout.focus(p);
                    }
                    w.focus = prev_focus;
                }
                if let Some(c) = args["command"].as_str() {
                    let c = c.trim_end_matches(['\r', '\n']);
                    self.hub.type_line(id, c).map_err(|e| e.to_string())?;
                }
                Ok(json!({"pane": id, "window": self.windows[wi].label}))
            }
            "close_pane" => {
                let id = pane_arg(args)?;
                let wi = self
                    .window_of(id)
                    .ok_or_else(|| format!("no pane {id} (see list_panes)"))?;
                let w = &self.windows[wi];
                if wi == 0 && w.layout.leaves().len() == 1 && w.background.is_empty() {
                    return Err(
                        "that is the main window's last pane: closing it would quit SplitView"
                            .into(),
                    );
                }
                self.close_pane(id);
                Ok(json!(format!("closed pane {id}")))
            }
            "focus_pane" => {
                let id = pane_arg(args)?;
                let wi = self
                    .window_of(id)
                    .ok_or_else(|| format!("no pane {id} (see list_panes)"))?;
                if let Some(i) = self.windows[wi].background.iter().position(|b| b.0 == id) {
                    self.restore_background(wi, i);
                }
                let w = &mut self.windows[wi];
                w.layout = w.layout.focus(id);
                w.focus = Focus::Terminal;
                Ok(json!(format!("pane {id} has the focus")))
            }
            "scratchpad_open" => {
                let wi = self.active_window();
                self.windows[wi].scratch_open = true;
                Ok(json!("the scratchpad is open"))
            }
            _ => self.mcp_scratch(tool, args),
        }
    }

    fn mcp_scratch(&mut self, tool: &str, args: &Value) -> Result<Value, String> {
        let cwd = match args["dir"].as_str() {
            Some(d) => Some(d.to_owned()),
            None => self
                .focused_pane(self.active_window())
                .and_then(|p| p.cwd.clone()),
        };
        let cwd = cwd.as_deref();
        let color = color_arg(args)?;
        let pad = &mut self.scratch;
        match tool {
            "scratchpad_read" => {
                let (project, items) = pad.items_for(cwd);
                let items: Vec<Value> = items
                    .iter()
                    .enumerate()
                    .map(|(i, it)| item_json(i, it))
                    .collect();
                Ok(json!({"project": project, "items": items}))
            }
            "scratchpad_add_note" => {
                let text = str_arg(args, "text")?.to_owned();
                let at = point(&args["at"]);
                let (project, id) = pad.edit(cwd, |items| {
                    let at = at.unwrap_or_else(|| next_free_at(items, 20.0));
                    items.push(Item::Text {
                        at,
                        text,
                        color: color.unwrap_or(0),
                    });
                    Ok(items.len() - 1)
                })?;
                Ok(json!({"id": id, "project": project}))
            }
            "scratchpad_add_shape" => {
                let shape = match str_arg(args, "shape")? {
                    "line" => Shape::Line,
                    "arrow" => Shape::Arrow,
                    "rect" => Shape::Rect,
                    "ellipse" => Shape::Ellipse,
                    s => return Err(format!("unknown shape '{s}'")),
                };
                let (Some(from), Some(to)) = (point(&args["from"]), point(&args["to"])) else {
                    return Err("`from` and `to` must be [x, y]".into());
                };
                let (project, id) = pad.edit(cwd, |items| {
                    items.push(Item::Ink {
                        shape,
                        points: vec![from, to],
                        color: color.unwrap_or(0),
                    });
                    Ok(items.len() - 1)
                })?;
                Ok(json!({"id": id, "project": project}))
            }
            "scratchpad_update_note" => {
                let id = args["id"].as_u64().ok_or("`id` is required")? as usize;
                let at = point(&args["at"]);
                let new_text = args["text"].as_str();
                pad.edit(cwd, |items| match items.get_mut(id) {
                    Some(Item::Text {
                        at: a,
                        text,
                        color: c,
                    }) => {
                        if let Some(t) = new_text {
                            *text = t.to_owned();
                        }
                        if let Some(p) = at {
                            *a = p;
                        }
                        if let Some(k) = color {
                            *c = k;
                        }
                        Ok(())
                    }
                    Some(_) => Err(format!("item {id} is a shape, not a note")),
                    None => Err(format!("no item {id} (see scratchpad_read)")),
                })?;
                Ok(json!(format!("note {id} updated")))
            }
            "scratchpad_delete" => {
                let mut ids: Vec<usize> = args["ids"]
                    .as_array()
                    .ok_or("`ids` is required")?
                    .iter()
                    .filter_map(|v| v.as_u64().map(|v| v as usize))
                    .collect();
                ids.sort_unstable();
                ids.dedup();
                let n = ids.len();
                pad.edit(cwd, |items| {
                    if let Some(bad) = ids.iter().find(|i| **i >= items.len()) {
                        return Err(format!("no item {bad} (see scratchpad_read)"));
                    }
                    for i in ids.into_iter().rev() {
                        items.remove(i);
                    }
                    Ok(())
                })?;
                Ok(json!(format!("deleted {n} item(s); later ids moved down")))
            }
            _ => Err(format!("unknown tool '{tool}'")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_and_tokens() {
        assert!(local_origin("http://localhost:3000"));
        assert!(local_origin("http://127.0.0.1"));
        assert!(local_origin("http://[::1]:80/x"));
        assert!(!local_origin("http://localhost.evil.com"));
        assert!(!local_origin("https://example.com"));
        assert!(!local_origin("null"));
        assert!(same(b"abc", b"abc"));
        assert!(!same(b"abc", b"abd"));
        assert!(!same(b"abc", b"ab"));
        assert_eq!(new_token().len(), 64);
        assert_ne!(new_token(), new_token());
    }

    fn post(port: u16, auth: &str, body: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            s,
            "POST /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    /// Over real loopback HTTP: auth, the handshake, tool listing, a terminal
    /// tool, and a UI tool answered from the "UI thread" (the test).
    #[test]
    fn serves_json_rpc_over_http() {
        let (tx, rx) = mpsc::channel();
        let hub = Hub::new(egui::Context::default(), tx);
        let server = Server::start(hub, 0, "tok".into()).unwrap();
        let port = server.port;

        assert!(post(port, "nope", "{}").starts_with("HTTP/1.1 401"));
        let init = post(
            port,
            "tok",
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        );
        assert!(init.starts_with("HTTP/1.1 200"), "{init}");
        assert!(init.contains(r#""protocolVersion":"2025-06-18""#));
        let note = post(
            port,
            "tok",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        );
        assert!(note.starts_with("HTTP/1.1 202"));
        let list = post(
            port,
            "tok",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        );
        assert!(list.contains("run_command") && list.contains("scratchpad_add_note"));
        let bad = post(
            port,
            "tok",
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_pane","arguments":{"pane":99}}}"#,
        );
        assert!(bad.contains(r#""isError":true"#) && bad.contains("no pane 99"));

        let ui = thread::spawn(move || {
            post(
                port,
                "tok",
                r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"list_panes"}}"#,
            )
        });
        let Ok(UiEvent::Mcp(call)) = rx.recv_timeout(Duration::from_secs(5)) else {
            panic!("no UI call")
        };
        assert_eq!(call.tool, "list_panes");
        call.reply.send(Ok(json!([]))).unwrap();
        let out = ui.join().unwrap();
        assert!(out.contains(r#""isError":false"#), "{out}");
        drop(server);
        assert!(
            TcpStream::connect(("127.0.0.1", port)).is_err(),
            "port freed"
        );
    }
}
