class Cito < Formula
  desc "Cross-harness agent and human notification network for macOS"
  homepage "https://tefx.github.io/cito/"
  url "https://github.com/Tefx/cito/releases/download/v0.19.0/Cito-macos-arm64.zip"
  version "0.19.0"
  sha256 "4a0f0ab818f9debbc158a88f1dd2dc249ff10c1a83a9c257f352cf521895c468"
  license "MIT"

  depends_on arch: :arm64
  depends_on :macos

  def install
    if File.directory?("Cito.app")
      prefix.install "Cito.app"
    elsif File.directory?("Contents")
      (prefix/"Cito.app").install "Contents"
    else
      odie "Cito.app bundle Contents not found in archive"
    end

    (bin/"cito").write <<~EOS
      #!/bin/sh
      export CITO_BUILTIN_PLUGINS_DIR="#{opt_prefix}/Cito.app/Contents/Resources/plugins"
      exec "#{opt_prefix}/Cito.app/Contents/Helpers/cito-native" "$@"
    EOS
    (bin/"cito-mcp").write <<~EOS
      #!/bin/sh
      export CITO_BUILTIN_PLUGINS_DIR="#{opt_prefix}/Cito.app/Contents/Resources/plugins"
      export CITO_ENDPOINT_PID="${CITO_ENDPOINT_PID:-$PPID}"
      if [ "$#" -eq 0 ]; then
        exec "#{opt_prefix}/Cito.app/Contents/Helpers/cito-native" mcp-stdio
      fi
      exec "#{opt_prefix}/Cito.app/Contents/Helpers/cito-native" "$@"
    EOS
    chmod 0755, [bin/"cito", bin/"cito-mcp"]
  end

  def post_install
    user_apps = Pathname.new(Dir.home)/"Applications"
    user_apps.mkpath
    target = user_apps/"Cito.app"
    if target.symlink? || !target.exist?
      FileUtils.rm_f(target)
      FileUtils.ln_s(opt_prefix/"Cito.app", target)
    end
    system "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister",
           "-f", (opt_prefix/"Cito.app").to_s
  rescue StandardError
    nil
  end

  def caveats
    <<~EOS
      Cito.app has been installed to:
        #{opt_prefix}/Cito.app
        ~/Applications/Cito.app (available in Spotlight)

      To launch the menu bar app:
        open -a Cito
    EOS
  end

  test do
    assert_match "cito v#{version}", shell_output("#{bin}/cito --version")
  end
end
