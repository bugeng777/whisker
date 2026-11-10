use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use agent_core::tool::ToolOutput;
use agent_core::{ProgressSink, Tool, ToolCtx};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PowerShellTool {
    program: String,
}

impl PowerShellTool {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

fn first_word(command: &str) -> String {
    command
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn is_read_only(command: &str) -> bool {
    if ["|", ">", ";", "&", "$(", "`"]
        .iter()
        .any(|token| command.contains(token))
    {
        return false;
    }
    let first = first_word(command);
    const ALLOWED: &[&str] = &[
        "get-childitem",
        "get-content",
        "get-location",
        "get-process",
        "get-service",
        "get-help",
        "get-command",
        "get-item",
        "get-itemproperty",
        "test-path",
        "resolve-path",
        "select-string",
        "select-object",
        "where-object",
        "measure-object",
        "write-output",
        "write-host",
        "format-table",
        "format-list",
        "sort-object",
        "out-string",
        "get-date",
        "get-member",
        "ls",
        "dir",
        "cat",
        "type",
        "pwd",
        "echo",
        "where",
        "findstr",
        "rg",
    ];
    if first == "git" {
        let second = command
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_ascii_lowercase();
        return [
            "status",
            "diff",
            "log",
            "show",
            "branch",
            "rev-parse",
            "ls-files",
        ]
        .contains(&second.as_str());
    }
    ALLOWED.contains(&first.as_str())
}

fn is_destructive(command: &str) -> bool {
    let first = first_word(command);
    const DESTRUCTIVE: &[&str] = &[
        "remove-item",
        "remove-itemproperty",
        "clear-content",
        "set-content",
        "add-content",
        "new-item",
        "move-item",
        "copy-item",
        "rename-item",
        "start-process",
        "invoke-expression",
        "iex",
        "stop-process",
        "stop-service",
        "restart-service",
        "restart-computer",
        "stop-computer",
        "set-executionpolicy",
        "format-volume",
        "reg",
        "schtasks",
        "shutdown",
        "rm",
        "del",
        "erase",
        "rd",
        "rmdir",
        "format",
    ];
    DESTRUCTIVE.contains(&first.as_str())
}

pub fn find_program() -> Option<String> {
    let candidates: &[&str] = if cfg!(windows) {
        &["pwsh.exe", "powershell.exe"]
    } else {
        &["pwsh"]
    };
    for &candidate in candidates {
        if candidate == "powershell.exe" {
            return Some("powershell".to_string());
        }
        if let Ok(path) = std::env::var("PATH") {
            for dir in std::env::split_paths(&path) {
                if Path::new(&dir).join(candidate).is_file() {
                    return Some(candidate.trim_end_matches(".exe").to_string());
                }
            }
        }
    }
    None
}

#[async_trait]
impl Tool for PowerShellTool {
    fn name(&self) -> &str {
        "powershell"
    }

    fn description(&self) -> &str {
        "Run a PowerShell command. Use it on Windows for builds, tests and inspection. Avoid destructive commands; they require explicit user approval."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string" },
                "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds (default 120000, max 600000)" }
            },
            "required": ["command"]
        })
    }

    fn is_read_only(&self, args: &Value) -> bool {
        args.get("command")
            .and_then(|value| value.as_str())
            .map(is_read_only)
            .unwrap_or(false)
    }

    fn is_destructive(&self, args: &Value) -> bool {
        args.get("command")
            .and_then(|value| value.as_str())
            .map(is_destructive)
            .unwrap_or(false)
    }

    fn preview(&self, args: &Value) -> Option<String> {
        args.get("command")
            .and_then(|value| value.as_str())
            .map(|command| format!("> {command}"))
    }

    async fn execute(
        &self,
        args: Value,
        ctx: ToolCtx,
        progress: ProgressSink,
    ) -> Result<ToolOutput> {
        let command = args
            .get("command")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("missing command"))?
            .to_string();
        let timeout = args
            .get("timeout_ms")
            .and_then(|value| value.as_u64())
            .unwrap_or(120_000)
            .clamp(1_000, 600_000);

        progress.emit(format!("> {command}"));

        let child = tokio::process::Command::new(&self.program)
            .args(["-NoProfile", "-NonInteractive", "-Command", &command])
            .current_dir(&ctx.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| anyhow!("failed to spawn {}: {err}", self.program))?;

        let wait = child.wait_with_output();

        tokio::select! {
            _ = ctx.cancel.cancelled() => Ok(ToolOutput::err("Cancelled by user.")),
            result = tokio::time::timeout(Duration::from_millis(timeout), wait) => {
                match result {
                    Err(_) => Ok(ToolOutput::err(format!("Command timed out after {timeout} ms."))),
                    Ok(Err(err)) => Err(anyhow!("failed to run command: {err}")),
                    Ok(Ok(output)) => {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        let mut body = String::new();
                        if !stdout.trim().is_empty() {
                            body.push_str(stdout.trim_end());
                        }
                        if !stderr.trim().is_empty() {
                            if !body.is_empty() {
                                body.push('\n');
                            }
                            body.push_str("[stderr]\n");
                            body.push_str(stderr.trim_end());
                        }
                        if body.is_empty() {
                            body.push_str("(no output)");
                        }
                        let code = output.status.code().unwrap_or(-1);
                        Ok(ToolOutput {
                            content: format!("Exit code: {code}\n{body}"),
                            is_error: !output.status.success(),
                            images: Vec::new(),
                        })
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_rules() {
        assert!(is_read_only("Get-ChildItem"));
        assert!(is_read_only("git status"));
        assert!(!is_read_only("Remove-Item foo"));
        assert!(!is_read_only("Get-ChildItem | Remove-Item"));
    }

    #[test]
    fn destructive_rules() {
        assert!(is_destructive("Remove-Item -Recurse ."));
        assert!(is_destructive("Stop-Process -Name node"));
        assert!(!is_destructive("Get-ChildItem"));
    }
}
