use std::sync::RwLock;
use std::time::Duration;

use agent_core::types::{FinishReason, FunctionCall, ImageRef, Message};
use agent_core::{ChatRequest, LlmClient, StreamEvent, ToolCall, Usage};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use base64::Engine;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningPolicy {
    Drop,
    Echo,
}

pub struct OpenAiProvider {
    client: Client,
    base_url: String,
    api_key: String,
    model: RwLock<String>,
    context_window: usize,
    reasoning_policy: ReasoningPolicy,
}

pub fn default_vision(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model == "deepseek-chat"
        || model.contains("flash")
        || model.contains("vision")
        || model.contains("vl")
        || model.contains("gpt-4o")
        || model.contains("gpt-4.1")
        || model.contains("claude")
        || model.contains("gemini")
}

impl OpenAiProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        context_window: usize,
        reasoning_policy: ReasoningPolicy,
    ) -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()?;
        let mut base_url = base_url.into();
        while base_url.ends_with('/') {
            base_url.pop();
        }
        Ok(Self {
            client,
            base_url,
            api_key: api_key.into(),
            model: RwLock::new(model.into()),
            context_window,
            reasoning_policy,
        })
    }
}

fn strip_reasoning_content(value: &mut serde_json::Value) {
    if let Some(items) = value.as_array_mut() {
        for item in items {
            if let Some(object) = item.as_object_mut() {
                object.remove("reasoning_content");
            }
        }
    }
}

fn image_part(image: &ImageRef) -> serde_json::Value {
    match std::fs::read(&image.path) {
        Ok(bytes) => {
            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
            json!({
                "type": "image_url",
                "image_url": {
                    "url": format!("data:{};base64,{}", image.mime, encoded)
                }
            })
        }
        Err(err) => json!({
            "type": "text",
            "text": format!("[image unavailable: {}: {err}]", image.path.display())
        }),
    }
}

fn messages_to_json(messages: &[Message]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = messages
        .iter()
        .map(|message| match message {
            Message::System { content } => json!({ "role": "system", "content": content }),
            Message::User { content, images } => {
                if images.is_empty() {
                    json!({ "role": "user", "content": content })
                } else {
                    let mut parts = Vec::new();
                    if !content.is_empty() {
                        parts.push(json!({ "type": "text", "text": content }));
                    }
                    for image in images {
                        parts.push(image_part(image));
                    }
                    json!({ "role": "user", "content": parts })
                }
            }
            Message::Assistant {
                content,
                tool_calls,
                reasoning_content,
            } => {
                let mut object = json!({ "role": "assistant", "content": content });
                if let Some(calls) = tool_calls {
                    object["tool_calls"] = json!(calls);
                }
                if let Some(reasoning) = reasoning_content {
                    object["reasoning_content"] = json!(reasoning);
                }
                object
            }
            Message::Tool {
                tool_call_id,
                content,
                name,
            } => {
                let mut object =
                    json!({ "role": "tool", "tool_call_id": tool_call_id, "content": content });
                if let Some(name) = name {
                    object["name"] = json!(name);
                }
                object
            }
        })
        .collect();
    serde_json::Value::Array(items)
}

#[derive(Debug, Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<Choice>,
    usage: Option<WireUsage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    delta: Option<Delta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        "stop" | "end_turn" | "stop_sequence" => FinishReason::Stop,
        "tool_calls" | "tool_use" | "function_call" => FinishReason::ToolCalls,
        "length" | "max_tokens" => FinishReason::MaxTokens,
        other => FinishReason::Other(other.to_string()),
    }
}

#[derive(Debug, Deserialize)]
struct Delta {
    content: Option<String>,
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<DeltaToolCall>>,
}

#[derive(Debug, Deserialize)]
struct DeltaToolCall {
    index: Option<u32>,
    id: Option<String>,
    function: Option<DeltaFunction>,
}

#[derive(Debug, Deserialize)]
struct DeltaFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireUsage {
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
    total_tokens: Option<u32>,
}

#[derive(Default)]
struct CallBuilder {
    id: String,
    name: String,
    arguments: String,
}

fn request_body(
    model: &str,
    request: &ChatRequest,
    reasoning_policy: ReasoningPolicy,
) -> serde_json::Value {
    let generation = &request.generation;
    let thinking_on = generation.thinking == Some(true)
        || (generation.thinking.is_none()
            && generation
                .reasoning_effort
                .as_deref()
                .is_some_and(|effort| !effort.eq_ignore_ascii_case("none")));
    let mut messages = messages_to_json(&request.messages);
    if reasoning_policy == ReasoningPolicy::Drop && !thinking_on {
        strip_reasoning_content(&mut messages);
    }
    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });

    if !request.tools.is_empty() {
        let tools: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|spec| json!({ "type": "function", "function": spec }))
            .collect();
        body["tools"] = serde_json::Value::Array(tools);
        body["tool_choice"] = json!("auto");
    }
    if let Some(temperature) = generation.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(max_tokens) = generation.max_tokens {
        body["max_tokens"] = json!(max_tokens);
    }
    if let Some(top_p) = generation.top_p {
        body["top_p"] = json!(top_p);
    }
    if let Some(thinking) = generation.thinking {
        body["thinking"] = json!({
            "type": if thinking { "enabled" } else { "disabled" }
        });
    }
    if let Some(effort) = &generation.reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }
    body
}

#[async_trait]
impl LlmClient for OpenAiProvider {
    fn model(&self) -> String {
        self.model.read().expect("model lock").clone()
    }

    fn set_model(&self, model: &str) -> bool {
        *self.model.write().expect("model lock") = model.to_string();
        default_vision(model)
    }

    fn context_window(&self) -> usize {
        self.context_window
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
        sink: mpsc::UnboundedSender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Result<Usage> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = request_body(&self.model(), &request, self.reasoning_policy);

        let mut attempt = 0u32;
        loop {
            attempt += 1;

            let response = self
                .client
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await;

            let response = match response {
                Ok(response) => response,
                Err(err) if attempt < 3 && err.is_connect() => {
                    tokio::time::sleep(Duration::from_millis(500 * attempt as u64)).await;
                    continue;
                }
                Err(err) => return Err(anyhow!("request failed: {err}")),
            };

            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                if attempt < 3 && (status.is_server_error() || status.as_u16() == 429) {
                    tokio::time::sleep(Duration::from_millis(800 * attempt as u64)).await;
                    continue;
                }
                return Err(anyhow!("api error {status}: {}", truncate(&text, 800)));
            }

            let mut stream = response.bytes_stream().eventsource();
            let mut builders: Vec<CallBuilder> = Vec::new();
            let mut usage = Usage::default();
            let mut sent_finish = false;

            loop {
                let item = tokio::select! {
                    _ = cancel.cancelled() => return Err(anyhow!("cancelled")),
                    item = stream.next() => item,
                };

                let Some(item) = item else { break };

                let event = item.map_err(|err| anyhow!("stream error: {err}"))?;
                let data = event.data.trim();
                if data.is_empty() {
                    continue;
                }
                if data == "[DONE]" {
                    break;
                }

                let chunk: Chunk = serde_json::from_str(data)
                    .map_err(|err| anyhow!("bad stream chunk: {err}: {}", truncate(data, 300)))?;

                if let Some(wire_usage) = chunk.usage {
                    if let Some(prompt) = wire_usage.prompt_tokens {
                        usage.prompt_tokens = prompt;
                    }
                    if let Some(completion) = wire_usage.completion_tokens {
                        usage.completion_tokens = completion;
                    }
                    if let Some(total) = wire_usage.total_tokens {
                        usage.total_tokens = total;
                    }
                }

                for choice in chunk.choices {
                    if let Some(reason) = choice.finish_reason {
                        if !sent_finish {
                            let _ = sink.send(StreamEvent::Finish(map_finish_reason(&reason)));
                            sent_finish = true;
                        }
                    }
                    let Some(delta) = choice.delta else { continue };

                    if let Some(content) = delta.content {
                        if !content.is_empty() {
                            let _ = sink.send(StreamEvent::Delta(content));
                        }
                    }
                    if let Some(reasoning) = delta.reasoning_content {
                        if !reasoning.is_empty() {
                            let _ = sink.send(StreamEvent::Reasoning(reasoning));
                        }
                    }
                    if let Some(tool_calls) = delta.tool_calls {
                        for delta_call in tool_calls {
                            let index = delta_call.index.unwrap_or(0) as usize;
                            while builders.len() <= index {
                                builders.push(CallBuilder::default());
                            }
                            let builder = &mut builders[index];
                            if let Some(id) = delta_call.id {
                                if !id.is_empty() {
                                    builder.id = id;
                                }
                            }
                            if let Some(function) = delta_call.function {
                                if let Some(name) = function.name {
                                    builder.name.push_str(&name);
                                }
                                if let Some(arguments) = function.arguments {
                                    builder.arguments.push_str(&arguments);
                                }
                            }
                        }
                    }
                }
            }

            for (index, builder) in builders.into_iter().enumerate() {
                if builder.name.is_empty() {
                    continue;
                }
                let id = if builder.id.is_empty() {
                    format!("call_{index}")
                } else {
                    builder.id
                };
                let _ = sink.send(StreamEvent::ToolCall(ToolCall {
                    id,
                    kind: "function".to_string(),
                    function: FunctionCall {
                        name: builder.name,
                        arguments: builder.arguments,
                    },
                }));
            }

            return Ok(usage);
        }
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        text.chars().take(max_chars).collect::<String>() + "..."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::GenerationSettings;

    fn deepseek_request(thinking: bool) -> ChatRequest {
        ChatRequest {
            messages: vec![
                Message::user("hi"),
                Message::Assistant {
                    content: Some("answer".to_string()),
                    tool_calls: None,
                    reasoning_content: Some("reasoning".to_string()),
                },
            ],
            tools: Vec::new(),
            generation: GenerationSettings {
                temperature: Some(0.2),
                max_tokens: Some(8_192),
                top_p: Some(0.95),
                thinking: Some(thinking),
                reasoning_effort: Some("high".to_string()),
            },
        }
    }

    #[test]
    fn serializes_deepseek_generation_settings() {
        let request = deepseek_request(true);
        let body = request_body("deepseek-v4-pro", &request, ReasoningPolicy::Drop);
        let temperature = body["temperature"].as_f64().expect("temperature");
        assert!((temperature - 0.2).abs() < 0.001);
        assert_eq!(body["max_tokens"], 8_192);
        let top_p = body["top_p"].as_f64().expect("top_p");
        assert!((top_p - 0.95).abs() < 0.001);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["reasoning_effort"], "high");
        assert_eq!(body["messages"][1]["reasoning_content"], "reasoning");
    }

    #[test]
    fn strips_reasoning_when_thinking_is_disabled() {
        let request = deepseek_request(false);
        let body = request_body("deepseek-v4-pro", &request, ReasoningPolicy::Drop);
        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body["messages"][1].get("reasoning_content").is_none());
    }

    #[test]
    fn user_images_become_content_parts() {
        let dir = std::env::temp_dir().join(format!("whisker-llm-image-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("test.png");
        std::fs::write(&path, b"fake-png-bytes").expect("image file");
        let message = Message::user_with_images(
            "describe",
            vec![ImageRef {
                path: path.clone(),
                mime: "image/png".to_string(),
            }],
        );
        let value = messages_to_json(&[message]);
        assert_eq!(value[0]["role"], "user");
        assert_eq!(value[0]["content"][0]["type"], "text");
        assert_eq!(value[0]["content"][0]["text"], "describe");
        assert_eq!(value[0]["content"][1]["type"], "image_url");
        let url = value[0]["content"][1]["image_url"]["url"]
            .as_str()
            .expect("data url");
        assert!(url.starts_with("data:image/png;base64,"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn user_text_without_images_stays_string() {
        let value = messages_to_json(&[Message::user("hi")]);
        assert_eq!(value[0]["role"], "user");
        assert_eq!(value[0]["content"], "hi");
    }

    #[test]
    fn set_model_updates_model_and_vision() {
        let provider = OpenAiProvider::new(
            "http://127.0.0.1:1/v1",
            "test-key",
            "deepseek-flash",
            1_048_576,
            ReasoningPolicy::Drop,
        )
        .expect("provider");
        assert_eq!(provider.model(), "deepseek-flash");
        assert!(!provider.set_model("deepseek-v4-pro"));
        assert_eq!(provider.model(), "deepseek-v4-pro");
        assert!(provider.set_model("deepseek-chat"));
        assert_eq!(provider.model(), "deepseek-chat");
    }

    #[test]
    fn maps_finish_reasons() {
        assert_eq!(map_finish_reason("stop"), FinishReason::Stop);
        assert_eq!(map_finish_reason("end_turn"), FinishReason::Stop);
        assert_eq!(map_finish_reason("tool_calls"), FinishReason::ToolCalls);
        assert_eq!(map_finish_reason("tool_use"), FinishReason::ToolCalls);
        assert_eq!(map_finish_reason("length"), FinishReason::MaxTokens);
        assert_eq!(map_finish_reason("max_tokens"), FinishReason::MaxTokens);
        assert_eq!(
            map_finish_reason("content_filter"),
            FinishReason::Other("content_filter".to_string())
        );
    }
}
