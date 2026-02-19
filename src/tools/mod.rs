pub mod config;
pub mod file;
pub mod git;
pub mod memory;
pub mod self_info;
pub mod shell;
pub mod skill;
pub mod traits;

pub use traits::{Tool, ToolResult};

use std::path::PathBuf;
use std::sync::Arc;

use crate::config::Config;
use crate::memory::Memory;
use crate::skills::SkillMeta;
use config::ConfigTool;
use file::{FileReadTool, FileWriteTool};
use git::GitTool;
use memory::{MemoryForgetTool, MemoryRecallTool, MemoryStoreTool};
use self_info::SelfInfoTool;
use shell::ShellTool;
use skill::SkillTool;

/// 创建所有工具实例
pub fn create_tools(
    app_config: Config,
    data_dir: PathBuf,
    log_dir: PathBuf,
    config_path: PathBuf,
    skills: Vec<SkillMeta>,
    memory: Arc<dyn Memory>,
) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(ShellTool),
        Box::new(FileReadTool),
        Box::new(FileWriteTool),
        Box::new(ConfigTool),
        Box::new(SelfInfoTool::new(app_config, data_dir, log_dir, config_path)),
        Box::new(SkillTool::new(skills)),
        Box::new(GitTool),
        Box::new(MemoryStoreTool::new(memory.clone())),
        Box::new(MemoryRecallTool::new(memory.clone())),
        Box::new(MemoryForgetTool::new(memory)),
    ]
}
