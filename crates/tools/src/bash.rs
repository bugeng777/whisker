use std::process::Stdio;
use std::time::Duration;

use agent_core::tool::ToolOutput;
use agent_core::{ProgressSink, Tool, ToolCtx};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct BashTool {
    sandbox: bool,
}

impl BashTool {
    pub fn new(sandbox: bool) -> Self {
        Self { sandbox }
    }
}

fn seatbelt_profile(cwd: &std::path::Path) -> String {
    let escape = |value: &str| value.replace('\\', "\\\\").replace('"', "\\\"");
    let cwd = escape(&cwd.to_string_lossy());
    let temp = escape(&std::env::temp_dir().to_string_lossy());
    format!(
        "(version 1)(allow default)(deny file-write*)(allow file-write* \
         (subpath \"{cwd}\") (subpath \"{temp}\") (subpath \"/private/tmp\") \
         (subpath \"/private/var/folders\") (literal \"/dev/null\") \
         (literal \"/dev/tty\") (literal \"/dev/stdout\") (literal \"/dev/stderr\"))"
    )
}

#[derive(Default)]
struct SafetyInfo {
    has_operator: bool,
    commands: Vec<String>,
}

fn visit(node: tree_sitter::Node, source: &[u8], info: &mut SafetyInfo) {
    match node.kind() {
        "file_redirect"
        | "heredoc_redirect"
        | "herestring_redirect"
        | "pipeline"
        | "list"
        | "command_substitution"
        | "process_substitution"
        | "subshell"
        | "binary_expression" => info.has_operator = true,
        "command" => {
            let name_node = node
                .child_by_field_name("name")
                .or_else(|| node.named_child(0));
            if let Some(name_node) = name_node {
                if let Ok(text) = name_node.utf8_text(source) {
                    info.commands.push(text.to_string());
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(child, source, info);
    }
}

fn analyze(command: &str) -> SafetyInfo {
    let mut info = SafetyInfo::default();
    let mut parser = tree_sitter::Parser::new();
    if parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .is_err()
    {
        return info;
    }
    let Some(tree) = parser.parse(command, None) else {
        return info;
    };
    if tree.root_node().has_error() {
        return info;
    }
    visit(tree.root_node(), command.as_bytes(), &mut info);
    info
}

const READ_ONLY_COMMANDS: &[&str] = &[
    "ls",
    "cat",
    "head",
    "tail",
    "grep",
    "rg",
    "find",
    "wc",
    "pwd",
    "echo",
    "which",
    "stat",
    "file",
    "du",
    "df",
    "tree",
    "git",
    "sort",
    "uniq",
    "cut",
    "tr",
    "diff",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "date",
    "whoami",
    "uname",
    "printenv",
    "true",
    "test",
    "nl",
    "less",
    "more",
    "bat",
    "fd",
    "jq",
    "yq",
    "od",
    "xxd",
    "shasum",
    "md5sum",
    "sha256sum",
];

fn is_read_only_command(command: &str) -> bool {
    let tokens = match shell_words::split(command) {
        Ok(tokens) => tokens,
        Err(_) => return false,
    };
    if tokens.is_empty() {
        return false;
    }
    let first = tokens[0].as_str();
    if !READ_ONLY_COMMANDS.contains(&first) {
        return false;
    }
    let lower = command.to_ascii_lowercase();
    if first == "git" {
        const READ_ONLY_GIT: &[&str] = &[
            "status",
            "diff",
            "log",
            "show",
            "branch",
            "rev-parse",
            "ls-files",
            "blame",
            "describe",
            "shortlog",
            "cat-file",
            "remote",
            "config",
            "grep",
        ];
        let sub = tokens.get(1).map(|token| token.as_str()).unwrap_or("");
        if !READ_ONLY_GIT.contains(&sub) {
            return false;
        }
        const WRITE_MARKERS: &[&str] = &[
            "git branch -d",
            "git branch -m",
            "git config --unset",
            "git config set",
            "git remote add",
            "git remote remove",
            "git remote set",
        ];
        if WRITE_MARKERS.iter().any(|marker| lower.contains(marker)) {
            return false;
        }
    }
    if first == "find" {
        if lower.contains(" -delete") || lower.contains(" -exec") || lower.contains(" -execdir") {
            return false;
        }
    }
    if first == "date" && lower.contains(" -s") {
        return false;
    }
    true
}

fn is_read_only(command: &str) -> bool {
    if command.trim().is_empty() {
        return false;
    }
    let info = analyze(command);
    if info.has_operator || info.commands.is_empty() {
        return false;
    }
    is_read_only_command(command)
}

fn is_destructive(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    let tokens = shell_words::split(command).unwrap_or_default();
    let first = tokens.first().map(|token| token.as_str()).unwrap_or("");
    const DANGEROUS: &[&str] = &[
        "rm",
        "rmdir",
        "sudo",
        "su",
        "mkfs",
        "dd",
        "shutdown",
        "reboot",
        "kill",
        "pkill",
        "killall",
        "chmod",
        "chown",
        "chgrp",
        "mv",
        "truncate",
        "fdisk",
        "diskutil",
        "launchctl",
        "systemctl",
        "service",
        "crontab",
        "useradd",
        "userdel",
        "passwd",
        "rmtrash",
    ];
    if DANGEROUS.contains(&first) {
        return true;
    }
    const PATTERNS: &[&str] = &[
        "| sh",
        "| bash",
        "| zsh",
        "| python",
        "| perl",
        "curl -s |",
        "wget -qo- |",
        "git push --force",
        "git push -f",
        "git reset --hard",
        "git clean -fd",
        "git clean -f",
        "> /dev/sd",
        "> /dev/disk",
        "chmod 777 /",
        "chown -r /",
        "dd if=",
    ];
    PATTERNS.iter().any(|pattern| lower.contains(pattern))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_allows_workspace_and_temp_only() {
        let profile = seatbelt_profile(std::path::Path::new("/tmp/workspace"));
        assert!(profile.contains("(deny file-write*)"));
        assert!(profile.contains("/tmp/workspace"));
        assert!(profile.contains("/private/tmp"));
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command in the workspace. Use it for builds, tests, git and inspection. Avoid destructive commands; they require explicit user approval."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to run with bash -lc" },
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
            .map(|command| format!("$ {command}"))
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

        progress.emit(format!("$ {command}"));

        let mut shell = if self.sandbox && cfg!(target_os = "macos") {
            let mut sandboxed = tokio::process::Command::new("sandbox-exec");
            sandboxed
                .arg("-p")
                .arg(seatbelt_profile(&ctx.cwd))
                .arg("bash");
            sandboxed
        } else {
            tokio::process::Command::new("bash")
        };
        let child = shell
            .arg("-lc")
            .arg(&command)
            .current_dir(&ctx.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| anyhow!("failed to spawn shell: {err}"))?;

        let wait = child.wait_with_output();

        tokio::select! {
            _ = ctx.cancel.cancelled() => Ok(ToolOutput::err("Cancelled by user.")),
            result = tokio::time::timeout(Duration::from_millis(timeout), wait) => {
                match result {
                    Err(_) => Ok(ToolOutput::err(format!(
                        "Command timed out after {timeout} ms."
                    ))),
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
