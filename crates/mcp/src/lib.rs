use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_core::tool::ToolOutput;
use agent_core::types::ImageRef;
use agent_core::{ProgressSink, Tool, ToolCtx};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use base64::Engine;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpServerConfig {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Streamable HTTP endpoint. When set, `command`/`args` are ignored.
    #[serde(default)]
    pub url: Option<String>,
}

enum Transport {
    Stdio {
        stdin: AsyncMutex<ChildStdin>,
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
        _child: Mutex<Child>,
    },
    Http {
        client: reqwest::Client,
        url: String,
        session: Mutex<Option<String>>,
    },
}

pub struct McpClient {
    transport: Transport,
    next_id: AtomicU64,
}

impl McpClient {
    pub async fn connect(config: &McpServerConfig) -> Result<Arc<Self>> {
        let transport = if let Some(url) = config.url.clone().filter(|url| !url.trim().is_empty()) {
            Transport::Http {
                client: reqwest::Client::builder()
                    .connect_timeout(Duration::from_secs(30))
                    .build()?,
                url,
                session: Mutex::new(None),
            }
        } else {
            Self::spawn_stdio(config)?
        };

        let client = Arc::new(Self {
            transport,
            next_id: AtomicU64::new(1),
        });
        client.initialize().await?;
        Ok(client)
    }

    fn spawn_stdio(config: &McpServerConfig) -> Result<Transport> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for (key, value) in &config.env {
            command.env(key, value);
        }

        let mut child = command
            .spawn()
            .map_err(|err| anyhow!("spawn failed: {err}"))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("missing stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("missing stdout"))?;

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(id) = value.get("id").and_then(|value| value.as_u64()) {
                    if let Some(sender) = reader_pending.lock().expect("mcp pending").remove(&id) {
                        let _ = sender.send(value);
                    }
                }
            }
        });

        Ok(Transport::Stdio {
            stdin: AsyncMutex::new(stdin),
            pending,
            _child: Mutex::new(child),
        })
    }

    async fn initialize(&self) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "whisker", "version": "0.5.0" }
            }),
        )
        .await?;
        self.notify("notifications/initialized", json!({})).await?;
        Ok(())
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        match &self.transport {
            Transport::Stdio { stdin, pending, .. } => {
                let (sender, receiver) = oneshot::channel();
                pending.lock().expect("mcp pending").insert(id, sender);

                {
                    let mut stdin = stdin.lock().await;
                    let line = serde_json::to_string(&message)?;
                    stdin.write_all(line.as_bytes()).await?;
                    stdin.write_all(b"\n").await?;
                    stdin.flush().await?;
                }

                match tokio::time::timeout(Duration::from_secs(60), receiver).await {
                    Ok(Ok(value)) => extract_response(value),
                    Ok(Err(_)) => Err(anyhow!("mcp connection closed")),
                    Err(_) => {
                        pending.lock().expect("mcp pending").remove(&id);
                        Err(anyhow!("mcp request timed out"))
                    }
                }
            }
            Transport::Http {
                client,
                url,
                session,
            } => {
                let session_id = session.lock().expect("mcp session").clone();
                let mut builder = client
                    .post(url)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .header(
                        reqwest::header::ACCEPT,
                        "application/json, text/event-stream",
                    )
                    .json(&message);
                if let Some(session_id) = &session_id {
                    builder = builder.header("Mcp-Session-Id", session_id);
                }

                let response = tokio::time::timeout(Duration::from_secs(120), builder.send())
                    .await
                    .map_err(|_| anyhow!("mcp http request timed out"))??;

                if method == "initialize" {
                    if let Some(value) = response
                        .headers()
                        .get("Mcp-Session-Id")
                        .and_then(|value| value.to_str().ok())
                    {
                        *session.lock().expect("mcp session") = Some(value.to_string());
                    }
                }

                if !response.status().is_success() {
                    let status = response.status();
                    let text = response.text().await.unwrap_or_default();
                    return Err(anyhow!("mcp http error {status}: {text}"));
                }

                let is_sse = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .map(|value| value.starts_with("text/event-stream"))
                    .unwrap_or(false);

                if is_sse {
                    let mut stream = response.bytes_stream().eventsource();
                    while let Some(event) = stream.next().await {
                        let event = event.map_err(|err| anyhow!("mcp stream error: {err}"))?;
                        if event.data.trim().is_empty() {
                            continue;
                        }
                        let value: Value = serde_json::from_str(&event.data)?;
                        if value.get("id").and_then(|value| value.as_u64()) == Some(id) {
                            return extract_response(value);
                        }
                    }
                    Err(anyhow!("mcp stream ended without a response"))
                } else {
                    let value: Value = response.json().await?;
                    extract_response(value)
                }
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });

        match &self.transport {
            Transport::Stdio { stdin, .. } => {
                let mut stdin = stdin.lock().await;
                let line = serde_json::to_string(&message)?;
                stdin.write_all(line.as_bytes()).await?;
                stdin.write_all(b"\n").await?;
                stdin.flush().await?;
                Ok(())
            }
            Transport::Http {
                client,
                url,
                session,
            } => {
                let session_id = session.lock().expect("mcp session").clone();
                let mut builder = client
                    .post(url)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .header(
                        reqwest::header::ACCEPT,
                        "application/json, text/event-stream",
                    )
                    .json(&message);
                if let Some(session_id) = &session_id {
                    builder = builder.header("Mcp-Session-Id", session_id);
                }
                let response = builder.send().await?;
                if !response.status().is_success() {
                    return Err(anyhow!("mcp http notify failed: {}", response.status()));
                }
                Ok(())
            }
        }
    }
}

fn extract_response(value: Value) -> Result<Value> {
    if let Some(error) = value.get("error") {
        return Err(anyhow!("mcp error: {error}"));
    }
    Ok(value.get("result").cloned().unwrap_or(Value::Null))
}

#[derive(Debug, Clone, Deserialize)]
struct McpToolDef {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(rename = "inputSchema", default)]
    input_schema: Option<Value>,
    #[serde(default)]
    annotations: Option<Value>,
}

pub struct McpTool {
    client: Arc<McpClient>,
    full_name: String,
    remote_name: String,
    description_text: String,
    parameters: Value,
    read_only: bool,
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

fn image_extension(mime: &str) -> &'static str {
    match mime.to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "png",
    }
}

fn inline_image(
    text: &str,
    spill_dir: &Path,
    call_id: &str,
    index: usize,
) -> Option<Result<ImageRef>> {
    let value: Value = serde_json::from_str(text.trim()).ok()?;
    let data = value.get("data").and_then(|value| value.as_str())?;
    let mime = value
        .get("mimeType")
        .or_else(|| value.get("mime_type"))
        .and_then(|value| value.as_str())
        .unwrap_or("image/png");
    if !mime.starts_with("image/") {
        return None;
    }
    Some(save_image(data, mime, spill_dir, call_id, index))
}

fn save_image(
    data: &str,
    mime: &str,
    spill_dir: &Path,
    call_id: &str,
    index: usize,
) -> Result<ImageRef> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(data.trim())?;
    let base = if spill_dir.as_os_str().is_empty() {
        PathBuf::from(std::env::temp_dir())
    } else {
        spill_dir.to_path_buf()
    };
    let dir = base.join("images");
    std::fs::create_dir_all(&dir)?;
    let stem = if call_id.is_empty() {
        format!("mcp-image-{index}")
    } else {
        format!("{}-{index}", sanitize(call_id))
    };
    let path = dir.join(format!("{stem}.{}", image_extension(mime)));
    std::fs::write(&path, bytes)?;
    Ok(ImageRef {
        path,
        mime: mime.to_string(),
    })
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.full_name
    }

    fn description(&self) -> &str {
        &self.description_text
    }

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    fn is_read_only(&self, _args: &Value) -> bool {
        self.read_only
    }

    async fn execute(
        &self,
        args: Value,
        ctx: ToolCtx,
        progress: ProgressSink,
    ) -> Result<ToolOutput> {
        let result = self
            .client
            .request(
                "tools/call",
                json!({ "name": self.remote_name, "arguments": args }),
            )
            .await?;
        let is_error = result
            .get("isError")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let mut texts = Vec::new();
        let mut images = Vec::new();
        if let Some(items) = result.get("content").and_then(|value| value.as_array()) {
            for (index, item) in items.iter().enumerate() {
                match item.get("type").and_then(|value| value.as_str()) {
                    Some("text") => {
                        if let Some(text) = item.get("text").and_then(|value| value.as_str()) {
                            match inline_image(text, &ctx.spill_dir, &progress.call_id, index) {
                                Some(Ok(image)) => {
                                    texts.push(format!("[image saved: {}]", image.path.display()));
                                    images.push(image);
                                }
                                Some(Err(err)) => {
                                    texts.push(format!("[image could not be saved: {err}]"))
                                }
                                None => texts.push(text.to_string()),
                            }
                        }
                    }
                    Some("image") => {
                        let data = item
                            .get("data")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default();
                        let mime = item
                            .get("mimeType")
                            .or_else(|| item.get("mime_type"))
                            .and_then(|value| value.as_str())
                            .unwrap_or("image/png");
                        match save_image(data, mime, &ctx.spill_dir, &progress.call_id, index) {
                            Ok(image) => {
                                texts.push(format!("[image saved: {}]", image.path.display()));
                                images.push(image);
                            }
                            Err(err) => texts.push(format!("[image could not be saved: {err}]")),
                        }
                    }
                    _ => texts.push(serde_json::to_string(item).unwrap_or_default()),
                }
            }
        }
        let content = if texts.is_empty() {
            serde_json::to_string(&result).unwrap_or_default()
        } else {
            texts.join("\n")
        };
        Ok(ToolOutput {
            content,
            is_error,
            images,
        })
    }
}

pub async fn load_tools(
    servers: &HashMap<String, McpServerConfig>,
) -> (Vec<Arc<dyn Tool>>, Vec<String>) {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut warnings = Vec::new();

    for (server_name, config) in servers {
        match load_server(server_name, config).await {
            Ok(mut server_tools) => tools.append(&mut server_tools),
            Err(err) => warnings.push(format!("mcp server '{server_name}' unavailable: {err}")),
        }
    }

    (tools, warnings)
}

async fn load_server(server_name: &str, config: &McpServerConfig) -> Result<Vec<Arc<dyn Tool>>> {
    let client = McpClient::connect(config).await?;
    let result = client.request("tools/list", json!({})).await?;
    let defs_value = result.get("tools").cloned().unwrap_or(json!([]));
    let defs: Vec<McpToolDef> = serde_json::from_value(defs_value)?;

    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    for def in defs {
        let full_name = format!("mcp__{}__{}", sanitize(server_name), sanitize(&def.name));
        let read_only = def
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get("readOnlyHint"))
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let description_text = def
            .description
            .clone()
            .unwrap_or_else(|| format!("MCP tool {} from {}", def.name, server_name));
        let parameters = def
            .input_schema
            .clone()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
        tools.push(Arc::new(McpTool {
            client: client.clone(),
            full_name,
            remote_name: def.name,
            description_text,
            parameters,
            read_only,
        }));
    }
    Ok(tools)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_result() {
        let value = json!({"jsonrpc": "2.0", "id": 1, "result": {"ok": true}});
        assert_eq!(extract_response(value).unwrap(), json!({"ok": true}));
    }

    #[test]
    fn surfaces_error() {
        let value = json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32601}});
        assert!(extract_response(value).is_err());
    }

    #[test]
    fn saves_image_content_to_spill_dir() {
        let dir = std::env::temp_dir().join(format!("whisker-mcp-image-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
        let image = save_image(data, "image/png", &dir, "call_1", 0).expect("save image");
        assert!(image.path.exists());
        assert_eq!(image.mime, "image/png");
        assert!(image.path.to_string_lossy().ends_with("call_1-0.png"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extracts_inline_image_json() {
        let dir = std::env::temp_dir().join(format!("whisker-mcp-inline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
        let text = format!(r#"{{"data":"{data}","mimeType":"image/png"}}"#);
        let image = inline_image(&text, &dir, "call_2", 1)
            .expect("inline image")
            .expect("saved");
        assert!(image.path.exists());
        assert!(image.path.to_string_lossy().ends_with("call_2-1.png"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
