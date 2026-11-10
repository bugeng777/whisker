use agent_core::tool::ToolOutput;
use agent_core::{ProgressSink, Tool, ToolCtx};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct AskUserTool;

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        "Ask the user a question when you are blocked or need a decision that only they can make. Offer concrete options when possible. Do not use it for questions you can answer by inspecting the codebase."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "question": { "type": "string" },
                "options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional concrete choices"
                }
            },
            "required": ["question"]
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
        let question = args
            .get("question")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("missing question"))?
            .to_string();
        let options = args
            .get("options")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(|text| text.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        match ctx.interactor.ask(question, options).await {
            Some(answer) => Ok(ToolOutput::ok(format!("User answer: {answer}"))),
            None => Ok(ToolOutput::ok(
                "The user did not answer (non-interactive session). Proceed with your best judgement or stop if the decision is irreversible."
                    .to_string(),
            )),
        }
    }
}
