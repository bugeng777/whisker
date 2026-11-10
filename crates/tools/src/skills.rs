use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use agent_core::tool::ToolOutput;
use agent_core::{ProgressSink, Tool, ToolCtx};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

const MAX_SKILL_BYTES: u64 = 200 * 1024;

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    pub path: String,
}

#[derive(Debug, Clone, Default)]
pub struct SkillCatalog {
    skills: Vec<Skill>,
}

impl SkillCatalog {
    pub fn load(cwd: &Path) -> Self {
        let mut map: BTreeMap<String, Skill> = BTreeMap::new();
        let home = dirs::home_dir();

        if let Some(home) = &home {
            load_dir(&home.join(".claude").join("skills"), &mut map);
            load_dir(&home.join(".whisker").join("skills"), &mut map);
        }
        load_dir(&cwd.join(".claude").join("skills"), &mut map);
        load_dir(&cwd.join(".whisker").join("skills"), &mut map);

        Self {
            skills: map.into_values().collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|skill| skill.name == name)
    }

    pub fn names(&self) -> Vec<String> {
        self.skills.iter().map(|skill| skill.name.clone()).collect()
    }

    pub fn summaries(&self) -> Vec<(String, String)> {
        self.skills
            .iter()
            .map(|skill| (skill.name.clone(), skill.description.clone()))
            .collect()
    }

    pub fn prompt_section(&self) -> String {
        if self.skills.is_empty() {
            return String::new();
        }
        let mut out = String::from(
            "\n# Skills\n\nWhen a task matches one of these skills, call the `skill` tool with its name to load the full instructions, then follow them. Do not load skills for unrelated tasks.\n\n",
        );
        for skill in &self.skills {
            let description = if skill.description.is_empty() {
                "(no description)"
            } else {
                skill.description.as_str()
            };
            out.push_str(&format!("- {}: {}\n", skill.name, description));
        }
        out
    }
}

fn load_dir(dir: &Path, map: &mut BTreeMap<String, Skill>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();

    for path in paths {
        let (fallback_name, file) = if path.is_dir() {
            let file = path.join("SKILL.md");
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            (name, file)
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            (name, path.clone())
        } else {
            continue;
        };

        if fallback_name.is_empty() || !file.is_file() {
            continue;
        }
        let metadata = match fs::metadata(&file) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.len() > MAX_SKILL_BYTES {
            tracing::warn!(
                "skill {} is larger than {} bytes, skipped",
                file.display(),
                MAX_SKILL_BYTES
            );
            continue;
        }
        let Ok(raw) = fs::read_to_string(&file) else {
            continue;
        };
        let mut skill = parse_skill(&fallback_name, &raw);
        skill.path = file.to_string_lossy().to_string();
        if !skill.body.trim().is_empty() {
            map.insert(skill.name.clone(), skill);
        }
    }
}

pub fn parse_skill(fallback_name: &str, raw: &str) -> Skill {
    let raw = raw.trim_start_matches('\u{feff}');
    let mut lines = raw.lines();
    if lines.next().map(|line| line.trim()) == Some("---") {
        let mut front = Vec::new();
        let mut body_lines = Vec::new();
        let mut in_front = true;
        let mut closed = false;
        for line in lines {
            if in_front && line.trim() == "---" {
                in_front = false;
                closed = true;
                continue;
            }
            if in_front {
                front.push(line);
            } else {
                body_lines.push(line);
            }
        }
        if closed {
            let mut name = None;
            let mut description = None;
            for line in front {
                let Some((key, value)) = line.split_once(':') else {
                    continue;
                };
                let value = value
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
                match key.trim() {
                    "name" if !value.is_empty() => name = Some(value),
                    "description" => description = Some(value),
                    _ => {}
                }
            }
            return Skill {
                name: name.unwrap_or_else(|| fallback_name.to_string()),
                description: description.unwrap_or_default(),
                body: body_lines.join("\n").trim().to_string(),
                path: String::new(),
            };
        }
    }

    let first_line = raw
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    Skill {
        name: fallback_name.to_string(),
        description: truncate_chars(first_line, 140),
        body: raw.trim().to_string(),
        path: String::new(),
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect::<String>() + "..."
    }
}

pub struct SkillTool {
    catalog: Arc<SkillCatalog>,
}

impl SkillTool {
    pub fn new(catalog: Arc<SkillCatalog>) -> Self {
        Self { catalog }
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        "Load the full instructions for a named skill. Use it when the user's task matches an available skill, then follow the loaded procedure."
    }

    fn parameters(&self) -> Value {
        let names: Vec<Value> = self
            .catalog
            .names()
            .into_iter()
            .map(Value::String)
            .collect();
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "enum": names,
                    "description": "Skill name"
                }
            },
            "required": ["name"]
        })
    }

    fn is_read_only(&self, _args: &Value) -> bool {
        true
    }

    async fn execute(
        &self,
        args: Value,
        _ctx: ToolCtx,
        _progress: ProgressSink,
    ) -> Result<ToolOutput> {
        let name = args
            .get("name")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("missing skill name"))?;
        let Some(skill) = self.catalog.get(name) else {
            return Ok(ToolOutput::err(format!(
                "Unknown skill '{name}'. Available skills: {}",
                self.catalog.names().join(", ")
            )));
        };
        Ok(ToolOutput::ok(format!(
            "# Skill: {}\n\n{}\n\n[Follow these instructions for the current task.]",
            skill.name, skill.body
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter() {
        let raw = "---\nname: review\ndescription: Review a diff\n---\nStep one.\nStep two.";
        let skill = parse_skill("fallback", raw);
        assert_eq!(skill.name, "review");
        assert_eq!(skill.description, "Review a diff");
        assert_eq!(skill.body, "Step one.\nStep two.");
    }

    #[test]
    fn parses_plain_markdown() {
        let raw = "Use this when debugging.\n\nMore detail.";
        let skill = parse_skill("debug", raw);
        assert_eq!(skill.name, "debug");
        assert_eq!(skill.description, "Use this when debugging.");
        assert!(skill.body.contains("More detail."));
    }

    #[test]
    fn prompt_section_lists_skills() {
        let catalog = SkillCatalog {
            skills: vec![Skill {
                name: "review".to_string(),
                description: "Review a diff".to_string(),
                body: "body".to_string(),
                path: String::new(),
            }],
        };
        let section = catalog.prompt_section();
        assert!(section.contains("- review: Review a diff"));
        assert!(section.contains("`skill` tool"));
    }
}
