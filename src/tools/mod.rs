pub mod config;
pub mod file;
pub mod git;
pub mod http;
pub mod memory;
pub mod routine;
pub mod self_info;
pub mod shell;
pub mod skill;
pub mod traits;

pub use traits::{Tool, ToolResult};

use std::path::PathBuf;
use std::sync::Arc;

use crate::config::Config;
use crate::memory::Memory;
use crate::routines::RoutineEngine;
use crate::skills::SkillMeta;
use config::ConfigTool;
use file::{FileReadTool, FileWriteTool};
use git::GitTool;
use http::HttpRequestTool;
use memory::{MemoryForgetTool, MemoryRecallTool, MemoryStoreTool};
use routine::RoutineTool;
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
    routine_engine: Option<Arc<RoutineEngine>>,
) -> Vec<Box<dyn Tool>> {
    let mut tools: Vec<Box<dyn Tool>> = vec![
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
        Box::new(HttpRequestTool),
    ];
    if let Some(engine) = routine_engine {
        tools.push(Box::new(RoutineTool::new(engine)));
    }
    tools
}
