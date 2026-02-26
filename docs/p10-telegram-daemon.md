# P10 Telegram Daemon Mode

## 背景

`rrclaw telegram` 目前是前台阻塞进程，终端被占用无法继续使用。
用户期望运行后终端立即返回，Bot 在后台持续运行。

## 目标

```bash
rrclaw telegram --detach    # 启动后台 Bot，终端立即返回
rrclaw telegram --stop      # 停止后台 Bot
rrclaw telegram --status    # 查看 Bot 运行状态
```

## 设计方案

### 方案选择

| 方案 | 原理 | 优劣 |
|------|------|------|
| A: `fork()` + `setsid()` | Unix 传统 daemon | 与 tokio 运行时冲突，不推荐 |
| B: re-exec 子进程 | 重新执行自身，不带 `--detach` | 干净、跨平台、无 async 问题 ✅ |
| C: `daemonize` crate | 封装 fork | 依赖重，同方案 A 的问题 |

**选方案 B（re-exec）**：

```
rrclaw telegram --detach
  └─ 检测到 --detach
  └─ 用 std::process::Command 重新启动自身（不带 --detach）
       stdout/stderr → ~/.rrclaw/logs/telegram.log
       stdin → /dev/null
  └─ 写 PID 到 ~/.rrclaw/telegram.pid
  └─ 父进程 exit(0)，终端立即返回
```

### 文件路径

| 文件 | 用途 |
|------|------|
| `~/.rrclaw/telegram.pid` | 存储子进程 PID |
| `~/.rrclaw/logs/telegram.log` | Bot 运行日志（追加模式） |

### --stop 实现

1. 读 `telegram.pid`
2. `kill(pid, SIGTERM)`
3. 删除 `telegram.pid`

### --status 实现

1. 读 `telegram.pid`，文件不存在 → 未运行
2. `kill(pid, 0)`（探活信号）→ 进程存在则运行中，否则 stale PID

## 改动范围

### 新增

- `src/main.rs` — `telegram` 子命令新增 `--detach` / `--stop` / `--status` flag
- `src/channels/daemon.rs` — daemon 工具函数：`detach()`, `stop()`, `status()`, `pid_path()`, `log_path()`

### 修改

- `src/main.rs` — `telegram` 子命令处理逻辑分支
- `src/channels/mod.rs` — 导出 daemon 模块（如果放这里）

## 提交策略

```
1. docs: add p10-telegram-daemon.md         ← 本文件
2. feat(daemon): add daemon utility module  ← src/channels/daemon.rs
3. feat(cli): add --detach/--stop/--status  ← src/main.rs
4. test(daemon): add unit tests             ← tests/
```

## 验证方式

```bash
rrclaw telegram --detach
# 终端立即返回，$? == 0

rrclaw telegram --status
# 输出: running (pid 12345)

cat ~/.rrclaw/logs/telegram.log
# 有启动日志

rrclaw telegram --stop
# 输出: stopped

rrclaw telegram --status
# 输出: not running
```

## 注意事项

- re-exec 需要能找到当前可执行文件路径：`std::env::current_exe()`
- 日志文件用 `OpenOptions::new().create(true).append(true)` 打开，不覆盖旧日志
- PID 文件在进程正常退出时应自动清理（注册 `ctrlc` 或在 telegram loop 结束后删除）
- Windows 不支持 `kill(pid, 0)`，但项目目前只面向 macOS/Linux，暂不考虑
