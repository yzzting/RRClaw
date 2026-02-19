---
name: git-commit
description: Git 提交规范。生成规范的 commit message，检查暂存区，执行原子化提交。当用户要求提交代码时使用。
tags: [dev, git]
---

# Git 提交规范

## 提交流程
1. 用 shell 运行 `git status` 查看当前状态
2. 用 shell 运行 `git diff --cached` 查看已暂存的变更内容
3. 分析变更，生成符合规范的 commit message
4. 用 shell 执行提交

## Commit Message 格式
```
<type>: <简短描述（英文，不超过 72 字符）>
```

type 取值：
- `feat` — 新功能
- `fix` — Bug 修复
- `docs` — 文档变更
- `test` — 测试相关
- `refactor` — 重构（不改变外部行为）
- `chore` — 构建/依赖/配置变更

## 原则
- 每个 commit 只做一件事（原子化）
- 描述 **为什么** 而不只是 **做了什么**
- 如果暂存区有多种不相关的改动，建议拆分成多个 commit
- 不要使用 `git add .` 或 `git add -A`，应精确指定文件

## 注意事项
- 不要在未确认的情况下 force push
- 不要修改已发布的 commit（--amend published commits）
- 提交前确认没有将 .env 或包含密钥的文件加入暂存区
