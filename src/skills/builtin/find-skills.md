---
name: find-skills
description: 搜索和添加 AI Agent 技能。当用户想添加新技能、搜索技能市场、或查找特定功能的技能时使用。
tags: [skill, registry, search, add]
---

# find-skills 技能

当用户想要搜索或添加新的 AI Agent 技能时使用此技能。

## 使用场景

1. **用户想添加新技能**：例如 "帮我添加一个 xxx 技能"
2. **用户想搜索技能**：例如 "有什么技能可以 xxx"
3. **用户想查看技能列表**：例如 "列出所有可用的技能"

## 操作步骤

### 1. 搜索技能

使用 `npx skills find <关键词>` 搜索技能市场。

示例：
```bash
npx skills find "react"
npx skills find "database"
```

### 2. 添加技能

使用 `npx skills add <仓库URL> --skill <技能名>` 添加技能。

常用技能仓库：
- `https://github.com/vercel-labs/skills` — Vercel 官方技能库
- `https://github.com/bolt-sdk/awesome-bolt-skills` — Bolt 技能集

示例：
```bash
# 从 vercel-labs 添加 find-skills 技能
npx skills add https://github.com/vercel-labs/skills --skill find-skills

# 添加其他技能
npx skills add https://github.com/vercel-labs/skills --skill <技能名>
```

### 3. 查看已安装技能

```bash
npx skills list
```

### 4. 删除技能

```bash
npx skills remove <技能名>
```

## 注意事项

- 添加技能前确保已安装 Node.js 和 npm
- 技能会被安装到用户目录下的 `.skills` 目录
- 部分技能可能需要配置环境变量或 API Key
