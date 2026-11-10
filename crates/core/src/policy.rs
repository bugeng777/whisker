use std::collections::HashSet;

use async_trait::async_trait;

use crate::context::Mode;
use crate::tool::Tool;

#[derive(Debug, Clone)]
pub struct Rule {
    pub tool: String,
    pub arg_pattern: Option<String>,
}

impl Rule {
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        if let Some(open) = raw.find('(') {
            if !raw.ends_with(')') {
                return None;
            }
            let tool = raw[..open].trim().to_string();
            let pattern = raw[open + 1..raw.len() - 1]
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
            Some(Self {
                tool,
                arg_pattern: Some(pattern),
            })
        } else {
            Some(Self {
                tool: raw.to_string(),
                arg_pattern: None,
            })
        }
    }
}

fn rule_subject(tool: &str, args: &serde_json::Value) -> String {
    if tool == "bash" {
        args.get("command")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string()
    } else if tool == "open_app" {
        args.get("app")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string()
    } else {
        args.get("path")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string()
    }
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut rest = text;
    if !parts[0].is_empty() {
        if !rest.starts_with(parts[0]) {
            return false;
        }
        rest = &rest[parts[0].len()..];
    }
    for (index, part) in parts.iter().enumerate().skip(1) {
        if part.is_empty() {
            continue;
        }
        if index == parts.len() - 1 && !pattern.ends_with('*') {
            return rest.ends_with(part);
        }
        match rest.find(part) {
            Some(position) => rest = &rest[position + part.len()..],
            None => return false,
        }
    }
    true
}

fn rule_matches(rule: &Rule, tool: &str, args: &serde_json::Value) -> bool {
    if rule.tool != "*" && rule.tool != tool {
        return false;
    }
    match &rule.arg_pattern {
        None => true,
        Some(pattern) => wildcard_match(pattern, &rule_subject(tool, args)),
    }
}

pub fn rule_key(tool: &str, args: &serde_json::Value) -> String {
    format!("{}({})", tool, rule_subject(tool, args))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask,
    Deny(String),
}

pub struct Policy {
    pub mode: Mode,
    allow: Vec<Rule>,
    deny: Vec<Rule>,
    session_allow: HashSet<String>,
}

impl Policy {
    pub fn new(mode: Mode, allow: &[String], deny: &[String]) -> Self {
        Self {
            mode,
            allow: allow.iter().filter_map(|raw| Rule::parse(raw)).collect(),
            deny: deny.iter().filter_map(|raw| Rule::parse(raw)).collect(),
            session_allow: HashSet::new(),
        }
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    pub fn cycle_mode(&mut self) -> Mode {
        self.mode = self.mode.next();
        self.mode
    }

    pub fn decide(&self, tool: &dyn Tool, args: &serde_json::Value) -> Decision {
        let name = tool.name();

        if let Some(rule) = self.deny.iter().find(|rule| rule_matches(rule, name, args)) {
            return Decision::Deny(format!(
                "matched deny rule {}({})",
                rule.tool,
                rule.arg_pattern.clone().unwrap_or_default()
            ));
        }

        if self.mode == Mode::Plan && !tool.is_read_only(args) {
            return Decision::Deny("plan mode allows read-only tools only".to_string());
        }

        if self.session_allow.contains(&rule_key(name, args)) {
            return Decision::Allow;
        }

        if self.allow.iter().any(|rule| rule_matches(rule, name, args)) {
            return Decision::Allow;
        }

        if tool.is_destructive(args) {
            return Decision::Ask;
        }

        if tool.is_read_only(args) {
            return Decision::Allow;
        }

        if self.mode == Mode::AutoAccept {
            return Decision::Allow;
        }

        Decision::Ask
    }

    pub fn allow_always(&mut self, tool: &str, args: &serde_json::Value) {
        self.session_allow.insert(rule_key(tool, args));
    }
}

#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub tool: String,
    pub preview: String,
    pub rule: String,
}

#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    Once,
    Always,
    Deny(String),
}

#[async_trait]
pub trait Interactor: Send + Sync {
    async fn approve(&self, request: ApprovalRequest) -> ApprovalDecision;
    async fn ask(&self, question: String, options: Vec<String>) -> Option<String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{ProgressSink, ToolCtx, ToolOutput};

    struct FakeOpenApp;

    #[async_trait]
    impl Tool for FakeOpenApp {
        fn name(&self) -> &str {
            "open_app"
        }

        fn description(&self) -> &str {
            ""
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
            _ctx: ToolCtx,
            _progress: ProgressSink,
        ) -> anyhow::Result<ToolOutput> {
            unreachable!()
        }
    }

    #[test]
    fn open_app_rule_uses_app_name() {
        let args = serde_json::json!({
            "app": "Google Chrome",
            "target": "https://example.com"
        });
        assert_eq!(rule_key("open_app", &args), "open_app(Google Chrome)");
        let rule = Rule::parse("open_app(Google Chrome)").expect("rule");
        assert!(rule_matches(&rule, "open_app", &args));
    }

    #[test]
    fn open_app_asks_by_default_and_allows_in_auto() {
        let args = serde_json::json!({"app": "Google Chrome"});
        let normal = Policy::new(Mode::Normal, &[], &[]);
        assert_eq!(normal.decide(&FakeOpenApp, &args), Decision::Ask);
        let auto = Policy::new(Mode::AutoAccept, &[], &[]);
        assert_eq!(auto.decide(&FakeOpenApp, &args), Decision::Allow);
        let allowed = Policy::new(Mode::Normal, &["open_app(Google Chrome)".to_string()], &[]);
        assert_eq!(allowed.decide(&FakeOpenApp, &args), Decision::Allow);
    }
}
