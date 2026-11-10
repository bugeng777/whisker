use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_core::tool::ToolOutput;
use agent_core::{ProgressSink, Tool, ToolCtx};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex as AsyncMutex};
use url::Url;

#[derive(Debug, Clone, Deserialize)]
pub struct LspServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub language_id: Option<String>,
}

async fn write_message(stdin: &Arc<AsyncMutex<ChildStdin>>, value: &Value) -> Result<()> {
    let payload = serde_json::to_vec(value)?;
    let header = format!("Content-Length: {}\r\n\r\n", payload.len());
    let mut stdin = stdin.lock().await;
    stdin.write_all(header.as_bytes()).await?;
    stdin.write_all(&payload).await?;
    stdin.flush().await?;
    Ok(())
}

struct LspClient {
    stdin: Arc<AsyncMutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    opened: Mutex<HashSet<String>>,
    next_id: AtomicU64,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    _child: Mutex<Child>,
}

impl LspClient {
    async fn connect(command: &str, args: &[String], root: &Path) -> Result<Arc<Self>> {
        let mut child = Command::new(command)
            .args(args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| anyhow!("failed to start {command}: {err}"))?;

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("missing stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("missing stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("missing stderr"))?;
        let stderr_tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        let tail = stderr_tail.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut tail = tail.lock().expect("lsp stderr");
                if tail.len() >= 20 {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        });
        let stdin = Arc::new(AsyncMutex::new(stdin));
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let reader_stdin = stdin.clone();
        let reader_pending = pending.clone();
        let reader_diagnostics = diagnostics.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut content_length: Option<usize> = None;
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line).await {
                        Ok(0) => return,
                        Ok(_) => {}
                        Err(_) => return,
                    }
                    let trimmed = line.trim_end_matches(['\r', '\n']);
                    if trimmed.is_empty() {
                        break;
                    }
                    if let Some(value) = trimmed.strip_prefix("Content-Length:") {
                        content_length = value.trim().parse().ok();
                    }
                }
                let Some(length) = content_length else {
                    continue;
                };
                let mut buffer = vec![0u8; length];
                if reader.read_exact(&mut buffer).await.is_err() {
                    return;
                }
                let Ok(value) = serde_json::from_slice::<Value>(&buffer) else {
                    continue;
                };

                if let (Some(id), Some(method)) = (
                    value.get("id").cloned(),
                    value.get("method").and_then(|v| v.as_str()),
                ) {
                    let result = if method == "workspace/configuration" {
                        json!([])
                    } else {
                        Value::Null
                    };
                    let response = json!({ "jsonrpc": "2.0", "id": id, "result": result });
                    let _ = write_message(&reader_stdin, &response).await;
                    continue;
                }

                if value.get("method").and_then(|v| v.as_str())
                    == Some("textDocument/publishDiagnostics")
                {
                    if let Some(params) = value.get("params") {
                        let uri = params
                            .get("uri")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let items = params
                            .get("diagnostics")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default();
                        if let Ok(mut map) = reader_diagnostics.lock() {
                            map.insert(uri, items);
                        }
                    }
                    continue;
                }

                if let Some(id) = value.get("id").and_then(|v| v.as_u64()) {
                    if let Some(sender) = reader_pending.lock().expect("lsp pending").remove(&id) {
                        let _ = sender.send(value);
                    }
                }
            }
        });

        let client = Arc::new(Self {
            stdin,
            pending,
            diagnostics,
            opened: Mutex::new(HashSet::new()),
            next_id: AtomicU64::new(1),
            stderr_tail,
            _child: Mutex::new(child),
        });

        let root_uri = Url::from_directory_path(root)
            .map(|url| url.to_string())
            .unwrap_or_else(|_| "file:///".to_string());
        let initialize = client
            .request(
                "initialize",
                json!({
                    "processId": std::process::id(),
                    "rootUri": root_uri,
                    "capabilities": {
                        "textDocument": {
                            "synchronization": { "dynamicRegistration": false },
                            "hover": { "contentFormat": ["markdown", "plaintext"] },
                            "definition": {},
                            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                            "publishDiagnostics": {}
                        }
                    },
                    "clientInfo": { "name": "whisker", "version": "0.5.0" }
                }),
            )
            .await;
        if let Err(err) = initialize {
            return Err(anyhow!("initialize failed: {err}{}", client.stderr_hint()));
        }
        client.notify("initialized", json!({})).await?;
        Ok(client)
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().expect("lsp pending").insert(id, sender);
        write_message(&self.stdin, &message).await?;
        match tokio::time::timeout(Duration::from_secs(30), receiver).await {
            Ok(Ok(value)) => {
                if let Some(error) = value.get("error") {
                    return Err(anyhow!("lsp error: {error}"));
                }
                Ok(value.get("result").cloned().unwrap_or(Value::Null))
            }
            Ok(Err(_)) => Err(anyhow!("lsp connection closed")),
            Err(_) => {
                self.pending.lock().expect("lsp pending").remove(&id);
                Err(anyhow!("lsp request timed out{}", self.stderr_hint()))
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        write_message(&self.stdin, &message).await
    }

    async fn ensure_open(&self, uri: &str, language_id: &str, text: String) -> Result<()> {
        let already_open = self.opened.lock().expect("lsp opened").contains(uri);
        if already_open {
            return Ok(());
        }
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": text
                }
            }),
        )
        .await?;
        self.opened
            .lock()
            .expect("lsp opened")
            .insert(uri.to_string());
        Ok(())
    }

    fn stderr_hint(&self) -> String {
        let tail = self.stderr_tail.lock().expect("lsp stderr");
        if tail.is_empty() {
            return String::new();
        }
        format!(
            "\nserver stderr:\n{}",
            tail.iter().cloned().collect::<Vec<_>>().join("\n")
        )
    }

    fn diagnostics_for(&self, uri: &str) -> Vec<Value> {
        self.diagnostics
            .lock()
            .expect("lsp diagnostics")
            .get(uri)
            .cloned()
            .unwrap_or_default()
    }
}

pub struct LspManager {
    cwd: PathBuf,
    servers: Vec<LspServerConfig>,
    clients: Mutex<HashMap<usize, Arc<LspClient>>>,
}

impl LspManager {
    pub fn new(cwd: PathBuf, servers: Vec<LspServerConfig>) -> Self {
        Self {
            cwd,
            servers,
            clients: Mutex::new(HashMap::new()),
        }
    }

    fn server_for(&self, path: &Path) -> Option<(usize, &LspServerConfig)> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        self.servers.iter().enumerate().find(|(_, server)| {
            server
                .extensions
                .iter()
                .any(|ext| ext.trim_start_matches('.').eq_ignore_ascii_case(&extension))
        })
    }

    async fn client_for(&self, index: usize) -> Result<Arc<LspClient>> {
        if let Some(client) = self
            .clients
            .lock()
            .expect("lsp clients")
            .get(&index)
            .cloned()
        {
            return Ok(client);
        }
        let server = &self.servers[index];
        let client = LspClient::connect(&server.command, &server.args, &self.cwd).await?;
        self.clients
            .lock()
            .expect("lsp clients")
            .insert(index, client.clone());
        Ok(client)
    }
}

pub struct LspTool {
    manager: Arc<LspManager>,
}

impl LspTool {
    pub fn new(cwd: PathBuf, servers: Vec<LspServerConfig>) -> Self {
        Self {
            manager: Arc::new(LspManager::new(cwd, servers)),
        }
    }
}

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &str {
        "lsp"
    }

    fn description(&self) -> &str {
        "Query the language server for a file: diagnostics, definition, hover or document symbols. Requires an LSP server configured for the file extension."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["diagnostics", "definition", "hover", "symbols"]
                },
                "path": { "type": "string" },
                "line": { "type": "integer", "description": "1-based line (definition/hover)" },
                "column": { "type": "integer", "description": "1-based column (definition/hover)" }
            },
            "required": ["action", "path"]
        })
    }

    fn is_read_only(&self, _args: &Value) -> bool {
        true
    }

    async fn execute(
        &self,
        args: Value,
        ctx: ToolCtx,
        _progress: ProgressSink,
    ) -> Result<ToolOutput> {
        let action = args
            .get("action")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("missing action"))?;
        let raw_path = args
            .get("path")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("missing path"))?;
        let path = if Path::new(raw_path).is_absolute() {
            PathBuf::from(raw_path)
        } else {
            ctx.cwd.join(raw_path)
        };
        if !path.is_file() {
            return Ok(ToolOutput::err(format!(
                "File not found: {}",
                path.display()
            )));
        }

        let Some((index, server)) = self.manager.server_for(&path) else {
            let extension = path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default();
            return Ok(ToolOutput::err(format!(
                "No LSP server configured for *.{extension}. Add one under [lsp.<name>] in the config."
            )));
        };
        let language_id = server.language_id.clone().unwrap_or_else(|| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("plaintext")
                .to_string()
        });

        let client = match self.manager.client_for(index).await {
            Ok(client) => client,
            Err(err) => return Ok(ToolOutput::err(format!("LSP server failed: {err}"))),
        };

        let uri = match Url::from_file_path(&path) {
            Ok(uri) => uri.to_string(),
            Err(_) => return Ok(ToolOutput::err("Cannot build a file URI for this path.")),
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => return Ok(ToolOutput::err(format!("Cannot read file: {err}"))),
        };
        if let Err(err) = client.ensure_open(&uri, &language_id, text).await {
            return Ok(ToolOutput::err(format!("LSP didOpen failed: {err}")));
        }

        let result = match action {
            "diagnostics" => {
                let mut diagnostics = Vec::new();
                for _ in 0..30 {
                    diagnostics = client.diagnostics_for(&uri);
                    if !diagnostics.is_empty() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Ok(format_diagnostics(&path, &diagnostics))
            }
            "definition" | "hover" => {
                let line = args
                    .get("line")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1)
                    .max(1)
                    - 1;
                let character = args
                    .get("column")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1)
                    .max(1)
                    - 1;
                let method = if action == "definition" {
                    "textDocument/definition"
                } else {
                    "textDocument/hover"
                };
                let response = client
                    .request(
                        method,
                        json!({
                            "textDocument": { "uri": uri },
                            "position": { "line": line, "character": character }
                        }),
                    )
                    .await;
                match response {
                    Ok(value) if action == "definition" => Ok(format_locations(&value)),
                    Ok(value) => Ok(format_hover(&value)),
                    Err(err) => Err(err),
                }
            }
            "symbols" => {
                let response = client
                    .request(
                        "textDocument/documentSymbol",
                        json!({ "textDocument": { "uri": uri } }),
                    )
                    .await;
                response.map(|value| format_symbols(&path, &value))
            }
            other => Err(anyhow!("unknown action '{other}'")),
        };

        match result {
            Ok(text) => Ok(ToolOutput::ok(text)),
            Err(err) => Ok(ToolOutput::err(format!("LSP request failed: {err}"))),
        }
    }
}

fn uri_to_path(uri: &str) -> String {
    Url::parse(uri)
        .ok()
        .and_then(|url| url.to_file_path().ok())
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| uri.to_string())
}

fn format_diagnostics(path: &Path, diagnostics: &[Value]) -> String {
    if diagnostics.is_empty() {
        return format!("{}: no diagnostics reported.", path.display());
    }
    let mut lines = Vec::new();
    for diagnostic in diagnostics {
        let range = diagnostic.get("range");
        let line = range
            .and_then(|range| range.get("start"))
            .and_then(|start| start.get("line"))
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
            + 1;
        let character = range
            .and_then(|range| range.get("start"))
            .and_then(|start| start.get("character"))
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
            + 1;
        let severity = match diagnostic.get("severity").and_then(|value| value.as_u64()) {
            Some(1) => "error",
            Some(2) => "warning",
            Some(3) => "info",
            _ => "hint",
        };
        let message = diagnostic
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        lines.push(format!(
            "{}:{}:{}: {}: {}",
            path.display(),
            line,
            character,
            severity,
            message
        ));
    }
    lines.join("\n")
}

fn format_locations(value: &Value) -> String {
    let items: Vec<Value> = if let Some(array) = value.as_array() {
        array.clone()
    } else if value.is_null() {
        Vec::new()
    } else {
        vec![value.clone()]
    };
    let mut lines = Vec::new();
    for item in items {
        let uri = item
            .get("uri")
            .or_else(|| item.get("targetUri"))
            .and_then(|value| value.as_str());
        let range = item
            .get("range")
            .or_else(|| item.get("targetSelectionRange"))
            .or_else(|| item.get("targetRange"));
        if let (Some(uri), Some(range)) = (uri, range) {
            let line = range
                .get("start")
                .and_then(|start| start.get("line"))
                .and_then(|value| value.as_u64())
                .unwrap_or(0)
                + 1;
            let character = range
                .get("start")
                .and_then(|start| start.get("character"))
                .and_then(|value| value.as_u64())
                .unwrap_or(0)
                + 1;
            lines.push(format!("{}:{}:{}", uri_to_path(uri), line, character));
        }
    }
    if lines.is_empty() {
        "No definition found.".to_string()
    } else {
        lines.join("\n")
    }
}

fn format_hover(value: &Value) -> String {
    let Some(contents) = value.get("contents") else {
        return "No hover information.".to_string();
    };
    let text = match contents {
        Value::String(text) => text.clone(),
        Value::Object(object) => object
            .get("value")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => text.clone(),
                Value::Object(object) => object
                    .get("value")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    if text.trim().is_empty() {
        "No hover information.".to_string()
    } else {
        text
    }
}

fn format_symbols(path: &Path, value: &Value) -> String {
    let Some(items) = value.as_array() else {
        return "No symbols found.".to_string();
    };
    let mut lines = Vec::new();
    for item in items {
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let kind = item.get("kind").and_then(|v| v.as_u64()).unwrap_or(0);
        let range = item.get("range").or_else(|| {
            item.get("location")
                .and_then(|location| location.get("range"))
        });
        let line = range
            .and_then(|range| range.get("start"))
            .and_then(|start| start.get("line"))
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
            + 1;
        lines.push(format!(
            "{}:{}: {} (kind {})",
            path.display(),
            line,
            name,
            kind
        ));
    }
    if lines.is_empty() {
        "No symbols found.".to_string()
    } else {
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_diagnostics() {
        let diagnostics = vec![json!({
            "range": { "start": { "line": 4, "character": 2 } },
            "severity": 1,
            "message": "mismatched types"
        })];
        let text = format_diagnostics(Path::new("src/main.rs"), &diagnostics);
        assert!(text.contains("src/main.rs:5:3: error: mismatched types"));
    }

    #[test]
    fn formats_hover_markup() {
        let value = json!({ "contents": { "kind": "markdown", "value": "fn main()" } });
        assert_eq!(format_hover(&value), "fn main()");
    }

    #[test]
    fn formats_definition_links() {
        let value = json!([{
            "targetUri": "file:///tmp/demo.rs",
            "targetSelectionRange": { "start": { "line": 0, "character": 0 } }
        }]);
        let text = format_locations(&value);
        assert!(text.contains("/tmp/demo.rs:1:1"));
    }
}
