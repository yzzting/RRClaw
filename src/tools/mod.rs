pub mod file;
pub mod shell;
pub mod traits;

pub use traits::{Tool, ToolResult};

use file::{FileReadTool, FileWriteTool};
use shell::ShellTool;

/// 创建所有 MVP 工具实例
pub fn create_tools() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(ShellTool),
        Box::new(FileReadTool),
        Box::new(FileWriteTool),
    ]
}
