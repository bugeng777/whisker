use std::process::Stdio;
use std::time::Duration;

use agent_core::tool::ToolOutput;
use agent_core::{ProgressSink, Tool, ToolCtx};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct OpenAppTool;

impl OpenAppTool {
    pub fn new() -> Self {
        Self
    }
}

fn parse_args(args: &Value) -> Result<(String, Option<String>), String> {
    let app = args
        .get("app")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim();
    if app.is_empty() {
        return Err("missing app".to_string());
    }
    if app.contains('\n') || app.contains('\r') || app.contains('\0') {
        return Err("app name must be a single line".to_string());
    }
    let target = args
        .get("target")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    Ok((app.to_string(), target))
}

#[cfg(target_os = "macos")]
fn launch_command(app: &str, target: Option<&str>) -> Result<(&'static str, Vec<String>)> {
    let mut args = vec!["-a".to_string(), app.to_string()];
    if let Some(target) = target {
        args.push(target.to_string());
    }
    Ok(("open", args))
}

#[cfg(target_os = "windows")]
fn launch_command(app: &str, target: Option<&str>) -> Result<(&'static str, Vec<String>)> {
    let mut args = vec![
        "/C".to_string(),
        "start".to_string(),
        String::new(),
        app.to_string(),
    ];
    if let Some(target) = target {
        args.push(target.to_string());
    }
    Ok(("cmd", args))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn launch_command(app: &str, target: Option<&str>) -> Result<(&'static str, Vec<String>)> {
    match target {
        Some(target) => Ok(("xdg-open", vec![target.to_string()])),
        None => Ok(("gtk-launch", vec![app.to_string()])),
    }
}

#[async_trait]
impl Tool for OpenAppTool {
    fn name(&self) -> &str {
        "open_app"
    }

    fn description(&self) -> &str {
        "Open or activate a desktop application by name, optionally with a file path or URL. Use this instead of bash for launching apps. It requires user approval unless the session is in auto mode."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "app": { "type": "string", "description": "Application name, for example \"Google Chrome\" or \"Blender\"" },
                "target": { "type": "string", "description": "Optional file path or URL to open with the application" }
            },
            "required": ["app"]
        })
    }

    fn preview(&self, args: &Value) -> Option<String> {
        let app = args.get("app").and_then(|value| value.as_str())?;
        let target = args.get("target").and_then(|value| value.as_str());
        Some(match target {
            Some(target) if !target.trim().is_empty() => format!("open {app} → {target}"),
            _ => format!("open {app}"),
        })
    }

    async fn validate(&self, args: &Value, _ctx: &ToolCtx) -> Result<(), String> {
        let (app, _) = parse_args(args)?;
        #[cfg(target_os = "macos")]
        {
            if !app.contains('/') {
                let status = tokio::process::Command::new("open")
                    .args(["-Ra", &app])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await
                    .map_err(|err| format!("failed to query application: {err}"))?;
                if !status.success() {
                    return Err(format!("application not found: {app}"));
                }
            }
        }
        Ok(())
    }

    async fn execute(
        &self,
        args: Value,
        ctx: ToolCtx,
        progress: ProgressSink,
    ) -> Result<ToolOutput> {
        let (app, target) = parse_args(&args).map_err(|err| anyhow!(err))?;
        let (program, command_args) =
            launch_command(&app, target.as_deref()).map_err(|err| anyhow!(err))?;
        progress.emit(match &target {
            Some(target) => format!("open {app} → {target}"),
            None => format!("open {app}"),
        });

        let child = tokio::process::Command::new(program)
            .args(&command_args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| anyhow!("failed to launch {program}: {err}"))?;
        let wait = child.wait_with_output();

        tokio::select! {
            _ = ctx.cancel.cancelled() => Ok(ToolOutput::err("Cancelled by user.")),
            result = tokio::time::timeout(Duration::from_secs(15), wait) => {
                match result {
                    Err(_) => Ok(ToolOutput::err("Timed out while launching the application.")),
                    Ok(Err(err)) => Err(anyhow!("failed to launch application: {err}")),
                    Ok(Ok(output)) => {
                        let mut content = match &target {
                            Some(target) => format!("Opened {app} with {target}"),
                            None => format!("Opened {app}"),
                        };
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        if !stdout.trim().is_empty() {
                            content.push('\n');
                            content.push_str(stdout.trim_end());
                        }
                        if !stderr.trim().is_empty() {
                            content.push('\n');
                            content.push_str(stderr.trim_end());
                        }
                        Ok(ToolOutput {
                            content,
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
    fn parses_app_and_optional_target() {
        assert_eq!(
            parse_args(&json!({"app": " Google Chrome "})).unwrap(),
            ("Google Chrome".to_string(), None)
        );
        assert_eq!(
            parse_args(&json!({"app": "Blender", "target": " /tmp/a.blend "})).unwrap(),
            ("Blender".to_string(), Some("/tmp/a.blend".to_string()))
        );
    }

    #[test]
    fn rejects_empty_app() {
        assert!(parse_args(&json!({})).is_err());
        assert!(parse_args(&json!({"app": "   "})).is_err());
        assert!(parse_args(&json!({"app": "bad\nname"})).is_err());
    }

    #[test]
    fn preview_shows_app_and_target() {
        let tool = OpenAppTool::new();
        assert_eq!(
            tool.preview(&json!({"app": "Google Chrome"})).as_deref(),
            Some("open Google Chrome")
        );
        assert_eq!(
            tool.preview(&json!({"app": "Google Chrome", "target": "https://example.com"}))
                .as_deref(),
            Some("open Google Chrome → https://example.com")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_launch_uses_open_app() {
        let (program, args) = launch_command("Google Chrome", Some("https://example.com")).unwrap();
        assert_eq!(program, "open");
        assert_eq!(
            args,
            vec!["-a", "Google Chrome", "https://example.com"]
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        );
    }
}
