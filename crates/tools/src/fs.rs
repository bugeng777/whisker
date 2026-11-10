use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use agent_core::tool::{ReadRecord, ToolOutput};
use agent_core::{ProgressSink, Tool, ToolCtx};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

fn resolve(cwd: &Path, value: Option<&Value>) -> Result<PathBuf> {
    let raw = value
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("missing path argument"))?;
    let path = Path::new(raw);
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    })
}

fn hash_text(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn record_read(ctx: &ToolCtx, path: &Path, content: &str, full: bool) {
    let mut ledger = ctx.read_ledger.lock().expect("read ledger");
    ledger.insert(
        path.to_path_buf(),
        ReadRecord {
            at: std::time::SystemTime::now(),
            hash: hash_text(content),
            full,
        },
    );
}

fn read_text(path: &Path) -> Result<String> {
    let bytes =
        fs::read(path).map_err(|err| anyhow!("Failed to read {}: {err}", path.display()))?;
    if bytes.iter().take(8192).any(|byte| *byte == 0) {
        return Err(anyhow!("{} appears to be a binary file", path.display()));
    }
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

fn ensure_fresh_full_read(ctx: &ToolCtx, path: &Path) -> Result<String> {
    let record = {
        let ledger = ctx.read_ledger.lock().expect("read ledger");
        ledger.get(path).cloned()
    };
    let Some(record) = record else {
        return Err(anyhow!(
            "File has not been read yet. Read it first before writing to it."
        ));
    };
    if !record.full {
        return Err(anyhow!(
            "The file was only partially read. Read the whole file before editing it."
        ));
    }
    let content = read_text(path)?;
    if hash_text(&content) != record.hash {
        return Err(anyhow!(
            "File has been modified since it was read. Read it again before attempting to write to it."
        ));
    }
    Ok(content)
}

fn occurrence_lines(content: &str, needle: &str) -> Vec<usize> {
    let mut lines = Vec::new();
    if needle.is_empty() {
        return lines;
    }
    let mut start = 0;
    while let Some(position) = content[start..].find(needle) {
        let absolute = start + position;
        let line = content[..absolute]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1;
        lines.push(line);
        start = absolute + needle.len();
    }
    lines
}

fn whitespace_pattern(needle: &str) -> Option<regex::Regex> {
    let parts: Vec<String> = needle
        .split_whitespace()
        .map(|part| regex::escape(part))
        .collect();
    if parts.is_empty() {
        return None;
    }
    regex::Regex::new(&parts.join(r"\s+")).ok()
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.chars()
        .zip(b.chars())
        .take_while(|(left, right)| left == right)
        .count()
}

fn nearest_line(content: &str, needle: &str) -> Option<(usize, String)> {
    let first = needle.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        return None;
    }
    content
        .lines()
        .enumerate()
        .max_by_key(|(_, line)| common_prefix(line.trim(), first))
        .map(|(index, line)| (index + 1, line.to_string()))
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        text.chars().take(max_chars).collect::<String>() + "..."
    }
}

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a file from disk. Output includes line numbers. Files must be fully read before they can be edited."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, relative to the workspace or absolute" },
                "offset": { "type": "integer", "description": "1-based line to start from" },
                "limit": { "type": "integer", "description": "Maximum number of lines to read (default 2000)" }
            },
            "required": ["path"]
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
        let path = resolve(&ctx.cwd, args.get("path"))?;
        if !path.exists() {
            return Ok(ToolOutput::err(format!(
                "File not found: {}",
                path.display()
            )));
        }
        if path.is_dir() {
            return Ok(ToolOutput::err(format!(
                "{} is a directory. Use glob to list files instead.",
                path.display()
            )));
        }
        let content = match read_text(&path) {
            Ok(content) => content,
            Err(err) => return Ok(ToolOutput::err(err.to_string())),
        };
        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        let offset = args
            .get("offset")
            .and_then(|value| value.as_u64())
            .unwrap_or(1)
            .max(1) as usize;
        let limit = args
            .get("limit")
            .and_then(|value| value.as_u64())
            .unwrap_or(2000)
            .min(5000) as usize;
        let start = (offset - 1).min(total);
        let end = (start + limit).min(total);
        let full = offset == 1 && end == total;

        let mut output = String::new();
        if total == 0 {
            output.push_str("(empty file)\n");
        }
        for (index, line) in lines[start..end].iter().enumerate() {
            output.push_str(&format!("{:>6}\t{}\n", start + index + 1, line));
        }
        if end < total {
            output.push_str(&format!(
                "\n[Showing lines {}-{} of {}. Use offset/limit to read more.]\n",
                start + 1,
                end,
                total
            ));
        }

        record_read(&ctx, &path, &content, full);
        Ok(ToolOutput::ok(output))
    }
}

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Create a new file or overwrite an existing file. Overwriting requires the file to have been read first."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"]
        })
    }

    fn preview(&self, args: &Value) -> Option<String> {
        let content = args.get("content").and_then(|value| value.as_str())?;
        Some(format!(
            "write {} ({} chars)\n{}",
            args.get("path")
                .and_then(|value| value.as_str())
                .unwrap_or("?"),
            content.chars().count(),
            truncate(content, 1200)
        ))
    }

    async fn validate(&self, args: &Value, ctx: &ToolCtx) -> Result<(), String> {
        let path = resolve(&ctx.cwd, args.get("path")).map_err(|err| err.to_string())?;
        if path.is_file() {
            ensure_fresh_full_read(ctx, &path).map_err(|err| err.to_string())?;
        }
        Ok(())
    }

    async fn execute(
        &self,
        args: Value,
        ctx: ToolCtx,
        _progress: ProgressSink,
    ) -> Result<ToolOutput> {
        let path = resolve(&ctx.cwd, args.get("path"))?;
        let content = args
            .get("content")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("missing content argument"))?
            .to_string();
        if path.is_file() {
            ensure_fresh_full_read(&ctx, &path)?;
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        fs::write(&path, &content)?;
        record_read(&ctx, &path, &content, true);
        Ok(ToolOutput::ok(format!(
            "Wrote {} chars to {}",
            content.chars().count(),
            path.display()
        )))
    }
}

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace an exact string in a file. The file must be read first. old_string must uniquely match unless replace_all is true."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" },
                "replace_all": { "type": "boolean" }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    fn preview(&self, args: &Value) -> Option<String> {
        let path = resolve(Path::new("."), args.get("path")).ok()?;
        let old_content = fs::read_to_string(path).ok()?;
        let old_string = args.get("old_string").and_then(|value| value.as_str())?;
        let new_string = args.get("new_string").and_then(|value| value.as_str())?;
        if old_string.is_empty() {
            return None;
        }
        let new_content = old_content.replacen(old_string, new_string, 1);
        if new_content == old_content {
            return Some(format!(
                "edit {} (old_string not found; execution will report details)",
                args.get("path")
                    .and_then(|value| value.as_str())
                    .unwrap_or("?")
            ));
        }
        let diff = similar::TextDiff::from_lines(&old_content, &new_content);
        Some(
            diff.unified_diff()
                .context_radius(3)
                .header("before", "after")
                .to_string(),
        )
    }

    async fn validate(&self, args: &Value, ctx: &ToolCtx) -> Result<(), String> {
        let path = resolve(&ctx.cwd, args.get("path")).map_err(|err| err.to_string())?;
        if !path.is_file() {
            return Err(format!("File not found: {}", path.display()));
        }
        ensure_fresh_full_read(ctx, &path).map_err(|err| err.to_string())?;
        let old_string = args
            .get("old_string")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if old_string.is_empty() {
            return Err("old_string must not be empty".to_string());
        }
        Ok(())
    }

    async fn execute(
        &self,
        args: Value,
        ctx: ToolCtx,
        _progress: ProgressSink,
    ) -> Result<ToolOutput> {
        let path = resolve(&ctx.cwd, args.get("path"))?;
        let old_string = args
            .get("old_string")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("missing old_string"))?
            .to_string();
        let new_string = args
            .get("new_string")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("missing new_string"))?
            .to_string();
        let replace_all = args
            .get("replace_all")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);

        let content = ensure_fresh_full_read(&ctx, &path)?;
        let mut updated = None;
        let mut occurrences = occurrence_lines(&content, &old_string).len();

        if occurrences == 0 {
            if let Some(pattern) = whitespace_pattern(&old_string) {
                let matches: Vec<_> = pattern.find_iter(&content).collect();
                if matches.len() == 1 {
                    let found = matches[0];
                    let mut candidate = String::with_capacity(content.len());
                    candidate.push_str(&content[..found.start()]);
                    candidate.push_str(&new_string);
                    candidate.push_str(&content[found.end()..]);
                    updated = Some(candidate);
                    occurrences = 1;
                }
            }
        }

        if occurrences == 0 {
            let hint = nearest_line(&content, &old_string)
                .map(|(line, text)| {
                    format!(
                        "\nClosest matching line is line {line}: {}",
                        truncate(text.trim(), 160)
                    )
                })
                .unwrap_or_default();
            return Ok(ToolOutput::err(format!(
                "String to replace not found in {}. Re-read the file and retry with an exact excerpt.{hint}",
                path.display()
            )));
        }

        if occurrences > 1 && !replace_all {
            let lines = occurrence_lines(&content, &old_string);
            let shown = lines
                .iter()
                .take(10)
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Ok(ToolOutput::err(format!(
                "old_string matches {occurrences} times (lines: {shown}). Provide a larger, unique excerpt or set replace_all=true."
            )));
        }

        let result = if let Some(candidate) = updated {
            candidate
        } else if replace_all {
            content.replace(&old_string, &new_string)
        } else {
            content.replacen(&old_string, &new_string, 1)
        };

        fs::write(&path, &result)?;
        record_read(&ctx, &path, &result, true);
        Ok(ToolOutput::ok(format!(
            "Edited {}: replaced {} occurrence(s).",
            path.display(),
            if replace_all { occurrences } else { 1 }
        )))
    }
}

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files by glob pattern. Respects .gitignore. Returns paths relative to the search root."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob such as **/*.rs or src/**/*.ts" },
                "path": { "type": "string", "description": "Directory to search (defaults to workspace root)" }
            },
            "required": ["pattern"]
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
        let root = match args.get("path") {
            Some(_) => resolve(&ctx.cwd, args.get("path"))?,
            None => ctx.cwd.clone(),
        };
        let pattern = args
            .get("pattern")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("missing pattern"))?;
        let glob = globset::GlobBuilder::new(pattern)
            .literal_separator(false)
            .build()
            .map_err(|err| anyhow!("invalid glob pattern: {err}"))?
            .compile_matcher();

        let mut results = Vec::new();
        let walker = ignore::WalkBuilder::new(&root).max_depth(Some(40)).build();

        for entry in walker.flatten() {
            if results.len() >= 500 {
                break;
            }
            let Some(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() {
                continue;
            }
            let path = entry.path();
            let relative = path.strip_prefix(&root).unwrap_or(path);
            let name_matches = path
                .file_name()
                .map(|name| glob.is_match(Path::new(name)))
                .unwrap_or(false);
            if glob.is_match(relative) || (!pattern.contains('/') && name_matches) {
                results.push(relative.to_string_lossy().to_string());
            }
        }

        results.sort();
        if results.is_empty() {
            return Ok(ToolOutput::ok("No files matched.".to_string()));
        }
        let mut output = results.join("\n");
        if results.len() >= 500 {
            output.push_str("\n[Truncated at 500 results. Narrow the pattern.]");
        }
        Ok(ToolOutput::ok(output))
    }
}

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents with a regular expression. Respects .gitignore, skips binary files. Returns path:line:text."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Rust regex syntax" },
                "path": { "type": "string", "description": "Directory or file to search" },
                "glob": { "type": "string", "description": "Optional file glob filter such as *.rs" },
                "ignore_case": { "type": "boolean" },
                "head_limit": { "type": "integer", "description": "Maximum matches (default 200)" }
            },
            "required": ["pattern"]
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
        let pattern = args
            .get("pattern")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("missing pattern"))?;
        let ignore_case = args
            .get("ignore_case")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let regex = regex::RegexBuilder::new(pattern)
            .case_insensitive(ignore_case)
            .build()
            .map_err(|err| anyhow!("invalid regex: {err}"))?;
        let limit = args
            .get("head_limit")
            .and_then(|value| value.as_u64())
            .unwrap_or(200)
            .min(1000) as usize;
        let root = match args.get("path") {
            Some(_) => resolve(&ctx.cwd, args.get("path"))?,
            None => ctx.cwd.clone(),
        };
        let glob_filter = match args.get("glob").and_then(|value| value.as_str()) {
            Some(pattern) => Some(
                globset::Glob::new(pattern)
                    .map_err(|err| anyhow!("invalid glob: {err}"))?
                    .compile_matcher(),
            ),
            None => None,
        };

        let mut results = Vec::new();
        let walker = ignore::WalkBuilder::new(&root).max_depth(Some(40)).build();

        'outer: for entry in walker.flatten() {
            let Some(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() {
                continue;
            }
            let path = entry.path();
            let relative = if path == root {
                path.strip_prefix(&ctx.cwd).unwrap_or(path)
            } else {
                path.strip_prefix(&root).unwrap_or(path)
            };
            if let Some(filter) = &glob_filter {
                if !filter.is_match(relative) && !filter.is_match(path) {
                    continue;
                }
            }
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if metadata.len() > 2_000_000 {
                continue;
            }
            let bytes = match fs::read(path) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            if bytes.iter().take(8192).any(|byte| *byte == 0) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            for (line_index, line) in text.lines().enumerate() {
                if regex.is_match(line) {
                    results.push(format!(
                        "{}:{}:{}",
                        relative.display(),
                        line_index + 1,
                        truncate(line.trim(), 400)
                    ));
                    if results.len() >= limit {
                        break 'outer;
                    }
                }
            }
        }

        if results.is_empty() {
            return Ok(ToolOutput::ok("No matches found.".to_string()));
        }
        let mut output = results.join("\n");
        if results.len() >= limit {
            output.push_str(&format!("\n[Truncated at {limit} matches.]"));
        }
        Ok(ToolOutput::ok(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{ApprovalDecision, ApprovalRequest, CancelHandle, Interactor};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    struct DenyInteractor;

    #[async_trait]
    impl Interactor for DenyInteractor {
        async fn approve(&self, _request: ApprovalRequest) -> ApprovalDecision {
            ApprovalDecision::Deny("test".to_string())
        }

        async fn ask(&self, _question: String, _options: Vec<String>) -> Option<String> {
            None
        }
    }

    fn test_ctx(cwd: PathBuf) -> ToolCtx {
        ToolCtx {
            cwd,
            read_ledger: Arc::new(Mutex::new(HashMap::new())),
            cancel: CancelHandle::new().token(),
            spill_dir: std::env::temp_dir(),
            interactor: Arc::new(DenyInteractor),
        }
    }

    #[tokio::test]
    async fn grep_searches_explicit_files_and_directories() {
        let root = std::env::temp_dir().join(format!("whisker-grep-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("temp dir");
        fs::write(root.join("sample.txt"), "hello world\nneedle here\n").expect("sample");

        let tool = GrepTool;
        let file_output = tool
            .execute(
                json!({"pattern": "needle", "path": "sample.txt"}),
                test_ctx(root.clone()),
                ProgressSink::default(),
            )
            .await
            .expect("grep file");
        assert_eq!(file_output.content, "sample.txt:2:needle here");

        let dir_output = tool
            .execute(
                json!({"pattern": "hello", "path": "."}),
                test_ctx(root.clone()),
                ProgressSink::default(),
            )
            .await
            .expect("grep dir");
        assert!(dir_output.content.contains("sample.txt:1:hello world"));

        fs::remove_dir_all(&root).ok();
    }
}
