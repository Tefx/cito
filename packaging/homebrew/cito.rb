class Cito < Formula
  desc "Cross-harness agent and human notification network for macOS"
  homepage "https://tefx.github.io/cito/"
  url "https://github.com/Tefx/cito/releases/download/v0.19.0/Cito-macos-arm64.zip"
  version "0.19.0"
  sha256 "f52a8047d71aecbc17f31b6d5d7641ae1dca155bb6faa6c09a70bc8ab73a7398"
  license "MIT"

  depends_on arch: :arm64
  depends_on :macos

  def install
    prefix.install "Cito.app" if File.directory?("Cito.app")

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

      To launch the menu bar app:
        open "#{opt_prefix}/Cito.app"
    EOS
  end

  test do
    assert_match "cito v#{version}", shell_output("#{bin}/cito --version")
  end
end
