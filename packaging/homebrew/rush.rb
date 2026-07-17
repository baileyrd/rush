# Homebrew formula for rush.
#
# rush isn't in homebrew-core (an experimental project, not yet a
# homebrew-core-eligible stable daily-driver — see README's own "Status:
# experimental" note), so this lives as a formula for a personal tap
# instead:
#
#   brew tap baileyrd/rush https://github.com/baileyrd/rush
#   brew install rush
#
# (a tap repo just needs this file under Formula/rush.rb — copy or
# symlink it there; kept here under packaging/ so it ships with the main
# rush repo instead of needing a second one to stay in sync.)
#
# To cut a new formula version after tagging vX.Y.Z:
#   url    = the tag's source tarball (below)
#   sha256 = `curl -sL <url> | shasum -a 256`
class Rush < Formula
  desc "Small, bash-compatible shell written in Rust"
  homepage "https://github.com/baileyrd/rush"
  url "https://github.com/baileyrd/rush/archive/refs/tags/v0.1.1.tar.gz"
  sha256 "" # fill in via `curl -sL <url> | shasum -a 256` after tagging
  license "MIT"
  head "https://github.com/baileyrd/rush.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
    man1.install "docs/rush.1"
    bash_completion.install "completions/rush.bash" => "rush"
    zsh_completion.install "completions/rush.zsh" => "_rush"
  end

  test do
    assert_match "hello", shell_output("#{bin}/rush -c 'echo hello'")
    assert_match version.to_s, shell_output("#{bin}/rush -c 'echo $RUSH_VERSION'")
  end
end
