import AppKit
import Foundation
import Darwin
import QuartzCore

// Headless tests use a real private pasteboard without changing the user clipboard.
let usesPrivatePasteboard = ProcessInfo.processInfo.environment["CITO_UI_HEADLESS"] == "1" || ProcessInfo.processInfo.environment["CITO_BANNER_HEADLESS"] == "1"
let appPasteboard = usesPrivatePasteboard ? NSPasteboard.withUniqueName() : NSPasteboard.general
atexit { if usesPrivatePasteboard { appPasteboard.releaseGlobally() } }

enum StatusIconState { case active, idle, warning }
func makeCitoStatusIcon(_ state: StatusIconState) -> NSImage {
    let image = NSImage(size: NSSize(width: 20, height: 20), flipped: false) { _ in
        let center = NSPoint(x: 9.2, y: 10.0)
        let arc = NSBezierPath()
        arc.appendArc(withCenter: center, radius: 7.45, startAngle: 42, endAngle: 318, clockwise: false)
        arc.lineWidth = 2.15; arc.lineCapStyle = .round
        NSColor.black.setStroke(); arc.stroke()
        NSColor.black.withAlphaComponent(state == .active ? 1 : 0.40).setFill()
        NSBezierPath(ovalIn: NSRect(x: 7.05, y: 7.85, width: 4.3, height: 4.3)).fill()
        let wave = NSBezierPath()
        wave.appendArc(withCenter: center, radius: 9.55, startAngle: -28, endAngle: 28, clockwise: false)
        wave.lineWidth = 2.00; wave.lineCapStyle = .round
        NSColor.black.withAlphaComponent(state == .active ? 1 : 0.40).setStroke(); wave.stroke()
        if state == .warning, let context = NSGraphicsContext.current?.cgContext {
            context.saveGState(); context.setBlendMode(.destinationOut)
            NSColor.black.setFill()
            NSBezierPath(ovalIn: NSRect(x: 11.8, y: 0.4, width: 7.8, height: 7.8)).fill()
            context.setBlendMode(.normal)
            NSBezierPath(ovalIn: NSRect(x: 13.1, y: 1.7, width: 5.2, height: 5.2)).fill()
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
extension NSColor {
    // Applying alpha eagerly snapshots semantic RGB values; defer it until appearance resolution.
    func withAppearanceAlpha(_ alpha: CGFloat) -> NSColor {
        NSColor(name: nil) { appearance in
            var resolved = self
            appearance.performAsCurrentDrawingAppearance { resolved = self.withAlphaComponent(alpha) }
            return resolved
        }
    }
}

final class SurfaceView: NSView {
    var fillColor: NSColor { didSet { needsDisplay = true; applyLayerColors() } }
    var strokeColor: NSColor? { didSet { needsDisplay = true; applyLayerColors() } }
    var cornerRadiusValue: CGFloat { didSet { layer?.cornerRadius = cornerRadiusValue; needsDisplay = true } }
    override var wantsUpdateLayer: Bool { true }

    init(frame: NSRect, radius: CGFloat, color: NSColor, border: NSColor? = nil) {
        fillColor = color; strokeColor = border; cornerRadiusValue = radius
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = radius
        layer?.cornerCurve = .continuous
        applyLayerColors()
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }
    func applyLayerColors() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.backgroundColor = fillColor.cgColor
            layer?.borderColor = strokeColor?.cgColor
            layer?.borderWidth = strokeColor == nil ? 0 : 1
        }
    }
    override func updateLayer() { applyLayerColors() }
    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applyLayerColors()
    }
}
func roundedView(_ frame: NSRect, radius: CGFloat, color: NSColor, border: NSColor? = nil) -> SurfaceView {
    SurfaceView(frame: frame, radius: radius, color: color, border: border)
}
func measureTextWidth(_ text: String, font: NSFont) -> CGFloat {
    let field = NSTextField(labelWithString: text)
    field.font = font
    return ceil(field.fittingSize.width)
}
func shortPublicId(_ raw: String) -> String {
    guard let split = raw.lastIndex(of: "-") else { return raw }
    let slug = String(raw[..<split]), fp4 = String(raw[raw.index(after: split)...].prefix(4))
    let cleanSlug = slug.count > 18 ? String(slug.prefix(8)) + "…" + String(slug.suffix(7)) : slug
    return "\(cleanSlug)#\(fp4)"
}
struct HarnessMeta: Decodable, Equatable {
    let id: String
    let display_name: String
    let sf_symbol: String
    let tint: String
    let tier: String
    let order: Int
}
struct HarnessVisual {
    let id: String
    let name: String
    let symbol: String
    let tint: NSColor
    let tier: String
    let order: Int
}
func tintColor(_ name: String) -> NSColor {
    switch name.lowercased() {
    case "indigo": return .systemIndigo
    case "teal": return .systemTeal
    case "orange", "amber", "yellow": return .systemOrange
    case "blue": return .systemBlue
    case "purple": return .systemPurple
    case "green": return .systemGreen
    case "pink": return .systemPink
    case "gray": return .systemGray
    default: return .systemBlue
    }
}
func resolveHarnessVisual(_ harness: String, harnesses: [String: HarnessMeta]) -> HarnessVisual {
    if let meta = harnesses[harness] {
        return HarnessVisual(
            id: meta.id,
            name: meta.display_name,
            symbol: meta.sf_symbol,
            tint: tintColor(meta.tint),
            tier: meta.tier,
            order: meta.order
        )
    }
    if harness == "human" {
        return HarnessVisual(
            id: "human",
            name: "Human",
            symbol: "person.crop.circle.fill",
            tint: .systemGreen,
            tier: "stable",
            order: -1
        )
    }
    return HarnessVisual(
        id: harness,
        name: harness.capitalized,
        symbol: "bolt.horizontal.fill",
        tint: .systemBlue,
        tier: "custom",
        order: 3
    )
}
func loadStartupHarnesses() -> [HarnessMeta] {
    let execUrl = URL(fileURLWithPath: CommandLine.arguments[0]).resolvingSymlinksInPath()
    let fileUrl = execUrl.deletingLastPathComponent().appendingPathComponent("harnesses.json")
    guard let data = try? Data(contentsOf: fileUrl),
          let items = try? JSONDecoder().decode([HarnessMeta].self, from: data) else {
        return []
    }
    return items
}

func makeHarnessBadge(_ frame: NSRect, radius: CGFloat, iconInset: CGFloat, harness: String, harnesses: [String: HarnessMeta]) -> SurfaceView {
    let visual = resolveHarnessVisual(harness, harnesses: harnesses)
    let badge = roundedView(frame, radius: radius, color: visual.tint.withAppearanceAlpha(0.88),
        border: .white.withAlphaComponent(0.18))
    let icon = NSImageView(frame: badge.bounds.insetBy(dx: iconInset, dy: iconInset))
    icon.image = NSImage(systemSymbolName: visual.symbol, accessibilityDescription: visual.name)
    icon.contentTintColor = .white
    badge.addSubview(icon)
    return badge
}

final class KeyPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}

/** Menu controls own their clicks so copy and scope changes keep native tracking open. */
final class MenuControl: NSControl {
    let label: NSTextField
    enum Style { case neutral, prominent, subtleHeader, destructiveHover }
    var leadingSymbolView: NSImageView?
    var centerSymbolView: NSImageView?
    var accessorySymbolView: NSImageView? { didSet { applyVisualState() } }
    var symbolName: String?
    var pressed: (() -> Void)?
    var style: Style = .neutral { didSet { applyVisualState() } }
    var normalColor: NSColor = .labelColor { didSet { applyVisualState() } }
    var feedbackTimer: Timer?
    private(set) var isHovered = false
    private var trackingArea: NSTrackingArea?
    static weak var activeHovered: MenuControl?
    private static var hoverWatchTimer: Timer?
    // Headless hooks exercise logical transitions without a visible window or physical pointer.
    static var tracksPhysicalPointer = true
    override var wantsUpdateLayer: Bool { true }
    init(_ title: String, frame: NSRect, symbol: String? = nil, leadingSymbol: String? = nil, color: NSColor = .labelColor, radius: CGFloat = 6) {
        let hasLeading = leadingSymbol != nil
        label = textLabel(title, NSRect(x: hasLeading ? 20 : 3, y: (frame.height - 15) / 2, width: frame.width - (hasLeading ? 24 : 6), height: 15))
        symbolName = leadingSymbol ?? symbol
        super.init(frame: frame)
        wantsLayer = true; layer?.cornerRadius = radius
        layer?.cornerCurve = .continuous
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
            image.contentTintColor = color; addSubview(image); centerSymbolView = image; label.isHidden = true
        }
        setAccessibilityElement(true); setAccessibilityRole(.button); setAccessibilityLabel(title)
        applyVisualState()
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }
    func setLeadingSymbol(_ name: String?) {
        symbolName = name
        if let name = name {
            if leadingSymbolView == nil {
                let image = NSImageView(frame: NSRect(x: 6, y: (bounds.height - 12) / 2, width: 12, height: 12))
                image.contentTintColor = normalColor
                addSubview(image)
                leadingSymbolView = image
            }
            leadingSymbolView?.frame = NSRect(x: 6, y: (bounds.height - 12) / 2, width: 12, height: 12)
            leadingSymbolView?.image = NSImage(systemSymbolName: name, accessibilityDescription: nil)
            leadingSymbolView?.isHidden = false
            label.frame = NSRect(x: 20, y: (bounds.height - 15) / 2, width: bounds.width - 24, height: 15)
        } else {
            leadingSymbolView?.isHidden = true
            label.frame = NSRect(x: 3, y: (bounds.height - 15) / 2, width: bounds.width - 6, height: 15)
        }
        applyVisualState()
    }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? { frame.contains(point) ? self : nil }
    override func mouseDown(with event: NSEvent) { pressed?() }
    override func accessibilityPerformPress() -> Bool { pressed?(); return true }
    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if trackingArea == nil {
            let area = NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self)
            trackingArea = area
            addTrackingArea(area)
        }
        if isHovered { MenuControl.reconcileActiveHover() }
    }
    override func mouseEntered(with event: NSEvent) { setHovered(true) }
    override func mouseExited(with event: NSEvent) { setHovered(false) }
    func setHovered(_ hovered: Bool) {
        if hovered {
            if let previous = MenuControl.activeHovered, previous !== self { previous.setHovered(false) }
            MenuControl.activeHovered = self
        } else if MenuControl.activeHovered === self {
            MenuControl.activeHovered = nil
        }
        isHovered = hovered
        applyVisualState()
        if MenuControl.activeHovered != nil { MenuControl.startHoverWatchTimer() }
        else { MenuControl.stopHoverWatchTimerIfIdle() }
    }
    static func clearHoverWhere(_ predicate: (MenuControl) -> Bool) {
        if let control = activeHovered, predicate(control) { control.setHovered(false) }
        stopHoverWatchTimerIfIdle()
    }
    static func reconcileActiveHover() {
        guard let control = activeHovered else { stopHoverWatchTimerIfIdle(); return }
        guard tracksPhysicalPointer else { return }
        guard let window = control.window, window.isVisible, !control.isHiddenOrHasHiddenAncestor,
              control.enclosingMenuItem?.isHidden != true else { control.setHovered(false); return }
        let windowPoint = window.convertPoint(fromScreen: NSEvent.mouseLocation)
        let localPoint = control.convert(windowPoint, from: nil)
        if !control.visibleRect.contains(localPoint) { control.setHovered(false) }
    }
    private static func startHoverWatchTimer() {
        guard hoverWatchTimer == nil else { return }
        let timer = Timer(timeInterval: 0.08, repeats: true) { _ in reconcileActiveHover() }
        hoverWatchTimer = timer
        RunLoop.main.add(timer, forMode: .common)
        RunLoop.main.add(timer, forMode: .eventTracking)
    }
    private static func stopHoverWatchTimerIfIdle() {
        guard activeHovered == nil else { return }
        hoverWatchTimer?.invalidate()
        hoverWatchTimer = nil
    }
    func applyVisualState() {
        let fill: NSColor
        let border: NSColor?
        let foreground: NSColor
        let primarySymbol: NSColor
        let accessorySymbol: NSColor
        switch style {
        case .neutral:
            fill = .labelColor.withAppearanceAlpha(isHovered ? 0.18 : 0.09)
            border = .labelColor.withAppearanceAlpha(isHovered ? 0.28 : 0.14)
            foreground = isHovered ? .labelColor : normalColor
            primarySymbol = foreground
            accessorySymbol = isHovered ? .labelColor : .secondaryLabelColor
        case .prominent:
            fill = isHovered ? .systemBlue : .systemBlue.withAppearanceAlpha(0.88)
            border = .white.withAlphaComponent(isHovered ? 0.36 : 0.22)
            foreground = .white; primarySymbol = .white; accessorySymbol = .white
        case .subtleHeader:
            fill = isHovered ? .labelColor.withAppearanceAlpha(0.10) : .clear
            border = nil
            foreground = isHovered ? .labelColor : normalColor
            primarySymbol = isHovered ? .labelColor : .secondaryLabelColor
            accessorySymbol = primarySymbol
        case .destructiveHover:
            fill = isHovered ? .systemRed.withAppearanceAlpha(0.88) : .labelColor.withAppearanceAlpha(0.09)
            border = isHovered ? .white.withAlphaComponent(0.26) : .labelColor.withAppearanceAlpha(0.14)
            foreground = isHovered ? .white : .secondaryLabelColor
            primarySymbol = foreground; accessorySymbol = foreground
        }
        let isCopied = feedbackTimer?.isValid == true
        label.textColor = isCopied ? .systemGreen : foreground
        leadingSymbolView?.contentTintColor = primarySymbol
        centerSymbolView?.contentTintColor = primarySymbol
        accessorySymbolView?.contentTintColor = accessorySymbol
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.backgroundColor = (isCopied ? NSColor.systemGreen.withAppearanceAlpha(0.16) : fill).cgColor
            layer?.borderColor = (isCopied ? NSColor.systemGreen.withAppearanceAlpha(0.36) : border)?.cgColor
            layer?.borderWidth = isCopied || border != nil ? 1 : 0
        }
    }
    override func updateLayer() { applyVisualState() }
    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applyVisualState()
    }
    func copied(_ message: String = "✓ Copied") {
        feedbackTimer?.invalidate()
        leadingSymbolView?.isHidden = true
        label.frame = NSRect(x: 3, y: (bounds.height - 15) / 2, width: bounds.width - 6, height: 15)
        label.stringValue = message; label.textColor = .systemGreen
        let restore = Timer(timeInterval: 1.2, repeats: false) { [weak self] _ in
            guard let self = self else { return }
            if self.leadingSymbolView != nil {
                self.leadingSymbolView?.isHidden = false
                self.label.frame = NSRect(x: 20, y: (self.bounds.height - 15) / 2, width: self.bounds.width - 24, height: 15)
            }
            self.feedbackTimer = nil
            self.label.stringValue = self.accessibilityLabel() ?? ""
            self.applyVisualState()
        }
        feedbackTimer = restore
        applyVisualState()
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
    let target_cwd: String?
    let signal_id: String?
    let hub: String?
    let from_scoped_id: String?

    var subtitle: String {
        (effective_wake ? "" : "[Passive] ") + "\(from_user)@\(from_host) · \(from_title)"
    }
    var displaySubtitle: String {
        let cleanHost = from_host.replacingOccurrences(of: "\\.local$", with: "", options: [.regularExpression, .caseInsensitive])
        let title = from_title.trimmingCharacters(in: .whitespacesAndNewlines)
        return (effective_wake ? "" : "[Passive] ") + "\(from_user)@\(cleanHost.isEmpty ? from_host : cleanHost)" + (title.isEmpty ? "" : " · \(title)")
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

final class FlippedView: NSView {
    override var isFlipped: Bool { true }
}

struct Hub: Decodable, Equatable {
    let name: String, status: String, auth_mode: String
    let user: String?, error: String?
}
struct Listener: Decodable, Equatable {
    let hub: String, listener_id: String, public_id: String, scoped_id: String
    let harness: String, session_id: String, title: String, cwd: String, scope: String
    let about: String?, repo: String?, branch: String?
    let channels: [String]?
}
struct Candidate: Decodable, Equatable {
    let harness: String, session_id: String, title: String, cwd: String
    let branch: String?, endpoint_pid: Int?, updated_at_ms: Double?
    let listening_hubs: [String]?
}
struct RemotePeer: Decodable, Equatable {
    let hub: String, listener_id: String, public_id: String, scoped_id: String
    let user: String, host_id: String, host_name: String, harness: String
    let title: String, about: String, scope: String, last_seen_at: String
    let cwd: String?, repo: String?, branch: String?
    let is_self_principal: Bool?
    let channels: [String]?
}
struct ActivityItem: Decodable, Equatable {
    let hub: String
    let signal_id: String
    let listener_id: String
    let from_user: String
    let from_host_name: String
    let from_harness: String?
    let from_title: String
    let from_scoped_id: String?
    let target_harness: String
    let target_title: String
    let target_scoped_id: String
    let message: String
    let held_reason: String?
    let in_reply_to: String?
    let created_at: String
    let unread: Bool
    let for_me: Bool
}
struct ChannelFieldItem: Decodable, Equatable {
    let key: String
    let scope: String
    let label: String
    let type: String
    let required: Bool
    let placeholder: String
    let value: String?
    let configured: Bool
    let hidden: Bool?
    let redacted: Bool?
    let configured_placeholder: String?
    let help_text: String?
}
struct ChannelSetupItem: Decodable, Equatable {
    let id: String
    let display_name: String
    let sf_symbol: String
    let tint: String
    let enabled: Bool
    let configured: Bool
    let status: String
    let detail: String
    let display_label: String?
    let hub_provided: Bool
    let pairing_supported: Bool
    let pairing_action_label: String?
    let pairing_code: String?
    let pairing_instruction: String?
    let fields: [ChannelFieldItem]
    let setup_guide: [String]?
    let pairing_help_text: String?
}
struct AppSetupItem: Decodable, Equatable {
    let id: String
    let display_name: String
    let tier: String
    let installed: Bool
    let configured: Bool
    let status: String
    let detail: String
    let paths: [String]
}
struct HostHubSummary: Decodable, Equatable {
    let enabled: Bool
    let running: Bool
    let port: Int
    let endpoint_url: String
    let token: String
    let invite_command: String
    let auth_mode: String?
    let error: String?
}
struct HubConfigItem: Decodable, Equatable {
    let name: String
    let url: String
    let auth_mode: String
    let enabled: Bool
    let is_default: Bool
    let status: String
    let user: String?
    let error: String?
    let issuer: String?
    let client_id: String?
}
struct SettingsPayload: Decodable, Equatable {
    let apps: [AppSetupItem]
    let channel_hub: String?
    let settings_hub: String?
    let channels: [ChannelSetupItem]?
    let desktop_banner_enabled: Bool?
    let hubs: [HubConfigItem]
    let host_hub: HostHubSummary
    let launch_at_login: Bool
}
struct Frame: Decodable {
    let op: String
    let hub: String?
    let ok: Bool?
    let error: String?
    let composer_error: String?
    let channel_id: String?
    let code: String?
    let instruction: String?
    let detail: String?
    let hubs: [Hub]?
    let listeners: [Listener]?
    let available: [Candidate]?
    let my_remote_listeners: [RemotePeer]?
    let directory_online_count: Int?
    let unread_count: Int?
    let activity: [ActivityItem]?
    let harnesses: [HarnessMeta]?
    let settings: SettingsPayload?
    let peers: [RemotePeer]?
    let matched_count: Int?
    let online_count: Int?
    let payload: Payload?
}

final class DirectoryPeerCellView: NSTableCellView {
    let badgeView: SurfaceView
    var inspectionId = ""
    private var isSelectedRow = false
    override var wantsUpdateLayer: Bool { true }
    let badgeIcon: NSImageView
    let line1TitleLabel: NSTextField
    let line1RepoLabel: NSTextField
    let line2UserLabel: NSTextField
    let line2MetaLabel: NSTextField
    let msgControl: MenuControl
    let copyControl: MenuControl

    override init(frame frameRect: NSRect) {
        badgeView = roundedView(NSRect(x: 14, y: 12, width: 28, height: 28), radius: 7, color: .clear)
        badgeIcon = NSImageView(frame: NSRect(x: 6, y: 6, width: 16, height: 16))
        badgeView.addSubview(badgeIcon)
        line1TitleLabel = textLabel("", NSRect(x: 52, y: 26, width: 180, height: 18), size: 13, weight: .semibold, color: .labelColor)
        line1RepoLabel = textLabel("", NSRect(x: 232, y: 26, width: 180, height: 18), size: 11.5, weight: .regular, color: .secondaryLabelColor)
        line2UserLabel = textLabel("", NSRect(x: 52, y: 7, width: 80, height: 16), size: 11.5, weight: .medium, color: .labelColor)
        line2MetaLabel = textLabel("", NSRect(x: 132, y: 7, width: 220, height: 16), size: 11.5, weight: .regular, color: .secondaryLabelColor)
        msgControl = MenuControl("Message", frame: NSRect(x: 356, y: 16, width: 78, height: 22), leadingSymbol: "bubble.left")
        copyControl = MenuControl("", frame: NSRect(x: 440, y: 16, width: 104, height: 22), leadingSymbol: "doc.on.doc")
        super.init(frame: frameRect)
        identifier = NSUserInterfaceItemIdentifier("DirectoryPeerCell")
        wantsLayer = true
        addSubview(badgeView)
        addSubview(line1TitleLabel)
        addSubview(line1RepoLabel)
        addSubview(line2UserLabel)
        addSubview(line2MetaLabel)
        addSubview(msgControl)
        addSubview(copyControl)
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

    private func applySelectionColor() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.backgroundColor = (isSelectedRow ? NSColor.controlAccentColor.withAlphaComponent(0.22) : NSColor.clear).cgColor
        }
    }
    func setSelected(_ selected: Bool) {
        guard isSelectedRow != selected else { return }
        isSelectedRow = selected
        applySelectionColor()
    }
    override func updateLayer() { applySelectionColor() }
    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applySelectionColor()
    }

    func configure(with peer: RemotePeer, selected: Bool, harnesses: [String: HarnessMeta], onMessage: (() -> Void)? = nil) {
        inspectionId = "dir_peer:\(peer.scoped_id)"
        isSelectedRow = selected
        frame = NSRect(x: 0, y: 0, width: 560, height: 52)
        applySelectionColor()
        let visual = resolveHarnessVisual(peer.harness, harnesses: harnesses)
        badgeView.fillColor = visual.tint.withAppearanceAlpha(0.88)
        badgeView.strokeColor = .white.withAlphaComponent(0.18)
        badgeIcon.image = NSImage(systemSymbolName: visual.symbol, accessibilityDescription: visual.name)
        badgeIcon.contentTintColor = .white

        let short = shortPublicId(peer.public_id)
        let copyFont = NSFont.monospacedSystemFont(ofSize: 11, weight: .medium)
        let copyWidth = max(measureTextWidth("✓ Copied", font: copyFont), measureTextWidth(short, font: copyFont)) + 24
        copyControl.feedbackTimer?.invalidate()
        copyControl.feedbackTimer = nil
        copyControl.setHovered(false)
        copyControl.frame = NSRect(x: 544 - copyWidth, y: 16, width: copyWidth, height: 22)
        copyControl.setLeadingSymbol("doc.on.doc")
        copyControl.label.font = copyFont
        copyControl.label.stringValue = short
        copyControl.applyVisualState()
        copyControl.setAccessibilityLabel(short)
        copyControl.toolTip = peer.scoped_id
        copyControl.identifier = NSUserInterfaceItemIdentifier("copy_dir:\(peer.scoped_id)")
        copyControl.pressed = { [weak copyControl] in
            appPasteboard.clearContents()
            appPasteboard.setString(peer.scoped_id, forType: .string)
            copyControl?.copied()
        }

        let msgWidth: CGFloat = 78
        msgControl.isHidden = false
        msgControl.setHovered(false)
        msgControl.frame = NSRect(x: 544 - copyWidth - msgWidth - 6, y: 16, width: msgWidth, height: 22)
        msgControl.setLeadingSymbol("bubble.left")
        msgControl.label.font = .systemFont(ofSize: 11, weight: .medium)
        msgControl.label.stringValue = "Message"
        msgControl.setAccessibilityLabel("Message")
        msgControl.toolTip = "Message \(peer.scoped_id)"
        msgControl.identifier = NSUserInterfaceItemIdentifier("msg_dir:\(peer.scoped_id)")
        msgControl.pressed = onMessage

        let availWidth = max(120, 544 - copyWidth - msgWidth - 66)
        let titleFont = NSFont.systemFont(ofSize: 13, weight: .semibold)
        let titleW = min(availWidth, measureTextWidth(peer.title, font: titleFont) + 4)
        line1TitleLabel.stringValue = peer.title
        line1TitleLabel.frame = NSRect(x: 52, y: 26, width: titleW, height: 18)
        line1TitleLabel.identifier = NSUserInterfaceItemIdentifier("dir_title:\(peer.scoped_id)")

        let branch = (peer.branch ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        let branchSuffix = (!branch.isEmpty && branch != "HEAD") ? " · \(branch)" : ""
        let repoText = (peer.repo ?? "").isEmpty ? branchSuffix : " · \(peer.repo ?? "")\(branchSuffix)"
        line1RepoLabel.isHidden = false
        line1RepoLabel.stringValue = repoText
        line1RepoLabel.frame = NSRect(x: 52 + titleW, y: 26, width: max(0, availWidth - titleW), height: 18)
        line1RepoLabel.identifier = NSUserInterfaceItemIdentifier("dir_repo:\(peer.scoped_id)")

        let userText = "@\(peer.user)"
        let userFont = NSFont.systemFont(ofSize: 11.5, weight: .medium)
        let userW = min(availWidth, measureTextWidth(userText, font: userFont) + 2)
        line2UserLabel.isHidden = false
        line2UserLabel.stringValue = userText
        line2UserLabel.frame = NSRect(x: 52, y: 7, width: userW, height: 16)
        line2UserLabel.identifier = NSUserInterfaceItemIdentifier("dir_user:\(peer.scoped_id)")

        let cleanHost = peer.host_name.replacingOccurrences(of: "\\.local$", with: "", options: [.regularExpression, .caseInsensitive])
        let hostText = (peer.channels ?? []).isEmpty ? (cleanHost.isEmpty ? peer.host_name : cleanHost) : peer.channels!.joined(separator: ", ")
        line2MetaLabel.stringValue = "@\(hostText) · \(peer.about)"
        line2MetaLabel.frame = NSRect(x: 52 + userW, y: 7, width: max(0, availWidth - userW), height: 16)
        line2MetaLabel.identifier = NSUserInterfaceItemIdentifier("dir_meta:\(peer.scoped_id)")
    }

    func configureActivity(with item: ActivityItem, selected: Bool, harnesses: [String: HarnessMeta], onReply: @escaping () -> Void) {
        inspectionId = "activity_row:\(item.signal_id)"
        isSelectedRow = selected
        frame = NSRect(x: 0, y: 0, width: 560, height: 52)
        applySelectionColor()
        let visual = resolveHarnessVisual(item.from_harness ?? item.target_harness, harnesses: harnesses)
        badgeView.fillColor = visual.tint.withAppearanceAlpha(0.88)
        badgeView.strokeColor = .white.withAlphaComponent(0.18)
        badgeIcon.image = NSImage(systemSymbolName: visual.symbol, accessibilityDescription: visual.name)
        badgeIcon.contentTintColor = .white

        msgControl.isHidden = true
        let replyWidth: CGFloat = 72
        copyControl.feedbackTimer?.invalidate()
        copyControl.feedbackTimer = nil
        copyControl.setHovered(false)
        copyControl.frame = NSRect(x: 544 - replyWidth, y: 16, width: replyWidth, height: 22)
        copyControl.setLeadingSymbol("arrowshape.turn.up.left")
        copyControl.label.font = .systemFont(ofSize: 11, weight: .medium)
        copyControl.label.stringValue = "Reply"
        copyControl.applyVisualState()
        copyControl.setAccessibilityLabel("Reply")
        copyControl.toolTip = "Reply to \(item.from_scoped_id ?? item.from_user)"
        copyControl.identifier = NSUserInterfaceItemIdentifier("reply_activity:\(item.signal_id)")
        copyControl.pressed = onReply

        let availWidth: CGFloat = 544 - replyWidth - 60
        let unreadPrefix = item.unread ? "● " : ""
        let headerText = "\(unreadPrefix)@\(item.from_user) → \(item.target_title)"
        line1TitleLabel.stringValue = headerText
        line1TitleLabel.frame = NSRect(x: 52, y: 26, width: availWidth, height: 18)
        line1TitleLabel.identifier = NSUserInterfaceItemIdentifier("act_title:\(item.signal_id)")
        line1RepoLabel.isHidden = true
        line2UserLabel.isHidden = true

        let singleLineMsg = item.message.replacingOccurrences(of: "\\s*\\n+\\s*", with: "  ", options: .regularExpression)
        line2MetaLabel.stringValue = singleLineMsg
        line2MetaLabel.frame = NSRect(x: 52, y: 7, width: availWidth, height: 16)
        line2MetaLabel.identifier = NSUserInterfaceItemIdentifier("act_msg:\(item.signal_id)")
    }
}

final class Companion: NSObject, NSMenuDelegate, NSSearchFieldDelegate, NSTextFieldDelegate, NSTableViewDataSource, NSTableViewDelegate {
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
    var hoverEventMonitor: Any?
    let cardMenu = NSMenu()
    var selectedHubs = Set<String>()
    var statusImage: NSImage?
    var uniqueSessionCount: Int { Set(listeners.map { "\($0.harness):\($0.session_id)" }).count }
    var hubs: [Hub] = []
    var listeners: [Listener] = []
    var available: [Candidate] = []
    var myRemote: [RemotePeer] = []
    var harnesses: [HarnessMeta] = loadStartupHarnesses()
    var harnessMap: [String: HarnessMeta] { Dictionary(uniqueKeysWithValues: harnesses.map { ($0.id, $0) }) }
    var selectedTargetHub: String = ""
    var myRemoteCollapsed = false
    var collapsedGroups = Set<String>()
    func statusColor(_ status: String) -> NSColor {
        status == "connected" ? .systemGreen : status == "connecting" ? .systemOrange : status == "auth_required" ? .systemRed : .systemGray
    }
    var effectiveTargetHub: String {
        if !selectedTargetHub.isEmpty && hubs.contains(where: { $0.name == selectedTargetHub && $0.status != "disconnected" }) {
            return selectedTargetHub
        }
        if let firstUnmountedConnected = hubs.first(where: { hub in
            hub.status == "connected" && available.contains(where: { !($0.listening_hubs ?? []).contains(hub.name) })
        }) {
            return firstUnmountedConnected.name
        }
        return hubs.first(where: { $0.status == "connected" })?.name ?? hubs.first(where: { $0.status != "disconnected" })?.name ?? ""
    }
    var directoryOnlineCount = 0
    var directoryMatchedCount = 0
    var unreadCount = 0
    var directoryMode = "directory"
    var activityFilter = "me"
    var activityAllItems: [ActivityItem] = []
    var activityFilteredItems: [ActivityItem] = []
    var directoryAllPeers: [RemotePeer] = []
    var directoryFilteredPeers: [RemotePeer] = []
    var directoryVisible = false
    var directoryQuery = ""
    var directoryScopeFilter = "all"
    var directoryHarnessFilter = "all"
    var directorySelectedIndex = 0
    var directoryPanel: KeyPanel?
    var directorySearchField: NSSearchField?
    var directoryModeDirPill: MenuControl?
    var directoryModeActPill: MenuControl?
    var directoryTableView: NSTableView?
    var directoryScrollView: NSScrollView?
    var directorySummaryLabel: NSTextField?
    var directoryShortcutsLabel: NSTextField?
    var serverDirectoryQuery: String?
    var serverDirectoryScope = "all"
    var serverDirectoryHarness = "all"
    var serverDirectoryMatchedCount = 0
    var directoryFacetBar: NSView?
    var directoryDynamicHarnessViews: [NSView] = []
    var lastDirectoryFacetSignature = ""
    var directoryFacetControls: [String: MenuControl] = [:]
    var directoryComposerBar: NSView?
    var composerTargetPill: MenuControl?
    var composerInputField: NSTextField?
    var composerTarget = ""
    var composerTargetHub = ""
    var composerReplySignalId: String?
    var composerErrorLabel: NSTextField?
    var composerPendingText: String?
    var composerPendingTarget = ""
    var composerPendingHub = ""
    var settings: SettingsPayload?
    var settingsVisible = false
    var settingsActiveTab = "apps"
    var settingsPanel: KeyPanel?
    var addHubFormOpen = false
    var editingExistingHub = false
    var editingOriginalHub = ""
    var onboardingGuideOpen = false
    var nextStepCalloutDismissed = false
    var hostHubPortPending = false
    var hostHubPortSaved = false
    var hostHubPortError: String?
    var addHubName = ""
    var addHubUrl = ""
    var addHubAuth = "none"
    var addHubToken = ""
    var addHubUser = ""
    var addHubIssuer = ""
    var addHubClientId = ""
    var settingsFieldControls: [String: NSTextField] = [:]
    var channelFieldDrafts: [String: String] = [:]
    var channelSecretOriginal: [String: String] = [:]
    var selectedChannelHub = ""
    var channelSettingsByHub: [String: [ChannelSetupItem]] = [:]
    var channelInstructions: [String: String] = [:]
    var channelResults: [String: (ok: Bool, detail: String)] = [:]
    var settingsScrollOffsets: [String: CGFloat] = [:]
    var pendingOnboardingMarker: String?
    var settingsKeyMonitor: Any?
    var effectiveChannelHub: String {
        selectedChannelHub.isEmpty ? (settings?.hubs.first(where: { $0.is_default })?.name ?? hubs.first?.name ?? "local") : selectedChannelHub
    }
    func draftKey(_ fieldId: String) -> String { effectiveChannelHub + "|" + fieldId }
    func acceptSettings(_ value: SettingsPayload, hub: String? = nil) {
        settings = value
        if hostHubPortPending {
            hostHubPortPending = false
            hostHubPortSaved = value.host_hub.error == nil
            hostHubPortError = nil
        }
        let target = value.settings_hub ?? value.channel_hub ?? hub ?? effectiveChannelHub
        channelSettingsByHub[target] = value.channels ?? []
        for channel in value.channels ?? [] {
            for field in channel.fields {
                let key = target + "|channel_field:\(channel.id):\(field.key)"
                if channelFieldDrafts[key] == (field.value ?? "") { channelFieldDrafts.removeValue(forKey: key) }
            }
            let key=target + "|" + channel.id
            if let instruction=channel.pairing_instruction, !instruction.isEmpty { channelInstructions[key]=instruction }
            else { channelInstructions.removeValue(forKey:key) }
        }
    }
    var uninstallConfirmOpen = false
    var uninstallPurgeData = false
    var hasCheckedFirstLaunchOnboarding = false
    var clipboardString = ""
    var hasRenderedState = false
    var pendingMutation = false
    var rebuildCount = 0

    func menuWillOpen(_ menu: NSMenu) {
        MenuControl.clearHoverWhere { $0.enclosingMenuItem != nil || ($0.window !== self.directoryPanel && $0.window !== self.settingsPanel) }
        emit(["op": "refresh_available"])
    }
    func menuDidClose(_ menu: NSMenu) {
        MenuControl.clearHoverWhere { $0.enclosingMenuItem != nil || ($0.window !== self.directoryPanel && $0.window !== self.settingsPanel) }
    }
    func start() {
        signal(SIGPIPE, SIG_IGN)
        signal(SIGUSR1, SIG_IGN)
        clickSource = DispatchSource.makeSignalSource(signal: SIGUSR1, queue: .main)
        clickSource?.setEventHandler { [weak self] in self?.hide(clicked: true) }
        clickSource?.resume()
        cardMenu.autoenablesItems = false
        cardMenu.delegate = self
        settingsKeyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            if event.modifierFlags.intersection(.deviceIndependentFlagsMask) == .command && event.charactersIgnoringModifiers == "," {
                self?.openSettingsPanel()
                return nil
            }
            return event
        }
        MenuControl.tracksPhysicalPointer = !headless
        if headless { headlessStatusButton = NSButton(frame: .zero) }
        else {
            status = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
            status?.menu = cardMenu
            statusImage = makeCitoStatusIcon(.idle)
            if let button = status?.button {
                formatStatusButton(button, count: 0, image: statusImage)
            }
            hoverEventMonitor = NSEvent.addLocalMonitorForEvents(matching: [.mouseMoved, .leftMouseDragged, .scrollWheel, .mouseExited, .leftMouseDown, .rightMouseDown]) { event in
                MenuControl.reconcileActiveHover()
                return event
            }
        }
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
                        RunLoop.main.perform(inModes: [.common, .eventTracking]) { [weak self] in self?.receive(line) }
                    } else if !dropping {
                        buffer.append(byte)
                        if buffer.count > 1_048_576 { diagnostic("oversized frame"); buffer.removeAll(); dropping = true }
                    }
                }
            }
        }
    }
    func findControl(in view: NSView, id: String) -> MenuControl? {
        guard !view.isHidden else { return nil }
        let rawId = view.identifier?.rawValue
        if rawId == id, let control = view as? MenuControl { return control }
        if let table = view as? NSTableView {
            let count = min(table.numberOfRows, 100)
            for r in 0..<count {
                if let cell = table.view(atColumn: 0, row: r, makeIfNecessary: true),
                   let found = findControl(in: cell, id: id) {
                    return found
                }
            }
        }
        let children: [NSView]
        if let scroll = view as? NSScrollView, let doc = scroll.documentView {
            children = [doc]
        } else {
            children = view.subviews
        }
        return children.compactMap { findControl(in: $0, id: id) }.first
    }
    func findTextField(in view: NSView, id: String) -> NSTextField? {
        guard !view.isHidden else { return nil }
        if view.identifier?.rawValue == id, let field = view as? NSTextField { return field }
        let children: [NSView]
        if let scroll = view as? NSScrollView, let doc = scroll.documentView {
            children = [doc]
        } else {
            children = view.subviews
        }
        return children.compactMap { findTextField(in: $0, id: id) }.first
    }
    func receive(_ data: Data) {
        // Headless hooks use the exact same menu handlers and banner click path.
        if headless, let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let op = object["op"] as? String {
            if op == "test_click" { hide(clicked: true); return }
            if op == "test_inspect" { inspectMenu(); return }
            if op == "test_render_png", let surface = object["surface"] as? String, let path = object["path"] as? String {
                renderPNG(surface: surface, path: path); return
            }
            if op == "test_menu_open" { menuWillOpen(cardMenu); return }
            if op == "test_menu_close" { menuDidClose(cardMenu); return }
            if op == "test_appearance", let name = object["appearance"] as? String, ["light", "dark"].contains(name) {
                let appearance = NSAppearance(named: name == "dark" ? .darkAqua : .aqua)
                for item in cardMenu.items { item.view?.appearance = appearance }
                directoryPanel?.appearance = appearance
                settingsPanel?.appearance = appearance
                return
            }
            if op == "test_hover", let id = object["id"] as? String, let hovered = object["hovered"] as? Bool {
                for item in cardMenu.items {
                    if let view = item.view, let control = findControl(in: view, id: id) {
                        control.setHovered(hovered)
                        return
                    }
                }
                if let rootView = directoryPanel?.contentView, let control = findControl(in: rootView, id: id) {
                    control.setHovered(hovered)
                    return
                }
                if let rootView = settingsPanel?.contentView, let control = findControl(in: rootView, id: id) {
                    control.setHovered(hovered)
                }
                return
            }
            if op == "test_directory_query", let query = object["query"] as? String {
                if directoryPanel == nil { openDirectoryPanel() }
                directorySearchField?.stringValue = query
                directoryQuery = query
                applyDirectoryFilter(fromDaemonFrame: false, daemonMatchedCount: nil)
                emit(["op": "query_directory", "query": directoryQuery, "scope": directoryScopeFilter, "harness": directoryHarnessFilter])
                return
            }
            if op == "test_directory_key", let key = object["key"] as? String {
                handleDirectoryKey(key)
                return
            }
            if op == "test_settings_scroll", let offset = object["offset"] as? Double {
                for case let scroll as NSScrollView in settingsPanel?.contentView?.subviews ?? [] where !scroll.isHidden {
                    let top=max(0,(scroll.documentView?.frame.height ?? 0)-scroll.contentSize.height)
                    scroll.contentView.scroll(to:NSPoint(x:0,y:max(0,min(top,CGFloat(offset)))))
                    scroll.reflectScrolledClipView(scroll.contentView)
                }
                return
            }
            if op == "test_settings_input", let id = object["id"] as? String, let value = object["value"] as? String {
                if id == "composer_input" {
                    composerInputField?.stringValue = value
                    return
                }
                if let field = settingsFieldControls[id] ?? (settingsPanel?.contentView.flatMap { findTextField(in: $0, id: id) }) {
                    field.stringValue = value
                    controlTextDidChange(Notification(name: NSControl.textDidChangeNotification, object: field))
                }
                syncAddHubFieldsFromControls()
                return
            }
            if op == "test_composer_input", let value = object["value"] as? String {
                composerInputField?.stringValue = value
                return
            }
            if op == "test_clipboard", let text = object["text"] as? String {
                if object["system_only"] as? Bool != true { clipboardString = text }
                appPasteboard.clearContents()
                appPasteboard.setString(text, forType: .string)
                return
            }
            if op == "test_settings_key", let key = object["key"] as? String {
                if key.lowercased() == "command-comma", let event = NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [.command], timestamp: 0, windowNumber: 0, context: nil, characters: ",", charactersIgnoringModifiers: ",", isARepeat: false, keyCode: 43) {
                    _ = cardMenu.performKeyEquivalent(with: event)
                }
                if key.lowercased() == "escape" {
                    closeSettingsPanel(emitAction: true)
                }
                return
            }
            if op == "test_control", let id = object["id"] as? String {
                for item in cardMenu.items {
                    if let view = item.view, let control = findControl(in: view, id: id) {
                        _ = control.accessibilityPerformPress()
                        return
                    }
                }
                if let rootView = directoryPanel?.contentView, let control = findControl(in: rootView, id: id) {
                    _ = control.accessibilityPerformPress()
                    return
                }
                if let rootView = settingsPanel?.contentView, let control = findControl(in: rootView, id: id) {
                    _ = control.accessibilityPerformPress()
                    return
                }
                if id.hasPrefix("dir_peer:"), let table = directoryTableView {
                    let scopedId = String(id.dropFirst("dir_peer:".count))
                    if let row = directoryFilteredPeers.firstIndex(where: { $0.scoped_id == scopedId }) {
                        table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
                    }
                } else if id.hasPrefix("activity_row:"), let table = directoryTableView {
                    let sigId = String(id.dropFirst("activity_row:".count))
                    if let row = activityFilteredItems.firstIndex(where: { $0.signal_id == sigId }) {
                        table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
                    }
                }
                return
            }
        }
        guard let frame = try? JSONDecoder().decode(Frame.self, from: data) else { diagnostic("invalid frame"); return }
        if frame.op == "composer_result" {
            guard composerPendingText != nil else { return }
            if frame.ok == true {
                if composerTarget == composerPendingTarget && composerTargetHub == composerPendingHub && composerInputField?.stringValue == composerPendingText { closeComposer() }
                else { composerErrorLabel?.stringValue = "Sent. Your newer draft is preserved." }
            } else {
                composerErrorLabel?.stringValue = frame.error ?? "Send failed. Your draft is preserved."
            }
            composerPendingText = nil
            composerPendingTarget = ""
            composerPendingHub = ""
            return
        }
        if frame.op == "error" {
            diagnostic("error \(frame.error ?? "Unknown daemon error")")
            return
        }
        if frame.op == "settings", let value = frame.settings {
            acceptSettings(value, hub: frame.hub)
            if settingsPanel != nil { rebuildSettingsPanel() }
            return
        }
        if frame.op == "pair_code", let id = frame.channel_id {
            channelInstructions["\(frame.hub ?? effectiveChannelHub)|\(id)"] = frame.instruction ?? frame.code ?? ""
            if settingsPanel != nil { rebuildSettingsPanel() }
            return
        }
        if frame.op == "channel_test_result", let id = frame.channel_id {
            channelResults["\(frame.hub ?? effectiveChannelHub)|\(id)"] = (ok: frame.ok == true, detail: frame.detail ?? frame.error ?? (frame.ok == true ? "Test sent" : "Test failed"))
            if settingsPanel != nil { rebuildSettingsPanel() }
            return
        }
        if frame.op == "state", let h = frame.hubs, let l = frame.listeners,
           h.allSatisfy({ ["connected", "connecting", "auth_required", "disconnected"].contains($0.status) && ["none", "token", "oidc"].contains($0.auth_mode) }),
           l.allSatisfy({ ["self", "workspace"].contains($0.scope) }) {
            let a = frame.available ?? []
            let r = (frame.my_remote_listeners ?? []).filter { $0.harness != "human" }
            let onlineCount = frame.directory_online_count ?? max(directoryOnlineCount, r.count)
            let nextUnread = frame.unread_count ?? unreadCount
            let nextActivity = frame.activity ?? activityAllItems
            let activityChanged = nextUnread != unreadCount || nextActivity != activityAllItems
            unreadCount = nextUnread
            activityAllItems = nextActivity
            let nextHarnesses = frame.harnesses ?? harnesses
            let harnessesChanged = nextHarnesses != harnesses
            if harnessesChanged { harnesses = nextHarnesses }
            let nextSettings = frame.settings ?? settings
            // An unchanged save acknowledgement still needs to refresh the settings feedback.
            let settingsChanged = nextSettings != settings || hostHubPortPending
            if settingsChanged, let value = nextSettings { acceptSettings(value, hub: frame.hub ?? value.hubs.first(where: { $0.is_default })?.name ?? h.first?.name) }
            if let error = frame.composer_error, !error.isEmpty { composerErrorLabel?.stringValue = error }
            if hasRenderedState && !pendingMutation && !harnessesChanged && !settingsChanged && !activityChanged && h == hubs && l == listeners && a == available && r == myRemote && onlineCount == directoryOnlineCount {
                let count = uniqueSessionCount
                let warning = hubs.contains { $0.status != "connected" || $0.error != nil }
                statusImage = makeCitoStatusIcon(warning ? .warning : count > 0 ? .active : .idle)
                if let button = status?.button ?? headlessStatusButton {
                    formatStatusButton(button, count: count, image: statusImage)
                }
            } else {
                hubs = h; listeners = l; available = a; myRemote = r; directoryOnlineCount = onlineCount
                hasRenderedState = true; pendingMutation = false
                rebuild()
                if directoryPanel != nil {
                    updateDirectoryModePills()
                    rebuildDirectoryFacetBar()
                    if directoryMode == "activity" {
                        applyActivityFilter()
                    } else if harnessesChanged {
                        directoryTableView?.reloadData()
                    }
                }
                if settingsPanel != nil {
                    rebuildSettingsPanel()
                }
            }
            if !hasCheckedFirstLaunchOnboarding, let s = settings, !s.apps.isEmpty {
                hasCheckedFirstLaunchOnboarding = true
                if !headless && !s.apps.contains(where: { $0.configured }) {
                    let rawStateDir = ProcessInfo.processInfo.environment["CITO_STATE_DIR"] ?? (NSHomeDirectory() + "/.cito")
                    let expandedStateDir = rawStateDir.hasPrefix("~/")
                        ? (NSHomeDirectory() + "/" + String(rawStateDir.dropFirst(2)))
                        : (rawStateDir == "~" ? NSHomeDirectory() : rawStateDir)
                    let markerPath = expandedStateDir + "/onboarding-shown"
                    if !FileManager.default.fileExists(atPath: markerPath) {
                        pendingOnboardingMarker = markerPath
                        settingsActiveTab = "apps"
                        openSettingsPanel()
                    }
                }
            }
            if headless { diagnostic("state hubs=\(h.count) listeners=\(l.count)" + (a.isEmpty ? "" : " available=\(a.count)")) }
        } else if frame.op == "directory", let peers = frame.peers {
            directoryAllPeers = peers
            serverDirectoryQuery = directoryQuery
            serverDirectoryScope = directoryScopeFilter
            serverDirectoryHarness = directoryHarnessFilter
            serverDirectoryMatchedCount = frame.matched_count ?? peers.count
            directoryOnlineCount = frame.online_count ?? peers.count
            if let act = frame.activity { activityAllItems = act }
            if let u = frame.unread_count { unreadCount = u }
            updateDirectoryModePills()
            rebuildDirectoryFacetBar()
            if directoryMode == "activity" {
                applyActivityFilter()
            } else {
                applyDirectoryFilter(fromDaemonFrame: true, daemonMatchedCount: frame.matched_count)
            }
            if hasRenderedState { rebuild() }
            if headless { diagnostic("directory peers=\(peers.count) matched=\(directoryMatchedCount) online=\(directoryOnlineCount)") }
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
    func emit(_ action: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: action, options: [.sortedKeys]) else { return }
        if let op = action["op"] as? String,
           ["channel_save", "channel_pair", "channel_toggle", "channel_unpair", "channel_cancel_pair", "disconnect_human_channel"].contains(op),
           let id = action["channel_id"] as? String {
            let key = "\(action["hub"] as? String ?? effectiveChannelHub)|\(id)"
            if channelResults.removeValue(forKey: key) != nil, settingsPanel != nil { rebuildSettingsPanel() }
        }
        FileHandle.standardOutput.write(data + Data([10]))
    }
    func rebuild() {
        MenuControl.clearHoverWhere { $0.enclosingMenuItem != nil || ($0.window !== self.directoryPanel && $0.window !== self.settingsPanel) }
        rebuildCount += 1
        let count = uniqueSessionCount
        let warning = hubs.contains { $0.status != "connected" || $0.error != nil }
        statusImage = makeCitoStatusIcon(warning ? .warning : count > 0 ? .active : .idle)
        if let button = status?.button ?? headlessStatusButton {
            formatStatusButton(button, count: count, image: statusImage)
        }
        let menu = cardMenu
        menu.removeAllItems()
        func host(_ view: NSView, id: String) -> NSMenuItem {
            view.identifier = NSUserInterfaceItemIdentifier(id)
            let item = NSMenuItem(); item.view = view; menu.addItem(item); return item
        }
        let header = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 40))
        header.addSubview(textLabel("Cito", NSRect(x: 12, y: 12, width: 32, height: 18), size: 13, weight: .bold))
        let subtitleText: String
        if count > 0 {
            subtitleText = myRemote.isEmpty ? "\(count) listening" : "\(count) listening · \(myRemote.count) remote"
        } else {
            subtitleText = myRemote.isEmpty ? "Idle" : "Idle · \(myRemote.count) remote"
        }
        let subWidth = ceil(measureTextWidth(subtitleText, font: .systemFont(ofSize: 11.5))) + 8
        let subLabel = textLabel(subtitleText, NSRect(x: 50, y: 12, width: subWidth, height: 17), size: 11.5, color: .secondaryLabelColor)
        subLabel.identifier = NSUserInterfaceItemIdentifier("header_subtitle")
        header.addSubview(subLabel)
        _ = host(header, id: "header")
        var drawerItems: [String: NSMenuItem] = [:]
        let btnFont = NSFont.systemFont(ofSize: 11, weight: .medium)
        for hub in hubs {
            let drawer = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 32))
            let loginRun = { self.emit(["op": "hub_action", "hub": hub.name, "action": hub.status == "connected" ? "switch_account" : "login"]) }
            let reconnectRun = { self.emit(["op": "hub_action", "hub": hub.name, "action": "reconnect"]) }
            let disconnectRun = { self.emit(["op": "hub_action", "hub": hub.name, "action": "disconnect"]) }
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
                let control = MenuControl(title, frame: NSRect(x: btnX, y: 5, width: width, height: 22))
                control.label.font = btnFont
                control.style = primary ? .prominent : .neutral
                control.identifier = NSUserInterfaceItemIdentifier("\(action):\(hub.name)")
                control.pressed = run; drawer.addSubview(control); btnX -= 4
            }
            let buttonsMinX = btnX + 4
            let statusText: String
            if let error = hub.error { statusText = error }
            else if hub.status == "connected" { statusText = hub.auth_mode == "oidc" ? "Connected as \(hub.user ?? "user")" : "Connected · \(hub.user ?? "local")" }
            else if hub.status == "connecting" { statusText = "Connecting to hub…" }
            else if hub.status == "auth_required" { statusText = "Authentication required" }
            else { statusText = "Offline · Disconnected" }
            let detail = textLabel(statusText, NSRect(x: 12, y: 8, width: max(40, buttonsMinX - 20), height: 16),
                color: hub.error != nil || hub.status == "auth_required" ? .labelColor : .secondaryLabelColor)
            detail.toolTip = hub.error ?? statusText; drawer.addSubview(detail)
            let item = host(drawer, id: "drawer:\(hub.name)")
            let autoExpand = hub.status != "connected" || hub.error != nil
            item.isHidden = selectedHubs.contains(hub.name) ? false : !autoExpand
            drawerItems[hub.name] = item
        }
        let stripX = max(138, 54 + subWidth)
        let stripWidth = max(44, 368 - stripX)
        let hubStrip = NSScrollView(frame: NSRect(x: stripX, y: 9, width: stripWidth, height: 22))
        hubStrip.drawsBackground = false; hubStrip.hasHorizontalScroller = false
        let pillContainer = NSView(frame: NSRect(x: 0, y: 0, width: stripWidth, height: 22))
        hubStrip.documentView = pillContainer; header.addSubview(hubStrip)
        var pillX: CGFloat = stripWidth
        for hub in hubs.reversed() {
            let color = statusColor(hub.status)
            let isSingleDefault = hubs.count == 1 && (hub.name == "default" || hub.name == "local")
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
                net.contentTintColor = .labelColor; pill.addSubview(net); pill.centerSymbolView = net
                let chev = NSImageView(frame: NSRect(x: 31, y: 7, width: 8, height: 8))
                chev.image = NSImage(systemSymbolName: "chevron.down", accessibilityDescription: nil)
                chev.contentTintColor = .secondaryLabelColor; pill.addSubview(chev); pill.accessorySymbolView = chev
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
                MenuControl.reconcileActiveHover()
            }
            pillContainer.addSubview(pill); pillX -= 4
        }
        if pillX < 0 {
            for pill in pillContainer.subviews { pill.frame.origin.x -= pillX }
            pillContainer.frame.size.width = stripWidth - pillX
        }
        var activeRows: [(id: String, view: NSView)] = []
        let home = NSHomeDirectory()
        let map = harnessMap
        let targetHub = effectiveTargetHub
        let hasHumanListener = listeners.contains { $0.harness == "human" && $0.hub == targetHub }
        let targetUser = hubs.first(where: { $0.name == targetHub })?.user ?? hubs.first?.user ?? "local"
        if !hasHumanListener && hubs.contains(where: { $0.name == targetHub && $0.status != "disconnected" }) {
            let row = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 52))
            let card = roundedView(NSRect(x: 10, y: 3, width: 360, height: 46), radius: 10,
                color: .labelColor.withAppearanceAlpha(0.05), border: .labelColor.withAppearanceAlpha(0.10))
            card.addSubview(makeHarnessBadge(NSRect(x: 10, y: 9, width: 28, height: 28), radius: 7, iconInset: 6, harness: "human", harnesses: map))
            card.addSubview(textLabel("Human Node (@\(targetUser))", NSRect(x: 46, y: 24, width: 220, height: 16), size: 12.5, weight: .semibold))
            card.addSubview(textLabel("Paused · Click Enable to receive signals", NSRect(x: 46, y: 8, width: 220, height: 14), size: 10.5, color: .secondaryLabelColor))
            let enable = MenuControl("Enable", frame: NSRect(x: 272, y: 12, width: 78, height: 22), leadingSymbol: "plus", radius: 6)
            enable.identifier = NSUserInterfaceItemIdentifier("listen_human")
            enable.style = .prominent
            enable.pressed = { [weak self] in self?.emit(["op": "listen_session", "hub": targetHub, "harness": "human", "session_id": "default", "about": "Human receiver"]) }
            card.addSubview(enable); row.addSubview(card)
            _ = host(row, id: "human_paused_card")
        }
        let orderedListeners = listeners.enumerated().sorted { a, b in
            let ah = a.element.harness == "human" ? 0 : 1
            let bh = b.element.harness == "human" ? 0 : 1
            return ah != bh ? ah < bh : a.offset < b.offset
        }.map { $0.element }
        for listener in orderedListeners {
            let aboutText = (listener.about ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            let hasDistinctAbout = !aboutText.isEmpty && aboutText.caseInsensitiveCompare(listener.title.trimmingCharacters(in: .whitespacesAndNewlines)) != .orderedSame
            let rowHeight: CGFloat = hasDistinctAbout ? 78 : 64
            let cardHeight: CGFloat = hasDistinctAbout ? 72 : 58
            let row = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: rowHeight))
            row.identifier = NSUserInterfaceItemIdentifier("session:\(listener.scoped_id)")
            let card = roundedView(NSRect(x: 10, y: 3, width: 360, height: cardHeight), radius: 10,
                color: .labelColor.withAppearanceAlpha(0.07), border: .labelColor.withAppearanceAlpha(0.11))
            row.addSubview(card)
            activeRows.append((id: "session:\(listener.scoped_id)", view: row))
            let badge = makeHarnessBadge(NSRect(x: 10, y: cardHeight - 44, width: 34, height: 34),
                radius: 8, iconInset: 8, harness: listener.harness, harnesses: map)
            card.addSubview(badge)
            let listenerHub = hubs.first(where: { $0.name == listener.hub })
            let showScopeToggle = !(listenerHub?.name == "local" && listenerHub?.auth_mode == "none")
            let hubPillWidth = min(76, max(46, ceil(measureTextWidth(listener.hub, font: btnFont)) + 22))
            let baseActionWidth: CGFloat = showScopeToggle ? 88 : 22
            let actionWidth: CGFloat = hubs.count > 1 ? baseActionWidth + hubPillWidth + 4 : baseActionWidth
            let title = textLabel(listener.title,
                NSRect(x: 54, y: cardHeight - 26, width: 350 - 54 - actionWidth - 6, height: 18), size: 13, weight: .semibold)
            title.toolTip = listener.title; card.addSubview(title)
            if hasDistinctAbout {
                let aboutLabel = textLabel(aboutText, NSRect(x: 54, y: 28, width: 296, height: 16), size: 11.5, color: .secondaryLabelColor)
                aboutLabel.toolTip = aboutText; card.addSubview(aboutLabel)
            }
            let short = shortPublicId(listener.public_id)
            let copyFont = NSFont.monospacedSystemFont(ofSize: 11, weight: .medium)
            let copyWidth = max(measureTextWidth("✓ Copied", font: copyFont), measureTextWidth(short, font: copyFont)) + 24
            let copy = MenuControl(short, frame: NSRect(x: 54, y: 7, width: copyWidth, height: 19), leadingSymbol: "doc.on.doc")
            copy.label.font = copyFont
            copy.toolTip = listener.scoped_id; copy.identifier = NSUserInterfaceItemIdentifier("copy:\(listener.scoped_id)")
            copy.pressed = { [weak self, weak copy] in
                self?.clipboardString = listener.scoped_id
                appPasteboard.clearContents(); appPasteboard.setString(listener.scoped_id, forType: .string)
                copy?.copied()
            }
            card.addSubview(copy)
            let cwd = listener.cwd == home ? "~" : listener.cwd.hasPrefix(home + "/") ? "~" + listener.cwd.dropFirst(home.count) : listener.cwd
            let branch = (listener.branch ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            let pathText: String
            if let chs = listener.channels, !chs.isEmpty, listener.harness == "human" {
                pathText = chs.joined(separator: " · ")
            } else {
                pathText = (!branch.isEmpty && branch != "HEAD") ? "\(cwd) · \(branch)" : cwd
            }
            let path = textLabel(pathText,
                NSRect(x: 54 + copyWidth + 8, y: 8, width: max(0, 296 - copyWidth - 8), height: 16), size: 11, color: .secondaryLabelColor)
            path.lineBreakMode = .byTruncatingMiddle; path.toolTip = listener.cwd; card.addSubview(path)
            let scopeX: CGFloat = 350 - baseActionWidth
            if hubs.count > 1 {
                let hubPill = MenuControl(listener.hub, frame: NSRect(x: scopeX - hubPillWidth - 4, y: cardHeight - 28, width: hubPillWidth, height: 22), radius: 6)
                hubPill.identifier = NSUserInterfaceItemIdentifier("switch_hub:\(listener.scoped_id)")
                hubPill.style = .neutral
                hubPill.label.font = btnFont
                hubPill.label.alignment = .left
                hubPill.label.frame = NSRect(x: 16, y: 3.5, width: hubPillWidth - 20, height: 15)
                let hubStatus = hubs.first(where: { $0.name == listener.hub })?.status ?? "disconnected"
                hubPill.addSubview(roundedView(NSRect(x: 7, y: 8, width: 6, height: 6), radius: 3, color: statusColor(hubStatus)))
                let currentIndex = hubs.firstIndex(where: { $0.name == listener.hub }) ?? 0
                let ring = (1..<hubs.count).map { hubs[(currentIndex + $0) % hubs.count] }.filter { $0.name != listener.hub && $0.status != "disconnected" }
                let nextTarget = ring.first(where: { h in
                    h.status == "connected" && !listeners.contains(where: { $0.hub == h.name && $0.harness == listener.harness && $0.session_id == listener.session_id })
                }) ?? ring.first
                if let nextHub = nextTarget?.name {
                    hubPill.toolTip = "Switch hub to \(nextHub)"
                    hubPill.pressed = { [weak self] in
                        guard let self = self else { return }
                        self.pendingMutation = true
                        self.emit(["op": "switch_hub", "from_hub": listener.hub, "to_hub": nextHub, "listener_id": listener.listener_id])
                    }
                }
                card.addSubview(hubPill)
            }
            var x: CGFloat = scopeX
            if showScopeToggle {
                var scope = listener.scope
                let scopeBtn = MenuControl(scope == "self" ? "Self" : "Team", frame: NSRect(x: x, y: cardHeight - 28, width: 62, height: 22),
                    leadingSymbol: scope == "self" ? "lock.fill" : "person.2.fill")
                scopeBtn.identifier = NSUserInterfaceItemIdentifier("scope:\(listener.scoped_id)")
                let scopeColor = { [weak scopeBtn] in
                    scopeBtn?.style = scope == "workspace" ? .prominent : .neutral
                }
                scopeColor()
                scopeBtn.pressed = { [weak self, weak scopeBtn] in
                    guard let self = self else { return }
                    self.pendingMutation = true
                    scope = scope == "self" ? "workspace" : "self"
                    scopeBtn?.setLeadingSymbol(scope == "self" ? "lock.fill" : "person.2.fill")
                    scopeBtn?.label.stringValue = scope == "self" ? "Self" : "Team"
                    scopeBtn?.setAccessibilityLabel(scopeBtn?.label.stringValue); scopeColor()
                    self.emit(["op": "set_scope", "hub": listener.hub, "listener_id": listener.listener_id, "scope": scope])
                }
                card.addSubview(scopeBtn); x += 66
            }
            let stop = MenuControl("Stop listening", frame: NSRect(x: x, y: cardHeight - 28, width: 22, height: 22), symbol: "xmark")
            stop.toolTip = "Stop listening"; stop.style = .destructiveHover
            stop.identifier = NSUserInterfaceItemIdentifier("stop:\(listener.scoped_id)")
            let unlisten = { self.emit(["op": "unlisten", "hub": listener.hub, "listener_id": listener.listener_id]) }
            stop.pressed = unlisten; card.addSubview(stop)
        }
        if activeRows.count <= 4 {
            for (id, row) in activeRows { _ = host(row, id: id) }
        } else {
            let totalHeight = activeRows.reduce(CGFloat(0)) { $0 + $1.view.frame.height }
            let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: 380, height: 260))
            scroll.drawsBackground = false; scroll.hasVerticalScroller = true
            scroll.hasHorizontalScroller = false; scroll.autohidesScrollers = true
            let doc = FlippedView(frame: NSRect(x: 0, y: 0, width: 380, height: totalHeight))
            var y: CGFloat = 0
            for (_, row) in activeRows {
                row.frame.origin = NSPoint(x: 0, y: y)
                doc.addSubview(row); y += row.frame.height
            }
            scroll.documentView = doc; _ = host(scroll, id: "active_scroll")
        }
        if listeners.isEmpty {
            if available.isEmpty {
                let row = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 92))
                let card = roundedView(NSRect(x: 10, y: 3, width: 360, height: 86), radius: 10, color: .labelColor.withAppearanceAlpha(0.045))
                let icon = NSImageView(frame: NSRect(x: 12, y: 22, width: 24, height: 24))
                icon.image = NSImage(systemSymbolName: "waveform.slash", accessibilityDescription: "No active listeners")
                icon.contentTintColor = .secondaryLabelColor; card.addSubview(icon)
                card.addSubview(textLabel("No active listeners", NSRect(x: 44, y: 57, width: 304, height: 18), size: 13, weight: .semibold))
                let subtitle = !(settings?.apps.contains(where:{$0.configured}) ?? true) ? "Connect your coding apps in Settings to start" : "Call cito_listen in a session or open Human Channels"
                card.addSubview(textLabel(subtitle, NSRect(x:44,y:36,width:304,height:16),size:10.5,color:.secondaryLabelColor))
                let apps=MenuControl("Set Up Agent Apps…",frame:NSRect(x:44,y:8,width:148,height:22),radius:6)
                apps.identifier=NSUserInterfaceItemIdentifier("empty_open_setup")
                apps.pressed={ [weak self] in self?.settingsActiveTab="apps"; self?.openSettingsPanel() }
                card.addSubview(apps)
                let channels=MenuControl("Human Channels…",frame:NSRect(x:198,y:8,width:148,height:22),radius:6)
                channels.identifier=NSUserInterfaceItemIdentifier("empty_open_channels")
                channels.pressed={ [weak self] in self?.settingsActiveTab="channels"; self?.openSettingsPanel() }
                card.addSubview(channels)
                row.addSubview(card); _ = host(row, id: "empty")
            } else {
                let row = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 34))
                let pill = roundedView(NSRect(x: 10, y: 3, width: 360, height: 28), radius: 8, color: .labelColor.withAppearanceAlpha(0.045))
                let icon = NSImageView(frame: NSRect(x: 12, y: 7, width: 14, height: 14))
                icon.image = NSImage(systemSymbolName: "waveform.slash", accessibilityDescription: "No active listeners")
                icon.contentTintColor = .secondaryLabelColor; pill.addSubview(icon)
                pill.addSubview(textLabel("No active listeners — click + Listen below",
                    NSRect(x: 32, y: 6, width: 316, height: 16), size: 11, color: .secondaryLabelColor))
                row.addSubview(pill); _ = host(row, id: "empty")
            }
        }
        // Zone 2 — My Other Hosts
        if !myRemote.isEmpty {
            let remoteHeader = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 26))
            remoteHeader.addSubview(roundedView(NSRect(x: 12, y: 24, width: 356, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
            let toggleRemote = MenuControl("MY OTHER HOSTS · \(myRemote.count)", frame: NSRect(x: 8, y: 1, width: 364, height: 22),
                leadingSymbol: myRemoteCollapsed ? "chevron.right" : "chevron.down", color: .secondaryLabelColor, radius: 6)
            toggleRemote.identifier = NSUserInterfaceItemIdentifier("toggle_my_remote")
            toggleRemote.style = .subtleHeader
            toggleRemote.label.font = .systemFont(ofSize: 10, weight: .semibold)
            toggleRemote.label.alignment = .left
            toggleRemote.pressed = { [weak self] in
                guard let self = self else { return }
                self.myRemoteCollapsed.toggle()
                self.rebuild()
            }
            remoteHeader.addSubview(toggleRemote)
            _ = host(remoteHeader, id: "my_remote_header")
            if !myRemoteCollapsed {
                var remoteRows: [(id: String, view: NSView)] = []
                let copyFont = NSFont.monospacedSystemFont(ofSize: 11, weight: .medium)
                for peer in myRemote {
                    let row = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 34))
                    let rowId = "remote:\(peer.scoped_id)"
                    row.identifier = NSUserInterfaceItemIdentifier(rowId)
                    let hostIcon = NSImageView(frame: NSRect(x: 16, y: 10, width: 14, height: 14))
                    hostIcon.image = NSImage(systemSymbolName: "desktopcomputer", accessibilityDescription: "Remote host")
                    hostIcon.contentTintColor = .secondaryLabelColor
                    row.addSubview(hostIcon)
                    let short = shortPublicId(peer.public_id)
                    let copyWidth = max(measureTextWidth("✓ Copied", font: copyFont), measureTextWidth(short, font: copyFont)) + 24
                    let copy = MenuControl(short, frame: NSRect(x: 368 - copyWidth, y: 7, width: copyWidth, height: 20), leadingSymbol: "doc.on.doc")
                    copy.label.font = copyFont
                    copy.toolTip = peer.scoped_id
                    copy.identifier = NSUserInterfaceItemIdentifier("copy_remote:\(peer.scoped_id)")
                    copy.pressed = { [weak self, weak copy] in
                        self?.clipboardString = peer.scoped_id
                        appPasteboard.clearContents()
                        appPasteboard.setString(peer.scoped_id, forType: .string)
                        copy?.copied()
                    }
                    row.addSubview(copy)
                    let cleanHost = peer.host_name.replacingOccurrences(of: "\\.local$", with: "", options: [.regularExpression, .caseInsensitive])
                    let hostText = cleanHost.isEmpty ? peer.host_name : cleanHost
                    let branch = (peer.branch ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
                    let branchSuffix = (!branch.isEmpty && branch != "HEAD") ? " · \(branch)" : ""
                    let info = textLabel("\(hostText) · \(peer.title)\(branchSuffix)",
                        NSRect(x: 36, y: 8, width: max(40, 368 - copyWidth - 44), height: 18), size: 12, weight: .regular, color: .labelColor)
                    info.lineBreakMode = .byTruncatingMiddle
                    info.toolTip = "\(peer.scoped_id) — \(peer.about)"
                    row.addSubview(info)
                    remoteRows.append((id: rowId, view: row))
                }
                if remoteRows.count <= 3 {
                    for (id, row) in remoteRows { _ = host(row, id: id) }
                } else {
                    let totalHeight = CGFloat(remoteRows.count) * 34
                    let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: 380, height: 102))
                    scroll.drawsBackground = false; scroll.hasVerticalScroller = true
                    scroll.hasHorizontalScroller = false; scroll.autohidesScrollers = true
                    let doc = FlippedView(frame: NSRect(x: 0, y: 0, width: 380, height: totalHeight))
                    for (idx, (_, row)) in remoteRows.enumerated() {
                        row.frame.origin = NSPoint(x: 0, y: CGFloat(idx) * 34)
                        doc.addSubview(row)
                    }
                    scroll.documentView = doc; _ = host(scroll, id: "my_remote_scroll")
                }
            }
        }
        // Zone 3 — Available Sessions with Per-Hub Target Selector & Collapsible Harness Groups
        if !available.isEmpty {
            let targetHub = effectiveTargetHub
            let availHeader = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 24))
            availHeader.addSubview(roundedView(NSRect(x: 12, y: 21, width: 356, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
            availHeader.addSubview(textLabel("AVAILABLE SESSIONS", NSRect(x: 12, y: 4, width: hubs.count > 1 ? 124 : 220, height: 14),
                size: 10, weight: .semibold, color: .secondaryLabelColor))
            if hubs.count > 1 {
                let targetStrip = NSScrollView(frame: NSRect(x: 140, y: 2, width: 228, height: 18))
                targetStrip.drawsBackground = false
                targetStrip.hasHorizontalScroller = false
                targetStrip.hasVerticalScroller = false
                let targetContainer = NSView(frame: NSRect(x: 0, y: 0, width: 228, height: 18))
                targetStrip.documentView = targetContainer
                availHeader.addSubview(targetStrip)
                let targetFont = NSFont.systemFont(ofSize: 10.5, weight: .medium)
                var targetX: CGFloat = 228
                for hub in hubs.reversed() where hub.status != "disconnected" {
                    let w = max(38, ceil(measureTextWidth(hub.name, font: targetFont)) + 16)
                    targetX -= w
                    let pill = MenuControl(hub.name, frame: NSRect(x: targetX, y: 0, width: w, height: 18), radius: 9)
                    pill.identifier = NSUserInterfaceItemIdentifier("target_hub:\(hub.name)")
                    pill.label.font = targetFont
                    pill.style = hub.name == targetHub ? .prominent : .neutral
                    pill.toolTip = "Listen on \(hub.name)"
                    pill.pressed = { [weak self] in
                        guard let self = self else { return }
                        self.selectedTargetHub = hub.name
                        self.rebuild()
                    }
                    targetContainer.addSubview(pill)
                    targetX -= 4
                }
                if targetX < 0 {
                    for pill in targetContainer.subviews { pill.frame.origin.x -= targetX }
                    targetContainer.frame.size.width = 228 - targetX
                }
            }
            _ = host(availHeader, id: "available_header")
            let visibleAvailable = hubs.count > 1 && !targetHub.isEmpty
                ? available.filter { !($0.listening_hubs ?? []).contains(targetHub) }
                : available
            if visibleAvailable.isEmpty {
                let emptyTarget = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 28))
                emptyTarget.addSubview(textLabel("All sessions listening on \(targetHub)",
                    NSRect(x: 24, y: 6, width: 332, height: 16), size: 11.5, weight: .regular, color: .secondaryLabelColor))
                _ = host(emptyTarget, id: "available_empty_target")
            }
            var groupsByHarness: [String: [Candidate]] = [:]
            for c in visibleAvailable { groupsByHarness[c.harness, default: []].append(c) }
            let orderedHarnesses = groupsByHarness.keys.sorted { a, b in
                let va = resolveHarnessVisual(a, harnesses: map)
                let vb = resolveHarnessVisual(b, harnesses: map)
                return va.order != vb.order ? va.order < vb.order : a < b
            }
            let groupedCandidates = orderedHarnesses.map { (harness: $0, items: groupsByHarness[$0]!) }
            for (harness, items) in groupedCandidates {
                let isCollapsed = collapsedGroups.contains(harness)
                let visual = resolveHarnessVisual(harness, harnesses: map)
                let noun: String
                if items.contains(where: { $0.endpoint_pid != nil }) {
                    noun = "open"
                } else if visual.tier == "stable" && items.contains(where: { $0.session_id != "default" }) {
                    noun = "recent"
                } else {
                    noun = "available"
                }
                let groupTitle = "\(visual.name) · \(items.count) \(noun)"
                let groupRow = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 28))
                let groupControl = MenuControl(groupTitle, frame: NSRect(x: 8, y: 2, width: 364, height: 24), radius: 6)
                groupControl.identifier = NSUserInterfaceItemIdentifier("toggle_group:\(harness)")
                if visual.tier == "experimental" {
                    groupControl.toolTip = "\(visual.name) (Experimental)"
                }
                groupControl.style = .subtleHeader
                groupControl.symbolName = isCollapsed ? "chevron.right" : "chevron.down"
                let chev = NSImageView(frame: NSRect(x: 6, y: 7, width: 10, height: 10))
                chev.image = NSImage(systemSymbolName: isCollapsed ? "chevron.right" : "chevron.down", accessibilityDescription: nil)
                chev.contentTintColor = .secondaryLabelColor
                groupControl.addSubview(chev)
                groupControl.accessorySymbolView = chev
                let badge = makeHarnessBadge(NSRect(x: 20, y: 3, width: 18, height: 18),
                    radius: 5, iconInset: 3, harness: harness, harnesses: map)
                groupControl.addSubview(badge)
                groupControl.label.frame = NSRect(x: 44, y: 4, width: 310, height: 16)
                groupControl.label.font = .systemFont(ofSize: 11.5, weight: .semibold)
                groupControl.label.alignment = .left
                groupControl.pressed = { [weak self] in
                    guard let self = self else { return }
                    if self.collapsedGroups.contains(harness) { self.collapsedGroups.remove(harness) }
                    else { self.collapsedGroups.insert(harness) }
                    self.rebuild()
                }
                groupRow.addSubview(groupControl)
                _ = host(groupRow, id: "group:\(harness)")
                if !isCollapsed {
                    var candidateRows: [(id: String, view: NSView)] = []
                    for c in items {
                        let row = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 36))
                        let rowId = "candidate:\(c.harness):\(c.session_id)"
                        row.identifier = NSUserInterfaceItemIdentifier(rowId)
                        let shortCwd = c.cwd == home ? "~" : c.cwd.hasPrefix(home + "/") ? "~" + c.cwd.dropFirst(home.count) : c.cwd
                        let branch = (c.branch ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
                        let branchSuffix = (!branch.isEmpty && branch != "HEAD") ? " · \(branch)" : ""
                        let info = textLabel("\(c.title) · \(shortCwd)\(branchSuffix)", NSRect(x: 24, y: 9, width: 274, height: 18),
                            size: 12, weight: .regular, color: .labelColor)
                        info.lineBreakMode = .byTruncatingMiddle
                        info.toolTip = "\(c.title) — \(c.cwd)"
                        row.addSubview(info)
                        let listenBtn = MenuControl("Listen", frame: NSRect(x: 304, y: 7, width: 64, height: 22), leadingSymbol: "plus", radius: 6)
                        listenBtn.identifier = NSUserInterfaceItemIdentifier("listen:\(c.harness):\(c.session_id)")
                        listenBtn.toolTip = "Start listening in \(c.title)"
                        let listenRun = { [weak self, weak listenBtn] in
                            guard let self = self else { return }
                            self.pendingMutation = true
                            listenBtn?.setLeadingSymbol(nil)
                            listenBtn?.label.stringValue = "…"
                            listenBtn?.setAccessibilityLabel("…")
                            var action: [String: Any] = ["op": "listen_session", "harness": c.harness, "session_id": c.session_id, "cwd": c.cwd, "title": c.title]
                            if let pid = c.endpoint_pid { action["endpoint_pid"] = pid }
                            if self.hubs.count > 1 && !targetHub.isEmpty { action["hub"] = targetHub }
                            self.emit(action)
                        }
                        listenBtn.pressed = listenRun; row.addSubview(listenBtn)
                        candidateRows.append((id: rowId, view: row))
                    }
                    if candidateRows.count <= 4 {
                        for (id, row) in candidateRows { _ = host(row, id: id) }
                    } else {
                        let totalHeight = CGFloat(candidateRows.count) * 36
                        let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: 380, height: 144))
                        scroll.drawsBackground = false; scroll.hasVerticalScroller = true
                        scroll.hasHorizontalScroller = false; scroll.autohidesScrollers = true
                        let doc = FlippedView(frame: NSRect(x: 0, y: 0, width: 380, height: totalHeight))
                        for (idx, (_, row)) in candidateRows.enumerated() {
                            row.frame.origin = NSPoint(x: 0, y: CGFloat(idx) * 36)
                            doc.addSubview(row)
                        }
                        let scrollId = groupedCandidates.count == 1 ? "available_scroll" : "available_scroll:\(harness)"
                        scroll.documentView = doc; _ = host(scroll, id: scrollId)
                    }
                }
            }
        }
        // Zone 4 — Network Directory Launcher Row
        if !hubs.isEmpty {
            let dirRow = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 34))
            dirRow.addSubview(roundedView(NSRect(x: 12, y: 32, width: 356, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
            let openDir = MenuControl("Search Network Directory…", frame: NSRect(x: 10, y: 4, width: 360, height: 26),
                leadingSymbol: "magnifyingglass", radius: 7)
            openDir.identifier = NSUserInterfaceItemIdentifier("open_directory")
            openDir.label.font = .systemFont(ofSize: 11.5, weight: .medium)
            openDir.label.alignment = .left
            openDir.label.frame = NSRect(x: 24, y: 5, width: 230, height: 16)
            if unreadCount > 0 || directoryOnlineCount > 0 {
                let badgeText = unreadCount > 0
                    ? "🔔 \(unreadCount) unread · \(directoryOnlineCount) online"
                    : "\(directoryOnlineCount) online"
                let countBadge = textLabel(badgeText, NSRect(x: 210, y: 5, width: 142, height: 16),
                    size: 11, weight: unreadCount > 0 ? .medium : .regular, color: .secondaryLabelColor)
                countBadge.alignment = .right
                countBadge.identifier = NSUserInterfaceItemIdentifier("directory_online_badge")
                openDir.addSubview(countBadge)
            }
            openDir.pressed = { [weak self, weak menu] in
                guard let self = self else { return }
                menu?.cancelTracking()
                let initialMode = self.unreadCount > 0 ? "activity" : "directory"
                self.openDirectoryPanel(mode: initialMode)
                self.emit(["op": "query_directory", "query": self.directoryQuery, "scope": self.directoryScopeFilter, "harness": self.directoryHarnessFilter])
            }
            dirRow.addSubview(openDir)
            _ = host(dirRow, id: "directory_row")
        }
        let footer = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 36))
        if !listeners.isEmpty {
            let stopAll = MenuControl("Stop All", frame: NSRect(x: 12, y: 6, width: 82, height: 24), leadingSymbol: "stop.fill")
            stopAll.identifier = NSUserInterfaceItemIdentifier("stop_all")
            stopAll.pressed = { self.emit(["op": "unlisten_all"]) }
            footer.addSubview(stopAll)
        }
        let openSettings = MenuControl("Settings…", frame: NSRect(x: 168, y: 6, width: 88, height: 24), leadingSymbol: "gearshape")
        openSettings.identifier = NSUserInterfaceItemIdentifier("open_settings")
        openSettings.pressed = { [weak self, weak menu] in
            guard let self = self else { return }
            menu?.cancelTracking()
            self.openSettingsPanel()
        }
        footer.addSubview(openSettings)
        let settingsShortcut = NSMenuItem(title: "Settings…", action: #selector(openSettingsShortcut), keyEquivalent: ",")
        settingsShortcut.target = self
        settingsShortcut.keyEquivalentModifierMask = [.command]
        settingsShortcut.isHidden = true
        settingsShortcut.allowsKeyEquivalentWhenHidden = true
        menu.addItem(settingsShortcut)
        let quit = MenuControl("Quit Cito ⌘Q", frame: NSRect(x: 262, y: 6, width: 106, height: 24))
        quit.identifier = NSUserInterfaceItemIdentifier("quit")
        quit.pressed = { self.emit(["op": "quit"]) }
        footer.addSubview(quit)
        let footerItem = host(footer, id: "footer")
        footerItem.target = self; footerItem.action = #selector(quitMenu)
        footerItem.keyEquivalent = "q"; footerItem.keyEquivalentModifierMask = [.command]
        menu.update()
    }
    @objc func openSettingsShortcut() { openSettingsPanel() }
    @objc func quitMenu() { emit(["op": "quit"]) }

    // MARK: - Surface 3: Native AppKit Settings Panel (520 × 460 pt KeyPanel: NSPanel)

    func ensureSettingsPanel() {
        if settingsPanel == nil {
            let window = KeyPanel(contentRect: NSRect(x: 0, y: 0, width: 520, height: 460),
                                  styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
            window.level = .floating
            window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
            window.isFloatingPanel = true
            window.hidesOnDeactivate = false
            window.isOpaque = false
            window.backgroundColor = .clear
            window.hasShadow = true
            settingsPanel = window
        }
        rebuildSettingsPanel()
    }

    func syncAddHubFieldsFromControls() {
        if let nameField = settingsFieldControls["settings_hub_name"] { addHubName = nameField.stringValue }
        if let urlField = settingsFieldControls["settings_hub_url"] { addHubUrl = urlField.stringValue }
        if let tokenField = settingsFieldControls["settings_hub_token"] { addHubToken = tokenField.stringValue }
        if let userField = settingsFieldControls["settings_hub_user"] { addHubUser = userField.stringValue }
        if let issuerField = settingsFieldControls["settings_hub_issuer"] { addHubIssuer = issuerField.stringValue }
        if let clientField = settingsFieldControls["settings_hub_client_id"] { addHubClientId = clientField.stringValue }
    }

    func makeEditableField(_ id: String, frame: NSRect, placeholder: String, value: String, secret: Bool = false) -> NSTextField {
        let field: NSTextField = secret ? NSSecureTextField(frame: frame) : NSTextField(frame: frame)
        field.identifier = NSUserInterfaceItemIdentifier(id)
        field.placeholderString = placeholder
        field.stringValue = value
        field.isEditable = true
        field.isSelectable = true
        field.isBezeled = true
        field.bezelStyle = .roundedBezel
        field.font = .systemFont(ofSize: 12)
        field.delegate = self
        settingsFieldControls[id] = field
        return field
    }

    func handlePasteHubInvite() {
        let systemText = appPasteboard.string(forType: .string) ?? ""
        let raw = (systemText.isEmpty && headless ? clipboardString : systemText)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard !raw.isEmpty else { return }

        var parsedName: String?
        var parsedUrl: String?
        var parsedAuth = "none"
        var parsedToken: String?
        var parsedIssuer: String?
        var parsedClientId: String?
        var setDefault = true

        if raw.hasPrefix("http://") || raw.hasPrefix("https://") {
            parsedUrl = raw
            if let u = URL(string: raw), let host = u.host {
                let slug = host.lowercased().replacingOccurrences(of: "[^a-z0-9_-]", with: "-", options: .regularExpression)
                    .trimmingCharacters(in: CharacterSet(charactersIn: "-_"))
                parsedName = slug.isEmpty ? "lan" : String(slug.prefix(32))
            } else {
                parsedName = "lan"
            }
        } else {
            let parts = raw.split(whereSeparator: { $0.isWhitespace }).map(String.init)
            var i = 0
            while i < parts.count {
                let p = parts[i]
                if p == "--hub", i + 1 < parts.count { parsedName = parts[i + 1]; i += 2; continue }
                if p.hasPrefix("--hub=") { parsedName = String(p.dropFirst("--hub=".count)); i += 1; continue }
                if p == "--url", i + 1 < parts.count { parsedUrl = parts[i + 1]; i += 2; continue }
                if p.hasPrefix("--url=") { parsedUrl = String(p.dropFirst("--url=".count)); i += 1; continue }
                if p == "--auth", i + 1 < parts.count { parsedAuth = parts[i + 1]; i += 2; continue }
                if p.hasPrefix("--auth=") { parsedAuth = String(p.dropFirst("--auth=".count)); i += 1; continue }
                if p == "--token", i + 1 < parts.count { parsedToken = parts[i + 1]; i += 2; continue }
                if p.hasPrefix("--token=") { parsedToken = String(p.dropFirst("--token=".count)); i += 1; continue }
                if p == "--issuer", i + 1 < parts.count { parsedIssuer = parts[i + 1]; i += 2; continue }
                if p.hasPrefix("--issuer=") { parsedIssuer = String(p.dropFirst("--issuer=".count)); i += 1; continue }
                if p == "--client-id", i + 1 < parts.count { parsedClientId = parts[i + 1]; i += 2; continue }
                if p.hasPrefix("--client-id=") { parsedClientId = String(p.dropFirst("--client-id=".count)); i += 1; continue }
                if p == "--default" { setDefault = true; i += 1; continue }
                i += 1
            }
        }

        guard let name = parsedName, !name.isEmpty, let url = parsedUrl, !url.isEmpty else { return }
        var action: [String: Any] = [
            "op": "setup_save_hub",
            "name": name,
            "url": url,
            "auth_mode": parsedAuth,
            "set_default": setDefault
        ]
        if let token = parsedToken, !token.isEmpty { action["token"] = token }
        if let issuer = parsedIssuer, !issuer.isEmpty { action["issuer"] = issuer }
        if let clientId = parsedClientId, !clientId.isEmpty { action["client_id"] = clientId }
        emit(action)
    }

    func mountSettingsCards(_ content: NSView, in root: NSView) {
        let id = content.identifier?.rawValue ?? ""
        let minY = content.subviews.map { $0.frame.minY }.min() ?? 8
        let extra = max(0, 8 - minY)
        for child in content.subviews { child.frame.origin.y += extra }
        let viewport = content.frame
        content.frame = NSRect(x: 0, y: 0, width: viewport.width, height: viewport.height + extra)
        let scroll = NSScrollView(frame: NSRect(x: 0, y: 44, width: 520, height: onboardingGuideOpen ? 300 : 368))
        scroll.identifier = NSUserInterfaceItemIdentifier(id)
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        scroll.isHidden = content.isHidden || uninstallConfirmOpen
        content.isHidden = false
        scroll.documentView = content
        root.addSubview(scroll)
        let top = max(0, content.frame.height - scroll.contentSize.height)
        scroll.contentView.scroll(to: NSPoint(x: 0, y: top - min(top, settingsScrollOffsets[id] ?? 0)))
        scroll.reflectScrolledClipView(scroll.contentView)
    }

    func rebuildSettingsPanel() {
        guard let window = settingsPanel else { return }
        MenuControl.clearHoverWhere { $0.window === window }
        for case let scroll as NSScrollView in window.contentView?.subviews ?? [] {
            if let id = scroll.identifier?.rawValue {
                settingsScrollOffsets[id] = max(0,(scroll.documentView?.frame.height ?? 0)-scroll.contentSize.height-scroll.contentView.bounds.origin.y)
            }
        }
        settingsFieldControls.removeAll()

        let root = SurfaceView(frame: NSRect(x: 0, y: 0, width: 520, height: 460), radius: 14,
            color: .clear, border: .labelColor.withAppearanceAlpha(0.15))
        root.identifier = NSUserInterfaceItemIdentifier("settings_panel")
        root.layer?.masksToBounds = true

        let effect = NSVisualEffectView(frame: root.bounds)
        effect.material = .popover
        effect.blendingMode = .behindWindow
        effect.state = .active
        root.addSubview(effect)

        // Header & Segmented Tab Bar (520 × 48 pt, y: 412)
        let header = NSView(frame: NSRect(x: 0, y: 412, width: 520, height: 48))
        header.identifier = NSUserInterfaceItemIdentifier("settings_header")
        header.addSubview(textLabel("Cito", NSRect(x: 16, y: 15, width: 38, height: 18), size: 13, weight: .bold))

        let appsTab = MenuControl("Agent Apps", frame: NSRect(x: 58, y: 12, width: 118, height: 24), leadingSymbol: "square.grid.2x2", radius: 7)
        appsTab.identifier = NSUserInterfaceItemIdentifier("settings_tab:apps")
        appsTab.style = settingsActiveTab == "apps" ? .prominent : .neutral
        appsTab.pressed = { [weak self] in
            guard let self = self else { return }
            self.syncAddHubFieldsFromControls()
            self.uninstallConfirmOpen = false
            self.settingsActiveTab = "apps"
            self.rebuildSettingsPanel()
        }
        header.addSubview(appsTab)

        let channelsTab = MenuControl("Human Channels", frame: NSRect(x: 182, y: 12, width: 148, height: 24), leadingSymbol: "bubble.left.and.bubble.right", radius: 7)
        channelsTab.identifier = NSUserInterfaceItemIdentifier("settings_tab:channels")
        channelsTab.style = settingsActiveTab == "channels" ? .prominent : .neutral
        channelsTab.pressed = { [weak self] in
            guard let self = self else { return }
            self.syncAddHubFieldsFromControls()
            self.uninstallConfirmOpen = false
            self.settingsActiveTab = "channels"
            self.rebuildSettingsPanel()
        }
        header.addSubview(channelsTab)

        let hubsTab = MenuControl("Hubs & Network", frame: NSRect(x: 334, y: 12, width: 122, height: 24), leadingSymbol: "network", radius: 7)
        hubsTab.identifier = NSUserInterfaceItemIdentifier("settings_tab:hubs")
        hubsTab.style = settingsActiveTab == "hubs" ? .prominent : .neutral
        hubsTab.pressed = { [weak self] in
            guard let self = self else { return }
            self.syncAddHubFieldsFromControls()
            self.uninstallConfirmOpen = false
            self.settingsActiveTab = "hubs"
            self.rebuildSettingsPanel()
        }
        header.addSubview(hubsTab)

        let guideBtn=MenuControl("?",frame:NSRect(x:460,y:12,width:22,height:24),symbol:"questionmark.circle")
        guideBtn.identifier=NSUserInterfaceItemIdentifier("toggle_onboarding_guide")
        guideBtn.toolTip="Toggle Setup Guide"
        guideBtn.style = onboardingGuideOpen ? .prominent : .neutral
        guideBtn.pressed={ [weak self] in self?.onboardingGuideOpen.toggle(); self?.rebuildSettingsPanel() }
        header.addSubview(guideBtn)
        let closeBtn = MenuControl("Close", frame: NSRect(x: 486, y: 12, width: 24, height: 24), symbol: "xmark")
        closeBtn.identifier = NSUserInterfaceItemIdentifier("close_settings")
        closeBtn.toolTip = "Close Settings"
        closeBtn.pressed = { [weak self] in self?.closeSettingsPanel(emitAction: true) }
        header.addSubview(closeBtn)
        header.addSubview(roundedView(NSRect(x: 0, y: 0, width: 520, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
        root.addSubview(header)

        // Tab 1: Agent Apps (520 × 368 pt, y: 44)
        let appsView = NSView(frame: NSRect(x: 0, y: 44, width: 520, height: 368))
        appsView.identifier = NSUserInterfaceItemIdentifier("settings_apps_tab")
        appsView.isHidden = settingsActiveTab != "apps"

        let resolvedApps: [AppSetupItem] = settings?.apps ?? harnesses.map { h in
            AppSetupItem(id: h.id, display_name: h.display_name, tier: h.tier, installed: true,
                configured: false, status: "not_configured", detail: "Click Set Up to connect", paths: [])
        }
        appsView.addSubview(textLabel("CONNECTED AGENT APPS", NSRect(x:20,y:338,width:240,height:14),size:10,weight:.semibold,color:.secondaryLabelColor))
        appsView.addSubview(textLabel("Enable 1-click notification & wake support in your coding apps.", NSRect(x:20,y:320,width:480,height:15),size:11.5,color:.secondaryLabelColor))
        if onboardingGuideOpen {
            let banner=roundedView(NSRect(x:20,y:348,width:480,height:58),radius:8,color:.controlAccentColor.withAppearanceAlpha(0.10))
            banner.identifier=NSUserInterfaceItemIdentifier("onboarding_banner")
            banner.addSubview(textLabel("Quick Setup Guide — Click any step to jump:", NSRect(x:12,y:34,width:420,height:16), size:11,weight:.semibold))
            let dismiss=MenuControl("Close Guide",frame:NSRect(x:450,y:32,width:22,height:20),symbol:"xmark")
            dismiss.identifier=NSUserInterfaceItemIdentifier("dismiss_onboarding_guide")
            dismiss.pressed={ [weak self] in self?.onboardingGuideOpen=false; self?.rebuildSettingsPanel() }
            banner.addSubview(dismiss)
            for (idx,step) in [("apps","1. Agent Apps"),("channels","2. Human Channels →"),("hubs","3. Hubs & Network →")].enumerated() {
                let button=MenuControl(step.1,frame:NSRect(x:8+CGFloat(idx)*156,y:8,width:152,height:22),radius:6)
                button.identifier=NSUserInterfaceItemIdentifier("onboarding_step:\(step.0)")
                button.style = settingsActiveTab == step.0 ? .prominent : .neutral
                button.pressed={ [weak self] in self?.settingsActiveTab=step.0; self?.rebuildSettingsPanel() }
                banner.addSubview(button)
            }
            root.addSubview(banner)
        }
        if resolvedApps.contains(where:{$0.configured}) && !nextStepCalloutDismissed && !(settings?.channels?.contains(where:{$0.configured}) ?? false) {
            let callout=roundedView(NSRect(x:20,y:226-CGFloat(max(0,resolvedApps.count-1))*90-46,width:480,height:38),radius:8,color:.controlAccentColor.withAppearanceAlpha(0.10))
            callout.identifier=NSUserInterfaceItemIdentifier("apps_next_step_callout")
            let calloutLabel=textLabel("Agent app connected · Optional: add a Human Channel",NSRect(x:12,y:5,width:264,height:28),size:10)
            calloutLabel.maximumNumberOfLines=2; calloutLabel.lineBreakMode = .byWordWrapping
            callout.addSubview(calloutLabel)
            let next=MenuControl("Human Channels →",frame:NSRect(x:282,y:7,width:136,height:24),radius:6)
            next.identifier=NSUserInterfaceItemIdentifier("goto_channels_tab")
            next.pressed={ [weak self] in self?.settingsActiveTab="channels"; self?.rebuildSettingsPanel() }
            let skip=MenuControl("Skip",frame:NSRect(x:424,y:7,width:48,height:24),radius:6)
            skip.identifier=NSUserInterfaceItemIdentifier("skip_next_step")
            skip.pressed={ [weak self] in self?.nextStepCalloutDismissed=true; self?.rebuildSettingsPanel() }
            callout.addSubview(skip); callout.addSubview(next); appsView.addSubview(callout)
        }
        let map = harnessMap
        for (idx, app) in resolvedApps.enumerated() {
            let cardY = 226 - CGFloat(idx) * 90
            let card = roundedView(NSRect(x: 20, y: cardY, width: 480, height: 82), radius: 10,
                color: .labelColor.withAppearanceAlpha(0.06), border: .labelColor.withAppearanceAlpha(0.11))
            card.identifier = NSUserInterfaceItemIdentifier("settings_app:\(app.id)")
            card.addSubview(makeHarnessBadge(NSRect(x: 12, y: 43, width: 28, height: 28), radius: 7, iconInset: 6, harness: app.id, harnesses: map))

            let titleFont = NSFont.systemFont(ofSize: 13, weight: .semibold)
            let titleW = min(app.tier == "experimental" ? 136 : 210, ceil(measureTextWidth(app.display_name, font: titleFont)) + 6)
            card.addSubview(textLabel(app.display_name, NSRect(x: 50, y: 55, width: titleW, height: 17), size: 13, weight: .semibold))
            if app.tier == "experimental" {
                let expPill = roundedView(NSRect(x: 54 + titleW, y: 56, width: 88, height: 16), radius: 8, color: .systemOrange.withAppearanceAlpha(0.18))
                expPill.identifier = NSUserInterfaceItemIdentifier("settings_app_tier:\(app.id)")
                let expLabel = textLabel("Experimental", NSRect(x: 6, y: 1, width: 76, height: 14), size: 9.5, weight: .semibold, color: .systemOrange)
                expLabel.alignment = .center
                expPill.addSubview(expLabel)
                card.addSubview(expPill)
            }
            let detail = textLabel(app.detail, NSRect(x:50,y:8,width:418,height:30),size:11,color:.secondaryLabelColor)
            detail.maximumNumberOfLines=2; detail.lineBreakMode = .byWordWrapping
            detail.toolTip=([app.detail]+app.paths).joined(separator:"\n")
            card.addSubview(detail)

            let statusText = app.configured ? "● Connected" : (app.status == "repair_needed" ? "⚠ Repair Needed" : "○ Not Set Up")
            let statusFrame = app.configured ? NSRect(x: 230, y: 49, width: 82, height: 16) : NSRect(x: 294, y: 49, width: 100, height: 16)
            let statusLabel = textLabel(statusText, statusFrame,
                size: 11, weight: .medium, color: app.configured ? .systemGreen : (app.status == "repair_needed" ? .systemOrange : .secondaryLabelColor))
            statusLabel.alignment = .right
            statusLabel.identifier = NSUserInterfaceItemIdentifier("settings_app_status:\(app.id)")
            card.addSubview(statusLabel)

            let btnTitle = app.configured ? "Re-check" : (app.status == "repair_needed" ? "Repair" : "Set Up")
            let btnFrame = app.configured ? NSRect(x: 318, y: 45, width: 68, height: 24) : NSRect(x: 402, y: 45, width: 66, height: 24)
            let btn = MenuControl(btnTitle, frame: btnFrame, radius: 6)
            btn.identifier = NSUserInterfaceItemIdentifier("setup_app:\(app.id)")
            btn.style = app.configured ? .neutral : .prominent
            let appId = app.id
            btn.pressed = { [weak self] in
                self?.emit(["op": "setup_apply", "harness": appId])
            }
            card.addSubview(btn)

            if app.configured {
                let removeBtn = MenuControl("Disconnect", frame: NSRect(x: 392, y: 45, width: 76, height: 24), radius: 6)
                removeBtn.identifier = NSUserInterfaceItemIdentifier("remove_app:\(app.id)")
                removeBtn.style = .destructiveHover
                removeBtn.pressed = { [weak self] in
                    self?.emit(["op": "setup_remove_app", "harness": appId])
                }
                card.addSubview(removeBtn)
            }
            appsView.addSubview(card)
        }
        mountSettingsCards(appsView, in: root)

        // Tab 2: Human Channels (520 × 368 pt, y: 44)
        let channelsView = NSView(frame: NSRect(x: 0, y: 44, width: 520, height: 368))
        channelsView.identifier = NSUserInterfaceItemIdentifier("settings_channels_tab")
        channelsView.isHidden = settingsActiveTab != "channels"

        let bannerEnabled = settings?.desktop_banner_enabled ?? true
        let bannerCard = roundedView(NSRect(x: 20, y: 282, width: 480, height: 48), radius: 10,
            color: .labelColor.withAppearanceAlpha(0.06), border: .labelColor.withAppearanceAlpha(0.11))
        bannerCard.identifier = NSUserInterfaceItemIdentifier("settings_desktop_banner_card")
        bannerCard.addSubview(textLabel("Mac Desktop Banner", NSRect(x: 14, y: 25, width: 240, height: 17), size: 13, weight: .semibold))
        bannerCard.addSubview(textLabel("Show floating HUD wake banners on this Mac when signals arrive.", NSRect(x: 14, y: 7, width: 350, height: 15), size: 11, color: .secondaryLabelColor))
        let toggleBanner = MenuControl(bannerEnabled ? "● Enabled" : "Off", frame: NSRect(x: 386, y: 12, width: 80, height: 24), radius: 12)
        toggleBanner.identifier = NSUserInterfaceItemIdentifier("toggle_desktop_banner")
        toggleBanner.style = bannerEnabled ? .prominent : .neutral
        toggleBanner.pressed = { [weak self] in
            self?.emit(["op": "set_desktop_banner", "enabled": !bannerEnabled])
        }
        bannerCard.addSubview(toggleBanner)
        channelsView.addSubview(bannerCard)

        channelsView.addSubview(textLabel("TARGET HUB:", NSRect(x: 20, y: 342, width: 78, height: 14), size: 10, weight: .semibold, color: .secondaryLabelColor))
        let selector = NSScrollView(frame: NSRect(x: 102, y: 336, width: 398, height: 28))
        selector.drawsBackground = false
        selector.hasHorizontalScroller = false
        let selectorContent = NSView(frame: NSRect(x: 0, y: 0, width: 480, height: 28))
        var selectorX: CGFloat = 0
        let channelHubs = settings?.hubs.map { $0.name } ?? hubs.map { $0.name }
        for name in channelHubs {
            let width = max(64, measureTextWidth(name, font: .systemFont(ofSize: 11)) + 24)
            let pill = MenuControl(name, frame: NSRect(x: selectorX, y: 3, width: width, height: 22), radius: 11)
            pill.identifier = NSUserInterfaceItemIdentifier("channel_hub_selector:\(name)")
            pill.style = effectiveChannelHub == name ? .prominent : .neutral
            pill.pressed = { [weak self] in
                guard let self = self else { return }
                self.selectedChannelHub = name
                self.emit(["op": "open_settings", "hub": name])
                self.rebuildSettingsPanel()
            }
            selectorContent.addSubview(pill)
            selectorX += width + 6
        }
        selectorContent.setFrameSize(NSSize(width: max(480, selectorX), height: 28))
        selector.documentView = selectorContent
        channelsView.addSubview(selector)

        let resolvedChannels = channelSettingsByHub[effectiveChannelHub] ?? []
        var chCursorY: CGFloat = 274
        for ch in resolvedChannels {
            let visibleFields = ch.fields.filter { $0.hidden != true }
            let instruction = channelInstructions["\(effectiveChannelHub)|\(ch.id)"] ?? ch.pairing_instruction ?? ""
            let instructionHeight = instruction.isEmpty ? CGFloat(0) : max(32, ceil((instruction as NSString).boundingRect(with: NSSize(width: 278, height: CGFloat.greatestFiniteMagnitude), options: [.usesLineFragmentOrigin], attributes: [.font: NSFont.monospacedSystemFont(ofSize: 11, weight: .semibold)]).height) + 4)
            let guide=(ch.setup_guide ?? []).joined(separator:"\n")
            let hubFields = visibleFields.filter { $0.scope == "hub" }
            let userFields = visibleFields.filter { $0.scope != "hub" }
            let hasHubScopeFields = ch.fields.contains { $0.scope == "hub" }
            let hubFieldsH = hubFields.reduce(CGFloat(0)) { $0 + ($1.help_text?.isEmpty == false ? 38 : 26) }
            let userFieldsH = userFields.reduce(CGFloat(0)) { $0 + ($1.help_text?.isEmpty == false ? 38 : 26) }
            let guideH: CGFloat = !ch.configured && !guide.isEmpty && !hubFields.isEmpty ? max(30,ceil((guide as NSString).boundingRect(with:NSSize(width:432,height:CGFloat.greatestFiniteMagnitude),options:[.usesLineFragmentOrigin],attributes:[.font:NSFont.systemFont(ofSize:10)]).height)+2) : 0
            let currentUser=settings?.hubs.first(where:{$0.name==effectiveChannelHub})?.user ?? hubs.first(where:{$0.name==effectiveChannelHub})?.user ?? "local"
            let gatewayRole = !hubFields.isEmpty ? (guideH > 0 ? "" : "Configured once by the Hub owner. Shared by all human nodes on '\(effectiveChannelHub)'.") :
                (hasHubScopeFields ? (ch.hub_provided ? "Provided by '\(effectiveChannelHub)' Hub administrator — no gateway setup needed on your Mac." : "Not yet configured by the '\(effectiveChannelHub)' Hub administrator.") : "No shared gateway credentials required for this channel.")
            let personalRole="Bind your personal \(ch.display_name) chat to receive wakes and reply on '\(effectiveChannelHub)'."
            func roleHeight(_ text: String) -> CGFloat {
                text.isEmpty ? 0 : max(13,ceil((text as NSString).boundingRect(with:NSSize(width:432,height:CGFloat.greatestFiniteMagnitude),options:[.usesLineFragmentOrigin],attributes:[.font:NSFont.systemFont(ofSize:10)]).height))
            }
            let gatewayRoleH=roleHeight(gatewayRole), personalRoleH=roleHeight(personalRole)
            let hubStratumH: CGFloat = 26 + gatewayRoleH + guideH + hubFieldsH + 6
            let userStratumH: CGFloat = 26 + personalRoleH + userFieldsH + (instructionHeight > 0 ? instructionHeight + 6 : 0) + 34
            let cardHeight: CGFloat = 42 + hubStratumH + 6 + userStratumH + 6
            chCursorY -= cardHeight
            let chCard = roundedView(NSRect(x: 20, y: chCursorY, width: 480, height: cardHeight), radius: 10,
                color: .labelColor.withAppearanceAlpha(0.06), border: .labelColor.withAppearanceAlpha(0.12))
            chCard.identifier = NSUserInterfaceItemIdentifier("settings_channel:\(ch.id)")

            let chBadge = roundedView(NSRect(x: 12, y: cardHeight - 34, width: 24, height: 24), radius: 6,
                color: tintColor(ch.tint).withAppearanceAlpha(0.88), border: .white.withAlphaComponent(0.18))
            let chIcon = NSImageView(frame: NSRect(x: 5, y: 5, width: 14, height: 14))
            chIcon.image = NSImage(systemSymbolName: ch.sf_symbol, accessibilityDescription: ch.display_name)
            chIcon.contentTintColor = .white
            chBadge.addSubview(chIcon)
            chCard.addSubview(chBadge)

            chCard.addSubview(textLabel(ch.display_name, NSRect(x: 44, y: cardHeight - 21, width: 180, height: 16), size: 12.5, weight: .semibold))
            let channelResult = channelResults["\(effectiveChannelHub)|\(ch.id)"]
            let chStatusText:String
            let chStatusColor:NSColor
            switch channelResult?.ok == false ? "error" : ch.status {
            case "ready": chStatusText = ch.display_label.flatMap { $0.isEmpty ? nil : "● Connected (\($0))" } ?? "● Connected"; chStatusColor = .systemGreen
            case "paused": chStatusText = "⏸ Paused"; chStatusColor = .systemOrange
            case "pairing_pending": chStatusText = "◌ Pairing…"; chStatusColor = .systemBlue
            case "error": chStatusText = "⚠ Error"; chStatusColor = .systemRed
            default: chStatusText = ch.hub_provided || ch.status == "hub_ready" ? "Hub Service: ● Ready" : "○ Not Configured"; chStatusColor = .secondaryLabelColor
            }
            let chStatusLabel = textLabel(chStatusText, NSRect(x: 240, y: cardHeight - 20, width: 226, height: 15), size: 11, weight: .medium, color: chStatusColor)
            chStatusLabel.alignment = .right
            chStatusLabel.identifier = NSUserInterfaceItemIdentifier("settings_channel_status:\(ch.id)")
            chCard.addSubview(chStatusLabel)

            let chDetail = textLabel(channelResult?.detail ?? ch.detail, NSRect(x: 44, y: cardHeight - 36, width: 422, height: 14), size: 10.5, color: .secondaryLabelColor)
            chDetail.identifier = NSUserInterfaceItemIdentifier("settings_channel_detail:\(ch.id)")
            chCard.addSubview(chDetail)

            let hubStratum = roundedView(NSRect(x:12,y:6+userStratumH+6,width:456,height:hubStratumH),radius:8,
                color:.controlAccentColor.withAppearanceAlpha(0.06),border:.controlAccentColor.withAppearanceAlpha(0.14))
            hubStratum.identifier=NSUserInterfaceItemIdentifier("stratum_hub:\(ch.id)")
            let hubTitle=textLabel("1. HUB GATEWAY · ONCE PER HUB",NSRect(x:12,y:hubStratumH-19,width:220,height:13),size:9.5,weight:.bold,color:.secondaryLabelColor)
            hubTitle.identifier=NSUserInterfaceItemIdentifier("stratum_hub_title:\(ch.id)")
            hubStratum.addSubview(hubTitle)
            let gatewayActive=ch.hub_provided || ch.status != "not_configured"
            let gatewayStatus=textLabel(gatewayActive ? "● Hub Gateway Active" : "○ Gateway Token Required",
                gatewayActive ? NSRect(x:236,y:hubStratumH-19,width:132,height:13) : NSRect(x:280,y:hubStratumH-19,width:164,height:13),size:9.5,weight:.medium,color:gatewayActive ? .systemGreen : .secondaryLabelColor)
            gatewayStatus.alignment = gatewayActive ? .left : .right
            gatewayStatus.identifier=NSUserInterfaceItemIdentifier("stratum_hub_status:\(ch.id)")
            hubStratum.addSubview(gatewayStatus)
            if ch.fields.contains(where:{$0.scope=="hub" && $0.configured && $0.hidden != true}) {
                let clear=MenuControl("Clear Token",frame:NSRect(x:372,y:hubStratumH-22,width:76,height:19),radius:6)
                clear.style = .destructiveHover
                clear.identifier=NSUserInterfaceItemIdentifier("clear_channel_service:\(ch.id)")
                clear.pressed={ [weak self] in guard let self=self else{return}; self.emit(["op":"disconnect_human_channel","hub":self.effectiveChannelHub,"channel_id":ch.id,"reset_hub":true,"reset_user":true]) }
                hubStratum.addSubview(clear)
            }
            if gatewayRoleH > 0 {
                let gatewayDescription=textLabel(gatewayRole,NSRect(x:12,y:hubStratumH-21-gatewayRoleH,width:432,height:gatewayRoleH),size:10,color:.secondaryLabelColor)
                gatewayDescription.maximumNumberOfLines=0; gatewayDescription.lineBreakMode = .byWordWrapping
                hubStratum.addSubview(gatewayDescription)
            }
            if guideH>0 {
                let box=textLabel(guide,NSRect(x:12,y:hubStratumH-21-gatewayRoleH-guideH,width:432,height:guideH),size:10,color:.secondaryLabelColor)
                box.identifier=NSUserInterfaceItemIdentifier("channel_setup_guide:\(ch.id)")
                box.maximumNumberOfLines=0; box.lineBreakMode = .byWordWrapping
                hubStratum.addSubview(box)
            }
            let userStratum=roundedView(NSRect(x:12,y:6,width:456,height:userStratumH),radius:8,
                color:.labelColor.withAppearanceAlpha(0.04),border:.labelColor.withAppearanceAlpha(0.10))
            userStratum.identifier=NSUserInterfaceItemIdentifier("stratum_user:\(ch.id)")
            let userTitle=textLabel("2. YOUR PERSONAL NODE (@\(currentUser)) · EACH HUMAN ON '\(effectiveChannelHub.uppercased())'",NSRect(x:12,y:userStratumH-19,width:432,height:13),size:9.5,weight:.bold,color:.secondaryLabelColor)
            userTitle.identifier=NSUserInterfaceItemIdentifier("stratum_user_title:\(ch.id)")
            userStratum.addSubview(userTitle)
            let personalDescription=textLabel(personalRole,NSRect(x:12,y:userStratumH-21-personalRoleH,width:432,height:personalRoleH),size:10,color:.secondaryLabelColor)
            personalDescription.maximumNumberOfLines=0; personalDescription.lineBreakMode = .byWordWrapping
            userStratum.addSubview(personalDescription)
            for (container, fields, top) in [(hubStratum,hubFields,hubStratumH-23-gatewayRoleH-guideH),(userStratum,userFields,userStratumH-23-personalRoleH)] {
                var fieldY=top-22
                for f in fields {
                    let fieldId="channel_field:\(ch.id):\(f.key)"
                    let labelView=textLabel(f.label,NSRect(x:12,y:fieldY+3,width:140,height:15),size:11,weight:.medium)
                    labelView.toolTip=f.label; container.addSubview(labelView)
                    let initialVal=channelFieldDrafts[draftKey(fieldId)] ?? (f.value ?? "")
                    let inputField=makeEditableField(fieldId,frame:NSRect(x:158,y:fieldY,width:286,height:22),placeholder:f.type=="secret" && f.configured ? (f.configured_placeholder ?? "•••••••• (Saved — enter new value to replace)") : f.placeholder,value:initialVal,secret:f.type=="secret")
                    if f.type=="secret" { channelSecretOriginal[draftKey(fieldId)]=f.value ?? "" }
                    inputField.toolTip=f.help_text; container.addSubview(inputField)
                    if let help=f.help_text, !help.isEmpty {
                        container.addSubview(textLabel(help,NSRect(x:158,y:fieldY-13,width:286,height:12),size:9.5,color:.secondaryLabelColor))
                        fieldY-=12
                    }
                    fieldY-=26
                }
            }
            chCard.addSubview(hubStratum); chCard.addSubview(userStratum)
            // Action and instruction coordinates are anchored to the outer card's grid.
            func addPersonalControl(_ view: NSView) {
                view.frame = view.frame.offsetBy(dx:-12,dy:-4)
                userStratum.addSubview(view)
            }
            if !instruction.isEmpty {
                let instrLabel = textLabel(instruction, NSRect(x: 14, y: 40, width: 278, height: instructionHeight), size: 11.5, weight: .semibold, color: .systemBlue)
                instrLabel.maximumNumberOfLines = 0
                instrLabel.lineBreakMode = .byWordWrapping
                instrLabel.isSelectable = true
                instrLabel.toolTip = instruction
                instrLabel.font = .monospacedSystemFont(ofSize: 11.5, weight: .semibold)
                instrLabel.identifier = NSUserInterfaceItemIdentifier("pairing_instruction:\(ch.id)")
                addPersonalControl(instrLabel)
            }

            let chId = ch.id
            let collectValues: () -> [String: String] = { [weak self] in
                guard let self = self else { return [:] }
                self.syncAddHubFieldsFromControls()
                var vals: [String: String] = [:]
                for f in ch.fields where f.hidden != true {
                    let fid = "channel_field:\(chId):\(f.key)"
                    let draft=self.channelFieldDrafts[self.draftKey(fid)]
                    if f.type == "secret" && f.configured {
                        if let d=draft, !d.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty, d != self.channelSecretOriginal[self.draftKey(fid)] { vals[f.key]=d }
                        else if draft == nil, let value=f.value, !value.isEmpty { vals[f.key]=value }
                    } else {
                        vals[f.key]=draft ?? self.settingsFieldControls[fid]?.stringValue ?? f.value ?? ""
                    }
                }
                return vals
            }

            if ch.pairing_supported {
                let pairLabel = ch.configured ? "Re-pair…" : (ch.pairing_action_label ?? "Pair Account…")
                let pairBtn = MenuControl(pairLabel, frame: NSRect(x: 14, y: 10, width: 112, height: 24), leadingSymbol: "link", radius: 6)
                pairBtn.toolTip=ch.pairing_help_text
                pairBtn.identifier = NSUserInterfaceItemIdentifier("pair_channel:\(ch.id)")
                pairBtn.pressed = { [weak self] in
                    guard let self = self else { return }
                    self.emit(["op": "channel_pair", "hub": self.effectiveChannelHub, "channel_id": chId, "values": collectValues()])
                }
                addPersonalControl(pairBtn)
            }

            if !instruction.isEmpty {
                let copy=MenuControl("Copy Command",frame:NSRect(x:298,y:42,width:114,height:22),leadingSymbol:"doc.on.doc",radius:6)
                copy.label.font = .systemFont(ofSize:10.5)
                copy.identifier=NSUserInterfaceItemIdentifier("copy_pair_command:\(ch.id)")
                copy.pressed={ [weak self] in
                    let command=instruction.range(of:"/start cito-[a-zA-Z0-9]+",options:.regularExpression).map { String(instruction[$0]) } ?? instruction
                    self?.clipboardString=command; appPasteboard.clearContents(); appPasteboard.setString(command,forType:.string)
                }
                addPersonalControl(copy)
                let cancel=MenuControl("Cancel",frame:NSRect(x:416,y:42,width:50,height:22),radius:6)
                cancel.identifier=NSUserInterfaceItemIdentifier("cancel_pair_channel:\(ch.id)")
                cancel.pressed={ [weak self] in
                    guard let self=self else{return}
                    self.channelInstructions.removeValue(forKey:self.effectiveChannelHub+"|"+chId)
                    self.emit(["op":"channel_cancel_pair","hub":self.effectiveChannelHub,"channel_id":chId])
                }
                addPersonalControl(cancel)
            }
            if ch.configured {
                let toggle=MenuControl(ch.enabled ? "Pause" : "Resume",frame:NSRect(x:132,y:10,width:64,height:24),radius:6)
                toggle.identifier=NSUserInterfaceItemIdentifier("toggle_channel:\(ch.id)")
                toggle.pressed={ [weak self] in guard let self=self else{return}; self.emit(["op":"channel_toggle","hub":self.effectiveChannelHub,"channel_id":chId,"enabled":!ch.enabled]) }
                addPersonalControl(toggle)
                let unpair=MenuControl("Unpair",frame:NSRect(x:202,y:10,width:62,height:24),radius:6)
                unpair.style = .destructiveHover
                unpair.identifier=NSUserInterfaceItemIdentifier("unpair_channel:\(ch.id)")
                unpair.pressed={ [weak self] in guard let self=self else{return}; self.emit(["op":"channel_unpair","hub":self.effectiveChannelHub,"channel_id":chId,"reset_user":true]) }
                addPersonalControl(unpair)
            } else {
                let skip=MenuControl("Skip for Now",frame:NSRect(x:132,y:10,width:96,height:24),radius:6)
                skip.identifier=NSUserInterfaceItemIdentifier("skip_channel:\(ch.id)")
                skip.pressed={ [weak self] in self?.nextStepCalloutDismissed=true; self?.settingsActiveTab="hubs"; self?.rebuildSettingsPanel() }
                addPersonalControl(skip)
            }
            let testBtn = MenuControl("Send Test", frame: NSRect(x: 312, y: 10, width: 80, height: 24), radius: 6)
            testBtn.identifier = NSUserInterfaceItemIdentifier("test_channel:\(ch.id)")
            testBtn.pressed = { [weak self] in
                guard let self = self else { return }
                self.emit(["op": "channel_test", "hub": self.effectiveChannelHub, "channel_id": chId, "values": collectValues()])
            }
            addPersonalControl(testBtn)

            let saveBtn = MenuControl("Save", frame: NSRect(x: 398, y: 10, width: 68, height: 24), radius: 6)
            saveBtn.identifier = NSUserInterfaceItemIdentifier("save_channel:\(ch.id)")
            saveBtn.style = .prominent
            saveBtn.pressed = { [weak self] in
                guard let self = self else { return }
                let values = collectValues()
                self.emit(["op": "channel_save", "hub": self.effectiveChannelHub, "channel_id": chId, "enabled": true, "values": values])
            }
            addPersonalControl(saveBtn)

            channelsView.addSubview(chCard)
            chCursorY -= 10
        }
        mountSettingsCards(channelsView, in: root)

        // Tab 3: Hubs & Network (520 × 368 pt, y: 44)
        let hubsView = NSView(frame: NSRect(x: 0, y: 44, width: 520, height: 368))
        hubsView.identifier = NSUserInterfaceItemIdentifier("settings_hubs_tab")
        hubsView.isHidden = settingsActiveTab != "hubs"

        let hostHub = settings?.host_hub ?? HostHubSummary(
            enabled: false, running: false, port: 4291,
            endpoint_url: "http://127.0.0.1:4291", token: "",
            invite_command: "cito setup --hub lan --url http://127.0.0.1:4291 --default", auth_mode: nil, error: nil
        )
        let hostCard = roundedView(NSRect(x: 20, y: 232, width: 480, height: 124), radius: 10,
            color: .labelColor.withAppearanceAlpha(0.06), border: .labelColor.withAppearanceAlpha(0.12))
        hostCard.identifier = NSUserInterfaceItemIdentifier("host_hub_card")
        hostCard.addSubview(textLabel("Share Hub on Local Network", NSRect(x: 14, y: 94, width: 260, height: 18), size: 13, weight: .semibold))
        hostCard.addSubview(textLabel("Let your other Macs or teammates connect to this Mac.", NSRect(x: 14, y: 74, width: 360, height: 15), size: 11, color: .secondaryLabelColor))

        let toggleTitle = hostHub.enabled ? "● Sharing" : "Off"
        let toggleHostBtn = MenuControl(toggleTitle, frame: NSRect(x: 396, y: 88, width: 70, height: 24), radius: 12)
        toggleHostBtn.identifier = NSUserInterfaceItemIdentifier("toggle_host_hub")
        toggleHostBtn.style = hostHub.enabled ? .prominent : .neutral
        let nextHostEnabled = !hostHub.enabled
        toggleHostBtn.pressed = { [weak self] in
            self?.emit(["op": "set_host_hub", "enabled": nextHostEnabled])
        }
        hostCard.addSubview(toggleHostBtn)

        let authMode=hostHub.auth_mode ?? (hostHub.token.isEmpty ? "none" : "token")
        let showToken=authMode == "token" && !hostHub.token.isEmpty
        let endpointLabel = textLabel(hostHub.endpoint_url, NSRect(x: 14, y: 46, width: showToken ? 254 : 452, height: 16),
            size: 11.5, weight: .medium, color: hostHub.enabled ? .labelColor : .secondaryLabelColor)
        endpointLabel.font = .monospacedSystemFont(ofSize: 11.5, weight: .medium)
        endpointLabel.identifier = NSUserInterfaceItemIdentifier("host_hub_endpoint")
        hostCard.addSubview(endpointLabel)
        if showToken {
            let token=MenuControl("Token: \(String(hostHub.token.prefix(8)))… Copy",frame:NSRect(x:274,y:44,width:192,height:20),leadingSymbol:"key.fill",radius:6)
            token.identifier=NSUserInterfaceItemIdentifier("copy_host_hub_token")
            token.pressed={ [weak self, weak token] in
                self?.clipboardString=hostHub.token
                appPasteboard.clearContents(); appPasteboard.setString(hostHub.token,forType:.string)
                token?.copied("✓ Token Copied")
            }
            hostCard.addSubview(token)
        }

        let copyInvite = MenuControl("Copy 1-Click Join Code", frame: NSRect(x: 14, y: 12, width: 168, height: 24), leadingSymbol: "doc.on.doc", radius: 6)
        copyInvite.identifier = NSUserInterfaceItemIdentifier("copy_hub_invite")
        let inviteCmd = hostHub.invite_command
        copyInvite.pressed = { [weak self, weak copyInvite] in
            guard let self = self else { return }
            self.clipboardString = inviteCmd
            appPasteboard.clearContents()
            appPasteboard.setString(inviteCmd, forType: .string)
            copyInvite?.copied("✓ Copied")
        }
        hostCard.addSubview(copyInvite)
        hostCard.addSubview(textLabel("Port:",NSRect(x:188,y:16,width:34,height:15),size:11))
        hostCard.addSubview(makeEditableField("host_hub_port",frame:NSRect(x:222,y:12,width:58,height:24),placeholder:"Port",value:"\(hostHub.port)"))
        let auth=MenuControl(authMode=="none" ? "Auth: None" : "Auth: Token",frame:NSRect(x:286,y:12,width:102,height:24),radius:6)
        auth.identifier=NSUserInterfaceItemIdentifier("toggle_host_hub_auth")
        auth.pressed={ [weak self] in
            guard let self=self else{return}
            let port=Int(self.settingsFieldControls["host_hub_port"]?.stringValue ?? "") ?? hostHub.port
            self.emit(["op":"set_host_hub","enabled":hostHub.enabled,"port":port,"auth_mode":authMode=="none" ? "token" : "none"])
        }
        hostCard.addSubview(auth)
        let apply=MenuControl("Apply",frame:NSRect(x:394,y:12,width:72,height:24),radius:6)
        apply.identifier=NSUserInterfaceItemIdentifier("apply_host_hub_port")
        if hostHubPortSaved { apply.copied("✓ Saved"); hostHubPortSaved=false }
        apply.pressed={ [weak self] in
            guard let self=self else{return}
            let raw=(self.settingsFieldControls["host_hub_port"]?.stringValue ?? "").trimmingCharacters(in:.whitespacesAndNewlines)
            guard let port=Int(raw), (1...65535).contains(port) else {
                self.hostHubPortError="Port must be an integer from 1 to 65535."
                self.hostHubPortSaved=false
                self.rebuildSettingsPanel()
                return
            }
            self.hostHubPortError=nil; self.hostHubPortPending=true; self.hostHubPortSaved=false
            self.emit(["op":"set_host_hub","enabled":hostHub.enabled,"port":port])
        }
        hostCard.addSubview(apply)
        if let error=hostHubPortError ?? hostHub.error, !error.isEmpty {
            let label=textLabel(error,NSRect(x:14,y:62,width:452,height:14),size:10,color:.systemRed)
            label.identifier=NSUserInterfaceItemIdentifier("host_hub_error"); hostCard.addSubview(label)
        }
        hubsView.addSubview(hostCard)

        hubsView.addSubview(textLabel("CONNECTED HUBS", NSRect(x: 20, y: 194, width: 180, height: 14),
            size: 10, weight: .semibold, color: .secondaryLabelColor))

        let pasteBtn = MenuControl("Paste Invite", frame: NSRect(x: 272, y: 190, width: 116, height: 22), leadingSymbol: "doc.on.clipboard", radius: 6)
        pasteBtn.identifier = NSUserInterfaceItemIdentifier("paste_hub_invite")
        pasteBtn.pressed = { [weak self] in self?.handlePasteHubInvite() }
        hubsView.addSubview(pasteBtn)

        let addBtn = MenuControl("Add Hub…", frame: NSRect(x: 396, y: 190, width: 104, height: 22), leadingSymbol: "plus", radius: 6)
        addBtn.identifier = NSUserInterfaceItemIdentifier("open_add_hub")
        addBtn.pressed = { [weak self] in
            guard let self = self else { return }
            let wasEditing = self.editingExistingHub
            self.editingExistingHub=false
            self.editingOriginalHub=""
            self.addHubName=""; self.addHubUrl=""; self.addHubToken=""; self.addHubUser=""; self.addHubIssuer=""; self.addHubClientId=""; self.addHubAuth="none"
            self.addHubFormOpen = wasEditing ? true : !self.addHubFormOpen
            self.rebuildSettingsPanel()
        }
        hubsView.addSubview(addBtn)

        if addHubFormOpen {
            let form = roundedView(NSRect(x: 20, y: 4, width: 480, height: 184), radius: 10,
                color: .labelColor.withAppearanceAlpha(0.06), border: .labelColor.withAppearanceAlpha(0.12))
            form.identifier = NSUserInterfaceItemIdentifier("add_hub_form")

            func fieldLabel(_ title: String, _ frame: NSRect) {
                form.addSubview(textLabel(title,frame,size:10,weight:.semibold,color:.secondaryLabelColor))
            }
            if editingExistingHub && editingOriginalHub == "local" {
                form.addSubview(textLabel("Edit Built-in Local Hub (local://)",NSRect(x:14,y:156,width:452,height:18),size:12.5,weight:.semibold))
                form.addSubview(textLabel("Runs in-process on this Mac (SQLite-backed). No external server or auth needed.",NSRect(x:14,y:134,width:452,height:16),size:11,color:.secondaryLabelColor))
                fieldLabel("YOUR DISPLAY HANDLE (@USERNAME)",NSRect(x:14,y:104,width:452,height:14))
                form.addSubview(makeEditableField("settings_hub_user",frame:NSRect(x:14,y:76,width:452,height:24),placeholder:"e.g. alice",value:addHubUser))
                form.addSubview(textLabel("Shown to local agent sessions when you send or receive signals on this Mac.",NSRect(x:14,y:54,width:452,height:14),size:10.5,color:.secondaryLabelColor))
                for (id,value) in [("settings_hub_name","local"),("settings_hub_url","local://")] {
                    let field=makeEditableField(id,frame:.zero,placeholder:"",value:value)
                    field.isHidden=true; form.addSubview(field)
                }
            } else {
                fieldLabel("HUB NAME",NSRect(x:14,y:164,width:146,height:13))
                fieldLabel("HUB URL (http://ip:4291 or https://host)",NSRect(x:168,y:164,width:298,height:13))
                form.addSubview(makeEditableField("settings_hub_name",frame:NSRect(x:14,y:138,width:146,height:24),placeholder:"e.g. team or office-mac",value:addHubName))
                form.addSubview(makeEditableField("settings_hub_url",frame:NSRect(x:168,y:138,width:298,height:24),placeholder:"http://192.168.1.10:4291",value:addHubUrl))
                fieldLabel("AUTH:",NSRect(x:14,y:110,width:42,height:14))
                let authOptions: [(String,String,CGFloat,CGFloat)] = [("none","Trusted / Tailscale",60,132),("token","Shared Token",198,104),("oidc","Team OIDC",308,92)]
                for (mode,label,x,w) in authOptions {
                    let pill=MenuControl(label,frame:NSRect(x:x,y:106,width:w,height:22),radius:11)
                    pill.identifier=NSUserInterfaceItemIdentifier("settings_hub_auth:\(mode)")
                    pill.style = addHubAuth == mode ? .prominent : .neutral
                    pill.pressed={ [weak self] in
                        guard let self=self else{return}
                        self.syncAddHubFieldsFromControls(); self.addHubAuth=mode; self.rebuildSettingsPanel()
                    }
                    form.addSubview(pill)
                }
                if addHubAuth == "none" {
                    fieldLabel("YOUR DISPLAY HANDLE (@USERNAME, OPTIONAL)",NSRect(x:14,y:82,width:452,height:14))
                    form.addSubview(makeEditableField("settings_hub_user",frame:NSRect(x:14,y:56,width:452,height:24),placeholder:"Defaults to your Mac username",value:addHubUser))
                } else if addHubAuth == "token" {
                    fieldLabel("SHARED ACCESS TOKEN",NSRect(x:14,y:82,width:274,height:14))
                    fieldLabel("YOUR HANDLE (OPTIONAL)",NSRect(x:296,y:82,width:170,height:14))
                    form.addSubview(makeEditableField("settings_hub_token",frame:NSRect(x:14,y:56,width:274,height:24),placeholder:editingExistingHub ? "•••••• (Saved — enter new token to replace)" : "Paste token from host Mac",value:addHubToken,secret:true))
                    form.addSubview(makeEditableField("settings_hub_user",frame:NSRect(x:296,y:56,width:170,height:24),placeholder:"e.g. alice",value:addHubUser))
                } else if addHubAuth == "oidc" {
                    fieldLabel("OIDC ISSUER URL",NSRect(x:14,y:82,width:260,height:14))
                    fieldLabel("OIDC CLIENT ID",NSRect(x:282,y:82,width:184,height:14))
                    form.addSubview(makeEditableField("settings_hub_issuer",frame:NSRect(x:14,y:56,width:260,height:24),placeholder:"https://id.example.com",value:addHubIssuer))
                    form.addSubview(makeEditableField("settings_hub_client_id",frame:NSRect(x:282,y:56,width:184,height:24),placeholder:"cito",value:addHubClientId))
                }
                let protocolHelp=textLabel("Supports http://ip:4291 (Mac Share) or https:// (Team Hub).",NSRect(x:14,y:8,width:284,height:28),size:10,color:.secondaryLabelColor)
                protocolHelp.maximumNumberOfLines=2; protocolHelp.lineBreakMode = .byWordWrapping
                form.addSubview(protocolHelp)
            }

            let cancelBtn = MenuControl("Cancel", frame: NSRect(x: 306, y: 12, width: 72, height: 24), radius: 6)
            cancelBtn.identifier = NSUserInterfaceItemIdentifier("cancel_add_hub")
            cancelBtn.pressed = { [weak self] in
                guard let self = self else { return }
                self.addHubFormOpen = false
                self.rebuildSettingsPanel()
            }
            form.addSubview(cancelBtn)

            let submitBtn = MenuControl(editingExistingHub ? "Save Hub" : "Join Hub", frame: NSRect(x: 386, y: 12, width: 80, height: 24), radius: 6)
            submitBtn.identifier = NSUserInterfaceItemIdentifier("submit_add_hub")
            submitBtn.style = .prominent
            submitBtn.pressed = { [weak self] in
                guard let self = self else { return }
                self.syncAddHubFieldsFromControls()
                let name = self.addHubName.trimmingCharacters(in: .whitespacesAndNewlines)
                let url = self.addHubUrl.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !name.isEmpty, !url.isEmpty else { return }
                var action: [String: Any] = [
                    "op": "setup_save_hub",
                    "name": name,
                    "url": url,
                    "auth_mode": self.addHubAuth,
                    "set_default": !self.editingExistingHub
                ]
                if self.editingExistingHub { action["edit"]=true; action["original_name"]=self.editingOriginalHub; action.removeValue(forKey:"set_default") }
                let tok = self.addHubToken.trimmingCharacters(in: .whitespacesAndNewlines)
                let usr = self.addHubUser.trimmingCharacters(in: .whitespacesAndNewlines)
                let iss = self.addHubIssuer.trimmingCharacters(in: .whitespacesAndNewlines)
                let cid = self.addHubClientId.trimmingCharacters(in: .whitespacesAndNewlines)
                if self.addHubAuth == "token", !tok.isEmpty { action["token"] = tok }
                if (self.addHubAuth == "none" || self.addHubAuth == "token"), !usr.isEmpty { action["user"] = usr }
                if self.addHubAuth == "oidc" {
                    if !iss.isEmpty { action["issuer"] = iss }
                    if !cid.isEmpty { action["client_id"] = cid }
                }
                self.addHubFormOpen = false
                self.emit(action)
                self.rebuildSettingsPanel()
            }
            form.addSubview(submitBtn)
            hubsView.addSubview(form)
        } else {
            let resolvedHubs: [HubConfigItem] = settings?.hubs ?? hubs.map { h in
                HubConfigItem(name: h.name, url: h.name == "local" ? "local://" : "http://127.0.0.1:4291",
                    auth_mode: h.auth_mode, enabled: true, is_default: false, status: h.status, user: h.user, error: h.error, issuer: nil, client_id: nil)
            }
            for (idx, hub) in resolvedHubs.enumerated() {
                let rowY = 132 - CGFloat(idx) * 54
                let row = roundedView(NSRect(x: 20, y: rowY, width: 480, height: 48), radius: 9,
                    color: .labelColor.withAppearanceAlpha(0.05), border: .labelColor.withAppearanceAlpha(0.10))
                row.identifier = NSUserInterfaceItemIdentifier("settings_hub:\(hub.name)")
                let dotColor = statusColor(hub.enabled ? hub.status : "disconnected")
                row.addSubview(roundedView(NSRect(x: 12, y: 20, width: 8, height: 8), radius: 4, color: dotColor))
                let local=hub.name == "local"
                let textWidth:CGFloat=local ? 300 : (hub.auth_mode == "oidc" ? 150 : 222)
                row.addSubview(textLabel(hub.name, NSRect(x: 28, y: 24, width: textWidth, height: 16), size: 12.5, weight: .semibold))
                let subtitle=local ? "Built-in on this Mac (local://) · Handle: @\(hub.user ?? "local")" : "\(hub.url) · \(hub.auth_mode)"
                row.addSubview(textLabel(subtitle, NSRect(x: 28, y: 7, width: textWidth, height: 14), size: 10.5, color: .secondaryLabelColor))

                let edit=MenuControl("Edit",frame:NSRect(x:local ? 338 : 260,y:13,width:52,height:22),radius:6)
                edit.identifier=NSUserInterfaceItemIdentifier("edit_hub:\(hub.name)")
                edit.pressed={ [weak self] in
                    guard let self=self else{return}
                    self.addHubName=hub.name; self.addHubUrl=hub.url; self.addHubAuth=hub.auth_mode
                    self.addHubUser=hub.user ?? ""; self.addHubIssuer=hub.issuer ?? ""; self.addHubClientId=hub.client_id ?? ""; self.addHubToken=""
                    self.editingExistingHub=true; self.editingOriginalHub=hub.name; self.addHubFormOpen=true
                    self.rebuildSettingsPanel()
                }
                row.addSubview(edit)
                if hub.auth_mode == "oidc" {
                    let loginBtn = MenuControl("Sign In…", frame: NSRect(x: 184, y: 13, width: 70, height: 22), radius: 6)
                    loginBtn.identifier = NSUserInterfaceItemIdentifier("login_hub:\(hub.name)")
                    let hubName = hub.name
                    loginBtn.pressed = { [weak self] in
                        self?.emit(["op": "hub_action", "hub": hubName, "action": "login"])
                    }
                    row.addSubview(loginBtn)
                }

                let toggleBtn = MenuControl(hub.enabled ? "Enabled" : "Disabled", frame: NSRect(x: local ? 396 : 318, y: 13, width: 72, height: 22), radius: 6)
                toggleBtn.identifier = NSUserInterfaceItemIdentifier("toggle_hub:\(hub.name)")
                toggleBtn.style = hub.enabled ? .prominent : .neutral
                let hubName = hub.name
                let nextEnabled = !hub.enabled
                toggleBtn.pressed = { [weak self] in
                    self?.emit(["op": "setup_toggle_hub", "hub": hubName, "enabled": nextEnabled])
                }
                row.addSubview(toggleBtn)

                if hub.name != "local" {
                    let removeBtn = MenuControl("Remove", frame: NSRect(x: 396, y: 13, width: 72, height: 22), radius: 6)
                    removeBtn.identifier = NSUserInterfaceItemIdentifier("remove_hub:\(hub.name)")
                    removeBtn.style = .destructiveHover
                    removeBtn.pressed = { [weak self] in
                        self?.emit(["op": "setup_remove_hub", "hub": hubName])
                    }
                    row.addSubview(removeBtn)
                }

                hubsView.addSubview(row)
            }
        }
        mountSettingsCards(hubsView, in: root)

        // Persistent Footer (520 × 44 pt, y: 0)
        let footerBar = NSView(frame: NSRect(x: 0, y: 0, width: 520, height: 44))
        footerBar.identifier = NSUserInterfaceItemIdentifier("settings_footer")
        footerBar.addSubview(roundedView(NSRect(x: 0, y: 43, width: 520, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
        let launchAtLogin = settings?.launch_at_login ?? false
        let loginBtn = MenuControl("Start Cito at Login", frame: NSRect(x: 20, y: 10, width: 164, height: 24),
            leadingSymbol: launchAtLogin ? "checkmark.square.fill" : "square", radius: 6)
        loginBtn.identifier = NSUserInterfaceItemIdentifier("settings_launch_at_login")
        loginBtn.pressed = { [weak self] in
            self?.emit(["op": "set_launch_at_login", "enabled": !launchAtLogin])
        }
        footerBar.addSubview(loginBtn)

        let uninstallBtn = MenuControl("Remove All Integrations…", frame: NSRect(x: 316, y: 10, width: 184, height: 24), radius: 6)
        uninstallBtn.identifier = NSUserInterfaceItemIdentifier("open_uninstall_confirm")
        uninstallBtn.style = .destructiveHover
        uninstallBtn.pressed = { [weak self] in
            guard let self = self else { return }
            self.uninstallConfirmOpen = true
            self.uninstallPurgeData = false
            self.rebuildSettingsPanel()
        }
        footerBar.addSubview(uninstallBtn)
        root.addSubview(footerBar)

        if uninstallConfirmOpen {
            let confirmBox = roundedView(NSRect(x: 20, y: 48, width: 480, height: 96), radius: 10,
                color: .windowBackgroundColor.withAppearanceAlpha(0.96), border: .systemRed.withAppearanceAlpha(0.35))
            confirmBox.identifier = NSUserInterfaceItemIdentifier("uninstall_confirm_box")

            confirmBox.addSubview(textLabel("Reset or Uninstall Cito",NSRect(x:12,y:68,width:456,height:18),size:12.5,weight:.semibold))
            let purgeCheckbox = MenuControl("Also delete ~/.cito local data & hub settings on full uninstall",
                frame: NSRect(x: 12, y: 40, width: 420, height: 22),
                leadingSymbol: uninstallPurgeData ? "checkmark.square.fill" : "square", radius: 6)
            purgeCheckbox.identifier = NSUserInterfaceItemIdentifier("uninstall_purge_data")
            purgeCheckbox.pressed = { [weak self] in
                guard let self = self else { return }
                self.uninstallPurgeData.toggle()
                self.rebuildSettingsPanel()
            }
            confirmBox.addSubview(purgeCheckbox)

            let cancelUninstallBtn = MenuControl("Cancel", frame: NSRect(x: 112, y: 8, width: 72, height: 24), radius: 6)
            cancelUninstallBtn.identifier = NSUserInterfaceItemIdentifier("cancel_uninstall_all")
            cancelUninstallBtn.pressed = { [weak self] in
                guard let self = self else { return }
                self.uninstallConfirmOpen = false
                self.uninstallPurgeData = false
                self.rebuildSettingsPanel()
            }
            confirmBox.addSubview(cancelUninstallBtn)

            let disconnect=MenuControl("Disconnect All",frame:NSRect(x:192,y:8,width:116,height:24),radius:6)
            disconnect.identifier=NSUserInterfaceItemIdentifier("confirm_disconnect_all")
            disconnect.pressed={ [weak self] in self?.uninstallConfirmOpen=false; self?.emit(["op":"setup_disconnect_all"]); self?.rebuildSettingsPanel() }
            confirmBox.addSubview(disconnect)
            let confirmUninstallBtn = MenuControl("Confirm Uninstall", frame: NSRect(x: 316, y: 8, width: 152, height: 24), radius: 6)
            confirmUninstallBtn.identifier = NSUserInterfaceItemIdentifier("confirm_uninstall_all")
            confirmUninstallBtn.style = .destructiveHover
            confirmUninstallBtn.pressed = { [weak self] in
                guard let self = self else { return }
                let purge = self.uninstallPurgeData
                self.uninstallConfirmOpen = false
                self.uninstallPurgeData = false
                self.emit(["op": "setup_uninstall_all", "purge_data": purge])
                self.rebuildSettingsPanel()
            }
            confirmBox.addSubview(confirmUninstallBtn)
            root.addSubview(confirmBox)
        }

        window.contentView = root
    }

    func openSettingsPanel() {
        ensureSettingsPanel()
        settingsVisible = true
        emit(["op": "open_settings", "hub": effectiveChannelHub])
        if !headless, let window = settingsPanel {
            if let screen = NSScreen.main {
                let area = screen.visibleFrame
                window.setFrameOrigin(NSPoint(x: area.midX - 260, y: area.midY - 200))
            }
            NSApp.activate(ignoringOtherApps: true)
            window.makeKeyAndOrderFront(nil)
            if window.isVisible, let markerPath = pendingOnboardingMarker {
                try? FileManager.default.createDirectory(atPath: (markerPath as NSString).deletingLastPathComponent, withIntermediateDirectories: true)
                try? "1\n".write(toFile: markerPath, atomically: true, encoding: .utf8)
                pendingOnboardingMarker = nil
            }
        }
    }

    func closeSettingsPanel(emitAction: Bool) {
        MenuControl.clearHoverWhere { $0.window === self.settingsPanel }
        settingsVisible = false
        settingsPanel?.orderOut(nil)
        if emitAction { emit(["op": "close_settings"]) }
    }

    // MARK: - Surface 2: Spotlight-Style Network Directory Panel (560 × 440 pt KeyPanel: NSPanel)

    func ensureDirectoryPanel() {
        if directoryPanel != nil { return }
        let window = KeyPanel(contentRect: NSRect(x: 0, y: 0, width: 560, height: 440),
                              styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        window.level = .floating
        window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        window.isFloatingPanel = true
        window.hidesOnDeactivate = false
        window.isOpaque = false
        window.backgroundColor = .clear
        window.hasShadow = true

        let root = SurfaceView(frame: NSRect(x: 0, y: 0, width: 560, height: 440), radius: 14,
            color: .clear, border: .labelColor.withAppearanceAlpha(0.15))
        root.identifier = NSUserInterfaceItemIdentifier("directory_panel")
        root.layer?.masksToBounds = true

        let effect = NSVisualEffectView(frame: root.bounds)
        effect.material = .popover
        effect.blendingMode = .behindWindow
        effect.state = .active
        root.addSubview(effect)

        // Top Search Bar (560 × 46 pt, y: 394)
        let searchBar = NSView(frame: NSRect(x: 0, y: 394, width: 560, height: 46))
        searchBar.identifier = NSUserInterfaceItemIdentifier("directory_search_bar")

        let dirPill = MenuControl("Directory", frame: NSRect(x: 16, y: 11, width: 76, height: 24), radius: 12)
        dirPill.identifier = NSUserInterfaceItemIdentifier("dir_mode:directory")
        dirPill.label.font = .systemFont(ofSize: 11.5, weight: .semibold)
        dirPill.pressed = { [weak self] in
            guard let self = self else { return }
            self.switchDirectoryMode("directory")
        }
        searchBar.addSubview(dirPill)
        directoryModeDirPill = dirPill

        let actTitle = unreadCount > 0 ? "Activity · \(unreadCount)" : "Activity"
        let actPill = MenuControl(actTitle, frame: NSRect(x: 96, y: 11, width: 88, height: 24), radius: 12)
        actPill.identifier = NSUserInterfaceItemIdentifier("dir_mode:activity")
        actPill.label.font = .systemFont(ofSize: 11.5, weight: .semibold)
        actPill.pressed = { [weak self] in
            guard let self = self else { return }
            self.switchDirectoryMode("activity")
        }
        searchBar.addSubview(actPill)
        directoryModeActPill = actPill

        let searchField = NSSearchField(frame: NSRect(x: 192, y: 9, width: 324, height: 28))
        searchField.identifier = NSUserInterfaceItemIdentifier("directory_search")
        searchField.placeholderString = "Search by user, repo, title, host, or #id…"
        searchField.delegate = self
        searchBar.addSubview(searchField)
        directorySearchField = searchField

        let closeBtn = MenuControl("Close", frame: NSRect(x: 524, y: 11, width: 24, height: 24), symbol: "xmark")
        closeBtn.identifier = NSUserInterfaceItemIdentifier("close_directory")
        closeBtn.toolTip = "Close Directory"
        closeBtn.pressed = { [weak self] in self?.closeDirectoryPanel(emitAction: true) }
        searchBar.addSubview(closeBtn)
        searchBar.addSubview(roundedView(NSRect(x: 0, y: 0, width: 560, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
        root.addSubview(searchBar)

        // Facet Bar (560 × 32 pt, y: 362)
        let facetBar = NSView(frame: NSRect(x: 0, y: 362, width: 560, height: 32))
        facetBar.identifier = NSUserInterfaceItemIdentifier("directory_facet_bar")
        let scopePills: [(String, String, CGFloat, CGFloat)] = [
            ("dir_scope:all", "All", 16, 42),
            ("dir_scope:mine", "My Hosts", 62, 76)
        ]
        for (id, title, x, w) in scopePills {
            let pill = MenuControl(title, frame: NSRect(x: x, y: 5, width: w, height: 22), radius: 11)
            pill.identifier = NSUserInterfaceItemIdentifier(id)
            pill.label.font = .systemFont(ofSize: 11, weight: .medium)
            let scopeVal = id == "dir_scope:mine" ? "mine" : "all"
            pill.pressed = { [weak self] in
                guard let self = self else { return }
                self.directoryScopeFilter = scopeVal
                self.updateDirectoryFacetStyles()
                self.applyDirectoryFilter(fromDaemonFrame: false, daemonMatchedCount: nil)
                self.emit(["op": "query_directory", "query": self.directoryQuery, "scope": self.directoryScopeFilter, "harness": self.directoryHarnessFilter])
            }
            facetBar.addSubview(pill)
            directoryFacetControls[id] = pill
        }
        let allHarnessesPill = MenuControl("All Harnesses", frame: NSRect(x: 154, y: 5, width: 98, height: 22), radius: 11)
        allHarnessesPill.identifier = NSUserInterfaceItemIdentifier("dir_harness:all")
        allHarnessesPill.label.font = .systemFont(ofSize: 11, weight: .medium)
        allHarnessesPill.pressed = { [weak self] in
            guard let self = self else { return }
            self.directoryHarnessFilter = "all"
            self.rebuildDirectoryFacetBar()
            self.applyDirectoryFilter(fromDaemonFrame: false, daemonMatchedCount: nil)
            self.emit(["op": "query_directory", "query": self.directoryQuery, "scope": self.directoryScopeFilter, "harness": self.directoryHarnessFilter])
        }
        facetBar.addSubview(allHarnessesPill)
        directoryFacetControls["dir_harness:all"] = allHarnessesPill

        let actPills: [(String, String, String, CGFloat, CGFloat)] = [
            ("act_filter:me", "me", "For Me", 16, 64),
            ("act_filter:all", "all", "All Activity", 84, 86),
            ("act_filter:unread", "unread", "Unread", 174, 64)
        ]
        for (id, filterVal, title, x, w) in actPills {
            let pill = MenuControl(title, frame: NSRect(x: x, y: 5, width: w, height: 22), radius: 11)
            pill.identifier = NSUserInterfaceItemIdentifier(id)
            pill.label.font = .systemFont(ofSize: 11, weight: .medium)
            pill.isHidden = true
            pill.pressed = { [weak self] in
                guard let self = self else { return }
                self.activityFilter = filterVal
                self.updateDirectoryFacetStyles()
                self.applyActivityFilter()
            }
            facetBar.addSubview(pill)
            directoryFacetControls[id] = pill
        }
        let markReadBtn = MenuControl("✓ Read All", frame: NSRect(x: 456, y: 5, width: 88, height: 22), radius: 11)
        markReadBtn.identifier = NSUserInterfaceItemIdentifier("activity_mark_all_read")
        markReadBtn.label.font = .systemFont(ofSize: 11, weight: .medium)
        markReadBtn.isHidden = true
        markReadBtn.pressed = { [weak self] in
            guard let self = self else { return }
            self.unreadCount = 0
            self.activityAllItems = self.activityAllItems.map { item in
                ActivityItem(
                    hub: item.hub, signal_id: item.signal_id, listener_id: item.listener_id,
                    from_user: item.from_user, from_host_name: item.from_host_name,
                    from_harness: item.from_harness, from_title: item.from_title,
                    from_scoped_id: item.from_scoped_id, target_harness: item.target_harness,
                    target_title: item.target_title, target_scoped_id: item.target_scoped_id,
                    message: item.message, held_reason: item.held_reason,
                    in_reply_to: item.in_reply_to, created_at: item.created_at,
                    unread: false, for_me: item.for_me
                )
            }
            self.updateDirectoryModePills()
            self.applyActivityFilter()
            self.emit(["op": "ack_signals", "ack_all": true])
        }
        facetBar.addSubview(markReadBtn)
        directoryFacetControls["activity_mark_all_read"] = markReadBtn

        facetBar.addSubview(roundedView(NSRect(x: 0, y: 0, width: 560, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
        directoryFacetBar = facetBar
        root.addSubview(facetBar)
        updateDirectoryModePills()
        rebuildDirectoryFacetBar()

        // Virtualized Result List (560 × 334 pt, y: 28)
        let scroll = NSScrollView(frame: NSRect(x: 0, y: 28, width: 560, height: 334))
        scroll.identifier = NSUserInterfaceItemIdentifier("directory_scroll")
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = false
        scroll.autohidesScrollers = true
        let table = NSTableView(frame: NSRect(x: 0, y: 0, width: 560, height: 334))
        table.identifier = NSUserInterfaceItemIdentifier("directory_table")
        if #available(macOS 11.0, *) { table.style = .plain }
        table.rowHeight = 52
        table.intercellSpacing = .zero
        table.headerView = nil
        table.backgroundColor = .clear
        table.selectionHighlightStyle = .regular
        let col = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("peer"))
        col.width = 560
        table.addTableColumn(col)
        table.delegate = self
        table.dataSource = self
        scroll.documentView = table
        directoryTableView = table
        directoryScrollView = scroll
        root.addSubview(scroll)

        // Bottom Single-Line ComposerBar (560 × 44 pt, y: 28)
        let composer = roundedView(NSRect(x: 0, y: 28, width: 560, height: 62), radius: 0,
            color: .windowBackgroundColor.withAppearanceAlpha(0.92))
        composer.identifier = NSUserInterfaceItemIdentifier("directory_composer")
        composer.isHidden = true
        composer.addSubview(roundedView(NSRect(x: 0, y: 43, width: 560, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.12)))

        let targetPill = MenuControl("", frame: NSRect(x: 12, y: 10, width: 120, height: 24), radius: 6)
        targetPill.identifier = NSUserInterfaceItemIdentifier("composer_target")
        targetPill.style = .neutral
        composer.addSubview(targetPill)
        composerTargetPill = targetPill

        let inputField = NSTextField(frame: NSRect(x: 138, y: 10, width: 274, height: 24))
        inputField.identifier = NSUserInterfaceItemIdentifier("composer_input")
        inputField.placeholderString = "Message via Hub…"
        inputField.isEditable = true
        inputField.isSelectable = true
        inputField.isBezeled = true
        inputField.bezelStyle = .roundedBezel
        inputField.font = .systemFont(ofSize: 12)
        inputField.delegate = self
        composer.addSubview(inputField)
        composerInputField = inputField

        let sendBtn = MenuControl("Send ⏎", frame: NSRect(x: 418, y: 10, width: 68, height: 24), radius: 6)
        sendBtn.identifier = NSUserInterfaceItemIdentifier("composer_send")
        sendBtn.style = .prominent
        sendBtn.pressed = { [weak self] in self?.submitComposer() }
        composer.addSubview(sendBtn)

        let cancelComposerBtn = MenuControl("Cancel", frame: NSRect(x: 492, y: 10, width: 56, height: 24), radius: 6)
        cancelComposerBtn.identifier = NSUserInterfaceItemIdentifier("composer_cancel")
        cancelComposerBtn.pressed = { [weak self] in self?.closeComposer() }
        composer.addSubview(cancelComposerBtn)

        let composerError = textLabel("", NSRect(x: 16, y: 44, width: 528, height: 15), size: 10.5, color: .systemRed)
        composerError.identifier = NSUserInterfaceItemIdentifier("composer_error")
        composerError.toolTip = "Send status. Failed sends preserve your draft."
        composer.addSubview(composerError)
        composerErrorLabel = composerError
        directoryComposerBar = composer
        root.addSubview(composer)

        // Footer Shortcut Bar (560 × 28 pt, y: 0)
        let footerBar = NSView(frame: NSRect(x: 0, y: 0, width: 560, height: 28))
        footerBar.identifier = NSUserInterfaceItemIdentifier("directory_footer")
        footerBar.addSubview(roundedView(NSRect(x: 0, y: 27, width: 560, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
        let shortcuts = textLabel("↵ Copy Selected ID    ⎋ Close", NSRect(x: 16, y: 6, width: 260, height: 16), size: 11, color: .secondaryLabelColor)
        shortcuts.identifier = NSUserInterfaceItemIdentifier("directory_shortcuts")
        directoryShortcutsLabel = shortcuts
        footerBar.addSubview(shortcuts)
        updateDirectoryShortcuts()
        let summary = textLabel("0 of 0 online", NSRect(x: 320, y: 6, width: 224, height: 16), size: 11, color: .secondaryLabelColor)
        summary.alignment = .right
        summary.identifier = NSUserInterfaceItemIdentifier("directory_summary")
        footerBar.addSubview(summary)
        directorySummaryLabel = summary
        root.addSubview(footerBar)

        window.contentView = root
        directoryPanel = window
        updateDirectoryFacetStyles()
    }

    func updateDirectoryShortcuts() {
        directoryShortcutsLabel?.stringValue = !(directoryComposerBar?.isHidden ?? true)
            ? "↵ Send    ⎋ Cancel Reply"
            : (directoryMode == "activity" ? "↵ Reply    ⎋ Close" : "↵ Copy Selected ID    ⎋ Close")
    }

    func updateDirectoryModePills() {
        updateDirectoryShortcuts()
        directoryModeDirPill?.style = directoryMode == "directory" ? .prominent : .neutral
        directoryModeActPill?.style = directoryMode == "activity" ? .prominent : .neutral
        let actTitle = unreadCount > 0 ? "Activity · \(unreadCount)" : "Activity"
        directoryModeActPill?.label.stringValue = actTitle
        directoryModeActPill?.setAccessibilityLabel(actTitle)
    }

    func switchDirectoryMode(_ mode: String) {
        directoryMode = mode
        updateDirectoryModePills()
        updateDirectoryFacetStyles()
        if mode == "activity" {
            applyActivityFilter()
        } else {
            applyDirectoryFilter(fromDaemonFrame: false, daemonMatchedCount: nil)
        }
    }

    func openComposer(target: String, hub: String = "", replySignalId: String? = nil) {
        ensureDirectoryPanel()
        composerErrorLabel?.stringValue = ""
        composerTarget = target
        composerTargetHub = hub
        composerReplySignalId = replySignalId
        let displayTarget=shortPublicId(target.split(separator: ":").last.map(String.init) ?? target)
        let targetWidth=max(120,min(360,measureTextWidth(displayTarget,font:.systemFont(ofSize:12))+12))
        composerTargetPill?.frame.size.width=targetWidth
        composerTargetPill?.label.frame.size.width=targetWidth-6
        composerTargetPill?.label.stringValue = displayTarget
        composerInputField?.frame=NSRect(x:18+targetWidth,y:10,width:394-targetWidth,height:24)
        composerTargetPill?.setAccessibilityLabel(target)
        composerTargetPill?.toolTip = target
        directoryComposerBar?.isHidden = false
        updateDirectoryShortcuts()
        directoryScrollView?.frame = NSRect(x: 0, y: 90, width: 560, height: 272)
        if !headless, let window = directoryPanel, let field = composerInputField {
            window.makeFirstResponder(field)
        }
    }

    func closeComposer() {
        composerTarget = ""
        composerTargetHub = ""
        composerReplySignalId = nil
        composerInputField?.stringValue = ""
        directoryComposerBar?.isHidden = true
        composerErrorLabel?.stringValue = ""
        updateDirectoryShortcuts()
        directoryScrollView?.frame = NSRect(x: 0, y: 28, width: 560, height: 334)
    }

    func submitComposer() {
        let text = (composerInputField?.stringValue ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard composerPendingText == nil, !composerTarget.isEmpty, !text.isEmpty else { return }
        var action: [String: Any] = [
            "op": "send_signal",
            "to": composerTarget,
            "message": text
        ]
        if !composerTargetHub.isEmpty {
            action["hub"] = composerTargetHub
        }
        if let replyId = composerReplySignalId, !replyId.isEmpty {
            action["in_reply_to"] = replyId
        }
        composerPendingText = composerInputField?.stringValue
        composerPendingTarget = composerTarget
        composerPendingHub = composerTargetHub
        composerErrorLabel?.stringValue = "Sending…"
        emit(action)
    }

    func rebuildDirectoryFacetBar() {
        guard let facetBar = directoryFacetBar else { return }
        let map = harnessMap
        var unionHarnesses = Set<String>(harnesses.map { $0.id })
        for peer in directoryAllPeers where !peer.harness.isEmpty && peer.harness != "human" { unionHarnesses.insert(peer.harness) }
        if directoryHarnessFilter != "all" && !directoryHarnessFilter.isEmpty && directoryHarnessFilter != "human" {
            unionHarnesses.insert(directoryHarnessFilter)
        }
        let ordered = unionHarnesses.sorted { a, b in
            let va = resolveHarnessVisual(a, harnesses: map)
            let vb = resolveHarnessVisual(b, harnesses: map)
            return va.order != vb.order ? va.order < vb.order : a < b
        }
        let signature = ordered.map { h in "\(h):\(resolveHarnessVisual(h, harnesses: map).name)" }.joined(separator: "|")
        if signature == lastDirectoryFacetSignature {
            updateDirectoryFacetStyles()
            return
        }
        lastDirectoryFacetSignature = signature
        for v in directoryDynamicHarnessViews { v.removeFromSuperview() }
        directoryDynamicHarnessViews.removeAll()
        for key in directoryFacetControls.keys where key.hasPrefix("dir_harness:") && key != "dir_harness:all" {
            directoryFacetControls.removeValue(forKey: key)
        }

        let pillFont = NSFont.systemFont(ofSize: 11, weight: .medium)
        var layout: [(id: String, harness: String, title: String, x: CGFloat, width: CGFloat)] = []
        var curX: CGFloat = 256
        for h in ordered {
            let visual = resolveHarnessVisual(h, harnesses: map)
            let w = max(42, ceil(measureTextWidth(visual.name, font: pillFont)) + 20)
            layout.append((id: "dir_harness:\(h)", harness: h, title: visual.name, x: curX, width: w))
            curX += w + 4
        }
        let totalEnd = layout.last.map { $0.x + $0.width } ?? 256
        if totalEnd <= 544 {
            for item in layout {
                let pill = MenuControl(item.title, frame: NSRect(x: item.x, y: 5, width: item.width, height: 22), radius: 11)
                pill.identifier = NSUserInterfaceItemIdentifier(item.id)
                pill.label.font = pillFont
                let harnessVal = item.harness
                pill.pressed = { [weak self] in
                    guard let self = self else { return }
                    self.directoryHarnessFilter = harnessVal
                    self.rebuildDirectoryFacetBar()
                    self.applyDirectoryFilter(fromDaemonFrame: false, daemonMatchedCount: nil)
                    self.emit(["op": "query_directory", "query": self.directoryQuery, "scope": self.directoryScopeFilter, "harness": self.directoryHarnessFilter])
                }
                facetBar.addSubview(pill)
                directoryDynamicHarnessViews.append(pill)
                directoryFacetControls[item.id] = pill
            }
        } else {
            let scrollWidth: CGFloat = 288
            let scroll = NSScrollView(frame: NSRect(x: 256, y: 5, width: scrollWidth, height: 22))
            scroll.identifier = NSUserInterfaceItemIdentifier("directory_harness_scroll")
            scroll.drawsBackground = false
            scroll.hasHorizontalScroller = true
            scroll.hasVerticalScroller = false
            scroll.autohidesScrollers = true
            let doc = NSView(frame: NSRect(x: 0, y: 0, width: totalEnd - 256, height: 22))
            for item in layout {
                let pill = MenuControl(item.title, frame: NSRect(x: item.x - 256, y: 0, width: item.width, height: 22), radius: 11)
                pill.identifier = NSUserInterfaceItemIdentifier(item.id)
                pill.label.font = pillFont
                let harnessVal = item.harness
                pill.pressed = { [weak self] in
                    guard let self = self else { return }
                    self.directoryHarnessFilter = harnessVal
                    self.rebuildDirectoryFacetBar()
                    self.applyDirectoryFilter(fromDaemonFrame: false, daemonMatchedCount: nil)
                    self.emit(["op": "query_directory", "query": self.directoryQuery, "scope": self.directoryScopeFilter, "harness": self.directoryHarnessFilter])
                }
                doc.addSubview(pill)
                directoryFacetControls[item.id] = pill
            }
            scroll.documentView = doc
            facetBar.addSubview(scroll)
            directoryDynamicHarnessViews.append(scroll)
        }
        updateDirectoryFacetStyles()
    }

    func updateDirectoryFacetStyles() {
        let isActivity = directoryMode == "activity"
        for v in directoryDynamicHarnessViews { v.isHidden = isActivity }
        for (id, pill) in directoryFacetControls {
            if id.hasPrefix("act_filter:") {
                pill.isHidden = !isActivity
                pill.style = (id == "act_filter:\(activityFilter)") ? .prominent : .neutral
            } else if id == "activity_mark_all_read" {
                pill.isHidden = !isActivity
                pill.style = .neutral
            } else {
                pill.isHidden = isActivity
                let active = (id == "dir_scope:\(directoryScopeFilter)") || (id == "dir_harness:\(directoryHarnessFilter)")
                pill.style = active ? .prominent : .neutral
            }
        }
    }

    func updateDirectoryEmptyState() {
        guard let root=directoryPanel?.contentView else{return}
        root.subviews.filter{$0.identifier?.rawValue=="directory_empty_state"}.forEach{$0.removeFromSuperview()}
        let empty=directoryMode=="directory" ? directoryFilteredPeers.isEmpty : activityFilteredItems.isEmpty
        let box=NSView(frame:NSRect(x:20,y:110,width:520,height:140))
        box.identifier=NSUserInterfaceItemIdentifier("directory_empty_state")
        box.isHidden = !empty
        let title:String, subtitle:String, id:String, label:String
        let action:()->Void
        if directoryMode=="directory" {
            if !directoryQuery.isEmpty || directoryScopeFilter != "all" || directoryHarnessFilter != "all" {
                title="No matching nodes"; subtitle="No active listeners match your current search or filter."
                id="directory_reset_filters"; label="Reset Filters"
                action={ [weak self] in
                    guard let self=self else{return}
                    self.directoryQuery=""; self.directoryScopeFilter="all"; self.directoryHarnessFilter="all"
                    self.directorySearchField?.stringValue=""; self.rebuildDirectoryFacetBar()
                    self.applyDirectoryFilter(fromDaemonFrame:false,daemonMatchedCount:nil)
                    self.emit(["op":"query_directory","query":"","scope":"all","harness":"all"])
                }
            } else {
                title="No active nodes on this Hub"; subtitle="Enroll a coding session with + Listen or connect an Agent App."
                id="dir_empty_open_apps"; label="Open Agent Apps Settings…"
                action={ [weak self] in self?.settingsActiveTab="apps"; self?.openSettingsPanel() }
            }
        } else if activityFilter=="unread" && !activityAllItems.isEmpty {
            title="All caught up"; subtitle="You have no unread signals."
            id="act_empty_show_all"; label="Show All Activity"
            action={ [weak self] in self?.activityFilter="all"; self?.updateDirectoryFacetStyles(); self?.applyActivityFilter() }
        } else {
            title="No recent signal activity"; subtitle="Signals sent between your sessions and channels will appear here."
            id="act_empty_open_channels"; label="Configure Human Channels…"
            action={ [weak self] in self?.settingsActiveTab="channels"; self?.openSettingsPanel() }
        }
        box.addSubview(textLabel(title,NSRect(x:20,y:96,width:480,height:22),size:15,weight:.semibold))
        box.addSubview(textLabel(subtitle,NSRect(x:20,y:64,width:480,height:20),size:12,color:.secondaryLabelColor))
        let button=MenuControl(label,frame:NSRect(x:166,y:18,width:188,height:26),radius:6)
        button.identifier=NSUserInterfaceItemIdentifier(id); button.pressed=action; box.addSubview(button)
        root.addSubview(box)
    }
    func applyActivityFilter() {
        let q = directoryQuery.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        activityFilteredItems = activityAllItems.filter { item in
            if activityFilter == "me" && !item.for_me { return false }
            if activityFilter == "unread" && !item.unread { return false }
            if !q.isEmpty {
                let hay = [item.from_user, item.from_host_name, item.from_title, item.target_title, item.message, item.signal_id]
                    .joined(separator: " ").lowercased()
                if !hay.contains(q) { return false }
            }
            return true
        }
        directorySelectedIndex = activityFilteredItems.isEmpty ? 0 : min(max(0, directorySelectedIndex), activityFilteredItems.count - 1)
        updateDirectoryEmptyState()
        directorySummaryLabel?.stringValue = "\(activityFilteredItems.count) signals · \(unreadCount) unread"
        if let table = directoryTableView {
            table.reloadData()
            if !activityFilteredItems.isEmpty {
                table.selectRowIndexes(IndexSet(integer: directorySelectedIndex), byExtendingSelection: false)
                table.scrollRowToVisible(directorySelectedIndex)
            }
            table.layoutSubtreeIfNeeded()
        }
    }

    func openDirectoryPanel(mode: String = "directory") {
        ensureDirectoryPanel()
        directoryVisible = true
        directoryMode = mode
        updateDirectoryModePills()
        updateDirectoryFacetStyles()
        if mode == "activity" {
            applyActivityFilter()
        } else {
            applyDirectoryFilter(fromDaemonFrame: false, daemonMatchedCount: nil)
        }
        if !headless, let window = directoryPanel {
            if let screen = NSScreen.main {
                let area = screen.visibleFrame
                window.setFrameOrigin(NSPoint(x: area.midX - 280, y: area.midY - 180))
            }
            NSApp.activate(ignoringOtherApps: true)
            window.makeKeyAndOrderFront(nil)
            if !(directoryComposerBar?.isHidden ?? true), let field = composerInputField {
                window.makeFirstResponder(field)
            } else if let field = directorySearchField {
                window.makeFirstResponder(field)
            }
        }
    }

    func closeDirectoryPanel(emitAction: Bool) {
        MenuControl.clearHoverWhere { $0.window === self.directoryPanel }
        directoryVisible = false
        directoryPanel?.orderOut(nil)
        if emitAction { emit(["op": "close_directory"]) }
    }

    func matchesDirectoryPeer(_ peer: RemotePeer) -> Bool {
        if directoryScopeFilter == "mine" && peer.is_self_principal != true { return false }
        if directoryHarnessFilter != "all" && !directoryHarnessFilter.isEmpty && peer.harness != directoryHarnessFilter { return false }
        let tokens = directoryQuery.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            .split(whereSeparator: { $0.isWhitespace }).map(String.init)
        for tok in tokens {
            if tok.hasPrefix("@") && tok.count > 1 {
                if !peer.user.lowercased().contains(tok.dropFirst()) { return false }
            } else if tok.hasPrefix("user:") && tok.count > 5 {
                if !peer.user.lowercased().contains(tok.dropFirst(5)) { return false }
            } else if tok.hasPrefix("repo:") && tok.count > 5 {
                if !(peer.repo ?? "").lowercased().contains(tok.dropFirst(5)) { return false }
            } else if tok.hasPrefix("host:") && tok.count > 5 {
                let h = String(tok.dropFirst(5))
                if !peer.host_name.lowercased().contains(h) && !peer.host_id.lowercased().contains(h) { return false }
            } else if tok.hasPrefix("is:") && tok.count > 3 {
                if peer.harness.lowercased() != tok.dropFirst(3) { return false }
            } else if tok.hasPrefix("#") && tok.count > 1 {
                let idTok = String(tok.dropFirst())
                if !peer.public_id.lowercased().contains(idTok) && !peer.scoped_id.lowercased().contains(idTok) && !peer.listener_id.lowercased().contains(idTok) { return false }
            } else {
                let hay = [peer.title, peer.about, peer.cwd ?? "", peer.repo ?? "", peer.branch ?? "", peer.user, peer.host_name, peer.host_id, peer.harness, peer.public_id, peer.scoped_id, peer.listener_id]
                    .joined(separator: " ").lowercased()
                if !hay.contains(tok) { return false }
            }
        }
        return true
    }

    func applyDirectoryFilter(fromDaemonFrame: Bool, daemonMatchedCount: Int?) {
        let previousSelectedId = directoryFilteredPeers.indices.contains(directorySelectedIndex) ? directoryFilteredPeers[directorySelectedIndex].scoped_id : nil
        let sameServerQuery = serverDirectoryQuery == directoryQuery && serverDirectoryScope == directoryScopeFilter && serverDirectoryHarness == directoryHarnessFilter
        directoryFilteredPeers = (fromDaemonFrame || sameServerQuery) ? directoryAllPeers : directoryAllPeers.filter { matchesDirectoryPeer($0) }
        directoryMatchedCount = daemonMatchedCount ?? (sameServerQuery ? serverDirectoryMatchedCount : directoryFilteredPeers.count)
        if directoryFilteredPeers.isEmpty {
            directorySelectedIndex = 0
        } else if let prev = previousSelectedId, let idx = directoryFilteredPeers.firstIndex(where: { $0.scoped_id == prev }) {
            directorySelectedIndex = idx
        } else {
            directorySelectedIndex = min(max(0, directorySelectedIndex), directoryFilteredPeers.count - 1)
        }
        updateDirectoryEmptyState()
        directorySummaryLabel?.stringValue = "\(directoryMatchedCount) of \(directoryOnlineCount) online"
        if let table = directoryTableView {
            table.reloadData()
            if !directoryFilteredPeers.isEmpty {
                table.selectRowIndexes(IndexSet(integer: directorySelectedIndex), byExtendingSelection: false)
                table.scrollRowToVisible(directorySelectedIndex)
            }
            table.layoutSubtreeIfNeeded()
        }
    }

    func handleDirectoryKey(_ key: String) {
        if (directorySearchField?.currentEditor() as? NSTextInputClient)?.hasMarkedText() == true { return }
        switch key.lowercased() {
        case "down":
            let count = directoryMode == "activity" ? activityFilteredItems.count : directoryFilteredPeers.count
            guard count > 0 else { return }
            directorySelectedIndex = min(count - 1, directorySelectedIndex + 1)
            directoryTableView?.reloadData()
            directoryTableView?.selectRowIndexes(IndexSet(integer: directorySelectedIndex), byExtendingSelection: false)
            directoryTableView?.scrollRowToVisible(directorySelectedIndex)
            directoryTableView?.layoutSubtreeIfNeeded()
        case "up":
            let count = directoryMode == "activity" ? activityFilteredItems.count : directoryFilteredPeers.count
            guard count > 0 else { return }
            directorySelectedIndex = max(0, directorySelectedIndex - 1)
            directoryTableView?.reloadData()
            directoryTableView?.selectRowIndexes(IndexSet(integer: directorySelectedIndex), byExtendingSelection: false)
            directoryTableView?.scrollRowToVisible(directorySelectedIndex)
            directoryTableView?.layoutSubtreeIfNeeded()
        case "return":
            if !(directoryComposerBar?.isHidden ?? true), !(composerInputField?.stringValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true) {
                submitComposer()
                return
            }
            if directoryMode == "activity" {
                guard activityFilteredItems.indices.contains(directorySelectedIndex) else { return }
                let item = activityFilteredItems[directorySelectedIndex]
                openComposer(target: item.from_scoped_id ?? "@\(item.from_user)", hub: item.hub, replySignalId: item.signal_id)
                return
            }
            guard directoryFilteredPeers.indices.contains(directorySelectedIndex) else { return }
            let peer = directoryFilteredPeers[directorySelectedIndex]
            clipboardString = peer.scoped_id
            appPasteboard.clearContents()
            appPasteboard.setString(peer.scoped_id, forType: .string)
            if let table = directoryTableView,
               let cell = table.view(atColumn: 0, row: directorySelectedIndex, makeIfNecessary: true) as? DirectoryPeerCellView {
                cell.copyControl.copied()
            }
        case "escape":
            if !(directoryComposerBar?.isHidden ?? true) {
                closeComposer()
                return
            }
            closeDirectoryPanel(emitAction: true)
        default:
            break
        }
    }

    func controlTextDidChange(_ obj: Notification) {
        if let field = obj.object as? NSSearchField, field === directorySearchField {
            directoryQuery = field.stringValue
            if directoryMode == "activity" {
                applyActivityFilter()
            } else {
                applyDirectoryFilter(fromDaemonFrame: false, daemonMatchedCount: nil)
            }
            emit(["op": "query_directory", "query": directoryQuery, "scope": directoryScopeFilter, "harness": directoryHarnessFilter])
            return
        }
        if let field = obj.object as? NSTextField, let id = field.identifier?.rawValue, id.hasPrefix("channel_field:") {
            channelFieldDrafts[draftKey(id)] = field.stringValue
        }
        syncAddHubFieldsFromControls()
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
        if textView.hasMarkedText() { return false }
        if control.window === settingsPanel {
            if commandSelector == #selector(NSResponder.cancelOperation(_:)) {
                closeSettingsPanel(emitAction: true)
                return true
            }
            return false
        }
        if commandSelector == #selector(NSResponder.moveDown(_:)) { handleDirectoryKey("down"); return true }
        if commandSelector == #selector(NSResponder.moveUp(_:)) { handleDirectoryKey("up"); return true }
        if commandSelector == #selector(NSResponder.insertNewline(_:)) { handleDirectoryKey("return"); return true }
        if commandSelector == #selector(NSResponder.cancelOperation(_:)) { handleDirectoryKey("escape"); return true }
        return false
    }

    func numberOfRows(in tableView: NSTableView) -> Int {
        return directoryMode == "activity" ? activityFilteredItems.count : directoryFilteredPeers.count
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let id = NSUserInterfaceItemIdentifier("DirectoryPeerCell")
        let cell = (tableView.makeView(withIdentifier: id, owner: self) as? DirectoryPeerCellView)
            ?? DirectoryPeerCellView(frame: NSRect(x: 0, y: 0, width: 560, height: 52))
        if directoryMode == "activity" {
            guard activityFilteredItems.indices.contains(row) else { return nil }
            let item = activityFilteredItems[row]
            cell.configureActivity(with: item, selected: row == directorySelectedIndex, harnesses: harnessMap) { [weak self] in
                guard let self = self else { return }
                self.openComposer(target: item.from_scoped_id ?? "@\(item.from_user)", hub: item.hub, replySignalId: item.signal_id)
            }
            return cell
        }
        guard directoryFilteredPeers.indices.contains(row) else { return nil }
        let peer = directoryFilteredPeers[row]
        cell.configure(with: peer, selected: row == directorySelectedIndex, harnesses: harnessMap) { [weak self] in
            guard let self = self else { return }
            self.openComposer(target: peer.scoped_id, hub: peer.hub, replySignalId: nil)
        }
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard let table = notification.object as? NSTableView, table === directoryTableView else { return }
        let row = table.selectedRow
        let count = directoryMode == "activity" ? activityFilteredItems.count : directoryFilteredPeers.count
        guard row >= 0 && row < count else { return }
        directorySelectedIndex = row
        for r in 0..<table.numberOfRows {
            if let cell = table.view(atColumn: 0, row: r, makeIfNecessary: false) as? DirectoryPeerCellView {
                cell.setSelected(r == row)
            }
        }
        table.layoutSubtreeIfNeeded()
    }

    // Headless inspection projects real AppKit objects; it does not synthesize a parallel menu model.
    func renderPNG(surface: String, path: String) {
        var restored: [(NSMenuItem, NSView, NSView?, NSRect)] = []
        let view: NSView
        var compositeWindow: NSPanel?
        if surface == "settings", let content = settingsPanel?.contentView { view = content }
        else if surface == "directory", let content = directoryPanel?.contentView { view = content }
        else if surface == "menu" {
            let rows=cardMenu.items.filter { !$0.isHidden }
            let height=rows.reduce(CGFloat(8)) { $0 + ($1.view?.frame.height ?? 9) }
            let composite=FlippedView(frame:NSRect(x:0,y:0,width:380,height:height))
            composite.wantsLayer=true
            composite.layer?.backgroundColor=NSColor.windowBackgroundColor.cgColor
            var y:CGFloat=4
            for row in rows {
                if let child=row.view {
                    restored.append((row,child,child.superview,child.frame))
                    // NSMenu otherwise keeps resetting its custom view's origin to zero.
                    row.view=nil
                    composite.addSubview(child); child.frame.origin=NSPoint(x:0,y:y); y+=child.frame.height
                } else { y+=9 }
            }
            let window=NSPanel(contentRect:composite.bounds,styleMask:.borderless,backing:.buffered,defer:false)
            window.contentView=composite
            compositeWindow=window
            view=composite
        } else { diagnostic("render failed: unavailable surface " + surface); return }
        defer {
            for (item,child,parent,frame) in restored {
                child.removeFromSuperview(); child.frame=frame; item.view=child; parent?.addSubview(child)
            }
            compositeWindow?.contentView=nil
        }
        view.layoutSubtreeIfNeeded()
        guard let bitmap=NSBitmapImageRep(bitmapDataPlanes:nil,pixelsWide:Int(view.bounds.width*2),pixelsHigh:Int(view.bounds.height*2),bitsPerSample:8,samplesPerPixel:4,hasAlpha:true,isPlanar:false,colorSpaceName:.deviceRGB,bytesPerRow:0,bitsPerPixel:0) else { diagnostic("render failed: bitmap allocation"); return }
        bitmap.size=view.bounds.size
        view.cacheDisplay(in:view.bounds,to:bitmap)
        guard let data=bitmap.representation(using:.png,properties:[:]) else { diagnostic("render failed: PNG encoding"); return }
        do { try data.write(to:URL(fileURLWithPath:path),options:.atomic); diagnostic("rendered " + path) }
        catch { diagnostic("render failed: " + error.localizedDescription) }
    }

    func inspectMenu() {
        var truncatedLabels: [String] = []
        func components(_ color: NSColor?) -> [CGFloat] {
            guard let rgb = color?.usingColorSpace(.deviceRGB) else { return [] }
            return [rgb.redComponent, rgb.greenComponent, rgb.blueComponent, rgb.alphaComponent]
        }
        func project(_ view: NSView, visible: Bool = true) -> [String: Any] {
            let visible = visible && !view.isHidden
            if visible {
                let label: NSTextField?
                if let control=view as? MenuControl { label=control.label }
                else if view.superview?.identifier?.rawValue.hasPrefix("settings_app_tier:") == true { label=view as? NSTextField }
                else { label=nil }
                if let label=label, !label.isHidden,
                   ceil(measureTextWidth(label.stringValue,font:label.font ?? .systemFont(ofSize:12))) > label.frame.width + 2.0 {
                    truncatedLabels.append(view.identifier?.rawValue ?? view.superview?.identifier?.rawValue ?? label.stringValue)
                }
            }
            let childViews: [NSView]
            if let table = view as? NSTableView {
                let count = min(table.numberOfRows, 100)
                childViews = (0..<count).compactMap { table.view(atColumn: 0, row: $0, makeIfNecessary: true) }
            } else if let scroll = view as? NSScrollView, let doc = scroll.documentView {
                if let table = doc as? NSTableView {
                    childViews = [table]
                } else {
                    childViews = doc.subviews
                }
            } else {
                childViews = view.subviews
            }
            var result: [String: Any] = ["id": (view as? DirectoryPeerCellView)?.inspectionId ?? view.identifier?.rawValue ?? "", "width": view.frame.width,
                "height": view.frame.height, "x": view.frame.origin.x, "y": view.frame.origin.y,
                "radius": view.layer?.cornerRadius ?? 0, "hidden": view.isHidden,
                "children": childViews.map { project($0, visible: visible) }]
            result["background"] = components(view.layer?.backgroundColor.flatMap { NSColor(cgColor: $0) })
            result["border"] = components(view.layer?.borderColor.flatMap { NSColor(cgColor: $0) })
            result["border_width"] = view.layer?.borderWidth ?? 0
            if let image = view as? NSImageView {
                view.effectiveAppearance.performAsCurrentDrawingAppearance { result["tint"] = components(image.contentTintColor) }
            }
            if let cell = view as? DirectoryPeerCellView { result["reuse_identifier"] = cell.identifier?.rawValue ?? "" }
            if let label = view as? NSTextField {
                result["text"] = label is NSSecureTextField ? "••••" : label.stringValue
                result["secure"] = label is NSSecureTextField
                result["placeholder"] = label.placeholderString ?? ""
            }
            if let control = view as? MenuControl {
                result["text"] = control.label.stringValue; result["tooltip"] = control.toolTip ?? ""
                result["label_width"] = control.label.frame.width
                result["label_measured_width"] = measureTextWidth(control.label.stringValue,font:control.label.font ?? .systemFont(ofSize:12))
                result["green"] = control.label.textColor == NSColor.systemGreen
                result["hovered"] = control.isHovered
                control.effectiveAppearance.performAsCurrentDrawingAppearance { result["foreground"] = components(control.label.textColor) }
                if let symbol = control.symbolName { result["symbol"] = symbol }
            }
            return result
        }
        let items = cardMenu.items.compactMap { item -> [String: Any]? in
            guard let view = item.view else { return nil }
            var value = project(view, visible: !item.isHidden); value["hidden"] = item.isHidden; return value
        }
        let directoryItems: [[String: Any]]
        if let rootView = directoryPanel?.contentView {
            directoryItems = [project(rootView)]
        } else {
            directoryItems = []
        }
        let settingsItems: [[String: Any]]
        if let rootView = settingsPanel?.contentView {
            settingsItems = [project(rootView)]
        } else {
            settingsItems = []
        }
        let selectedId: String
        if directoryMode == "activity" {
            selectedId = activityFilteredItems.indices.contains(directorySelectedIndex)
                ? activityFilteredItems[directorySelectedIndex].signal_id : ""
        } else {
            selectedId = directoryFilteredPeers.indices.contains(directorySelectedIndex)
                ? directoryFilteredPeers[directorySelectedIndex].scoped_id : ""
        }
        let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: 40, pixelsHigh: 40, bitsPerSample: 8,
            samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)
        NSGraphicsContext.current?.cgContext.scaleBy(x: 2, y: 2)
        statusImage?.draw(in: NSRect(x: 0, y: 0, width: 20, height: 20))
        NSGraphicsContext.restoreGraphicsState()
        func alpha(_ x: CGFloat, _ y: CGFloat) -> CGFloat {
            // Bitmap rows start at the top; drawing coordinates start at the bottom.
            return bitmap.colorAt(x: Int(x * 2), y: 39 - Int(y * 2))?.alphaComponent ?? 0
        }
        let probes: [String: CGFloat] = ["arc": alpha(1.75, 10), "core": alpha(9.2, 10),
            "wave": alpha(18.75, 10), "arc_gap": alpha(15.2, 10), "warning_dot": alpha(15.7, 4.3),
            "warning_ring": alpha(12.5, 4.3), "outside_cutout": alpha(11.0, 4.3), "inside_dot": alpha(17.0, 4.3)]
        let button = status?.button ?? headlessStatusButton
        let font = button.flatMap { $0.attributedTitle.length > 0 ? $0.attributedTitle.attribute(.font, at: 0, effectiveRange: nil) as? NSFont : nil }
        let traits = font?.fontDescriptor.object(forKey: .traits) as? [NSFontDescriptor.TraitKey: Any]
        let effectiveClipboard = !clipboardString.isEmpty ? clipboardString : (appPasteboard.string(forType: .string) ?? "")
        let snapshot: [String: Any] = ["items": items, "truncated_labels": truncatedLabels, "icon_alpha": probes, "count": uniqueSessionCount,
            "available_count": available.count, "rebuilds": rebuildCount,
            "directory_open": directoryVisible, "directory_mode": directoryMode, "activity_filter": activityFilter,
            "unread_count": unreadCount, "composer_open": !(directoryComposerBar?.isHidden ?? true),
            "composer_target": composerTarget, "composer_reply_signal_id": composerReplySignalId ?? "",
            "directory_online_count": directoryOnlineCount,
            "directory_matched_count": directoryMatchedCount, "directory_query": directoryQuery,
            "directory_scope": directoryScopeFilter, "directory_harness": directoryHarnessFilter,
            "directory_selected_id": selectedId, "directory_items": directoryItems,
            "settings_open": settingsVisible, "settings_tab": settingsActiveTab,
            "settings_launch_at_login": settings?.launch_at_login ?? false, "settings_items": settingsItems,
            "icon_template": statusImage?.isTemplate ?? false, "icon_width": statusImage?.size.width ?? 0,
            "icon_height": statusImage?.size.height ?? 0, "clipboard": effectiveClipboard,
            "button_title": button?.title ?? "", "button_image_position": button?.imagePosition.rawValue ?? -1,
            "button_font_size": font?.pointSize ?? 0, "button_font_weight": traits?[.weight] as? CGFloat ?? 0,
            "button_font_features": (font?.fontDescriptor.object(forKey: .featureSettings) as? [[NSFontDescriptor.FeatureKey: Any]] ?? []).map { feature in
                ["type": feature[.typeIdentifier] as? Int ?? -1, "selector": feature[.selectorIdentifier] as? Int ?? -1]
            },
            "button_digit_widths": font.map { font in ["11111111", "88888888"].map { ($0 as NSString).size(withAttributes: [.font: font]).width } } ?? []]
        diagnostic("menu " + String(data: try! JSONSerialization.data(withJSONObject: snapshot, options: [.sortedKeys]), encoding: .utf8)!)
    }
    func actionLabel(for value: Payload) -> String {
        return "Click to reply in Cito ↩"
    }
    func openDirectoryForReply(_ value: Payload) {
        let target = (value.from_scoped_id?.isEmpty == false) ? value.from_scoped_id! : "@\(value.from_user)"
        openDirectoryPanel(mode: "activity")
        if let sigId = value.signal_id, let idx = activityFilteredItems.firstIndex(where: { $0.signal_id == sigId }) {
            directorySelectedIndex = idx
            directoryTableView?.selectRowIndexes(IndexSet(integer: idx), byExtendingSelection: false)
        }
        openComposer(target: target, hub: value.hub ?? "", replySignalId: value.signal_id)
        emit(["op": "open_directory", "mode": "activity"])
    }
    func hide(clicked: Bool) {
        timer?.cancel()
        let clickedPayload = (clicked && visible) ? payload : nil
        visible = false; panel?.orderOut(nil)
        if let val = clickedPayload {
            openDirectoryForReply(val)
        }
        if headless { diagnostic(clicked ? "banner clicked" : "banner hidden") }
    }
    func show(_ value: Payload) {
        let title = "Cito · \(value.target_title)" + (displayedCount > 1 ? " (\(displayedCount))" : "")
        let semanticLabel = (value.effective_wake ? "Notification" : "Passive") + (displayedCount > 1 ? " · ×\(displayedCount)" : "")
        let actionText = actionLabel(for: value)
        visible = true
        if headless {
            // Diagnostics expose the actual presentation model, without adding
            // test-only socket commands or bypassing delivery/retirement.
            diagnostic("display " + String(data: try! JSONSerialization.data(withJSONObject: [
                "pid": getpid(), "title": title, "subtitle": value.subtitle, "message": value.message,
                "displayed_count": displayedCount, "display_subtitle": value.displaySubtitle,
                "display_message": value.displayMessage, "semantic_label": semanticLabel, "action_label": actionText
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
        labels[3].stringValue = actionText
        labels[3].textColor = .systemBlue
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

}

let app = NSApplication.shared
let companion = Companion()
app.setActivationPolicy(.accessory)
companion.start()
app.run()
