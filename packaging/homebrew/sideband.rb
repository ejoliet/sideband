# Homebrew formula for the brew-first install route.
#
# This copy is the template. The published one lives in the tap repository
# (ejoliet/homebrew-tap), where the release workflow updates `version` and the
# four `sha256` lines from SHA256SUMS. Keep them in sync by editing here first.
class Sideband < Formula
  desc "Unlimited, end-to-end encrypted file transfer between teammates, brokered by Slack"
  homepage "https://github.com/ejoliet/sideband"
  version "0.1.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/ejoliet/sideband/releases/download/v#{version}/sideband-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/ejoliet/sideband/releases/download/v#{version}/sideband-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/ejoliet/sideband/releases/download/v#{version}/sideband-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  def install
    bin.install "sideband"
  end

  def caveats
    <<~EOS
      sideband needs a Slack bot token in the environment:
        export SLACK_BOT_TOKEN=xoxb-...
      `sideband bot` also needs SLACK_APP_TOKEN. Check your setup with:
        sideband doctor
    EOS
  end

  test do
    assert_match "sideband", shell_output("#{bin}/sideband --version")
  end
end
