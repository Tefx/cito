class Cito < Formula
  desc "Cross-harness agent and human notification network for macOS"
  homepage "https://tefx.github.io/cito/"
  url "https://github.com/Tefx/cito/releases/download/v0.20.1/Cito-macos-arm64.zip"
  version "0.20.1"
  sha256 "babec6e0a9ac119dfe70b3db8c4fe96857773493863aac08d3d3833ebac62d14"
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
