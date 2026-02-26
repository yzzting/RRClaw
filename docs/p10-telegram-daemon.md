# P10 Daemon 模式

## 背景

`rrclaw agent` 目前绑定终端进程，关闭终端会同时终止 Telegram Bot。
用户期望 Telegram 后台持续运行，终端只是其中一个接入渠道。

## 目标

```bash
rrclaw start    # 启动后台 daemon（含 Telegram + IPC server）
rrclaw chat     # 连接 daemon，开始终端对话
rrclaw stop     # 停止 daemon
rrclaw restart  # 重启 daemon（stop + start）
rrclaw status   # 查看 daemon 运行状态
```

关闭终端 → Telegram 继续跑；再次打开终端 → `rrclaw chat` 重新接入。

## 架构

```
┌─────────────────────────────────────┐
│  rrclaw daemon（后台进程）            │
│                                     │
│  ┌─────────────┐  ┌───────────────┐ │
│  │ Agent (TG)  │  │ Agent (Chat1) │ │  ← 每个 channel 独立 Agent 实例
│  │ per chat_id │  │ per session   │ │
│  └──────┬──────┘  └──────┬────────┘ │
│         │                │          │
│  ┌──────▼────────────────▼────────┐ │
│  │     Shared: Memory (SQLite)    │ │  ← 共享记忆、配置、Skills
│  └────────────────────────────────┘ │
│                                     │
│  Unix socket: ~/.rrclaw/daemon.sock  │
└──────────────┬──────────────────────┘
               │ IPC
┌──────────────▼──────────────────────┐
│  rrclaw chat（前台 CLI 客户端）       │
│  连接 socket → 收发消息 → 终端展示   │
└─────────────────────────────────────┘
```

## Channel 隔离

**不同 channel 的对话互不冲突**，共享底层资源：

| 资源 | 隔离/共享 |
|------|---------|
| 对话历史（history） | **隔离**：每个 channel 独立 Agent 实例 |
| Telegram 每个 chat_id | **隔离**：每个 chat_id 独立 Agent 实例 |
| CLI session | **隔离**：每次 `rrclaw chat` 独立 Agent 实例 |
| Memory（SQLite） | **共享**：所有 channel 共用同一记忆库 |
| Config / Skills | **共享** |
| SecurityPolicy | **共享**（来自 config） |

## 文件路径

| 文件 | 用途 |
|------|------|
| `~/.rrclaw/daemon.pid` | daemon 进程 PID |
| `~/.rrclaw/daemon.sock` | Unix domain socket（IPC） |
| `~/.rrclaw/logs/daemon.log` | daemon 运行日志（追加） |

## 命令实现

### `rrclaw start`

1. 检查 `daemon.pid` 是否已存在且进程活着 → 若是，报"already running"
2. re-exec 自身为子进程（不带 `start`，带内部 flag `--daemon-worker`）
   - stdout/stderr → `daemon.log`
   - stdin → `/dev/null`
3. 子进程启动后：
   - 打开 Unix socket `daemon.sock`
   - 启动 Telegram Bot（若配置了）
   - 写 PID 到 `daemon.pid`
4. 父进程确认 socket 就绪后 exit(0)，终端立即返回

### `rrclaw chat`

1. 检查 `daemon.sock` 是否存在，不存在 → 提示"daemon not running, run `rrclaw start` first"
2. 连接 socket
3. 启动本地 REPL：用户输入 → 发送到 socket → 收到回复 → 显示

### `rrclaw stop`

1. 读 `daemon.pid`
2. 发 SIGTERM
3. 等待进程退出（最多 5s），超时则 SIGKILL
4. 删除 `daemon.pid` 和 `daemon.sock`

### `rrclaw restart`

`stop()` → `start()`

### `rrclaw status`

读 `daemon.pid` → `kill(pid, 0)` 探活：

```
● rrclaw daemon running (pid 12345, uptime 2h 34m)
  Telegram: enabled
  Active chat sessions: 2
```

## IPC 协议（Unix socket）

使用 JSON Lines（每行一个 JSON 对象），简单可靠：

```json
// client → daemon：发送消息
{"type": "message", "session_id": "cli-abc123", "content": "你好"}

// daemon → client：流式 token
{"type": "token", "content": "你"}
{"type": "token", "content": "好"}
{"type": "done"}

// daemon → client：工具确认请求（Supervised 模式）
{"type": "confirm", "request_id": "xxx", "tool": "shell", "args": {"command": "ls"}}

// client → daemon：确认响应
{"type": "confirm_response", "request_id": "xxx", "approved": true}
```

## 改动范围

### 新增

- `src/daemon/mod.rs` — daemon 管理（start/stop/status/pid/socket）
- `src/daemon/server.rs` — daemon server（socket listener + session 管理）
- `src/daemon/client.rs` — chat 客户端（socket client + REPL）
- `src/daemon/protocol.rs` — IPC 消息协议定义

### 修改

- `src/main.rs` — 新增 `start/chat/stop/restart/status` 子命令
- `src/agent/loop_.rs` — Agent 支持通过 channel 流式输出（替换直接打印）

### 保留

- `rrclaw agent` 子命令继续保留（单次模式 `-m` 仍有用）
- `rrclaw telegram` 子命令废弃（由 daemon 取代）

## 提交策略

```
1. docs: update p10 daemon design
2. feat(daemon): IPC protocol types
3. feat(daemon): daemon server (socket + session manager)
4. feat(daemon): daemon client (REPL over socket)
5. feat(daemon): start/stop/restart/status commands
6. feat(main): wire up new subcommands
7. test(daemon): integration tests
```

## 验证方式

```bash
rrclaw start
# → daemon started (pid 12345)

rrclaw status
# → running (pid 12345)

rrclaw chat
# → 终端可对话，关闭后 daemon 继续

# Telegram 发消息 → 收到回复（daemon 持续运行）

rrclaw restart
# → stopped → started

rrclaw stop
# → stopped
```

## 测试用例

### D1 Daemon 生命周期

| ID | 场景 | 预期 |
|----|------|------|
| D1-1 | `start` → `status` | status 显示 running + pid |
| D1-2 | `start` 后再次 `start` | 报 "already running"，不重复启动 |
| D1-3 | `stop` → `status` | status 显示 not running |
| D1-4 | `stop` 后再次 `stop` | 报 "not running"，不报错 |
| D1-5 | `restart` | 先 stop 后 start，pid 变化，服务恢复 |
| D1-6 | `start` 后 kill -9 daemon 进程 | `daemon.pid` 和 `daemon.sock` 为 stale；下次 `start` 能正常启动 |
| D1-7 | `status` 无 daemon | 输出 "not running" |

### D2 IPC 通信

| ID | 场景 | 预期 |
|----|------|------|
| D2-1 | `chat` 连接，发消息，收回复 | 完整收到 token 流 + done |
| D2-2 | `chat` 断开后重新连接 | 新 session，历史清空，daemon 继续运行 |
| D2-3 | 两个 `chat` 同时连接 | 各自独立会话，互不干扰 |
| D2-4 | `chat` 时 daemon 崩溃 | client 收到连接断开错误，提示重启 daemon |
| D2-5 | `chat` 在 daemon 未启动时运行 | 提示 "daemon not running, run `rrclaw start` first" |

### D3 Channel 隔离

| ID | 场景 | 预期 |
|----|------|------|
| D3-1 | CLI session A 发消息，session B 查历史 | B 看不到 A 的对话历史 |
| D3-2 | TG chat_id 1 发消息，chat_id 2 查历史 | 各自独立，互不可见 |
| D3-3 | CLI 和 TG 同时发消息 | 各自独立处理，不互相阻塞 |

### D4 Memory 共享

| ID | 场景 | 预期 |
|----|------|------|
| D4-1 | CLI 对话中 Agent 存入记忆 → TG 侧 recall | TG 能查到 CLI 存入的记忆 |
| D4-2 | TG 存入记忆 → 重启 daemon → CLI recall | 重启后记忆仍在（SQLite 持久化） |

### D5 Supervised 模式工具确认

| ID | 场景 | 预期 |
|----|------|------|
| D5-1 | CLI chat 中触发工具调用 | confirm 请求通过 IPC 推送到 CLI 客户端，等待用户 y/n |
| D5-2 | 用户输入 `y` | 工具执行，结果返回 |
| D5-3 | 用户输入 `n` | 工具取消，Agent 收到拒绝结果 |
| D5-4 | CLI 断开时有 pending confirm | daemon 自动取消该 confirm，不死锁 |

### D6 Telegram 持久性

| ID | 场景 | 预期 |
|----|------|------|
| D6-1 | `start` 后关闭终端 | TG Bot 继续响应消息 |
| D6-2 | 关闭终端再 `rrclaw chat` | 能重新连接，TG 侧无感知 |
| D6-3 | 无 `[telegram]` 配置时 `start` | daemon 正常启动，仅无 TG channel |

---

## 注意事项

- `rrclaw chat` 断开不影响 daemon，daemon 保留该 session 的 Agent 实例一段时间（可配置 TTL）
- Supervised 模式下工具确认需通过 IPC 协议转发给 CLI 客户端
- daemon 进程自身 panic 时 `daemon.pid` 和 `daemon.sock` 需清理（用 drop guard 或 signal handler）
- 首次实现可以简化：CLI session 断开时 Agent 实例直接销毁（不保留 TTL）
