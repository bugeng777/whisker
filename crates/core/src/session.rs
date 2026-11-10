use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::context::TurnContext;
use crate::types::{Event, Message, Usage};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationState {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub model: String,
    pub messages: Vec<Message>,
    pub ctx: TurnContext,
    pub usage: Usage,
    #[serde(default)]
    pub plan: Option<String>,
}

pub struct SessionStore {
    pub dir: PathBuf,
    pub state: ConversationState,
}

impl SessionStore {
    pub fn create(base: &Path, model: &str) -> Result<Self> {
        fs::create_dir_all(base)?;
        let id = format!(
            "{}-{}",
            Utc::now().format("%Y%m%d-%H%M%S"),
            &Uuid::new_v4().to_string()[..8]
        );
        let dir = base.join(&id);
        fs::create_dir_all(dir.join("spill"))?;
        let store = Self {
            dir,
            state: ConversationState {
                id,
                created_at: Utc::now(),
                model: model.to_string(),
                messages: Vec::new(),
                ctx: TurnContext::default(),
                usage: Usage::default(),
                plan: None,
            },
        };
        store.save()?;
        Ok(store)
    }

    pub fn resume_latest(base: &Path) -> Result<Self> {
        let mut candidates: Vec<PathBuf> = fs::read_dir(base)
            .with_context(|| format!("read session base {}", base.display()))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_dir() && path.join("state.json").exists())
            .collect();
        candidates.sort();
        let dir = candidates
            .pop()
            .ok_or_else(|| anyhow!("no resumable session found in {}", base.display()))?;
        Self::from_dir(dir)
    }

    pub fn resume(base: &Path, prefix: &str) -> Result<Self> {
        let mut candidates: Vec<PathBuf> = fs::read_dir(base)
            .with_context(|| format!("read session base {}", base.display()))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_dir()
                    && path.join("state.json").exists()
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(|name| name.starts_with(prefix))
                        .unwrap_or(false)
            })
            .collect();
        candidates.sort();
        match candidates.len() {
            0 => Err(anyhow!("no session matching '{}'", prefix)),
            1 => Self::from_dir(candidates.remove(0)),
            _ => Err(anyhow!("session prefix '{}' is ambiguous", prefix)),
        }
    }

    fn from_dir(dir: PathBuf) -> Result<Self> {
        let raw = fs::read_to_string(dir.join("state.json"))
            .with_context(|| format!("read state in {}", dir.display()))?;
        let state: ConversationState = serde_json::from_str(&raw)?;
        fs::create_dir_all(dir.join("spill")).ok();
        Ok(Self { dir, state })
    }

    pub fn append_event(&self, event: &Event) -> Result<()> {
        let line = serde_json::to_string(event)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("events.jsonl"))?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        let raw = serde_json::to_string_pretty(&self.state)?;
        fs::write(self.dir.join("state.json"), raw)?;
        Ok(())
    }

    pub fn spill_dir(&self) -> PathBuf {
        self.dir.join("spill")
    }
}
