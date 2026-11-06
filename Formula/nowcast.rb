# The release workflow rewrites url and sha256 on every version tag.
class Nowcast < Formula
  desc "Local service streaming ready-to-use data to agents, dashboards and apps"
  homepage "https://github.com/phvn-orga/nowcast"
  url "https://github.com/phvn-orga/nowcast/releases/download/v0.1.0/nowcast-v0.1.0-x86_64-unknown-linux-musl.tar.gz"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  depends_on arch: :x86_64
  depends_on :linux

  def install
    bin.install "nowcast"
    (pkgshare/"systemd").install "systemd/nowcast.socket", "systemd/nowcast.service"
    # The repository's unit runs the binary from ~/.local/bin, with a comment
    # saying so. This one runs it from the opt path, the same across upgrades.
    inreplace pkgshare/"systemd/nowcast.service",
              /^(#.*\n)*ExecStart=.*$/,
              "ExecStart=#{opt_bin}/nowcast daemon"
  end

  def caveats
    <<~EOS
      To let systemd start the service on the first connection:
        mkdir -p ~/.config/systemd/user
        ln -sf #{opt_pkgshare}/systemd/nowcast.socket #{opt_pkgshare}/systemd/nowcast.service ~/.config/systemd/user/
        systemctl --user daemon-reload
        systemctl --user enable --now nowcast.socket
    EOS
  end

  test do
    assert_equal "nowcast #{version}", shell_output("#{bin}/nowcast --version").strip
  end
end
