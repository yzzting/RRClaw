# P2: 交互式 Provider 切换

## 问题

当前 `/provider <name>` 和 `/model <name>` 需要手动输入名称，容易出错。
且只能切到已配置的 provider，如果没有 API key 就无法使用。

## 方案

用 `dialoguer` 做交互式菜单选择，替代手动输名称。

### `/provider` 交互流程

```
rrclaw〉/provider
当前 Provider: deepseek

选择 Provider:
> deepseek (已配置 ✓)
  claude (已配置 ✓)
  glm (未配置)
  minimax (未配置)
  gpt (未配置)

// 选择已配置的 → 直接切换
已切换到 claude (模型: claude-sonnet-4-5-20250929, URL: https://api.anthropic.com)

// 选择未配置的 → 引导输入 API key
glm API Key: ****
Base URL [https://open.bigmodel.cn/api/paas/v4]:
默认模型 [glm-4-flash]:
已配置并切换到 glm
```

### `/model` 交互流程

```
rrclaw〉/model
当前模型: deepseek-chat (Provider: deepseek)

输入模型名 (当前 Provider: deepseek): deepseek-reasoner
模型已切换为: deepseek-reasoner
```

仅切同 Provider 下的模型，不做菜单（模型太多列不完）。
要切 Provider 用 `/provider`。

### `/apikey` 修改已有 Provider 的 API key

```
rrclaw〉/apikey
选择 Provider:
> deepseek
  claude
deepseek API Key: ****
已更新 deepseek 的 API Key。
```

修改后写入 config.toml（用 `toml_edit` 保留格式），当前 session 如果是该 provider 也立即生效。

### 改动文件

| 文件 | 改动 |
|------|------|
| `src/channels/cli.rs` | `/provider` 改为交互式菜单，新增 `/apikey`，`/model` 简化 |
| `src/config/setup.rs` | 提取 `PROVIDERS` 常量为 `pub`，供 cli.rs 复用 |

### 提交策略

1. `refactor: export PROVIDERS constant from config/setup`
2. `feat: interactive /provider selection with auto-configure`
3. `feat: add /apikey command for updating provider credentials`

### 验证

1. `/provider` → 弹出选择菜单，选已配置的直接切换
2. `/provider` → 选未配置的，引导输入 API key，配置并切换
3. `/apikey` → 选 provider，输入新 key，检查 config.toml 已更新
4. `/model` → 输入模型名切换
5. `cargo test` 全部通过
