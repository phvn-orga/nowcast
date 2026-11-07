# The release workflow points url and sha256 at each release it publishes.
class Nowcast < Formula
  desc "Local service streaming ready-to-use data to agents, dashboards and apps"
  homepage "https://github.com/phvn-orga/nowcast"
  url "https://github.com/phvn-orga/nowcast/releases/download/v0.1.0-alpha.2/nowcast-v0.1.0-alpha.2-x86_64-unknown-linux-musl.tar.gz"
  sha256 "d14d2a384a223e0f4f5c828afb173660a9283d971f38eaf7ed6cb483064a26d8"

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
    warning = if version.to_s.include?("-")
      "nowcast #{version} is an experimental pre-release, for testing only: " \
        "interfaces and behaviour change without notice.\n\n"
    end
    warning.to_s + <<~EOS
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
