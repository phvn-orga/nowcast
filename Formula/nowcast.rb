# The release workflow points url and sha256 at each release it publishes.
class Nowcast < Formula
  desc "Local service streaming ready-to-use data to agents, dashboards and apps"
  homepage "https://github.com/phvn-orga/nowcast"
  url "https://github.com/phvn-orga/nowcast/releases/download/v0.1.0-alpha.4/nowcast-v0.1.0-alpha.4-x86_64-unknown-linux-musl.tar.gz"
  sha256 "2be1206d35c7631c84c54ef79f2128ec32a2db4b6e86c9d7e1375f8871bcea4c"

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

  # The socket is enabled by a link through ~/.config/systemd/user, not by
  # `systemctl enable`, which would record the versioned Cellar path behind it.
  def caveats
    warning = if version.to_s.include?("-")
      "nowcast #{version} is an experimental pre-release, for testing only: " \
        "interfaces and behaviour change without notice.\n\n"
    end
    warning.to_s + <<~EOS
      To let systemd start the service on the first connection:
        mkdir -p ~/.config/systemd/user/sockets.target.wants
        ln -sf #{opt_pkgshare}/systemd/nowcast.socket #{opt_pkgshare}/systemd/nowcast.service ~/.config/systemd/user/
        ln -sfn ../nowcast.socket ~/.config/systemd/user/sockets.target.wants/nowcast.socket
        systemctl --user daemon-reload
        systemctl --user start nowcast.socket
    EOS
  end

  test do
    assert_equal "nowcast #{version}", shell_output("#{bin}/nowcast --version").strip
  end
end
