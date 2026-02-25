class Rrclaw < Formula
  desc "安全优先的 AI 助手 CLI，100% Rust，Trait 可插拔架构"
  homepage "https://github.com/yzzting/rrclaw"
  license "MIT"
  version "0.0.2"

  on_macos do
    on_arm do
      url "https://github.com/yzzting/rrclaw/releases/download/v0.0.2/rrclaw-macos-aarch64.tar.gz"
      sha256 "ecd5c29d0ab9013708b702beb820839365762c432c08396a03c646d74d940294"
    end
    on_intel do
      url "https://github.com/yzzting/rrclaw/releases/download/v0.0.2/rrclaw-macos-x86_64.tar.gz"
      sha256 "71c1ad17a75dff6a013f30beb5e7bbce495f34bde29c83c160a3dc82567de7f9"
    end
  end

  on_linux do
    url "https://github.com/yzzting/rrclaw/releases/download/v0.0.2/rrclaw-linux-x86_64.tar.gz"
    sha256 "6cb6da4aa4106031f39a97e4766169e9511a0ffd21fd7af6eea4d524da8ec9cd"
  end

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

  test do
    system "#{bin}/rrclaw", "--help"
  end
end
