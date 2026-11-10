use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::policy::Interactor;
use crate::types::{Event, ImageRef, ToolSpec};

#[derive(Debug, Clone)]
pub struct ReadRecord {
    pub at: SystemTime,
    pub hash: u64,
    pub full: bool,
}

pub type ReadLedger = Arc<Mutex<HashMap<PathBuf, ReadRecord>>>;

#[derive(Clone)]
pub struct ToolCtx {
    pub cwd: PathBuf,
    pub read_ledger: ReadLedger,
    pub cancel: CancellationToken,
    pub spill_dir: PathBuf,
    pub interactor: Arc<dyn Interactor>,
}

#[derive(Clone, Default)]
pub struct ProgressSink {
    pub tx: Option<mpsc::UnboundedSender<Event>>,
    pub call_id: String,
    pub name: String,
}

impl ProgressSink {
    pub fn new(
        tx: mpsc::UnboundedSender<Event>,
        call_id: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            tx: Some(tx),
            call_id: call_id.into(),
            name: name.into(),
        }
    }

    pub fn emit(&self, text: impl Into<String>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Event::ToolProgress {
                id: self.call_id.clone(),
                name: self.name.clone(),
                text: text.into(),
            });
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
    pub images: Vec<ImageRef>,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            images: Vec::new(),
        }
    }

    pub fn err(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            images: Vec::new(),
        }
    }

    pub fn with_images(mut self, images: Vec<ImageRef>) -> Self {
        self.images = images;
        self
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;

    fn description(&self) -> &str;

    fn parameters(&self) -> serde_json::Value;

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters(),
        }
    }

    fn is_read_only(&self, _args: &serde_json::Value) -> bool {
        false
    }

    fn is_destructive(&self, _args: &serde_json::Value) -> bool {
        false
    }

    fn concurrency_safe(&self, args: &serde_json::Value) -> bool {
        self.is_read_only(args)
    }

    fn preview(&self, _args: &serde_json::Value) -> Option<String> {
        None
    }

    async fn validate(&self, _args: &serde_json::Value, _ctx: &ToolCtx) -> Result<(), String> {
        Ok(())
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: ToolCtx,
        progress: ProgressSink,
    ) -> anyhow::Result<ToolOutput>;
}

pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new(tools: Vec<Arc<dyn Tool>>) -> Self {
        Self { tools }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|tool| tool.name() == name).cloned()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|tool| tool.spec()).collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect()
    }

    pub fn all(&self) -> &[Arc<dyn Tool>] {
        &self.tools
    }

    pub fn push(&mut self, tool: Arc<dyn Tool>) {
        self.tools.push(tool);
    }

    pub fn filtered(&self, names: &[&str]) -> ToolRegistry {
        ToolRegistry {
            tools: self
                .tools
                .iter()
                .filter(|tool| names.contains(&tool.name()))
                .cloned()
                .collect(),
        }
    }
}
