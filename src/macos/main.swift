import AppKit
import Darwin

let invocation = URL(fileURLWithPath: CommandLine.arguments[0])
let executable = invocation.resolvingSymlinksInPath()
// The flat helper alias is retained for existing daemon and release consumers.
// AppKit locates the app bundle from the executable path, not the symlink target.
if invocation.path.hasSuffix("/Helpers/cito-ui"),
   executable.path.hasSuffix("/Helpers/Cito.app/Contents/MacOS/cito-ui") {
    let argv = CommandLine.unsafeArgv
    let original = argv[0]
    executable.withUnsafeFileSystemRepresentation { path in
        guard let path else { return }
        argv[0] = UnsafeMutablePointer(mutating: path)
        _ = execv(path, argv)
        argv[0] = original
    }
    fputs("Cito could not start its bundled companion.\n", stderr)
    exit(EXIT_FAILURE)
}

// Headless tests use a real private pasteboard without changing the user clipboard.
let usesPrivatePasteboard = ProcessInfo.processInfo.environment["CITO_UI_HEADLESS"] == "1"
let appPasteboard = usesPrivatePasteboard ? NSPasteboard.withUniqueName() : NSPasteboard.general
atexit { if usesPrivatePasteboard { appPasteboard.releaseGlobally() } }

let app = NSApplication.shared
let companion = Companion()
app.delegate = companion
app.setActivationPolicy(.accessory)
if !companion.headless {
    let icon = Bundle.main.url(forResource: "AppIcon", withExtension: "icns")
        ?? executable.deletingLastPathComponent().appendingPathComponent("AppIcon.icns")
    if let image = NSImage(contentsOf: icon) { app.applicationIconImage = image }
}
companion.start()
if let menu = app.mainMenu?.items.first?.submenu {
    let about = NSMenuItem(title: "About Cito", action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)), keyEquivalent: "")
    about.target = app
    menu.insertItem(about, at: 0)
    menu.insertItem(.separator(), at: 1)
}
app.run()
