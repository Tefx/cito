import AppKit
import Foundation
import Darwin
import QuartzCore

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
    var displaySubtitle: String {
        let cleanHost = from_host.replacingOccurrences(of: "\\.local$", with: "", options: [.regularExpression, .caseInsensitive])
        let title = from_title.trimmingCharacters(in: .whitespacesAndNewlines)
        return "\(from_user)@\(cleanHost.isEmpty ? from_host : cleanHost)" + (title.isEmpty ? "" : " · \(title)")
    }
    var actionLabel: String {
        target_harness == "codex" ? "点击跳转至 Codex 对话 ↗" : "\(target_harness) 终端会话 · 点击关闭横幅"
    }
    var displayMessage: String {
        message.replacingOccurrences(of: "\\s*\\n+\\s*", with: "  ", options: .regularExpression)
    }
}


func diagnostic(_ message: String) {
    FileHandle.standardError.write(Data(("cito-ui: " + message + "\n").utf8))
}
final class ClickView: NSView {
    var clicked: (() -> Void)?
    private var tracking: NSTrackingArea?
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? { bounds.contains(point) ? self : nil }
    override func mouseDown(with event: NSEvent) { clicked?() }
    override func accessibilityPerformPress() -> Bool { clicked?(); return true }
    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let tracking = tracking { removeTrackingArea(tracking) }
        let area = NSTrackingArea(rect: bounds, options: [.mouseEnteredAndExited, .cursorUpdate, .activeAlways, .inVisibleRect], owner: self)
        addTrackingArea(area)
        tracking = area
    }
    override func resetCursorRects() { addCursorRect(bounds, cursor: .pointingHand) }
    override func cursorUpdate(with event: NSEvent) { NSCursor.pointingHand.set() }
    override func mouseEntered(with event: NSEvent) {
        NSCursor.pointingHand.set()
        layer?.borderColor = NSColor.white.withAlphaComponent(0.28).cgColor
    }
    override func mouseExited(with event: NSEvent) {
        NSCursor.arrow.set()
        layer?.borderColor = NSColor.white.withAlphaComponent(0.14).cgColor
    }
}


struct Hub: Decodable {
    let name: String, status: String, auth_mode: String
    let user: String?, error: String?
}
struct Listener: Decodable {
    let hub: String, listener_id: String, public_id: String, scoped_id: String
    let harness: String, session_id: String, title: String, cwd: String, scope: String
    let repo: String?
    let can_jump: Bool
}
struct Frame: Decodable {
    let op: String
    let hubs: [Hub]?
    let listeners: [Listener]?
    let payload: Payload?
}
final class Companion: NSObject {
    let headless = ProcessInfo.processInfo.environment["CITO_UI_HEADLESS"] == "1" || ProcessInfo.processInfo.environment["CITO_BANNER_HEADLESS"] == "1"
    let dismissMs = max(1, Int(ProcessInfo.processInfo.environment["CITO_BANNER_DISMISS_MS"] ?? "6000") ?? 6000)
    var status: NSStatusItem?
    var panel: NSPanel?
    var labels: [NSTextField] = []
    var accentBar: NSView?
    var iconBadge: NSView?
    var icon: NSImageView?
    var payload: Payload?
    var displayedCount = 0
    var visible = false
    var timer: DispatchWorkItem?
    var clickSource: DispatchSourceSignal?
    var menuActions: [() -> Void] = []
    var hubs: [Hub] = []
    var listeners: [Listener] = []
    func start() {
        signal(SIGPIPE, SIG_IGN)
        signal(SIGUSR1, SIG_IGN)
        clickSource = DispatchSource.makeSignalSource(signal: SIGUSR1, queue: .main)
        clickSource?.setEventHandler { [weak self] in self?.hide(clicked: true) }
        clickSource?.resume()
        if !headless { status = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength) }
        DispatchQueue.global().async { [weak self] in
            var buffer = Data()
            var dropping = false
            var bytes = [UInt8](repeating: 0, count: 8192)
            while true {
                let n = Darwin.read(STDIN_FILENO, &bytes, bytes.count)
                if n < 0 && errno == EINTR { continue }
                if n <= 0 { exit(0) }
                for byte in bytes.prefix(n) {
                    if byte == 10 {
                        let line = buffer; buffer.removeAll(keepingCapacity: true)
                        if dropping { dropping = false; continue }
                        DispatchQueue.main.async { self?.receive(line) }
                    } else if !dropping {
                        buffer.append(byte)
                        if buffer.count > 1_048_576 { diagnostic("oversized frame"); buffer.removeAll(); dropping = true }
                    }
                }
            }
        }
    }
    func receive(_ data: Data) {
        // Headless hooks use the exact same menu handlers and banner click path.
        if headless, let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let op = object["op"] as? String {
            if op == "test_click" { hide(clicked: true); return }
            if op == "test_action", let index = object["index"] as? Int, menuActions.indices.contains(index) {
                menuActions[index](); return
            }
        }
        guard let frame = try? JSONDecoder().decode(Frame.self, from: data) else { diagnostic("invalid frame"); return }
        if frame.op == "state", let h = frame.hubs, let l = frame.listeners,
           h.allSatisfy({ ["connected", "connecting", "auth_required", "disconnected"].contains($0.status) && ["none", "oidc"].contains($0.auth_mode) }),
           l.allSatisfy({ ["self", "workspace"].contains($0.scope) }) {
            hubs = h; listeners = l; rebuild()
            if headless { diagnostic("state hubs=\(h.count) listeners=\(l.count)") }
        } else if frame.op == "banner", let value = frame.payload, !value.target_harness.isEmpty {
            if visible, let previous = payload, previous.target_session_id == value.target_session_id && previous.target_harness == value.target_harness && previous.effective_wake == value.effective_wake {
                displayedCount += value.count ?? 1
            } else { displayedCount = value.count ?? 1 }
            payload = value; show(value)
            timer?.cancel()
            let work = DispatchWorkItem { [weak self] in self?.hide(clicked: false) }
            timer = work
            DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(dismissMs), execute: work)
        } else { diagnostic("invalid frame") }
    }
    func emit(_ action: [String: String]) {
        guard let data = try? JSONSerialization.data(withJSONObject: action, options: [.sortedKeys]) else { return }
        FileHandle.standardOutput.write(data + Data([10]))
    }
    @objc func act(_ sender: NSMenuItem) {
        if menuActions.indices.contains(sender.tag) { menuActions[sender.tag]() }
    }
    @discardableResult func item(_ title: String, _ menu: NSMenu, _ action: @escaping () -> Void) -> NSMenuItem {
        let value = NSMenuItem(title: title, action: #selector(act(_:)), keyEquivalent: "")
        value.target = self; value.tag = menuActions.count; menuActions.append(action); menu.addItem(value); return value
    }
    func rebuild() {
        menuActions.removeAll()
        status?.button?.title = listeners.isEmpty ? "Cito" : "🔔 \(listeners.count)"
        let menu = NSMenu()
        menu.addItem(withTitle: "活跃监听", action: nil, keyEquivalent: "")
        for listener in listeners {
            let title = "\(listener.title) · \(listener.scoped_id) · \(listener.harness)"
            let parent = NSMenuItem(title: title, action: nil, keyEquivalent: "")
            menu.addItem(parent)
            let sub = NSMenu(); parent.submenu = sub
            if listener.can_jump { item("跳转至对话 ↗", sub) { self.openCodex(listener.session_id) } }
            item("复制 \(listener.scoped_id)", sub) {
                NSPasteboard.general.clearContents(); NSPasteboard.general.setString(listener.scoped_id, forType: .string)
            }
            item("可见范围: \(listener.scope) — 切换", sub) {
                self.emit(["op": "set_scope", "hub": listener.hub, "listener_id": listener.listener_id,
                           "scope": listener.scope == "self" ? "workspace" : "self"])
            }
            let unlisten = { self.emit(["op": "unlisten", "hub": listener.hub, "listener_id": listener.listener_id]) }
            item("取消监听", sub, unlisten)
            let alternate = item(title + " — 取消监听", menu, unlisten)
            alternate.isAlternate = true; alternate.keyEquivalentModifierMask = [.option]
        }
        if !listeners.isEmpty { item("取消全部监听", menu) { self.emit(["op": "unlisten_all"]) } }
        menu.addItem(.separator())
        menu.addItem(withTitle: "Hubs", action: nil, keyEquivalent: "")
        for hub in hubs {
            let parent = NSMenuItem(title: "\(hub.name) · \(hub.status) · \(hub.user ?? "—")", action: nil, keyEquivalent: "")
            menu.addItem(parent)
            let sub = NSMenu(); parent.submenu = sub
            if let error = hub.error { sub.addItem(withTitle: error, action: nil, keyEquivalent: "") }
            for (title, action) in [("登录 OIDC", "login"), ("重连", "reconnect"), ("断开", "disconnect")] {
                if action == "login" && hub.auth_mode != "oidc" { continue }
                item(title, sub) { self.emit(["op": "hub_action", "hub": hub.name, "action": action]) }
            }
        }
        menu.addItem(.separator())
        item("退出 Cito", menu) { self.emit(["op": "quit"]) }
        status?.menu = menu
    }
    func hide(clicked: Bool) {
        timer?.cancel()
        if clicked, visible, let value = payload, value.target_harness == "codex" { openCodex(value.target_session_id) }
        visible = false; panel?.orderOut(nil)
        if headless { diagnostic(clicked ? "banner clicked" : "banner hidden") }
    }
    func show(_ value: Payload) {
        let title = "Cito · \(value.target_title)" + (displayedCount > 1 ? " (\(displayedCount))" : "")
        let semanticLabel = (value.effective_wake ? "唤醒" : "仅知会") + (displayedCount > 1 ? " · ×\(displayedCount)" : "")
        visible = true
        if headless {
            // Diagnostics expose the actual presentation model, without adding
            // test-only socket commands or bypassing delivery/retirement.
            diagnostic("display " + String(data: try! JSONSerialization.data(withJSONObject: [
                "pid": getpid(), "title": title, "subtitle": value.subtitle, "message": value.message,
                "displayed_count": displayedCount, "display_subtitle": value.displaySubtitle,
                "display_message": value.displayMessage, "semantic_label": semanticLabel, "action_label": value.actionLabel
            ], options: [.sortedKeys]), encoding: .utf8)!)
            return
        }
        if panel == nil {
            let window = NSPanel(contentRect: NSRect(x: 0, y: 0, width: 388, height: 136),
                                 styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
            window.level = .floating
            window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
            window.isFloatingPanel = true
            window.hidesOnDeactivate = false
            window.isOpaque = false
            window.backgroundColor = .clear
            window.hasShadow = true
            window.appearance = NSAppearance(named: .vibrantDark)
            let root = ClickView(frame: window.contentView!.bounds)
            root.clicked = { [weak self] in self?.hide(clicked: true) }
            root.setAccessibilityElement(true)
            root.setAccessibilityRole(.button)
            root.wantsLayer = true
            root.layer?.cornerRadius = 16
            root.layer?.cornerCurve = .continuous
            root.layer?.masksToBounds = true
            root.layer?.borderWidth = 1
            root.layer?.borderColor = NSColor.white.withAlphaComponent(0.14).cgColor
            let effect = NSVisualEffectView(frame: root.bounds)
            effect.material = .hudWindow
            effect.blendingMode = .behindWindow
            effect.state = .active
            effect.wantsLayer = true
            effect.layer?.cornerRadius = 16
            effect.layer?.cornerCurve = .continuous
            effect.layer?.masksToBounds = true
            root.addSubview(effect)
            let bar = NSView(frame: NSRect(x: 0, y: 0, width: 3.5, height: 136))
            bar.wantsLayer = true
            root.addSubview(bar)
            accentBar = bar
            let badge = NSView(frame: NSRect(x: 16, y: 88, width: 32, height: 32))
            badge.wantsLayer = true
            badge.layer?.cornerRadius = 9
            badge.layer?.cornerCurve = .continuous
            let image = NSImageView(frame: NSRect(x: 6, y: 6, width: 20, height: 20))
            badge.addSubview(image)
            root.addSubview(badge)
            iconBadge = badge
            icon = image
            let frames = [NSRect(x: 60, y: 103, width: 224, height: 19),
                          NSRect(x: 60, y: 84, width: 312, height: 18),
                          NSRect(x: 16, y: 40, width: 356, height: 38),
                          NSRect(x: 16, y: 13, width: 356, height: 18),
                          NSRect(x: 288, y: 103, width: 84, height: 20)]
            for (index, size) in [13.0, 11.0, 12.0, 11.0, 10.0].enumerated() {
                let label = NSTextField(wrappingLabelWithString: "")
                label.isSelectable = false
                label.font = index == 0 || index == 4 ? .boldSystemFont(ofSize: size) : .systemFont(ofSize: size)
                label.textColor = index == 1 ? NSColor.white.withAlphaComponent(0.7) : .white
                label.frame = frames[index]
                label.maximumNumberOfLines = index == 2 ? 2 : 1
                label.lineBreakMode = .byTruncatingTail
                if index == 4 {
                    label.alignment = .center
                    label.wantsLayer = true
                    label.layer?.cornerRadius = 7
                    label.layer?.masksToBounds = true
                }
                root.addSubview(label)
                labels.append(label)
            }
            window.contentView = root
            panel = window
        }
        let color = value.effective_wake
            ? NSColor(srgbRed: 1, green: 159.0 / 255, blue: 10.0 / 255, alpha: 1)
            : NSColor(srgbRed: 100.0 / 255, green: 210.0 / 255, blue: 1, alpha: 1)
        accentBar?.layer?.backgroundColor = color.cgColor
        iconBadge?.layer?.backgroundColor = color.withAlphaComponent(0.16).cgColor
        icon?.image = NSImage(systemSymbolName: value.effective_wake ? "bell.badge.fill" : "tray.and.arrow.down.fill", accessibilityDescription: semanticLabel)
        icon?.contentTintColor = color
        labels[0].stringValue = "Cito" + (value.target_title.isEmpty ? "" : " · \(value.target_title)")
        labels[1].stringValue = value.displaySubtitle
        labels[2].stringValue = value.displayMessage
        labels[3].stringValue = value.actionLabel
        labels[3].textColor = value.target_harness == "codex" ? .systemBlue : .tertiaryLabelColor
        labels[4].stringValue = semanticLabel
        labels[4].textColor = color
        labels[4].layer?.backgroundColor = color.withAlphaComponent(0.14).cgColor
        panel?.contentView?.setAccessibilityLabel("\(labels[0].stringValue). \(semanticLabel). \(value.displaySubtitle). \(value.displayMessage). \(labels[3].stringValue)")
        let initialDisplay = !(panel?.isVisible ?? false)
        if let screen = NSScreen.main, let window = panel {
            let area = screen.visibleFrame
            window.setFrameTopLeftPoint(NSPoint(x: area.maxX - 404, y: area.maxY - 16))
            let destination = window.frame
            if initialDisplay {
                window.setFrame(destination.offsetBy(dx: 20, dy: 0), display: false)
                window.orderFrontRegardless()
                NSAnimationContext.runAnimationGroup { context in
                    context.duration = 0.20
                    context.timingFunction = CAMediaTimingFunction(name: .easeOut)
                    window.animator().setFrame(destination, display: true)
                }
            }
        }
        panel?.orderFrontRegardless() // Never makeKey, activate, or change first responder.
    }


    func openCodex(_ session: String) {
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
        } else if headless { diagnostic("open " + url.absoluteString) }
        else { NSWorkspace.shared.open(url) }
    }
}


let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let companion = Companion()
companion.start()
app.run()
