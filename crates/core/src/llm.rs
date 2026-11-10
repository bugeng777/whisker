use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::types::{ChatRequest, StreamEvent, Usage};

#[async_trait]
pub trait LlmClient: Send + Sync {
    fn model(&self) -> String;

    /// Switch the model used for subsequent requests. Returns whether the
    /// new model accepts image content parts.
    fn set_model(&self, model: &str) -> bool;

    fn context_window(&self) -> usize;

    async fn chat_stream(
        &self,
        request: ChatRequest,
        sink: mpsc::UnboundedSender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Result<Usage>;
}
