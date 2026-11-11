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

func formatStatusButton(_ button: NSButton, count: Int, unreadCount: Int, image: NSImage?) {
    button.image = image
    let title = (count > 0 ? " \(count)" : "") + (unreadCount > 0 ? " 🔔\(unreadCount)" : "")
    button.title = ""
    button.imagePosition = title.isEmpty ? .imageOnly : .imageLeading
    if !title.isEmpty {
        button.attributedTitle = NSAttributedString(string: title,
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

// Semantic foregrounds retain WCAG AA contrast on both translucent surface backdrops.
enum CitoPalette {
    private static func adaptive(_ light: NSColor, _ dark: NSColor) -> NSColor {
        NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? dark : light
        }
    }
    static let secondaryText = adaptive(
        NSColor(srgbRed: 0.28, green: 0.29, blue: 0.31, alpha: 1),
        NSColor(srgbRed: 0.78, green: 0.79, blue: 0.82, alpha: 1))
    static let successText = adaptive(
        NSColor(srgbRed: 0.04, green: 0.43, blue: 0.16, alpha: 1),
        NSColor(srgbRed: 0.28, green: 0.85, blue: 0.44, alpha: 1))
    static let warningText = adaptive(
        NSColor(srgbRed: 0.64, green: 0.28, blue: 0.00, alpha: 1),
        NSColor(srgbRed: 1.00, green: 0.70, blue: 0.25, alpha: 1))
    static let dangerText = adaptive(
        NSColor(srgbRed: 0.74, green: 0.10, blue: 0.14, alpha: 1),
        NSColor(srgbRed: 1.00, green: 0.44, blue: 0.42, alpha: 1))
    static let infoText = adaptive(
        NSColor(srgbRed: 0.00, green: 0.34, blue: 0.75, alpha: 1),
        NSColor(srgbRed: 0.40, green: 0.74, blue: 1.00, alpha: 1))
    static let copiedFill = adaptive(
        NSColor(srgbRed: 0.04, green: 0.43, blue: 0.16, alpha: 0.14),
        NSColor(srgbRed: 0.19, green: 0.82, blue: 0.35, alpha: 0.22))
    static let copiedBorder = adaptive(
        NSColor(srgbRed: 0.04, green: 0.43, blue: 0.16, alpha: 0.34),
        NSColor(srgbRed: 0.19, green: 0.82, blue: 0.35, alpha: 0.45))
    static let warningFill = adaptive(
        NSColor(srgbRed: 0.64, green: 0.28, blue: 0.00, alpha: 0.14),
        NSColor(srgbRed: 1.00, green: 0.62, blue: 0.04, alpha: 0.18))
    // Warning chips use an opaque text-surface base, avoiding stacked tint losses.
    static let warningSurface = NSColor(name: nil) { appearance in
        var color = NSColor.clear
        appearance.performAsCurrentDrawingAppearance {
            let fill = warningFill.usingColorSpace(.sRGB)
            color = NSColor.textBackgroundColor.blended(withFraction: fill?.alphaComponent ?? 0,
                of: fill?.withAlphaComponent(1) ?? .clear) ?? .textBackgroundColor
        }
        return color
    }
    static let warningBorder = adaptive(
        NSColor(srgbRed: 0.64, green: 0.28, blue: 0.00, alpha: 0.32),
        NSColor(srgbRed: 1.00, green: 0.62, blue: 0.04, alpha: 0.40))
    static let surfaceBackground = adaptive(
        NSColor(srgbRed: 236/255, green: 239/255, blue: 243/255, alpha: 1),
        NSColor(srgbRed: 30/255, green: 32/255, blue: 36/255, alpha: 1))
    // Filled controls keep white titles, so their fill stays dark in either appearance.
    static let prominentFill = NSColor(srgbRed: 0.00, green: 0.34, blue: 0.75, alpha: 1)
    static let destructiveFill = NSColor(srgbRed: 0.74, green: 0.10, blue: 0.14, alpha: 1)
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
func nodeDisplayTitle(_ title: String, harness: String, user: String) -> String {
    let raw = title.trimmingCharacters(in: .whitespacesAndNewlines)
    if harness == "human" && (raw.isEmpty || raw == "Human" || raw == "@\(user)" || raw == "@\(user) (Me)" || raw.hasPrefix("Human Node")) {
        return "You (@\(user))"
    }
    return title
}
func presentationError(_ raw: String) -> String {
    raw.replacingOccurrences(of: "^([A-Z0-9_]+:\\s*)+", with: "", options: .regularExpression)
}
func distinctAbout(_ raw: String?, title: String, user: String) -> String? {
    let value = (raw ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
    let lower = value.lowercased()
    return value.isEmpty || [title.lowercased(), "@\(user.lowercased())", "session", "agent session", "coding session", "human", "human receiver", "human node", "human channel"].contains(lower) ? nil : value
}
struct HarnessMeta: Decodable, Equatable {
    let id: String
    let display_name: String
    let sf_symbol: String
    let tint: String
    let tier: String
    let order: Int
    let kind: String?
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
    case "orange", "amber", "yellow": return CitoPalette.warningText
    case "blue": return CitoPalette.infoText
    case "purple": return .systemPurple
    case "green": return CitoPalette.successText
    case "pink": return .systemPink
    case "gray": return .systemGray
    default: return CitoPalette.infoText
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
            tint: CitoPalette.successText,
            tier: "stable",
            order: -1
        )
    }
    return HarnessVisual(
        id: harness,
        name: harness.capitalized,
        symbol: "bolt.horizontal.fill",
        tint: CitoPalette.infoText,
        tier: "custom",
        order: 3
    )
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
    override func mouseDown(with event: NSEvent) { guard isEnabled else { return }; pressed?() }
    override func accessibilityPerformPress() -> Bool { guard isEnabled else { return false }; pressed?(); return true }
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
            accessorySymbol = isHovered ? .labelColor : CitoPalette.secondaryText
        case .prominent:
            fill = CitoPalette.prominentFill
            border = .white.withAlphaComponent(isHovered ? 0.36 : 0.22)
            foreground = .white; primarySymbol = .white; accessorySymbol = .white
        case .subtleHeader:
            fill = isHovered ? .labelColor.withAppearanceAlpha(0.10) : .clear
            border = nil
            foreground = isHovered ? .labelColor : normalColor
            primarySymbol = isHovered ? .labelColor : CitoPalette.secondaryText
            accessorySymbol = primarySymbol
        case .destructiveHover:
            fill = isHovered ? CitoPalette.destructiveFill : .labelColor.withAppearanceAlpha(0.09)
            border = isHovered ? .white.withAlphaComponent(0.26) : .labelColor.withAppearanceAlpha(0.14)
            foreground = isHovered ? .white : CitoPalette.secondaryText
            primarySymbol = foreground; accessorySymbol = foreground
        }
        let isCopied = feedbackTimer?.isValid == true
        label.textColor = isCopied ? CitoPalette.successText : foreground
        leadingSymbolView?.contentTintColor = primarySymbol
        centerSymbolView?.contentTintColor = primarySymbol
        accessorySymbolView?.contentTintColor = accessorySymbol
        effectiveAppearance.performAsCurrentDrawingAppearance {
            // A copied chip uses the canonical surface beneath its translucent fill;
            // stacking it over a tinted card would lower the specified green below AA.
            let copiedFill = CitoPalette.copiedFill.usingColorSpace(.sRGB)
            let copiedBackground = CitoPalette.surfaceBackground.blended(
                withFraction: copiedFill?.alphaComponent ?? 0,
                of: copiedFill?.withAlphaComponent(1) ?? .clear)
            layer?.backgroundColor = (isCopied ? copiedBackground ?? CitoPalette.surfaceBackground : fill).cgColor
            layer?.borderColor = (isCopied ? CitoPalette.copiedBorder : border)?.cgColor
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
        label.stringValue = message; label.textColor = CitoPalette.successText
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
    let from_display_address: String?
    let target_display_address: String?
    let from_harness: String?

    var subtitle: String {
        (effective_wake ? "" : "[Passive] ") + "\(from_user)@\(from_host) · \(from_title)"
    }
    var displaySubtitle: String {
        let cleanHost = from_host.replacingOccurrences(of: "\\.local$", with: "", options: [.regularExpression, .caseInsensitive])
        let title = from_title.trimmingCharacters(in: .whitespacesAndNewlines)
        return "\(from_user)@\(cleanHost.isEmpty ? from_host : cleanHost)" + (title.isEmpty ? "" : " · \(title)")
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
    var appearanceChanged: (() -> Void)?
    private var hovered = false
    private var tracking: NSTrackingArea?
    private func applyBorderColor() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.borderColor = NSColor.labelColor.withAppearanceAlpha(hovered ? 0.28 : 0.16).cgColor
        }
    }
    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applyBorderColor()
        appearanceChanged?()
    }
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
        hovered = true; applyBorderColor()
    }
    override func mouseExited(with event: NSEvent) {
        NSCursor.arrow.set()
        hovered = false; applyBorderColor()
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
    let display_address: String
    let user: String?, owner_user: String?
    let channels: [String]?
}
struct Candidate: Decodable, Equatable {
    let harness: String, session_id: String, title: String, cwd: String
    let branch: String?, endpoint_pid: Int?, updated_at_ms: Double?
    let listening_hubs: [String]?
    let needs_reload: Bool?
    let reload_hint: String?
}
struct RemotePeer: Decodable, Equatable {
    let hub: String, listener_id: String, public_id: String, scoped_id: String
    let user: String, host_id: String, host_name: String, harness: String
    let display_address: String
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
    let from_display_address: String?
    let target_display_address: String?
    let message_truncated: Bool?
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
    var pairing_command: String?
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
    let configured: Bool?
    var isConfigured: Bool { configured == true }
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
    let source_op: String?
    let request_id: String?
    let composer_error: String?
    let channel_id: String?
    let code: String?
    let instruction: String?
    let pairing_command: String?
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
    let query: String?
    let scope: String?
    let harness: String?
    let activity_total: Int?
    let activity_unread_count: Int?
    let activity_filter: String?
    let activity_query: String?
    let message: String?
    let part: Int?
    let complete: Bool?
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
        line1RepoLabel = textLabel("", NSRect(x: 232, y: 26, width: 180, height: 18), size: 11.5, weight: .regular, color: CitoPalette.secondaryText)
        line2UserLabel = textLabel("", NSRect(x: 52, y: 7, width: 80, height: 16), size: 11.5, weight: .medium, color: .labelColor)
        line2MetaLabel = textLabel("", NSRect(x: 132, y: 7, width: 220, height: 16), size: 11.5, weight: .regular, color: CitoPalette.secondaryText)
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

    func configure(with peer: RemotePeer, selected: Bool, harnesses: [String: HarnessMeta], onCopy: @escaping (String, MenuControl) -> Void, onMessage: (() -> Void)? = nil) {
        badgeView.frame.origin.y = 12
        line2MetaLabel.maximumNumberOfLines = 1
        line2MetaLabel.lineBreakMode = .byTruncatingTail
        inspectionId = "dir_peer:\(peer.scoped_id)"
        isSelectedRow = selected
        frame = NSRect(x: 0, y: 0, width: 560, height: 52)
        applySelectionColor()
        let visual = resolveHarnessVisual(peer.harness, harnesses: harnesses)
        badgeView.fillColor = visual.tint.withAppearanceAlpha(0.88)
        badgeView.strokeColor = .white.withAlphaComponent(0.18)
        badgeIcon.image = NSImage(systemSymbolName: visual.symbol, accessibilityDescription: visual.name)
        badgeIcon.contentTintColor = .white

        let short = peer.display_address
        let copyFont = NSFont.monospacedSystemFont(ofSize: 11, weight: .medium)
        let copyWidth = max(measureTextWidth("✓ Copied", font: copyFont), measureTextWidth(short, font: copyFont)) + 28
        copyControl.feedbackTimer?.invalidate()
        copyControl.feedbackTimer = nil
        copyControl.isHidden = false
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
            if let control = copyControl { onCopy(peer.display_address, control) }
        }

        let msgWidth: CGFloat = 86
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
        let normalizedTitle = nodeDisplayTitle(peer.title, harness: peer.harness, user: peer.user)
        let displayTitle = peer.harness == "human" && normalizedTitle == "You (@\(peer.user))" ? "@\(peer.user)" + (peer.is_self_principal == true ? " (You)" : "") : normalizedTitle
        let titleW = min(availWidth, measureTextWidth(displayTitle, font: titleFont) + 4)
        line1TitleLabel.stringValue = displayTitle
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
        let displayAbout = distinctAbout(peer.about, title: peer.title, user: peer.user)
        line2MetaLabel.stringValue = " · \(hostText)" + (displayAbout.map { " · \($0)" } ?? "")
        line2MetaLabel.frame = NSRect(x: 52 + userW, y: 7, width: max(0, availWidth - userW), height: 16)
        line2MetaLabel.identifier = NSUserInterfaceItemIdentifier("dir_meta:\(peer.scoped_id)")
        line2MetaLabel.toolTip = line2MetaLabel.stringValue
    }

    func configureActivity(with item: ActivityItem, selected: Bool, harnesses: [String: HarnessMeta], onCopy: @escaping (MenuControl) -> Void, onReply: @escaping () -> Void) {
        inspectionId = "activity_row:\(item.signal_id)"
        isSelectedRow = selected
        let expanded = selected && (item.message.contains("\n") || measureTextWidth(item.message,font:.systemFont(ofSize:11.5)) > 330)
        let rowHeight: CGFloat = expanded ? 108 : 52
        frame = NSRect(x:0,y:0,width:560,height:rowHeight)
        applySelectionColor()
        let visual = resolveHarnessVisual(item.from_harness ?? item.target_harness, harnesses:harnesses)
        badgeView.frame.origin.y = rowHeight - 40
        badgeView.fillColor = visual.tint.withAppearanceAlpha(0.88)
        badgeView.strokeColor = .white.withAlphaComponent(0.18)
        badgeIcon.image = NSImage(systemSymbolName:visual.symbol,accessibilityDescription:visual.name)
        badgeIcon.contentTintColor = .white
        let canReply = item.for_me && !(item.from_display_address ?? item.from_scoped_id ?? "").isEmpty
        msgControl.isHidden = !canReply
        msgControl.frame = NSRect(x:404,y:rowHeight-36,width:68,height:22)
        msgControl.setLeadingSymbol("arrowshape.turn.up.left")
        msgControl.label.stringValue = "Reply"
        msgControl.setAccessibilityLabel("Reply")
        msgControl.toolTip = "Reply"
        msgControl.identifier = NSUserInterfaceItemIdentifier("reply_activity:\(item.signal_id)")
        msgControl.pressed = canReply ? onReply : nil
        copyControl.isHidden = false
        copyControl.feedbackTimer?.invalidate(); copyControl.feedbackTimer = nil
        copyControl.frame = NSRect(x:478,y:rowHeight-36,width:66,height:22)
        copyControl.setLeadingSymbol("doc.on.doc")
        copyControl.label.font = .systemFont(ofSize:11,weight:.medium)
        copyControl.label.stringValue = "Copy"
        copyControl.setAccessibilityLabel("Copy message")
        copyControl.toolTip = "Copy message"
        copyControl.identifier = NSUserInterfaceItemIdentifier("copy_activity_msg:\(item.signal_id)")
        copyControl.pressed = { [weak copyControl] in if let c = copyControl { onCopy(c) } }
        copyControl.applyVisualState()
        let held = ["stale_generation":" · Held (session reloaded)","driver_unavailable":" · Held (app unavailable)"][item.held_reason ?? ""] ?? ""
        line1TitleLabel.stringValue = (item.unread ? "● " : "") + "@\(item.from_user) → " + (item.target_harness == "human" ? "You" : item.target_title) + held
        line1TitleLabel.frame = NSRect(x:52,y:rowHeight-26,width:canReply ? 346 : 418,height:18)
        line1TitleLabel.identifier = NSUserInterfaceItemIdentifier("act_title:\(item.signal_id)")
        line1RepoLabel.isHidden = true; line2UserLabel.isHidden = true
        line2MetaLabel.toolTip = item.message
        line2MetaLabel.stringValue = expanded ? item.message : item.message.replacingOccurrences(of:"\\s*\\n+\\s*",with:"  ",options:.regularExpression)
        line2MetaLabel.maximumNumberOfLines = expanded ? 4 : 1
        line2MetaLabel.lineBreakMode = expanded ? .byWordWrapping : .byTruncatingTail
        line2MetaLabel.frame = NSRect(x:52,y:7,width:expanded ? 492 : (canReply ? 346 : 418),height:expanded ? 64 : 16)
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
    var uniqueSessionCount: Int {
        Dictionary(grouping: listeners.filter { $0.harness != "human" }, by: { $0.harness })
            .values.reduce(0) { $0 + Set($1.map { $0.session_id }).count }
    }
    var humanListening: Bool { listeners.contains { $0.harness == "human" } }
    var wakeActionText = ""
    var testAppearance: NSAppearance?
    var hubs: [Hub] = []
    var listeners: [Listener] = []
    var available: [Candidate] = []
    var myRemote: [RemotePeer] = []
    var harnesses: [HarnessMeta] = []
    var harnessMap: [String: HarnessMeta] { Dictionary(uniqueKeysWithValues: harnesses.map { ($0.id, $0) }) }
    var selectedTargetHub: String = ""
    var myRemoteCollapsed = false
    var collapsedGroups = Set<String>()
    func statusColor(_ status: String) -> NSColor {
        status == "connected" ? CitoPalette.successText : status == "connecting" ? CitoPalette.warningText : status == "auth_required" ? CitoPalette.dangerText : .systemGray
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
    var directoryRequestId = ""
    var composerDraftIdempotencyKey = UUID().uuidString
    struct PendingMessageCopy {
        let requestId: String
        weak var control: MenuControl?
        var buffer = ""
        var nextPart = 0
    }
    var pendingMessageCopy: PendingMessageCopy?
    var activityTotal = 0
    var activityUnreadCount = 0
    var serverActivityFilter: String?
    var serverActivityQuery = ""
    func copyText(_ text: String, control: MenuControl? = nil, feedback: String = "✓ Copied") {
        pendingMessageCopy = nil
        clipboardString = text
        appPasteboard.clearContents()
        appPasteboard.setString(text, forType: .string)
        control?.copied(feedback)
    }
    func showMessageCopyError(_ message: String) {
        pendingMessageCopy = nil
        setComposerStatus(message)
        if directoryComposerBar?.isHidden != false {
            directorySummaryLabel?.stringValue = presentationError(message)
            directorySummaryLabel?.toolTip = presentationError(message)
        }
    }
    func queryDirectory(op: String = "query_directory") {
        directoryRequestId = UUID().uuidString
        emit(["op":op,"request_id":directoryRequestId,"query":directoryQuery,"scope":directoryScopeFilter,"harness":directoryHarnessFilter,"mode":directoryMode,"filter":activityFilter])
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
    func setComposerStatus(_ message: String) {
        composerErrorLabel?.stringValue = presentationError(message)
        composerErrorLabel?.toolTip = presentationError(message)
    }
    struct PendingComposerSend {
        let text: String
        let requestId: String
        let target: String
        let hub: String
        let replySignalId: String?
    }
    var pendingComposer: PendingComposerSend?
    var settings: SettingsPayload?
    var settingsVisible = false
    var settingsActiveTab = "apps"
    var settingsPanel: KeyPanel?
    var addHubFormOpen = false
    var addHubError: String?
    struct PendingHubSave {
        let name: String
        let url: String
        let requestId: String
        let generation: Int
    }
    var pendingHubSave: PendingHubSave?
    var settingsBannerError: String?
    var menuBannerError: String?
    var hubInvitePending = false
    var hubInvitePastedUntil = Date.distantPast
    var editingExistingHub = false
    var editingOriginalHub = ""
    var hostHubPortPending = false
    var hostHubPortSaved = false
    var hostHubPortSavedUntil = Date.distantPast
    var hostHubPortError: String?
    var hostHubPortDraft: String?
    var submittedHostHubPort: Int?
    var submittedHostHubRequestId = ""
    var addHubName = ""
    var addHubUrl = ""
    var addHubDraftGeneration = 0
    var addHubAuth = "none"
    var addHubToken = ""
    var addHubUser = ""
    var addHubIssuer = ""
    var addHubClientId = ""
    var settingsFieldControls: [String: NSTextField] = [:]
    var channelFieldDrafts: [String: String] = [:]
    var editingHubGateway = Set<String>()
    var pendingChannelValues: [String: (hub: String, channelId: String, scope: String, values: [String: String])] = [:]
    var selectedChannelHub = ""
    var channelSettingsByHub: [String: [ChannelSetupItem]] = [:]
    var channelInstructions: [String: String] = [:]
    var channelResults: [String: (ok: Bool, detail: String)] = [:]
    var settingsScrollOffsets: [String: CGFloat] = [:]
    var pendingOnboardingMarker: String?
    var settingsKeyMonitor: Any?
    var effectiveChannelHub: String {
        let inventory = settings?.hubs.map { $0.name } ?? hubs.map { $0.name }
        if !selectedChannelHub.isEmpty { return inventory.contains(selectedChannelHub) ? selectedChannelHub : "local" }
        return settings?.hubs.first(where: { $0.is_default })?.name ?? hubs.first?.name ?? "local"
    }
    func draftKey(_ fieldId: String) -> String { effectiveChannelHub + "|" + fieldId }
    func clearChannelDrafts(_ channel: ChannelSetupItem, scope: String? = nil, hub: String? = nil) {
        for field in channel.fields where scope == nil || field.scope == scope {
            channelFieldDrafts.removeValue(forKey: "\(hub ?? effectiveChannelHub)|channel_field:\(channel.id):\(field.key)")
        }
    }
    func acceptSettings(_ value: SettingsPayload, hub: String? = nil, savedChannel: String? = nil, savedRequest: String? = nil, confirmHubSave: Bool = false) {
        settings = value
        if hostHubPortPending && savedRequest == submittedHostHubRequestId &&
           (value.host_hub.port == submittedHostHubPort || value.host_hub.error != nil) {
            hostHubPortPending = false
            hostHubPortSaved = value.host_hub.port == submittedHostHubPort && value.host_hub.error == nil
            if hostHubPortSaved { hostHubPortSavedUntil = Date().addingTimeInterval(1.2) }
            hostHubPortError = nil
            if hostHubPortSaved && Int(hostHubPortDraft ?? "") == submittedHostHubPort { hostHubPortDraft = nil }
        }
        if confirmHubSave, let pending = pendingHubSave, savedRequest == pending.requestId,
           let saved = value.hubs.first(where: { $0.name == pending.name && $0.url == pending.url }) {
            pendingHubSave = nil
            addHubError = saved.error
            if saved.error == nil && addHubDraftGeneration == pending.generation {
                addHubFormOpen = false; addHubToken = ""
            }
        }
        let target = value.settings_hub ?? value.channel_hub ?? hub ?? effectiveChannelHub
        channelSettingsByHub[target] = value.channels ?? []
        for channel in value.channels ?? [] {
            let key=target + "|" + channel.id
            // An older acknowledgement cannot clear a newer draft or a failed
            // save, and test/pair result refreshes are not save acknowledgements.
            var acknowledged: (scope: String, values: [String: String])?
            if savedChannel == channel.id, let savedRequest = savedRequest,
               let entry = pendingChannelValues[savedRequest], entry.hub == target && entry.channelId == channel.id {
                pendingChannelValues.removeValue(forKey: savedRequest)
                acknowledged = (scope: entry.scope, values: entry.values)
            }
            for field in channel.fields {
                let draft = target + "|channel_field:\(channel.id):\(field.key)"
                if let sent = acknowledged?.values[field.key], channelFieldDrafts[draft] == sent {
                    channelFieldDrafts.removeValue(forKey: draft)
                } else if field.type != "secret" && channelFieldDrafts[draft] == (field.value ?? "") {
                    channelFieldDrafts.removeValue(forKey: draft)
                }
            }
            if acknowledged?.scope == "hub" && (channel.hub_provided || channel.status != "not_configured" || (channel.fields.contains { $0.scope == "hub" } && channel.fields.filter { $0.scope == "hub" }.allSatisfy { $0.configured })) {
                editingHubGateway.remove(key)
            }
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
            guard let self = self else { return event }
            let window = event.window ?? NSApp.keyWindow
            if (window?.firstResponder as? NSTextInputClient)?.hasMarkedText() == true { return event }
            let command = event.modifierFlags.contains(.command)
            if command && event.charactersIgnoringModifiers == "," { self.openSettingsPanel(); return nil }
            if command && event.charactersIgnoringModifiers == "k" { self.openDirectoryPanel(mode:"directory"); return nil }
            let key: String?
            if command && event.charactersIgnoringModifiers == "w" { key = "command-w" }
            else { key = [53:"escape",126:"up",125:"down",36:"return",76:"return"][Int(event.keyCode)] }
            if let key = key {
                if self.directoryVisible && window === self.directoryPanel { self.handleDirectoryKey(key); return nil }
                if self.settingsVisible && window === self.settingsPanel { self.handleSettingsKey(key); return nil }
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
                formatStatusButton(button, count: 0, unreadCount: 0, image: statusImage)
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
            if ["test_appearance", "test_inspect", "test_inspect_menu", "test_render_png"].contains(op),
               let name = object["appearance"] as? String, ["light", "dark"].contains(name) {
                testAppearance = NSAppearance(named: name == "light" ? .aqua : .darkAqua)
                for item in cardMenu.items { item.view?.appearance = testAppearance }
                for window in [directoryPanel, settingsPanel, panel].compactMap({ $0 }) {
                    window.appearance = testAppearance
                    window.contentView?.appearance = testAppearance
                }
            }
            if op == "test_inspect" || op == "test_inspect_menu" { inspectMenu(); return }
            if op == "test_oracle_calibration", let kind = object["kind"] as? String {
                if kind == "reset" { rebuild(); if settingsPanel != nil { rebuildSettingsPanel() }; return }
                let roots = cardMenu.items.compactMap { $0.view }
                func control(_ id: String) -> MenuControl? {
                    for root in roots { if let c = findControl(in:root,id:id) { return c } }
                    return nil
                }
                if let c = control("quit") {
                    if kind == "truncated_labels" { c.label.stringValue = String(repeating:"Wide chrome ",count:20) }
                    if kind == "jargon_leaks" { c.label.stringValue = "Human receiver" }
                    if kind == "low_contrast_labels" { c.normalColor = CitoPalette.surfaceBackground.withAlphaComponent(0.01); c.applyVisualState() }
                }
                if kind == "address_mismatches", let listener = listeners.first { control("copy:\(listener.scoped_id)")?.label.stringValue = "wrong address" }
                if kind == "activity_jargon", let cell = directoryTableView?.view(atColumn:0,row:0,makeIfNecessary:true) as? DirectoryPeerCellView {
                    cell.line1TitleLabel.stringValue = "@bob → Human Node"
                }
                if kind == "banner_jargon", labels.count > 1 { labels[1].stringValue = "[Passive] " + labels[1].stringValue }
                if let header = roots.first(where: { $0.identifier?.rawValue == "header" }) {
                    if kind == "overlapping_siblings" || kind == "clipped_views" {
                        let probe = MenuControl("Probe",frame:NSRect(x:kind == "clipped_views" ? 370 : 12,y:12,width:60,height:18))
                        probe.identifier = NSUserInterfaceItemIdentifier("oracle_probe")
                        header.addSubview(probe)
                    }
                }
                return
            }
            if op == "test_render_png", let surface = object["surface"] as? String, let path = object["path"] as? String {
                renderPNG(surface: surface, path: path); return
            }
            if op == "test_menu_open" { menuWillOpen(cardMenu); return }
            if op == "test_menu_close" { menuDidClose(cardMenu); return }
            if op == "test_appearance" { return }
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
                queryDirectory()
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
                    composerDraftIdempotencyKey = UUID().uuidString
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
                composerDraftIdempotencyKey = UUID().uuidString
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
                handleSettingsKey(key)
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
        if frame.ok == false || frame.op == "error", let requestId = frame.request_id {
            pendingChannelValues.removeValue(forKey: requestId)
        }
        if frame.op == "activity_message" {
            guard var pending = pendingMessageCopy, frame.request_id == pending.requestId else { return }
            if let message = frame.message, frame.part == pending.nextPart, frame.error == nil, frame.ok != false {
                pending.buffer += message; pending.nextPart += 1
                if frame.complete != false { copyText(pending.buffer, control:pending.control) }
                else { pendingMessageCopy = pending }
            }
            else { showMessageCopyError(frame.error ?? "Message retrieval failed.") }
            return
        }
        if frame.op == "error", let pending = pendingMessageCopy, frame.request_id == pending.requestId {
            showMessageCopyError(frame.error ?? "Message retrieval failed."); return
        }
        if frame.op == "composer_result" {
            guard let pending = pendingComposer, frame.request_id == pending.requestId else { return }
            if frame.ok == true {
                if composerTarget == pending.target && composerTargetHub == pending.hub && composerReplySignalId == pending.replySignalId && composerInputField?.stringValue == pending.text { closeComposer() }
                else { setComposerStatus("Sent. Your newer draft is preserved.") }
            } else {
                setComposerStatus(frame.error ?? "Send failed. Your draft is preserved.")
            }
            pendingComposer = nil
            return
        }
        if frame.op == "error" {
            let sourceOp = frame.source_op ?? ""
            // A command may emit both its result and a trailing generic error.
            // Neither can settle a retry or overwrite its feedback.
            if ["send_signal", "send_reply_from_human"].contains(sourceOp),
               (pendingComposer == nil || frame.request_id != pendingComposer?.requestId) { return }
            if sourceOp == "set_host_hub", let requestId = frame.request_id,
               requestId != submittedHostHubRequestId { return }
            let isAddHubOp = ["setup_save_hub", "save_hub"].contains(sourceOp)
            if isAddHubOp, let requestId = frame.request_id,
               requestId != pendingHubSave?.requestId { return }
            // Only the matching save result owns an active form submission.
            // Untagged invite/transport failures remain visible in the settings banner.
            let isAddHubError: Bool
            if let pending = pendingHubSave {
                isAddHubError = isAddHubOp && frame.request_id == pending.requestId
            } else {
                isAddHubError = addHubFormOpen && (isAddHubOp || sourceOp.isEmpty)
            }
            let error = frame.error ?? "Unknown daemon error"
            if ["switch_hub", "set_scope", "listen_session", ""].contains(sourceOp) { pendingMutation = false }
            if (sourceOp == "set_host_hub" || sourceOp.isEmpty) &&
               frame.request_id == submittedHostHubRequestId {
                hostHubPortPending = false
                submittedHostHubRequestId = ""
            }
            if (["paste_hub_invite", "setup_save_hub", "save_hub"].contains(sourceOp) && frame.request_id == nil) || sourceOp.isEmpty {
                hubInvitePending=false; hubInvitePastedUntil = .distantPast
            }
            if isAddHubError { pendingHubSave = nil }
            let isComposerError = ["send_signal", "send_reply_from_human", ""].contains(sourceOp) &&
                pendingComposer != nil && frame.request_id == pendingComposer?.requestId
            if isComposerError {
                setComposerStatus(error)
                pendingComposer = nil
            } else if isAddHubError { addHubError = error }
            else if settingsVisible || pendingHubSave != nil { settingsBannerError = error }
            else { menuBannerError = error }
            diagnostic("error \(error)")
            rebuild()
            if settingsPanel != nil { rebuildSettingsPanel() }
            return
        }
        if frame.op == "settings", let value = frame.settings {
            if hubInvitePending && frame.ok == true && ["setup_save_hub","save_hub"].contains(frame.source_op ?? "") {
                hubInvitePending=false; hubInvitePastedUntil=Date().addingTimeInterval(1.2)
            }
            let saved = frame.ok == true && ["channel_save","save_hub_service","configure_human_channel"].contains(frame.source_op ?? "") ? frame.channel_id : nil
            acceptSettings(value, hub: frame.hub, savedChannel: saved, savedRequest: frame.request_id, confirmHubSave: frame.ok == true && ["setup_save_hub","save_hub"].contains(frame.source_op ?? ""))
            if settingsPanel != nil { rebuildSettingsPanel() }
            return
        }
        if frame.op == "pair_code", let id = frame.channel_id {
            let hub = frame.hub ?? effectiveChannelHub
            channelInstructions["\(hub)|\(id)"] = frame.instruction ?? frame.code ?? ""
            if let index = channelSettingsByHub[hub]?.firstIndex(where: { $0.id == id }) {
                channelSettingsByHub[hub]?[index].pairing_command = frame.pairing_command ?? frame.instruction
            }
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
            let activityChanged = nextUnread != unreadCount || nextActivity != activityAllItems || (frame.activity_total != nil && frame.activity_total != activityTotal)
            activityTotal = frame.activity_total ?? activityTotal
            activityUnreadCount = frame.activity_unread_count ?? frame.unread_count ?? activityUnreadCount
            serverActivityFilter = frame.activity_filter
            serverActivityQuery = frame.activity_query ?? ""
            unreadCount = nextUnread
            activityAllItems = nextActivity
            let nextHarnesses = frame.harnesses ?? harnesses
            let harnessesChanged = nextHarnesses != harnesses
            if harnessesChanged { harnesses = nextHarnesses }
            let nextSettings = frame.settings ?? settings
            // An unchanged save acknowledgement still needs to refresh the settings feedback.
            let settingsChanged = nextSettings != settings || hostHubPortPending
            if settingsChanged, let value = nextSettings { acceptSettings(value, hub: frame.hub ?? value.hubs.first(where: { $0.is_default })?.name ?? h.first?.name) }
            if let error = frame.composer_error, !error.isEmpty { setComposerStatus(error) }
            if hasRenderedState && !pendingMutation && !harnessesChanged && !settingsChanged && !activityChanged && h == hubs && l == listeners && a == available && r == myRemote && onlineCount == directoryOnlineCount {
                let count = uniqueSessionCount
                let warning = hubs.contains { $0.status != "connected" || $0.error != nil }
                statusImage = makeCitoStatusIcon(warning ? .warning : count > 0 ? .active : .idle)
                if let button = status?.button ?? headlessStatusButton {
                    formatStatusButton(button, count: count, unreadCount: unreadCount, image: statusImage)
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
                if settingsChanged && settingsPanel != nil {
                    rebuildSettingsPanel()
                }
            }
            if !hasCheckedFirstLaunchOnboarding, let s = settings, !s.apps.isEmpty {
                hasCheckedFirstLaunchOnboarding = true
                if !headless && !s.apps.contains(where: { $0.isConfigured || $0.status == "error" }) {
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
            guard frame.request_id == directoryRequestId && frame.query == directoryQuery && frame.scope == directoryScopeFilter && frame.harness == directoryHarnessFilter else { return }
            activityTotal = frame.activity_total ?? 0
            activityUnreadCount = frame.activity_unread_count ?? frame.unread_count ?? 0
            serverActivityFilter = frame.activity_filter
            serverActivityQuery = frame.activity_query ?? ""
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
            if visible, let previous = payload, previous.hub == value.hub && previous.target_title == value.target_title && previous.target_session_id == value.target_session_id && previous.target_harness == value.target_harness && previous.effective_wake == value.effective_wake {
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
        if menuBannerError != nil { menuBannerError = nil; rebuild() }
        var outgoing = action
        if let op = action["op"] as? String, ["channel_save", "save_hub_service"].contains(op),
           let id = action["channel_id"] as? String, let values = action["values"] as? [String: String] {
            let requestId = UUID().uuidString
            outgoing["request_id"] = requestId
            let hub = action["hub"] as? String ?? effectiveChannelHub
            let scope = (action["scope"] as? String) ?? (op == "save_hub_service" ? "hub" : "user")
            // Supersede only this scope's older intent, never another scope's save.
            pendingChannelValues = pendingChannelValues.filter { $0.value.hub != hub || $0.value.channelId != id || $0.value.scope != scope }
            pendingChannelValues[requestId] = (hub: hub, channelId: id, scope: scope, values: values)
        }
        if let op = action["op"] as? String,
           ["channel_save", "save_hub_service", "channel_pair", "channel_toggle", "channel_unpair", "channel_cancel_pair", "disconnect_human_channel"].contains(op),
           let id = action["channel_id"] as? String {
            let key = "\(action["hub"] as? String ?? effectiveChannelHub)|\(id)"
            if channelResults.removeValue(forKey: key) != nil, settingsPanel != nil { rebuildSettingsPanel() }
        }
        guard let data = try? JSONSerialization.data(withJSONObject: outgoing, options: [.sortedKeys]) else { return }
        FileHandle.standardOutput.write(data + Data([10]))
    }
    func rebuild() {
        MenuControl.clearHoverWhere { $0.enclosingMenuItem != nil || ($0.window !== self.directoryPanel && $0.window !== self.settingsPanel) }
        rebuildCount += 1
        let count = uniqueSessionCount
        let warning = hubs.contains { $0.status != "connected" || $0.error != nil }
        statusImage = makeCitoStatusIcon(warning ? .warning : count > 0 ? .active : .idle)
        if let button = status?.button ?? headlessStatusButton {
            formatStatusButton(button, count: count, unreadCount: unreadCount, image: statusImage)
        }
        let menu = cardMenu
        menu.removeAllItems()
        func host(_ view: NSView, id: String) -> NSMenuItem {
            view.identifier = NSUserInterfaceItemIdentifier(id)
            view.appearance = testAppearance
            let item = NSMenuItem(); item.view = view; menu.addItem(item); return item
        }
        let btnFont = NSFont.systemFont(ofSize: 11, weight: .medium)
        let header = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 40))
        header.addSubview(textLabel("Cito", NSRect(x: 12, y: 12, width: 36, height: 18), size: 13, weight: .bold))
        let subtitleText: String
        if count > 0 {
            subtitleText = "\(count) session\(count == 1 ? "" : "s") listening"
        } else {
            subtitleText = humanListening ? "Idle" : "Notifications paused"
        }
        let totalHubPillsWidth = hubs.reduce(CGFloat(0)) { $0 + min(180, measureTextWidth($1.name, font: btnFont) + 26) }
            + CGFloat(max(0, hubs.count - 1)) * 4
        let subtitleLimit: CGFloat = hubs.count > 1 ? min(246, 368 - 54 - min(144, totalHubPillsWidth)) : 246
        let subWidth = min(subtitleLimit, ceil(measureTextWidth(subtitleText, font: .systemFont(ofSize: 11.5))) + 8)
        let subLabel = textLabel(subtitleText, NSRect(x: 50, y: 12, width: subWidth, height: 17), size: 11.5, color: CitoPalette.secondaryText)
        subLabel.identifier = NSUserInterfaceItemIdentifier("header_subtitle")
        subLabel.lineBreakMode = .byTruncatingMiddle; subLabel.toolTip = subtitleText
        header.addSubview(subLabel)
        _ = host(header, id: "header")
        if let error = menuBannerError {
            let row = NSView(frame:NSRect(x:0,y:0,width:380,height:36))
            let label = textLabel(presentationError(error),NSRect(x:12,y:3,width:320,height:30),size:10,color:CitoPalette.dangerText)
            label.maximumNumberOfLines=2; label.lineBreakMode = .byTruncatingMiddle; label.toolTip=label.stringValue
            row.addSubview(label)
            let dismiss = MenuControl("", frame:NSRect(x:340,y:7,width:26,height:22), symbol:"xmark")
            dismiss.identifier = NSUserInterfaceItemIdentifier("dismiss_menu_error")
            dismiss.toolTip = "Dismiss error"
            dismiss.pressed = { [weak self] in self?.menuBannerError = nil; self?.rebuild() }
            row.addSubview(dismiss)
            _ = host(row,id:"menu_error_row")
        }
        var drawerItems: [String: NSMenuItem] = [:]
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
            if let error = hub.error { statusText = presentationError(error) }
            else if hub.status == "connected" { statusText = hub.auth_mode == "oidc" ? "Connected as \(hub.user ?? "user")" : "Connected · \(hub.user ?? "local")" }
            else if hub.status == "connecting" { statusText = "Connecting to hub…" }
            else if hub.status == "auth_required" { statusText = "Authentication required" }
            else { statusText = "Offline · Disconnected" }
            let detail = textLabel(statusText, NSRect(x: 12, y: 8, width: max(40, buttonsMinX - 20), height: 16),
                color: hub.error != nil || hub.status == "auth_required" ? .labelColor : CitoPalette.secondaryText)
            detail.lineBreakMode = .byTruncatingMiddle
            detail.toolTip = statusText; drawer.addSubview(detail)
            let item = host(drawer, id: "drawer:\(hub.name)")
            let autoExpand = hub.status != "connected" || hub.error != nil
            item.isHidden = selectedHubs.contains(hub.name) ? false : !autoExpand
            drawerItems[hub.name] = item
        }
        let stripX = max(138, 54 + subWidth)
        let stripWidth = max(44, 368 - stripX)
        let hubStrip = NSScrollView(frame: NSRect(x: stripX, y: 9, width: stripWidth, height: 22))
        hubStrip.scrollerStyle = .overlay
        hubStrip.identifier = NSUserInterfaceItemIdentifier("header_hub_strip")
        hubStrip.drawsBackground = false; hubStrip.hasHorizontalScroller = false
        let pillContainer = NSView(frame: NSRect(x: 0, y: 0, width: stripWidth, height: 22))
        hubStrip.documentView = pillContainer; header.addSubview(hubStrip)
        var pillX: CGFloat = 0
        for hub in hubs {
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
            let compactChips = hubs.count > 2 && totalHubPillsWidth > stripWidth
            let chipCap = stripWidth >= 92 ? floor((stripWidth-4)/2) : stripWidth
            let width: CGFloat = isCompactIcon ? 44 : (compactChips ? chipCap : min(180, measureTextWidth(title, font: btnFont) + 26))
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
                chev.contentTintColor = CitoPalette.secondaryText; pill.addSubview(chev); pill.accessorySymbolView = chev
                pill.toolTip = "Connected · \(hub.user ?? "local")"
            } else {
                pill.label.font = btnFont
                pill.label.lineBreakMode = .byTruncatingMiddle; pill.label.toolTip = title
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
            pillContainer.addSubview(pill)
            pillX += width + 4
        }
        pillContainer.frame.size.width = max(stripWidth,pillX-4)
        if let active = pillContainer.subviews.first(where: { view in
            let id = view.identifier?.rawValue ?? ""
            return selectedHubs.contains(String(id.dropFirst(4))) || hubs.contains { id == "hub:\($0.name)" && ($0.error != nil || $0.status != "connected") }
        }) { active.scrollToVisible(active.bounds) }
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
            let title = textLabel("You (@\(targetUser))", NSRect(x: 46, y: 24, width: 220, height: 16), size: 12.5, weight: .semibold)
            title.lineBreakMode = .byTruncatingMiddle; title.toolTip = title.stringValue
            card.addSubview(title)
            card.addSubview(textLabel("Paused · Click Enable to receive messages", NSRect(x: 46, y: 8, width: 220, height: 14), size: 10.5, color: CitoPalette.secondaryText))
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
            let user = [listener.user, listener.owner_user, hubs.first { $0.name == listener.hub }?.user]
                .compactMap { $0 }.first { !$0.isEmpty } ?? targetUser
            let displayTitle = nodeDisplayTitle(listener.title, harness: listener.harness, user: user)
            let hasDistinctAbout = distinctAbout(listener.about, title: listener.title, user: user) != nil
            let rowHeight: CGFloat = hasDistinctAbout ? 78 : 64
            let cardHeight: CGFloat = hasDistinctAbout ? 72 : 58
            let row = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: rowHeight))
            row.identifier = NSUserInterfaceItemIdentifier("session:\(listener.scoped_id)")
            let card = roundedView(NSRect(x: 10, y: 3, width: 360, height: cardHeight), radius: 10,
                color: .labelColor.withAppearanceAlpha(0.07), border: .labelColor.withAppearanceAlpha(0.11))
            row.addSubview(card)
            activeRows.append((id: "session:\(listener.scoped_id)", view: row))
            let multiHub = hubs.count > 1
            let badgeSize: CGFloat = multiHub ? 28 : 34
            let titleX: CGFloat = multiHub ? 42 : 54
            let badge = makeHarnessBadge(NSRect(x: 10, y: cardHeight - 44, width: badgeSize, height: badgeSize),
                radius: 8, iconInset: multiHub ? 7 : 8, harness: listener.harness, harnesses: map)
            card.addSubview(badge)
            let listenerHub = hubs.first(where: { $0.name == listener.hub })
            let showScopeToggle = !(listenerHub?.name == "local" && listenerHub?.auth_mode == "none")
            let hubPillWidth = min(88, max(44, ceil(measureTextWidth(listener.hub, font: btnFont)) + 20))
            let scopeWidth: CGFloat = 62
            let stopX: CGFloat = 328
            let scopeX = showScopeToggle ? stopX - 4 - scopeWidth : stopX
            let titleRight = multiHub ? scopeX - hubPillWidth - 8 : scopeX - 6
            let title = textLabel(displayTitle,
                NSRect(x: titleX, y: cardHeight - 26, width: titleRight - titleX, height: 18), size: 13, weight: .semibold)
            title.lineBreakMode = .byTruncatingMiddle
            title.toolTip = displayTitle; card.addSubview(title)
            if hasDistinctAbout {
                let aboutLabel = textLabel(aboutText, NSRect(x: titleX, y: 28, width: 350 - titleX, height: 16), size: 11.5, color: CitoPalette.secondaryText)
                aboutLabel.lineBreakMode = .byTruncatingMiddle
                aboutLabel.toolTip = aboutText; card.addSubview(aboutLabel)
            }
            let short = listener.display_address
            let copyFont = NSFont.monospacedSystemFont(ofSize: 11, weight: .medium)
            let copyWidth = max(measureTextWidth("✓ Copied", font: copyFont), measureTextWidth(short, font: copyFont)) + 28
            let copy = MenuControl(short, frame: NSRect(x: titleX, y: 7, width: copyWidth, height: 19), leadingSymbol: "doc.on.doc")
            copy.label.font = copyFont
            copy.toolTip = listener.scoped_id; copy.identifier = NSUserInterfaceItemIdentifier("copy:\(listener.scoped_id)")
            copy.pressed = { [weak self, weak copy] in
                self?.copyText(listener.display_address, control: copy)
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
                NSRect(x: titleX + copyWidth + 8, y: 8, width: max(0, 350 - titleX - copyWidth - 8), height: 16), size: 11, color: CitoPalette.secondaryText)
            path.lineBreakMode = .byTruncatingMiddle; path.toolTip = listener.cwd; card.addSubview(path)
            if hubs.count > 1 {
                let hubPill = MenuControl(listener.hub, frame: NSRect(x: scopeX - hubPillWidth - 4, y: cardHeight - 28, width: hubPillWidth, height: 22), radius: 6)
                hubPill.identifier = NSUserInterfaceItemIdentifier("switch_hub:\(listener.scoped_id)")
                hubPill.style = .neutral
                hubPill.label.font = btnFont
                hubPill.label.lineBreakMode = .byTruncatingMiddle; hubPill.label.toolTip = listener.hub
                hubPill.toolTip = listener.hub
                hubPill.label.alignment = .left
                hubPill.label.frame = NSRect(x: 13, y: 3.5, width: hubPillWidth - 13, height: 15)
                let hubStatus = hubs.first(where: { $0.name == listener.hub })?.status ?? "disconnected"
                hubPill.addSubview(roundedView(NSRect(x: 6, y: 8.5, width: 5, height: 5), radius: 2.5, color: statusColor(hubStatus)))
                let currentIndex = hubs.firstIndex(where: { $0.name == listener.hub }) ?? 0
                let ring = (1..<hubs.count).map { hubs[(currentIndex + $0) % hubs.count] }.filter { $0.name != listener.hub && $0.status != "disconnected" }
                let nextTarget = ring.first(where: { h in
                    h.status == "connected" && !listeners.contains(where: { $0.hub == h.name && $0.harness == listener.harness && $0.session_id == listener.session_id })
                }) ?? ring.first
                if let nextHub = nextTarget?.name {
                    hubPill.toolTip = "\(listener.hub) · Switch hub to \(nextHub)"
                    hubPill.pressed = { [weak self] in
                        guard let self = self else { return }
                        self.pendingMutation = true
                        self.emit(["op": "switch_hub", "from_hub": listener.hub, "to_hub": nextHub, "listener_id": listener.listener_id])
                    }
                }
                card.addSubview(hubPill)
            }
            if showScopeToggle {
                var scope = listener.scope
                let scopeBtn = MenuControl(scope == "self" ? "Self" : "Team", frame: NSRect(x: scopeX, y: cardHeight - 28, width: scopeWidth, height: 22),
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
                card.addSubview(scopeBtn)
            }
            let stop = MenuControl("Stop listening", frame: NSRect(x: stopX, y: cardHeight - 28, width: 22, height: 22), symbol: "xmark")
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
            scroll.scrollerStyle = .overlay
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
                icon.contentTintColor = CitoPalette.secondaryText; card.addSubview(icon)
                card.addSubview(textLabel("No active listeners", NSRect(x: 44, y: 57, width: 304, height: 18), size: 13, weight: .semibold))
                let subtitle = !(settings?.apps.contains(where:{$0.isConfigured}) ?? true) ? "Connect your coding apps in Settings to start" : "Call cito_listen in a session or open Human Channels"
                card.addSubview(textLabel(subtitle, NSRect(x:44,y:36,width:304,height:16),size:10.5,color:CitoPalette.secondaryText))
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
                icon.contentTintColor = CitoPalette.secondaryText; pill.addSubview(icon)
                pill.addSubview(textLabel("No active listeners — click + Listen below",
                    NSRect(x: 32, y: 6, width: 316, height: 16), size: 11, color: CitoPalette.secondaryText))
                row.addSubview(pill); _ = host(row, id: "empty")
            }
        }
        // Zone 2 — My Other Hosts
        if !myRemote.isEmpty {
            let remoteHeader = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 26))
            remoteHeader.addSubview(roundedView(NSRect(x: 12, y: 24, width: 356, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
            let toggleRemote = MenuControl("MY OTHER HOSTS · \(myRemote.count)", frame: NSRect(x: 8, y: 1, width: 364, height: 22),
                leadingSymbol: myRemoteCollapsed ? "chevron.right" : "chevron.down", color: CitoPalette.secondaryText, radius: 6)
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
                    hostIcon.contentTintColor = CitoPalette.secondaryText
                    row.addSubview(hostIcon)
                    let short = peer.display_address
                    let copyWidth = max(measureTextWidth("✓ Copied", font: copyFont), measureTextWidth(short, font: copyFont)) + 28
                    let copy = MenuControl(short, frame: NSRect(x: 368 - copyWidth, y: 7, width: copyWidth, height: 20), leadingSymbol: "doc.on.doc")
                    copy.label.font = copyFont
                    copy.toolTip = peer.scoped_id
                    copy.identifier = NSUserInterfaceItemIdentifier("copy_remote:\(peer.scoped_id)")
                    copy.pressed = { [weak self, weak copy] in
                        self?.copyText(peer.display_address, control: copy)
                    }
                    row.addSubview(copy)
                    let cleanHost = peer.host_name.replacingOccurrences(of: "\\.local$", with: "", options: [.regularExpression, .caseInsensitive])
                    let hostText = cleanHost.isEmpty ? peer.host_name : cleanHost
                    let branch = (peer.branch ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
                    let branchSuffix = (!branch.isEmpty && branch != "HEAD") ? " · \(branch)" : ""
                    let info = textLabel("\(hostText) · \(nodeDisplayTitle(peer.title,harness:peer.harness,user:peer.user))\(branchSuffix)",
                        NSRect(x: 36, y: 8, width: max(40, 368 - copyWidth - 44), height: 18), size: 12, weight: .regular, color: .labelColor)
                    info.lineBreakMode = .byTruncatingMiddle
                    info.toolTip = peer.scoped_id + (distinctAbout(peer.about, title: peer.title, user: peer.user).map { " — \($0)" } ?? "")
                    row.addSubview(info)
                    remoteRows.append((id: rowId, view: row))
                }
                if remoteRows.count <= 3 {
                    for (id, row) in remoteRows { _ = host(row, id: id) }
                } else {
                    let totalHeight = CGFloat(remoteRows.count) * 34
                    let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: 380, height: 102))
                    scroll.scrollerStyle = .overlay
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
                size: 10, weight: .semibold, color: CitoPalette.secondaryText))
            if hubs.count > 1 {
                let targetStrip = NSScrollView(frame: NSRect(x: 140, y: 2, width: 228, height: 18))
                targetStrip.scrollerStyle = .overlay
                targetStrip.identifier = NSUserInterfaceItemIdentifier("targetStrip")
                targetStrip.drawsBackground = false
                targetStrip.hasHorizontalScroller = false
                targetStrip.hasVerticalScroller = false
                let targetContainer = NSView(frame: NSRect(x: 0, y: 0, width: 228, height: 18))
                targetStrip.documentView = targetContainer
                availHeader.addSubview(targetStrip)
                let targetFont = NSFont.systemFont(ofSize: 10.5, weight: .medium)
                var targetX: CGFloat = 228
                for hub in hubs.reversed() where hub.status != "disconnected" {
                    let w = min(targetStrip.contentSize.width, max(38, ceil(measureTextWidth(hub.name, font: targetFont)) + 16))
                    targetX -= w
                    let pill = MenuControl(hub.name, frame: NSRect(x: targetX, y: 0, width: w, height: 18), radius: 9)
                    pill.identifier = NSUserInterfaceItemIdentifier("target_hub:\(hub.name)")
                    pill.label.font = targetFont
                    pill.label.lineBreakMode = .byTruncatingMiddle
                    pill.label.toolTip = hub.name
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
                if let active = targetContainer.subviews.first(where: { $0.identifier?.rawValue == "target_hub:\(targetHub)" }) { active.scrollToVisible(active.bounds) }
            }
            _ = host(availHeader, id: "available_header")
            let visibleAvailable = hubs.count > 1 && !targetHub.isEmpty
                ? available.filter { !($0.listening_hubs ?? []).contains(targetHub) }
                : available
            if visibleAvailable.isEmpty {
                let emptyTarget = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 28))
                let label = textLabel("All sessions listening on \(targetHub)",
                    NSRect(x: 24, y: 6, width: 332, height: 16), size: 11.5, weight: .regular, color: CitoPalette.secondaryText)
                label.lineBreakMode = .byTruncatingMiddle; label.toolTip = label.stringValue
                emptyTarget.addSubview(label)
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
                groupControl.toolTip = visual.tier == "experimental" ? "\(groupTitle) (Experimental)" : groupTitle
                groupControl.style = .subtleHeader
                groupControl.symbolName = isCollapsed ? "chevron.right" : "chevron.down"
                let chev = NSImageView(frame: NSRect(x: 6, y: 7, width: 10, height: 10))
                chev.image = NSImage(systemSymbolName: isCollapsed ? "chevron.right" : "chevron.down", accessibilityDescription: nil)
                chev.contentTintColor = CitoPalette.secondaryText
                groupControl.addSubview(chev)
                groupControl.accessorySymbolView = chev
                let badge = makeHarnessBadge(NSRect(x: 20, y: 3, width: 18, height: 18),
                    radius: 5, iconInset: 3, harness: harness, harnesses: map)
                groupControl.addSubview(badge)
                groupControl.label.frame = NSRect(x: 44, y: 4, width: 310, height: 16)
                groupControl.label.font = .systemFont(ofSize: 11.5, weight: .semibold)
                groupControl.label.alignment = .left
                groupControl.label.lineBreakMode = .byTruncatingMiddle; groupControl.label.toolTip = groupTitle
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
                        let needsReload = c.needs_reload == true
                        let row = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: needsReload ? 46 : 36))
                        let rowId = "candidate:\(c.harness):\(c.session_id)"
                        row.identifier = NSUserInterfaceItemIdentifier(rowId)
                        let shortCwd = c.cwd == home ? "~" : c.cwd.hasPrefix(home + "/") ? "~" + c.cwd.dropFirst(home.count) : c.cwd
                        let branch = (c.branch ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
                        let branchSuffix = (!branch.isEmpty && branch != "HEAD") ? " · \(branch)" : ""
                        let titleWidth = min(160,measureTextWidth(c.title,font:.systemFont(ofSize:12))+2)
                        let info = textLabel(c.title, NSRect(x:24,y:needsReload ? 24 : 9,width:titleWidth,height:18),size:12,color:.labelColor)
                        info.lineBreakMode = .byTruncatingTail
                        let location = textLabel(" · \(shortCwd)\(branchSuffix)",NSRect(x:24+titleWidth,y:needsReload ? 24 : 9,width:274-titleWidth,height:18),size:12,color:CitoPalette.secondaryText)
                        location.lineBreakMode = .byTruncatingMiddle
                        location.identifier = NSUserInterfaceItemIdentifier("candidate_cwd:\(c.harness):\(c.session_id)")
                        row.addSubview(location)
                        info.toolTip = "\(c.title) — \(c.cwd)"
                        info.identifier=NSUserInterfaceItemIdentifier("candidate_title:\(c.harness):\(c.session_id)")
                        row.addSubview(info)
                        if needsReload {
                            let hint=textLabel(c.reload_hint ?? "Run /reload in \(visual.name) to enable 1-click listen",NSRect(x:24,y:5,width:274,height:15),size:10,color:CitoPalette.warningText)
                            hint.identifier=NSUserInterfaceItemIdentifier("candidate_hint:\(c.harness):\(c.session_id)")
                            row.addSubview(hint)
                            let reload=MenuControl("/reload",frame:NSRect(x:298,y:12,width:70,height:22),leadingSymbol:"doc.on.doc")
                            reload.label.font = .systemFont(ofSize:10)
                            reload.identifier=NSUserInterfaceItemIdentifier("reload:\(c.harness):\(c.session_id)")
                            reload.pressed={ [weak self, weak reload] in
                                guard let self=self else{return}
                                self.copyText("/reload", control:reload)
                                self.emit(["op":"refresh_available"])
                            }
                            row.addSubview(reload); candidateRows.append((id:rowId,view:row)); continue
                        }
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
                        let totalHeight = candidateRows.reduce(CGFloat(0)) { $0 + $1.view.frame.height }
                        let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: 380, height: 144))
                        // Overlay scrollers preserve the authored row width even
                        // when macOS is configured to always show scrollbars.
                        scroll.scrollerStyle = .overlay
                        scroll.drawsBackground = false; scroll.hasVerticalScroller = true
                        scroll.hasHorizontalScroller = false; scroll.autohidesScrollers = true
                        let doc = FlippedView(frame: NSRect(x: 0, y: 0, width: 380, height: totalHeight))
                        var rowY: CGFloat = 0
                        for (_, row) in candidateRows {
                            row.frame.origin = NSPoint(x:0,y:rowY)
                            rowY += row.frame.height
                            doc.addSubview(row)
                        }
                        let scrollId = groupedCandidates.count == 1 ? "available_scroll" : "available_scroll:\(harness)"
                        scroll.documentView = doc; _ = host(scroll, id: scrollId)
                    }
                }
            }
        }
        // Zone 4 — Explicit Directory and Activity entry points.
        if !hubs.isEmpty {
            let dirRow = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: 34))
            dirRow.addSubview(roundedView(NSRect(x: 12, y: 32, width: 356, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
            for (mode, x, title, symbol, trailing, tip) in [
                ("directory", CGFloat(12), "Directory…", "magnifyingglass", "\(directoryOnlineCount) online", "Search Network Directory (⌘K)"),
                ("activity", CGFloat(194), "Activity…", unreadCount > 0 ? "bell.badge.fill" : "clock.arrow.circlepath",
                 unreadCount > 0 ? "\(unreadCount) unread" : "History", "Open Activity Stream")
            ] {
                let control = MenuControl(title, frame: NSRect(x: x, y: 2, width: 174, height: 28), leadingSymbol: symbol, radius: 7)
                control.identifier = NSUserInterfaceItemIdentifier("open_\(mode)")
                control.toolTip = tip
                control.label.font = .systemFont(ofSize: 11, weight: .medium)
                control.label.alignment = .left
                control.label.frame = NSRect(x: 22, y: 6, width: 76, height: 16)
                let badge = textLabel(trailing, NSRect(x: 100, y: 6, width: 68, height: 16), size: 10, color: CitoPalette.secondaryText)
                badge.alignment = .right; badge.lineBreakMode = .byTruncatingMiddle; badge.toolTip = trailing
                badge.identifier = NSUserInterfaceItemIdentifier(mode == "directory" ? "directory_online_badge" : "activity_unread_badge")
                control.addSubview(badge)
                control.pressed = { [weak self, weak menu] in
                    guard let self = self else { return }
                    menu?.cancelTracking()
                    self.openDirectoryPanel(mode: mode)
                }
                dirRow.addSubview(control)
            }
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
        let directoryShortcut = NSMenuItem(title:"Directory…",action:#selector(openDirectoryShortcut),keyEquivalent:"k")
        directoryShortcut.target = self; directoryShortcut.keyEquivalentModifierMask = [.command]
        directoryShortcut.isHidden = true; directoryShortcut.allowsKeyEquivalentWhenHidden = true
        menu.addItem(directoryShortcut)
        menu.update()
    }
    @objc func openDirectoryShortcut() { openDirectoryPanel(mode:"directory") }
    @objc func openSettingsShortcut() { openSettingsPanel() }
    @objc func quitMenu() { emit(["op": "quit"]) }

    // MARK: - Surface 3: Native AppKit Settings Panel (520 × 460 pt KeyPanel: NSPanel)

    func ensureSettingsPanel() {
        if settingsPanel == nil {
            let window = KeyPanel(contentRect: NSRect(x: 0, y: 0, width: 520, height: 460),
                                  styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
            window.appearance = testAppearance
            window.level = .floating
            window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
            window.isFloatingPanel = true
            window.hidesOnDeactivate = false
            window.isOpaque = false
            window.backgroundColor = .clear
            window.hasShadow = true
            window.isMovableByWindowBackground = true
            settingsPanel = window
        }
        rebuildSettingsPanel()
    }

    func handleSettingsKey(_ key: String) {
        if (settingsPanel?.firstResponder as? NSTextInputClient)?.hasMarkedText() == true { return }
        switch key.lowercased() {
        case "command-comma": openSettingsPanel()
        case "command-w": closeSettingsPanel(emitAction:true)
        case "escape":
            if uninstallConfirmOpen { uninstallConfirmOpen = false; uninstallPurgeData = false; rebuildSettingsPanel() }
            else if addHubFormOpen { addHubFormOpen = false; pendingHubSave = nil; rebuildSettingsPanel() }
            else if !editingHubGateway.isEmpty {
                for (hub, channels) in channelSettingsByHub {
                    for ch in channels where editingHubGateway.contains("\(hub)|\(ch.id)") { clearChannelDrafts(ch, scope:"hub", hub:hub) }
                }
                editingHubGateway.removeAll(); rebuildSettingsPanel()
            }
            else { closeSettingsPanel(emitAction:true) }
        default: break
        }
    }
    func syncAddHubFieldsFromControls() {
        if editingOriginalHub == "local" { addHubName = "local"; addHubUrl = "local://" }
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
        let systemText = appPasteboard.string(forType:.string) ?? ""
        let raw = (systemText.isEmpty && headless ? clipboardString : systemText).trimmingCharacters(in:.whitespacesAndNewlines)
        let isURL = (raw.hasPrefix("http://") || raw.hasPrefix("https://")) && URL(string:raw)?.host != nil
        let isCLI = raw.hasPrefix("cito setup ") && raw.contains("--hub") && raw.contains("--url")
        let invite = raw.data(using:.utf8).flatMap { try? JSONSerialization.jsonObject(with:$0) } as? [String:Any]
        let inviteURL = (invite?["url"] as? String).flatMap { URL(string:$0) }
        let isJSON = inviteURL?.host != nil && ["http", "https"].contains(inviteURL?.scheme ?? "")
        guard isURL || isCLI || isJSON else {
            settingsBannerError = "Clipboard does not contain a valid Hub invite or http(s):// URL."
            rebuildSettingsPanel(); return
        }
        // One authoritative parser in the daemon handles every invite format.
        // Clipboard text is data, never a shell command to execute.
        settingsBannerError=nil; hubInvitePending=true; hubInvitePastedUntil = .distantPast
        emit(["op":"paste_hub_invite","invite":raw])
        rebuildSettingsPanel()
    }

    func mountSettingsCards(_ content: NSView, in root: NSView) {
        let id = content.identifier?.rawValue ?? ""
        let minY = content.subviews.map { $0.frame.minY }.min() ?? 8
        let extra = max(0, 8 - minY)
        for child in content.subviews { child.frame.origin.y += extra }
        let viewport = content.frame
        content.frame = NSRect(x: 0, y: 0, width: viewport.width, height: viewport.height + extra)
        let scroll = NSScrollView(frame: NSRect(x: 0, y: 44, width: 520, height: 368 - (settingsBannerError == nil ? 0 : 36)))
        scroll.scrollerStyle = .overlay
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
        let focused = settingsFieldControls.first { $0.value.currentEditor() != nil && $0.value.currentEditor() === window.firstResponder }
        let focusedId = focused?.key
        let selectedRange = (focused?.value.currentEditor() as? NSTextView)?.selectedRange()
        syncAddHubFieldsFromControls()
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

        let closeBtn = MenuControl("Close", frame: NSRect(x: 486, y: 12, width: 24, height: 24), symbol: "xmark")
        closeBtn.identifier = NSUserInterfaceItemIdentifier("close_settings")
        closeBtn.toolTip = "Close Settings"
        closeBtn.pressed = { [weak self] in self?.closeSettingsPanel(emitAction: true) }
        header.addSubview(closeBtn)
        header.addSubview(roundedView(NSRect(x: 0, y: 0, width: 520, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
        root.addSubview(header)
        if let error = settingsBannerError {
            let banner=roundedView(NSRect(x:20,y:376,width:480,height:32),radius:6,color:CitoPalette.dangerText.withAppearanceAlpha(0.10))
            banner.identifier=NSUserInterfaceItemIdentifier("settings_error_banner")
            let label=textLabel(presentationError(error),NSRect(x:10,y:3,width:432,height:26),size:10,color:CitoPalette.dangerText)
            label.maximumNumberOfLines=2; label.lineBreakMode = .byTruncatingMiddle; label.toolTip=label.stringValue
            label.identifier=NSUserInterfaceItemIdentifier("settings_error_label"); banner.addSubview(label)
            let dismiss=MenuControl("Dismiss",frame:NSRect(x:450,y:6,width:22,height:20),symbol:"xmark")
            dismiss.identifier=NSUserInterfaceItemIdentifier("dismiss_settings_error")
            dismiss.pressed={ [weak self] in self?.settingsBannerError=nil; self?.rebuildSettingsPanel() }
            banner.addSubview(dismiss); root.addSubview(banner)
        }

        // Tab 1: Agent Apps (520 × 368 pt, y: 44)
        let appsView = NSView(frame: NSRect(x: 0, y: 44, width: 520, height: 368))
        appsView.identifier = NSUserInterfaceItemIdentifier("settings_apps_tab")
        appsView.isHidden = settingsActiveTab != "apps"

        let resolvedApps: [AppSetupItem] = settings?.apps ?? harnesses.map { h in
            AppSetupItem(id: h.id, display_name: h.display_name, tier: h.tier, installed: true,
                configured: false, status: "not_configured", detail: "Click Set Up to connect", paths: [])
        }
        appsView.addSubview(textLabel("CONNECTED AGENT APPS", NSRect(x:20,y:338,width:240,height:14),size:10,weight:.semibold,color:CitoPalette.secondaryText))
        let appsHint = resolvedApps.contains(where: { $0.isConfigured || $0.status == "error" }) ? "Enable 1-click notification & wake support in your coding apps." : "Step 1: Click Set Up on your coding app below, then click + Listen in the Cito menu bar."
        appsView.addSubview(textLabel(appsHint, NSRect(x:20,y:320,width:480,height:15),size:10,color:CitoPalette.secondaryText))
        let map = harnessMap
        for (idx, app) in resolvedApps.enumerated() {
            let cardY = 226 - CGFloat(idx) * 90
            let card = roundedView(NSRect(x: 20, y: cardY, width: 480, height: 82), radius: 10,
                color: .labelColor.withAppearanceAlpha(0.06), border: .labelColor.withAppearanceAlpha(0.11))
            card.identifier = NSUserInterfaceItemIdentifier("settings_app:\(app.id)")
            card.addSubview(makeHarnessBadge(NSRect(x: 12, y: 43, width: 28, height: 28), radius: 7, iconInset: 6, harness: app.id, harnesses: map))

            let titleFont = NSFont.systemFont(ofSize: 13, weight: .semibold)
            let titleW = min(app.tier == "experimental" ? (app.isConfigured ? 84 : 148) : (app.isConfigured ? 174 : 238), ceil(measureTextWidth(app.display_name, font: titleFont)) + 6)
            let title = textLabel(app.display_name, NSRect(x: 50, y: 55, width: titleW, height: 17), size: 13, weight: .semibold)
            title.lineBreakMode = .byTruncatingMiddle; title.toolTip = app.display_name
            card.addSubview(title)
            if app.tier == "experimental" {
                let expPill = roundedView(NSRect(x: 54 + titleW, y: 56, width: 88, height: 16), radius: 8, color: CitoPalette.warningSurface, border: CitoPalette.warningBorder)
                expPill.identifier = NSUserInterfaceItemIdentifier("settings_app_tier:\(app.id)")
                let expLabel = textLabel("Experimental", NSRect(x: 6, y: 1, width: 76, height: 14), size: 9.5, weight: .semibold, color: CitoPalette.warningText)
                expLabel.alignment = .center
                expPill.addSubview(expLabel)
                card.addSubview(expPill)
            }
            let detail = textLabel(presentationError(app.detail), NSRect(x:50,y:8,width:418,height:30),size:11,color:CitoPalette.secondaryText)
            detail.maximumNumberOfLines=2; detail.lineBreakMode = .byTruncatingMiddle
            detail.toolTip=([presentationError(app.detail)]+app.paths).joined(separator:"\n")
            card.addSubview(detail)

            let statusText = app.isConfigured ? "● Connected" : (app.status == "error" ? "Inspection failed" : (app.status == "repair_needed" ? "⚠ Repair Needed" : "○ Not Set Up"))
            let statusFrame = app.isConfigured ? NSRect(x: 230, y: 49, width: 82, height: 16) : NSRect(x: 294, y: 49, width: 100, height: 16)
            let statusLabel = textLabel(statusText, statusFrame,
                size: 11, weight: .medium, color: app.isConfigured ? CitoPalette.successText : (app.status == "repair_needed" ? CitoPalette.warningText : CitoPalette.secondaryText))
            statusLabel.alignment = .right
            statusLabel.identifier = NSUserInterfaceItemIdentifier("settings_app_status:\(app.id)")
            card.addSubview(statusLabel)

            let btnTitle = app.isConfigured || app.status == "error" ? "Re-check" : (app.status == "repair_needed" ? "Repair" : "Set Up")
            let btnFrame = app.isConfigured ? NSRect(x: 318, y: 45, width: 68, height: 24) : NSRect(x: 402, y: 45, width: 66, height: 24)
            let btn = MenuControl(btnTitle, frame: btnFrame, radius: 6)
            btn.identifier = NSUserInterfaceItemIdentifier("setup_app:\(app.id)")
            btn.style = app.isConfigured ? .neutral : .prominent
            let appId = app.id
            btn.pressed = { [weak self] in
                self?.emit(["op": "setup_apply", "harness": appId])
            }
            card.addSubview(btn)

            if app.isConfigured {
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
        bannerCard.addSubview(textLabel("Show floating HUD wake banners on this Mac when signals arrive.", NSRect(x: 14, y: 7, width: 366, height: 15), size: 10.5, color: CitoPalette.secondaryText))
        let toggleBanner = MenuControl(bannerEnabled ? "● Enabled" : "Off", frame: NSRect(x: 386, y: 12, width: 80, height: 24), radius: 12)
        toggleBanner.identifier = NSUserInterfaceItemIdentifier("toggle_desktop_banner")
        toggleBanner.style = bannerEnabled ? .prominent : .neutral
        toggleBanner.pressed = { [weak self] in
            self?.emit(["op": "set_desktop_banner", "enabled": !bannerEnabled])
        }
        bannerCard.addSubview(toggleBanner)
        channelsView.addSubview(bannerCard)

        let channelHubs = settings?.hubs.map { $0.name } ?? hubs.map { $0.name }
        if channelHubs.count > 1 { channelsView.addSubview(textLabel("TARGET HUB:", NSRect(x: 20, y: 342, width: 78, height: 14), size: 10, weight: .semibold, color: CitoPalette.secondaryText)) }
        else { bannerCard.frame.origin.y += 36 }
        let selector = NSScrollView(frame: NSRect(x: 102, y: 336, width: 398, height: 28))
        selector.scrollerStyle = .overlay
        selector.identifier = NSUserInterfaceItemIdentifier("selector")
        selector.drawsBackground = false
        selector.hasHorizontalScroller = false
        let selectorContent = NSView(frame: NSRect(x: 0, y: 0, width: 480, height: 28))
        var selectorX: CGFloat = 0
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
        selectorContent.setFrameSize(NSSize(width: max(398, selectorX), height: 28))
        selector.documentView = selectorContent
        if channelHubs.count > 1 {
            channelsView.addSubview(selector)
            if let active = selectorContent.subviews.first(where: { $0.identifier?.rawValue == "channel_hub_selector:\(effectiveChannelHub)" }) { active.scrollToVisible(active.bounds) }
        }

        let resolvedChannels = channelSettingsByHub[effectiveChannelHub] ?? []
        var chCursorY: CGFloat = channelHubs.count > 1 ? 278 : 314
        for ch in resolvedChannels {
            let visibleFields = ch.fields.filter { $0.hidden != true }
            let instruction = channelInstructions["\(effectiveChannelHub)|\(ch.id)"] ?? ch.pairing_instruction ?? ""
            let instructionHeight = instruction.isEmpty ? CGFloat(0) : max(32, ceil((instruction as NSString).boundingRect(with: NSSize(width: 248, height: CGFloat.greatestFiniteMagnitude), options: [.usesLineFragmentOrigin], attributes: [.font: NSFont.monospacedSystemFont(ofSize: 11.5, weight: .semibold)]).height) + 4)
            let guide=(ch.setup_guide ?? []).joined(separator:"\n")
            let hubFields = visibleFields.filter { $0.scope == "hub" }
            let userFields = visibleFields.filter { $0.scope != "hub" }
            let hasHubScopeFields = ch.fields.contains { $0.scope == "hub" }
            let hubFieldsH = hubFields.reduce(CGFloat(0)) { $0 + ($1.help_text?.isEmpty == false ? 38 : 26) }
            let userFieldsH = userFields.reduce(CGFloat(0)) { $0 + ($1.help_text?.isEmpty == false ? 38 : 26) }
            let channelKey = "\(effectiveChannelHub)|\(ch.id)"
            let gatewayActive = !hasHubScopeFields || ch.hub_provided || ch.status != "not_configured" || (hasHubScopeFields && ch.fields.filter { $0.scope == "hub" }.allSatisfy { $0.configured })
            let hubGatewayExpanded = !gatewayActive || editingHubGateway.contains(channelKey)
            let guideH: CGFloat = !gatewayActive && !guide.isEmpty && !hubFields.isEmpty ? max(30,ceil((guide as NSString).boundingRect(with:NSSize(width:432,height:CGFloat.greatestFiniteMagnitude),options:[.usesLineFragmentOrigin],attributes:[.font:NSFont.systemFont(ofSize:10)]).height)+2) : 0
            let currentUser=settings?.hubs.first(where:{$0.name==effectiveChannelHub})?.user ?? hubs.first(where:{$0.name==effectiveChannelHub})?.user ?? "local"
            let gatewayRole = !hubFields.isEmpty ? (guideH > 0 ? "" : "Configured once by the Hub owner. Shared by all human nodes on '\(effectiveChannelHub)'.") :
                (hasHubScopeFields ? (ch.hub_provided ? "Provided by '\(effectiveChannelHub)' Hub administrator — no gateway setup needed on your Mac." : "Not yet configured by the '\(effectiveChannelHub)' Hub administrator.") : "No shared gateway credentials required for this channel.")
            let personalRole = gatewayActive ? "Bind your personal \(ch.display_name) chat to receive wakes and reply on '\(effectiveChannelHub)'." : "Complete Step 1 (Hub Gateway Token) above to enable personal pairing on '\(effectiveChannelHub)'."
            func roleHeight(_ text: String) -> CGFloat {
                text.isEmpty ? 0 : max(13,ceil((text as NSString).boundingRect(with:NSSize(width:432,height:CGFloat.greatestFiniteMagnitude),options:[.usesLineFragmentOrigin],attributes:[.font:NSFont.systemFont(ofSize:10)]).height))
            }
            let gatewayRoleH=roleHeight(gatewayRole), personalRoleH=roleHeight(personalRole)
            let hubStratumH: CGFloat = hubGatewayExpanded ? 36 + gatewayRoleH + guideH + hubFieldsH + (!hubFields.isEmpty ? 32 : 0) : 36
            let userStratumH: CGFloat = gatewayActive ? 36 + personalRoleH + userFieldsH + (instructionHeight > 0 ? instructionHeight + 8 : 0) + 34 : 40
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

            let chTitle = textLabel(ch.display_name, NSRect(x: 44, y: cardHeight - 21, width: 180, height: 16), size: 12.5, weight: .semibold)
            chTitle.lineBreakMode = .byTruncatingMiddle; chTitle.toolTip = ch.display_name
            chCard.addSubview(chTitle)
            let channelResult = channelResults["\(effectiveChannelHub)|\(ch.id)"]
            let chStatusText:String
            let chStatusColor:NSColor
            switch channelResult?.ok == false ? "error" : ch.status {
            case "ready": chStatusText = ch.display_label.flatMap { $0.isEmpty ? nil : "● Connected (\($0))" } ?? "● Connected"; chStatusColor = CitoPalette.successText
            case "paused": chStatusText = "⏸ Paused"; chStatusColor = CitoPalette.warningText
            case "pairing_pending": chStatusText = "◌ Pairing…"; chStatusColor = CitoPalette.infoText
            case "error": chStatusText = "⚠ Error"; chStatusColor = CitoPalette.dangerText
            default: chStatusText = ch.hub_provided || ch.status == "hub_ready" ? "Hub Service: ● Ready" : "○ Not Configured"; chStatusColor = CitoPalette.secondaryText
            }
            let chStatusLabel = textLabel(chStatusText, NSRect(x: 240, y: cardHeight - 20, width: 226, height: 15), size: 11, weight: .medium, color: chStatusColor)
            chStatusLabel.alignment = .right
            chStatusLabel.lineBreakMode = .byTruncatingMiddle; chStatusLabel.toolTip = chStatusText
            chStatusLabel.identifier = NSUserInterfaceItemIdentifier("settings_channel_status:\(ch.id)")
            chCard.addSubview(chStatusLabel)

            let chDetail = textLabel(presentationError(channelResult?.detail ?? ch.detail), NSRect(x: 44, y: cardHeight - 36, width: 422, height: 14), size: 10.5, color: CitoPalette.secondaryText)
            chDetail.lineBreakMode = .byTruncatingMiddle; chDetail.toolTip = chDetail.stringValue
            chDetail.identifier = NSUserInterfaceItemIdentifier("settings_channel_detail:\(ch.id)")
            chCard.addSubview(chDetail)

            let hubStratum = roundedView(NSRect(x:12,y:6+userStratumH+6,width:456,height:hubStratumH),radius:8,
                color:.controlAccentColor.withAppearanceAlpha(0.06),border:.controlAccentColor.withAppearanceAlpha(0.14))
            hubStratum.identifier=NSUserInterfaceItemIdentifier("stratum_hub:\(ch.id)")
            let hubTitle=textLabel("1. HUB GATEWAY · ONCE PER HUB",NSRect(x:10,y:hubStratumH-25,width:168,height:14),size:9,weight:.bold,color:CitoPalette.secondaryText)
            hubTitle.identifier=NSUserInterfaceItemIdentifier("stratum_hub_title:\(ch.id)")
            if !gatewayRole.isEmpty { hubTitle.toolTip = gatewayRole }
            hubStratum.addSubview(hubTitle)
            let gatewayStatus=textLabel(gatewayActive ? "● Hub Gateway Active" : "○ Gateway Token Required",
                gatewayActive ? NSRect(x:182,y:hubStratumH-25,width:106,height:14) : NSRect(x:280,y:hubStratumH-25,width:164,height:14),size:9,weight:.medium,color:gatewayActive ? CitoPalette.successText : CitoPalette.secondaryText)
            gatewayStatus.alignment = gatewayActive ? .left : .right
            gatewayStatus.identifier=NSUserInterfaceItemIdentifier("stratum_hub_status:\(ch.id)")
            hubStratum.addSubview(gatewayStatus)
            if gatewayActive && !hubFields.isEmpty {
                let clear=MenuControl("Clear Token",frame:hubGatewayExpanded ? NSRect(x:12,y:6,width:84,height:24) : NSRect(x:378,y:hubStratumH-28,width:70,height:20),radius:6)
                clear.label.font = .systemFont(ofSize:9.5)
                clear.style = .destructiveHover
                clear.identifier=NSUserInterfaceItemIdentifier("clear_channel_service:\(ch.id)")
                clear.pressed={ [weak self] in guard let self=self else{return}; self.clearChannelDrafts(ch); self.editingHubGateway.remove(channelKey); self.emit(["op":"disconnect_human_channel","hub":self.effectiveChannelHub,"channel_id":ch.id,"reset_hub":true,"reset_user":true]) }
                hubStratum.addSubview(clear)
            }
            if hubGatewayExpanded && gatewayRoleH > 0 {
                let gatewayDescription=textLabel(gatewayRole,NSRect(x:12,y:hubStratumH-30-gatewayRoleH,width:432,height:gatewayRoleH),size:10,color:CitoPalette.secondaryText)
                gatewayDescription.maximumNumberOfLines=0; gatewayDescription.lineBreakMode = .byWordWrapping
                hubStratum.addSubview(gatewayDescription)
            }
            if hubGatewayExpanded && guideH>0 {
                let box=textLabel(guide,NSRect(x:12,y:hubStratumH-30-gatewayRoleH-guideH,width:432,height:guideH),size:10,color:CitoPalette.secondaryText)
                box.identifier=NSUserInterfaceItemIdentifier("channel_setup_guide:\(ch.id)")
                box.maximumNumberOfLines=0; box.lineBreakMode = .byWordWrapping
                hubStratum.addSubview(box)
            }
            let userStratum=roundedView(NSRect(x:12,y:6,width:456,height:userStratumH),radius:8,
                color:.labelColor.withAppearanceAlpha(0.04),border:.labelColor.withAppearanceAlpha(0.10))
            userStratum.identifier=NSUserInterfaceItemIdentifier("stratum_user:\(ch.id)")
            let userTitle=textLabel(gatewayActive ? "2. Your Personal Account (@\(currentUser))" : "2. Your Personal Account (@\(currentUser)) — Complete Step 1 above to enable personal pairing.",NSRect(x:12,y:userStratumH-19,width:432,height:13),size:9.5,weight:.bold,color:CitoPalette.secondaryText)
            userTitle.lineBreakMode = .byTruncatingMiddle; userTitle.toolTip = userTitle.stringValue
            userTitle.identifier=NSUserInterfaceItemIdentifier("stratum_user_title:\(ch.id)")
            userStratum.addSubview(userTitle)
            let personalDescription=textLabel(personalRole,NSRect(x:12,y:userStratumH-26-personalRoleH,width:432,height:personalRoleH),size:10,color:CitoPalette.secondaryText)
            personalDescription.maximumNumberOfLines=0; personalDescription.lineBreakMode = .byWordWrapping
            userStratum.addSubview(personalDescription)
            for (container, fields, top) in [(hubStratum,hubGatewayExpanded ? hubFields : [],hubStratumH-34-gatewayRoleH-guideH),(userStratum,gatewayActive ? userFields : [],userStratumH-30-personalRoleH)] {
                var fieldY=top-22
                for f in fields {
                    let fieldId="channel_field:\(ch.id):\(f.key)"
                    let labelView=textLabel(f.label,NSRect(x:12,y:fieldY+3,width:140,height:15),size:11,weight:.medium)
                    labelView.toolTip=f.label; container.addSubview(labelView)
                    let initialVal=channelFieldDrafts[draftKey(fieldId)] ?? (f.value ?? "")
                    let inputField=makeEditableField(fieldId,frame:NSRect(x:158,y:fieldY,width:286,height:22),placeholder:f.type=="secret" && f.configured ? (f.configured_placeholder ?? "•••••••• (Saved — enter new value to replace)") : f.placeholder,value:initialVal,secret:f.type=="secret")
                    if container === userStratum && !gatewayActive { inputField.isEnabled=false; inputField.alphaValue=0.55 }
                    inputField.toolTip=f.help_text; container.addSubview(inputField)
                    if let help=f.help_text, !help.isEmpty {
                        container.addSubview(textLabel(help,NSRect(x:158,y:fieldY-13,width:286,height:12),size:9.5,color:CitoPalette.secondaryText))
                        fieldY-=12
                    }
                    fieldY-=26
                }
            }
            chCard.addSubview(hubStratum); chCard.addSubview(userStratum)
            func gate(_ control: MenuControl) { control.isEnabled=gatewayActive; control.alphaValue=gatewayActive ? 1 : 0.55 }
            if !instruction.isEmpty {
                let instrLabel = textLabel(instruction, NSRect(x: 12, y: 40, width: 248, height: instructionHeight), size: 11.5, weight: .semibold, color: CitoPalette.infoText)
                instrLabel.maximumNumberOfLines = 0
                instrLabel.lineBreakMode = .byWordWrapping
                instrLabel.isSelectable = true
                instrLabel.toolTip = instruction
                instrLabel.font = .monospacedSystemFont(ofSize: 11.5, weight: .semibold)
                instrLabel.identifier = NSUserInterfaceItemIdentifier("pairing_instruction:\(ch.id)")
                userStratum.addSubview(instrLabel)
            }

            let chId = ch.id
            let collectValues: (String) -> [String: String] = { [weak self] scopeFilter in
                guard let self = self else { return [:] }
                self.syncAddHubFieldsFromControls()
                var vals: [String: String] = [:]
                for f in ch.fields where f.hidden != true && f.scope == scopeFilter {
                    let fid = "channel_field:\(chId):\(f.key)"
                    let draft=self.channelFieldDrafts[self.draftKey(fid)]
                    if f.type == "secret" && f.configured {
                        if let d=draft, !d.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty, d != (f.value ?? "") { vals[f.key]=d }
                    } else {
                        vals[f.key]=draft ?? self.settingsFieldControls[fid]?.stringValue ?? f.value ?? ""
                    }
                }
                return vals
            }

            if !hubFields.isEmpty {
                if hubGatewayExpanded {
                    let save = MenuControl("Save Gateway Token",frame:NSRect(x:304,y:6,width:140,height:24))
                    save.identifier=NSUserInterfaceItemIdentifier("save_hub_service:\(ch.id)"); save.style = .prominent
                    save.pressed={ [weak self] in guard let self=self else{return}; self.emit(["op":"save_hub_service","hub":self.effectiveChannelHub,"channel_id":ch.id,"scope":"hub","values":collectValues("hub")]) }
                    hubStratum.addSubview(save)
                    if gatewayActive {
                        let cancel=MenuControl("Cancel",frame:NSRect(x:232,y:6,width:66,height:24))
                        cancel.identifier=NSUserInterfaceItemIdentifier("cancel_edit_hub_service:\(ch.id)")
                        cancel.pressed={ [weak self] in self?.clearChannelDrafts(ch, scope:"hub"); self?.editingHubGateway.remove(channelKey); self?.rebuildSettingsPanel() }
                        hubStratum.addSubview(cancel)
                    }
                } else {
                    let edit=MenuControl("Change Token…",frame:NSRect(x:292,y:hubStratumH-28,width:82,height:20))
                    edit.label.font = .systemFont(ofSize:9)
                    edit.label.frame = NSRect(x:0,y:2.5,width:82,height:15)
                    edit.identifier=NSUserInterfaceItemIdentifier("edit_hub_service:\(ch.id)")
                    edit.pressed={ [weak self] in self?.editingHubGateway.insert(channelKey); self?.rebuildSettingsPanel() }
                    hubStratum.addSubview(edit)
                }
            }
            if ch.pairing_supported {
                let pairLabel = ch.configured ? "Re-pair…" : (ch.pairing_action_label ?? "Pair Account…")
                let pairBtn = MenuControl(pairLabel, frame: NSRect(x: 12, y: 6, width: 112, height: 24), leadingSymbol: "link", radius: 6)
                pairBtn.toolTip=ch.pairing_help_text
                pairBtn.identifier = NSUserInterfaceItemIdentifier("pair_channel:\(ch.id)")
                pairBtn.pressed = { [weak self] in
                    guard let self = self else { return }
                    self.emit(["op": "channel_pair", "hub": self.effectiveChannelHub, "channel_id": chId, "values": collectValues("user")])
                }
                gate(pairBtn); userStratum.addSubview(pairBtn)
            }

            if !instruction.isEmpty {
                let copy=MenuControl("Copy Command",frame:NSRect(x:266,y:42,width:114,height:22),leadingSymbol:"doc.on.doc",radius:6)
                copy.label.font = .systemFont(ofSize:10.5)
                copy.identifier=NSUserInterfaceItemIdentifier("copy_pair_command:\(ch.id)")
                copy.pressed={ [weak self, weak copy] in
                    self?.copyText(ch.pairing_command ?? ch.pairing_code ?? instruction, control: copy)
                }
                userStratum.addSubview(copy)
                let cancel=MenuControl("Cancel",frame:NSRect(x:386,y:42,width:58,height:22),radius:6)
                cancel.identifier=NSUserInterfaceItemIdentifier("cancel_pair_channel:\(ch.id)")
                cancel.pressed={ [weak self] in
                    guard let self=self else{return}
                    self.channelInstructions.removeValue(forKey:self.effectiveChannelHub+"|"+chId)
                    self.emit(["op":"channel_cancel_pair","hub":self.effectiveChannelHub,"channel_id":chId])
                }
                userStratum.addSubview(cancel)
            }
            if ch.configured {
                let toggle=MenuControl(ch.enabled ? "Pause" : "Resume",frame:NSRect(x:130,y:6,width:60,height:24),radius:6)
                toggle.identifier=NSUserInterfaceItemIdentifier("toggle_channel:\(ch.id)")
                toggle.pressed={ [weak self] in guard let self=self else{return}; self.emit(["op":"channel_toggle","hub":self.effectiveChannelHub,"channel_id":chId,"enabled":!ch.enabled]) }
                userStratum.addSubview(toggle)
                let unpair=MenuControl("Unpair",frame:NSRect(x:196,y:6,width:60,height:24),radius:6)
                unpair.style = .destructiveHover
                unpair.identifier=NSUserInterfaceItemIdentifier("unpair_channel:\(ch.id)")
                unpair.pressed={ [weak self] in guard let self=self else{return}; self.clearChannelDrafts(ch, scope:"user"); self.emit(["op":"channel_unpair","hub":self.effectiveChannelHub,"channel_id":chId,"reset_user":true]) }
                userStratum.addSubview(unpair)
            } else {
                let skip=MenuControl("Skip for Now",frame:NSRect(x:130,y:6,width:96,height:24),radius:6)
                skip.identifier=NSUserInterfaceItemIdentifier("skip_channel:\(ch.id)")
                skip.pressed={ [weak self] in self?.settingsActiveTab="hubs"; self?.rebuildSettingsPanel() }
                userStratum.addSubview(skip)
            }
            let testBtn = MenuControl("Send Test", frame: NSRect(x: 290, y: 6, width: 80, height: 24), radius: 6)
            testBtn.identifier = NSUserInterfaceItemIdentifier("test_channel:\(ch.id)")
            testBtn.pressed = { [weak self] in
                guard let self = self else { return }
                self.emit(["op": "channel_test", "hub": self.effectiveChannelHub, "channel_id": chId, "values": collectValues("user")])
            }
            gate(testBtn); userStratum.addSubview(testBtn)

            let saveBtn = MenuControl("Save", frame: NSRect(x: 376, y: 6, width: 68, height: 24), radius: 6)
            saveBtn.identifier = NSUserInterfaceItemIdentifier("save_channel:\(ch.id)")
            saveBtn.style = .prominent
            saveBtn.pressed = { [weak self] in
                guard let self = self else { return }
                let values = collectValues("user")
                self.emit(["op": "channel_save", "hub": self.effectiveChannelHub, "channel_id": chId, "enabled": true, "values": values])
            }
            gate(saveBtn); userStratum.addSubview(saveBtn)

            if !gatewayActive {
                for child in userStratum.subviews where child !== userTitle { child.removeFromSuperview() }
                let textH = max(14, ceil((userTitle.stringValue as NSString).boundingRect(with:NSSize(width:432,height:40),options:[.usesLineFragmentOrigin],attributes:[.font:userTitle.font ?? .boldSystemFont(ofSize:9.5)]).height))
                userTitle.frame = NSRect(x:12,y:floor((40-textH)/2),width:432,height:textH)
                userTitle.maximumNumberOfLines = 2; userTitle.lineBreakMode = .byWordWrapping
            }
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
        let hostExpanded = hostHub.enabled || hostHubPortPending || hostHubPortDraft != nil || hostHubPortError != nil || hostHub.error != nil
        let hostCard = roundedView(NSRect(x: 20, y: 236, width: 480, height: 124), radius: 10,
            color: .labelColor.withAppearanceAlpha(0.06), border: .labelColor.withAppearanceAlpha(0.12))
        hostCard.identifier = NSUserInterfaceItemIdentifier("host_hub_card")
        hostCard.addSubview(textLabel("Share Hub on Local Network", NSRect(x: 14, y: 104, width: 260, height: 18), size: 13, weight: .semibold))
        hostCard.addSubview(textLabel("Let your other Macs or teammates connect to this Mac.", NSRect(x: 14, y: 86, width: 360, height: 15), size: 11, color: CitoPalette.secondaryText))

        let toggleTitle = hostHub.enabled ? "● Sharing" : "Off"
        let toggleHostBtn = MenuControl(toggleTitle, frame: NSRect(x: 396, y: 100, width: 70, height: 24), radius: 12)
        toggleHostBtn.identifier = NSUserInterfaceItemIdentifier("toggle_host_hub")
        toggleHostBtn.style = hostHub.enabled ? .prominent : .neutral
        let nextHostEnabled = !hostHub.enabled
        toggleHostBtn.pressed = { [weak self] in
            self?.emit(["op": "set_host_hub", "enabled": nextHostEnabled, "port": hostHub.port])
        }
        hostCard.addSubview(toggleHostBtn)

        let authMode=hostHub.auth_mode ?? (hostHub.token.isEmpty ? "none" : "token")
        let showToken=authMode == "token" && !hostHub.token.isEmpty
        let endpointLabel = textLabel(hostHub.endpoint_url, NSRect(x: 14, y: 46, width: showToken ? 254 : 452, height: 16),
            size: 11.5, weight: .medium, color: hostHub.enabled ? .labelColor : CitoPalette.secondaryText)
        endpointLabel.font = .monospacedSystemFont(ofSize: 11.5, weight: .medium)
        endpointLabel.lineBreakMode = .byTruncatingMiddle; endpointLabel.toolTip = endpointLabel.stringValue
        endpointLabel.identifier = NSUserInterfaceItemIdentifier("host_hub_endpoint")
        hostCard.addSubview(endpointLabel)
        if showToken {
            let token=MenuControl("Token: \(String(hostHub.token.prefix(8)))… Copy",frame:NSRect(x:274,y:44,width:192,height:20),leadingSymbol:"key.fill",radius:6)
            token.identifier=NSUserInterfaceItemIdentifier("copy_host_hub_token")
            token.pressed={ [weak self, weak token] in
                self?.copyText(hostHub.token, control:token, feedback:"✓ Token Copied")
            }
            hostCard.addSubview(token)
        }

        let copyInvite = MenuControl("Copy 1-Click Join Code", frame: NSRect(x: 14, y: 12, width: 168, height: 24), leadingSymbol: "doc.on.doc", radius: 6)
        copyInvite.identifier = NSUserInterfaceItemIdentifier("copy_hub_invite")
        let inviteCmd = hostHub.invite_command
        copyInvite.pressed = { [weak self, weak copyInvite] in
            self?.copyText(inviteCmd, control:copyInvite)
        }
        hostCard.addSubview(copyInvite)
        hostCard.addSubview(textLabel("Port:",NSRect(x:188,y:16,width:34,height:15),size:11))
        hostCard.addSubview(makeEditableField("host_hub_port",frame:NSRect(x:222,y:12,width:58,height:24),placeholder:"Port",value:hostHubPortDraft ?? "\(hostHub.port)"))
        let auth=MenuControl(authMode=="none" ? "Auth: None" : "Auth: Token",frame:NSRect(x:286,y:12,width:102,height:24),radius:6)
        auth.identifier=NSUserInterfaceItemIdentifier("toggle_host_hub_auth")
        auth.pressed={ [weak self] in
            guard let self=self else{return}
            self.emit(["op":"set_host_hub","enabled":hostHub.enabled,"port":hostHub.port,"auth_mode":authMode=="none" ? "token" : "none"])
        }
        hostCard.addSubview(auth)
        let apply=MenuControl("Apply",frame:NSRect(x:394,y:12,width:72,height:24),radius:6)
        apply.identifier=NSUserInterfaceItemIdentifier("apply_host_hub_port")
        if hostHubPortSaved && Date() < hostHubPortSavedUntil { apply.copied("✓ Saved") }
        apply.pressed={ [weak self] in
            guard let self=self else{return}
            let raw=(self.settingsFieldControls["host_hub_port"]?.stringValue ?? "").trimmingCharacters(in:.whitespacesAndNewlines)
            guard let port=Int(raw), (1...65535).contains(port) else {
                self.hostHubPortError="Port must be an integer from 1 to 65535."
                self.hostHubPortSaved=false
                self.rebuildSettingsPanel()
                return
            }
            let requestId = UUID().uuidString
            self.hostHubPortError=nil; self.hostHubPortPending=true; self.hostHubPortSaved=false; self.submittedHostHubPort=port
            self.submittedHostHubRequestId = requestId
            self.emit(["op":"set_host_hub","enabled":hostHub.enabled,"port":port,"request_id":requestId])
        }
        hostCard.addSubview(apply)
        if let error=hostHubPortError ?? hostHub.error, !error.isEmpty {
            let label=textLabel(presentationError(error),NSRect(x:14,y:66,width:452,height:14),size:10,color:CitoPalette.dangerText)
            label.lineBreakMode = .byTruncatingMiddle; label.toolTip = label.stringValue
            label.identifier=NSUserInterfaceItemIdentifier("host_hub_error"); hostCard.addSubview(label)
        }
        if !hostExpanded {
            for child in hostCard.subviews {
                if child === toggleHostBtn || child.frame.minY >= 86 { child.frame.origin.y -= 68 }
                else { child.isHidden = true }
            }
            hostCard.frame = NSRect(x:20,y:304,width:480,height:56)
        }
        hubsView.addSubview(hostCard)
        let hubOffset: CGFloat = hostExpanded ? 0 : 68

        hubsView.addSubview(textLabel("CONNECTED HUBS", NSRect(x: 20, y: 194 + hubOffset, width: 180, height: 14),
            size: 10, weight: .semibold, color: CitoPalette.secondaryText))

        let pasteBtn = MenuControl("Paste Invite", frame: NSRect(x: 272, y: 190 + hubOffset, width: 116, height: 22), leadingSymbol: "doc.on.clipboard", radius: 6)
        pasteBtn.identifier = NSUserInterfaceItemIdentifier("paste_hub_invite")
        if Date() < hubInvitePastedUntil { pasteBtn.copied("✓ Pasted") }
        pasteBtn.pressed = { [weak self] in self?.handlePasteHubInvite() }
        hubsView.addSubview(pasteBtn)

        let addBtn = MenuControl("Add Hub…", frame: NSRect(x: 396, y: 190 + hubOffset, width: 104, height: 22), leadingSymbol: "plus", radius: 6)
        addBtn.identifier = NSUserInterfaceItemIdentifier("open_add_hub")
        addBtn.pressed = { [weak self] in
            guard let self = self else { return }
            let wasEditing = self.editingExistingHub
            self.editingExistingHub=false
            self.editingOriginalHub=""
            self.addHubName=""; self.addHubUrl=""; self.addHubToken=""; self.addHubUser=""; self.addHubIssuer=""; self.addHubClientId=""; self.addHubAuth="none"
            self.addHubDraftGeneration += 1
            self.addHubError=nil; self.pendingHubSave=nil
            self.addHubFormOpen = wasEditing ? true : !self.addHubFormOpen
            self.rebuildSettingsPanel()
        }
        hubsView.addSubview(addBtn)

        if addHubFormOpen {
            let form = roundedView(NSRect(x: 20, y: 4, width: 480, height: 184), radius: 10,
                color: .labelColor.withAppearanceAlpha(0.06), border: .labelColor.withAppearanceAlpha(0.12))
            form.identifier = NSUserInterfaceItemIdentifier("add_hub_form")

            func fieldLabel(_ title: String, _ frame: NSRect) {
                form.addSubview(textLabel(title,frame,size:10,weight:.semibold,color:CitoPalette.secondaryText))
            }
            if editingExistingHub && editingOriginalHub == "local" {
                form.addSubview(textLabel("This Mac (Local Hub)",NSRect(x:14,y:156,width:452,height:18),size:12.5,weight:.semibold))
                form.addSubview(textLabel("Set the username shown when you send or receive messages on this Mac.",NSRect(x:14,y:134,width:452,height:16),size:11,color:CitoPalette.secondaryText))
                fieldLabel("YOUR USERNAME (OPTIONAL)",NSRect(x:14,y:104,width:452,height:14))
                form.addSubview(makeEditableField("settings_hub_user",frame:NSRect(x:14,y:76,width:452,height:24),placeholder:"e.g. alice",value:addHubUser))
                form.addSubview(textLabel("Shown to local agent sessions when you send or receive signals on this Mac.",NSRect(x:14,y:54,width:452,height:14),size:10.5,color:CitoPalette.secondaryText))
            } else {
                fieldLabel("HUB NAME",NSRect(x:14,y:164,width:146,height:13))
                fieldLabel("HUB URL (http://ip:4291 or https://host)",NSRect(x:168,y:164,width:298,height:13))
                form.addSubview(makeEditableField("settings_hub_name",frame:NSRect(x:14,y:138,width:146,height:24),placeholder:"e.g. work or studio",value:addHubName))
                form.addSubview(makeEditableField("settings_hub_url",frame:NSRect(x:168,y:138,width:298,height:24),placeholder:"http://192.168.1.10:4291",value:addHubUrl))
                fieldLabel("Access",NSRect(x:14,y:110,width:42,height:14))
                let authOptions: [(String,String,CGFloat,CGFloat)] = [("none","Trusted / Tailscale",60,132),("token","Shared Token",198,104),("oidc","Team OIDC",308,92)]
                for (mode,label,x,w) in authOptions {
                    let pill=MenuControl(label,frame:NSRect(x:x,y:106,width:w,height:22),radius:11)
                    pill.identifier=NSUserInterfaceItemIdentifier("settings_hub_auth:\(mode)")
                    pill.style = addHubAuth == mode ? .prominent : .neutral
                    pill.pressed={ [weak self] in
                        guard let self=self else{return}
                        self.syncAddHubFieldsFromControls()
                        if self.addHubAuth != mode { self.addHubDraftGeneration += 1 }
                        self.addHubAuth=mode; self.rebuildSettingsPanel()
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
                let protocolHelp=textLabel("Supports http://ip:4291 (Mac Share) or https:// (Team Hub).",NSRect(x:14,y:8,width:284,height:28),size:10,color:CitoPalette.secondaryText)
                protocolHelp.maximumNumberOfLines=2; protocolHelp.lineBreakMode = .byWordWrapping
                form.addSubview(protocolHelp)
            }

            if let error=addHubError {
                let label=textLabel(presentationError(error),NSRect(x:14,y:36,width:452,height:15),size:10,color:CitoPalette.dangerText)
                label.identifier=NSUserInterfaceItemIdentifier("add_hub_error"); label.toolTip=label.stringValue
                label.lineBreakMode = .byTruncatingMiddle; form.addSubview(label)
            }
            let cancelBtn = MenuControl("Cancel", frame: NSRect(x: 306, y: 12, width: 72, height: 24), radius: 6)
            cancelBtn.identifier = NSUserInterfaceItemIdentifier("cancel_add_hub")
            cancelBtn.pressed = { [weak self] in
                guard let self = self else { return }
                self.addHubFormOpen = false
                self.pendingHubSave = nil
                self.rebuildSettingsPanel()
            }
            form.addSubview(cancelBtn)

            let submitBtn = MenuControl(editingExistingHub ? "Save Hub" : "Join Hub", frame: NSRect(x: 386, y: 12, width: 80, height: 24), radius: 6)
            submitBtn.identifier = NSUserInterfaceItemIdentifier("submit_add_hub")
            submitBtn.style = .prominent
            submitBtn.isEnabled = pendingHubSave == nil
            submitBtn.pressed = { [weak self] in
                guard let self = self else { return }
                self.syncAddHubFieldsFromControls()
                let name = self.addHubName.trimmingCharacters(in: .whitespacesAndNewlines)
                let url = self.addHubUrl.trimmingCharacters(in: .whitespacesAndNewlines)
                let validation: String?
                if name.isEmpty { validation="Hub name is required." }
                else if !["http://","https://","local://"].contains(where:{url.hasPrefix($0)}) { validation="Hub URL must start with http://, https://, or local://." }
                else if !self.editingExistingHub && self.addHubAuth == "token" && self.addHubToken.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty { validation="Shared access token is required." }
                else if self.addHubAuth == "oidc" && (self.addHubIssuer.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty || self.addHubClientId.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty) { validation="OIDC issuer and client ID are required." }
                else { validation=nil }
                if let error=validation { self.addHubError=error; self.rebuildSettingsPanel(); return }
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
                let pending = PendingHubSave(name:name, url:url, requestId:UUID().uuidString, generation:self.addHubDraftGeneration)
                self.pendingHubSave=pending; self.addHubError=nil
                action["request_id"]=pending.requestId
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
            var rowTop = 180 + hubOffset
            for hub in resolvedHubs {
                let local=hub.name == "local"
                let needsSignIn = hub.auth_mode == "oidc" && hub.enabled && ["auth_required","disconnected"].contains(hub.status)
                let textWidth:CGFloat=local ? 300 : (needsSignIn ? 150 : 222)
                let authTitle = ["none":"Trusted Network","token":"Shared Token","oidc":"Team Sign-In"][hub.auth_mode] ?? "Network"
                let endpoint = hub.url == "local://" ? "On this Mac" : hub.url.replacingOccurrences(of:"^https?://",with:"",options:.regularExpression)
                let detail = hub.error.map(presentationError) ?? (hub.status == "connected" ? "" : (["auth_required":"Sign-in required","disconnected":"Offline","connecting":"Connecting…"][hub.status] ?? "Unavailable"))
                let subtitle=local ? "Built-in on this Mac · Signed in as @\(hub.user ?? "local")" : "\(endpoint) · \(authTitle)" + (hub.user.map { " · @\($0)" } ?? "") + (detail.isEmpty ? "" : " · \(detail)")
                let subtitleWidth:CGFloat = hub.status == "connected" && hub.error == nil ? (local ? 300 : 222) : 424
                let subtitleHeight = max(14,ceil((subtitle as NSString).boundingRect(with:NSSize(width:subtitleWidth,height:10000),options:[.usesLineFragmentOrigin,.usesFontLeading],attributes:[.font:NSFont.systemFont(ofSize:10.5)]).height)+2)
                let rowHeight = max(48,subtitleHeight+32)
                rowTop -= rowHeight
                let row = roundedView(NSRect(x:20,y:rowTop,width:480,height:rowHeight),radius:9,color:.labelColor.withAppearanceAlpha(0.05),border:.labelColor.withAppearanceAlpha(0.10))
                rowTop -= 6
                row.identifier = NSUserInterfaceItemIdentifier("settings_hub:\(hub.name)")
                row.addSubview(roundedView(NSRect(x:12,y:rowHeight-28,width:8,height:8),radius:4,color:statusColor(hub.enabled ? hub.status : "disconnected")))
                let nameLabel = textLabel(hub.name,NSRect(x:28,y:rowHeight-24,width:textWidth,height:16),size:12.5,weight:.semibold)
                nameLabel.lineBreakMode = .byTruncatingMiddle; nameLabel.toolTip = hub.name
                row.addSubview(nameLabel)
                let subtitleLabel = textLabel(subtitle,NSRect(x:28,y:4,width:subtitleWidth,height:subtitleHeight),size:10.5,color:CitoPalette.secondaryText)
                subtitleLabel.maximumNumberOfLines=0; subtitleLabel.lineBreakMode = .byWordWrapping
                subtitleLabel.toolTip = hub.url + (hub.error.map { "\n" + presentationError($0) } ?? "")
                row.addSubview(subtitleLabel)

                let edit=MenuControl("Edit",frame:NSRect(x:local ? 338 : 260,y:rowHeight-25,width:52,height:22),radius:6)
                edit.identifier=NSUserInterfaceItemIdentifier("edit_hub:\(hub.name)")
                edit.pressed={ [weak self] in
                    guard let self=self else{return}
                    self.addHubName=hub.name; self.addHubUrl=hub.url; self.addHubAuth=hub.auth_mode
                    self.addHubUser=hub.user ?? ""; self.addHubIssuer=hub.issuer ?? ""; self.addHubClientId=hub.client_id ?? ""; self.addHubToken=""
                    self.addHubDraftGeneration += 1
                    self.pendingHubSave=nil; self.addHubError=nil
                    self.editingExistingHub=true; self.editingOriginalHub=hub.name; self.addHubFormOpen=true
                    self.rebuildSettingsPanel()
                }
                row.addSubview(edit)
                if needsSignIn {
                    let loginBtn = MenuControl("Sign In…", frame: NSRect(x: 184, y: rowHeight-25, width: 70, height: 22), radius: 6)
                    loginBtn.identifier = NSUserInterfaceItemIdentifier("login_hub:\(hub.name)")
                    let hubName = hub.name
                    loginBtn.pressed = { [weak self] in
                        self?.emit(["op": "hub_action", "hub": hubName, "action": "login"])
                    }
                    row.addSubview(loginBtn)
                }

                let toggleBtn = MenuControl(hub.enabled ? "Enabled" : "Disabled", frame: NSRect(x: local ? 396 : 318, y: rowHeight-25, width: 72, height: 22), radius: 6)
                toggleBtn.identifier = NSUserInterfaceItemIdentifier("toggle_hub:\(hub.name)")
                toggleBtn.style = hub.enabled ? .prominent : .neutral
                let hubName = hub.name
                let nextEnabled = !hub.enabled
                toggleBtn.pressed = { [weak self] in
                    self?.emit(["op": "setup_toggle_hub", "hub": hubName, "enabled": nextEnabled])
                }
                row.addSubview(toggleBtn)

                if hub.name != "local" {
                    let removeBtn = MenuControl("Remove", frame: NSRect(x: 396, y: rowHeight-25, width: 72, height: 22), radius: 6)
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
        footerBar.isHidden = uninstallConfirmOpen
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
            let overlay = NSView(frame:NSRect(x:0,y:0,width:520,height:412))
            overlay.identifier = NSUserInterfaceItemIdentifier("uninstall_modal_overlay")
            let confirmBox = roundedView(NSRect(x: 40, y: 142, width: 440, height: 128), radius: 10,
                color: .windowBackgroundColor.withAppearanceAlpha(0.96), border: CitoPalette.dangerText.withAppearanceAlpha(0.35))
            confirmBox.identifier = NSUserInterfaceItemIdentifier("uninstall_confirm_box")

            confirmBox.addSubview(textLabel("Reset or Uninstall Cito",NSRect(x:12,y:98,width:416,height:18),size:12.5,weight:.semibold))
            let purgeCheckbox = MenuControl("Also delete local data and Hub settings",
                frame: NSRect(x: 12, y: 62, width: 416, height: 22),
                leadingSymbol: uninstallPurgeData ? "checkmark.square.fill" : "square", radius: 6)
            purgeCheckbox.identifier = NSUserInterfaceItemIdentifier("uninstall_purge_data")
            purgeCheckbox.label.alignment = .left
            purgeCheckbox.pressed = { [weak self] in
                guard let self = self else { return }
                self.uninstallPurgeData.toggle()
                self.rebuildSettingsPanel()
            }
            confirmBox.addSubview(purgeCheckbox)

            let cancelUninstallBtn = MenuControl("Cancel", frame: NSRect(x: 32, y: 16, width: 72, height: 24), radius: 6)
            cancelUninstallBtn.identifier = NSUserInterfaceItemIdentifier("cancel_uninstall_all")
            cancelUninstallBtn.pressed = { [weak self] in
                guard let self = self else { return }
                self.uninstallConfirmOpen = false
                self.uninstallPurgeData = false
                self.rebuildSettingsPanel()
            }
            confirmBox.addSubview(cancelUninstallBtn)

            let disconnect=MenuControl("Disconnect All",frame:NSRect(x:112,y:16,width:116,height:24),radius:6)
            disconnect.identifier=NSUserInterfaceItemIdentifier("confirm_disconnect_all")
            disconnect.pressed={ [weak self] in self?.uninstallConfirmOpen=false; self?.emit(["op":"setup_disconnect_all"]); self?.rebuildSettingsPanel() }
            confirmBox.addSubview(disconnect)
            let confirmUninstallBtn = MenuControl("Confirm Uninstall", frame: NSRect(x: 236, y: 16, width: 192, height: 24), radius: 6)
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
            overlay.addSubview(confirmBox); root.addSubview(overlay)
        }

        window.contentView = root
        if let id = focusedId, let field = settingsFieldControls[id] {
            window.makeFirstResponder(field)
            if let range = selectedRange, let editor = field.currentEditor() as? NSTextView { editor.setSelectedRange(range) }
        }
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
        uninstallConfirmOpen = false; uninstallPurgeData = false
        settingsPanel?.orderOut(nil)
        if emitAction { emit(["op": "close_settings"]) }
    }

    // MARK: - Surface 2: Spotlight-Style Network Directory Panel (560 × 440 pt KeyPanel: NSPanel)

    func ensureDirectoryPanel() {
        if directoryPanel != nil { return }
        let window = KeyPanel(contentRect: NSRect(x: 0, y: 0, width: 560, height: 440),
                              styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        window.appearance = testAppearance
        window.level = .floating
        window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        window.isFloatingPanel = true
        window.hidesOnDeactivate = false
        window.isOpaque = false
        window.backgroundColor = .clear
        window.hasShadow = true
        window.isMovableByWindowBackground = true

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
            ("dir_scope:all", "All", 16, 38),
            ("dir_scope:mine", "My Macs", 58, 68)
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
                self.queryDirectory()
            }
            facetBar.addSubview(pill)
            directoryFacetControls[id] = pill
        }
        let allHarnessesPill = MenuControl("All Apps", frame: NSRect(x: 138, y: 5, width: 68, height: 22), radius: 11)
        allHarnessesPill.identifier = NSUserInterfaceItemIdentifier("dir_harness:all")
        allHarnessesPill.label.font = .systemFont(ofSize: 11, weight: .medium)
        allHarnessesPill.pressed = { [weak self] in
            guard let self = self else { return }
            self.directoryHarnessFilter = "all"
            self.rebuildDirectoryFacetBar()
            self.applyDirectoryFilter(fromDaemonFrame: false, daemonMatchedCount: nil)
            self.queryDirectory()
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
                self.queryDirectory()
                self.updateDirectoryFacetStyles()
                self.applyActivityFilter()
            }
            facetBar.addSubview(pill)
            directoryFacetControls[id] = pill
        }
        let markReadBtn = MenuControl("✓ Read All", frame: NSRect(x: 390, y: 5, width: 88, height: 22), radius: 11)
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
                    from_scoped_id: item.from_scoped_id, from_display_address: item.from_display_address, target_display_address: item.target_display_address, message_truncated: item.message_truncated, target_harness: item.target_harness,
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
        let clear = MenuControl("Clear",frame:NSRect(x:484,y:5,width:60,height:22),leadingSymbol:"trash")
        clear.identifier = NSUserInterfaceItemIdentifier("activity_clear")
        clear.pressed = { [weak self] in self?.emit(["op":"clear_activity_history"]); self?.queryDirectory() }
        facetBar.addSubview(clear); directoryFacetControls["activity_clear"] = clear

        facetBar.addSubview(roundedView(NSRect(x: 0, y: 0, width: 560, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
        directoryFacetBar = facetBar
        root.addSubview(facetBar)
        updateDirectoryModePills()
        rebuildDirectoryFacetBar()

        // Virtualized Result List (560 × 334 pt, y: 28)
        let scroll = NSScrollView(frame: NSRect(x: 0, y: 28, width: 560, height: 334))
        scroll.scrollerStyle = .overlay
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

        let composerError = textLabel("", NSRect(x: 16, y: 44, width: 528, height: 15), size: 10.5, color: CitoPalette.dangerText)
        composerError.identifier = NSUserInterfaceItemIdentifier("composer_error")
        composerError.lineBreakMode = .byTruncatingMiddle
        composer.addSubview(composerError)
        composerErrorLabel = composerError
        directoryComposerBar = composer
        root.addSubview(composer)

        // Footer Shortcut Bar (560 × 28 pt, y: 0)
        let footerBar = NSView(frame: NSRect(x: 0, y: 0, width: 560, height: 28))
        footerBar.identifier = NSUserInterfaceItemIdentifier("directory_footer")
        footerBar.addSubview(roundedView(NSRect(x: 0, y: 27, width: 560, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
        let shortcuts = textLabel("↵ Copy Selected ID    ⎋ Close", NSRect(x: 16, y: 6, width: 260, height: 16), size: 11, color: CitoPalette.secondaryText)
        shortcuts.identifier = NSUserInterfaceItemIdentifier("directory_shortcuts")
        directoryShortcutsLabel = shortcuts
        footerBar.addSubview(shortcuts)
        updateDirectoryShortcuts()
        let summary = textLabel("0 of 0 online", NSRect(x: 320, y: 6, width: 224, height: 16), size: 11, color: CitoPalette.secondaryText)
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
        let canReply = activityFilteredItems.indices.contains(directorySelectedIndex) && activityFilteredItems[directorySelectedIndex].for_me && !(activityFilteredItems[directorySelectedIndex].from_display_address ?? activityFilteredItems[directorySelectedIndex].from_scoped_id ?? "").isEmpty
        directoryShortcutsLabel?.stringValue = !(directoryComposerBar?.isHidden ?? true)
            ? "↵ Send    ⎋ Cancel Reply"
            : (directoryMode == "activity" ? (canReply ? "↵ Reply    ⎋ Close" : "⎋ Close") : "↵ Copy Selected ID    ⎋ Close")
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
        queryDirectory()
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
        setComposerStatus("")
        composerTarget = target
        composerTargetHub = hub
        composerReplySignalId = replySignalId
        composerDraftIdempotencyKey = UUID().uuidString
        let displayTarget=target
        let targetWidth=max(120,min(200,measureTextWidth(displayTarget,font:.systemFont(ofSize:12))+12))
        composerTargetPill?.frame.size.width=targetWidth
        composerTargetPill?.label.frame.size.width=targetWidth-6
        composerTargetPill?.label.stringValue = displayTarget
        composerTargetPill?.label.lineBreakMode = .byTruncatingMiddle
        composerTargetPill?.label.toolTip = target
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
        // Keep the entire pending-send snapshot until its result, even after Cancel.
        composerDraftIdempotencyKey = UUID().uuidString
        composerTarget = ""
        composerTargetHub = ""
        composerReplySignalId = nil
        composerInputField?.stringValue = ""
        directoryComposerBar?.isHidden = true
        setComposerStatus("")
        updateDirectoryShortcuts()
        directoryScrollView?.frame = NSRect(x: 0, y: 28, width: 560, height: 334)
        directoryPanel?.makeFirstResponder(directorySearchField)
    }

    func submitComposer() {
        let text = (composerInputField?.stringValue ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard pendingComposer == nil, !composerTarget.isEmpty, !text.isEmpty else { return }
        // Match the daemon's Unicode code-point limit, including combining scalars.
        guard text.unicodeScalars.count <= 65_536 else {
            setComposerStatus("INVALID_INPUT: Message exceeds maximum length")
            return
        }
        let requestId = UUID().uuidString
        var action: [String: Any] = [
            "op": "send_signal",
            "to": composerTarget,
            "message": text,
            "request_id": requestId,
            "idempotency_key": composerDraftIdempotencyKey
        ]
        if !composerTargetHub.isEmpty {
            action["hub"] = composerTargetHub
        }
        if let replyId = composerReplySignalId, !replyId.isEmpty {
            action["in_reply_to"] = replyId
        }
        guard let data = try? JSONSerialization.data(withJSONObject: action, options: [.sortedKeys]), data.count <= 1_048_576 else {
            setComposerStatus("INVALID_INPUT: UI action exceeds 1 MiB")
            return
        }
        pendingComposer = PendingComposerSend(text:composerInputField?.stringValue ?? text,
            requestId:requestId, target:composerTarget, hub:composerTargetHub, replySignalId:composerReplySignalId)
        setComposerStatus("Sending…")
        emit(action)
    }

    func rebuildDirectoryFacetBar() {
        guard let facetBar = directoryFacetBar else { return }
        let map = harnessMap
        let unionHarnesses = Set<String>(harnesses.filter { $0.id != "human" && ($0.kind == nil || $0.kind == "agent" || $0.kind == "agent-harness") }.map { $0.id })
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
        var curX: CGFloat = 210
        for h in ordered {
            let visual = resolveHarnessVisual(h, harnesses: map)
            let title = ordered.count <= 3 ? visual.name.replacingOccurrences(of:" (Experimental)",with:"") : h
            let w = max(42, ceil(measureTextWidth(title, font: pillFont)) + 20)
            layout.append((id: "dir_harness:\(h)", harness: h, title: title, x: curX, width: w))
            curX += w + 4
        }
        let totalEnd = layout.last.map { $0.x + $0.width } ?? 210
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
                    self.queryDirectory()
                }
                facetBar.addSubview(pill)
                directoryDynamicHarnessViews.append(pill)
                directoryFacetControls[item.id] = pill
            }
        } else {
            let scrollWidth: CGFloat = 334
            let scroll = NSScrollView(frame: NSRect(x: 210, y: 5, width: scrollWidth, height: 22))
            scroll.identifier = NSUserInterfaceItemIdentifier("directory_harness_scroll")
            scroll.scrollerStyle = .overlay
            scroll.drawsBackground = false
            scroll.hasHorizontalScroller = false
            scroll.hasVerticalScroller = false
            scroll.autohidesScrollers = true
            let doc = NSView(frame: NSRect(x: 0, y: 0, width: totalEnd - 210, height: 22))
            for item in layout {
                let pill = MenuControl(item.title, frame: NSRect(x: item.x - 210, y: 0, width: item.width, height: 22), radius: 11)
                pill.identifier = NSUserInterfaceItemIdentifier(item.id)
                pill.label.font = pillFont
                let harnessVal = item.harness
                pill.pressed = { [weak self] in
                    guard let self = self else { return }
                    self.directoryHarnessFilter = harnessVal
                    self.rebuildDirectoryFacetBar()
                    self.applyDirectoryFilter(fromDaemonFrame: false, daemonMatchedCount: nil)
                    self.queryDirectory()
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
            } else if id == "activity_mark_all_read" || id == "activity_clear" {
                pill.isHidden = !isActivity
                pill.style = .neutral
            } else {
                pill.isHidden = isActivity
                let active = (id == "dir_scope:\(directoryScopeFilter)") || (id == "dir_harness:\(directoryHarnessFilter)")
                pill.style = active ? .prominent : .neutral
                if !isActivity && active && id.hasPrefix("dir_harness:") { pill.scrollToVisible(pill.bounds) }
            }
        }
    }

    func updateDirectoryEmptyState() {
        guard let root=directoryPanel?.contentView else{return}
        root.subviews.filter{$0.identifier?.rawValue=="directory_empty_state"}.forEach{$0.removeFromSuperview()}
        let empty=directoryMode=="directory" ? directoryFilteredPeers.isEmpty : activityFilteredItems.isEmpty
        directoryScrollView?.isHidden = empty
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
                    self.queryDirectory()
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
        box.addSubview(textLabel(subtitle,NSRect(x:20,y:64,width:480,height:20),size:12,color:CitoPalette.secondaryText))
        let button=MenuControl(label,frame:NSRect(x:166,y:18,width:188,height:26),radius:6)
        button.identifier=NSUserInterfaceItemIdentifier(id); button.pressed=action; box.addSubview(button)
        root.addSubview(box)
    }
    func applyActivityFilter() {
        let q = directoryQuery.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let serverMatches = serverActivityFilter == activityFilter && serverActivityQuery == directoryQuery
        activityFilteredItems = activityAllItems.filter { item in
            if serverMatches { return true }
            if activityFilter == "me" && !item.for_me { return false }
            if activityFilter == "unread" && !item.unread { return false }
            if !q.isEmpty {
                let hay = [item.from_user, item.from_host_name, item.from_title, item.target_title, item.message, item.signal_id, item.listener_id, item.target_harness == "human" ? "You" : ""]
                    .joined(separator: " ").lowercased()
                if !hay.contains(q) { return false }
            }
            return true
        }
        directorySelectedIndex = activityFilteredItems.isEmpty ? 0 : min(max(0, directorySelectedIndex), activityFilteredItems.count - 1)
        updateDirectoryShortcuts()
        updateDirectoryEmptyState()
        let count = serverMatches ? max(activityTotal, activityFilteredItems.count) : activityFilteredItems.count
        directorySummaryLabel?.stringValue = "\(count) signal\(count == 1 ? "" : "s") · \(serverMatches ? activityUnreadCount : activityFilteredItems.filter { $0.unread }.count) unread"
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
        queryDirectory(op:"open_directory")
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
            } else if tok.hasPrefix("harness:") && tok.count > 8 {
                if !peer.harness.lowercased().contains(tok.dropFirst(8)) { return false }
            } else if tok.hasPrefix("#") && tok.count > 1 {
                let idTok = String(tok.dropFirst())
                if !peer.public_id.lowercased().contains(idTok) && !peer.scoped_id.lowercased().contains(idTok) && !peer.listener_id.lowercased().contains(idTok) { return false }
            } else {
                let userSlug = peer.public_id.split(separator:"-").dropLast().joined(separator:"-")
                let fullAlias = "\(userSlug)/\(peer.harness)#\(peer.listener_id.hasPrefix("c_") ? String(peer.listener_id.dropFirst(2)) : "")"
                let hay = [fullAlias, peer.display_address, peer.title, peer.about, peer.cwd ?? "", peer.repo ?? "", peer.branch ?? "", peer.user, peer.host_name, peer.host_id, peer.harness, peer.public_id, peer.scoped_id, peer.listener_id]
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
        if (directoryPanel?.firstResponder as? NSTextInputClient)?.hasMarkedText() == true { return }
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
            if !(directoryComposerBar?.isHidden ?? true) {
                if composerInputField?.stringValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true { setComposerStatus("Enter a message to send.") }
                else { submitComposer() }
                return
            }
            if directoryMode == "activity" {
                guard activityFilteredItems.indices.contains(directorySelectedIndex) else { return }
                replyToActivity(activityFilteredItems[directorySelectedIndex])
                return
            }
            guard directoryFilteredPeers.indices.contains(directorySelectedIndex) else { return }
            let peer = directoryFilteredPeers[directorySelectedIndex]
            copyText(peer.display_address)
            if let table = directoryTableView,
               let cell = table.view(atColumn: 0, row: directorySelectedIndex, makeIfNecessary: true) as? DirectoryPeerCellView {
                cell.copyControl.copied()
            }
        case "command-w": closeDirectoryPanel(emitAction:true)
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
            queryDirectory()
            return
        }
        if let field = obj.object as? NSTextField, field === composerInputField { composerDraftIdempotencyKey = UUID().uuidString }
        if let field=obj.object as? NSTextField, field.identifier?.rawValue == "host_hub_port" { hostHubPortDraft=field.stringValue; hostHubPortSaved=false; hostHubPortError=nil }
        if let field = obj.object as? NSTextField, let id = field.identifier?.rawValue {
            if id.hasPrefix("channel_field:") { channelFieldDrafts[draftKey(id)] = field.stringValue }
            else if id.hasPrefix("settings_hub_") { addHubDraftGeneration += 1 }
        }
        syncAddHubFieldsFromControls()
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
        if textView.hasMarkedText() { return false }
        if control.window === settingsPanel {
            if commandSelector == #selector(NSResponder.cancelOperation(_:)) {
                handleSettingsKey("escape")
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

    func copyActivity(_ item: ActivityItem, control: MenuControl) {
        if item.message_truncated != true { copyText(item.message,control:control); return }
        let request = UUID().uuidString
        pendingMessageCopy = PendingMessageCopy(requestId:request,control:control)
        emit(["op":"copy_activity_message","request_id":request,"hub":item.hub,"signal_id":item.signal_id])
    }
    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        guard directoryMode == "activity", row == directorySelectedIndex, activityFilteredItems.indices.contains(row) else { return 52 }
        let text = activityFilteredItems[row].message
        return text.contains("\n") || measureTextWidth(text,font:.systemFont(ofSize:11.5))>330 ? 108 : 52
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
            cell.configureActivity(with: item, selected: row == directorySelectedIndex, harnesses: harnessMap, onCopy: { [weak self] control in self?.copyActivity(item,control:control) }) { [weak self] in
                guard let self = self else { return }
                self.replyToActivity(item)
            }
            return cell
        }
        guard directoryFilteredPeers.indices.contains(row) else { return nil }
        let peer = directoryFilteredPeers[row]
        cell.configure(with: peer, selected: row == directorySelectedIndex, harnesses: harnessMap, onCopy: { [weak self] text, control in self?.copyText(text,control:control) }) { [weak self] in
            guard let self = self else { return }
            self.openComposer(target: peer.display_address, hub: peer.hub, replySignalId: nil)
        }
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard let table = notification.object as? NSTableView, table === directoryTableView else { return }
        let row = table.selectedRow
        let count = directoryMode == "activity" ? activityFilteredItems.count : directoryFilteredPeers.count
        guard row >= 0 && row < count else { return }
        let previous = directorySelectedIndex
        directorySelectedIndex = row
        if directoryMode == "activity" {
            let indexes = IndexSet([previous,row].filter { $0 >= 0 && $0 < count })
            table.noteHeightOfRows(withIndexesChanged:indexes)
            for index in indexes {
                if let cell = table.view(atColumn:0,row:index,makeIfNecessary:false) as? DirectoryPeerCellView {
                    let item = activityFilteredItems[index]
                    cell.configureActivity(with:item,selected:index==row,harnesses:harnessMap,onCopy:{ [weak self] c in self?.copyActivity(item,control:c) },onReply:{ [weak self] in self?.replyToActivity(item) })
                }
            }
        }
        updateDirectoryShortcuts()
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
        else if surface == "banner", let content = panel?.contentView { view = content }
        else if surface == "menu" {
            let rows=cardMenu.items.filter { !$0.isHidden }
            let height=rows.reduce(CGFloat(8)) { $0 + ($1.view?.frame.height ?? 9) }
            let composite=FlippedView(frame:NSRect(x:0,y:0,width:380,height:height))
            composite.wantsLayer=true
            composite.appearance = testAppearance
            composite.effectiveAppearance.performAsCurrentDrawingAppearance {
                composite.layer?.backgroundColor = CitoPalette.surfaceBackground.cgColor
            }
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
        if surface == "directory", let table = directoryTableView {
            for row in 0..<min(table.numberOfRows, 100) {
                _ = table.view(atColumn:0, row:row, makeIfNecessary:true)
            }
            table.layoutSubtreeIfNeeded()
        }
        view.window?.appearance = testAppearance
        view.appearance = testAppearance
        // Behind-window effects cannot sample a desktop in an offscreen panel.
        // Substitute the declared appearance backdrop only for capture, restoring
        // the live material and its root fill before returning to event handling.
        let effects = view.subviews.compactMap { $0 as? NSVisualEffectView }
        let effectVisibility = effects.map { $0.isHidden }
        let surface = view as? SurfaceView
        let originalFill = surface?.fillColor
        effects.forEach { $0.isHidden = true }
        surface?.fillColor = CitoPalette.surfaceBackground
        defer {
            for (effect, hidden) in zip(effects, effectVisibility) { effect.isHidden = hidden }
            if let fill = originalFill { surface?.fillColor = fill }
        }
        view.layoutSubtreeIfNeeded()
        guard let bitmap=NSBitmapImageRep(bitmapDataPlanes:nil,pixelsWide:Int(view.bounds.width*2),pixelsHigh:Int(view.bounds.height*2),bitsPerSample:8,samplesPerPixel:4,hasAlpha:true,isPlanar:false,colorSpaceName:.deviceRGB,bytesPerRow:0,bitsPerPixel:0) else { diagnostic("render failed: bitmap allocation"); return }
        bitmap.size=view.bounds.size
        // Offscreen visual effects have no desktop to sample. Provide the same opaque
        // Light/Dark backing used by the contrast oracle, then composite real AppKit drawing.
        view.effectiveAppearance.performAsCurrentDrawingAppearance {
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)
            NSGraphicsContext.current?.cgContext.scaleBy(x: 2, y: 2)
            CitoPalette.surfaceBackground.setFill()
            view.bounds.fill()
            NSGraphicsContext.restoreGraphicsState()
            view.cacheDisplay(in:view.bounds,to:bitmap)
        }
        guard let data=bitmap.representation(using:.png,properties:[:]) else { diagnostic("render failed: PNG encoding"); return }
        do { try data.write(to:URL(fileURLWithPath:path),options:.atomic); diagnostic("rendered " + path) }
        catch { diagnostic("render failed: " + error.localizedDescription) }
    }

    func inspectMenu() {
        var truncatedLabels: [String] = []
        var overlappingSiblings: [String] = []
        var clippedViews: [String] = []
        var lowContrastLabels: [String] = []
        var jargonLeaks: [String] = []
        var addressMismatches: [String] = []
        func name(_ view: NSView) -> String {
            view.identifier?.rawValue ?? (view as? NSTextField)?.stringValue ?? String(describing:type(of:view))
        }
        func isDataRow(_ view: NSView) -> Bool {
            var ancestor: NSView? = view
            while let current = ancestor {
                let id = current.identifier?.rawValue ?? ""
                if id.hasPrefix("dir_") || id.hasPrefix("act_") || id.hasPrefix("session:") || id.hasPrefix("candidate:") || id.hasPrefix("remote:") || id.hasPrefix("banner_data:") || current is DirectoryPeerCellView { return true }
                ancestor = current.superview
            }
            return false
        }
        func components(_ color: NSColor?) -> [CGFloat] {
            guard let rgb = color?.usingColorSpace(.deviceRGB) else { return [] }
            return [rgb.redComponent, rgb.greenComponent, rgb.blueComponent, rgb.alphaComponent]
        }
        func resolved(_ color: NSColor, in appearance: NSAppearance) -> NSColor {
            var rgb = NSColor.black
            appearance.performAsCurrentDrawingAppearance { rgb = color.usingColorSpace(.sRGB) ?? .black }
            return rgb
        }
        func blended(_ foreground: NSColor, over background: NSColor, opacity: CGFloat = 1) -> NSColor {
            let a = foreground.alphaComponent * opacity
            return NSColor(srgbRed: foreground.redComponent * a + background.redComponent * (1-a),
                green: foreground.greenComponent * a + background.greenComponent * (1-a),
                blue: foreground.blueComponent * a + background.blueComponent * (1-a), alpha: 1)
        }
        func luminance(_ color: NSColor) -> CGFloat {
            func linear(_ c: CGFloat) -> CGFloat { c <= 0.04045 ? c/12.92 : pow((c+0.055)/1.055, 2.4) }
            return 0.2126 * linear(color.redComponent) + 0.7152 * linear(color.greenComponent) + 0.0722 * linear(color.blueComponent)
        }
        func project(_ view: NSView, visible: Bool = true, background: NSColor? = nil, opacity: CGFloat = 1, enabled: Bool = true) -> [String: Any] {
            let visible = visible && !view.isHidden
            let opacity = opacity * view.alphaValue
            let enabled = enabled && ((view as? NSControl)?.isEnabled ?? true)
            let base = background ?? resolved(CitoPalette.surfaceBackground, in: view.effectiveAppearance)
            var surface = base
            // Resolve dynamic layer-backed fills in the same appearance as the text.
            if let control = view as? MenuControl { control.applyVisualState() }
            if let fill = view as? SurfaceView { fill.applyLayerColors() }
            if let fill = view.layer?.backgroundColor.flatMap({ NSColor(cgColor: $0) }) {
                surface = blended(resolved(fill, in: view.effectiveAppearance), over: base, opacity: opacity)
            }
            var contrast: CGFloat?
            if visible, enabled, opacity > 0.6, let label = view as? NSTextField,
               !label.stringValue.isEmpty, let color = label.textColor {
                let foreground = blended(resolved(color, in: view.effectiveAppearance), over: surface, opacity: opacity)
                let a = luminance(foreground), b = luminance(surface)
                let ratio = (max(a,b)+0.05)/(min(a,b)+0.05)
                contrast = ratio
                if ratio < 4.5 {
                    let labelName = (label.superview as? MenuControl).map { name($0) } ?? name(label)
                    lowContrastLabels.append("\(labelName): \(String(format: "%.2f", Double(ratio)))")
                }
            }
            if visible, let label = view as? NSTextField, label.isEditable {
                if label.frame.width < 48 || measureTextWidth(label.placeholderString ?? "", font:label.font ?? .systemFont(ofSize:12)) > label.frame.width - 8 { truncatedLabels.append(name(label)) }
            }
            if visible, let label = view as? NSTextField, !label.isEditable,
               (label.superview is MenuControl || !isDataRow(label) || name(label).hasPrefix("dir_user:")) {
                let font = label.font ?? .systemFont(ofSize:12)
                let truncated: Bool
                if label.maximumNumberOfLines == 1 {
                    // Native cell insets and attributed glyph metrics determine
                    // what AppKit can actually draw, not the outer field width.
                    let drawingWidth = label.cell?.drawingRect(forBounds:label.bounds).width ?? label.bounds.width
                    let requiredWidth = label.cell?.cellSize.width ?? label.attributedStringValue.size().width
                    truncated = requiredWidth > drawingWidth + 0.5
                } else {
                    let height = ceil((label.stringValue as NSString).boundingRect(with:NSSize(width:label.frame.width,height:CGFloat.greatestFiniteMagnitude),options:[.usesLineFragmentOrigin],attributes:[.font:font]).height)
                    truncated = height > label.frame.height + 2
                }
                if truncated { truncatedLabels.append((label.superview as? MenuControl).map { name($0) } ?? name(label)) }
            }
            if visible, let label = view as? NSTextField, !label.isEditable {
                let id = name(label)
                let data = isDataRow(label) && !(label.superview is MenuControl) && !id.hasPrefix("dir_user:")
                var generatedText = label.stringValue
                if id.hasPrefix("act_title:"), let item = activityAllItems.first(where: { $0.signal_id == String(id.dropFirst("act_title:".count)) }), item.target_harness != "human" {
                    // This line interpolates only the target title. Exclude its exact span,
                    // retaining generated prefixes/suffixes even if they repeat that title.
                    let prefix = (item.unread ? "● " : "") + "@\(item.from_user) → "
                    let titleSpan = prefix + item.target_title
                    if generatedText.hasPrefix(titleSpan) { generatedText = prefix + String(generatedText.dropFirst(titleSpan.count)) }
                }
                let dataJargon = (id == "banner_data:1" && label.stringValue.hasPrefix("[Passive]")) || (id.hasPrefix("act_title:") && generatedText.range(of:"Human Node|Human receiver|\\[Passive\\]|^[A-Z0-9_]{3,}:",options:.regularExpression) != nil)
                if dataJargon || !data && label.stringValue.range(of:"local://|Handle:|Human Node|Human listening|Human receiver|All Harnesses|\\bSession\\b|· (none|token|oidc|auth_required|disconnected)\\b|\\[Passive\\]|^[A-Z0-9_]{3,}:",options:.regularExpression) != nil { jargonLeaks.append(id) }
            }
            if visible, let control = view as? MenuControl {
                let id = control.identifier?.rawValue ?? ""
                let scope = ["copy:","copy_remote:","copy_dir:"].first { id.hasPrefix($0) }
                if let prefix = scope {
                    let scoped = String(id.dropFirst(prefix.count))
                    let address = listeners.first { $0.scoped_id == scoped }?.display_address ?? (myRemote + directoryAllPeers).first { $0.scoped_id == scoped }?.display_address
                    let label = control.feedbackTimer?.isValid == true ? (control.accessibilityLabel() ?? "") : control.label.stringValue
                    let listener = listeners.first { $0.scoped_id == scoped }
                    let peer = (myRemote + directoryAllPeers).first { $0.scoped_id == scoped }
                    let hub = listener?.hub ?? peer?.hub ?? ""
                    let harness = listener?.harness ?? peer?.harness ?? ""
                    let defaultHub = settings?.hubs.first(where: { $0.is_default })?.name ?? hubs.first?.name ?? ""
                    if label.isEmpty || address != label || (harness == "human" && !label.contains("@")) || (hub != defaultHub && !label.hasPrefix("\(hub):")) { addressMismatches.append(id) }
                }
            }
            if visible, let scroll = view as? NSScrollView {
                let defaultLayout: Bool
                switch name(scroll) {
                case "settings_apps_tab": defaultLayout = (settings?.apps.count ?? 0) <= 3
                case "settings_channels_tab": defaultLayout = settings?.channels?.count == 1 && editingHubGateway.isEmpty && (settings?.channels?.first?.fields.count ?? 0) <= 2
                case "settings_hubs_tab": defaultLayout = (settings?.hubs.count ?? 0) <= 2 && settings?.host_hub.enabled != true && settings?.host_hub.error == nil && !hostHubPortPending && hostHubPortDraft == nil && hostHubPortError == nil && !addHubFormOpen
                default: defaultLayout = false
                }
                if defaultLayout && settingsBannerError == nil && (settingsScrollOffsets[name(scroll)] ?? 0) == 0, let doc = scroll.documentView, doc.frame.height > scroll.contentSize.height + 0.75 { clippedViews.append(name(scroll)+": default viewport needs scrolling") }
                if ((scroll.hasVerticalScroller || scroll.hasHorizontalScroller) && scroll.scrollerStyle != .overlay) || (scroll.hasHorizontalScroller && scroll.frame.height < 34) { overlappingSiblings.append(name(scroll)+": scroller geometry") }
                if let doc = scroll.documentView {
                    for pill in doc.subviews.compactMap({ $0 as? MenuControl }) where pill.style == .prominent || selectedHubs.contains(String((pill.identifier?.rawValue ?? "").dropFirst(4))) {
                        if !scroll.contentView.bounds.contains(pill.frame) { clippedViews.append(name(scroll)+": selected pill") }
                    }
                }
            }
            let childViews: [NSView]
            // Native field/image internals are AppKit rendering layers, not layout
            // siblings authored by this UI (they intentionally overlay one another).
            if view is NSTextField || view is NSImageView {
                childViews = []
            } else if let table = view as? NSTableView {
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
            if visible {
                let children = childViews.filter { !$0.isHidden && !($0 is NSVisualEffectView) }
                let parentBounds = (view as? NSScrollView)?.documentView?.bounds ?? view.bounds
                func frame(_ child: NSView) -> NSRect {
                    // Table cells live in separate row views; their local origins
                    // are all zero. Compare them in the table's coordinate space.
                    view is NSTableView ? child.convert(child.bounds,to:view) : child.frame
                }
                for (index, child) in children.enumerated() {
                    let f = frame(child)
                    if f.minX < -0.5 || f.minY < -0.5 || f.maxX > parentBounds.width + 0.5 || f.maxY > parentBounds.height + 0.5 {
                        clippedViews.append("\(name(view)) > \(name(child))")
                    }
                    for other in children.dropFirst(index+1) where f.insetBy(dx:0.5,dy:0.5).intersects(frame(other).insetBy(dx:0.5,dy:0.5)) {
                        overlappingSiblings.append("\(name(view)): \(name(child)) / \(name(other))")
                    }
                }
            }
            var result: [String: Any] = ["id": (view as? DirectoryPeerCellView)?.inspectionId ?? view.identifier?.rawValue ?? "", "width": view.frame.width,
                "height": view.frame.height, "x": view.frame.origin.x, "y": view.frame.origin.y,
                "radius": view.layer?.cornerRadius ?? 0, "hidden": view.isHidden,
                "children": childViews.map { project($0, visible: visible, background: surface, opacity: opacity, enabled: enabled) }]
            result["contrast"] = contrast
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
                result["tooltip"] = label.toolTip ?? ""
            }
            if let control = view as? NSControl { result["enabled"] = control.isEnabled; result["alpha"] = control.alphaValue }
            if let control = view as? MenuControl {
                result["text"] = control.label.stringValue; result["tooltip"] = control.toolTip ?? ""
                result["label_width"] = control.label.frame.width
                result["label_measured_width"] = measureTextWidth(control.label.stringValue,font:control.label.font ?? .systemFont(ofSize:12))
                result["green"] = control.label.textColor == CitoPalette.successText
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
        let bannerItems = panel?.contentView.map { [project($0,visible:self.visible)] } ?? []
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
        let snapshot: [String: Any] = ["items": items, "truncated_labels": truncatedLabels, "overlapping_siblings": overlappingSiblings, "clipped_views": clippedViews, "icon_alpha": probes, "count": uniqueSessionCount,
            "low_contrast_labels": lowContrastLabels, "jargon_leaks":jargonLeaks, "address_mismatches":addressMismatches, "banner_items":bannerItems, "wake_action_text": wakeActionText,
            "available_count": available.count, "rebuilds": rebuildCount,
            "directory_open": directoryVisible, "directory_mode": directoryMode, "activity_filter": activityFilter,
            "unread_count": unreadCount, "composer_open": !(directoryComposerBar?.isHidden ?? true),
            "composer_target": composerTarget, "composer_reply_signal_id": composerReplySignalId ?? "",
            "directory_online_count": directoryOnlineCount,
            "directory_matched_count": directoryMatchedCount, "directory_query": directoryQuery,
            "directory_scope": directoryScopeFilter, "directory_harness": directoryHarnessFilter,
            "directory_selected_id": selectedId, "directory_selected_index": directorySelectedIndex, "directory_items": directoryItems,
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
    func replyToActivity(_ item: ActivityItem) {
        guard item.for_me, let target = item.from_display_address ?? item.from_scoped_id, !target.isEmpty else { return }
        openComposer(target: target, hub: item.hub, replySignalId: item.signal_id)
    }
    private func matchesWakeSignal(_ item: ActivityItem, payload: Payload) -> Bool {
        guard let signalId = payload.signal_id, !signalId.isEmpty else { return false }
        let parts = signalId.split(separator: ":", maxSplits: 1).map(String.init)
        let effectiveHub = (payload.hub ?? "").isEmpty ? (parts.count == 2 ? parts[0] : "") : (payload.hub ?? "")
        if !effectiveHub.isEmpty && item.hub != effectiveHub { return false }
        return item.signal_id == signalId
            || (!item.hub.isEmpty && "\(item.hub):\(item.signal_id)" == signalId)
            || (parts.count == 2 && parts[0] == item.hub && parts[1] == item.signal_id)
    }
    func openActivityForSignal(_ value: Payload) {
        closeComposer()
        directoryQuery = ""; directorySearchField?.stringValue = ""
        activityFilter = "all"
        openDirectoryPanel(mode: "activity")
        directorySelectedIndex = activityFilteredItems.firstIndex { matchesWakeSignal($0, payload: value) } ?? 0
        applyActivityFilter()
    }
    func openDirectoryForReply(_ value: Payload) {
        guard value.target_harness == "human" else { return }
        guard let target = value.from_display_address ?? value.from_scoped_id, !target.isEmpty else { openActivityForSignal(value); return }
        directoryQuery = ""; directorySearchField?.stringValue = ""
        activityFilter = "all"
        openDirectoryPanel(mode: "activity")
        directorySelectedIndex = activityFilteredItems.firstIndex { matchesWakeSignal($0, payload: value) } ?? 0
        applyActivityFilter()
        openComposer(target: target, hub: value.hub ?? "", replySignalId: value.signal_id)
    }
    func hide(clicked: Bool) {
        timer?.cancel()
        let clickedPayload = (clicked && visible) ? payload : nil
        visible = false; panel?.orderOut(nil)
        if let val = clickedPayload {
            if val.target_harness == "human" { openDirectoryForReply(val) }
            else { openActivityForSignal(val) }
        }
        if headless { diagnostic(clicked ? "banner clicked" : "banner hidden") }
    }
    // CALayer stores resolved CGColors, so refresh the banner when its appearance changes.
    func refreshBannerColors() {
        guard let value = payload, labels.count == 5 else { return }
        let color = value.effective_wake ? CitoPalette.warningText : CitoPalette.infoText
        panel?.effectiveAppearance.performAsCurrentDrawingAppearance {
            accentBar?.layer?.backgroundColor = color.cgColor
            iconBadge?.layer?.backgroundColor = (value.effective_wake ? CitoPalette.warningFill : CitoPalette.infoText.withAppearanceAlpha(0.14)).cgColor
            labels[4].layer?.backgroundColor = (value.effective_wake ? CitoPalette.warningSurface : CitoPalette.infoText.withAppearanceAlpha(0.14)).cgColor
        }
        icon?.contentTintColor = color
        labels[4].textColor = color
    }
    func show(_ value: Payload) {
        let title = (value.target_harness == "human" ? "Cito · Direct Message" : "Cito · \(value.target_title)") + (displayedCount > 1 ? " (\(displayedCount))" : "")
        let semanticLabel = (value.effective_wake ? "Notification" : "Passive") + (displayedCount > 1 ? " · ×\(displayedCount)" : "")
        let actionText = value.target_harness == "human" ? "Click to reply in Cito ↩" : "Click to view in Activity ↗"
        wakeActionText = actionText
        visible = true
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
            window.appearance = testAppearance
            let root = ClickView(frame: window.contentView!.bounds)
            root.clicked = { [weak self] in self?.hide(clicked: true) }
            root.appearanceChanged = { [weak self] in self?.refreshBannerColors() }
            root.setAccessibilityElement(true)
            root.setAccessibilityRole(.button)
            root.wantsLayer = true
            root.layer?.cornerRadius = 16
            root.layer?.cornerCurve = .continuous
            root.layer?.masksToBounds = true
            root.layer?.borderWidth = 1
            root.layer?.borderColor = NSColor.labelColor.withAppearanceAlpha(0.16).cgColor
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
                label.textColor = index == 1 ? CitoPalette.secondaryText : .labelColor
                label.frame = frames[index]
                label.identifier = NSUserInterfaceItemIdentifier(index < 3 ? "banner_data:\(index)" : "banner_chrome:\(index)")
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
        icon?.image = NSImage(systemSymbolName: value.effective_wake ? "bell.badge.fill" : "tray.and.arrow.down.fill", accessibilityDescription: semanticLabel)
        let badgeW = max(76,ceil(measureTextWidth(semanticLabel,font:.boldSystemFont(ofSize:10))) + 16)
        labels[4].frame = NSRect(x:372-badgeW,y:103,width:badgeW,height:20)
        labels[0].frame.size.width = 372-badgeW-68
        labels[0].identifier = NSUserInterfaceItemIdentifier(value.target_harness == "human" ? "banner_chrome:0" : "banner_data:0")
        labels[0].stringValue = title
        labels[1].stringValue = value.displaySubtitle
        labels[2].stringValue = value.displayMessage
        labels[3].stringValue = actionText
        labels[3].textColor = CitoPalette.infoText
        labels[4].stringValue = semanticLabel
        refreshBannerColors()

        panel?.contentView?.setAccessibilityLabel("\(labels[0].stringValue). \(semanticLabel). \(value.displaySubtitle). \(value.displayMessage). \(labels[3].stringValue)")
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
