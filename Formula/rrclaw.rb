class Rrclaw < Formula
  desc "安全优先的 AI 助手 CLI，100% Rust，Trait 可插拔架构"
  homepage "https://github.com/yzzting/rrclaw"
  license "MIT"
  version "0.1.0"

  on_macos do
    on_arm do
      url "https://github.com/yzzting/rrclaw/releases/download/v0.1.0/rrclaw-macos-aarch64.tar.gz"
      sha256 "PLACEHOLDER_MACOS_ARM64_SHA256"
    end
    on_intel do
      url "https://github.com/yzzting/rrclaw/releases/download/v0.1.0/rrclaw-macos-x86_64.tar.gz"
      sha256 "PLACEHOLDER_MACOS_X86_64_SHA256"
    end
  end

  on_linux do
    url "https://github.com/yzzting/rrclaw/releases/download/v0.1.0/rrclaw-linux-x86_64.tar.gz"
    sha256 "PLACEHOLDER_LINUX_X86_64_SHA256"
  end

  def install
    bin.install "rrclaw"
  end

  test do
    system "#{bin}/rrclaw", "--help"
  end
end
