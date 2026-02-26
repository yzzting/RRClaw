# Release Guide

发布 RRClaw 新版本的完整流程。

---

## 前置条件（一次性配置）

### 1. HOMEBREW_TAP_TOKEN（已完成？）

在 GitHub 创建 Fine-grained PAT，权限：`yzzting/homebrew-rrclaw` → Contents: Read & Write。

添加到主仓库 Secret：
**GitHub → yzzting/RRClaw → Settings → Secrets and variables → Actions**
→ New repository secret → Name: `HOMEBREW_TAP_TOKEN`

配置好后，每次发布 Release 时 CI 会自动更新 `homebrew-rrclaw` 的 SHA256，无需手动操作。

### 2. crates.io token（发布到 crates.io 时需要）

```bash
cargo login   # 从 https://crates.io/me 获取 token
```

---

## 发布流程

### 第一步：确认代码状态

```bash
# 确保测试全通过
cargo test --features telegram

# 确保 clippy 零警告
cargo clippy --features telegram -- -D warnings

# 确保两种 feature 组合都能编译
cargo check
cargo check --features telegram
```

### 第二步：更新版本号

修改 `Cargo.toml`：

```toml
version = "0.x.x"   # 改为新版本
```

提交：

```bash
git add Cargo.toml
git commit -m "chore: bump version to 0.x.x"
git push
```

### 第三步：打 tag 并推送

```bash
git tag v0.x.x
git push origin v0.x.x
```

推送 tag 后，GitHub Actions 自动触发：
- **release.yml** — 构建 4 个平台二进制，创建 GitHub Release，上传 tar.gz
- **update-homebrew.yml** — 下载二进制计算 SHA256，更新 `homebrew-rrclaw` Formula，推送

等 CI 全部跑完（约 10-15 分钟），即可用 `brew install rrclaw` 安装新版本。

### 第四步（可选）：发布到 crates.io

```bash
# 预检，确认包内容正确
cargo publish --dry-run --features telegram

# 正式发布（默认不含 telegram）
cargo publish

# 注：crates.io 不支持带 optional dep 的 --features 发布，
# 用户用 cargo install rrclaw --features telegram 安装完整版
```

---

## CI 自动化说明

| Workflow | 触发条件 | 做什么 |
|----------|----------|--------|
| `ci.yml` | push / PR | 跑测试、clippy、两种 feature 编译验证 |
| `release.yml` | push tag `v*` | 构建 4 平台二进制，创建 GitHub Release |
| `update-homebrew.yml` | Release published | 计算 SHA256，更新 homebrew-rrclaw Formula |

**4 个构建平台：**
- `rrclaw-macos-aarch64.tar.gz` — macOS Apple Silicon
- `rrclaw-macos-x86_64.tar.gz` — macOS Intel
- `rrclaw-linux-x86_64.tar.gz` — Linux x86_64
- `rrclaw-linux-aarch64.tar.gz` — Linux ARM64（如有）

---

## Formula 注意事项

压缩包内文件名带平台后缀（如 `rrclaw-macos-aarch64`），Formula 安装时需重命名：

```ruby
def install
  on_macos do
    on_arm do
      bin.install "rrclaw-macos-aarch64" => "rrclaw"
    end
    on_intel do
      bin.install "rrclaw-macos-x86_64" => "rrclaw"
    end
  end
  on_linux do
    bin.install "rrclaw-linux-x86_64" => "rrclaw"
  end
end
```

`update-homebrew.yml` 的 sed 只更新 version/URL/SHA256，**install 块不会被覆盖**，无需每次发布手动修改。

---

## 手动修复（CI 未跑时）

如果 `update-homebrew.yml` 失败（通常是 `HOMEBREW_TAP_TOKEN` 未配置），手动更新步骤：

```bash
# 1. 下载各平台二进制，计算 SHA256
cd /tmp
curl -sL -o arm64.tar.gz   "https://github.com/yzzting/rrclaw/releases/download/v0.x.x/rrclaw-macos-aarch64.tar.gz"
curl -sL -o x86_64.tar.gz  "https://github.com/yzzting/rrclaw/releases/download/v0.x.x/rrclaw-macos-x86_64.tar.gz"
curl -sL -o linux.tar.gz   "https://github.com/yzzting/rrclaw/releases/download/v0.x.x/rrclaw-linux-x86_64.tar.gz"
shasum -a 256 arm64.tar.gz x86_64.tar.gz linux.tar.gz

# 2. 手动更新 homebrew-rrclaw/Formula/rrclaw.rb 中的 version、url、sha256

# 3. 推送
cd /path/to/homebrew-rrclaw
git add Formula/rrclaw.rb
git commit -m "chore: bump rrclaw to 0.x.x"
git push
```

---

## 用户安装命令（供 README 参考）

```bash
# Homebrew
brew tap yzzting/rrclaw
brew install rrclaw

# crates.io（核心版）
cargo install rrclaw

# crates.io（含 Telegram）
cargo install rrclaw --features telegram
```
