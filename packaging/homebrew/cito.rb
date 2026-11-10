class Cito < Formula
  desc "Cross-harness agent and human notification network for macOS"
  homepage "https://tefx.github.io/cito/"
  url "https://github.com/Tefx/cito/releases/download/v0.21.0/Cito-macos-arm64.zip"
  version "0.21.0"
  sha256 "ee1c94676031b941e253846b40f10fe9ef8bcb256ccbe474dbbc4fd1d445d272"
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

  def caveats
    <<~EOS
      Cito.app has been installed to:
        #{opt_prefix}/Cito.app

      To launch the menu bar app (or install the cask `brew install --cask tefx/tap/cito` for /Applications & Spotlight):
        open "#{opt_prefix}/Cito.app"
    EOS
  end

  test do
    assert_match "cito v#{version}", shell_output("#{bin}/cito --version")
  end
end
