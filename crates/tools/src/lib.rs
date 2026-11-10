pub mod ask;
pub mod bash;
pub mod fs;
pub mod lsp;
pub mod open_app;
pub mod powershell;
pub mod skills;
pub mod todo;

pub use lsp::{LspServerConfig, LspTool};
pub use open_app::OpenAppTool;
pub use powershell::PowerShellTool;
pub use skills::{SkillCatalog, SkillTool};

use std::sync::Arc;

use agent_core::Tool;

pub fn native_tools(sandbox: bool) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(fs::ReadTool),
        Arc::new(fs::WriteTool),
        Arc::new(fs::EditTool),
        Arc::new(fs::GlobTool),
        Arc::new(fs::GrepTool),
        Arc::new(bash::BashTool::new(sandbox)),
        Arc::new(OpenAppTool::new()),
        Arc::new(todo::TodoWriteTool),
        Arc::new(ask::AskUserTool),
    ]
}
