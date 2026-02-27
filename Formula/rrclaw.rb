class Rrclaw < Formula
  desc "安全优先的 AI 助手 CLI，100% Rust，Trait 可插拔架构"
  homepage "https://github.com/yzzting/rrclaw"
  license "MIT"
  version "0.0.3"

  on_macos do
    on_arm do
      url "https://github.com/yzzting/rrclaw/releases/download/v0.0.3/rrclaw-macos-aarch64.tar.gz"
      sha256 "b1375021fc24d1c5e7a9d9ff74fe284e2fe268b3185c8a5428206fb4d72521d5"
    end
    on_intel do
      url "https://github.com/yzzting/rrclaw/releases/download/v0.0.3/rrclaw-macos-x86_64.tar.gz"
      sha256 "624f731add6c19b98782add9d5fd2519ecccb1d1f256cdcd704580ab0abc2dd1"
    end
  end

  on_linux do
    url "https://github.com/yzzting/rrclaw/releases/download/v0.0.3/rrclaw-linux-x86_64.tar.gz"
    sha256 "7396cceafc43e63f39250723c157d80de2208c1fbae95dcee4a5f17b584c4652"
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
