import AppKit
import Foundation
import Darwin
import SQLite3

// Build: /usr/bin/swiftc src/macos/cito-banner.swift -o <state>/bin/cito-banner
// Headless uses the same server, timers, retirement locks and click action. SIGUSR1
// routes through the panel's click handler; CITO_BANNER_URL_OPENER replaces only
// NSWorkspace.open (executable receives the URL as its single argument).
struct Payload: Decodable {
    let target_harness: String
    let target_session_id: String
    let target_title: String
    let from_user: String
    let from_host: String
    let from_title: String
    let message: String
    let effective_wake: Bool
    let count: Int?

    var subtitle: String {
        (effective_wake ? "" : "[仅知会] ") + "\(from_user)@\(from_host) · \(from_title)"
    }
}

func diagnostic(_ message: String) {
    FileHandle.standardError.write(Data(("cito-banner: " + message + "\n").utf8))
}

func acquire(_ fd: Int32, milliseconds: Int) -> Bool {
    let deadline = DispatchTime.now().uptimeNanoseconds + UInt64(milliseconds) * 1_000_000
    repeat {
        if flock(fd, LOCK_EX | LOCK_NB) == 0 { return true }
        if errno != EWOULDBLOCK && errno != EINTR { return false }
        if DispatchTime.now().uptimeNanoseconds >= deadline { return false }
        usleep(10_000)
    } while true
}

final class ClickView: NSView {
    var clicked: (() -> Void)?
    override func mouseDown(with event: NSEvent) { clicked?() }
    override func accessibilityPerformPress() -> Bool { clicked?(); return true }
}

final class Banner {
    let state: String
    let headless = ProcessInfo.processInfo.environment["CITO_BANNER_HEADLESS"] == "1"
    let instanceFd: Int32
    let lockFd: Int32
    var server: Int32 = -1
    var source: DispatchSourceRead?
    var clickSource: DispatchSourceSignal?
    var timer: DispatchWorkItem?
    var payload: Payload?
    var panel: NSPanel?
    var labels: [NSTextField] = []
    var retiring = false
    let queue = DispatchQueue(label: "cito.banner.socket")
    let dismissMs: Int

    init(state: String, instanceFd: Int32) {
        self.state = state
        self.instanceFd = instanceFd
        self.lockFd = Darwin.open("banner.lock", O_CREAT | O_RDWR | O_CLOEXEC, 0o600)
        self.dismissMs = max(1, Int(ProcessInfo.processInfo.environment["CITO_BANNER_DISMISS_MS"] ?? "6000") ?? 6000)
    }

    func start() {
        guard lockFd >= 0 else { exit(1) }
        signal(SIGPIPE, SIG_IGN)
        server = socket(AF_UNIX, SOCK_STREAM, 0)
        guard server >= 0 else { exit(1) }
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        // chdir was performed before any threads started. Relative bind avoids
        // Darwin's 104-byte sun_path limit even for a deeply nested state dir.
        withUnsafeMutableBytes(of: &address.sun_path) { buffer in
            buffer.copyBytes(from: Array("banner.sock\0".utf8))
        }
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.bind(server, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard bound == 0, chmod("banner.sock", 0o600) == 0, listen(server, 16) == 0 else {
            diagnostic("bind/listen failed: \(errno)"); exit(1)
        }
        _ = fcntl(server, F_SETFL, O_NONBLOCK)
        let readSource = DispatchSource.makeReadSource(fileDescriptor: server, queue: queue)
        readSource.setEventHandler { [weak self] in self?.acceptClients() }
        source = readSource
        readSource.resume()
        signal(SIGUSR1, SIG_IGN)
        clickSource = DispatchSource.makeSignalSource(signal: SIGUSR1, queue: .main)
        clickSource?.setEventHandler { [weak self] in self?.retire(clicked: true) }
        clickSource?.resume()
        resetTimer() // Also retire if a sender disappears between spawn and delivery.
    }

    func acceptClients() {
        while true {
            let client = accept(server, nil, nil)
            if client < 0 { return }
            // Darwin accept() inherits O_NONBLOCK from the listener. Read the
            // complete line with the bounded receive timeout, not an EAGAIN race
            // against the sender's first write.
            _ = fcntl(client, F_SETFL, 0)
            var timeout = timeval(tv_sec: 1, tv_usec: 0)
            _ = setsockopt(client, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
            var data = Data()
            var bytes = [UInt8](repeating: 0, count: 4096)
            var complete = false
            while data.count <= 65_536 {
                let n = Darwin.read(client, &bytes, bytes.count)
                if n <= 0 { break }
                data.append(contentsOf: bytes.prefix(n))
                if data.contains(10) { complete = true; break }
            }
            guard complete, let end = data.firstIndex(of: 10),
                  let next = try? JSONDecoder().decode(Payload.self, from: data[..<end]),
                  ["codex", "pi"].contains(next.target_harness) else {
                close(client); continue
            }
            DispatchQueue.main.async { [weak self] in
                defer { close(client) }
                guard let self = self, !self.retiring else { return }
                self.payload = next
                self.show(next)
                self.resetTimer()
                let ack = Array("{\"ok\":true}\n".utf8)
                _ = ack.withUnsafeBytes { Darwin.write(client, $0.baseAddress, $0.count) }
            }
        }
    }

    func show(_ value: Payload) {
        if headless {
            // Diagnostics expose the actual presentation model, without adding
            // test-only socket commands or bypassing delivery/retirement.
            diagnostic("display " + String(data: try! JSONSerialization.data(withJSONObject: [
                "pid": getpid(), "subtitle": value.subtitle, "message": value.message
            ], options: [.sortedKeys]), encoding: .utf8)!)
            return
        }
        if panel == nil {
            let window = NSPanel(contentRect: NSRect(x: 0, y: 0, width: 380, height: 156),
                                 styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
            window.level = .floating
            window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
            window.isFloatingPanel = true
            window.hidesOnDeactivate = false
            window.isOpaque = false
            window.backgroundColor = .clear
            window.hasShadow = true
            let root = ClickView(frame: window.contentView!.bounds)
            root.clicked = { [weak self] in self?.retire(clicked: true) }
            root.setAccessibilityElement(true)
            root.setAccessibilityRole(.button)
            let effect = NSVisualEffectView(frame: root.bounds)
            effect.material = .hudWindow
            effect.blendingMode = .behindWindow
            effect.state = .active
            effect.wantsLayer = true
            effect.layer?.cornerRadius = 14
            root.addSubview(effect)
            for (index, size) in [14.0, 12.0, 13.0].enumerated() {
                let label = NSTextField(wrappingLabelWithString: "")
                label.font = index == 0 ? .boldSystemFont(ofSize: size) : .systemFont(ofSize: size)
                label.textColor = index == 1 ? .secondaryLabelColor : .labelColor
                label.frame = NSRect(x: 16, y: [118.0, 94.0, 14.0][index], width: 348, height: index == 2 ? 70 : 22)
                label.maximumNumberOfLines = index == 2 ? 3 : 1
                root.addSubview(label)
                labels.append(label)
            }
            window.contentView = root
            panel = window
        }
        labels[0].stringValue = "Cito · \(value.target_title)" + ((value.count ?? 1) > 1 ? " (\(value.count!))" : "")
        labels[1].stringValue = value.subtitle
        labels[2].stringValue = value.message
        panel?.contentView?.setAccessibilityLabel("\(labels[0].stringValue). \(value.subtitle). \(value.message)")
        if let screen = NSScreen.main {
            let area = screen.visibleFrame
            panel?.setFrameTopLeftPoint(NSPoint(x: area.maxX - 396, y: area.maxY - 16))
        }
        panel?.orderFrontRegardless() // Never makeKey, activate, or change first responder.
    }

    func resetTimer(after milliseconds: Int? = nil) {
        timer?.cancel()
        let item = DispatchWorkItem { [weak self] in self?.retire(clicked: false) }
        timer = item
        DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(milliseconds ?? dismissMs), execute: item)
    }

    func retire(clicked: Bool) {
        guard !retiring else { return }
        guard acquire(lockFd, milliseconds: clicked ? 500 : 0) else {
            // A failed sender may not reset the timer. Retry without dropping
            // the active instance; successful deliveries replace this timer.
            resetTimer(after: 50)
            return
        }
        retiring = true
        timer?.cancel()
        unlink("banner.sock")
        source?.cancel()
        close(server)
        if clicked, let value = payload, value.target_harness == "codex" { openCodex(value.target_session_id) }
        if clicked { panel?.orderOut(nil); exit(0) }
        NSAnimationContext.runAnimationGroup { context in
            context.duration = headless ? 0 : 0.25
            panel?.animator().alphaValue = 0
        }
        // Preserve the lifetime locks through fade-out, including headless tests.
        DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(headless ? 10 : 250)) { [self] in
            panel?.orderOut(nil)
            exit(0)
        }
    }

    func openCodex(_ session: String) {
        var db: OpaquePointer?
        let status = sqlite3_open_v2(state + "/state.sqlite", &db, SQLITE_OPEN_READWRITE, nil)
        if status == SQLITE_OK, let db = db {
            sqlite3_busy_timeout(db, 1000)
            var statement: OpaquePointer?
            let sql = "UPDATE local_listeners SET human_event_seq = human_event_seq + 1 WHERE harness = 'codex' AND session_id = ?"
            let prepared = sqlite3_prepare_v2(db, sql, -1, &statement, nil)
            if prepared == SQLITE_OK {
                let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
                sqlite3_bind_text(statement, 1, session, -1, transient)
                let result = sqlite3_step(statement)
                if result != SQLITE_DONE { diagnostic("SQLite click update: \(String(cString: sqlite3_errmsg(db)))") }
            } else { diagnostic("SQLite click prepare: \(String(cString: sqlite3_errmsg(db)))") }
            sqlite3_finalize(statement)
        } else { diagnostic("SQLite unavailable (\(status)); opening thread without watermark update") }
        if db != nil { sqlite3_close(db) }
        var allowed = CharacterSet.urlPathAllowed
        allowed.remove(charactersIn: "/?#%")
        guard let encoded = session.addingPercentEncoding(withAllowedCharacters: allowed),
              let url = URL(string: "codex://threads/" + encoded) else { return }
        if let override = ProcessInfo.processInfo.environment["CITO_BANNER_URL_OPENER"] {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: override)
            process.arguments = [url.absoluteString]
            do { try process.run(); process.waitUntilExit() }
            catch { diagnostic("URL opener failed: \(error)") }
        } else { NSWorkspace.shared.open(url) }
    }
}

let state = URL(fileURLWithPath: ProcessInfo.processInfo.environment["CITO_STATE_DIR"] ?? (NSHomeDirectory() + "/.cito")).standardizedFileURL.path
let sockets = state + "/sockets"
do { try FileManager.default.createDirectory(atPath: sockets, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700]) }
catch { diagnostic("cannot create sockets: \(error)"); exit(1) }
guard chdir(sockets) == 0 else { exit(1) }
let instance = Darwin.open("banner-instance.lock", O_CREAT | O_RDWR | O_CLOEXEC, 0o600)
guard instance >= 0 else { exit(1) }
guard acquire(instance, milliseconds: 2000) else { exit(0) }
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let banner = Banner(state: state, instanceFd: instance)
banner.start()
app.run()
