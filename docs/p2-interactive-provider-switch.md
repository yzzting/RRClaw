# P2: 交互式 Provider/Model 切换

## 问题

当前 `/provider` 和 `/model` 需要手动输入名称，容易出错。
只能切到已配置的 provider，如果没有 API key 就无法使用。
没有修改 API key 和 base_url 的入口。

## 方案

全部用 `dialoguer` 选项菜单，零手动输入。

### 数据源：每个 Provider 的已知模型列表

在 `src/config/setup.rs` 中维护一个 Provider → Models 映射：

```rust
pub const PROVIDERS: &[ProviderInfo] = &[
    ProviderInfo {
        name: "deepseek",
        base_url: "https://api.deepseek.com/v1",
        models: &["deepseek-chat", "deepseek-reasoner"],
    },
    ProviderInfo {
        name: "claude",
        base_url: "https://api.anthropic.com",
        models: &["claude-sonnet-4-5-20250929", "claude-haiku-4-5-20251001"],
        // auth_style: Some("x-api-key")
    },
    ProviderInfo {
        name: "glm",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        models: &["glm-4-flash", "glm-4-plus"],
    },
    // ...
];
```

### `/provider` 交互流程

```
rrclaw〉/provider
当前: deepseek (https://api.deepseek.com/v1)

选择 Provider:
> deepseek (已配置 ✓)
  claude (已配置 ✓)
  glm (未配置)
  minimax (未配置)
  gpt (未配置)

// 选择已配置的 → 直接切换
已切换到 claude (模型: claude-sonnet-4-5-20250929, URL: https://api.anthropic.com)

// 选择未配置的 → 引导输入
glm API Key: ****
Base URL [https://open.bigmodel.cn/api/paas/v4]: (直接回车用默认)
选择模型:
> glm-4-flash
  glm-4-plus
已配置并切换到 glm (模型: glm-4-flash)
```

### `/model` 交互流程（选项菜单）

```
rrclaw〉/model
当前模型: deepseek-chat (Provider: deepseek)

选择模型:
> deepseek-chat
  deepseek-reasoner

模型已切换为: deepseek-reasoner
```

列出当前 provider 的已知模型列表。如果用户想用列表中没有的模型，
菜单最后加一个"自定义..."选项，选中后才手动输入。

### `/apikey` 修改 API key 和 base_url

```
rrclaw〉/apikey
选择 Provider:
> deepseek (当前)
  claude

修改什么？
> API Key
  Base URL
  两者都改

deepseek API Key: ****
已更新。
```

修改后用 `toml_edit` 写入 config.toml，当前 session 立即生效（重建 Provider 实例）。

### 改动文件

| 文件 | 改动 |
|------|------|
| `src/config/setup.rs` | `PROVIDERS` 改为 `pub` 结构体数组，含 models 列表 |
| `src/channels/cli.rs` | `/provider`, `/model`, `/apikey` 全部改为 dialoguer 选项菜单 |
| `src/config/schema.rs` | （可能）增加 `ProviderConfig` 的 `toml_edit` 写入辅助方法 |

### 提交策略

1. `refactor: restructure PROVIDERS as pub ProviderInfo with models`
2. `feat: interactive /provider selection with auto-configure`
3. `feat: interactive /model selection menu`
4. `feat: add /apikey command for updating credentials and base_url`

### 验证

1. `/provider` → 弹出菜单，选已配置的直接切换，显示 base_url
2. `/provider` → 选未配置的，引导输入 API key + base_url + 选模型
3. `/model` → 弹出当前 provider 模型列表，选择切换
4. `/model` → 选"自定义..."，输入自定义模型名
5. `/apikey` → 选 provider，修改 API key 或 base_url，config.toml 更新
6. `cargo test` 全部通过
