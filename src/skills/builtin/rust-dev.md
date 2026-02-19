---
name: rust-dev
description: Rust 开发辅助。代码生成、错误处理模式、性能优化、cargo 命令指导。当用户进行 Rust 开发时使用。
tags: [dev, rust]
---

# Rust 开发辅助

## 代码规范
- 使用 `thiserror` 定义错误类型，`color_eyre` 处理顶层错误
- 异步代码使用 `tokio`，async trait 用 `async_trait`
- 序列化使用 `serde` + `serde_json`
- 优先用 `&str` 而非 `String`，减少不必要的 clone
- 公开 API 返回 `Result<T>`，内部函数可用 `Option<T>`

## 工作流程
1. 用 file_read 阅读相关代码了解上下文和现有风格
2. 生成代码时遵循项目现有约定
3. 用 shell 运行 `cargo check` 快速验证编译
4. 用 shell 运行 `cargo test` 验证测试
5. 用 shell 运行 `cargo clippy -- -W clippy::all` 消除 lint 警告
6. 确认 `cargo build --release` 通过

## 常用命令
- `cargo check` — 快速编译检查（不生成二进制）
- `cargo test` — 运行所有测试
- `cargo test <name>` — 运行特定测试
- `cargo clippy -- -W clippy::all` — lint 检查
- `cargo build --release` — 发布构建
- `cargo doc --open` — 生成并查看文档
- `cargo add <crate>` — 添加依赖

## 注意事项
- 不要引入不必要的新 crate，优先用标准库
- 测试放在 `#[cfg(test)] mod tests` 块内
- 涉及文件系统的测试用 `tempfile::tempdir()` 创建临时目录
