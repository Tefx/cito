import AppKit
import Foundation
import Darwin
import QuartzCore

enum StatusIconState { case active, idle, warning }
func makeCitoStatusIcon(_ state: StatusIconState) -> NSImage {
    let image = NSImage(size: NSSize(width: 18, height: 18), flipped: false) { _ in
        let center = NSPoint(x: 8, y: 9)
        let arc = NSBezierPath()
        arc.appendArc(withCenter: center, radius: 5.50, startAngle: 42, endAngle: 318, clockwise: false)
        arc.lineWidth = 1.75; arc.lineCapStyle = .round
        NSColor.black.setStroke(); arc.stroke()
        NSColor.black.withAlphaComponent(state == .active ? 1 : 0.40).setFill()
        NSBezierPath(ovalIn: NSRect(x: 6.35, y: 7.35, width: 3.3, height: 3.3)).fill()
        let wave = NSBezierPath()
        wave.appendArc(withCenter: center, radius: 7.75, startAngle: -28, endAngle: 28, clockwise: false)
        wave.lineWidth = 1.65; wave.lineCapStyle = .round
        NSColor.black.withAlphaComponent(state == .active ? 1 : 0.40).setStroke(); wave.stroke()
        if state == .warning, let context = NSGraphicsContext.current?.cgContext {
            context.saveGState(); context.setBlendMode(.destinationOut)
            NSColor.black.setFill()
            NSBezierPath(ovalIn: NSRect(x: 10.6, y: 0.4, width: 7.2, height: 7.2)).fill()
            context.setBlendMode(.normal)
            NSBezierPath(ovalIn: NSRect(x: 11.8, y: 1.6, width: 4.8, height: 4.8)).fill()
            context.restoreGState()
        }
        return true
    }
    image.isTemplate = true
    return image
}

func formatStatusButton(_ button: NSButton, count: Int, image: NSImage?) {
    button.image = image
    button.title = ""
    button.imagePosition = count == 0 ? .imageOnly : .imageLeading
    if count > 0 {
        button.attributedTitle = NSAttributedString(string: " \(count)",
            attributes: [.font: NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .semibold)])
    }
}

func textLabel(_ title: String, _ frame: NSRect, size: CGFloat = 11, weight: NSFont.Weight = .regular,
               color: NSColor = .labelColor) -> NSTextField {
    let label = NSTextField(labelWithString: title)
    label.frame = frame; label.font = .systemFont(ofSize: size, weight: weight); label.textColor = color
    label.lineBreakMode = .byTruncatingTail; label.maximumNumberOfLines = 1
    return label
}
func roundedView(_ frame: NSRect, radius: CGFloat, color: NSColor) -> NSView {
    let view = NSView(frame: frame); view.wantsLayer = true
    view.layer?.cornerRadius = radius; view.layer?.backgroundColor = color.cgColor
    return view
}
func measureTextWidth(_ text: String, font: NSFont) -> CGFloat {
    let field = NSTextField(labelWithString: text)
    field.font = font
    return ceil(field.fittingSize.width)
}

/** Menu controls own their clicks so copy and scope changes keep native tracking open. */
final class MenuControl: NSControl {
    let label: NSTextField
    var leadingSymbolView: NSImageView?
    var symbolName: String?
    var pressed: (() -> Void)?
    var hoverColor: NSColor?
    var normalColor: NSColor = .labelColor
    var normalBackground = NSColor.labelColor.withAlphaComponent(0.06)
    var feedbackTimer: Timer?
    init(_ title: String, frame: NSRect, symbol: String? = nil, leadingSymbol: String? = nil, color: NSColor = .labelColor, radius: CGFloat = 6) {
        let hasLeading = leadingSymbol != nil
        label = textLabel(title, NSRect(x: hasLeading ? 20 : 3, y: (frame.height - 15) / 2, width: frame.width - (hasLeading ? 24 : 6), height: 15))
        symbolName = leadingSymbol ?? symbol
        super.init(frame: frame)
        wantsLayer = true; layer?.cornerRadius = radius
        layer?.backgroundColor = NSColor.labelColor.withAlphaComponent(0.06).cgColor
        label.alignment = .center; label.textColor = color; normalColor = color
        addSubview(label)
        if let leadingSymbol = leadingSymbol {
            let image = NSImageView(frame: NSRect(x: 6, y: (frame.height - 12) / 2, width: 12, height: 12))
            image.image = NSImage(systemSymbolName: leadingSymbol, accessibilityDescription: nil)
            image.contentTintColor = color; addSubview(image); leadingSymbolView = image
        }
        if let symbol = symbol {
            let image = NSImageView(frame: bounds.insetBy(dx: 5, dy: 5))
            image.image = NSImage(systemSymbolName: symbol, accessibilityDescription: title)
            image.contentTintColor = color; addSubview(image); label.isHidden = true
        }
        setAccessibilityElement(true); setAccessibilityRole(.button); setAccessibilityLabel(title)
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }
    func setLeadingSymbol(_ name: String?) {
        symbolName = name
        if let name = name {
            leadingSymbolView?.image = NSImage(systemSymbolName: name, accessibilityDescription: nil)
            leadingSymbolView?.isHidden = false
            label.frame = NSRect(x: 20, y: (bounds.height - 15) / 2, width: bounds.width - 24, height: 15)
        } else {
            leadingSymbolView?.isHidden = true
            label.frame = NSRect(x: 3, y: (bounds.height - 15) / 2, width: bounds.width - 6, height: 15)
        }
    }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? { frame.contains(point) ? self : nil }
    override func mouseDown(with event: NSEvent) { pressed?() }
    override func accessibilityPerformPress() -> Bool { pressed?(); return true }
    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        for area in trackingAreas { removeTrackingArea(area) }
        addTrackingArea(NSTrackingArea(rect: bounds, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self))
    }
    override func mouseEntered(with event: NSEvent) {
        let tint = hoverColor ?? .controlAccentColor
        layer?.backgroundColor = tint.withAlphaComponent(0.15).cgColor
        label.textColor = feedbackTimer?.isValid == true ? .systemGreen : tint
        for case let image as NSImageView in subviews { image.contentTintColor = tint }
    }
    override func mouseExited(with event: NSEvent) {
        layer?.backgroundColor = normalBackground.cgColor
        label.textColor = feedbackTimer?.isValid == true ? .systemGreen : normalColor
        for case let image as NSImageView in subviews { image.contentTintColor = normalColor }
    }
    func copied() {
        feedbackTimer?.invalidate()
        leadingSymbolView?.isHidden = true
        label.frame = NSRect(x: 3, y: (bounds.height - 15) / 2, width: bounds.width - 6, height: 15)
        label.stringValue = "✓ Copied"; label.textColor = .systemGreen
        let restore = Timer(timeInterval: 1.2, repeats: false) { [weak self] _ in
            guard let self = self else { return }
            if self.leadingSymbolView != nil {
                self.leadingSymbolView?.isHidden = false
                self.label.frame = NSRect(x: 20, y: (self.bounds.height - 15) / 2, width: self.bounds.width - 24, height: 15)
            }
            self.label.stringValue = self.accessibilityLabel() ?? ""; self.label.textColor = self.normalColor
        }
        feedbackTimer = restore
        RunLoop.main.add(restore, forMode: .common); RunLoop.main.add(restore, forMode: .eventTracking)
    }
}

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
        (effective_wake ? "" : "[Passive] ") + "\(from_user)@\(from_host) · \(from_title)"
    }
    var displaySubtitle: String {
        let cleanHost = from_host.replacingOccurrences(of: "\\.local$", with: "", options: [.regularExpression, .caseInsensitive])
        let title = from_title.trimmingCharacters(in: .whitespacesAndNewlines)
        return (effective_wake ? "" : "[Passive] ") + "\(from_user)@\(cleanHost.isEmpty ? from_host : cleanHost)" + (title.isEmpty ? "" : " · \(title)")
    }
    var actionLabel: String {
        target_harness == "codex" ? "Open in Codex ↗" : "\(target_harness) session · Click to dismiss"
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
    let about: String?, repo: String?, branch: String?
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
    var headlessStatusButton: NSButton?
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
    var cardMenu: NSMenu?
    var selectedHubs = Set<String>()
    var statusImage: NSImage?
    var uniqueSessionCount: Int { Set(listeners.map { "\($0.harness):\($0.session_id)" }).count }
    var hubs: [Hub] = []
    var listeners: [Listener] = []
    func start() {
        signal(SIGPIPE, SIG_IGN)
        signal(SIGUSR1, SIG_IGN)
        clickSource = DispatchSource.makeSignalSource(signal: SIGUSR1, queue: .main)
        clickSource?.setEventHandler { [weak self] in self?.hide(clicked: true) }
        clickSource?.resume()
        if headless { headlessStatusButton = NSButton(frame: .zero) }
        else { status = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength) }
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
            if op == "test_inspect" { inspectMenu(); return }
            if op == "test_control", let id = object["id"] as? String {
                func find(_ view: NSView) -> MenuControl? {
                    if view.identifier?.rawValue == id, let control = view as? MenuControl { return control }
                    return view.subviews.compactMap { find($0) }.first
                }
                for item in cardMenu?.items ?? [] {
                    if let view = item.view, let control = find(view) { _ = control.accessibilityPerformPress(); break }
                }
                return
            }
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
    func rebuild() {
        menuActions.removeAll()
        let count = uniqueSessionCount
        let warning = hubs.contains { $0.status != "connected" || $0.error != nil }
        statusImage = makeCitoStatusIcon(warning ? .warning : count > 0 ? .active : .idle)
        if let button = status?.button ?? headlessStatusButton {
            formatStatusButton(button, count: count, image: statusImage)
        }
        let menu = NSMenu(); menu.autoenablesItems = false
        cardMenu = menu
        func host(_ view: NSView, id: String) -> NSMenuItem {
            view.identifier = NSUserInterfaceItemIdentifier(id)
            let item = NSMenuItem(); item.view = view; menu.addItem(item); return item
        }
        func register(_ action: @escaping () -> Void) { menuActions.append(action) }
        let header = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 40))
        header.addSubview(textLabel("Cito", NSRect(x: 12, y: 12, width: 32, height: 18), size: 13, weight: .bold))
        header.addSubview(textLabel(count > 0 ? "\(count) listening" : "Idle",
            NSRect(x: 50, y: 12, width: 84, height: 17), size: 11.5, color: .secondaryLabelColor))
        _ = host(header, id: "header")
        var drawerItems: [String: NSMenuItem] = [:]
        var hubActions: [() -> Void] = []
        let btnFont = NSFont.systemFont(ofSize: 11, weight: .medium)
        for hub in hubs {
            let drawer = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 32))
            let loginRun = { self.emit(["op": "hub_action", "hub": hub.name, "action": "login"]) }
            let reconnectRun = { self.emit(["op": "hub_action", "hub": hub.name, "action": "reconnect"]) }
            let disconnectRun = { self.emit(["op": "hub_action", "hub": hub.name, "action": "disconnect"]) }
            if hub.auth_mode == "oidc" { hubActions.append(loginRun) }
            hubActions.append(reconnectRun); hubActions.append(disconnectRun)
            let buttons: [(String, String, Bool, () -> Void)]
            switch hub.status {
            case "connected" where hub.error != nil:
                buttons = [("Reconnect", "reconnect", false, reconnectRun), ("Disconnect", "disconnect", false, disconnectRun)]
            case "connected" where hub.auth_mode == "oidc":
                buttons = [("Switch Account", "login", false, loginRun), ("Disconnect", "disconnect", false, disconnectRun)]
            case "connected":
                buttons = [("Disconnect", "disconnect", false, disconnectRun)]
            case "connecting":
                buttons = [("Cancel", "disconnect", false, disconnectRun)]
            case "auth_required":
                buttons = [("Sign In…", "login", true, loginRun), ("Disconnect", "disconnect", false, disconnectRun)]
            default:
                buttons = hub.auth_mode == "oidc"
                    ? [("Connect", "reconnect", true, reconnectRun), ("Sign In…", "login", false, loginRun)]
                    : [("Connect", "reconnect", true, reconnectRun)]
            }
            var btnX: CGFloat = 368
            for (title, action, primary, run) in buttons.reversed() {
                let width = measureTextWidth(title, font: btnFont) + 16
                btnX -= width
                let control = MenuControl(title, frame: NSRect(x: btnX, y: 5, width: width, height: 22), color: primary ? .systemBlue : .labelColor)
                control.label.font = btnFont
                if primary {
                    control.normalBackground = NSColor.systemBlue.withAlphaComponent(0.15)
                    control.layer?.backgroundColor = control.normalBackground.cgColor
                }
                control.identifier = NSUserInterfaceItemIdentifier("\(action):\(hub.name)")
                control.pressed = run; drawer.addSubview(control); btnX -= 4
            }
            let buttonsMinX = btnX + 4
            let statusText: String
            if let error = hub.error { statusText = error }
            else if hub.status == "connected" { statusText = hub.auth_mode == "oidc" ? "Connected as \(hub.user ?? "user")" : "Connected · \(hub.user ?? "local")" }
            else if hub.status == "connecting" { statusText = "Connecting to hub…" }
            else if hub.status == "auth_required" { statusText = "Authentication required" }
            else { statusText = "Disconnected" }
            let detail = textLabel(statusText, NSRect(x: 12, y: 8, width: max(40, buttonsMinX - 20), height: 16), color: .secondaryLabelColor)
            detail.toolTip = hub.error ?? statusText; drawer.addSubview(detail)
            let item = host(drawer, id: "drawer:\(hub.name)")
            let autoExpand = hub.status != "connected" || hub.error != nil
            item.isHidden = selectedHubs.contains(hub.name) ? false : !autoExpand
            drawerItems[hub.name] = item
        }
        let hubStrip = NSScrollView(frame: NSRect(x: 138, y: 9, width: 230, height: 22))
        hubStrip.drawsBackground = false; hubStrip.hasHorizontalScroller = false
        let pillContainer = NSView(frame: NSRect(x: 0, y: 0, width: 230, height: 22))
        hubStrip.documentView = pillContainer; header.addSubview(hubStrip)
        var pillX: CGFloat = 230
        for hub in hubs.reversed() {
            let color: NSColor = hub.status == "connected" ? .systemGreen : hub.status == "connecting" ? .systemOrange : hub.status == "auth_required" ? .systemRed : .systemGray
            let isSingleDefault = hubs.count == 1 && hub.name == "default"
            let isCompactIcon = isSingleDefault && hub.auth_mode == "none" && hub.status == "connected" && hub.error == nil
            let title: String
            if isCompactIcon {
                title = ""
            } else if isSingleDefault {
                switch hub.status {
                case "connected": title = hub.user ?? "Connected"
                case "connecting": title = "Connecting…"
                case "auth_required": title = "Sign In"
                default: title = "Offline"
                }
            } else {
                title = hub.name
            }
            let width: CGFloat = isCompactIcon ? 44 : min(180, measureTextWidth(title, font: btnFont) + 26)
            pillX -= width
            let pill = MenuControl(title, frame: NSRect(x: pillX, y: 0, width: width, height: 22), radius: 11)
            pill.identifier = NSUserInterfaceItemIdentifier("hub:\(hub.name)")
            if isCompactIcon {
                pill.label.isHidden = true
                pill.addSubview(roundedView(NSRect(x: 7, y: 8, width: 6, height: 6), radius: 3, color: color))
                let net = NSImageView(frame: NSRect(x: 16, y: 5, width: 12, height: 12))
                net.image = NSImage(systemSymbolName: "network", accessibilityDescription: "Connected")
                net.contentTintColor = .labelColor; pill.addSubview(net)
                let chev = NSImageView(frame: NSRect(x: 31, y: 7, width: 8, height: 8))
                chev.image = NSImage(systemSymbolName: "chevron.down", accessibilityDescription: nil)
                chev.contentTintColor = .secondaryLabelColor; pill.addSubview(chev)
                pill.toolTip = "Connected · \(hub.user ?? "local")"
            } else {
                pill.label.font = btnFont
                pill.label.frame.origin.x = 16; pill.label.frame.size.width = width - 20
                pill.addSubview(roundedView(NSRect(x: 7, y: 8, width: 6, height: 6), radius: 3, color: color))
                pill.toolTip = isSingleDefault ? (hub.status == "connected" ? "Connected · \(hub.user ?? "local")" : title) : "\(hub.name) · \(hub.status)"
            }
            pill.pressed = { [weak self, weak menu] in
                guard let self = self, let drawer = drawerItems[hub.name] else { return }
                drawer.isHidden = !drawer.isHidden
                if drawer.isHidden { self.selectedHubs.remove(hub.name) }
                else { self.selectedHubs.insert(hub.name) }
                menu?.update()
            }
            pillContainer.addSubview(pill); pillX -= 4
        }
        if pillX < 0 {
            for pill in pillContainer.subviews { pill.frame.origin.x -= pillX }
            pillContainer.frame.size.width = 230 - pillX
        }
        for listener in listeners {
            let aboutText = (listener.about ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            let hasDistinctAbout = !aboutText.isEmpty && aboutText.caseInsensitiveCompare(listener.title.trimmingCharacters(in: .whitespacesAndNewlines)) != .orderedSame
            let rowHeight: CGFloat = hasDistinctAbout ? 78 : 64
            let cardHeight: CGFloat = hasDistinctAbout ? 72 : 58
            let row = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: rowHeight))
            let card = roundedView(NSRect(x: 10, y: 3, width: 360, height: cardHeight), radius: 10,
                color: .labelColor.withAlphaComponent(0.045))
            row.addSubview(card)
            _ = host(row, id: "session:\(listener.scoped_id)")
            let symbol: String, tint: NSColor, harnessName: String
            switch listener.harness {
            case "pi": symbol = "terminal.fill"; tint = .systemIndigo; harnessName = "Pi"
            case "codex": symbol = "sparkles"; tint = .systemTeal; harnessName = "Codex"
            case "claude": symbol = "cpu.fill"; tint = .systemOrange; harnessName = "Claude"
            default: symbol = "bolt.horizontal.fill"; tint = .secondaryLabelColor; harnessName = listener.harness.capitalized
            }
            let badge = roundedView(NSRect(x: 10, y: cardHeight - 44, width: 34, height: 34), radius: 8, color: tint.withAlphaComponent(0.13))
            let icon = NSImageView(frame: NSRect(x: 8, y: 8, width: 18, height: 18))
            icon.image = NSImage(systemSymbolName: symbol, accessibilityDescription: harnessName); icon.contentTintColor = tint
            badge.addSubview(icon); card.addSubview(badge)
            let actionWidth: CGFloat = listener.can_jump ? 114 : 88
            let title = textLabel(listener.title + (hubs.count > 1 ? " · \(listener.hub)" : ""),
                NSRect(x: 54, y: cardHeight - 26, width: 350 - 54 - actionWidth - 6, height: 18), size: 13, weight: .semibold)
            title.toolTip = listener.title; card.addSubview(title)
            if hasDistinctAbout {
                let aboutLabel = textLabel(aboutText, NSRect(x: 54, y: 28, width: 296, height: 16), size: 11.5, color: .secondaryLabelColor)
                aboutLabel.toolTip = aboutText; card.addSubview(aboutLabel)
            }
            let raw = listener.public_id, split = raw.lastIndex(of: "-")
            let short: String
            if let split = split {
                let slug = String(raw[..<split]), fp4 = String(raw[raw.index(after: split)...].prefix(4))
                let cleanSlug = slug.count > 18 ? String(slug.prefix(8)) + "…" + String(slug.suffix(7)) : slug
                short = "\(cleanSlug)#\(fp4)"
            } else { short = raw }
            let copyFont = NSFont.monospacedSystemFont(ofSize: 11, weight: .medium)
            let copyWidth = max(measureTextWidth("✓ Copied", font: copyFont), measureTextWidth(short, font: copyFont)) + 24
            let copy = MenuControl(short, frame: NSRect(x: 54, y: 7, width: copyWidth, height: 19), leadingSymbol: "doc.on.doc")
            copy.label.font = copyFont
            copy.toolTip = listener.scoped_id; copy.identifier = NSUserInterfaceItemIdentifier("copy:\(listener.scoped_id)")
            copy.pressed = { [weak copy] in
                NSPasteboard.general.clearContents(); NSPasteboard.general.setString(listener.scoped_id, forType: .string)
                copy?.copied()
            }
            card.addSubview(copy)
            let home = NSHomeDirectory()
            let cwd = listener.cwd == home ? "~" : listener.cwd.hasPrefix(home + "/") ? "~" + listener.cwd.dropFirst(home.count) : listener.cwd
            let branch = (listener.branch ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            let pathText = (!branch.isEmpty && branch != "HEAD") ? "\(cwd) · \(branch)" : cwd
            let path = textLabel(pathText,
                NSRect(x: 54 + copyWidth + 8, y: 8, width: max(0, 296 - copyWidth - 8), height: 16), size: 11, color: .tertiaryLabelColor)
            path.lineBreakMode = .byTruncatingMiddle; path.toolTip = listener.cwd; card.addSubview(path)
            var x: CGFloat = 350 - actionWidth
            var scope = listener.scope
            let toggle = MenuControl(scope == "self" ? "Self" : "Team", frame: NSRect(x: x, y: cardHeight - 28, width: 62, height: 22),
                leadingSymbol: scope == "self" ? "lock.fill" : "person.2.fill")
            toggle.identifier = NSUserInterfaceItemIdentifier("scope:\(listener.scoped_id)")
            let scopeColor = { [weak toggle] in
                toggle?.normalBackground = scope == "workspace" ? NSColor.systemBlue.withAlphaComponent(0.15) : NSColor.labelColor.withAlphaComponent(0.06)
                toggle?.layer?.backgroundColor = toggle?.normalBackground.cgColor
            }
            scopeColor()
            toggle.pressed = { [weak toggle] in
                scope = scope == "self" ? "workspace" : "self"
                toggle?.setLeadingSymbol(scope == "self" ? "lock.fill" : "person.2.fill")
                toggle?.label.stringValue = scope == "self" ? "Self" : "Team"
                toggle?.setAccessibilityLabel(toggle?.label.stringValue); scopeColor()
                self.emit(["op": "set_scope", "hub": listener.hub, "listener_id": listener.listener_id, "scope": scope])
            }
            card.addSubview(toggle); x += 66
            if listener.can_jump {
                let open = MenuControl("Open in Codex", frame: NSRect(x: x, y: cardHeight - 28, width: 22, height: 22), symbol: "arrow.up.forward.app")
                open.toolTip = "Open in Codex"; open.identifier = NSUserInterfaceItemIdentifier("open:\(listener.scoped_id)")
                let run = { [weak menu] in self.openCodex(listener.session_id); menu?.cancelTracking() }
                open.pressed = run; register(run); card.addSubview(open); x += 26
            }
            register { [weak copy] in copy?.pressed?() }
            register { [weak toggle] in toggle?.pressed?() }
            let stop = MenuControl("Stop listening", frame: NSRect(x: x, y: cardHeight - 28, width: 22, height: 22), symbol: "xmark")
            stop.toolTip = "Stop listening"; stop.hoverColor = .systemRed
            stop.identifier = NSUserInterfaceItemIdentifier("stop:\(listener.scoped_id)")
            let unlisten = { self.emit(["op": "unlisten", "hub": listener.hub, "listener_id": listener.listener_id]) }
            stop.pressed = unlisten; card.addSubview(stop); register(unlisten); register(unlisten)
        }
        if listeners.isEmpty {
            let row = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 70))
            let card = roundedView(NSRect(x: 10, y: 3, width: 360, height: 64), radius: 10, color: .labelColor.withAlphaComponent(0.045))
            let icon = NSImageView(frame: NSRect(x: 12, y: 22, width: 24, height: 24))
            icon.image = NSImage(systemSymbolName: "waveform.slash", accessibilityDescription: "No active listeners")
            icon.contentTintColor = .secondaryLabelColor; card.addSubview(icon)
            card.addSubview(textLabel("No active listeners", NSRect(x: 44, y: 33, width: 304, height: 18), size: 13, weight: .semibold))
            card.addSubview(textLabel("Call cito_listen in a session to receive notifications",
                NSRect(x: 44, y: 12, width: 304, height: 16), size: 11, color: .secondaryLabelColor))
            row.addSubview(card); _ = host(row, id: "empty")
        }
        let footer = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 36))
        if !listeners.isEmpty {
            let stopAll = MenuControl("Stop All", frame: NSRect(x: 12, y: 6, width: 82, height: 24), leadingSymbol: "stop.fill")
            stopAll.identifier = NSUserInterfaceItemIdentifier("stop_all")
            stopAll.pressed = { self.emit(["op": "unlisten_all"]) }
            footer.addSubview(stopAll); register { [weak stopAll] in stopAll?.pressed?() }
        }
        hubActions.forEach { register($0) }
        let quit = MenuControl("Quit Cito ⌘Q", frame: NSRect(x: 262, y: 6, width: 106, height: 24))
        quit.identifier = NSUserInterfaceItemIdentifier("quit")
        quit.pressed = { self.emit(["op": "quit"]) }
        footer.addSubview(quit); register { [weak quit] in quit?.pressed?() }
        let footerItem = host(footer, id: "footer")
        footerItem.target = self; footerItem.action = #selector(quitMenu)
        footerItem.keyEquivalent = "q"; footerItem.keyEquivalentModifierMask = [.command]
        status?.menu = menu
    }
    @objc func quitMenu() { emit(["op": "quit"]) }
    // Headless inspection projects real AppKit objects; it does not synthesize a parallel menu model.
    func inspectMenu() {
        func project(_ view: NSView) -> [String: Any] {
            var result: [String: Any] = ["id": view.identifier?.rawValue ?? "", "width": view.frame.width,
                "height": view.frame.height, "x": view.frame.origin.x, "y": view.frame.origin.y,
                "radius": view.layer?.cornerRadius ?? 0, "hidden": view.isHidden,
                "children": view.subviews.map { project($0) }]
            if let label = view as? NSTextField { result["text"] = label.stringValue }
            if let control = view as? MenuControl {
                result["text"] = control.label.stringValue; result["tooltip"] = control.toolTip ?? ""
                result["green"] = control.label.textColor == NSColor.systemGreen
                if let symbol = control.symbolName { result["symbol"] = symbol }
            }
            return result
        }
        let items = (cardMenu?.items ?? []).compactMap { item -> [String: Any]? in
            guard let view = item.view else { return nil }
            var value = project(view); value["hidden"] = item.isHidden; return value
        }
        let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: 36, pixelsHigh: 36, bitsPerSample: 8,
            samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)
        NSGraphicsContext.current?.cgContext.scaleBy(x: 2, y: 2)
        statusImage?.draw(in: NSRect(x: 0, y: 0, width: 18, height: 18))
        NSGraphicsContext.restoreGraphicsState()
        func alpha(_ x: CGFloat, _ y: CGFloat) -> CGFloat {
            // Bitmap rows start at the top; drawing coordinates start at the bottom.
            return bitmap.colorAt(x: Int(x * 2), y: 35 - Int(y * 2))?.alphaComponent ?? 0
        }
        let probes: [String: CGFloat] = ["arc": alpha(2.5, 9), "core": alpha(8, 9),
            "wave": alpha(15.75, 9), "arc_gap": alpha(13.5, 9), "warning_dot": alpha(14.2, 4),
            "warning_ring": alpha(11.3, 4), "outside_cutout": alpha(10.2, 4), "inside_dot": alpha(16, 4)]
        let button = status?.button ?? headlessStatusButton
        let font = button.flatMap { $0.attributedTitle.length > 0 ? $0.attributedTitle.attribute(.font, at: 0, effectiveRange: nil) as? NSFont : nil }
        let traits = font?.fontDescriptor.object(forKey: .traits) as? [NSFontDescriptor.TraitKey: Any]
        let snapshot: [String: Any] = ["items": items, "icon_alpha": probes, "actions": menuActions.count, "count": uniqueSessionCount,
            "icon_template": statusImage?.isTemplate ?? false, "icon_width": statusImage?.size.width ?? 0,
            "icon_height": statusImage?.size.height ?? 0, "clipboard": NSPasteboard.general.string(forType: .string) ?? "",
            "button_title": button?.title ?? "", "button_image_position": button?.imagePosition.rawValue ?? -1,
            "button_font_size": font?.pointSize ?? 0, "button_font_weight": traits?[.weight] as? CGFloat ?? 0,
            "button_font_features": (font?.fontDescriptor.object(forKey: .featureSettings) as? [[NSFontDescriptor.FeatureKey: Any]] ?? []).map { feature in
                ["type": feature[.typeIdentifier] as? Int ?? -1, "selector": feature[.selectorIdentifier] as? Int ?? -1]
            },
            "button_digit_widths": font.map { font in ["11111111", "88888888"].map { ($0 as NSString).size(withAttributes: [.font: font]).width } } ?? []]
        diagnostic("menu " + String(data: try! JSONSerialization.data(withJSONObject: snapshot, options: [.sortedKeys]), encoding: .utf8)!)
    }
    func hide(clicked: Bool) {
        timer?.cancel()
        if clicked, visible, let value = payload, value.target_harness == "codex" { openCodex(value.target_session_id) }
        visible = false; panel?.orderOut(nil)
        if headless { diagnostic(clicked ? "banner clicked" : "banner hidden") }
    }
    func show(_ value: Payload) {
        let title = "Cito · \(value.target_title)" + (displayedCount > 1 ? " (\(displayedCount))" : "")
        let semanticLabel = (value.effective_wake ? "Notification" : "Passive") + (displayedCount > 1 ? " · ×\(displayedCount)" : "")
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
