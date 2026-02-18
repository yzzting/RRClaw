# P2: 合并为单个 /switch 命令

## 问题

`/provider` `/model` `/apikey` 三个命令太分散，合并为一个 `/switch` 一站式完成。

## 方案

### `/switch` 交互流程

```
rrclaw〉/switch

① 选择 Provider:
> deepseek (当前 ✓)
  claude (已配置)
  glm
  minimax
  gpt

② 选择模型:                    ← 选了 provider 后立刻选模型
> deepseek-chat (当前 ✓)
  deepseek-reasoner
  自定义...

// 如果选的 provider 未配置 → 插入 API Key 输入
③ API Key: ****
   Base URL [https://api.deepseek.com/v1]:

已切换到 deepseek / deepseek-reasoner
```

**核心逻辑**：
1. 选 Provider（标记当前 + 已配置）
2. 选模型（该 provider 的已知模型列表 + 自定义）
3. 如果未配置 → 输入 API Key + Base URL → 写入 config.toml
4. 切换完成

如果选的 provider 和 model 跟当前一样 → 提示"无变化"。

### 删除的命令

- `/provider` → 删除，合并到 `/switch`
- `/model` → 删除，合并到 `/switch`
- `/apikey` → 保留（独立功能，改已有凭据不需要切换）

### 最终命令列表

| 命令 | 功能 |
|------|------|
| `/help` | 帮助 |
| `/new` | 新建对话 |
| `/clear` | 清屏 |
| `/config` | 显示当前配置 |
| `/switch` | 切换 Provider + 模型（一站式） |
| `/apikey` | 修改已有 Provider 的 API Key / Base URL |

### 改动文件

| 文件 | 改动 |
|------|------|
| `src/channels/cli.rs` | 删除 `cmd_provider`/`cmd_model`，新增 `cmd_switch` |

### 提交

1. `feat: replace /provider and /model with unified /switch command`

### 验证

1. `/switch` → 选 provider → 选 model → 切换成功
2. `/switch` → 选未配置的 provider → 输入 API Key + Base URL → 切换成功
3. `/switch` → 选当前 provider 和 model → 提示无变化
