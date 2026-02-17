pub mod config;
pub mod file;
pub mod shell;
pub mod traits;

pub use traits::{Tool, ToolResult};

use config::ConfigTool;
use file::{FileReadTool, FileWriteTool};
use shell::ShellTool;

/// 创建所有工具实例
pub fn create_tools() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(ShellTool),
        Box::new(FileReadTool),
        Box::new(FileWriteTool),
        Box::new(ConfigTool),
    ]
}
