class Whisker < Formula
  desc "Terminal coding agent for OpenAI-compatible models"
  homepage "https://github.com/bugeng777/whisker"
  version "0.6.1"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/bugeng777/whisker/releases/download/v0.6.1/whisker-aarch64-apple-darwin.tar.gz"
      sha256 "d1272a3fd15339610df3e7c6d81f99d0296c135b5f39142f320a87de54cdf2b8"
    end
    on_intel do
      url "https://github.com/bugeng777/whisker/releases/download/v0.6.1/whisker-x86_64-apple-darwin.tar.gz"
      sha256 "32e03d9daa8909a9d296e4f89914c9d6d07d899a12fcbcd3fe116f29ff41e8b8"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/bugeng777/whisker/releases/download/v0.6.1/whisker-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "5863e266dea1e73fe5daadad143f224563fd0a35340b54afefab60865bac9e87"
    end
    on_intel do
      url "https://github.com/bugeng777/whisker/releases/download/v0.6.1/whisker-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "d295bb0583a9710e42d2daa06cf284c9ccf28692ac80a45e48db3e3e89b70ea4"
    end
  end

  def install
    bin.install "whisker"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/whisker --version")
  end
end
