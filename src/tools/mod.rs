pub mod config;
pub mod file;
pub mod self_info;
pub mod shell;
pub mod traits;

pub use traits::{Tool, ToolResult};

use std::path::PathBuf;

use crate::config::Config;
use config::ConfigTool;
use file::{FileReadTool, FileWriteTool};
use self_info::SelfInfoTool;
use shell::ShellTool;

/// 创建所有工具实例
pub fn create_tools(
    app_config: Config,
    data_dir: PathBuf,
    log_dir: PathBuf,
    config_path: PathBuf,
) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(ShellTool),
        Box::new(FileReadTool),
        Box::new(FileWriteTool),
        Box::new(ConfigTool),
        Box::new(SelfInfoTool::new(app_config, data_dir, log_dir, config_path)),
    ]
}
