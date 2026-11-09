# The release workflow points url and sha256 at each release it publishes.
class Nowcast < Formula
  desc "Local service streaming ready-to-use data to agents, dashboards and apps"
  homepage "https://github.com/phvn-orga/nowcast"
  url "https://github.com/phvn-orga/nowcast/releases/download/v0.1.0-alpha.5/nowcast-v0.1.0-alpha.5-x86_64-unknown-linux-musl.tar.gz"
  sha256 "012d58d4f3ddb5326f7e4c9b24a4d86bddf89adde514dd2129345e28c5b57525"

  depends_on arch: :x86_64
  depends_on :linux

  def install
    bin.install "nowcast"
  end

  # `nowcast setup` writes the systemd units, the service running from the opt
  # path. Run again after an upgrade, it rewrites a unit that changed.
  def caveats
    warning = if version.to_s.include?("-")
      "nowcast #{version} is an experimental pre-release, for testing only: " \
        "interfaces and behaviour change without notice.\n\n"
    end
    warning.to_s + <<~EOS
      To let systemd start the service on the first connection, and choose its
      bricks, after this install and after each upgrade:
        nowcast setup
      Before `brew uninstall`:
        nowcast setup --remove
    EOS
  end

  test do
    assert_equal "nowcast #{version}", shell_output("#{bin}/nowcast --version").strip
  end
end
