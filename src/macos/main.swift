import AppKit
import Darwin

// Headless tests use a real private pasteboard without changing the user clipboard.
let usesPrivatePasteboard = ProcessInfo.processInfo.environment["CITO_UI_HEADLESS"] == "1"
let appPasteboard = usesPrivatePasteboard ? NSPasteboard.withUniqueName() : NSPasteboard.general
atexit { if usesPrivatePasteboard { appPasteboard.releaseGlobally() } }

let app = NSApplication.shared
let companion = Companion()
app.delegate = companion
app.setActivationPolicy(.accessory)
companion.start()
app.run()
