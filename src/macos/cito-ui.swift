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
    let jump_url_template: String?
}
struct HarnessVisual {
    let id: String
    let name: String
    let symbol: String
    let tint: NSColor
    let tier: String
    let order: Int
    let jumpUrlTemplate: String?
}
func tintColor(_ name: String) -> NSColor {
    switch name.lowercased() {
    case "indigo": return .systemIndigo
    case "teal": return .systemTeal
    case "orange": return .systemOrange
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
            order: meta.order,
            jumpUrlTemplate: meta.jump_url_template
        )
    }
    return HarnessVisual(
        id: harness,
        name: harness.capitalized,
        symbol: "bolt.horizontal.fill",
        tint: .systemBlue,
        tier: "custom",
        order: 3,
        jumpUrlTemplate: nil
    )
}
func isSafeJumpUrl(_ raw: String?) -> Bool {
    guard let raw = raw?.trimmingCharacters(in: .whitespacesAndNewlines), !raw.isEmpty else { return false }
    if raw.unicodeScalars.contains(where: { $0.value <= 0x20 || $0.value == 0x7F }) { return false }
    guard let colon = raw.firstIndex(of: ":") else { return false }
    let scheme = String(raw[..<colon]).lowercased()
    let forbidden: Set<String> = ["javascript", "file", "data", "http", "https", "vbscript", "about", "blob"]
    if forbidden.contains(scheme) { return false }
    guard let url = URL(string: raw), let parsedScheme = url.scheme?.lowercased(), !forbidden.contains(parsedScheme) else {
        return false
    }
    return true
}
func expandJumpUrl(template: String?, sessionId: String, cwd: String? = nil) -> String? {
    guard let template = template?.trimmingCharacters(in: .whitespacesAndNewlines), !template.isEmpty else { return nil }
    if template.contains("{session_id}") && sessionId.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return nil }
    if template.contains("{cwd}") {
        guard let cwd = cwd?.trimmingCharacters(in: .whitespacesAndNewlines), !cwd.isEmpty else { return nil }
    }
    var allowed = CharacterSet.urlPathAllowed
    allowed.remove(charactersIn: "/?#[]")
    guard let encodedSession = sessionId.addingPercentEncoding(withAllowedCharacters: allowed) else { return nil }
    var expanded = template.replacingOccurrences(of: "{session_id}", with: encodedSession)
    if let cwd = cwd {
        let pathEncoded = cwd.split(separator: "/", omittingEmptySubsequences: false)
            .map { String($0).addingPercentEncoding(withAllowedCharacters: allowed) ?? String($0) }
            .joined(separator: "/")
        let queryEncoded = cwd.addingPercentEncoding(withAllowedCharacters: allowed) ?? cwd
        expanded = expanded.replacingOccurrences(of: "/{cwd}", with: pathEncoded.hasPrefix("/") ? pathEncoded : "/" + pathEncoded)
        expanded = expanded.replacingOccurrences(of: "{cwd}", with: queryEncoded)
    }
    return isSafeJumpUrl(expanded) ? expanded : nil
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
    let jump_url: String?
    let jump_label: String?

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
    let can_jump: Bool
    let jump_url: String?
    let jump_label: String?
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
}
struct Frame: Decodable {
    let op: String
    let hubs: [Hub]?
    let listeners: [Listener]?
    let available: [Candidate]?
    let my_remote_listeners: [RemotePeer]?
    let directory_online_count: Int?
    let harnesses: [HarnessMeta]?
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
    let copyControl: MenuControl

    override init(frame frameRect: NSRect) {
        badgeView = roundedView(NSRect(x: 14, y: 12, width: 28, height: 28), radius: 7, color: .clear)
        badgeIcon = NSImageView(frame: NSRect(x: 6, y: 6, width: 16, height: 16))
        badgeView.addSubview(badgeIcon)
        line1TitleLabel = textLabel("", NSRect(x: 52, y: 26, width: 180, height: 18), size: 13, weight: .semibold, color: .labelColor)
        line1RepoLabel = textLabel("", NSRect(x: 232, y: 26, width: 180, height: 18), size: 11.5, weight: .regular, color: .secondaryLabelColor)
        line2UserLabel = textLabel("", NSRect(x: 52, y: 7, width: 80, height: 16), size: 11.5, weight: .medium, color: .labelColor)
        line2MetaLabel = textLabel("", NSRect(x: 132, y: 7, width: 280, height: 16), size: 11.5, weight: .regular, color: .secondaryLabelColor)
        copyControl = MenuControl("", frame: NSRect(x: 440, y: 16, width: 104, height: 22), leadingSymbol: "doc.on.doc")
        super.init(frame: frameRect)
        identifier = NSUserInterfaceItemIdentifier("DirectoryPeerCell")
        wantsLayer = true
        addSubview(badgeView)
        addSubview(line1TitleLabel)
        addSubview(line1RepoLabel)
        addSubview(line2UserLabel)
        addSubview(line2MetaLabel)
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

    func configure(with peer: RemotePeer, selected: Bool, harnesses: [String: HarnessMeta]) {
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
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(peer.scoped_id, forType: .string)
            copyControl?.copied()
        }

        let availWidth = max(120, 544 - copyWidth - 60)
        let titleFont = NSFont.systemFont(ofSize: 13, weight: .semibold)
        let titleW = min(availWidth, measureTextWidth(peer.title, font: titleFont) + 4)
        line1TitleLabel.stringValue = peer.title
        line1TitleLabel.frame = NSRect(x: 52, y: 26, width: titleW, height: 18)
        line1TitleLabel.identifier = NSUserInterfaceItemIdentifier("dir_title:\(peer.scoped_id)")

        let branch = (peer.branch ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        let branchSuffix = (!branch.isEmpty && branch != "HEAD") ? " · \(branch)" : ""
        let repoText = (peer.repo ?? "").isEmpty ? branchSuffix : " · \(peer.repo ?? "")\(branchSuffix)"
        line1RepoLabel.stringValue = repoText
        line1RepoLabel.frame = NSRect(x: 52 + titleW, y: 26, width: max(0, availWidth - titleW), height: 18)
        line1RepoLabel.identifier = NSUserInterfaceItemIdentifier("dir_repo:\(peer.scoped_id)")

        let userText = "@\(peer.user)"
        let userFont = NSFont.systemFont(ofSize: 11.5, weight: .medium)
        let userW = min(availWidth, measureTextWidth(userText, font: userFont) + 2)
        line2UserLabel.stringValue = userText
        line2UserLabel.frame = NSRect(x: 52, y: 7, width: userW, height: 16)
        line2UserLabel.identifier = NSUserInterfaceItemIdentifier("dir_user:\(peer.scoped_id)")

        let cleanHost = peer.host_name.replacingOccurrences(of: "\\.local$", with: "", options: [.regularExpression, .caseInsensitive])
        let hostText = cleanHost.isEmpty ? peer.host_name : cleanHost
        line2MetaLabel.stringValue = "@\(hostText) · \(peer.about)"
        line2MetaLabel.frame = NSRect(x: 52 + userW, y: 7, width: max(0, availWidth - userW), height: 16)
        line2MetaLabel.identifier = NSUserInterfaceItemIdentifier("dir_meta:\(peer.scoped_id)")
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
    var menuActions: [() -> Void] = []
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
        if !selectedTargetHub.isEmpty && hubs.contains(where: { $0.name == selectedTargetHub }) {
            return selectedTargetHub
        }
        if let firstUnmountedConnected = hubs.first(where: { hub in
            hub.status == "connected" && available.contains(where: { !($0.listening_hubs ?? []).contains(hub.name) })
        }) {
            return firstUnmountedConnected.name
        }
        return hubs.first(where: { $0.status == "connected" })?.name ?? hubs.first?.name ?? ""
    }
    var directoryOnlineCount = 0
    var directoryMatchedCount = 0
    var directoryAllPeers: [RemotePeer] = []
    var directoryFilteredPeers: [RemotePeer] = []
    var directoryVisible = false
    var directoryQuery = ""
    var directoryScopeFilter = "all"
    var directoryHarnessFilter = "all"
    var directorySelectedIndex = 0
    var directoryPanel: KeyPanel?
    var directorySearchField: NSSearchField?
    var directoryTableView: NSTableView?
    var directorySummaryLabel: NSTextField?
    var directoryFacetBar: NSView?
    var directoryDynamicHarnessViews: [NSView] = []
    var lastDirectoryFacetSignature = ""
    var directoryFacetControls: [String: MenuControl] = [:]
    var hasRenderedState = false
    var pendingMutation = false
    var rebuildCount = 0

    func menuWillOpen(_ menu: NSMenu) {
        MenuControl.clearHoverWhere { $0.enclosingMenuItem != nil || $0.window !== self.directoryPanel }
        emit(["op": "refresh_available"])
    }
    func menuDidClose(_ menu: NSMenu) {
        MenuControl.clearHoverWhere { $0.enclosingMenuItem != nil || $0.window !== self.directoryPanel }
    }
    func start() {
        signal(SIGPIPE, SIG_IGN)
        signal(SIGUSR1, SIG_IGN)
        clickSource = DispatchSource.makeSignalSource(signal: SIGUSR1, queue: .main)
        clickSource?.setEventHandler { [weak self] in self?.hide(clicked: true) }
        clickSource?.resume()
        cardMenu.autoenablesItems = false
        cardMenu.delegate = self
        MenuControl.tracksPhysicalPointer = !headless
        if headless { headlessStatusButton = NSButton(frame: .zero) }
        else {
            status = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
            status?.menu = cardMenu
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
        if view.identifier?.rawValue == id, let control = view as? MenuControl { return control }
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
    func receive(_ data: Data) {
        // Headless hooks use the exact same menu handlers and banner click path.
        if headless, let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let op = object["op"] as? String {
            if op == "test_click" { hide(clicked: true); return }
            if op == "test_inspect" { inspectMenu(); return }
            if op == "test_menu_open" { menuWillOpen(cardMenu); return }
            if op == "test_menu_close" { menuDidClose(cardMenu); return }
            if op == "test_appearance", let name = object["appearance"] as? String, ["light", "dark"].contains(name) {
                let appearance = NSAppearance(named: name == "dark" ? .darkAqua : .aqua)
                for item in cardMenu.items { item.view?.appearance = appearance }
                directoryPanel?.appearance = appearance
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
                if id.hasPrefix("dir_peer:"), let table = directoryTableView {
                    let scopedId = String(id.dropFirst("dir_peer:".count))
                    if let row = directoryFilteredPeers.firstIndex(where: { $0.scoped_id == scopedId }) {
                        table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
                    }
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
            let a = frame.available ?? []
            let r = frame.my_remote_listeners ?? []
            let onlineCount = frame.directory_online_count ?? max(directoryOnlineCount, r.count)
            let nextHarnesses = frame.harnesses ?? harnesses
            let harnessesChanged = nextHarnesses != harnesses
            if harnessesChanged { harnesses = nextHarnesses }
            if hasRenderedState && !pendingMutation && !harnessesChanged && h == hubs && l == listeners && a == available && r == myRemote && onlineCount == directoryOnlineCount {
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
                    rebuildDirectoryFacetBar()
                    if harnessesChanged { directoryTableView?.reloadData() }
                }
            }
            if headless { diagnostic("state hubs=\(h.count) listeners=\(l.count)" + (a.isEmpty ? "" : " available=\(a.count)")) }
        } else if frame.op == "directory", let peers = frame.peers {
            directoryAllPeers = peers
            directoryOnlineCount = frame.online_count ?? peers.count
            rebuildDirectoryFacetBar()
            applyDirectoryFilter(fromDaemonFrame: true, daemonMatchedCount: frame.matched_count)
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
        FileHandle.standardOutput.write(data + Data([10]))
    }
    func rebuild() {
        MenuControl.clearHoverWhere { $0.enclosingMenuItem != nil || $0.window !== self.directoryPanel }
        rebuildCount += 1
        menuActions.removeAll()
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
        func register(_ action: @escaping () -> Void) { menuActions.append(action) }
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
            else { statusText = "Disconnected" }
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
        for listener in listeners {
            let visual = resolveHarnessVisual(listener.harness, harnesses: map)
            let jumpUrl: String? = listener.can_jump
                ? (isSafeJumpUrl(listener.jump_url) ? listener.jump_url : expandJumpUrl(template: visual.jumpUrlTemplate, sessionId: listener.session_id, cwd: listener.cwd))
                : nil
            let canJump = jumpUrl != nil
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
            let hubPillWidth = min(76, max(46, ceil(measureTextWidth(listener.hub, font: btnFont)) + 22))
            let baseActionWidth: CGFloat = canJump ? 114 : 88
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
            copy.pressed = { [weak copy] in
                NSPasteboard.general.clearContents(); NSPasteboard.general.setString(listener.scoped_id, forType: .string)
                copy?.copied()
            }
            card.addSubview(copy)
            let cwd = listener.cwd == home ? "~" : listener.cwd.hasPrefix(home + "/") ? "~" + listener.cwd.dropFirst(home.count) : listener.cwd
            let branch = (listener.branch ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            let pathText = (!branch.isEmpty && branch != "HEAD") ? "\(cwd) · \(branch)" : cwd
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
                let ring = (1..<hubs.count).map { hubs[(currentIndex + $0) % hubs.count] }.filter { $0.name != listener.hub }
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
            var scope = listener.scope
            let toggle = MenuControl(scope == "self" ? "Self" : "Team", frame: NSRect(x: x, y: cardHeight - 28, width: 62, height: 22),
                leadingSymbol: scope == "self" ? "lock.fill" : "person.2.fill")
            toggle.identifier = NSUserInterfaceItemIdentifier("scope:\(listener.scoped_id)")
            let scopeColor = { [weak toggle] in
                toggle?.style = scope == "workspace" ? .prominent : .neutral
            }
            scopeColor()
            toggle.pressed = { [weak self, weak toggle] in
                guard let self = self else { return }
                self.pendingMutation = true
                scope = scope == "self" ? "workspace" : "self"
                toggle?.setLeadingSymbol(scope == "self" ? "lock.fill" : "person.2.fill")
                toggle?.label.stringValue = scope == "self" ? "Self" : "Team"
                toggle?.setAccessibilityLabel(toggle?.label.stringValue); scopeColor()
                self.emit(["op": "set_scope", "hub": listener.hub, "listener_id": listener.listener_id, "scope": scope])
            }
            card.addSubview(toggle); x += 66
            if let jumpUrl = jumpUrl {
                let jumpLabel = (listener.jump_label?.isEmpty == false ? listener.jump_label! : "Open in \(visual.name)")
                let open = MenuControl(jumpLabel, frame: NSRect(x: x, y: cardHeight - 28, width: 22, height: 22), symbol: "arrow.up.forward.app")
                open.toolTip = jumpLabel; open.identifier = NSUserInterfaceItemIdentifier("open:\(listener.scoped_id)")
                let run = { [weak self, weak menu] in self?.openJumpUrl(jumpUrl); menu?.cancelTracking() }
                open.pressed = run; register(run); card.addSubview(open); x += 26
            }
            register { [weak copy] in copy?.pressed?() }
            register { [weak toggle] in toggle?.pressed?() }
            let stop = MenuControl("Stop listening", frame: NSRect(x: x, y: cardHeight - 28, width: 22, height: 22), symbol: "xmark")
            stop.toolTip = "Stop listening"; stop.style = .destructiveHover
            stop.identifier = NSUserInterfaceItemIdentifier("stop:\(listener.scoped_id)")
            let unlisten = { self.emit(["op": "unlisten", "hub": listener.hub, "listener_id": listener.listener_id]) }
            stop.pressed = unlisten; card.addSubview(stop); register(unlisten); register(unlisten)
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
                let row = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 70))
                let card = roundedView(NSRect(x: 10, y: 3, width: 360, height: 64), radius: 10, color: .labelColor.withAppearanceAlpha(0.045))
                let icon = NSImageView(frame: NSRect(x: 12, y: 22, width: 24, height: 24))
                icon.image = NSImage(systemSymbolName: "waveform.slash", accessibilityDescription: "No active listeners")
                icon.contentTintColor = .secondaryLabelColor; card.addSubview(icon)
                card.addSubview(textLabel("No active listeners", NSRect(x: 44, y: 33, width: 304, height: 18), size: 13, weight: .semibold))
                card.addSubview(textLabel("Call cito_listen in a session to receive notifications",
                    NSRect(x: 44, y: 12, width: 304, height: 16), size: 11, color: .secondaryLabelColor))
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
                    copy.pressed = { [weak copy] in
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(peer.scoped_id, forType: .string)
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
        var candidateActions: [() -> Void] = []
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
                for hub in hubs.reversed() {
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
                } else if visual.jumpUrlTemplate != nil {
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
                        candidateActions.append({ [weak listenBtn] in listenBtn?.pressed?() })
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
            if directoryOnlineCount > 0 {
                let countBadge = textLabel("\(directoryOnlineCount) online", NSRect(x: 256, y: 5, width: 96, height: 16),
                    size: 11, weight: .regular, color: .secondaryLabelColor)
                countBadge.alignment = .right
                countBadge.identifier = NSUserInterfaceItemIdentifier("directory_online_badge")
                openDir.addSubview(countBadge)
            }
            openDir.pressed = { [weak self, weak menu] in
                guard let self = self else { return }
                menu?.cancelTracking()
                self.openDirectoryPanel()
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
            footer.addSubview(stopAll); register { [weak stopAll] in stopAll?.pressed?() }
        }
        hubActions.forEach { register($0) }
        let quit = MenuControl("Quit Cito ⌘Q", frame: NSRect(x: 262, y: 6, width: 106, height: 24))
        quit.identifier = NSUserInterfaceItemIdentifier("quit")
        quit.pressed = { self.emit(["op": "quit"]) }
        footer.addSubview(quit); register { [weak quit] in quit?.pressed?() }
        candidateActions.forEach { register($0) }
        let footerItem = host(footer, id: "footer")
        footerItem.target = self; footerItem.action = #selector(quitMenu)
        footerItem.keyEquivalent = "q"; footerItem.keyEquivalentModifierMask = [.command]
        menu.update()
    }
    @objc func quitMenu() { emit(["op": "quit"]) }

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
        let searchField = NSSearchField(frame: NSRect(x: 16, y: 9, width: 500, height: 28))
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
        facetBar.addSubview(roundedView(NSRect(x: 0, y: 0, width: 560, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
        directoryFacetBar = facetBar
        root.addSubview(facetBar)
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
        root.addSubview(scroll)

        // Footer Shortcut Bar (560 × 28 pt, y: 0)
        let footerBar = NSView(frame: NSRect(x: 0, y: 0, width: 560, height: 28))
        footerBar.identifier = NSUserInterfaceItemIdentifier("directory_footer")
        footerBar.addSubview(roundedView(NSRect(x: 0, y: 27, width: 560, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
        let shortcuts = textLabel("↵ Copy Selected ID    ⎋ Close", NSRect(x: 16, y: 6, width: 260, height: 16), size: 11, color: .secondaryLabelColor)
        shortcuts.identifier = NSUserInterfaceItemIdentifier("directory_shortcuts")
        footerBar.addSubview(shortcuts)
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

    func rebuildDirectoryFacetBar() {
        guard let facetBar = directoryFacetBar else { return }
        let map = harnessMap
        var unionHarnesses = Set<String>(harnesses.map { $0.id })
        for peer in directoryAllPeers where !peer.harness.isEmpty { unionHarnesses.insert(peer.harness) }
        if directoryHarnessFilter != "all" && !directoryHarnessFilter.isEmpty {
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
        for (id, pill) in directoryFacetControls {
            let active = (id == "dir_scope:\(directoryScopeFilter)") || (id == "dir_harness:\(directoryHarnessFilter)")
            pill.style = active ? .prominent : .neutral
        }
    }

    func openDirectoryPanel() {
        ensureDirectoryPanel()
        directoryVisible = true
        applyDirectoryFilter(fromDaemonFrame: false, daemonMatchedCount: nil)
        if !headless, let window = directoryPanel {
            if let screen = NSScreen.main {
                let area = screen.visibleFrame
                window.setFrameOrigin(NSPoint(x: area.midX - 280, y: area.midY - 180))
            }
            NSApp.activate(ignoringOtherApps: true)
            window.makeKeyAndOrderFront(nil)
            if let field = directorySearchField { window.makeFirstResponder(field) }
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
        directoryFilteredPeers = fromDaemonFrame ? directoryAllPeers : directoryAllPeers.filter { matchesDirectoryPeer($0) }
        directoryMatchedCount = daemonMatchedCount ?? directoryFilteredPeers.count
        if directoryFilteredPeers.isEmpty {
            directorySelectedIndex = 0
        } else if let prev = previousSelectedId, let idx = directoryFilteredPeers.firstIndex(where: { $0.scoped_id == prev }) {
            directorySelectedIndex = idx
        } else {
            directorySelectedIndex = min(max(0, directorySelectedIndex), directoryFilteredPeers.count - 1)
        }
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
            guard !directoryFilteredPeers.isEmpty else { return }
            directorySelectedIndex = min(directoryFilteredPeers.count - 1, directorySelectedIndex + 1)
            directoryTableView?.reloadData()
            directoryTableView?.selectRowIndexes(IndexSet(integer: directorySelectedIndex), byExtendingSelection: false)
            directoryTableView?.scrollRowToVisible(directorySelectedIndex)
            directoryTableView?.layoutSubtreeIfNeeded()
        case "up":
            guard !directoryFilteredPeers.isEmpty else { return }
            directorySelectedIndex = max(0, directorySelectedIndex - 1)
            directoryTableView?.reloadData()
            directoryTableView?.selectRowIndexes(IndexSet(integer: directorySelectedIndex), byExtendingSelection: false)
            directoryTableView?.scrollRowToVisible(directorySelectedIndex)
            directoryTableView?.layoutSubtreeIfNeeded()
        case "return":
            guard directoryFilteredPeers.indices.contains(directorySelectedIndex) else { return }
            let peer = directoryFilteredPeers[directorySelectedIndex]
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(peer.scoped_id, forType: .string)
            if let table = directoryTableView,
               let cell = table.view(atColumn: 0, row: directorySelectedIndex, makeIfNecessary: true) as? DirectoryPeerCellView {
                cell.copyControl.copied()
            }
        case "escape":
            closeDirectoryPanel(emitAction: true)
        default:
            break
        }
    }

    func controlTextDidChange(_ obj: Notification) {
        guard let field = obj.object as? NSSearchField, field === directorySearchField else { return }
        directoryQuery = field.stringValue
        applyDirectoryFilter(fromDaemonFrame: false, daemonMatchedCount: nil)
        emit(["op": "query_directory", "query": directoryQuery, "scope": directoryScopeFilter, "harness": directoryHarnessFilter])
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
        if textView.hasMarkedText() { return false }
        if commandSelector == #selector(NSResponder.moveDown(_:)) { handleDirectoryKey("down"); return true }
        if commandSelector == #selector(NSResponder.moveUp(_:)) { handleDirectoryKey("up"); return true }
        if commandSelector == #selector(NSResponder.insertNewline(_:)) { handleDirectoryKey("return"); return true }
        if commandSelector == #selector(NSResponder.cancelOperation(_:)) { handleDirectoryKey("escape"); return true }
        return false
    }

    func numberOfRows(in tableView: NSTableView) -> Int {
        return directoryFilteredPeers.count
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard directoryFilteredPeers.indices.contains(row) else { return nil }
        let id = NSUserInterfaceItemIdentifier("DirectoryPeerCell")
        let cell = (tableView.makeView(withIdentifier: id, owner: self) as? DirectoryPeerCellView)
            ?? DirectoryPeerCellView(frame: NSRect(x: 0, y: 0, width: 560, height: 52))
        cell.configure(with: directoryFilteredPeers[row], selected: row == directorySelectedIndex, harnesses: harnessMap)
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard let table = notification.object as? NSTableView, table === directoryTableView else { return }
        let row = table.selectedRow
        guard directoryFilteredPeers.indices.contains(row) else { return }
        directorySelectedIndex = row
        for r in 0..<table.numberOfRows {
            if let cell = table.view(atColumn: 0, row: r, makeIfNecessary: false) as? DirectoryPeerCellView {
                cell.setSelected(r == row)
            }
        }
        table.layoutSubtreeIfNeeded()
    }

    // Headless inspection projects real AppKit objects; it does not synthesize a parallel menu model.
    func inspectMenu() {
        func components(_ color: NSColor?) -> [CGFloat] {
            guard let rgb = color?.usingColorSpace(.deviceRGB) else { return [] }
            return [rgb.redComponent, rgb.greenComponent, rgb.blueComponent, rgb.alphaComponent]
        }
        func project(_ view: NSView) -> [String: Any] {
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
                "children": childViews.map { project($0) }]
            result["background"] = components(view.layer?.backgroundColor.flatMap { NSColor(cgColor: $0) })
            result["border"] = components(view.layer?.borderColor.flatMap { NSColor(cgColor: $0) })
            result["border_width"] = view.layer?.borderWidth ?? 0
            if let image = view as? NSImageView {
                view.effectiveAppearance.performAsCurrentDrawingAppearance { result["tint"] = components(image.contentTintColor) }
            }
            if let cell = view as? DirectoryPeerCellView { result["reuse_identifier"] = cell.identifier?.rawValue ?? "" }
            if let label = view as? NSTextField { result["text"] = label.stringValue }
            if let control = view as? MenuControl {
                result["text"] = control.label.stringValue; result["tooltip"] = control.toolTip ?? ""
                result["green"] = control.label.textColor == NSColor.systemGreen
                result["hovered"] = control.isHovered
                control.effectiveAppearance.performAsCurrentDrawingAppearance { result["foreground"] = components(control.label.textColor) }
                if let symbol = control.symbolName { result["symbol"] = symbol }
            }
            return result
        }
        let items = cardMenu.items.compactMap { item -> [String: Any]? in
            guard let view = item.view else { return nil }
            var value = project(view); value["hidden"] = item.isHidden; return value
        }
        let directoryItems: [[String: Any]]
        if let rootView = directoryPanel?.contentView {
            directoryItems = [project(rootView)]
        } else {
            directoryItems = []
        }
        let selectedId = directoryFilteredPeers.indices.contains(directorySelectedIndex)
            ? directoryFilteredPeers[directorySelectedIndex].scoped_id : ""
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
            "available_count": available.count, "rebuilds": rebuildCount,
            "directory_open": directoryVisible, "directory_online_count": directoryOnlineCount,
            "directory_matched_count": directoryMatchedCount, "directory_query": directoryQuery,
            "directory_scope": directoryScopeFilter, "directory_harness": directoryHarnessFilter,
            "directory_selected_id": selectedId, "directory_items": directoryItems,
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
    func resolvedJumpUrl(for value: Payload) -> String? {
        if isSafeJumpUrl(value.jump_url) { return value.jump_url }
        let visual = resolveHarnessVisual(value.target_harness, harnesses: harnessMap)
        return expandJumpUrl(template: visual.jumpUrlTemplate, sessionId: value.target_session_id, cwd: value.target_cwd)
    }
    func actionLabel(for value: Payload) -> String {
        let visual = resolveHarnessVisual(value.target_harness, harnesses: harnessMap)
        if resolvedJumpUrl(for: value) != nil {
            if let label = value.jump_label, !label.isEmpty { return label }
            return "Open in \(visual.name) ↗"
        }
        return "\(value.target_harness) session · Click to dismiss"
    }
    func hide(clicked: Bool) {
        timer?.cancel()
        if clicked, visible, let value = payload, let url = resolvedJumpUrl(for: value) { openJumpUrl(url) }
        visible = false; panel?.orderOut(nil)
        if headless { diagnostic(clicked ? "banner clicked" : "banner hidden") }
    }
    func show(_ value: Payload) {
        let title = "Cito · \(value.target_title)" + (displayedCount > 1 ? " (\(displayedCount))" : "")
        let semanticLabel = (value.effective_wake ? "Notification" : "Passive") + (displayedCount > 1 ? " · ×\(displayedCount)" : "")
        let actionText = actionLabel(for: value)
        let hasJump = resolvedJumpUrl(for: value) != nil
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
        labels[3].textColor = hasJump ? .systemBlue : .tertiaryLabelColor
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

    func openJumpUrl(_ url: String) {
        guard isSafeJumpUrl(url) else { return }
        if let opener = ProcessInfo.processInfo.environment["CITO_BANNER_URL_OPENER"], !opener.isEmpty {
            let child = Process()
            child.executableURL = URL(fileURLWithPath: opener)
            child.arguments = [url]
            try? child.run()
        } else if let target = URL(string: url) {
            NSWorkspace.shared.open(target)
        }
    }
}

let app = NSApplication.shared
let companion = Companion()
app.setActivationPolicy(.accessory)
companion.start()
app.run()
