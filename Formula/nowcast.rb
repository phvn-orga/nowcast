# The release workflow points url and sha256 at each release it publishes.
class Nowcast < Formula
  desc "Local service streaming ready-to-use data to agents, dashboards and apps"
  homepage "https://github.com/phvn-orga/nowcast"
  url "https://github.com/phvn-orga/nowcast/releases/download/v0.1.0-alpha.7/nowcast-v0.1.0-alpha.7-x86_64-unknown-linux-musl.tar.gz"
  sha256 "6d012e1586005b6aab4799d4a80fbd461b31e66ff9505ff2e73a79f6bec31a05"

  depends_on arch: :x86_64
  depends_on :linux

  def install
    bin.install "nowcast"
  end

  # `nowcast setup` writes the systemd units, the service running from the opt
  # path. After an upgrade that changes them, `nowcast status` says to run it
  # again.
  def caveats
    warning = if version.to_s.include?("-")
      "nowcast #{version} is an experimental pre-release, for testing only: " \
        "interfaces and behaviour change without notice.\n\n"
    end
    warning.to_s + <<~EOS
      To let systemd start the service on the first connection, and choose its
      bricks:
        nowcast setup
      `nowcast status` says when an upgrade needs it again. Before `brew uninstall`:
        nowcast setup --remove
    EOS
  end

  test do
    assert_equal "nowcast #{version}", shell_output("#{bin}/nowcast --version").strip
  end
end
