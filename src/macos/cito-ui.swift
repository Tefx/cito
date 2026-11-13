import AppKit
import Foundation
import Darwin
import QuartzCore

// Headless tests use a real private pasteboard without changing the user clipboard.
let usesPrivatePasteboard = ProcessInfo.processInfo.environment["CITO_UI_HEADLESS"] == "1" || ProcessInfo.processInfo.environment["CITO_BANNER_HEADLESS"] == "1"
let appPasteboard = usesPrivatePasteboard ? NSPasteboard.withUniqueName() : NSPasteboard.general
atexit { if usesPrivatePasteboard { appPasteboard.releaseGlobally() } }

enum CitoMetrics {
    static let gap: CGFloat = 6
    static let menuWidth: CGFloat = 380
    static let menuInset: CGFloat = 12
    static let menuRailWidth = menuWidth - 2 * menuInset
    static let settingsWidth: CGFloat = 520
    static let settingsInset: CGFloat = 20
    static let cardInset: CGFloat = 12
    static let headerInset: CGFloat = 14
}

func normalizedHubURL(_ raw: String) -> String {
    let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
    return trimmed.hasSuffix("://") ? trimmed : trimmed.replacingOccurrences(of: "/+$", with: "", options: .regularExpression)
}

func proposedHubName(_ raw: String, url: String, existing: [HubConfigItem], creating: Bool, original: String = "") -> String {
    let url = normalizedHubURL(url)
    let automatic = raw.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty
    var source = raw
    if automatic {
        let host = URLComponents(string:url)?.host ?? ""
        let labels = host.split(separator:".")
        if labels.count == 4 && labels.allSatisfy({ Int($0).map { (0...255).contains($0) } ?? false }) {
            let ip = labels.compactMap { Int($0) }
            source = ip[0] == 10 || ip[0] == 192 && ip[1] == 168 || ip[0] == 172 && (16...31).contains(ip[1]) ? "lan" : (ip[0] == 100 && (64...127).contains(ip[1]) ? "tailscale" : "remote")
        } else if !host.contains(":"), let first = labels.first(where: { $0.unicodeScalars.contains { $0.isASCII && CharacterSet.alphanumerics.contains($0) } }) {
            source = String(first)
        } else { source = "remote" }
    }
    var slug = ""
    for scalar in source.unicodeScalars {
        if scalar.isASCII && CharacterSet.alphanumerics.contains(scalar) {
            if slug.count < 32 { slug += String(scalar).lowercased() }
        } else if !slug.isEmpty && !slug.hasSuffix("-") && slug.count < 32 { slug += "-" }
    }
    slug = slug.trimmingCharacters(in:CharacterSet(charactersIn:"-"))
    if automatic {
        if slug.isEmpty { slug = "remote" }
        let base = slug
        var suffix = 2
        while slug == "local" || existing.contains(where:{(creating || $0.name != original) && $0.name == slug && normalizedHubURL($0.url) != url}) {
            let ending = "-\(suffix)"
            slug = String(base.prefix(32-ending.count)).trimmingCharacters(in:CharacterSet(charactersIn:"-")) + ending
            suffix += 1
        }
    }
    return automatic && !creating && !original.isEmpty ? original : slug
}

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

func formatStatusButton(_ button: NSButton, unreadCount: Int, image: NSImage?) {
    button.image = image
    let title = unreadCount > 0 ? " \(unreadCount)" : ""
    button.title = ""
    button.imagePosition = title.isEmpty ? .imageOnly : .imageLeading
    if !title.isEmpty {
        button.attributedTitle = NSAttributedString(string: title,
            attributes: [.font: NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .semibold)])
    }
}

/// Edge packing is the only authority for horizontal bar positions. Item widths
/// remain intrinsic; flexible content owns the interval between the two edges.
func layoutBar(in container: NSView, insetLeft: CGFloat = CitoMetrics.cardInset, insetRight: CGFloat = CitoMetrics.cardInset,
               y: CGFloat, height: CGFloat, spacing: CGFloat = CitoMetrics.gap,
               leading: [NSView] = [], flexible: NSView? = nil, trailing: [NSView] = []) {
    let leftItems = leading.filter { !$0.isHidden }
    let rightItems = trailing.filter { !$0.isHidden }
    for case let control as MenuControl in leftItems + rightItems where !control.label.isHidden
        && control.subviews.allSatisfy({ $0 === control.label || $0 === control.leadingSymbolView || $0 === control.leadingDotView || $0 === control.centerSymbolView }) {
        let id = control.identifier?.rawValue ?? ""
        // Address chips reserve copy feedback space; scope toggles keep a uniform width.
        let reservedWidth = id.hasPrefix("copy:") || id.hasPrefix("copy_remote:") || id.hasPrefix("scope:") ? control.frame.width : 0
        let pairedWidth = MenuControl.toggleWidth(id, font: control.label.font ?? .systemFont(ofSize:11,weight:.medium))
        control.frame.size.width = max(reservedWidth, pairedWidth, MenuControl.intrinsicWidth(control.label.stringValue,
            font:control.label.font ?? .systemFont(ofSize:11,weight:.medium),
            leadingSymbol:control.leadingSymbolView?.isHidden == false ? control.symbolName : nil,
            leadingDot:control.leadingDotView?.isHidden == false,
            paddingX: id.hasPrefix("copy:") || id.hasPrefix("copy_remote:") ? 3 : (id == "open_settings" ? 6 : (id.hasPrefix("scope:") ? 9 : (id == "open_uninstall_confirm" ? 12 : 11)))))
        control.layoutContents()
    }
    var left = insetLeft
    var right = container.bounds.width - insetRight
    func place(_ view: NSView, x: CGFloat) {
        view.frame.origin = NSPoint(x: x, y: y + (height - view.frame.height) / 2)
    }
    for view in leftItems { place(view, x: left); left += view.frame.width + spacing }
    for view in rightItems.reversed() { right -= view.frame.width; place(view, x: right); right -= spacing }
    if let view = flexible, !view.isHidden {
        view.frame = NSRect(x: left, y: y + (height - view.frame.height) / 2,
                            width: max(0, right - left), height: view.frame.height)
    }
}

enum PillAlignment { case leading, trailing }
@discardableResult
func mountPillStrip(in container: NSView, frame: NSRect, id: String, alignment: PillAlignment = .leading,
                    spacing: CGFloat = 4, pills: [NSView], activePredicate: (NSView) -> Bool) -> OverlayScrollView {
    let visible = pills.filter { !$0.isHidden }
    let totalWidth = visible.isEmpty ? 0 : visible.enumerated().reduce(CGFloat(4)) {
        $0 + $1.element.frame.width + ($1.offset > 0 ? spacing : 0)
    }
    let trackWidth = min(frame.width, max(totalWidth, 0))
    let scrollFrame = NSRect(x: alignment == .trailing ? frame.maxX - trackWidth : frame.minX,
                             y: frame.minY, width: trackWidth, height: frame.height)
    let scroll = OverlayScrollView(frame: scrollFrame)
    scroll.identifier = NSUserInterfaceItemIdentifier(id)
    scroll.scrollerStyle = .overlay; scroll.drawsBackground = false
    scroll.hasHorizontalScroller = false; scroll.hasVerticalScroller = false
    let doc = NSView(frame: NSRect(x: 0, y: 0, width: max(trackWidth, totalWidth), height: frame.height))
    var cursorX: CGFloat = 2
    for pill in visible {
        pill.frame.origin = NSPoint(x: cursorX, y: (frame.height - pill.frame.height) / 2)
        doc.addSubview(pill); cursorX += pill.frame.width + spacing
    }
    scroll.documentView = doc; container.addSubview(scroll)
    if let active = visible.first(where: activePredicate) { active.scrollToVisible(active.bounds) }
    return scroll
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
        NSColor(srgbRed: 0.61, green: 0.26, blue: 0.00, alpha: 1),
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
            color = surfaceBackground.blended(withFraction: fill?.alphaComponent ?? 0,
                of: fill?.withAlphaComponent(1) ?? .clear) ?? surfaceBackground
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
    // Opaque badge fills guarantee white-symbol contrast in both appearances.
    static func badgeFill(_ name: String) -> NSColor {
        switch name.lowercased() {
        case "green", "human": return NSColor(srgbRed:0.04,green:0.43,blue:0.16,alpha:1)
        case "teal": return NSColor(srgbRed:0.00,green:0.45,blue:0.48,alpha:1)
        case "orange", "amber", "yellow": return NSColor(srgbRed:0.66,green:0.36,blue:0.00,alpha:1)
        case "red": return destructiveFill
        case "pink": return NSColor(srgbRed:0.72,green:0.18,blue:0.39,alpha:1)
        case "purple", "indigo": return NSColor(srgbRed:0.40,green:0.29,blue:0.72,alpha:1)
        case "gray": return NSColor(srgbRed:0.43,green:0.44,blue:0.46,alpha:1)
        default: return prominentFill
        }
    }
}

class SurfaceView: NSView {
    var fillColor: NSColor { didSet { needsDisplay = true; applyLayerColors() } }
    var strokeColor: NSColor? { didSet { needsDisplay = true; applyLayerColors() } }
    var strokeWidth: CGFloat = 1 { didSet { applyLayerColors() } }
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
            layer?.borderWidth = strokeColor == nil ? 0 : strokeWidth
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
func statusLabel(_ text: String, frame: NSRect, color: NSColor, filled: Bool = true, size: CGFloat = 11) -> SurfaceView {
    var frame=frame
    frame.size.width=min(frame.width,ceil(measureTextWidth(text,font:.systemFont(ofSize:size,weight:.medium)))+16)
    let row=roundedView(frame,radius:0,color:.clear)
    let dot=roundedView(NSRect(x:0,y:floor((frame.height-6)/2),width:6,height:6),radius:3,color:filled ? color : .clear,border:filled ? nil : color)
    let label=textLabel(text,NSRect(x:12,y:0,width:max(0,frame.width-12),height:frame.height),size:size,weight:.medium,color:color)
    label.lineBreakMode = .byTruncatingMiddle
    row.addSubview(dot); row.addSubview(label); row.toolTip=text
    row.setAccessibilityElement(true); row.setAccessibilityRole(.staticText); row.setAccessibilityLabel(text)
    return row
}
func measureTextWidth(_ text: String, font: NSFont) -> CGFloat {
    let field = NSTextField(labelWithString: text)
    field.font = font
    return ceil(field.fittingSize.width)
}
/// Compact only the chip presentation; callers retain the full address for copying.
func compactAddressLabel(_ address: String, maxWidth: CGFloat, font: NSFont, paddingX: CGFloat) -> String {
    if MenuControl.intrinsicWidth(address,font:font,leadingSymbol:"doc.on.doc",paddingX:paddingX) <= maxWidth { return address }
    for count in stride(from:max(1,address.count-1),through:1,by:-1) {
        let text = String(address.prefix((count+1)/2)) + "…" + String(address.suffix(count/2))
        if MenuControl.intrinsicWidth(text,font:font,leadingSymbol:"doc.on.doc",paddingX:paddingX) <= maxWidth { return text }
    }
    return "…"
}

func compactSessionPath(_ shortCwd: String, branchSuffix: String, maxWidth: CGFloat = 244,
                        font: NSFont = .systemFont(ofSize: 10.5)) -> String {
    let full = shortCwd + branchSuffix
    guard measureTextWidth(full, font: font) > maxWidth else { return full }
    let segments = shortCwd.split(separator: "/")
    guard segments.count >= 3, let leaf = segments.last else { return full }
    let prefix = shortCwd.hasPrefix("~") ? "~/…/" : "/…/"
    let parentAndLeaf = prefix + segments.suffix(2).joined(separator: "/") + branchSuffix
    if measureTextWidth(parentAndLeaf, font: font) <= maxWidth { return parentAndLeaf }
    return prefix + leaf + branchSuffix
}

func activityRowHeight(_ text: String, selected: Bool) -> CGFloat {
    let font = NSFont.systemFont(ofSize: 12)
    guard selected && (measureTextWidth(text, font: font) > 492 || text.contains("\n")) else { return 60 }
    let wrappedRect = (text as NSString).boundingRect(with: NSSize(width: 492, height: CGFloat.greatestFiniteMagnitude),
        options: [.usesLineFragmentOrigin, .usesFontLeading], attributes: [.font: font])
    return min(108, max(60, 44 + ceil(wrappedRect.height)))
}

func cleanHostDisplay(_ raw: String) -> String {
    raw.replacingOccurrences(of: "\\.local\\b", with: "", options: [.regularExpression, .caseInsensitive])
}
func isGenericSessionLabel(_ raw: String, user: String, homeBasename: String = (NSHomeDirectory() as NSString).lastPathComponent) -> Bool {
    let label = raw.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    let identity = label.hasPrefix("@") ? String(label.dropFirst()) : label
    let homeBasename = homeBasename.lowercased()
    return label.isEmpty || identity == user.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() || identity == homeBasename
        || label == "you (@\(user.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()))"
        || ["agent", "agent session", "session", "cli session", "human", "human node", "human channel", "human receiver", "you", "coding session", "default"].contains(label)
}
func nodeDisplayTitle(_ title: String, harness: String, user: String, about: String? = nil, harnessName: String? = nil, customTitle: Bool = false) -> String {
    if customTitle { return title }
    let raw = title.trimmingCharacters(in: .whitespacesAndNewlines)
    if harness == "human" && (raw.isEmpty || raw == "Human" || raw == "@\(user)" || raw == "@\(user) (Me)" || raw.hasPrefix("Human Node")) {
        return "You (@\(user))"
    }
    // A meaningful title remains authoritative even when its session lives at home.
    if harness != "human" && isGenericSessionLabel(raw, user: user) {
        return distinctAbout(about, title: raw, user: user) ?? (harnessName ?? harness)
    }
    return title
}
func presentationError(_ raw: String) -> String {
    var message = raw.replacingOccurrences(of: "^([A-Z0-9_]+:\\s*)+", with: "", options: .regularExpression)
        .replacingOccurrences(of: "\\s*\\(os error [0-9]+\\)$", with: "", options: .regularExpression)
    if message.hasPrefix("CITO_NATS_URL") { return "Enter a valid Hub URL (http://ip:4291 or https://host)." }
    let bindPrefix = "Cannot bind host hub listener: "
    if message.hasPrefix(bindPrefix) {
        let reason = String(message.dropFirst(bindPrefix.count))
        return "Cannot start sharing: " + reason
    }
    for (token, copy) in [("CITO_OIDC_ISSUER", "OIDC issuer URL"), ("CITO_OIDC_CLIENT_ID", "OIDC client ID"), ("CITO_HUB_TOKEN", "shared access token"), ("CITO_AUTH_MODE", "access mode"), (" (http://127.0.0.1 is allowed for tests)", "")] { message = message.replacingOccurrences(of: token, with: copy) }
    return message
}
func distinctAbout(_ raw: String?, title: String, user: String, otherTitle: String? = nil) -> String? {
    let value = (raw ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
    // Descriptions use the daemon/UI process HOME; discovered folder titles use the account home.
    return isGenericSessionLabel(value, user:user, homeBasename:((ProcessInfo.processInfo.environment["HOME"] ?? "") as NSString).lastPathComponent)
        || [title, otherTitle ?? ""].contains { value.lowercased() == $0.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() } ? nil : value
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
func resolveHarnessVisual(_ harness: String, harnesses: [String: HarnessMeta]) -> HarnessVisual {
    if let meta = harnesses[harness] {
        return HarnessVisual(
            id: meta.id,
            name: meta.display_name,
            symbol: meta.sf_symbol,
            tint: CitoPalette.badgeFill(meta.tint),
            tier: meta.tier,
            order: meta.order
        )
    }
    if harness == "human" {
        return HarnessVisual(
            id: "human",
            name: "Human",
            symbol: "person.crop.circle.fill",
            tint: CitoPalette.badgeFill("human"),
            tier: "stable",
            order: -1
        )
    }
    return HarnessVisual(
        id: harness,
        name: harness.capitalized,
        symbol: "bolt.horizontal.fill",
        tint: CitoPalette.badgeFill("blue"),
        tier: "custom",
        order: 3
    )
}
private var pluginIconCache: [String: NSImage] = [:]
func pluginIconImage(for id: String) -> NSImage? {
    if let image = pluginIconCache[id] { return image }
    let env = ProcessInfo.processInfo.environment
    func rootURL(_ raw: String) -> URL? {
        let path = raw.trimmingCharacters(in:.whitespacesAndNewlines)
        guard !path.isEmpty else { return nil }
        let home = env["HOME"] ?? NSHomeDirectory()
        let expanded = path == "~" ? home : path.hasPrefix("~/") ? home + String(path.dropFirst()) : path
        return URL(fileURLWithPath:expanded)
    }
    var roots = (env["CITO_PLUGIN_PATH"] ?? "").split(separator: ":").compactMap { rootURL(String($0)) }
    let state = rootURL(env["CITO_STATE_DIR"] ?? NSHomeDirectory()+"/.cito") ?? URL(fileURLWithPath:NSHomeDirectory()+"/.cito")
    roots += [state.appendingPathComponent("plugins"),state.appendingPathComponent("runtime/plugins")]
    if let builtin = env["CITO_BUILTIN_PLUGINS_DIR"].flatMap(rootURL) { roots.append(builtin) }
    let executable = URL(fileURLWithPath:CommandLine.arguments[0]).resolvingSymlinksInPath().deletingLastPathComponent()
    roots += [executable.appendingPathComponent("../Resources/plugins"),executable.appendingPathComponent("../../plugins")]
    for root in roots {
        if let image = NSImage(contentsOf:root.appendingPathComponent(id).appendingPathComponent("icon.png")) {
            pluginIconCache[id] = image
            return image
        }
    }
    return nil
}

func configureBadge(_ badge: SurfaceView, icon: NSImageView, id: String, symbol: String, title: String, tint: NSColor, inset: CGFloat) {
    if let image = pluginIconImage(for:id) {
        badge.fillColor = .clear
        badge.strokeColor = nil
        badge.layer?.masksToBounds = true
        icon.frame = badge.bounds
        icon.imageScaling = .scaleProportionallyUpOrDown
        icon.image = image
        icon.identifier = NSUserInterfaceItemIdentifier("plugin_icon:\(id)")
        icon.contentTintColor = nil
    } else {
        badge.fillColor = tint
        badge.strokeColor = .white.withAlphaComponent(0.18)
        icon.frame = badge.bounds.insetBy(dx:inset,dy:inset)
        icon.image = NSImage(systemSymbolName:symbol,accessibilityDescription:title)
        icon.identifier = NSUserInterfaceItemIdentifier("plugin_symbol:\(id)")
        icon.contentTintColor = .white
    }
}

func makeHarnessBadge(_ frame: NSRect, radius: CGFloat, iconInset: CGFloat, harness: String, harnesses: [String: HarnessMeta]) -> SurfaceView {
    let visual = resolveHarnessVisual(harness, harnesses: harnesses)
    let badge = roundedView(frame, radius: radius, color: visual.tint,
        border: .white.withAlphaComponent(0.18))
    let icon = NSImageView(frame: badge.bounds.insetBy(dx: iconInset, dy: iconInset))
    configureBadge(badge,icon:icon,id:harness,symbol:visual.symbol,title:visual.name,tint:visual.tint,inset:iconInset)
    badge.addSubview(icon)
    return badge
}

func makePillBadge(_ text: String, id: String, height: CGFloat = 20,
                   font: NSFont = .systemFont(ofSize: 9.5, weight: .semibold),
                   textColor: NSColor, fill: NSColor, border: NSColor? = nil, paddingX: CGFloat = 8, minWidth: CGFloat = 88) -> SurfaceView {
    let label = textLabel(text, .zero, color: textColor)
    label.font = font; label.alignment = .center
    let size = label.cell?.cellSize ?? NSSize(width: measureTextWidth(text, font: font), height: 12)
    let w = max(minWidth, ceil(size.width) + paddingX * 2)
    let h = ceil(size.height)
    let pill = roundedView(NSRect(x: 0, y: 0, width: w, height: height), radius: height / 2, color: fill, border: border)
    pill.identifier = NSUserInterfaceItemIdentifier(id)
    label.frame = NSRect(x: 0, y: floor((height - h) / 2), width: w, height: h)
    pill.addSubview(label)
    return pill
}

func panelShortcutKey(_ event: NSEvent) -> String? {
    let flags = event.modifierFlags.intersection([.command, .shift, .control, .option])
    guard let character = event.charactersIgnoringModifiers?.lowercased(),
          flags == [.command] || (flags == [.command, .shift] && character == "z"),
          ["v", "c", "x", "a", "z", "w", ","].contains(character) else { return nil }
    return (flags.contains(.shift) ? "shift-command-" : "command-") + (character == "," ? "comma" : character)
}

final class KeyPanel: NSPanel {
    var onKey: ((String) -> Void)?
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }
    @discardableResult
    override func makeFirstResponder(_ responder: NSResponder?) -> Bool {
        let accepted = super.makeFirstResponder(responder)
        if accepted {
            let editor = (responder as? NSTextField)?.currentEditor() as? NSTextView ?? firstResponder as? NSTextView
            editor?.inputContext?.activate()
        }
        return accepted
    }
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        if (firstResponder as? NSTextInputClient)?.hasMarkedText() == true { return super.performKeyEquivalent(with: event) }
        if let key = panelShortcutKey(event), let onKey = onKey { onKey(key); return true }
        return super.performKeyEquivalent(with: event)
    }
}

final class OverlayScrollView: NSScrollView {
    var nativeScrollerStyle: NSScroller.Style { super.scrollerStyle }
    override var scrollerStyle: NSScroller.Style {
        get { .overlay }
        set { super.scrollerStyle = .overlay }
    }
    override func tile() {
        if super.scrollerStyle != .overlay { super.scrollerStyle = .overlay }
        super.tile()
    }
}

/** Menu controls own their clicks so copy and scope changes keep native tracking open. */
final class MenuControl: NSControl {
    let label: NSTextField
    enum Style { case neutral, prominent, subtleHeader, destructiveHover, destructive }
    override var frame: NSRect { didSet { if oldValue.size != frame.size { layoutContents() } } }
    static func intrinsicWidth(_ title: String, font: NSFont = .systemFont(ofSize: 11, weight: .medium),
                               leadingSymbol: String? = nil, leadingDot: Bool = false, paddingX: CGFloat = 11, minWidth: CGFloat = 0) -> CGFloat {
        max(minWidth, ceil(measureTextWidth(title, font: font)) + paddingX * 2 + (leadingSymbol == nil ? 0 : 17) + (leadingDot ? 12 : 0))
    }
    static func toggleWidth(_ id: String, font: NSFont = .systemFont(ofSize:11,weight:.medium)) -> CGFloat {
        let states: [(String,String?)]
        let padding: CGFloat
        if id.hasPrefix("scope:") { states = [("Self","lock.fill"),("Team","person.2.fill")]; padding = 9 }
        else if id == "toggle_auto_listen" { states = [("Auto-Listen",nil),("Auto-Listen: On",nil)]; padding = 11 }
        else if id.hasPrefix("toggle_hub:") { states = [("Enable",nil),("Disable",nil)]; padding = 11 }
        else if id == "toggle_desktop_banner" { states = [("Enabled",nil),("Disabled",nil)]; padding = 11 }
        else if id == "toggle_host_hub" { states = [("Share",nil),("Sharing",nil),("Starting",nil),("Failed",nil)]; padding = 11 }
        else if id == "toggle_host_hub_auth" { states = [("Access: Trusted",nil),("Access: Token",nil)]; padding = 11 }
        else if id.hasPrefix("toggle_channel:") { states = [("Pause",nil),("Resume",nil)]; padding = 11 }
        else { return 0 }
        return states.map { intrinsicWidth($0.0,font:font,leadingSymbol:$0.1,leadingDot:id == "toggle_host_hub" || id == "toggle_desktop_banner",paddingX:padding) }.max() ?? 0
    }
    func layoutContents() {
        let font = label.font ?? .systemFont(ofSize: 11)
        let cellH = ceil(label.cell?.cellSize.height ?? (font.ascender - font.descender + font.leading))
        let labelY = floor((bounds.height - cellH) / 2)
        centerSymbolView?.frame = NSRect(x: floor((bounds.width - 12) / 2), y: floor((bounds.height - 12) / 2), width: 12, height: 12)
        if !label.isHidden, let dot=leadingDotView, !dot.isHidden {
            let textW=ceil(measureTextWidth(label.stringValue,font:font))+6
            let start=max(6,floor((bounds.width-textW-12)/2))
            dot.frame=NSRect(x:start,y:floor((bounds.height-6)/2),width:6,height:6)
            label.frame=NSRect(x:start+12,y:labelY,width:max(0,bounds.width-start-16),height:cellH)
        } else if !label.isHidden, let icon = leadingSymbolView, !icon.isHidden {
            // NSTextField needs its drawable insets beyond cellSize to avoid native ellipsis.
            let textW = max(0, min(bounds.width - 23, ceil(measureTextWidth(label.stringValue, font: font)) + 6))
            let id = identifier?.rawValue ?? ""
            let copyChip = id.hasPrefix("copy:") || id.hasPrefix("copy_remote:")
            let startX: CGFloat = label.alignment == .left ? (copyChip ? 0 : 6) : max(6, floor((bounds.width - 17 - textW) / 2))
            icon.frame = NSRect(x: startX, y: floor((bounds.height - 12) / 2), width: 12, height: 12)
            label.frame = NSRect(x: startX + 17, y: labelY, width: label.alignment == .left ? max(0, bounds.width - (copyChip ? 17 : 27)) : textW, height: cellH)
        } else {
            label.frame = NSRect(x: paddingX, y: labelY, width: max(0, bounds.width - 2 * paddingX), height: cellH)
        }
    }
    var paddingX: CGFloat = 4
    var leadingDotView: SurfaceView?
    private var dotColor: NSColor = CitoPalette.secondaryText
    private var dotRing = false
    func setLeadingDot(_ color: NSColor, ring: Bool = false) {
        dotColor=color; dotRing=ring
        if leadingDotView == nil {
            let dot=roundedView(NSRect(x:6,y:6,width:6,height:6),radius:3,color:color)
            addSubview(dot); leadingDotView=dot
        }
        setAccessibilityLabel(label.stringValue)
        layoutContents(); applyVisualState()
    }
    var leadingSymbolView: NSImageView?
    var centerSymbolView: NSImageView?
    var accessorySymbolView: NSImageView? { didSet { applyVisualState() } }
    var symbolName: String?
    var pressed: (() -> Void)?
    var style: Style = .neutral { didSet { layoutContents(); applyVisualState() } }
    var normalColor: NSColor = .labelColor { didSet { applyVisualState() } }
    var feedbackTimer: Timer?
    var restingTitle: String?
    private(set) var isHovered = false
    private var trackingArea: NSTrackingArea?
    static weak var activeHovered: MenuControl?
    private static var hoverWatchTimer: Timer?
    // Headless hooks exercise logical transitions without a visible window or physical pointer.
    static var tracksPhysicalPointer = true
    override var wantsUpdateLayer: Bool { true }
    init(_ title: String, frame: NSRect, symbol: String? = nil, leadingSymbol: String? = nil, color: NSColor = .labelColor, radius: CGFloat = 6, paddingX: CGFloat = 4) {
        self.paddingX = paddingX
        label = textLabel(title, .zero)
        symbolName = leadingSymbol ?? symbol
        super.init(frame: frame)
        wantsLayer = true; layer?.cornerRadius = radius
        layer?.cornerCurve = .continuous
        label.font = .systemFont(ofSize:11,weight:.medium)
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
        layoutContents()
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
            leadingSymbolView?.image = NSImage(systemSymbolName: name, accessibilityDescription: nil)
            leadingSymbolView?.isHidden = false
        } else {
            leadingSymbolView?.isHidden = true
        }
        layoutContents()
        applyVisualState()
    }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? {
        guard !isHidden && !isHiddenOrHasHiddenAncestor && alphaValue > 0.01 else { return nil }
        return frame.contains(point) ? self : nil
    }
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
        let hovered = hovered && isEnabled
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
    override var isEnabled: Bool { didSet { applyVisualState() } }
    func applyVisualState() {
        alphaValue = isEnabled ? 1 : 0.45
        let fill: NSColor
        let border: NSColor?
        let foreground: NSColor
        let primarySymbol: NSColor
        let accessorySymbol: NSColor
        switch style {
        case .neutral:
            fill = .labelColor.withAppearanceAlpha(isHovered ? 0.16 : 0.08)
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
            fill = isHovered ? CitoPalette.destructiveFill : .labelColor.withAppearanceAlpha(0.08)
            border = isHovered ? .white.withAlphaComponent(0.26) : .labelColor.withAppearanceAlpha(0.14)
            foreground = isHovered ? .white : normalColor
            primarySymbol = foreground; accessorySymbol = foreground
        case .destructive:
            fill = isHovered ? CitoPalette.destructiveFill : CitoPalette.dangerText.withAppearanceAlpha(0.14)
            border = isHovered ? .white.withAlphaComponent(0.28) : CitoPalette.dangerText.withAppearanceAlpha(0.38)
            foreground = isHovered ? .white : CitoPalette.dangerText
            primarySymbol = foreground; accessorySymbol = foreground
        }
        leadingDotView?.fillColor = dotRing ? .clear : (style == .prominent ? .white : dotColor)
        leadingDotView?.strokeColor = dotRing ? (style == .prominent ? .white : dotColor) : nil
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
        if feedbackTimer == nil { restingTitle = label.stringValue }
        feedbackTimer?.invalidate()
        leadingSymbolView?.isHidden = true
        label.stringValue = message; label.textColor = CitoPalette.successText
        layoutContents()
        let restore = Timer(timeInterval: 1.2, repeats: false) { [weak self] _ in
            guard let self = self else { return }
            if self.leadingSymbolView != nil {
                self.leadingSymbolView?.isHidden = false
            }
            self.feedbackTimer = nil
            self.label.stringValue = self.restingTitle ?? self.accessibilityLabel() ?? ""
            self.restingTitle = nil
            self.layoutContents()
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
        let cleanHost = cleanHostDisplay(from_host)
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
final class ClickView: SurfaceView {
    init(frame: NSRect) { super.init(frame:frame,radius:14,color:.clear,border:.labelColor.withAppearanceAlpha(0.22)) }
    required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }
    var hoverChanged: ((Bool) -> Void)?
    func resetHover() { hovered = false; applyBorderColor() }
    func setHovered(_ value: Bool) { hovered=value; applyBorderColor(); hoverChanged?(value) }
    var clicked: (() -> Void)?
    var appearanceChanged: (() -> Void)?
    private var hovered = false
    private var tracking: NSTrackingArea?
    private func applyBorderColor() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.borderColor = NSColor.labelColor.withAppearanceAlpha(hovered ? 0.28 : 0.22).cgColor
        }
    }
    override func updateLayer() { super.updateLayer(); applyBorderColor() }
    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applyBorderColor()
        appearanceChanged?()
    }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? {
        guard !isHidden && !isHiddenOrHasHiddenAncestor && alphaValue > 0.01 else { return nil }
        guard frame.contains(point) else { return nil }
        var child = super.hitTest(point)
        while let view = child, view !== self {
            if let control = view as? NSControl, !(control is NSTextField) || (control as? NSTextField)?.isEditable == true { return control }
            child = view.superview
        }
        return self
    }
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
        setHovered(true)
    }
    override func mouseExited(with event: NSEvent) {
        NSCursor.arrow.set()
        setHovered(false)
    }
}

final class ModalOverlay: NSView {
    override func hitTest(_ point: NSPoint) -> NSView? { guard frame.contains(point) else{return nil}; return super.hitTest(point) ?? self }
    override func mouseDown(with event: NSEvent) {}
}
final class FlippedView: NSView {
    override var isFlipped: Bool { true }
}

struct Hub: Decodable, Equatable {
    let name: String, status: String, auth_mode: String
    let user: String?, error: String?
    let enabled: Bool?, auto_listen: Bool?
    let human_paused: Bool?, human_about: String?
}
struct Listener: Decodable, Equatable {
    let hub: String, listener_id: String, public_id: String, scoped_id: String
    let harness: String, session_id: String, title: String, cwd: String, scope: String
    let about: String?, repo: String?, branch: String?
    let custom_title: Bool?
    let display_address: String
    let user: String?, owner_user: String?
    let host_name: String?
    let host_id: String?
    let channels: [String]?
}
struct Candidate: Decodable, Equatable {
    let harness: String, session_id: String, title: String, cwd: String
    let branch: String?, endpoint_pid: Int?, updated_at_ms: Double?
    let listening_hubs: [String]?
    let custom_title: Bool?
    let needs_reload: Bool?
    let reload_hint: String?
}
struct RemotePeer: Decodable, Equatable {
    let hub: String?
    let listener_id: String, public_id: String, scoped_id: String
    let user: String, host_id: String, host_name: String, harness: String
    let display_address: String
    let title: String, about: String, scope: String, last_seen_at: String
    var session_id: String? = nil
    var custom_title: Bool? = nil
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
    var from_listener_id: String? = nil
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
    var pairing_url: String? = nil
    var pairing_open_url_label: String? = nil
    var pairing_manual_disclosure: Bool? = nil
    var warning: Bool? = nil
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
    var tailscale_url: String? = nil
    var tailscale_invite_command: String? = nil
    var lan_url: String? = nil
    var lan_invite_command: String? = nil
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
    let auto_listen: Bool?
    let is_default: Bool
    let status: String
    let user: String?
    let error: String?
    let issuer: String?
    let client_id: String?
    var human_paused: Bool? = nil
    var human_about: String? = nil
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
    let warnings: [String]?
    let source_op: String?
    let request_id: String?
    let composer_error: String?
    let channel_id: String?
    let code: String?
    let instruction: String?
    let pairing_command: String?
    let pairing_url: String?
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
    let activity_all_total: Int?
    let activity_has_any: Bool?
    let activity_unread_count: Int?
    let activity_filter: String?
    let activity_query: String?
    let message: String?
    let part: Int?
    let complete: Bool?
}

// NSTableView retains row-selection hits for custom NSControl subclasses.
// Route embedded menu controls explicitly while preserving native row selection.
final class DirectoryTableView: NSTableView {
    override func hitTest(_ point: NSPoint) -> NSView? {
        let hit = super.hitTest(point)
        guard hit === self else { return hit }
        let local = convert(point, from:superview)
        let index = row(at:local)
        if index >= 0, let row = rowView(atRow:index,makeIfNecessary:false),
           let control = row.hitTest(local) as? MenuControl, control.isEnabled {
            return control
        }
        return hit
    }
}

func relativeTime(_ timestamp: String) -> String {
    let formatter = ISO8601DateFormatter()
    formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    let date = formatter.date(from: timestamp) ?? {
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.date(from: timestamp)
    }()
    guard let date = date else { return "Just now" }
    let seconds = max(0, Date().timeIntervalSince(date))
    if seconds < 60 { return "Just now" }
    if seconds < 3600 { return "\(Int(seconds / 60))m ago" }
    if seconds < 86400 { return "\(Int(seconds / 3600))h ago" }
    return "\(Int(seconds / 86400))d ago"
}

final class DirectoryPeerCellView: NSTableCellView {
    let badgeView: SurfaceView
    let unreadAccentBar = roundedView(.zero, radius: 1.5, color: CitoPalette.infoText)
    let rowDivider = roundedView(.zero, radius: 0, color: .labelColor.withAppearanceAlpha(0.05))
    let deleteControl = MenuControl("Delete message", frame: NSRect(x:520,y:30,width:24,height:22), symbol:"xmark")
    var inspectionId = ""
    private(set) var activityItem: ActivityItem?
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
        unreadAccentBar.identifier = NSUserInterfaceItemIdentifier("unread_accent_bar")
        rowDivider.identifier = NSUserInterfaceItemIdentifier("activity_row_divider")
        addSubview(unreadAccentBar); addSubview(rowDivider)
        deleteControl.style = .subtleHeader
        addSubview(deleteControl)
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
        activityItem = nil
        badgeView.frame.origin.y = 12
        unreadAccentBar.isHidden = true; deleteControl.isHidden = true; rowDivider.isHidden = true
        line1TitleLabel.font = .systemFont(ofSize:13,weight:.semibold)
        line1TitleLabel.textColor = .labelColor
        line2MetaLabel.font = .systemFont(ofSize:11.5)
        line2MetaLabel.textColor = CitoPalette.secondaryText
        msgControl.style = .neutral; copyControl.style = .neutral
        line2MetaLabel.maximumNumberOfLines = 1
        line2MetaLabel.lineBreakMode = .byTruncatingTail
        inspectionId = "dir_peer:\(peer.scoped_id)"
        isSelectedRow = selected
        frame = NSRect(x: 0, y: 0, width: 560, height: 52)
        applySelectionColor()
        let visual = resolveHarnessVisual(peer.harness, harnesses: harnesses)
        configureBadge(badgeView,icon:badgeIcon,id:peer.harness,symbol:visual.symbol,title:visual.name,tint:visual.tint,inset:6)

        let short = peer.display_address
        let copyFont = NSFont.monospacedSystemFont(ofSize: 11, weight: .medium)
        let shownAddress = compactAddressLabel(short,maxWidth:190,font:copyFont,paddingX:11)
        let copyWidth = min(190,max(MenuControl.intrinsicWidth("✓ Copied", font:copyFont, leadingSymbol:"doc.on.doc"), MenuControl.intrinsicWidth(shownAddress, font:copyFont, leadingSymbol:"doc.on.doc")))
        copyControl.feedbackTimer?.invalidate()
        copyControl.feedbackTimer = nil
        copyControl.restingTitle = nil
        copyControl.isHidden = false
        copyControl.setHovered(false)
        copyControl.frame = NSRect(x: 544 - MenuControl.intrinsicWidth("Message", leadingSymbol:"bubble.left") - 6 - copyWidth, y: 15, width: copyWidth, height: 22)
        copyControl.setLeadingSymbol("doc.on.doc")
        copyControl.label.font = copyFont
        copyControl.label.stringValue = shownAddress
        copyControl.applyVisualState()
        copyControl.setAccessibilityLabel(short)
        copyControl.toolTip = shownAddress == short ? peer.scoped_id : short
        copyControl.identifier = NSUserInterfaceItemIdentifier("copy_dir:\(peer.scoped_id)")
        copyControl.pressed = { [weak copyControl] in
            if let control = copyControl { onCopy(peer.display_address, control) }
        }

        let msgWidth: CGFloat = MenuControl.intrinsicWidth("Message", leadingSymbol:"bubble.left")
        msgControl.isHidden = false
        msgControl.setHovered(false)
        msgControl.frame = NSRect(x: 544 - msgWidth, y: 15, width: msgWidth, height: 22)
        msgControl.setLeadingSymbol("bubble.left")
        msgControl.label.font = .systemFont(ofSize: 11, weight: .medium)
        msgControl.label.stringValue = "Message"
        msgControl.setAccessibilityLabel("Message")
        msgControl.toolTip = "Message \(peer.scoped_id)"
        msgControl.identifier = NSUserInterfaceItemIdentifier("msg_dir:\(peer.scoped_id)")
        msgControl.pressed = onMessage

        let availWidth = max(120, 544 - copyWidth - msgWidth - 66)
        let titleFont = NSFont.systemFont(ofSize: 13, weight: .semibold)
        let normalizedTitle = nodeDisplayTitle(peer.title, harness: peer.harness, user: peer.user, about: peer.about, harnessName: visual.name, customTitle: peer.custom_title == true)
        let displayTitle = peer.harness == "human" && normalizedTitle == "You (@\(peer.user))" ? "@\(peer.user)" + (peer.is_self_principal == true ? " (You)" : "") : normalizedTitle
        let titleW = min(availWidth, measureTextWidth(displayTitle, font: titleFont) + 4)
        line1TitleLabel.stringValue = displayTitle
        line1TitleLabel.frame = NSRect(x: 52, y: 26, width: titleW, height: 18)
        line1TitleLabel.identifier = NSUserInterfaceItemIdentifier("dir_title:\(peer.scoped_id)")

        let branch = (peer.branch ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        let branchSuffix = (!branch.isEmpty && branch != "HEAD") ? " · \(branch)" : ""
        let cwd = peer.cwd ?? ""
        let home = NSHomeDirectory()
        let shortCwd = cwd == home ? "~" : cwd.hasPrefix(home + "/") ? "~" + cwd.dropFirst(home.count) : cwd
        let context = (peer.repo ?? "").isEmpty ? (peer.harness == "human" ? "" : shortCwd) : peer.repo ?? ""
        let repoText = (context.isEmpty ? "" : " · \(context)") + branchSuffix
        line1RepoLabel.font = .systemFont(ofSize:11)
        line1RepoLabel.isHidden = false
        line1RepoLabel.stringValue = repoText
        line1RepoLabel.frame = NSRect(x: 52 + titleW, y: 26, width: max(0, availWidth - titleW), height: 18)
        line1RepoLabel.identifier = NSUserInterfaceItemIdentifier("dir_repo:\(peer.scoped_id)")

        let userText = "@\(peer.user)"
        let userFont = NSFont.systemFont(ofSize: 11.5, weight: .medium)
        let userW = min(availWidth, measureTextWidth(userText, font: userFont) + 2)
        line2UserLabel.isHidden = peer.harness == "human"
        line2UserLabel.stringValue = peer.harness == "human" ? "" : userText
        line2UserLabel.frame = NSRect(x: 52, y: 7, width: userW, height: 16)
        line2UserLabel.identifier = NSUserInterfaceItemIdentifier("dir_user:\(peer.scoped_id)")

        let internalTokens = ["mac", "hub", "human", "desktop notifications"]
        var hosts: [String] = []
        for raw in [peer.host_name] + (peer.channels ?? []) {
            let host = cleanHostDisplay(raw)
            if !host.isEmpty && !internalTokens.contains(host.lowercased()) && !hosts.contains(host) { hosts.append(host) }
        }
        let hostText = hosts.joined(separator: ", ")
        let displayAbout = distinctAbout(peer.about, title:peer.title, user:peer.user, otherTitle:displayTitle)
        let segments = ([peer.harness == "human" ? "Human" : "",hostText,displayAbout ?? ""]).filter { !$0.isEmpty }
        line2MetaLabel.stringValue = (peer.harness != "human" && !segments.isEmpty ? " · " : "") + segments.joined(separator:" · ")
        let metaOffset = peer.harness == "human" ? 0 : userW
        line2MetaLabel.frame = NSRect(x: 52 + metaOffset, y: 7, width: max(0, availWidth - metaOffset), height: 16)
        line2MetaLabel.identifier = NSUserInterfaceItemIdentifier("dir_meta:\(peer.scoped_id)")
        line2MetaLabel.toolTip = line2MetaLabel.stringValue
        copyControl.layoutContents(); msgControl.layoutContents()
    }

    func configureActivity(with item: ActivityItem, selected: Bool, harnesses: [String: HarnessMeta], onCopy: @escaping (MenuControl) -> Void, onDelete: @escaping () -> Void, onReply: @escaping () -> Void) {
        let keepCopyFeedback = activityItem?.hub == item.hub && activityItem?.signal_id == item.signal_id && copyControl.feedbackTimer?.isValid == true
        activityItem = item
        inspectionId = "activity_row:\(item.signal_id)"
        isSelectedRow = selected
        let rowHeight = activityRowHeight(item.message, selected: selected)
        let expanded = rowHeight > 60
        frame = NSRect(x:0,y:0,width:560,height:rowHeight)
        applySelectionColor()
        let visual = resolveHarnessVisual(item.from_harness ?? item.target_harness, harnesses:harnesses)
        badgeView.frame.origin.y = rowHeight - 40
        configureBadge(badgeView,icon:badgeIcon,id:item.from_harness ?? item.target_harness,symbol:visual.symbol,title:visual.name,tint:visual.tint,inset:6)
        let canReply = item.for_me && !(item.from_display_address ?? item.from_scoped_id ?? "").isEmpty
        let copyFont = NSFont.systemFont(ofSize: 11, weight: .medium)
        let copyW = max(66, MenuControl.intrinsicWidth("✓ Copied", font:copyFont, leadingSymbol:"doc.on.doc"))
        let copyX = 514 - copyW
        let replyW = MenuControl.intrinsicWidth("Reply", font:copyFont, leadingSymbol:"arrowshape.turn.up.left")
        let textW = (canReply ? copyX - 6 - replyW : copyX) - 58
        unreadAccentBar.isHidden = !item.unread
        unreadAccentBar.frame = NSRect(x:4,y:8,width:3,height:rowHeight-16)
        rowDivider.isHidden = false; rowDivider.frame = NSRect(x:0,y:0,width:560,height:1)
        deleteControl.isHidden = false
        deleteControl.frame = NSRect(x:520,y:rowHeight-26,width:24,height:22)
        deleteControl.identifier = NSUserInterfaceItemIdentifier("delete_activity:\(item.signal_id)")
        deleteControl.toolTip = "Delete message"
        deleteControl.pressed = onDelete
        msgControl.isHidden = !canReply
        msgControl.style = .neutral
        msgControl.frame = NSRect(x:copyX-6-replyW,y:rowHeight-26,width:replyW,height:22)
        msgControl.setLeadingSymbol("arrowshape.turn.up.left")
        msgControl.label.stringValue = "Reply"
        msgControl.setAccessibilityLabel("Reply")
        msgControl.toolTip = "Reply"
        msgControl.identifier = NSUserInterfaceItemIdentifier("reply_activity:\(item.signal_id)")
        msgControl.pressed = canReply ? onReply : nil
        copyControl.isHidden = false
        if !keepCopyFeedback {
            copyControl.feedbackTimer?.invalidate(); copyControl.feedbackTimer = nil
            copyControl.restingTitle = nil
            copyControl.label.stringValue = "Copy"
        }
        copyControl.style = .neutral
        copyControl.frame = NSRect(x:copyX,y:rowHeight-26,width:copyW,height:22)
        copyControl.setLeadingSymbol("doc.on.doc")
        copyControl.leadingSymbolView?.isHidden = keepCopyFeedback
        copyControl.label.font = copyFont
        copyControl.setAccessibilityLabel("Copy message")
        copyControl.toolTip = "Copy message"
        copyControl.identifier = NSUserInterfaceItemIdentifier("copy_activity_msg:\(item.signal_id)")
        copyControl.pressed = { [weak copyControl] in if let c = copyControl { onCopy(c) } }
        copyControl.applyVisualState()
        let held = ["stale_generation":" · Held (session reloaded)","driver_unavailable":" · Held (app unavailable)",
            "session_switched":" · Held (session changed)","unlistened":" · Held (listening paused)",
            "unlistened_before_dispatch":" · Held (listening paused)","fenced_before_dispatch":" · Held (connection changed)"][item.held_reason ?? ""] ?? ""
        let senderTitle = distinctAbout(item.from_title,title:"",user:item.from_user)
        line1TitleLabel.stringValue = "@\(item.from_user)" + (senderTitle.map { " (\($0))" } ?? "") + " → " + (item.target_harness == "human" ? "You" : item.target_title) + held
        line1TitleLabel.font = .systemFont(ofSize:12.5,weight:item.unread ? .semibold : .medium)
        line1TitleLabel.textColor = item.unread ? .labelColor : CitoPalette.secondaryText
        line1TitleLabel.frame = NSRect(x:52,y:rowHeight-24,width:max(0,textW),height:18)
        line1TitleLabel.identifier = NSUserInterfaceItemIdentifier("act_title:\(item.signal_id)")
        let time=relativeTime(item.created_at)
        let timeWidth=ceil(measureTextWidth(time,font:.systemFont(ofSize:10.5)))+4
        line1RepoLabel.isHidden=false; line1RepoLabel.stringValue=time
        line1RepoLabel.font = .systemFont(ofSize:10.5); line1RepoLabel.textColor=CitoPalette.secondaryText
        line1RepoLabel.frame=NSRect(x:52+textW-timeWidth,y:rowHeight-24,width:timeWidth,height:18)
        line1RepoLabel.identifier=NSUserInterfaceItemIdentifier("act_time:\(item.signal_id)")
        line1TitleLabel.frame.size.width=max(0,textW-timeWidth-6)
        line2UserLabel.isHidden = true
        line2MetaLabel.toolTip = item.message
        line2MetaLabel.stringValue = expanded ? item.message : item.message.replacingOccurrences(of:"\\s*\\n+\\s*",with:"  ",options:.regularExpression)
        line2MetaLabel.maximumNumberOfLines = expanded ? 4 : 1
        line2MetaLabel.lineBreakMode = expanded ? .byWordWrapping : .byTruncatingTail
        line2MetaLabel.font = .systemFont(ofSize:12)
        line2MetaLabel.textColor = item.unread ? .labelColor : CitoPalette.secondaryText
        let messageHeight = expanded ? rowHeight - 42 : 18
        line2MetaLabel.frame = NSRect(x:52,y:rowHeight - 34 - messageHeight,width:492,height:messageHeight)
        msgControl.layoutContents(); copyControl.layoutContents()
        line2MetaLabel.identifier = NSUserInterfaceItemIdentifier("act_msg:\(item.signal_id)")
    }
}

struct ChannelUiState {
    var editingGateway = false
    var editingPersonal = false
    var manualEntry = false
    var drafts: [String: String] = [:]
    var pendingPairRequest: String?
    var pairingInstruction = ""
    var pairingCommand: String?
    var pairingURL: String?
    var pairCopiedUntil = Date.distantPast
    var savedUntil: [String: Date] = [:]
    var testResult: (ok: Bool, detail: String)?

    mutating func resetPersonal() {
        editingPersonal = false
        manualEntry = false
        pendingPairRequest = nil
        pairingInstruction = ""
        pairingCommand = nil
        pairingURL = nil
        pairCopiedUntil = .distantPast
        savedUntil.removeValue(forKey: "user")
    }
    mutating func resetGateway() {
        editingGateway = false
        savedUntil.removeValue(forKey: "hub")
    }
    mutating func resetAll() { self = ChannelUiState() }
}

final class Companion: NSObject, NSMenuDelegate, NSSearchFieldDelegate, NSTextFieldDelegate, NSTableViewDataSource, NSTableViewDelegate {
    let headless = ProcessInfo.processInfo.environment["CITO_UI_HEADLESS"] == "1" || ProcessInfo.processInfo.environment["CITO_BANNER_HEADLESS"] == "1"
    let dismissMs = max(1, Int(ProcessInfo.processInfo.environment["CITO_BANNER_DISMISS_MS"] ?? "6000") ?? 6000)
    var status: NSStatusItem?
    var headlessStatusButton: NSButton?
    var panel: NSPanel?
    var labels: [NSTextField] = []
    var accentBar: SurfaceView?
    var iconBadge: SurfaceView?
    var bannerHovered = false
    var semanticPill: SurfaceView?
    var semanticPillLabel: NSTextField?
    var icon: NSImageView?
    var payload: Payload?
    var displayedCount = 0
    var visible = false
    var timer: DispatchWorkItem?
    var clickSource: DispatchSourceSignal?
    var hoverEventMonitor: Any?
    let cardMenu = NSMenu()
    var hubDrawerVisibility: [String: Bool] = [:]
    var menuScrollOffsets: [String: NSPoint] = [:]
    var menuScrollKeys: [String: String] = [:]
    var menuScrollAnchors: [String: (rowId: String, deltaY: CGFloat)] = [:]
    var statusImage: NSImage?
    var uniqueSessionCount: Int {
        Dictionary(grouping: listeners.filter { $0.harness != "human" && (effectiveTargetHub.isEmpty || $0.hub == effectiveTargetHub) }, by: { $0.harness })
            .values.reduce(0) { $0 + Set($1.map { $0.session_id }).count }
    }
    var humanListening: Bool { listeners.contains { $0.harness == "human" && (effectiveTargetHub.isEmpty || $0.hub == effectiveTargetHub) } }
    var wakeActionText = ""
    var testAppearance: NSAppearance?
    var hubs: [Hub] = []
    var listeners: [Listener] = []
    var available: [Candidate] = []
    var myRemote: [RemotePeer] = []
    var harnesses: [HarnessMeta] = []
    var harnessMap: [String: HarnessMeta] { Dictionary(uniqueKeysWithValues: harnesses.map { ($0.id, $0) }) }
    var selectedTargetHub: String = ""
    var renamingSessionKey: String?
    weak var renameField: NSTextField?
    var myRemoteCollapsed = false
    var collapsedGroups = Set<String>()
    var autoCollapsedRecentGroups = Set<String>()
    func statusColor(_ status: String) -> NSColor {
        status == "connected" ? CitoPalette.successText : status == "connecting" ? CitoPalette.warningText : status == "auth_required" ? CitoPalette.dangerText : .systemGray
    }
    var effectiveTargetHub: String {
        if !selectedTargetHub.isEmpty && hubs.contains(where: { $0.name == selectedTargetHub }) {
            return selectedTargetHub
        }
        if let defaultHub = settings?.hubs.first(where: { $0.is_default && $0.enabled && $0.status == "connected" }),
           hubs.contains(where: { $0.name == defaultHub.name }) { return defaultHub.name }
        return hubs.first(where: { $0.status == "connected" })?.name ?? hubs.first?.name ?? ""
    }
    var directoryWarnings: [String] = []
    var directoryQueryError: String?
    var directoryActionError: String?
    var directoryActionErrorSourceOp: String?
    var latestDirectoryActionRequestId: String?
    let directoryActionOps = ["delete_activity", "clear_activity_history", "ack_signals", "mark_all_activity_read", "copy_activity_message"]
    var directoryError: String? { directoryActionError ?? directoryQueryError }
    func clearDirectoryActionError() {
        let oldCopyError = directoryActionErrorSourceOp == "copy_activity_message" ? directoryActionError.map(presentationError) : nil
        directoryActionError = nil; directoryActionErrorSourceOp = nil
        if let oldCopyError = oldCopyError {
            if directorySummaryLabel?.toolTip == oldCopyError { directorySummaryLabel?.toolTip = nil }
            if directorySummaryLabel?.stringValue == oldCopyError {
                if directoryMode == "activity" { applyActivityFilter() }
                else { applyDirectoryFilter(fromDaemonFrame:false,daemonMatchedCount:nil) }
            }
        }
        updateDirectoryWarnings()
    }
    var directoryWarningLabel: NSTextField?
    func updateDirectoryWarnings() {
        let hasError = directoryError != nil
        let hasWarning = hasError || !directoryWarnings.isEmpty
        directoryWarningLabel?.isHidden = !hasWarning
        directoryWarningLabel?.stringValue = hasError ? (directoryMode == "activity" ? "Cannot update Activity. Please try again." : "Cannot refresh Directory. Showing the last available results.") : "Some Hubs are unavailable. Results may be incomplete."
        directoryWarningLabel?.toolTip = directoryError.map(presentationError) ?? directoryWarnings.map(presentationError).joined(separator:"\n")
        directoryWarningLabel?.textColor = hasError ? CitoPalette.dangerText : CitoPalette.secondaryText
        let bottom: CGFloat = directoryComposerBar?.isHidden == false ? 28 + (directoryComposerBar?.frame.height ?? 44) : 28
        directoryScrollView?.frame = NSRect(x:0,y:bottom,width:560,height:362-bottom-(hasWarning ? 28 : 0))
    }
    var directoryRequestId = ""
    var composerDraftIdempotencyKey = UUID().uuidString
    struct PendingMessageCopy {
        let requestId: String
        let hub: String
        let signalId: String
        var buffer = ""
        var nextPart = 0
    }
    var pendingMessageCopy: PendingMessageCopy?
    var activityTotal = 0
    var activityHasAny = false
    var activityUnreadCount = 0
    var serverActivityFilter: String?
    var serverActivityQuery = ""
    func copyText(_ text: String, control: MenuControl? = nil, feedback: String = "✓ Copied") {
        if let pending = pendingMessageCopy, latestDirectoryActionRequestId == pending.requestId { latestDirectoryActionRequestId = nil }
        pendingMessageCopy = nil
        clipboardString = text
        appPasteboard.clearContents()
        appPasteboard.setString(text, forType: .string)
        control?.copied(feedback)
    }
    func showMessageCopyError(_ message: String) {
        let ownsFeedback = latestDirectoryActionRequestId != nil && pendingMessageCopy?.requestId == latestDirectoryActionRequestId
        pendingMessageCopy = nil
        guard ownsFeedback else { return }
        directoryActionError = message; directoryActionErrorSourceOp = "copy_activity_message"
        updateDirectoryWarnings()
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
    var composerTargetPill: SurfaceView?
    var composerTargetLabel: NSTextField?
    var composerSendButton: MenuControl?
    var composerError = ""
    var composerInputField: NSTextField?
    var composerTarget = ""
    var composerTargetHub = ""
    var composerReplySignalId: String?
    var composerErrorLabel: NSTextField?
    func setComposerStatus(_ message: String) {
        composerError = presentationError(message)
        composerErrorLabel?.stringValue = composerError
        composerErrorLabel?.toolTip = composerError
        composerErrorLabel?.isHidden = composerError.isEmpty
        directoryComposerBar?.frame.size.height = composerError.isEmpty ? 44 : 62
        directoryComposerBar?.subviews.first { $0.identifier?.rawValue == "composer_divider" }?.frame.origin.y = (directoryComposerBar?.frame.height ?? 44)-1
        updateDirectoryWarnings()
    }
    struct PendingComposerSend {
        let text: String
        let requestId: String
        let target: String
        let hub: String
        let replySignalId: String?
    }
    var pendingComposer: PendingComposerSend? { didSet {
        composerSendButton?.feedbackTimer?.invalidate(); composerSendButton?.feedbackTimer=nil
        composerSendButton?.isEnabled = pendingComposer == nil
        composerSendButton?.label.stringValue = pendingComposer == nil ? "Send ⏎" : "Sending…"
        composerSendButton?.layoutContents()
    } }
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
    var inviteParseRequest = ""
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
    var channelUiStates: [String: ChannelUiState] = [:]
    var appCheckedUntil: [String: Date] = [:]
    var pendingAppChecks: [String: String] = [:]
    var hostHubAddress = "tailscale"
    var pendingChannelValues: [String: (hub: String, channelId: String, scope: String, values: [String: String])] = [:]
    var selectedChannelHub = ""
    var channelSettingsByHub: [String: [ChannelSetupItem]] = [:]
    var settingsScrollOffsets: [String: CGFloat] = [:]
    var pendingOnboardingMarker: String?
    var settingsKeyMonitor: Any?
    var effectiveChannelHub: String {
        let inventory = settings?.hubs.filter { $0.enabled != false }.map { $0.name } ?? hubs.filter { $0.enabled != false }.map { $0.name }
        if !selectedChannelHub.isEmpty { return inventory.contains(selectedChannelHub) ? selectedChannelHub : inventory.first ?? "local" }
        return settings?.hubs.first(where: { $0.is_default && $0.enabled })?.name ?? inventory.first ?? "local"
    }
    func clearChannelDrafts(_ channel: ChannelSetupItem, scope: String? = nil, hub: String? = nil) {
        let key = "\(hub ?? effectiveChannelHub)|\(channel.id)"
        for field in channel.fields where scope == nil || field.scope == scope {
            channelUiStates[key, default: ChannelUiState()].drafts.removeValue(forKey: field.key)
        }
    }
    func acceptSettings(_ value: SettingsPayload, hub: String? = nil, savedChannel: String? = nil, savedRequest: String? = nil, confirmHubSave: Bool = false) {
        settings = value
        if value.desktop_banner_enabled == false { hide(clicked:false) }
        if !selectedChannelHub.isEmpty && !value.hubs.contains(where: { $0.name == selectedChannelHub && $0.enabled }) {
            selectedChannelHub = value.hubs.first(where: { $0.enabled })?.name ?? ""
            if !selectedChannelHub.isEmpty { emit(["op": "open_settings", "hub": selectedChannelHub], userInitiated:false) }
        }
        if hostHubPortPending && savedRequest == submittedHostHubRequestId &&
           (value.host_hub.port == submittedHostHubPort || value.host_hub.error != nil) {
            hostHubPortPending = false
            hostHubPortSaved = value.host_hub.port == submittedHostHubPort && value.host_hub.error == nil && (hostHubPortDraft == nil || Int((hostHubPortDraft ?? "").trimmingCharacters(in:.whitespacesAndNewlines)) == submittedHostHubPort)
            if hostHubPortSaved { hostHubPortSavedUntil = Date().addingTimeInterval(1.2) }
            hostHubPortError = nil
            if hostHubPortSaved && Int((hostHubPortDraft ?? "").trimmingCharacters(in:.whitespacesAndNewlines)) == submittedHostHubPort { hostHubPortDraft = nil }
        }
        if confirmHubSave, let pending = pendingHubSave, savedRequest == pending.requestId,
           let saved = value.hubs.first(where: { $0.name == pending.name && $0.url == pending.url }) {
            pendingHubSave = nil
            addHubError = saved.error
            if saved.error == nil {
                if addHubDraftGeneration == pending.generation {
                    addHubFormOpen = false; addHubToken = ""
                } else if editingExistingHub || proposedHubName(addHubName,url:addHubUrl,existing:value.hubs,creating:false,original:pending.name) == pending.name {
                    // Newer edits to the acknowledged Hub are an explicit edit,
                    // not a new create that could overwrite a different URL.
                    editingExistingHub = true; editingOriginalHub = pending.name
                }
            }
        }
        let target = value.settings_hub ?? value.channel_hub ?? hub ?? effectiveChannelHub
        let previousChannels = channelSettingsByHub[target] ?? []
        channelSettingsByHub[target] = value.channels ?? []
        for channel in value.channels ?? [] {
            let key=target + "|" + channel.id
            var ui = channelUiStates[key] ?? ChannelUiState()
            if channel.configured && (previousChannels.contains(where: { $0.id == channel.id && !$0.configured }) || (!ui.pairingInstruction.isEmpty && (channel.pairing_instruction ?? "").isEmpty)) { ui.resetPersonal() }
            // An older acknowledgement cannot clear a newer draft or a failed
            // save, and test/pair result refreshes are not save acknowledgements.
            var acknowledged: (scope: String, values: [String: String], unchanged: Bool)?
            if savedChannel == channel.id, let savedRequest = savedRequest,
               let entry = pendingChannelValues[savedRequest], entry.hub == target && entry.channelId == channel.id {
                pendingChannelValues.removeValue(forKey: savedRequest)
                let unchanged = channel.fields.filter { $0.scope == entry.scope }.allSatisfy { field in
                    let draft = ui.drafts[field.key]
                    if let sent = entry.values[field.key] { return (draft ?? field.value ?? "") == sent }
                    return draft == nil || draft == (field.value ?? "")
                }
                acknowledged = (scope: entry.scope, values: entry.values, unchanged: unchanged)
                if unchanged {
                    if entry.scope == "user" && channel.configured { ui.resetPersonal() }
                    ui.savedUntil[entry.scope] = Date().addingTimeInterval(1.5)
                } else { ui.savedUntil.removeValue(forKey: entry.scope) }
                DispatchQueue.main.asyncAfter(deadline:.now()+1.5) { [weak self] in
                    if self?.settingsVisible == true { self?.rebuildSettingsPanel() }
                }
            }
            for field in channel.fields {
                if let sent = acknowledged?.values[field.key], ui.drafts[field.key] == sent {
                    ui.drafts.removeValue(forKey: field.key)
                } else if field.type != "secret" && ui.drafts[field.key] == (field.value ?? "") {
                    ui.drafts.removeValue(forKey: field.key)
                }
            }
            if acknowledged?.scope == "hub" && acknowledged?.unchanged == true && (channel.hub_provided || channel.status != "not_configured" || (channel.fields.contains { $0.scope == "hub" } && channel.fields.filter { $0.scope == "hub" }.allSatisfy { $0.configured })) {
                ui.editingGateway = false
            }
            ui.pairingInstruction = channel.pairing_instruction ?? ""
            ui.pairingCommand = channel.pairing_command ?? channel.pairing_code
            ui.pairingURL = channel.pairing_url
            channelUiStates[key] = ui
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
        renamingSessionKey = nil
        MenuControl.clearHoverWhere { $0.enclosingMenuItem != nil || ($0.window !== self.directoryPanel && $0.window !== self.settingsPanel) }
    }
    func ensureEditMainMenu() {
        let main = NSApp.mainMenu ?? NSMenu()
        if main.items.isEmpty {
            let appItem = NSMenuItem()
            let appMenu = NSMenu(title: "Cito")
            let quit = NSMenuItem(title: "Quit Cito", action: #selector(quitMenu), keyEquivalent: "q")
            quit.target = self; appMenu.addItem(quit)
            appItem.submenu = appMenu; main.addItem(appItem)
        }
        if !main.items.contains(where: { $0.submenu?.title == "Edit" }) {
            let item = NSMenuItem(title: "Edit", action: nil, keyEquivalent: "")
            let edit = NSMenu(title: "Edit")
            for (title, action, key) in [("Undo", "undo:", "z"), ("Redo", "redo:", "Z"),
                ("Cut", "cut:", "x"), ("Copy", "copy:", "c"), ("Paste", "paste:", "v"), ("Select All", "selectAll:", "a")] {
                let command = NSMenuItem(title: title, action: NSSelectorFromString(action), keyEquivalent: key)
                command.keyEquivalentModifierMask = title == "Redo" ? [.command, .shift] : [.command]
                edit.addItem(command)
            }
            item.submenu = edit; main.addItem(item)
        }
        if NSApp.mainMenu == nil { NSApp.mainMenu = main }
    }

    func handleTextEditShortcut(_ key: String, in window: NSWindow?) -> Bool {
        guard let editor = window?.firstResponder as? NSTextView else { return false }
        guard editor.isEditable, !editor.hasMarkedText() else { return false }
        let field = editor.delegate as? NSTextField
        func changeText(_ change: () -> Void) {
            editor.allowsUndo = true
            editor.breakUndoCoalescing()
            let undo = editor.undoManager
            let groupsByEvent = undo?.groupsByEvent ?? true
            // Own only this gesture's group; never close AppKit's event group directly.
            undo?.groupsByEvent = false
            undo?.beginUndoGrouping()
            defer {
                editor.breakUndoCoalescing()
                undo?.endUndoGrouping()
                undo?.groupsByEvent = groupsByEvent
            }
            change()
        }
        switch key.lowercased() {
        case "command-v":
            // The pasteboard remains authoritative after a copy in another application.
            let systemText = appPasteboard.string(forType: .string) ?? ""
            let text = systemText.isEmpty && headless ? clipboardString : systemText
            clipboardString = text
            changeText { editor.insertText(text, replacementRange: editor.selectedRange()) }
        case "command-a": editor.selectAll(nil); return true
        case "command-c", "command-x":
            guard !(field is NSSecureTextField) else { return true }
            let range = editor.selectedRange()
            guard range.length > 0 else { return true }
            copyText((editor.string as NSString).substring(with: range))
            if key.lowercased() == "command-c" { return true }
            changeText {
                if editor.shouldChangeText(in: range, replacementString: "") {
                    editor.replaceCharacters(in: range, with: "")
                    editor.didChangeText()
                }
            }
        case "command-z": editor.breakUndoCoalescing(); editor.undoManager?.undo()
        case "shift-command-z": editor.breakUndoCoalescing(); editor.undoManager?.redo()
        default: return false
        }
        if let field = field, field.stringValue != editor.string {
            field.validateEditing()
            controlTextDidChange(Notification(name: NSControl.textDidChangeNotification, object: field))
        }
        return true
    }

    func start() {
        ensureEditMainMenu()
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
            if let key = panelShortcutKey(event), self.handleTextEditShortcut(key, in: window) { return nil }
            // Text Services must see navigation and commit keys before our shortcuts.
            if window?.firstResponder is NSTextView && [126,125,36,76,53,51,117].contains(Int(event.keyCode)) { return event }
            let command = event.modifierFlags.contains(.command)
            if command && event.charactersIgnoringModifiers == "," { self.openSettingsPanel(); return nil }
            if command && event.charactersIgnoringModifiers == "k" { self.openDirectoryPanel(mode:"directory"); return nil }
            let key: String?
            if command && event.charactersIgnoringModifiers == "w" { key = "command-w" }
            else { key = [53:"escape",126:"up",125:"down",36:"return",76:"return",51:"delete",117:"delete"][Int(event.keyCode)] }
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
                formatStatusButton(button, unreadCount: 0, image: statusImage)
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
                if kind == "banner_material" { panel?.contentView?.subviews.compactMap { $0 as? NSVisualEffectView }.first?.material = .hudWindow; return }
                if kind == "reset" { panel?.contentView?.subviews.compactMap { $0 as? NSVisualEffectView }.first?.material = .popover; rebuild(); if settingsPanel != nil { rebuildSettingsPanel() }; updateDirectoryFacetStyles(); return }
                let roots = cardMenu.items.compactMap { $0.view } + [settingsPanel?.contentView,directoryPanel?.contentView].compactMap { $0 }
                func control(_ id: String) -> MenuControl? {
                    for root in roots { if let c = findControl(in:root,id:id) { return c } }
                    return nil
                }
                if let c = control("quit") {
                    if kind == "truncated_labels" { c.label.stringValue = String(repeating:"Wide chrome ",count:20) }
                    if kind == "jargon_leaks" { c.label.stringValue = object["text"] as? String ?? "Human receiver" }
                    if kind == "fractional_truncation", let cell = c.label.cell {
                        let slack = cell.drawingRect(forBounds:c.label.bounds).width - cell.cellSize.width
                        c.label.frame.size.width -= max(0,slack) + 0.25
                    }
                    if kind == "status_emoji", let button = status?.button ?? headlessStatusButton { button.attributedTitle = NSAttributedString(string: "🔔3") }
                    if kind == "low_contrast_labels" { c.normalColor = CitoPalette.surfaceBackground.withAlphaComponent(0.01); c.applyVisualState() }
                }
                if kind == "bar_anchor", let c = control(object["id"] as? String ?? "quit") { c.frame.origin.x -= 24 }
                if kind == "card_padding", let card = roots.flatMap({$0.subviews}).first(where:{$0 is SurfaceView && ($0.layer?.borderWidth ?? 0)>0 && $0.frame.width>=60 && $0.frame.height>=32}), let child = card.subviews.first(where:{!$0.isHidden && !($0 is NSVisualEffectView)}) { child.frame.origin.x = 2 }
                if kind == "address_mismatches", let listener = listeners.first { control("copy:\(listener.scoped_id)")?.label.stringValue = "wrong address" }
                let row = object["row"] as? Int ?? 0
                if kind == "activity_jargon", let table = directoryTableView, row >= 0, row < table.numberOfRows,
                   let cell = table.view(atColumn:0,row:row,makeIfNecessary:true) as? DirectoryPeerCellView {
                    cell.line1TitleLabel.stringValue = object["text"] as? String ?? "@bob → Human Node"
                }
                if kind == "banner_jargon", labels.count > 1 {
                    visible = true; timer?.cancel()
                    labels[0].stringValue = "[Passive] " + labels[0].stringValue
                }
                if kind == "hit_test", let target = control("open_settings"), let parent = target.superview {
                    let blocker = NSView(frame:target.frame)
                    blocker.identifier = NSUserInterfaceItemIdentifier("hit_test_probe")
                    parent.addSubview(blocker)
                }
                if let header = roots.first(where: { $0.identifier?.rawValue == "header" }) {
                    if kind == "strip_anchor", let scroll = header.subviews.first(where: { $0.identifier?.rawValue == "header_hub_strip" }) as? NSScrollView {
                        for pill in scroll.documentView?.subviews ?? [] { pill.frame.origin.x -= 8 }
                    }
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
            if op == "test_scroll", let id = object["id"] as? String, let offset = object["offset"] as? Double {
                func scroll(_ view: NSView) {
                    if let target = view as? NSScrollView, target.identifier?.rawValue == id {
                        target.contentView.scroll(to:NSPoint(x:0,y:CGFloat(offset)))
                        target.reflectScrolledClipView(target.contentView)
                    }
                    for child in view.subviews { scroll(child) }
                }
                for root in cardMenu.items.compactMap({ $0.view }) + [directoryPanel?.contentView].compactMap({ $0 }) { scroll(root) }
                return
            }
            if op == "test_tile_scroll_views" {
                func tile(_ view: NSView) {
                    if let scroll = view as? OverlayScrollView { scroll.scrollerStyle = .legacy; scroll.tile() }
                    for child in view.subviews { tile(child) }
                }
                for root in cardMenu.items.compactMap({ $0.view }) + [settingsPanel?.contentView, directoryPanel?.contentView].compactMap({ $0 }) { tile(root) }
                return
            }
            if op == "test_menu_open" { menuWillOpen(cardMenu); return }
            if op == "test_menu_close" { menuDidClose(cardMenu); return }
            if op == "test_appearance" { return }
            if op == "test_hover", let id = object["id"] as? String, let hovered = object["hovered"] as? Bool {
                if id == "banner_root" { (panel?.contentView as? ClickView)?.setHovered(hovered); return }
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
            if ["test_focus_field", "test_settings_input"].contains(op), let id = object["id"] as? String, id.hasPrefix("channel_field:") {
                let parts = id.split(separator: ":")
                if parts.count > 1 { channelUiStates["\(effectiveChannelHub)|\(parts[1])", default: ChannelUiState()].editingPersonal = true; rebuildSettingsPanel() }
            }
            if op == "test_focus_field", let id = object["id"] as? String {
                if id == "directory_table" { directoryPanel?.makeFirstResponder(directoryTableView); return }
                for window in [settingsPanel, directoryPanel].compactMap({ $0 }) {
                    if let root = window.contentView, let field = findTextField(in: root, id: id), field.isEditable, field.isEnabled {
                        window.makeFirstResponder(field)
                        field.selectText(nil)
                        return
                    }
                }
                diagnostic("editable field not found: \(id)"); return
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
                if let field = settingsFieldControls[id] ?? (settingsPanel?.contentView.flatMap { findTextField(in: $0, id: id) }) ?? cardMenu.items.compactMap({ $0.view }).compactMap({ findTextField(in: $0, id: id) }).first {
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
            if op == "test_menu_key", let key = object["key"] as? String, let field = renameField {
                let editor = NSTextView(); editor.string = field.stringValue
                _ = control(field, textView: editor, doCommandBy: key == "escape" ? #selector(NSResponder.cancelOperation(_:)) : #selector(NSResponder.insertNewline(_:)))
                return
            }
            if ["test_settings_key", "test_directory_key"].contains(op), let key = object["key"] as? String {
                let window = op == "test_settings_key" ? settingsPanel : directoryPanel
                if object["via"] as? String == "panel" {
                    let character = key.hasSuffix("comma") ? "," : String(key.suffix(1))
                    let flags: NSEvent.ModifierFlags = key.hasPrefix("shift-") ? [.command, .shift] : [.command]
                    if let event = NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: flags, timestamp: 0, windowNumber: window?.windowNumber ?? 0, context: nil, characters: character, charactersIgnoringModifiers: character, isARepeat: false, keyCode: 0) {
                        _ = window?.performKeyEquivalent(with: event)
                    }
                    return
                }
                if op == "test_settings_key" { handleSettingsKey(key) } else { handleDirectoryKey(key) }
                return
            }
            if op == "test_expire_copy_feedback", let id = object["id"] as? String {
                let roots = cardMenu.items.compactMap { $0.view } + [settingsPanel?.contentView, directoryPanel?.contentView].compactMap { $0 }
                for root in roots {
                    if let control = findControl(in: root, id: id), let timer = control.feedbackTimer {
                        timer.fire(); timer.invalidate()
                        return
                    }
                }
                return
            }
            if op == "test_control", let requestedId = object["id"] as? String {
                let id = requestedId.hasPrefix("target_hub:") ? "hub:" + requestedId.dropFirst("target_hub:".count) : requestedId
                if let root=panel?.contentView, let control=findControl(in:root,id:id) { _ = control.accessibilityPerformPress(); return }
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
        if let object=(try? JSONSerialization.jsonObject(with:data)) as? [String:Any], object["op"] as? String == "hub_invite" {
            guard !inviteParseRequest.isEmpty, object["request_id"] as? String == inviteParseRequest, let cfg=object["config"] as? [String:Any] else{return}
            inviteParseRequest=""; addHubError=nil
            addHubUrl=cfg["url"] as? String ?? ""; addHubName=cfg["name"] as? String ?? ""
            addHubAuth=cfg["authMode"] as? String ?? cfg["auth_mode"] as? String ?? "none"
            addHubToken=cfg["token"] as? String ?? ""; addHubUser=cfg["user"] as? String ?? ""
            addHubIssuer=cfg["issuer"] as? String ?? cfg["oidcIssuer"] as? String ?? ""; addHubClientId=cfg["clientId"] as? String ?? cfg["client_id"] as? String ?? cfg["oidcClientId"] as? String ?? ""
            settingsFieldControls.removeAll(); rebuildSettingsPanel(); return
        }
        guard let frame = try? JSONDecoder().decode(Frame.self, from: data) else { diagnostic("invalid frame"); return }
        if frame.ok == false || frame.op == "error", let requestId = frame.request_id {
            pendingChannelValues.removeValue(forKey: requestId)
            for key in Array(channelUiStates.keys) where channelUiStates[key]?.pendingPairRequest == requestId {
                channelUiStates[key]?.pendingPairRequest = nil
            }
        }
        let actionFailure = (frame.op == "error" || frame.ok == false) && directoryActionOps.contains(frame.source_op ?? "")
        if actionFailure, let requestId = frame.request_id, !requestId.isEmpty,
           requestId != latestDirectoryActionRequestId {
            if requestId == pendingMessageCopy?.requestId { pendingMessageCopy = nil }
            return
        }
        if ["state", "directory"].contains(frame.op), frame.ok != false, frame.error == nil,
           let latest = latestDirectoryActionRequestId, frame.request_id == latest {
            clearDirectoryActionError()
            latestDirectoryActionRequestId = nil
        }
        if frame.op == "activity_message" {
            guard var pending = pendingMessageCopy, frame.request_id == pending.requestId else { return }
            if let message = frame.message, frame.part == pending.nextPart, frame.error == nil, frame.ok != false {
                pending.buffer += message; pending.nextPart += 1
                if frame.complete != false {
                    if frame.request_id == latestDirectoryActionRequestId { clearDirectoryActionError() }
                    let index = directoryMode == "activity" ? activityFilteredItems.firstIndex { $0.hub == pending.hub && $0.signal_id == pending.signalId } : nil
                    let cell = index.flatMap { directoryTableView?.view(atColumn:0,row:$0,makeIfNecessary:false) as? DirectoryPeerCellView }
                    let sameItem = cell?.activityItem?.hub == pending.hub && cell?.activityItem?.signal_id == pending.signalId
                    copyText(pending.buffer, control:sameItem ? cell?.copyControl : nil)
                }
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
            pendingComposer = nil
            if frame.ok == true {
                if composerTarget == pending.target && composerTargetHub == pending.hub && composerReplySignalId == pending.replySignalId && composerInputField?.stringValue == pending.text { closeComposer() }
                else { setComposerStatus(""); composerSendButton?.copied("✓ Sent") }
            } else {
                setComposerStatus(frame.error ?? "Send failed. Your draft is preserved.")
            }
            return
        }
        if let hasAny=frame.activity_has_any { activityHasAny=hasAny }
        if frame.op == "error" || actionFailure {
            if let request = frame.request_id { pendingAppChecks.removeValue(forKey: request) }
            let sourceOp = frame.source_op ?? ""
            if sourceOp == "paste_hub_invite", frame.request_id != nil {
                if frame.request_id == inviteParseRequest { inviteParseRequest="" }
                return
            }
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
            else if ["query_directory", "open_directory"].contains(sourceOp) {
                if frame.request_id == nil || frame.request_id == directoryRequestId { directoryQueryError = error; updateDirectoryWarnings() }
            }
            else if directoryVisible && directoryActionOps.contains(sourceOp) {
                directoryActionError = error; directoryActionErrorSourceOp = sourceOp; updateDirectoryWarnings()
            }
            else if settingsVisible || pendingHubSave != nil { settingsBannerError = error }
            else { menuBannerError = error }
            diagnostic("error \(error)")
            rebuild()
            if settingsPanel != nil { rebuildSettingsPanel() }
            return
        }
        if frame.op == "settings", let value = frame.settings {
            if let request = frame.request_id, let app = pendingAppChecks.removeValue(forKey: request),
               frame.ok == true && frame.source_op == "setup_apply" {
                appCheckedUntil[app] = Date().addingTimeInterval(1.5)
                DispatchQueue.main.asyncAfter(deadline:.now()+1.5) { [weak self] in
                    if self?.settingsVisible == true { self?.rebuildSettingsPanel() }
                }
            }
            if hubInvitePending && frame.ok == true && ["setup_save_hub","save_hub"].contains(frame.source_op ?? "") {
                hubInvitePending=false; hubInvitePastedUntil=Date().addingTimeInterval(1.2)
            }
            let saved = frame.ok == true && ["channel_save","save_hub_service","configure_human_channel"].contains(frame.source_op ?? "") ? frame.channel_id : nil
            acceptSettings(value, hub: frame.hub, savedChannel: saved, savedRequest: frame.request_id, confirmHubSave: frame.ok == true && ["setup_save_hub","save_hub"].contains(frame.source_op ?? ""))
            if settingsPanel != nil { rebuildSettingsPanel() }
            return
        }
        if frame.op == "hide_banner" { hide(clicked:false); return }
        if frame.settings?.desktop_banner_enabled == false { hide(clicked:false) }
        if frame.op == "pair_code", let id = frame.channel_id {
            let hub = frame.hub ?? effectiveChannelHub
            let key="\(hub)|\(id)"
            guard let request = frame.request_id, var ui = channelUiStates[key], ui.pendingPairRequest == request else { return }
            ui.pendingPairRequest = nil
            ui.pairingInstruction = frame.instruction ?? frame.code ?? ""
            ui.pairingCommand = frame.pairing_command ?? frame.instruction
            ui.pairingURL = frame.pairing_url
            if let command = frame.pairing_command { copyText(command); ui.pairCopiedUntil = Date().addingTimeInterval(1.2) }
            channelUiStates[key] = ui
            if settingsPanel != nil { rebuildSettingsPanel() }
            return
        }
        if frame.op == "channel_test_result", let id = frame.channel_id {
            channelUiStates["\(frame.hub ?? effectiveChannelHub)|\(id)", default: ChannelUiState()].testResult = (ok: frame.ok == true, detail: frame.detail ?? frame.error ?? (frame.ok == true ? "Test sent" : "Test failed"))
            if settingsPanel != nil { rebuildSettingsPanel() }
            return
        }
        if frame.op == "state", let h = frame.hubs, let l = frame.listeners,
           h.allSatisfy({ ["connected", "connecting", "auth_required", "disconnected", "unreachable", "error"].contains($0.status) && ["none", "token", "oidc"].contains($0.auth_mode) }),
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
            let directoryMembershipChanged = h != hubs || l != listeners
            let nextHarnesses = frame.harnesses ?? harnesses
            let harnessesChanged = nextHarnesses != harnesses
            if harnessesChanged { harnesses = nextHarnesses }
            let nextSettings = frame.settings ?? settings
            // An unchanged save acknowledgement still needs to refresh the settings feedback.
            let settingsChanged = nextSettings != settings || hostHubPortPending
            if settingsChanged, let value = nextSettings { acceptSettings(value, hub: frame.hub ?? value.hubs.first(where: { $0.is_default })?.name ?? h.first?.name) }
            if let error = frame.composer_error, !error.isEmpty { setComposerStatus(error) }
            if hasRenderedState && !pendingMutation && !harnessesChanged && !settingsChanged && !activityChanged && h == hubs && l == listeners && a == available && r == myRemote && onlineCount == directoryOnlineCount {
                let warning = hubs.contains { $0.status != "connected" || $0.error != nil }
                statusImage = makeCitoStatusIcon(warning ? .warning : listeners.contains { $0.harness != "human" } ? .active : .idle)
                if let button = status?.button ?? headlessStatusButton {
                    formatStatusButton(button, unreadCount: unreadCount, image: statusImage)
                }
            } else {
                hubs = h; listeners = l; available = a; myRemote = r; directoryOnlineCount = onlineCount
                seedDirectoryPeersIfEmpty()
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
                if directoryVisible && directoryMembershipChanged { queryDirectory() }
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
            activityTotal = frame.activity_total ?? activityTotal
            activityUnreadCount = frame.activity_unread_count ?? frame.unread_count ?? activityUnreadCount
            directoryWarnings = frame.warnings ?? directoryWarnings
            directoryQueryError = nil
            updateDirectoryWarnings()
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
            guard settings?.desktop_banner_enabled != false else { hide(clicked:false); return }
            if visible, let previous = payload, previous.hub == value.hub && previous.target_title == value.target_title && previous.target_session_id == value.target_session_id && previous.target_harness == value.target_harness && previous.effective_wake == value.effective_wake {
                displayedCount += value.count ?? 1
            } else { displayedCount = value.count ?? 1 }
            payload = value; show(value)
            scheduleBannerDismiss(dismissMs)
        } else { diagnostic("invalid frame") }
    }
    func emit(_ action: [String: Any], userInitiated: Bool = true) {
        let operation = action["op"] as? String ?? ""
        let passive = ["query_directory", "refresh_available"].contains(operation)
        if userInitiated && !passive && menuBannerError != nil { menuBannerError = nil; rebuild() }
        var outgoing = action
        if directoryActionOps.contains(operation) {
            // Full-message copy already owns this ID through its multipart buffer.
            let request = (action["request_id"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? UUID().uuidString
            outgoing["request_id"] = request
            latestDirectoryActionRequestId = request
            clearDirectoryActionError()
        }
        if operation == "channel_pair", let id=action["channel_id"] as? String {
            let request=UUID().uuidString
            outgoing["request_id"]=request
            channelUiStates["\(action["hub"] as? String ?? effectiveChannelHub)|\(id)", default: ChannelUiState()].pendingPairRequest = request
        }
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
           ["channel_save", "save_hub_service", "channel_pair", "channel_toggle", "channel_unpair", "channel_cancel_pair", "disconnect_human_channel", "clear_channel_service"].contains(op),
           let id = action["channel_id"] as? String {
            let key = "\(action["hub"] as? String ?? effectiveChannelHub)|\(id)"
            var needsRebuild = channelUiStates[key]?.testResult != nil
            switch op {
            case "channel_unpair", "channel_cancel_pair":
                channelUiStates[key, default: ChannelUiState()].resetPersonal()
                needsRebuild = true
            case "clear_channel_service":
                channelUiStates[key, default: ChannelUiState()].resetAll()
                needsRebuild = true
            case "disconnect_human_channel":
                if action["reset_hub"] as? Bool == true { channelUiStates[key, default: ChannelUiState()].resetAll() }
                else { channelUiStates[key, default: ChannelUiState()].resetPersonal() }
                needsRebuild = true
            default: break
            }
            channelUiStates[key, default: ChannelUiState()].testResult = nil
            // Preserve native field editors and undo history when a submission
            // changes no presentation; cancellation must redraw immediately.
            if needsRebuild && settingsPanel != nil { rebuildSettingsPanel() }
        }
        guard let data = try? JSONSerialization.data(withJSONObject: outgoing, options: [.sortedKeys]) else { return }
        FileHandle.standardOutput.write(data + Data([10]))
    }
    func rebuild() {
        MenuControl.clearHoverWhere { $0.enclosingMenuItem != nil || ($0.window !== self.directoryPanel && $0.window !== self.settingsPanel) }
        rebuildCount += 1
        let count = uniqueSessionCount
        let warning = hubs.contains { $0.status != "connected" || $0.error != nil }
        statusImage = makeCitoStatusIcon(warning ? .warning : listeners.contains { $0.harness != "human" } ? .active : .idle)
        if let button = status?.button ?? headlessStatusButton {
            formatStatusButton(button, unreadCount: unreadCount, image: statusImage)
        }
        let menu = cardMenu
        let renameDraft = renameField?.currentEditor()?.string ?? renameField?.stringValue
        for item in menu.items {
            let scroll = (item.view as? NSScrollView) ?? item.view?.subviews.compactMap { $0 as? NSScrollView }.first
            if let scroll = scroll, let id = scroll.identifier?.rawValue,
               ["active_scroll","my_remote_scroll","available_scroll"].contains(where: { id == $0 || id.hasPrefix($0 + ":") }) {
                let key = menuScrollKeys[id] ?? id
                let offset = scroll.contentView.bounds.origin
                menuScrollOffsets[key] = offset; menuScrollOffsets[id] = offset
                if let top = scroll.documentView?.subviews.sorted(by: { $0.frame.minY < $1.frame.minY }).first(where: { $0.frame.maxY > offset.y }), let rowId = top.identifier?.rawValue {
                    menuScrollAnchors[key] = (rowId,offset.y-top.frame.minY)
                    menuScrollAnchors[id] = menuScrollAnchors[key]
                }
            }
        }
        menu.removeAllItems()
        func host(_ view: NSView, id: String, scrollKey: String? = nil) -> NSMenuItem {
            view.identifier = NSUserInterfaceItemIdentifier(id)
            let item = NSMenuItem()
            if view is NSScrollView {
                // Menu roots use the same unflipped coordinate space as their host.
                let root = NSView(frame:NSRect(origin:.zero,size:view.frame.size))
                root.appearance = testAppearance
                root.addSubview(view); item.view = root
            } else { view.appearance = testAppearance; item.view = view }
            menu.addItem(item)
            let key = scrollKey ?? id
            if view is NSScrollView { menuScrollKeys[id] = key }
            if let scroll = view as? NSScrollView, let offset = menuScrollOffsets[key], let doc = scroll.documentView {
                scroll.layoutSubtreeIfNeeded()
                let anchoredY = menuScrollAnchors[key].flatMap { anchor in doc.subviews.first { $0.identifier?.rawValue == anchor.rowId }.map { $0.frame.minY + anchor.deltaY } } ?? offset.y
                scroll.contentView.scroll(to:NSPoint(x:max(0,min(offset.x,doc.frame.width-scroll.contentSize.width)),y:max(0,min(anchoredY,doc.frame.height-scroll.contentSize.height))))
                scroll.reflectScrolledClipView(scroll.contentView)
            }
            return item
        }
        let btnFont = NSFont.systemFont(ofSize: 11, weight: .medium)
        let header = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 40))
        header.addSubview(textLabel("Cito", NSRect(x: 12, y: 12, width: 36, height: 18), size: 13, weight: .bold))
        let subtitleText: String
        if count > 0 {
            subtitleText = "\(count) session\(count == 1 ? "" : "s") listening"
        } else {
            let humanHub = hubs.first { $0.name == effectiveTargetHub }
            if humanListening { subtitleText = "Idle" }
            else if humanHub?.human_paused != true && humanHub?.status == "auth_required" { subtitleText = "Sign in required" }
            else if humanHub?.human_paused != true && ["unreachable","error"].contains(humanHub?.status ?? "") { subtitleText = "Hub unavailable" }
            else if humanHub?.human_paused != true && humanHub?.status == "connecting" { subtitleText = "Connecting…" }
            else { subtitleText = humanHub?.human_paused == false ? "Not listening" : "Notifications paused" }
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
            dismiss.style = .subtleHeader
            dismiss.toolTip = "Dismiss error"; dismiss.setAccessibilityLabel("Dismiss error")
            dismiss.pressed = { [weak self] in self?.menuBannerError = nil; self?.rebuild() }
            row.addSubview(dismiss)
            _ = host(row,id:"menu_error_row")
        }
        var drawerItems: [String: NSMenuItem] = [:]
        let showHubControls = hubs.count != 1 || hubs.contains {
            !(["local", "default"].contains($0.name) && $0.auth_mode == "none" && $0.status == "connected" && $0.error == nil)
        }
        for hub in hubs where showHubControls {
            let drawer = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 32))
            let loginRun = { self.emit(["op": "hub_action", "hub": hub.name, "action": hub.status == "connected" ? "switch_account" : "login"]) }
            let reconnectRun = { self.emit(["op": "hub_action", "hub": hub.name, "action": "reconnect"]) }
            let disconnectRun = { self.settingsActiveTab="hubs"; self.openSettingsPanel() }
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
            var drawerControls: [NSView] = []
            for (title, action, primary, run) in buttons {
                let width = measureTextWidth(title, font: btnFont) + 16
                let control = MenuControl(title, frame: NSRect(x: 0, y: 5, width: width, height: 22))
                control.label.font = btnFont
                control.style = primary ? .prominent : .neutral
                control.identifier = NSUserInterfaceItemIdentifier("\(action):\(hub.name)")
                control.pressed = run; drawer.addSubview(control); drawerControls.append(control)
            }
            let statusText: String
            if let error = hub.error { statusText = presentationError(error) }
            else if hub.status == "connected" { statusText = hub.auth_mode == "oidc" ? "Connected as \(hub.user ?? "user")" : "Connected · \(hub.user ?? "local")" }
            else if hub.status == "connecting" { statusText = "Connecting to hub…" }
            else if hub.status == "auth_required" { statusText = "Authentication required" }
            else { statusText = "Offline · Disconnected" }
            let detail = textLabel(hub.error == nil ? statusText : "Check Hub settings", NSRect(x: 12, y: 8, width: 40, height: 16),
                color: hub.error != nil || hub.status == "auth_required" ? .labelColor : CitoPalette.secondaryText)
            detail.lineBreakMode = .byTruncatingMiddle
            detail.toolTip = statusText; drawer.addSubview(detail)
            layoutBar(in: drawer, y: 5, height: 22, spacing: 4, flexible: detail, trailing: drawerControls)
            let item = host(drawer, id: "drawer:\(hub.name)")
            let autoExpand = hub.status != "connected" || hub.error != nil
            item.isHidden = !(hubDrawerVisibility[hub.name] ?? autoExpand)
            drawerItems[hub.name] = item
        }
        let stripX = max(138, 54 + subWidth)
        let stripWidth = max(44, 368 - stripX)
        var hubPills: [NSView] = []
        for hub in hubs where showHubControls {
            let color = hub.name == effectiveTargetHub && hub.status == "connected" && hub.error == nil ? NSColor.white : statusColor(hub.status)
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
            let pill = MenuControl(title, frame: NSRect(x: 0, y: 0, width: width, height: 22), radius: 11)
            pill.identifier = NSUserInterfaceItemIdentifier("hub:\(hub.name)")
            pill.style = hub.name == effectiveTargetHub
                ? (hub.status == "connected" && hub.error == nil ? .prominent : .neutral)
                : .subtleHeader
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
                pill.toolTip = isSingleDefault ? (hub.status == "connected" ? "Connected · \(hub.user ?? "local")" : title) : "\(hub.name) · \(["connected":"Connected", "auth_required":"Sign-in required", "disconnected":"Offline", "connecting":"Connecting…"][hub.status] ?? "Unavailable")"
            }
            pill.setAccessibilityLabel(pill.toolTip ?? hub.name)
            pill.pressed = { [weak self, weak menu] in
                guard let self = self, let drawer = drawerItems[hub.name] else { return }
                if self.hubs.count > 1 {
                    let alreadySelected = self.effectiveTargetHub == hub.name
                    self.selectedTargetHub = hub.name
                    if alreadySelected || hub.status != "connected" || hub.error != nil {
                        self.hubDrawerVisibility[hub.name] = drawer.isHidden
                    }
                    self.rebuild()
                    return
                }
                drawer.isHidden = !drawer.isHidden
                self.hubDrawerVisibility[hub.name] = !drawer.isHidden
                menu?.update()
                MenuControl.reconcileActiveHover()
            }
            hubPills.append(pill)
        }
        if showHubControls {
            let strip = mountPillStrip(in: header, frame: NSRect(x: stripX, y: 9, width: stripWidth, height: 22),
                           id: "header_hub_strip", alignment: .trailing, pills: hubPills) { view in
                let id = view.identifier?.rawValue ?? ""
                return id == "hub:\(effectiveTargetHub)"
            }
            if hubs.count > 1 {
                strip.wantsLayer = true
                strip.backgroundColor = NSColor.labelColor.withAppearanceAlpha(0.06)
                strip.drawsBackground = true
                strip.layer?.cornerRadius = 11
                strip.layer?.masksToBounds = true
            }
        }
        var activeRows: [(id: String, view: NSView)] = []
        let home = NSHomeDirectory()
        let map = harnessMap
        let targetHub = effectiveTargetHub
        let listeners = self.listeners.filter { targetHub.isEmpty || $0.hub == targetHub }
        let myRemote = self.myRemote.filter { targetHub.isEmpty || $0.hub == nil || $0.hub == targetHub }
        let hasHumanListener = listeners.contains { $0.harness == "human" }
        let targetHubObj = hubs.first { $0.name == targetHub }
        let targetUser = hubs.first(where: { $0.name == targetHub })?.user ?? hubs.first?.user ?? "local"
        if !hasHumanListener && hubs.contains(where: { $0.name == targetHub && $0.status != "disconnected" }) {
            let rawPausedAbout = targetHubObj?.human_about
            let humanDisplayTitle = "You (@\(targetUser))"
            let about = distinctAbout(rawPausedAbout, title:"Human", user:targetUser, otherTitle:humanDisplayTitle)
            let resumedAbout = (rawPausedAbout ?? "").trimmingCharacters(in:.whitespacesAndNewlines)
            let resumeAbout = resumedAbout.isEmpty ? "Human" : resumedAbout
            let rowHeight: CGFloat = about == nil ? 64 : 78, cardHeight: CGFloat = about == nil ? 58 : 72
            let row = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: rowHeight))
            let card = roundedView(NSRect(x: CitoMetrics.menuInset, y: 3, width: CitoMetrics.menuRailWidth, height: cardHeight), radius: 10,
                color: .labelColor.withAppearanceAlpha(0.05), border: .labelColor.withAppearanceAlpha(0.10))
            card.addSubview(makeHarnessBadge(NSRect(x: 12, y: cardHeight - 43, width: 28, height: 28), radius: 7, iconInset: 6, harness: "human", harnesses: map))
            let title = textLabel(humanDisplayTitle, NSRect(x: 48, y: cardHeight - 26, width: 220, height: 18), size: 13, weight: .semibold)
            title.lineBreakMode = .byTruncatingMiddle; title.toolTip = title.stringValue
            card.addSubview(title)
            let status: String
            if targetHubObj?.human_paused == true { status = "Notifications paused" }
            else if targetHubObj?.status == "auth_required" { status = "Sign in above to receive notifications" }
            else if ["unreachable","error"].contains(targetHubObj?.status ?? "") { status = "Hub unavailable — reconnect above" }
            else if targetHubObj?.status == "connecting" { status = "Connecting to receive notifications" }
            else { status = targetHubObj?.human_paused == false ? "Notifications not connected" : "Notifications paused" }
            let statusLabel = textLabel(status, NSRect(x: 48, y: 8, width: 200, height: 16), size: 11, color: CitoPalette.secondaryText)
            statusLabel.identifier = NSUserInterfaceItemIdentifier("human_listening_status"); statusLabel.toolTip = status
            card.addSubview(statusLabel)
            if let about = about {
                let detail = textLabel(about,NSRect(x:48,y:28,width:296,height:16),size:11.5,color:CitoPalette.secondaryText)
                detail.lineBreakMode = .byTruncatingMiddle; detail.toolTip = about
                detail.identifier = NSUserInterfaceItemIdentifier("human_paused_about"); card.addSubview(detail)
            }
            let enable = MenuControl("Resume", frame: NSRect(x: card.bounds.width-12-MenuControl.intrinsicWidth("Resume"), y: about == nil ? 18 : cardHeight - 28, width: MenuControl.intrinsicWidth("Resume"), height: 22), radius: 6)
            enable.identifier = NSUserInterfaceItemIdentifier("listen_human")
            enable.style = .prominent
            enable.pressed = { [weak self] in self?.emit(["op": "listen_session", "hub": targetHub, "harness": "human", "session_id": "default", "about": resumeAbout]) }
            title.frame.size.width = max(0,enable.frame.minX-52)
            card.addSubview(enable); row.addSubview(card)
            _ = host(row, id: "human_paused_card")
        }
        let orderedListeners = listeners.enumerated().sorted { a, b in
            let ah = a.element.harness == "human" ? 0 : 1
            let bh = b.element.harness == "human" ? 0 : 1
            return ah != bh ? ah < bh : a.offset < b.offset
        }.map { $0.element }
        for listener in orderedListeners {
            let sessionKey = "\(listener.harness):\(listener.session_id)"
            let aboutText = (listener.about ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            let user = [listener.user, listener.owner_user, hubs.first { $0.name == listener.hub }?.user]
                .compactMap { $0 }.first { !$0.isEmpty } ?? targetUser
            let displayTitle = nodeDisplayTitle(listener.title, harness: listener.harness, user: user, about: listener.about, harnessName: map[listener.harness]?.display_name, customTitle: listener.custom_title == true)
            let hasDistinctAbout = distinctAbout(listener.about, title:listener.title, user:user, otherTitle:displayTitle) != nil
            let rowHeight: CGFloat = hasDistinctAbout ? 78 : 64
            let cardHeight: CGFloat = hasDistinctAbout ? 72 : 58
            let row = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: rowHeight))
            row.identifier = NSUserInterfaceItemIdentifier("session:\(listener.scoped_id)")
            let card = roundedView(NSRect(x: CitoMetrics.menuInset, y: 3, width: CitoMetrics.menuRailWidth, height: cardHeight), radius: 10,
                color: .labelColor.withAppearanceAlpha(0.045), border: .labelColor.withAppearanceAlpha(0.08))
            row.addSubview(card)
            activeRows.append((id: "session:\(listener.scoped_id)", view: row))
            let titleX: CGFloat = 48
            let badge = makeHarnessBadge(NSRect(x: 12, y: cardHeight - 43, width: 28, height: 28),
                radius: 7, iconInset: 6, harness: listener.harness, harnesses: map)
            card.addSubview(badge)
            let listenerHub = hubs.first(where: { $0.name == listener.hub })
            let hasOtherUsers = directoryAllPeers.contains(where: { $0.hub == listener.hub && !$0.user.isEmpty && $0.user != user }) || listeners.contains(where: { $0.hub == listener.hub && !($0.user ?? "").isEmpty && $0.user != user })
            let showScopeToggle = listenerHub?.auth_mode == "oidc" || hasOtherUsers
            let scopeWidth: CGFloat = 68
            var sessionActions: [NSView] = []
            let title = textLabel(displayTitle,
                NSRect(x: titleX, y: cardHeight - 26, width: 40, height: 18), size: 13, weight: .semibold)
            title.lineBreakMode = .byTruncatingMiddle
            title.toolTip = displayTitle; card.addSubview(title)
            if hasDistinctAbout {
                let aboutLabel = textLabel(aboutText, NSRect(x: titleX, y: 28, width: card.bounds.width - CitoMetrics.cardInset - titleX, height: 16), size: 11.5, color: CitoPalette.secondaryText)
                aboutLabel.lineBreakMode = .byTruncatingMiddle
                aboutLabel.toolTip = aboutText; card.addSubview(aboutLabel)
            }
            let short = listener.display_address
            let copyFont = NSFont.monospacedSystemFont(ofSize: 11, weight: .medium)
            let shownAddress = compactAddressLabel(short,maxWidth:220,font:copyFont,paddingX:3)
            let copyWidth = min(220,max(MenuControl.intrinsicWidth("✓ Copied", font:copyFont, leadingSymbol:"doc.on.doc", paddingX:3), MenuControl.intrinsicWidth(shownAddress, font:copyFont, leadingSymbol:"doc.on.doc", paddingX:3)))
            let copy = MenuControl(shownAddress, frame: NSRect(x: titleX, y: 7, width: copyWidth, height: 19), leadingSymbol: "doc.on.doc")
            copy.label.font = copyFont
            copy.style = .subtleHeader
            copy.toolTip = shownAddress == short ? listener.scoped_id : short; copy.identifier = NSUserInterfaceItemIdentifier("copy:\(listener.scoped_id)")
            copy.setAccessibilityLabel(short)
            copy.label.alignment = .left
            copy.layoutContents()
            copy.pressed = { [weak self, weak copy] in
                self?.copyText(listener.display_address, control: copy)
            }
            card.addSubview(copy)
            let cwd = listener.cwd == home ? "~" : listener.cwd.hasPrefix(home + "/") ? "~" + listener.cwd.dropFirst(home.count) : listener.cwd
            let branch = (listener.branch ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            let pathText: String
            if listener.harness == "human" {
                let chs = (listener.channels ?? []).filter { !["mac", "this mac", "desktop banner", "desktop notifications", "hub", ""].contains($0.lowercased()) && !$0.hasPrefix("@") }
                let banner=settings?.desktop_banner_enabled != false
                pathText = chs.isEmpty ? (banner ? "Desktop banner" : "Banner off · Inbox only") : (chs.joined(separator:" · ") + (banner ? " · Banner" : " only · Banner off"))
            } else {
                pathText = (!branch.isEmpty && branch != "HEAD") ? "\(cwd) · \(branch)" : cwd
            }
            let path = textLabel(pathText,
                NSRect(x: titleX + copyWidth + 8, y: 8, width: max(0, card.bounds.width - CitoMetrics.cardInset - titleX - copyWidth - 8), height: 16), size: 11, color: CitoPalette.secondaryText)
            path.lineBreakMode = .byTruncatingMiddle; path.toolTip = pathText; path.isHidden = pathText.isEmpty; card.addSubview(path)
            if showScopeToggle {
                var scope = listener.scope
                let scopeBtn = MenuControl(scope == "self" ? "Self" : "Team", frame: NSRect(x: 0, y: cardHeight - 28, width: scopeWidth, height: 22),
                    leadingSymbol: scope == "self" ? "lock.fill" : "person.2.fill")
                scopeBtn.identifier = NSUserInterfaceItemIdentifier("scope:\(listener.scoped_id)")
                let scopeColor = { [weak scopeBtn] in
                    scopeBtn?.style = scope == "workspace" ? .prominent : .neutral
                    scopeBtn?.toolTip = scope == "self"
                        ? "Visibility: Only Me (@\(user)) — only your own sessions and devices on '\(listener.hub)' can message this session. Click to share with Team."
                        : "Visibility: Team — anyone connected to '\(listener.hub)' can discover and message this session. Click to restrict to Only Me."
                }
                scopeColor()
                scopeBtn.pressed = { [weak self, weak scopeBtn] in
                    guard let self = self else { return }
                    self.pendingMutation = true
                    scope = scope == "self" ? "workspace" : "self"
                    scopeBtn?.label.stringValue = scope == "self" ? "Self" : "Team"
                    scopeBtn?.setLeadingSymbol(scope == "self" ? "lock.fill" : "person.2.fill")
                    scopeBtn?.setAccessibilityLabel(scopeBtn?.label.stringValue); scopeColor()
                    self.emit(["op": "set_scope", "hub": listener.hub, "listener_id": listener.listener_id, "scope": scope])
                }
                card.addSubview(scopeBtn); sessionActions.append(scopeBtn)
            }
            let personal = listener.harness == "human"
            let stop = MenuControl("Pause", frame: NSRect(x: 0, y: cardHeight - 28, width: 52, height: 22))
            stop.toolTip = personal ? "Pause all human channels on this Hub, including this Mac and external channels" : "Pause listening for this session"
            stop.style = .neutral
            stop.identifier = NSUserInterfaceItemIdentifier("stop:\(listener.scoped_id)")
            let unlisten = { self.emit(["op": "unlisten_session", "hub": listener.hub, "harness": listener.harness, "session_id": listener.session_id]) }
            stop.pressed = unlisten; card.addSubview(stop); sessionActions.append(stop)
            layoutBar(in: card, insetLeft: titleX, y: cardHeight - 28, height: 22, spacing: CitoMetrics.gap, trailing: sessionActions)
            let rightEdge = sessionActions.map(\.frame.minX).min() ?? (card.bounds.width - CitoMetrics.cardInset)
            if !personal && renamingSessionKey == sessionKey {
                title.isHidden = true
                let field = NSTextField(string: renameDraft ?? listener.title)
                field.frame = NSRect(x: titleX, y: cardHeight - 27, width: max(80, rightEdge - titleX - 6), height: 20)
                field.identifier = NSUserInterfaceItemIdentifier("rename_field:\(listener.scoped_id)")
                field.delegate = self; field.font = .systemFont(ofSize: 13)
                card.addSubview(field); renameField = field
                DispatchQueue.main.async { field.window?.makeFirstResponder(field); field.selectText(nil) }
            } else if !personal {
                let titleW = max(24, min(rightEdge - titleX - 24, ceil(measureTextWidth(displayTitle, font: .systemFont(ofSize: 13, weight: .semibold))) + 4))
                title.frame = NSRect(x: titleX, y: cardHeight - 26, width: titleW, height: 18)
                let renameBtn = MenuControl("Rename session", frame: NSRect(x: titleX + titleW + 2, y: cardHeight - 26, width: 18, height: 18), symbol: "pencil")
                renameBtn.identifier = NSUserInterfaceItemIdentifier("rename:\(listener.scoped_id)")
                renameBtn.style = .subtleHeader; renameBtn.toolTip = "Rename session"
                renameBtn.pressed = { [weak self] in self?.renameField = nil; self?.renamingSessionKey = sessionKey; self?.rebuild() }
                card.addSubview(renameBtn)
            } else {
                title.frame.size.width = max(24, rightEdge - titleX - 6)
            }
            layoutBar(in: card, insetLeft: titleX, insetRight: 10, y: 7, height: 19, leading: [copy], flexible: path)
        }
        if activeRows.count <= 4 {
            for (id, row) in activeRows { _ = host(row, id: id) }
        } else {
            let totalHeight = activeRows.reduce(CGFloat(0)) { $0 + $1.view.frame.height }
            let scroll = OverlayScrollView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 256))
            scroll.scrollerStyle = .overlay
            scroll.drawsBackground = false; scroll.hasVerticalScroller = true
            scroll.hasHorizontalScroller = false; scroll.autohidesScrollers = true
            let doc = FlippedView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: totalHeight))
            var y: CGFloat = 0
            for (_, row) in activeRows {
                row.frame.origin = NSPoint(x: 0, y: y)
                doc.addSubview(row); y += row.frame.height
            }
            scroll.documentView = doc; _ = host(scroll, id: "active_scroll")
        }
        if listeners.isEmpty {
            if available.isEmpty {
                let row = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 92))
                let card = roundedView(NSRect(x: CitoMetrics.menuInset, y: 3, width: CitoMetrics.menuRailWidth, height: 86), radius: 10, color: .labelColor.withAppearanceAlpha(0.045))
                let icon = NSImageView(frame: NSRect(x: 12, y: 22, width: 24, height: 24))
                icon.image = NSImage(systemSymbolName: "waveform.slash", accessibilityDescription: "No active listeners")
                icon.contentTintColor = CitoPalette.secondaryText; card.addSubview(icon)
                card.addSubview(textLabel("No active listeners", NSRect(x: 44, y: 57, width: 304, height: 18), size: 13, weight: .semibold))
                let subtitle = !(settings?.apps.contains(where:{$0.isConfigured}) ?? true) ? "Connect your coding apps in Settings to start" : "Open a coding session in your app or configure Human Channels"
                let emptyHint = textLabel(subtitle, NSRect(x:44,y:32,width:304,height:26),size:10,color:CitoPalette.secondaryText)
                emptyHint.maximumNumberOfLines = 2; emptyHint.lineBreakMode = .byWordWrapping
                card.addSubview(emptyHint)
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
                let row = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 34))
                let pill = roundedView(NSRect(x: CitoMetrics.menuInset, y: 3, width: CitoMetrics.menuRailWidth, height: 28), radius: 8, color: .labelColor.withAppearanceAlpha(0.045))
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
            let remoteHeader = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 26))
            remoteHeader.addSubview(roundedView(NSRect(x: 12, y: 24, width: 356, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
            let toggleRemote = MenuControl("MY OTHER HOSTS · \(myRemote.count)", frame: NSRect(x: CitoMetrics.menuInset, y: 1, width: CitoMetrics.menuRailWidth, height: 22),
                leadingSymbol: myRemoteCollapsed ? "chevron.right" : "chevron.down", color: CitoPalette.secondaryText, radius: 6)
            toggleRemote.identifier = NSUserInterfaceItemIdentifier("toggle_my_remote")
            toggleRemote.style = .subtleHeader
            toggleRemote.label.font = .systemFont(ofSize: 10, weight: .semibold)
            toggleRemote.label.alignment = .left
            toggleRemote.layoutContents()
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
                    let row = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 34))
                    let rowId = "remote:\(peer.scoped_id)"
                    row.identifier = NSUserInterfaceItemIdentifier(rowId)
                    let hostIcon = NSImageView(frame: NSRect(x: 16, y: 10, width: 14, height: 14))
                    hostIcon.image = NSImage(systemSymbolName: "desktopcomputer", accessibilityDescription: "Remote host")
                    hostIcon.contentTintColor = CitoPalette.secondaryText
                    row.addSubview(hostIcon)
                    let short = peer.display_address
                    let shownAddress = compactAddressLabel(short,maxWidth:220,font:copyFont,paddingX:3)
                    let copyWidth = min(220,max(MenuControl.intrinsicWidth("✓ Copied", font:copyFont, leadingSymbol:"doc.on.doc", paddingX:3), MenuControl.intrinsicWidth(shownAddress, font:copyFont, leadingSymbol:"doc.on.doc", paddingX:3)))
                    let copy = MenuControl(shownAddress, frame: NSRect(x: 368 - copyWidth, y: 7, width: copyWidth, height: 20), leadingSymbol: "doc.on.doc")
                    copy.label.font = copyFont
                    copy.label.alignment = .left
                    copy.toolTip = shownAddress == short ? peer.scoped_id : short
                    copy.setAccessibilityLabel(short)
                    copy.identifier = NSUserInterfaceItemIdentifier("copy_remote:\(peer.scoped_id)")
                    copy.layoutContents()
                    copy.pressed = { [weak self, weak copy] in
                        self?.copyText(peer.display_address, control: copy)
                    }
                    row.addSubview(copy)
                    let cleanHost = cleanHostDisplay(peer.host_name)
                    let hostText = cleanHost.isEmpty ? peer.host_name : cleanHost
                    let branch = (peer.branch ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
                    let branchSuffix = (!branch.isEmpty && branch != "HEAD") ? " · \(branch)" : ""
                    let displayTitle = nodeDisplayTitle(peer.title,harness:peer.harness,user:peer.user,about:peer.about,harnessName:map[peer.harness]?.display_name,customTitle:peer.custom_title == true)
                    let info = textLabel("\(hostText) · \(displayTitle)\(branchSuffix)",
                        NSRect(x: 36, y: 8, width: max(40, 368 - copyWidth - 44), height: 18), size: 12, weight: .regular, color: .labelColor)
                    info.lineBreakMode = .byTruncatingMiddle
                    info.toolTip = peer.scoped_id + (distinctAbout(peer.about, title:peer.title, user:peer.user, otherTitle:displayTitle).map { " — \($0)" } ?? "")
                    row.addSubview(info)
                    layoutBar(in: row, insetLeft: 16, y: 7, height: 20, leading: [hostIcon], flexible: info, trailing: [copy])
                    remoteRows.append((id: rowId, view: row))
                }
                if remoteRows.count <= 3 {
                    for (id, row) in remoteRows { _ = host(row, id: id) }
                } else {
                    let totalHeight = CGFloat(remoteRows.count) * 34
                    let scroll = OverlayScrollView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 102))
                    scroll.scrollerStyle = .overlay
                    scroll.drawsBackground = false; scroll.hasVerticalScroller = true
                    scroll.hasHorizontalScroller = false; scroll.autohidesScrollers = true
                    let doc = FlippedView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: totalHeight))
                    for (idx, (_, row)) in remoteRows.enumerated() {
                        row.frame.origin = NSPoint(x: 0, y: CGFloat(idx) * 34)
                        doc.addSubview(row)
                    }
                    scroll.documentView = doc; _ = host(scroll, id: "my_remote_scroll")
                }
            }
        }
        // Zone 3 — Available sessions on the network selected in the header.
        if !available.isEmpty || !targetHub.isEmpty {
            let availHeader = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 24))
            availHeader.addSubview(roundedView(NSRect(x: 12, y: 21, width: 356, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
            availHeader.addSubview(textLabel("AVAILABLE SESSIONS", NSRect(x: 12, y: 4, width: 190, height: 14),
                size: 10, weight: .semibold, color: CitoPalette.secondaryText))
            let autoListen = hubs.first(where: { $0.name == targetHub })?.auto_listen ?? false
            let autoTitle = autoListen ? "Auto-Listen: On" : "Auto-Listen"
            let autoWidth = MenuControl.toggleWidth("toggle_auto_listen")
            let auto = MenuControl(autoTitle, frame: NSRect(x: 368 - autoWidth, y: 2, width: autoWidth, height: 18), radius: 9)
            auto.identifier = NSUserInterfaceItemIdentifier("toggle_auto_listen")
            auto.style = autoListen ? .prominent : .neutral
            auto.isEnabled = !targetHub.isEmpty
            auto.pressed = { [weak self] in self?.emit(["op": "set_auto_listen", "hub": targetHub, "enabled": !autoListen]) }
            availHeader.addSubview(auto)
            _ = host(availHeader, id: "available_header")
            let visibleAvailable = hubs.count > 1 && !targetHub.isEmpty
                ? available.filter { !($0.listening_hubs ?? []).contains(targetHub) }
                : available
            if visibleAvailable.isEmpty && listeners.contains(where: { $0.harness != "human" }) {
                let emptyTarget = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 28))
                let label = textLabel(autoListen ? "Auto-Listen is on — open sessions connect automatically" : "All sessions listening on \(targetHub)",
                    NSRect(x: 24, y: 6, width: 332, height: 16), size: 11.5, weight: .regular, color: CitoPalette.secondaryText)
                label.lineBreakMode = .byTruncatingMiddle; label.toolTip = label.stringValue
                emptyTarget.addSubview(label)
                _ = host(emptyTarget, id: "available_empty_target")
            }
            if autoListen && visibleAvailable.contains(where: { $0.needs_reload == true }) {
                let feedback = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 38))
                let caption = textLabel("Auto-Listen is on — run /reload below to finish connecting", NSRect(x: 24, y: 4, width: 332, height: 30), size: 11, color: CitoPalette.secondaryText)
                caption.maximumNumberOfLines = 2
                caption.cell?.wraps = true
                feedback.addSubview(caption)
                _ = host(feedback, id: "auto_listen_reload_status")
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
                let recentKey = "\(targetHub):\(harness)"
                if autoListen && items.allSatisfy({ $0.endpoint_pid == nil && $0.needs_reload != true }) && autoCollapsedRecentGroups.insert(recentKey).inserted {
                    collapsedGroups.insert(harness)
                }
            }
            let expandedGroupCount = groupedCandidates.filter { !collapsedGroups.contains($0.harness) }.count
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
                let groupRow = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 28))
                let groupControl = MenuControl(groupTitle, frame: NSRect(x: CitoMetrics.menuInset, y: 2, width: CitoMetrics.menuRailWidth, height: 24), radius: 6)
                groupControl.identifier = NSUserInterfaceItemIdentifier("toggle_group:\(harness)")
                groupControl.toolTip = visual.tier == "experimental" ? "\(groupTitle) (Experimental)" : groupTitle
                groupControl.style = .subtleHeader
                groupControl.symbolName = isCollapsed ? "chevron.right" : "chevron.down"
                let chev = NSImageView(frame: NSRect(x: 2, y: 8, width: 8, height: 8))
                chev.image = NSImage(systemSymbolName: isCollapsed ? "chevron.right" : "chevron.down", accessibilityDescription: nil)
                chev.contentTintColor = CitoPalette.secondaryText
                groupControl.addSubview(chev)
                groupControl.accessorySymbolView = chev
                let badge = makeHarnessBadge(NSRect(x: 14, y: 3, width: 18, height: 18),
                    radius: 5, iconInset: 3, harness: harness, harnesses: map)
                groupControl.addSubview(badge)
                groupControl.label.frame = NSRect(x: 34, y: 4, width: 310, height: 16)
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
                        let rowHeight: CGFloat = needsReload ? 58 : 44
                        let row = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: rowHeight))
                        let rowId = "candidate:\(c.harness):\(c.session_id)"
                        row.identifier = NSUserInterfaceItemIdentifier(rowId)
                        let shortCwd = c.cwd == home ? "~" : c.cwd.hasPrefix(home + "/") ? "~" + c.cwd.dropFirst(home.count) : c.cwd
                        let branch = (c.branch ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
                        let branchSuffix = (!branch.isEmpty && branch != "HEAD") ? " · \(branch)" : ""
                        let candidateTitle = nodeDisplayTitle(c.title, harness: c.harness, user: targetUser, harnessName: visual.name, customTitle: c.custom_title == true)
                        let info = textLabel(candidateTitle, NSRect(x:46,y:rowHeight-21,width:244,height:16),size:12,weight:.medium,color:.labelColor)
                        info.lineBreakMode = .byTruncatingTail
                        let location = textLabel(compactSessionPath(shortCwd, branchSuffix: branchSuffix, maxWidth:244),NSRect(x:46,y:needsReload ? 21 : 6,width:244,height:14),size:10.5,color:CitoPalette.secondaryText)
                        location.lineBreakMode = .byTruncatingMiddle
                        location.toolTip = "\(shortCwd)\(branchSuffix)"
                        location.identifier = NSUserInterfaceItemIdentifier("candidate_cwd:\(c.harness):\(c.session_id)")
                        row.addSubview(location)
                        info.toolTip = "\(candidateTitle) — \(c.cwd)"
                        info.identifier=NSUserInterfaceItemIdentifier("candidate_title:\(c.harness):\(c.session_id)")
                        row.addSubview(info)
                        if needsReload {
                            let hint=textLabel(c.reload_hint ?? "Run /reload in \(visual.name) to enable 1-click listen",NSRect(x:46,y:5,width:244,height:14),size:10,color:CitoPalette.warningText)
                            hint.toolTip=hint.stringValue
                            hint.identifier=NSUserInterfaceItemIdentifier("candidate_hint:\(c.harness):\(c.session_id)")
                            row.addSubview(hint)
                            let reload=MenuControl("/reload",frame:NSRect(x:298,y:floor((rowHeight-22)/2),width:70,height:22),leadingSymbol:"doc.on.doc")
                            reload.label.font = .systemFont(ofSize:10)
                            reload.identifier=NSUserInterfaceItemIdentifier("reload:\(c.harness):\(c.session_id)")
                            reload.toolTip = "Copy /reload to paste in \(visual.name)"
                            reload.pressed={ [weak self, weak reload] in
                                guard let self=self else{return}
                                self.copyText("/reload", control:reload)
                                self.emit(["op":"refresh_available"])
                            }
                            row.addSubview(reload)
                            candidateRows.append((id:rowId,view:row)); continue
                        }
                        let listenBtn = MenuControl("Listen", frame: NSRect(x: 298, y: floor((rowHeight-22)/2), width: 70, height: 22), leadingSymbol: "plus", radius: 6)
                        listenBtn.identifier = NSUserInterfaceItemIdentifier("listen:\(c.harness):\(c.session_id)")
                        listenBtn.toolTip = "Start listening in \(candidateTitle)"
                        listenBtn.isEnabled = !targetHub.isEmpty
                        let listenRun = { [weak self, weak listenBtn] in
                            guard let self = self else { return }
                            self.pendingMutation = true
                            listenBtn?.setLeadingSymbol(nil)
                            listenBtn?.label.stringValue = "…"
                            listenBtn?.setAccessibilityLabel("…")
                            var action: [String: Any] = ["op": "listen_session", "harness": c.harness, "session_id": c.session_id, "cwd": c.cwd, "title": c.custom_title == true ? c.title : candidateTitle]
                            if let pid = c.endpoint_pid { action["endpoint_pid"] = pid }
                            action["hub"] = targetHub
                            self.emit(action)
                        }
                        listenBtn.pressed = listenRun; row.addSubview(listenBtn)
                        candidateRows.append((id: rowId, view: row))
                    }
                    if candidateRows.count <= 4 {
                        for (id, row) in candidateRows { _ = host(row, id: id) }
                    } else {
                        let totalHeight = candidateRows.reduce(CGFloat(0)) { $0 + $1.view.frame.height }
                        let scroll = OverlayScrollView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 176))
                        // Overlay scrollers preserve the authored row width even
                        // when macOS is configured to always show scrollbars.
                        scroll.scrollerStyle = .overlay
                        scroll.drawsBackground = false; scroll.hasVerticalScroller = true
                        scroll.hasHorizontalScroller = false; scroll.autohidesScrollers = true
                        let doc = FlippedView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: totalHeight))
                        var rowY: CGFloat = 0
                        for (_, row) in candidateRows {
                            row.frame.origin = NSPoint(x:0,y:rowY)
                            rowY += row.frame.height
                            doc.addSubview(row)
                        }
                        let scrollId = expandedGroupCount == 1 ? "available_scroll" : "available_scroll:\(harness)"
                        scroll.documentView = doc; _ = host(scroll, id: scrollId, scrollKey:"available_scroll:\(harness)")
                    }
                }
            }
        }
        // Zone 4 — Explicit Directory and Activity entry points.
        if !hubs.isEmpty {
            let dirRow = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 34))
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
                control.layoutContents()
                control.label.frame = NSRect(x: 23, y: 6, width: 76, height: 16)
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
            let entries = dirRow.subviews.filter { $0 is MenuControl }
            layoutBar(in: dirRow, y: 2, height: 28, leading: Array(entries.prefix(1)), trailing: Array(entries.suffix(1)))
            _ = host(dirRow, id: "directory_row")
        }
        let footer = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 36))
        if self.listeners.contains(where: { $0.harness != "human" }) {
            let stopAll = MenuControl("Pause All", frame: NSRect(x: 12, y: 6, width: 82, height: 24), leadingSymbol: "pause.fill")
            stopAll.identifier = NSUserInterfaceItemIdentifier("stop_all")
            stopAll.style = .subtleHeader
            stopAll.pressed = { self.emit(["op": "unlisten_all", "hub":targetHub]) }
            footer.addSubview(stopAll)
        }
        let openSettings = MenuControl("Settings…", frame: NSRect(x: 168, y: 6, width: MenuControl.intrinsicWidth("Settings…", leadingSymbol: "gearshape", paddingX: 6), height: 24), leadingSymbol: "gearshape")
        openSettings.identifier = NSUserInterfaceItemIdentifier("open_settings")
        openSettings.style = .subtleHeader
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
        quit.style = .subtleHeader
        quit.pressed = { self.emit(["op": "quit"]) }
        footer.addSubview(quit)
        let bannerOn=settings?.desktop_banner_enabled != false
        let bell=MenuControl(bannerOn ? "Mute desktop banners" : "Enable desktop banners",frame:NSRect(x:0,y:6,width:24,height:24),symbol:bannerOn ? "bell.fill" : "bell.slash.fill")
        bell.identifier=NSUserInterfaceItemIdentifier("menu_toggle_desktop_banner"); bell.style = .subtleHeader
        bell.toolTip="Local Mac popups only; external channels and Activity remain active."
        bell.pressed={ [weak self] in self?.emit(["op":"set_desktop_banner","enabled":!bannerOn]) }
        footer.addSubview(bell)
        let stopAllControls = footer.subviews.filter { $0.identifier?.rawValue == "stop_all" }
        layoutBar(in: footer, y: 6, height: 24, leading: stopAllControls + [openSettings,bell], trailing: [quit])
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
        ensureEditMainMenu()
        if settingsPanel == nil {
            let window = KeyPanel(contentRect: NSRect(x: 0, y: 0, width: 520, height: 460),
                                  styleMask: [.titled, .fullSizeContentView], backing: .buffered, defer: false)
            window.titleVisibility = .hidden
            window.titlebarAppearsTransparent = true
            for button in [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton] {
                window.standardWindowButton(button)?.isHidden = true
            }
            window.appearance = testAppearance
            window.level = .floating
            window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
            window.isFloatingPanel = true
            window.hidesOnDeactivate = false
            window.isOpaque = false
            window.backgroundColor = .clear
            window.hasShadow = true
            window.isMovableByWindowBackground = true
            window.onKey = { [weak self] key in self?.handleSettingsKey(key) }
            settingsPanel = window
        }
        rebuildSettingsPanel()
    }

    func handleSettingsKey(_ key: String) {
        if (settingsPanel?.firstResponder as? NSTextInputClient)?.hasMarkedText() == true { return }
        if handleTextEditShortcut(key, in: settingsPanel) { return }
        switch key.lowercased() {
        case "command-comma": openSettingsPanel()
        case "command-w": closeSettingsPanel(emitAction:true)
        case "escape":
            if uninstallConfirmOpen { uninstallConfirmOpen = false; uninstallPurgeData = false; rebuildSettingsPanel() }
            else if addHubFormOpen { addHubFormOpen = false; pendingHubSave = nil; inviteParseRequest=""; rebuildSettingsPanel() }
            else if channelUiStates.values.contains(where: { $0.editingGateway }) {
                for (hub, channels) in channelSettingsByHub {
                    for ch in channels where channelUiStates["\(hub)|\(ch.id)"]?.editingGateway == true {
                        clearChannelDrafts(ch, scope:"hub", hub:hub)
                        channelUiStates["\(hub)|\(ch.id)"]?.resetGateway()
                    }
                }
                rebuildSettingsPanel()
            }
            else if channelUiStates.values.contains(where: { $0.editingPersonal }) {
                for (hub, channels) in channelSettingsByHub {
                    for ch in channels where channelUiStates["\(hub)|\(ch.id)"]?.editingPersonal == true {
                        clearChannelDrafts(ch, scope:"user", hub:hub)
                        channelUiStates["\(hub)|\(ch.id)"]?.resetPersonal()
                    }
                }
                rebuildSettingsPanel()
            }
            else { closeSettingsPanel(emitAction:true) }
        case "return":
            if uninstallConfirmOpen { return }
            let id = (settingsPanel?.firstResponder as? NSTextView)?.delegate as? NSTextField
            let field=id?.identifier?.rawValue ?? ""
            let actionId: String
            if field == "host_hub_port" { actionId="apply_host_hub_port" }
            else if field.hasPrefix("channel_field:"), let channel=(channelSettingsByHub[effectiveChannelHub] ?? []).first(where:{field.hasPrefix("channel_field:\($0.id):")}) {
                let scope=channel.fields.first(where:{field=="channel_field:\(channel.id):\($0.key)"})?.scope
                actionId=(scope == "hub" ? "save_hub_service:" : "save_channel:")+channel.id
            } else if addHubFormOpen { actionId="submit_add_hub" }
            else { return }
            if let root=settingsPanel?.contentView, let control=findControl(in:root,id:actionId), control.isEnabled { control.pressed?() }
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
        let scroll = OverlayScrollView(frame: NSRect(x: 0, y: 44, width: 520, height: 368 - (settingsBannerError == nil ? 0 : 48)))
        scroll.scrollerStyle = .overlay
        scroll.identifier = NSUserInterfaceItemIdentifier(id)
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        scroll.isHidden = content.isHidden
        scroll.alphaValue = uninstallConfirmOpen ? 0.35 : 1
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
        header.alphaValue = uninstallConfirmOpen ? 0.35 : 1
        header.addSubview(textLabel("Cito", NSRect(x: 16, y: 15, width: 38, height: 18), size: 13, weight: .bold))

        let tabWidths = [("Agent Apps","square.grid.2x2"),("Human Channels","bubble.left.and.bubble.right"),("Hubs & Network","network")].map {
            MenuControl.intrinsicWidth($0.0, leadingSymbol:$0.1, paddingX:12)
        }
        let tabsWidth = tabWidths.reduce(0,+) + 12
        let tabsTrack = roundedView(NSRect(x:62+floor((400-tabsWidth)/2),y:10,width:tabsWidth,height:28),radius:9,color:.labelColor.withAppearanceAlpha(0.06),border:.labelColor.withAppearanceAlpha(0.08))
        tabsTrack.identifier = NSUserInterfaceItemIdentifier("settings_tab_track")
        header.addSubview(tabsTrack)
        let appsTab = MenuControl("Agent Apps", frame: NSRect(x: 2, y: 2, width: tabWidths[0], height: 24), leadingSymbol: "square.grid.2x2", radius: 7)
        appsTab.identifier = NSUserInterfaceItemIdentifier("settings_tab:apps")
        appsTab.style = settingsActiveTab == "apps" ? .prominent : .subtleHeader
        appsTab.pressed = { [weak self] in
            guard let self = self else { return }
            self.syncAddHubFieldsFromControls()
            self.uninstallConfirmOpen = false
            self.settingsActiveTab = "apps"
            self.rebuildSettingsPanel()
        }
        tabsTrack.addSubview(appsTab)

        let channelsTab = MenuControl("Human Channels", frame: NSRect(x: tabWidths[0]+6, y: 2, width: tabWidths[1], height: 24), leadingSymbol: "bubble.left.and.bubble.right", radius: 7)
        channelsTab.identifier = NSUserInterfaceItemIdentifier("settings_tab:channels")
        channelsTab.style = settingsActiveTab == "channels" ? .prominent : .subtleHeader
        channelsTab.pressed = { [weak self] in
            guard let self = self else { return }
            self.syncAddHubFieldsFromControls()
            self.uninstallConfirmOpen = false
            self.settingsActiveTab = "channels"
            self.rebuildSettingsPanel()
        }
        tabsTrack.addSubview(channelsTab)

        let hubsTab = MenuControl("Hubs & Network", frame: NSRect(x: tabWidths[0]+tabWidths[1]+10, y: 2, width: tabWidths[2], height: 24), leadingSymbol: "network", radius: 7)
        hubsTab.identifier = NSUserInterfaceItemIdentifier("settings_tab:hubs")
        hubsTab.style = settingsActiveTab == "hubs" ? .prominent : .subtleHeader
        hubsTab.pressed = { [weak self] in
            guard let self = self else { return }
            self.syncAddHubFieldsFromControls()
            self.uninstallConfirmOpen = false
            self.settingsActiveTab = "hubs"
            self.rebuildSettingsPanel()
        }
        tabsTrack.addSubview(hubsTab)

        let closeBtn = MenuControl("Close", frame: NSRect(x: CitoMetrics.settingsWidth - CitoMetrics.settingsInset - 24, y: 12, width: 24, height: 24), symbol: "xmark")
        closeBtn.identifier = NSUserInterfaceItemIdentifier("close_settings")
        closeBtn.style = .subtleHeader
        closeBtn.toolTip = "Close Settings"
        closeBtn.pressed = { [weak self] in self?.closeSettingsPanel(emitAction: true) }
        header.addSubview(closeBtn)
        header.addSubview(roundedView(NSRect(x: 0, y: 0, width: 520, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
        root.addSubview(header)
        if let error = settingsBannerError, !uninstallConfirmOpen {
            let banner=roundedView(NSRect(x:20,y:368,width:480,height:40),radius:6,color:CitoPalette.dangerText.withAppearanceAlpha(0.10))
            banner.identifier=NSUserInterfaceItemIdentifier("settings_error_banner")
            let label=textLabel(presentationError(error),NSRect(x:10,y:7,width:432,height:26),size:10,color:CitoPalette.dangerText)
            label.maximumNumberOfLines=2; label.lineBreakMode = .byTruncatingMiddle; label.toolTip=label.stringValue
            label.identifier=NSUserInterfaceItemIdentifier("settings_error_label"); banner.addSubview(label)
            let dismiss=MenuControl("Dismiss",frame:NSRect(x:450,y:10,width:22,height:20),symbol:"xmark")
            dismiss.identifier=NSUserInterfaceItemIdentifier("dismiss_settings_error")
            dismiss.style = .subtleHeader
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
        appsView.addSubview(textLabel("AGENT APPS", NSRect(x:20,y:338,width:240,height:14),size:10,weight:.semibold,color:CitoPalette.secondaryText))
        let map = harnessMap
        var appTop: CGFloat = 328
        for app in resolvedApps {
            let diagnostic = !["Click Set Up to connect", "Connected", "Connected and ready", ""].contains(app.detail)
                && !app.detail.hasPrefix("Connected · ")
                && !app.detail.hasPrefix("Disconnected · ")
            let expanded = !app.installed || app.status == "error" || app.status == "repair_needed" || diagnostic
            let cardHeight: CGFloat = expanded ? 76 : 52
            appTop -= cardHeight
            let card = roundedView(NSRect(x:20,y:appTop,width:480,height:cardHeight),radius:10,
                color:.labelColor.withAppearanceAlpha(0.045),border:.labelColor.withAppearanceAlpha(0.08))
            card.identifier = NSUserInterfaceItemIdentifier("settings_app:\(app.id)")
            let barY: CGFloat = expanded ? 40 : 14
            card.addSubview(makeHarnessBadge(NSRect(x:12,y:barY-2,width:28,height:28),radius:7,iconInset:6,harness:app.id,harnesses:map))
            let titleW = min(app.isConfigured ? 184 : 180, measureTextWidth(app.display_name,font:.systemFont(ofSize:13,weight:.semibold)) + 2)
            let title = textLabel(app.display_name,NSRect(x:50,y:barY+3.5,width:titleW,height:17),size:13,weight:.semibold)
            title.toolTip = app.display_name; title.lineBreakMode = .byTruncatingMiddle
            card.addSubview(title)
            var leading: [NSView] = [title]
            if app.tier == "experimental" {
                let tier = makePillBadge("Experimental", id: "settings_app_tier:\(app.id)", textColor: CitoPalette.warningText, fill: CitoPalette.warningSurface, border: CitoPalette.warningBorder)
                card.addSubview(tier); leading.append(tier)
            }
            if expanded {
                let detailText = diagnostic ? presentationError(app.detail) : (!app.installed ? "Install this app to connect." : "")
                let detail = textLabel(detailText,NSRect(x:50,y:8,width:418,height:26),size:11,color:CitoPalette.secondaryText)
                detail.maximumNumberOfLines=2; detail.lineBreakMode = .byTruncatingMiddle
                detail.toolTip=([presentationError(app.detail)]+app.paths).joined(separator:"\n")
                card.addSubview(detail)
            }
            let statusText = app.isConfigured ? "Connected" : (app.status == "error" ? "Inspection failed" : (app.status == "repair_needed" ? "Repair Needed" : "Not Set Up"))
            let status = statusLabel(statusText,frame:NSRect(x:0,y:0,width:app.isConfigured ? 84 : 112,height:16),color:app.isConfigured ? CitoPalette.successText : CitoPalette.secondaryText,filled:app.isConfigured)
            status.identifier = NSUserInterfaceItemIdentifier("settings_app_status:\(app.id)")
            status.isHidden = app.status == "not_configured"
            card.addSubview(status)
            let appActionWidth = max(76, MenuControl.intrinsicWidth("Disconnect"))
            let btn = MenuControl(app.isConfigured || app.status == "error" ? "Re-check" : (app.status == "repair_needed" ? "Repair" : "Set Up"),frame:NSRect(x:0,y:0,width:app.isConfigured ? MenuControl.intrinsicWidth("✓ Checked") : appActionWidth,height:24))
            btn.identifier = NSUserInterfaceItemIdentifier("setup_app:\(app.id)")
            btn.style = app.isConfigured ? .neutral : .prominent
            if Date() < (appCheckedUntil[app.id] ?? .distantPast) { btn.copied("✓ Checked") }
            btn.pressed = { [weak self] in
                let request = UUID().uuidString
                if app.isConfigured, let self = self {
                    self.pendingAppChecks = self.pendingAppChecks.filter { $0.value != app.id }
                    self.pendingAppChecks[request] = app.id
                }
                self?.emit(["op":"setup_apply","harness":app.id,"request_id":request])
            }
            card.addSubview(btn)
            var trailing: [NSView] = status.isHidden ? [btn] : [status,btn]
            if app.isConfigured {
                let remove = MenuControl("Disconnect",frame:NSRect(x:0,y:0,width:appActionWidth,height:24))
                remove.identifier = NSUserInterfaceItemIdentifier("remove_app:\(app.id)"); remove.style = .destructiveHover
                remove.pressed = { [weak self] in self?.emit(["op":"setup_remove_app","harness":app.id]) }
                card.addSubview(remove); trailing.append(remove)
            }
            let barSpacing: CGFloat = 5
            let trailingWidth = trailing.reduce(CGFloat(0)) { $0 + $1.frame.width + barSpacing }
            let tierWidth: CGFloat = leading.count > 1 ? (88 + barSpacing) : 0
            title.frame.size.width = max(0,min(titleW,418 - trailingWidth - tierWidth))
            layoutBar(in:card,insetLeft:50,y:barY,height:24,spacing:barSpacing,leading:leading,trailing:trailing)
            appsView.addSubview(card); appTop -= 8
        }
        mountSettingsCards(appsView, in: root)

        // Tab 2: Human Channels (520 × 368 pt, y: 44)
        let channelsView = NSView(frame: NSRect(x: 0, y: 44, width: 520, height: 368))
        channelsView.identifier = NSUserInterfaceItemIdentifier("settings_channels_tab")
        channelsView.isHidden = settingsActiveTab != "channels"

        let bannerEnabled = settings?.desktop_banner_enabled ?? true
        var cursorY: CGFloat = 358
        cursorY -= 60
        let bannerCard = roundedView(NSRect(x: CitoMetrics.settingsInset, y: cursorY, width: 480, height: 60), radius: 10,
            color: .labelColor.withAppearanceAlpha(0.045), border: .labelColor.withAppearanceAlpha(0.08))
        bannerCard.identifier = NSUserInterfaceItemIdentifier("settings_desktop_banner_card")
        bannerCard.addSubview(textLabel("Mac Desktop Banner", NSRect(x: 14, y: 33, width: 240, height: 17), size: 13, weight: .semibold))
        bannerCard.addSubview(textLabel("Local popups only. External channels and Activity stay active.",NSRect(x:14,y:12,width:452,height:15),size:10.5,color:CitoPalette.secondaryText))
        let toggleBanner = MenuControl(bannerEnabled ? "Enabled" : "Disabled", frame: NSRect(x: 386, y: 11, width: 80, height: 24), radius: 12)
        toggleBanner.identifier = NSUserInterfaceItemIdentifier("toggle_desktop_banner")
        toggleBanner.style = .neutral
        toggleBanner.setLeadingDot(bannerEnabled ? CitoPalette.successText : CitoPalette.secondaryText,ring:!bannerEnabled)
        toggleBanner.pressed = { [weak self] in
            self?.emit(["op": "set_desktop_banner", "enabled": !bannerEnabled])
        }
        bannerCard.addSubview(toggleBanner)
        layoutBar(in: bannerCard, insetRight: CitoMetrics.headerInset, y: 30, height: 24, trailing: [toggleBanner])
        channelsView.addSubview(bannerCard)

        cursorY -= 12
        if settings?.hubs.first(where:{$0.name == effectiveChannelHub})?.human_paused == true {
            cursorY -= 44
            let paused=roundedView(NSRect(x:20,y:cursorY,width:480,height:44),radius:8,color:CitoPalette.warningSurface)
            paused.identifier=NSUserInterfaceItemIdentifier("human_paused_banner")
            let title=textLabel("Hub paused — Mac and external channels are paused",NSRect(x:12,y:14,width:370,height:16),size:11,color:CitoPalette.warningText)
            paused.addSubview(title)
            let resume=MenuControl("Resume",frame:NSRect(x:0,y:10,width:MenuControl.intrinsicWidth("Resume"),height:24))
            resume.identifier=NSUserInterfaceItemIdentifier("resume_human_channels")
            resume.pressed={ [weak self] in guard let self=self else{return}; self.emit(["op":"listen_session","hub":self.effectiveChannelHub,"harness":"human","session_id":"default","title":"Human","about":self.settings?.hubs.first(where:{$0.name==self.effectiveChannelHub})?.human_about ?? "Human receiver"]) }
            paused.addSubview(resume)
            layoutBar(in:paused,y:10,height:24,flexible:title,trailing:[resume])
            channelsView.addSubview(paused); cursorY -= 10
        }
        let channelHubs = settings?.hubs.filter { $0.enabled != false }.map { $0.name } ?? hubs.filter { $0.enabled != false }.map { $0.name }
        if channelHubs.count > 1 { channelsView.addSubview(textLabel("Target Hub", NSRect(x: 20, y: cursorY-22, width: 78, height: 14), size: 10, weight: .semibold, color: CitoPalette.secondaryText)) }
        var channelPills: [NSView] = []
        for name in channelHubs {
            let width = max(64, measureTextWidth(name, font: .systemFont(ofSize: 11)) + 24)
            let pill = MenuControl(name, frame: NSRect(x: 0, y: 3, width: width, height: 22), radius: 11)
            pill.identifier = NSUserInterfaceItemIdentifier("channel_hub_selector:\(name)")
            pill.style = effectiveChannelHub == name ? .prominent : .subtleHeader
            pill.pressed = { [weak self] in
                guard let self = self else { return }
                self.selectedChannelHub = name
                self.emit(["op": "open_settings", "hub": name])
                self.rebuildSettingsPanel()
            }
            channelPills.append(pill)
        }
        if channelHubs.count > 1 {
            let strip = mountPillStrip(in: channelsView, frame: NSRect(x: 102, y: cursorY-26, width: 398, height: 26),
                           id: "channel_hub_strip", spacing: 6, pills: channelPills) {
                $0.identifier?.rawValue == "channel_hub_selector:\(effectiveChannelHub)"
            }
            strip.wantsLayer = true
            strip.backgroundColor = .labelColor.withAppearanceAlpha(0.06)
            strip.drawsBackground = true
            strip.layer?.cornerRadius = 13
            strip.layer?.masksToBounds = true
            cursorY -= 36
        }

        let resolvedChannels = channelSettingsByHub[effectiveChannelHub] ?? []
        var chCursorY: CGFloat = cursorY
        for ch in resolvedChannels {
            let visibleFields = ch.fields.filter { $0.hidden != true }
            let channelKey = "\(effectiveChannelHub)|\(ch.id)"
            let channelUi = channelUiStates[channelKey] ?? ChannelUiState()
            let instruction = channelUi.pairingInstruction
            let instructionHeight = instruction.isEmpty ? CGFloat(0) : max(16, ceil((instruction as NSString).boundingRect(with: NSSize(width: 432, height: CGFloat.greatestFiniteMagnitude), options: [.usesLineFragmentOrigin], attributes: [.font: NSFont.monospacedSystemFont(ofSize: 10.5, weight: .semibold)]).height) + 4)
            let guide = (ch.setup_guide ?? []).joined(separator:"\n")
            let hubFields = visibleFields.filter { $0.scope == "hub" }
            let disclosure = ch.pairing_supported && ch.pairing_manual_disclosure == true
            let manualExpanded = channelUi.manualEntry
            let userFields = visibleFields.filter { $0.scope != "hub" }
            let formFields = disclosure && !manualExpanded ? [] : userFields
            let hasHubScopeFields = ch.fields.contains { $0.scope == "hub" }
            let hubFieldsH = CGFloat(hubFields.count) * 26
            let userFieldsH = CGFloat(formFields.count) * 26 + (disclosure && instruction.isEmpty ? 24 : 0)
            let gatewayActive = !hasHubScopeFields || ch.hub_provided || ch.status != "not_configured" || (hasHubScopeFields && ch.fields.filter { $0.scope == "hub" }.allSatisfy { $0.configured })
            let hubGatewayExpanded = !gatewayActive || channelUi.editingGateway
            let personalCollapsed = gatewayActive && ch.configured && !channelUi.editingPersonal && instruction.isEmpty
            let guideH: CGFloat = !gatewayActive && !guide.isEmpty && !hubFields.isEmpty ? max(13,ceil((guide as NSString).boundingRect(with:NSSize(width:432,height:CGFloat.greatestFiniteMagnitude),options:[.usesLineFragmentOrigin],attributes:[.font:NSFont.systemFont(ofSize:10)]).height)+2) : 0
            let currentUser=settings?.hubs.first(where:{$0.name==effectiveChannelHub})?.user ?? hubs.first(where:{$0.name==effectiveChannelHub})?.user ?? "local"
            let gatewayRole = !hubFields.isEmpty ? (guideH > 0 ? "" : "Configured once by the Hub owner. Shared by everyone on this Hub.") :
                (hasHubScopeFields ? (ch.hub_provided ? "Provided by '\(effectiveChannelHub)' Hub administrator — no gateway setup needed on your Mac." : "Not yet configured by the '\(effectiveChannelHub)' Hub administrator.") : "No shared gateway credentials required for this channel.")
            func roleHeight(_ text: String) -> CGFloat {
                text.isEmpty ? 0 : max(13,ceil((text as NSString).boundingRect(with:NSSize(width:432,height:CGFloat.greatestFiniteMagnitude),options:[.usesLineFragmentOrigin],attributes:[.font:NSFont.systemFont(ofSize:10)]).height))
            }
            let gatewayRoleH=roleHeight(gatewayRole)
            let hubStratumH: CGFloat = hubGatewayExpanded ? 36 + gatewayRoleH + guideH + hubFieldsH + (!hubFields.isEmpty ? 32 : 0) : 36
            let userStratumH: CGFloat = !gatewayActive ? 0 : (personalCollapsed ? 36 : 36 + userFieldsH + (instructionHeight > 0 ? instructionHeight + 34 : 0) + 34)
            let channelResult = ch.warning == true || ch.status == "error" ? nil : channelUi.testResult
            let showDetail = ch.status == "error" || ch.warning == true || channelResult != nil
            let detailHeight = showDetail ? max(14,ceil((presentationError(channelResult?.detail ?? ch.detail) as NSString).boundingRect(with:NSSize(width:422,height:CGFloat.greatestFiniteMagnitude),options:[.usesLineFragmentOrigin],attributes:[.font:NSFont.systemFont(ofSize:10.5)]).height)) : 0
            let headerHeight: CGFloat = showDetail ? 38 + detailHeight : 38
            let unavailable = ch.status == "error" && ch.fields.isEmpty
            let cardHeight: CGFloat = unavailable ? max(56, headerHeight + 8) : headerHeight + hubStratumH + CitoMetrics.gap + userStratumH + 8
            chCursorY -= cardHeight
            let chCard = roundedView(NSRect(x: 20, y: chCursorY, width: 480, height: cardHeight), radius: 10,
                color: .labelColor.withAppearanceAlpha(0.045), border: .labelColor.withAppearanceAlpha(0.08))
            chCard.identifier = NSUserInterfaceItemIdentifier("settings_channel:\(ch.id)")

            let chBadge = roundedView(NSRect(x: 12, y: cardHeight - 34, width: 24, height: 24), radius: 6,
                color: CitoPalette.badgeFill(ch.tint), border: .white.withAlphaComponent(0.18))
            let chIcon = NSImageView(frame: NSRect(x: 5, y: 5, width: 14, height: 14))
            configureBadge(chBadge,icon:chIcon,id:ch.id,symbol:ch.sf_symbol,title:ch.display_name,tint:CitoPalette.badgeFill(ch.tint),inset:5)
            chBadge.addSubview(chIcon)
            chCard.addSubview(chBadge)

            let chTitle = textLabel(ch.display_name, NSRect(x: 44, y: cardHeight - 26, width: 180, height: 16), size: 12.5, weight: .semibold)
            chTitle.lineBreakMode = .byTruncatingMiddle; chTitle.toolTip = ch.display_name
            chCard.addSubview(chTitle)
            let chStatusText:String
            let chStatusColor:NSColor
            switch channelResult?.ok == false ? "error" : ch.status {
            case "ready": chStatusText = "Connected"; chStatusColor = CitoPalette.successText
            case "paused": chStatusText = "⏸ Paused"; chStatusColor = CitoPalette.warningText
            case "pairing_pending": chStatusText = "◌ Pairing…"; chStatusColor = CitoPalette.infoText
            case "error": chStatusText = "⚠ Error"; chStatusColor = CitoPalette.dangerText
            default: chStatusText = ch.hub_provided || ch.status == "hub_ready" ? "Account Not Paired" : "Not Set Up"; chStatusColor = CitoPalette.secondaryText
            }
            let chStatusLabel = statusLabel(chStatusText,frame:NSRect(x:240,y:cardHeight-26,width:226,height:16),color:chStatusColor,filled:ch.status == "ready")
            chStatusLabel.identifier = NSUserInterfaceItemIdentifier("settings_channel_status:\(ch.id)")
            chCard.addSubview(chStatusLabel)
            layoutBar(in:chCard,insetLeft:44,insetRight:14,y:cardHeight-26,height:16,flexible:chTitle,trailing:[chStatusLabel])

            let chDetail = textLabel(presentationError(channelResult?.detail ?? ch.detail), NSRect(x: 44, y: cardHeight - 32 - detailHeight, width: 422, height: detailHeight), size: 10.5, color:channelResult?.ok == false || ch.status == "error" ? CitoPalette.dangerText : (ch.warning == true ? CitoPalette.warningText : CitoPalette.secondaryText))
            chDetail.maximumNumberOfLines=0; chDetail.lineBreakMode = .byWordWrapping; chDetail.toolTip = chDetail.stringValue
            chDetail.identifier = NSUserInterfaceItemIdentifier("settings_channel_detail:\(ch.id)")
            if showDetail { chCard.addSubview(chDetail) }
            if unavailable {
                channelsView.addSubview(chCard)
                chCursorY -= 10
                continue
            }

            let hubStratum = roundedView(NSRect(x:12,y:8+userStratumH+6,width:456,height:hubStratumH),radius:8,
                color:.labelColor.withAppearanceAlpha(0.035),border:nil)
            hubStratum.identifier=NSUserInterfaceItemIdentifier("stratum_hub:\(ch.id)")
            var gatewayTop: [NSView] = [], gatewayLeading: [NSView] = [], gatewayTrailing: [NSView] = []
            var personalActions: [NSView] = [], pairingActions: [NSView] = []
            let cleanConnectedState = personalCollapsed && !hubGatewayExpanded
            let hubTitle=textLabel(ch.configured ? "Bot Gateway" : "1. Shared Bot Gateway",NSRect(x:10,y:hubStratumH-25,width:168,height:14),size:9.5,weight:.bold,color:CitoPalette.secondaryText)
            hubTitle.identifier=NSUserInterfaceItemIdentifier("stratum_hub_title:\(ch.id)")
            if !gatewayRole.isEmpty { hubTitle.toolTip = gatewayRole }
            hubStratum.addSubview(hubTitle)
            let gatewayStatus=statusLabel(gatewayActive ? "Bot Ready" : "Gateway Token Required",frame:NSRect(x:0,y:hubStratumH-25,width:gatewayActive ? 76 : 164,height:14),color:gatewayActive ? CitoPalette.successText : CitoPalette.secondaryText,filled:gatewayActive,size:9)
            gatewayStatus.identifier=NSUserInterfaceItemIdentifier("stratum_hub_status:\(ch.id)")
            gatewayStatus.isHidden = cleanConnectedState
            hubStratum.addSubview(gatewayStatus)
            if gatewayActive && !hubFields.isEmpty {
                let clear=MenuControl("Clear Token",frame:NSRect(x:0,y:7,width:MenuControl.intrinsicWidth("Clear Token", font:.systemFont(ofSize:10.5)),height:22),radius:6)
                clear.label.font = .systemFont(ofSize:10.5)
                clear.style = .destructiveHover
                clear.identifier=NSUserInterfaceItemIdentifier("clear_channel_service:\(ch.id)")
                clear.pressed={ [weak self] in guard let self=self else{return}; self.clearChannelDrafts(ch); self.emit(["op":"disconnect_human_channel","hub":self.effectiveChannelHub,"channel_id":ch.id,"reset_hub":true,"reset_user":false]) }
                hubStratum.addSubview(clear)
                if hubGatewayExpanded { gatewayLeading.append(clear) } else { gatewayTop.append(clear) }
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
            let userStratum=roundedView(NSRect(x:12,y:8,width:456,height:userStratumH),radius:8,
                color:.labelColor.withAppearanceAlpha(0.035),border:nil)
            userStratum.identifier=NSUserInterfaceItemIdentifier("stratum_user:\(ch.id)")
            let userTitle=textLabel((ch.configured ? "" : "2. ") + "Your Personal Account (@\(currentUser))",NSRect(x:12,y:userStratumH-21,width:432,height:13),size:9.5,weight:.bold,color:CitoPalette.secondaryText)
            userTitle.lineBreakMode = .byTruncatingMiddle; userTitle.toolTip = userTitle.stringValue
            userTitle.identifier=NSUserInterfaceItemIdentifier("stratum_user_title:\(ch.id)")
            if !personalCollapsed { userStratum.addSubview(userTitle) }
            for (container, fields, top) in [(hubStratum,hubGatewayExpanded ? hubFields : [],hubStratumH-34-gatewayRoleH-guideH),(userStratum,gatewayActive && !personalCollapsed ? formFields : [],userStratumH-30)] {
                let labelW = max(56, min(110, (fields.map { ceil(measureTextWidth($0.label, font: .systemFont(ofSize: 11, weight: .medium))) }.max() ?? 56) + 6))
                let inputX = 12 + labelW + 10
                let inputW = container.bounds.width - 12 - inputX
                var fieldY=top-22
                for f in fields {
                    let fieldId="channel_field:\(ch.id):\(f.key)"
                    let labelView=textLabel(f.label,NSRect(x:12,y:fieldY+3,width:labelW,height:15),size:11,weight:.medium)
                    labelView.toolTip=f.label; container.addSubview(labelView)
                    let initialVal=channelUi.drafts[f.key] ?? (f.value ?? "")
                    let inputField=makeEditableField(fieldId,frame:NSRect(x:inputX,y:fieldY,width:inputW,height:22),placeholder:f.type=="secret" && f.configured ? (f.configured_placeholder ?? "•••••••• (Saved — enter new value to replace)") : f.placeholder,value:initialVal,secret:f.type=="secret")
                    inputField.toolTip=f.help_text; container.addSubview(inputField)
                    fieldY-=26
                }
            }
            chCard.addSubview(hubStratum)
            if gatewayActive { chCard.addSubview(userStratum) }
            func gate(_ control: MenuControl) { control.isEnabled=gatewayActive }
            if !instruction.isEmpty {
                let instrLabel = textLabel(instruction, NSRect(x: 12, y: 68, width: 432, height: instructionHeight), size: 10.5, weight: .semibold, color: CitoPalette.infoText)
                instrLabel.maximumNumberOfLines = 0
                instrLabel.lineBreakMode = .byWordWrapping
                instrLabel.isSelectable = true
                instrLabel.toolTip = instruction
                instrLabel.font = .monospacedSystemFont(ofSize: 10.5, weight: .semibold)
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
                    let draft=self.channelUiStates[channelKey]?.drafts[f.key]
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
                    hubStratum.addSubview(save); gatewayTrailing.append(save)
                    if gatewayActive {
                        let cancel=MenuControl("Cancel",frame:NSRect(x:232,y:6,width:66,height:24))
                        cancel.identifier=NSUserInterfaceItemIdentifier("cancel_edit_hub_service:\(ch.id)")
                        cancel.pressed={ [weak self] in self?.clearChannelDrafts(ch, scope:"hub"); self?.channelUiStates[channelKey]?.resetGateway(); self?.rebuildSettingsPanel() }
                        hubStratum.addSubview(cancel); gatewayTrailing.insert(cancel, at: 0)
                    }
                } else {
                    let edit=MenuControl("Change Token…",frame:NSRect(x:0,y:7,width:MenuControl.intrinsicWidth("Change Token…", font:.systemFont(ofSize:10.5)),height:22))
                    edit.style = .neutral
                    edit.label.font = .systemFont(ofSize:10.5)
                    edit.identifier=NSUserInterfaceItemIdentifier("edit_hub_service:\(ch.id)")
                    edit.pressed={ [weak self] in self?.channelUiStates[channelKey, default: ChannelUiState()].editingGateway = true; self?.rebuildSettingsPanel() }
                    hubStratum.addSubview(edit); gatewayTop.insert(edit, at: 0)
                }
            }
            if disclosure && !personalCollapsed && gatewayActive && instruction.isEmpty {
                let manual=MenuControl(manualExpanded ? "Hide manual entry" : "Enter Chat ID manually…",frame:NSRect(x:12,y:40,width:190,height:22),paddingX:0)
                manual.label.alignment = .left; manual.layoutContents()
                manual.style = .subtleHeader; manual.identifier=NSUserInterfaceItemIdentifier("manual_channel:\(ch.id)")
                manual.pressed={ [weak self] in guard let self=self else{return}; self.channelUiStates[channelKey, default: ChannelUiState()].manualEntry.toggle(); self.rebuildSettingsPanel() }
                userStratum.addSubview(manual)
            }
            if ch.pairing_supported && !personalCollapsed && gatewayActive && instruction.isEmpty {
                let pairLabel = ch.configured ? "Re-pair…" : (ch.pairing_action_label ?? "Pair Account…")
                let pairBtn = MenuControl(pairLabel, frame: NSRect(x: 12, y: 6, width: MenuControl.intrinsicWidth(pairLabel, leadingSymbol: "link"), height: 24), leadingSymbol: "link", radius: 6)
                pairBtn.toolTip=ch.pairing_help_text
                pairBtn.identifier = NSUserInterfaceItemIdentifier("pair_channel:\(ch.id)")
                pairBtn.pressed = { [weak self] in
                    guard let self = self else { return }
                    self.emit(["op": "channel_pair", "hub": self.effectiveChannelHub, "channel_id": chId, "values": collectValues("user")])
                }
                gate(pairBtn); pairBtn.style = ch.configured ? .neutral : .prominent; userStratum.addSubview(pairBtn); personalActions.append(pairBtn)
            }

            if !instruction.isEmpty {
                let copy=MenuControl("Copy Command",frame:NSRect(x:266,y:42,width:114,height:22),leadingSymbol:"doc.on.doc",radius:6)
                copy.label.font = .systemFont(ofSize:10.5)
                copy.identifier=NSUserInterfaceItemIdentifier("copy_pair_command:\(ch.id)")
                copy.pressed={ [weak self, weak copy] in
                    self?.copyText(channelUi.pairingCommand ?? instruction, control: copy)
                }
                if Date() < channelUi.pairCopiedUntil { copy.copied() }
                if let raw=channelUi.pairingURL, let url=URL(string:raw) {
                    let open=MenuControl(ch.pairing_open_url_label ?? "Open Pairing Link ↗",frame:NSRect(x:12,y:42,width:150,height:22))
                    open.identifier=NSUserInterfaceItemIdentifier("open_pair_url:\(ch.id)"); open.style = .prominent
                    open.pressed={ if !usesPrivatePasteboard { NSWorkspace.shared.open(url) } }
                    userStratum.addSubview(open); pairingActions.append(open)
                }
                userStratum.addSubview(copy); pairingActions.append(copy)
                let cancel=MenuControl("Cancel",frame:NSRect(x:386,y:42,width:58,height:22),radius:6)
                cancel.identifier=NSUserInterfaceItemIdentifier("cancel_pair_channel:\(ch.id)")
                cancel.pressed={ [weak self] in
                    guard let self=self else{return}
                    self.emit(["op":"channel_cancel_pair","hub":self.effectiveChannelHub,"channel_id":chId])
                }
                userStratum.addSubview(cancel); pairingActions.append(cancel)
            }
            if ch.configured {
                let toggle=MenuControl(ch.enabled ? "Pause" : "Resume",frame:NSRect(x:130,y:6,width:MenuControl.intrinsicWidth(ch.enabled ? "Pause" : "Resume"),height:24),radius:6)
                toggle.identifier=NSUserInterfaceItemIdentifier("toggle_channel:\(ch.id)")
                toggle.style = ch.enabled ? .neutral : .prominent
                toggle.pressed={ [weak self] in guard let self=self else{return}; self.emit(["op":"channel_toggle","hub":self.effectiveChannelHub,"channel_id":chId,"enabled":!ch.enabled]) }
                userStratum.addSubview(toggle); personalActions.append(toggle)
                let unpair=MenuControl("Unpair",frame:NSRect(x:196,y:6,width:MenuControl.intrinsicWidth("Unpair"),height:24),radius:6)
                unpair.style = .destructiveHover
                unpair.identifier=NSUserInterfaceItemIdentifier("unpair_channel:\(ch.id)")
                unpair.pressed={ [weak self] in guard let self=self else{return}; self.clearChannelDrafts(ch, scope:"user"); self.emit(["op":"channel_unpair","hub":self.effectiveChannelHub,"channel_id":chId,"reset_user":true]) }
                userStratum.addSubview(unpair); personalActions.append(unpair)
            }
            let testBtn = MenuControl("Send Test", frame: NSRect(x: 290, y: 6, width: MenuControl.intrinsicWidth("Send Test"), height: 24), radius: 6)
            testBtn.identifier = NSUserInterfaceItemIdentifier("test_channel:\(ch.id)")
            testBtn.pressed = { [weak self] in
                guard let self = self else { return }
                self.emit(["op": "channel_test", "hub": self.effectiveChannelHub, "channel_id": chId, "values": collectValues("user")])
            }
            gate(testBtn)
            testBtn.isHidden = disclosure && !ch.configured && !manualExpanded
            if disclosure && !ch.configured { testBtn.isEnabled = gatewayActive && !collectValues("user").values.allSatisfy { $0.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty } }
            userStratum.addSubview(testBtn)

            let saveBtn = MenuControl("Save", frame: NSRect(x: 376, y: 6, width: 68, height: 24), radius: 6)
            saveBtn.identifier = NSUserInterfaceItemIdentifier("save_channel:\(ch.id)")
            saveBtn.style = .prominent
            saveBtn.pressed = { [weak self] in
                guard let self = self else { return }
                let values = collectValues("user")
                self.emit(["op": "channel_save", "hub": self.effectiveChannelHub, "channel_id": chId, "enabled": true, "values": values])
            }
            gate(saveBtn)
            if !personalCollapsed && gatewayActive && (!disclosure || manualExpanded) { userStratum.addSubview(saveBtn) } else { saveBtn.isHidden=true }
            if Date() < (channelUi.savedUntil["user"] ?? .distantPast) { saveBtn.copied("✓ Saved") }

            layoutBar(in: hubStratum, y: hubStratumH-29, height: 22, flexible: hubTitle, trailing: [gatewayStatus] + gatewayTop)
            if hubGatewayExpanded { layoutBar(in: hubStratum, y: 8, height: 24, leading: gatewayLeading, trailing: gatewayTrailing) }
            if personalCollapsed {
                let chatId = userFields.first?.value ?? ""
                let saved = Date() < (channelUi.savedUntil["user"] ?? .distantPast)
                let summary = textLabel(saved ? "✓ Saved" : "Paired · \(ch.display_label ?? chatId)", NSRect(x:12,y:10,width:188,height:16),size:10,color:saved ? CitoPalette.successText : CitoPalette.secondaryText)
                summary.identifier = NSUserInterfaceItemIdentifier("channel_summary:\(ch.id)"); summary.toolTip = chatId; summary.lineBreakMode = .byTruncatingMiddle
                userStratum.addSubview(summary)
                let edit = MenuControl("Edit…",frame:NSRect(x:0,y:6,width:MenuControl.intrinsicWidth("Edit…"),height:24))
                edit.identifier = NSUserInterfaceItemIdentifier("edit_user_channel:\(ch.id)"); edit.style = .neutral
                edit.pressed = { [weak self] in self?.channelUiStates[channelKey, default: ChannelUiState()].editingPersonal = true; self?.channelUiStates[channelKey, default: ChannelUiState()].manualEntry = true; self?.rebuildSettingsPanel() }
                userStratum.addSubview(edit)
                for case let button as MenuControl in [testBtn, edit] + personalActions {
                    button.label.font = .systemFont(ofSize: 10.5)
                }
                let toggle = personalActions.first { $0.identifier?.rawValue == "toggle_channel:\(ch.id)" }
                let unpair = personalActions.first { $0.identifier?.rawValue == "unpair_channel:\(ch.id)" }
                layoutBar(in:userStratum,y:6,height:24,spacing:4,flexible:summary,trailing:[testBtn] + [toggle].compactMap { $0 } + [edit] + [unpair].compactMap { $0 })
            } else if gatewayActive {
                var topActions: [NSView] = []
                if ch.configured {
                    topActions = personalActions.filter { $0.identifier?.rawValue.hasPrefix("toggle_channel:") == true || $0.identifier?.rawValue.hasPrefix("unpair_channel:") == true }
                    personalActions.removeAll { view in topActions.contains { $0 === view } }
                    layoutBar(in:userStratum,y:userStratumH-29,height:22,flexible:userTitle,trailing:topActions)
                }
                var trailing: [NSView] = [testBtn,saveBtn]
                if channelUi.editingPersonal {
                    let cancel=MenuControl("Cancel",frame:NSRect(x:0,y:8,width:62,height:24))
                    cancel.identifier=NSUserInterfaceItemIdentifier("cancel_edit_user_channel:\(ch.id)")
                    cancel.pressed={ [weak self] in self?.clearChannelDrafts(ch,scope:"user"); self?.channelUiStates[channelKey]?.resetPersonal(); self?.rebuildSettingsPanel() }
                    userStratum.addSubview(cancel); trailing.insert(cancel,at:0)
                }
                if testBtn.isHidden && saveBtn.isHidden && personalActions.count == 1 {
                    layoutBar(in:userStratum,y:8,height:24,trailing:personalActions)
                } else {
                    layoutBar(in:userStratum,y:8,height:24,leading:personalActions,trailing:trailing)
                }
                if !pairingActions.isEmpty { layoutBar(in: userStratum, y: 40, height: 22, trailing: pairingActions) }
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
        let hostingUnavailable = !hostHub.enabled || !hostHub.running || hostHub.error != nil
        let hostError = (hostHubPortError ?? hostHub.error).flatMap { $0.isEmpty ? nil : $0 } ?? (hostHub.enabled && hostingUnavailable ? (hostHub.running ? "Hub sharing is unavailable" : "Hub server is not running") : nil)
        let hostExpanded = hostHub.enabled || hostHubPortPending || hostHubPortDraft != nil || hostError != nil
        let hasAddressChoice = hostHub.tailscale_url != nil && hostHub.lan_url != nil && hostHub.tailscale_url != hostHub.lan_url
        let hostNotice = hostError ?? (!hostHub.enabled && hostExpanded ? "Sharing is off — enable Share to accept connections" : nil)
        let hostHeight: CGFloat = hostExpanded ? (hasAddressChoice ? 148 : 108) + (hostNotice == nil ? 0 : 28) : 46
        let hostCard = roundedView(NSRect(x: CitoMetrics.settingsInset, y: 356 - hostHeight, width: 480, height: hostHeight), radius: 10,
            color: .labelColor.withAppearanceAlpha(0.045), border: .labelColor.withAppearanceAlpha(0.08))
        hostCard.identifier = NSUserInterfaceItemIdentifier("host_hub_card")
        hostCard.addSubview(textLabel("Share This Mac as a Hub", NSRect(x: 14, y: hostExpanded ? hostHeight - 30.5 : 14.5, width: 350, height: 17), size: 13, weight: .semibold))
        if hostExpanded, let error = hostNotice {
            let subtitle = textLabel(presentationError(error), NSRect(x:14,y:hostHeight-52,width:452,height:18), size:11, color:hostError == nil ? CitoPalette.secondaryText : CitoPalette.dangerText)
            subtitle.identifier = NSUserInterfaceItemIdentifier("host_hub_error")
            subtitle.toolTip = error
            hostCard.addSubview(subtitle)
        }

        let toggleTitle = !hostHub.enabled ? "Share" : hostHub.error != nil ? "Failed" : hostHub.running ? "Sharing" : "Starting"
        let toggleHostBtn = MenuControl(toggleTitle, frame: NSRect(x: 0, y: 0, width: 80, height: 24), radius: 12)
        toggleHostBtn.identifier = NSUserInterfaceItemIdentifier("toggle_host_hub")
        toggleHostBtn.style = hostHub.error != nil ? .destructiveHover : .neutral
        if hostHub.enabled { toggleHostBtn.setLeadingDot(hostHub.error != nil ? CitoPalette.dangerText : (hostHub.running ? CitoPalette.successText : CitoPalette.warningText)) }
        let nextHostEnabled = !hostHub.enabled
        toggleHostBtn.pressed = { [weak self] in
            guard let self=self else{return}
            let port=Int(self.settingsFieldControls["host_hub_port"]?.stringValue ?? self.hostHubPortDraft ?? "") ?? hostHub.port
            self.emit(["op": "set_host_hub", "enabled": nextHostEnabled, "port": port])
        }
        hostCard.addSubview(toggleHostBtn)
        layoutBar(in:hostCard,insetLeft:14,insetRight:14,y:hostExpanded ? hostHeight-34 : 11,height:24,trailing:[toggleHostBtn])

        if hostExpanded {
        let selectedLan = hostHubAddress == "lan" || hostHub.tailscale_url == nil
        let selectedEndpoint = (selectedLan ? hostHub.lan_url : hostHub.tailscale_url) ?? hostHub.endpoint_url
        let selectedInvite = (selectedLan ? hostHub.lan_invite_command : hostHub.tailscale_invite_command) ?? hostHub.invite_command
        if hasAddressChoice {
            let addressTrack = roundedView(NSRect(x:14,y:82,width:0,height:26),radius:8,color:.labelColor.withAppearanceAlpha(0.06),border:.labelColor.withAppearanceAlpha(0.08))
            addressTrack.identifier = NSUserInterfaceItemIdentifier("host_hub_addr_track")
            var addressPills: [NSView] = []
            for (mode,title) in [("tailscale","Tailscale"),("lan","Local IP")] {
                let pill = MenuControl(title,frame:NSRect(x:0,y:78,width:80,height:22))
                pill.identifier = NSUserInterfaceItemIdentifier("host_hub_addr:\(mode)")
                pill.frame.size.width = MenuControl.intrinsicWidth(title)
                pill.style = (selectedLan ? mode == "lan" : mode == "tailscale") ? .prominent : .subtleHeader
                pill.pressed = { [weak self] in self?.hostHubAddress=mode; self?.rebuildSettingsPanel() }
                addressTrack.addSubview(pill); addressPills.append(pill)
            }
            addressTrack.frame.size.width = addressPills.reduce(4) { $0 + $1.frame.width } + 4
            layoutBar(in:addressTrack,insetLeft:2,insetRight:2,y:2,height:22,spacing:4,leading:addressPills)
            hostCard.addSubview(addressTrack)
        }
        let authMode=hostHub.auth_mode ?? (hostHub.token.isEmpty ? "none" : "token")
        let endpointLabel = textLabel(selectedEndpoint, NSRect(x: 14, y: 46, width: 280, height: 16),
            size: 11.5, weight: .medium, color: hostHub.enabled ? .labelColor : CitoPalette.secondaryText)
        endpointLabel.font = .monospacedSystemFont(ofSize: 11.5, weight: .medium)
        endpointLabel.lineBreakMode = .byTruncatingMiddle; endpointLabel.toolTip = endpointLabel.stringValue
        endpointLabel.identifier = NSUserInterfaceItemIdentifier("host_hub_endpoint")
        hostCard.addSubview(endpointLabel)
        var inviteActions: [NSView] = []

        let copyInvite = MenuControl("Copy Invite Code", frame: NSRect(x: 0, y: 48, width: MenuControl.intrinsicWidth("Copy Invite Code", leadingSymbol:"doc.on.doc"), height: 24), leadingSymbol: "doc.on.doc", radius: 6)
        copyInvite.identifier = NSUserInterfaceItemIdentifier("copy_hub_invite")
        copyInvite.style = hostingUnavailable ? .neutral : .prominent
        if !hostHub.enabled { copyInvite.toolTip = "Enable sharing and apply the port before inviting other Macs" }
        else if hostingUnavailable { copyInvite.toolTip = "Hub sharing is unavailable. This invite will work after the server recovers." }
        let inviteCmd = selectedInvite
        copyInvite.pressed = { [weak self, weak copyInvite] in
            self?.copyText(inviteCmd, control:copyInvite, feedback:"✓ Invite Copied")
        }
        hostCard.addSubview(copyInvite); inviteActions.append(copyInvite)
        let portLabel = textLabel("Port:",NSRect(x:0,y:12,width:ceil(measureTextWidth("Port:", font:.systemFont(ofSize:11))) + 4,height:15),size:11)
        let portField = makeEditableField("host_hub_port",frame:NSRect(x:0,y:12,width:58,height:24),placeholder:"Port",value:hostHubPortDraft ?? "\(hostHub.port)")
        hostCard.addSubview(portLabel); hostCard.addSubview(portField)
        let auth=MenuControl(authMode=="none" ? "Access: Trusted" : "Access: Token",frame:NSRect(x:286,y:12,width:112,height:24),radius:6)
        auth.style = .neutral
        auth.identifier=NSUserInterfaceItemIdentifier("toggle_host_hub_auth")
        auth.pressed={ [weak self] in
            guard let self=self else{return}
            let port=Int(self.settingsFieldControls["host_hub_port"]?.stringValue ?? self.hostHubPortDraft ?? "") ?? hostHub.port
            self.emit(["op":"set_host_hub","enabled":hostHub.enabled,"port":port,"auth_mode":authMode=="none" ? "token" : "none"])
        }
        hostCard.addSubview(auth)
        let apply=MenuControl("Apply",frame:NSRect(x:394,y:12,width:72,height:24),radius:6)
        apply.identifier=NSUserInterfaceItemIdentifier("apply_host_hub_port")
        apply.isEnabled = !hostHubPortPending && Int(hostHubPortDraft ?? "\(hostHub.port)") != hostHub.port
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
        layoutBar(in:hostCard,insetLeft:14,insetRight:14,y:48,height:24,flexible:endpointLabel,trailing:inviteActions)
        let copyToken=MenuControl("Copy Token",frame:NSRect(x:0,y:12,width:90,height:24))
        copyToken.identifier=NSUserInterfaceItemIdentifier("copy_host_hub_token"); copyToken.isHidden=hostHub.token.isEmpty
        copyToken.pressed={ [weak self, weak copyToken] in self?.copyText(hostHub.token,control:copyToken) }
        hostCard.addSubview(copyToken)
        layoutBar(in:hostCard,insetLeft:14,insetRight:14,y:12,height:24,leading:[portLabel,portField,apply,copyToken],trailing:[auth])
        }
        hubsView.addSubview(hostCard)
        let hubOffset: CGFloat = hostExpanded ? -(hostHeight - 136) : 80

        hubsView.addSubview(textLabel("CONNECTED HUBS", NSRect(x: 20, y: 194 + hubOffset, width: 180, height: 14),
            size: 10, weight: .semibold, color: CitoPalette.secondaryText))

        let pasteBtn = MenuControl("Paste Invite Code", frame: NSRect(x: 250, y: 190 + hubOffset, width: MenuControl.intrinsicWidth("Paste Invite Code", leadingSymbol:"doc.on.clipboard"), height: 22), leadingSymbol: "doc.on.clipboard", radius: 6)
        pasteBtn.identifier = NSUserInterfaceItemIdentifier("paste_hub_invite")
        if Date() < hubInvitePastedUntil { pasteBtn.copied("✓ Joined") }
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
            self.addHubError=nil; self.pendingHubSave=nil; self.inviteParseRequest=""
            self.addHubFormOpen = wasEditing ? true : !self.addHubFormOpen
            self.rebuildSettingsPanel()
        }
        hubsView.addSubview(addBtn)

        if addHubFormOpen {
            let editingLocalHub = editingExistingHub && editingOriginalHub == "local"
            let localErrorHeight: CGFloat = editingLocalHub && addHubError != nil ? 20 : 0
            let formHeight: CGFloat = editingLocalHub ? 124 + localErrorHeight : 252
            let form = roundedView(NSRect(x: 20, y: 180 + hubOffset - formHeight, width: 480, height: formHeight), radius: 10,
                color: .labelColor.withAppearanceAlpha(0.045), border: .labelColor.withAppearanceAlpha(0.08))
            form.identifier = NSUserInterfaceItemIdentifier("add_hub_form")

            func fieldLabel(_ title: String, _ frame: NSRect) {
                form.addSubview(textLabel(title,frame,size:10,weight:.semibold,color:CitoPalette.secondaryText))
            }
            if editingLocalHub {
                form.addSubview(textLabel("This Mac (Local Hub)",NSRect(x:14,y:94+localErrorHeight,width:452,height:18),size:12.5,weight:.semibold))
                fieldLabel("Username (Optional)",NSRect(x:14,y:72+localErrorHeight,width:452,height:14))
                form.addSubview(makeEditableField("settings_hub_user",frame:NSRect(x:14,y:44,width:452,height:24),placeholder:"Defaults to your Mac username",value:addHubUser))
            } else {
                fieldLabel("Hub URL",NSRect(x:14,y:179,width:298,height:13))
                fieldLabel("Hub Name (Optional)",NSRect(x:320,y:179,width:146,height:13))
                form.addSubview(makeEditableField("settings_hub_url",frame:NSRect(x:14,y:153,width:298,height:24),placeholder:"http://192.168.1.10:4291",value:addHubUrl))
                form.addSubview(makeEditableField("settings_hub_name",frame:NSRect(x:320,y:153,width:146,height:24),placeholder:hubNamePlaceholder,value:addHubName))
                fieldLabel("Access",NSRect(x:14,y:125,width:48,height:14))
                let authOptions = [("none","Trusted Network"),("token","Shared Token"),("oidc","Team Sign-In")]
                let authTrack = roundedView(NSRect(x:68,y:118,width:398,height:26),radius:13,
                    color:.labelColor.withAppearanceAlpha(0.06))
                authTrack.identifier = NSUserInterfaceItemIdentifier("settings_hub_auth_track")
                var authPills: [NSView] = []
                for (mode,label) in authOptions {
                    let pill=MenuControl(label,frame:NSRect(x:0,y:3,width:MenuControl.intrinsicWidth(label),height:22),radius:11)
                    pill.identifier=NSUserInterfaceItemIdentifier("settings_hub_auth:\(mode)")
                    pill.style = addHubAuth == mode ? .prominent : .subtleHeader
                    pill.pressed={ [weak self] in
                        guard let self=self else{return}
                        self.syncAddHubFieldsFromControls()
                        if self.addHubAuth != mode { self.addHubDraftGeneration += 1 }
                        self.addHubAuth=mode; self.rebuildSettingsPanel()
                    }
                    authTrack.addSubview(pill); authPills.append(pill)
                }
                layoutBar(in: authTrack, insetLeft: 2, insetRight: 2, y: 2, height: 22, spacing: 4, leading: authPills)
                authTrack.frame.size.width = authPills.reduce(CGFloat(4)) { $0 + $1.frame.width } + CGFloat(max(0,authPills.count-1)) * 4
                form.addSubview(authTrack)
                if addHubAuth == "none" {
                    fieldLabel("Username (Optional)",NSRect(x:14,y:97,width:452,height:14))
                    form.addSubview(makeEditableField("settings_hub_user",frame:NSRect(x:14,y:71,width:452,height:24),placeholder:"Defaults to your Mac username",value:addHubUser))
                } else if addHubAuth == "token" {
                    fieldLabel("Shared Access Token",NSRect(x:14,y:97,width:274,height:14))
                    fieldLabel("Username (Optional)",NSRect(x:296,y:97,width:170,height:14))
                    form.addSubview(makeEditableField("settings_hub_token",frame:NSRect(x:14,y:71,width:274,height:24),placeholder:editingExistingHub ? "•••••• (Saved — enter new token to replace)" : "Paste token from host Mac",value:addHubToken,secret:true))
                    form.addSubview(makeEditableField("settings_hub_user",frame:NSRect(x:296,y:71,width:170,height:24),placeholder:"e.g. alice",value:addHubUser))
                } else if addHubAuth == "oidc" {
                    fieldLabel("OIDC Issuer URL",NSRect(x:14,y:97,width:260,height:14))
                    fieldLabel("OIDC Client ID",NSRect(x:282,y:97,width:184,height:14))
                    form.addSubview(makeEditableField("settings_hub_issuer",frame:NSRect(x:14,y:71,width:260,height:24),placeholder:"https://id.example.com",value:addHubIssuer))
                    form.addSubview(makeEditableField("settings_hub_client_id",frame:NSRect(x:282,y:71,width:184,height:24),placeholder:"cito",value:addHubClientId))
                }
            }

            if !(editingExistingHub && editingOriginalHub == "local") {
                for control in form.subviews { control.frame.origin.y += 48 }
                let visibility = textLabel("Personal Hubs connect your Macs automatically; Team Sign-In (OIDC) Hubs start private to your account (@user) until shared with Team.", NSRect(x: 14, y: 60, width: 452, height: 48), size: 11, color: CitoPalette.secondaryText)
                visibility.identifier = NSUserInterfaceItemIdentifier("hub_visibility_caption")
                visibility.maximumNumberOfLines = 3
                visibility.cell?.wraps = true
                form.addSubview(visibility)
            }
            if let error=addHubError {
                let label=textLabel(presentationError(error),NSRect(x:14,y:editingLocalHub ? 74 : 38,width:452,height:15),size:10,color:CitoPalette.dangerText)
                label.identifier=NSUserInterfaceItemIdentifier("add_hub_error"); label.toolTip=label.stringValue
                label.lineBreakMode = .byTruncatingMiddle; form.addSubview(label)
            }
            let cancelBtn = MenuControl("Cancel", frame: NSRect(x: 306, y: 12, width: 72, height: 24), radius: 6)
            cancelBtn.identifier = NSUserInterfaceItemIdentifier("cancel_add_hub")
            cancelBtn.pressed = { [weak self] in
                guard let self = self else { return }
                self.addHubFormOpen = false
                self.inviteParseRequest=""
                self.pendingHubSave = nil
                self.rebuildSettingsPanel()
            }
            form.addSubview(cancelBtn)

            let submitBtn = MenuControl(editingExistingHub ? (editingOriginalHub == "local" ? "Save" : "Save Hub") : "Join Hub", frame: NSRect(x: 386, y: 12, width: 80, height: 24), radius: 6)
            submitBtn.identifier = NSUserInterfaceItemIdentifier("submit_add_hub")
            submitBtn.style = .prominent
            submitBtn.isEnabled = pendingHubSave == nil
            submitBtn.pressed = { [weak self] in
                guard let self = self else { return }
                self.syncAddHubFieldsFromControls()
                let url = normalizedHubURL(self.addHubUrl)
                let knownHubs = self.settings?.hubs ?? []
                let name = proposedHubName(self.addHubName,url:url,existing:knownHubs,creating:!self.editingExistingHub,original:self.editingOriginalHub)
                let validation: String?
                if name.isEmpty { validation="Hub name must contain a letter or number." }
                else if !self.editingExistingHub && (name == "local" || knownHubs.contains(where:{$0.name == name && $0.url != url})) { validation="Hub name already exists. Choose a different name." }
                else if !["http://","https://","local://"].contains(where:{url.hasPrefix($0)}) { validation="Hub URL must start with http:// or https://." }
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
                if self.addHubAuth == "none" || self.addHubAuth == "token" { if !usr.isEmpty { action["user"]=usr } else if self.editingExistingHub { action["user"]=NSNull() } }
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
            layoutBar(in: form, insetRight: 14, y: 10, height: 24, trailing: [cancelBtn,submitBtn])
            hubsView.addSubview(form)
        } else {
            let resolvedHubs: [HubConfigItem] = settings?.hubs ?? hubs.map { h in
                HubConfigItem(name: h.name, url: h.name == "local" ? "local://" : "http://127.0.0.1:4291",
                    auth_mode: h.auth_mode, enabled: h.enabled != false, auto_listen: h.auto_listen, is_default: false, status: h.status, user: h.user, error: h.error, issuer: nil, client_id: nil)
            }
            var rowTop = 180 + hubOffset
            for hub in resolvedHubs {
                let local=hub.name == "local"
                let needsSignIn = hub.auth_mode == "oidc" && hub.enabled && ["auth_required","disconnected"].contains(hub.status)
                let authTitle = ["none":"Trusted Network","token":"Shared Token","oidc":"Team Sign-In"][hub.auth_mode] ?? "Network"
                let endpoint = hub.url == "local://" ? "On this Mac" : hub.url.replacingOccurrences(of:"^https?://",with:"",options:.regularExpression)
                let detail = hub.error.map(presentationError) ?? (hub.status == "connected" ? "" : (["auth_required":"Sign-in required","disconnected":"Offline","connecting":"Connecting…"][hub.status] ?? "Unavailable"))
                let subtitle=local ? "Built-in on this Mac · Signed in as @\(hub.user ?? "local")" : "\(endpoint) · \(authTitle)" + (hub.user.map { " · @\($0)" } ?? "") + (detail.isEmpty ? "" : " · \(detail)")
                let subtitleWidth:CGFloat = 438
                let subtitleHeight = max(14,ceil((subtitle as NSString).boundingRect(with:NSSize(width:subtitleWidth,height:10000),options:[.usesLineFragmentOrigin,.usesFontLeading],attributes:[.font:NSFont.systemFont(ofSize:10.5)]).height)+2)
                let rowHeight = max(56,subtitleHeight+42)
                rowTop -= rowHeight
                let row = roundedView(NSRect(x:20,y:rowTop,width:480,height:rowHeight),radius:9,color:.labelColor.withAppearanceAlpha(0.05),border:.labelColor.withAppearanceAlpha(0.10))
                rowTop -= 6
                row.identifier = NSUserInterfaceItemIdentifier("settings_hub:\(hub.name)")
                row.addSubview(roundedView(NSRect(x:14,y:rowHeight-24,width:8,height:8),radius:4,color:statusColor(hub.enabled ? hub.status : "disconnected")))
                let nameLabel = textLabel(hub.name,NSRect(x:28,y:rowHeight-24,width:0,height:16),size:12.5,weight:.semibold)
                nameLabel.lineBreakMode = .byTruncatingMiddle; nameLabel.toolTip = hub.name
                row.addSubview(nameLabel)
                let subtitleLabel = textLabel(subtitle,NSRect(x:28,y:9,width:subtitleWidth,height:subtitleHeight),size:10.5,color:CitoPalette.secondaryText)
                subtitleLabel.maximumNumberOfLines=0; subtitleLabel.lineBreakMode = .byWordWrapping
                subtitleLabel.toolTip = hub.url + (hub.error.map { "\n" + presentationError($0) } ?? "")
                row.addSubview(subtitleLabel)

                let edit=MenuControl("Edit",frame:NSRect(x:0,y:rowHeight-25,width:52,height:22),radius:6)
                edit.identifier=NSUserInterfaceItemIdentifier("edit_hub:\(hub.name)")
                edit.style = .neutral
                edit.pressed={ [weak self] in
                    guard let self=self else{return}
                    self.addHubName=hub.name; self.addHubUrl=hub.url; self.addHubAuth=hub.auth_mode
                    self.addHubUser=hub.user ?? ""; self.addHubIssuer=hub.issuer ?? ""; self.addHubClientId=hub.client_id ?? ""; self.addHubToken=""
                    self.addHubDraftGeneration += 1
                    self.pendingHubSave=nil; self.addHubError=nil; self.inviteParseRequest=""
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

                let toggleBtn = MenuControl(hub.enabled ? "Disable" : "Enable", frame: NSRect(x: 0, y: rowHeight-25, width: 72, height: 22), radius: 6)
                toggleBtn.identifier = NSUserInterfaceItemIdentifier("toggle_hub:\(hub.name)")
                toggleBtn.style = hub.enabled ? .neutral : .prominent
                let hubName = hub.name
                let nextEnabled = !hub.enabled
                toggleBtn.pressed = { [weak self] in
                    self?.emit(["op": "setup_toggle_hub", "hub": hubName, "enabled": nextEnabled])
                }
                toggleBtn.isHidden = resolvedHubs.filter { $0.enabled }.count <= 1 && hub.enabled
                if !toggleBtn.isHidden { row.addSubview(toggleBtn) }

                if hub.name != "local" {
                    let removeBtn = MenuControl("Remove", frame: NSRect(x: 396, y: rowHeight-25, width: 72, height: 22), radius: 6)
                    removeBtn.identifier = NSUserInterfaceItemIdentifier("remove_hub:\(hub.name)")
                    removeBtn.style = .destructiveHover
                    removeBtn.pressed = { [weak self] in
                        self?.emit(["op": "setup_remove_hub", "hub": hubName])
                    }
                    row.addSubview(removeBtn)
                }

                let signIn = row.subviews.filter { $0.identifier?.rawValue == "login_hub:\(hub.name)" }
                let remove = row.subviews.filter { $0.identifier?.rawValue == "remove_hub:\(hub.name)" }
                layoutBar(in: row, insetLeft: 28, y: rowHeight-31, height: 22, flexible: nameLabel, trailing: signIn + [edit,toggleBtn] + remove)
                hubsView.addSubview(row)
            }
        }
        mountSettingsCards(hubsView, in: root)

        // Persistent Footer (520 × 44 pt, y: 0)
        let footerBar = NSView(frame: NSRect(x: 0, y: 0, width: 520, height: 44))
        footerBar.identifier = NSUserInterfaceItemIdentifier("settings_footer")
        footerBar.alphaValue = uninstallConfirmOpen ? 0.35 : 1
        footerBar.addSubview(roundedView(NSRect(x: 0, y: 43, width: 520, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
        let launchAtLogin = settings?.launch_at_login ?? false
        let loginBtn = MenuControl("Start Cito at Login", frame: NSRect(x: 20, y: 10, width: 164, height: 24),
            leadingSymbol: launchAtLogin ? "checkmark.square.fill" : "square", radius: 6)
        loginBtn.identifier = NSUserInterfaceItemIdentifier("settings_launch_at_login")
        loginBtn.style = .subtleHeader
        loginBtn.label.alignment = .left
        loginBtn.layoutContents()
        loginBtn.pressed = { [weak self] in
            self?.emit(["op": "set_launch_at_login", "enabled": !launchAtLogin])
        }
        footerBar.addSubview(loginBtn)

        let uninstallBtn = MenuControl("Disconnect All…", frame: NSRect(x: 316, y: 10, width: MenuControl.intrinsicWidth("Disconnect All…", paddingX: 12), height: 24), radius: 6)
        uninstallBtn.identifier = NSUserInterfaceItemIdentifier("open_uninstall_confirm")
        uninstallBtn.style = .destructiveHover
        uninstallBtn.pressed = { [weak self] in
            guard let self = self else { return }
            self.uninstallConfirmOpen = true
            self.uninstallPurgeData = false
            self.rebuildSettingsPanel()
        }
        footerBar.addSubview(uninstallBtn)
        layoutBar(in: footerBar, insetLeft: 20, insetRight: 20, y: 10, height: 24, leading: [loginBtn], trailing: [uninstallBtn])
        root.addSubview(footerBar)

        if uninstallConfirmOpen {
            let overlay = ModalOverlay(frame:NSRect(x:0,y:0,width:520,height:460))
            overlay.identifier = NSUserInterfaceItemIdentifier("uninstall_modal_overlay")
            let confirmBox = roundedView(NSRect(x: 40, y: 150, width: 440, height: 112), radius: 10,
                color: .windowBackgroundColor.withAppearanceAlpha(0.96), border: CitoPalette.dangerText.withAppearanceAlpha(0.35))
            confirmBox.identifier = NSUserInterfaceItemIdentifier("uninstall_confirm_box")

            confirmBox.addSubview(textLabel("Disconnect All Integrations",NSRect(x:12,y:82,width:416,height:18),size:12.5,weight:.semibold))
            let purgeCheckbox = MenuControl("Also delete local data and Hub settings",
                frame: NSRect(x: 12, y: 50, width: 416, height: 22),
                leadingSymbol: uninstallPurgeData ? "checkmark.square.fill" : "square", radius: 6)
            purgeCheckbox.identifier = NSUserInterfaceItemIdentifier("uninstall_purge_data")
            purgeCheckbox.style = .subtleHeader
            purgeCheckbox.label.alignment = .left
            purgeCheckbox.layoutContents()
            purgeCheckbox.pressed = { [weak self] in
                guard let self = self else { return }
                self.uninstallPurgeData.toggle()
                self.rebuildSettingsPanel()
            }
            confirmBox.addSubview(purgeCheckbox)

            let cancelUninstallBtn = MenuControl("Cancel", frame: NSRect(x: 32, y: 16, width: MenuControl.intrinsicWidth("Cancel"), height: 24), radius: 6)
            cancelUninstallBtn.identifier = NSUserInterfaceItemIdentifier("cancel_uninstall_all")
            cancelUninstallBtn.pressed = { [weak self] in
                guard let self = self else { return }
                self.uninstallConfirmOpen = false
                self.uninstallPurgeData = false
                self.rebuildSettingsPanel()
            }
            confirmBox.addSubview(cancelUninstallBtn)

            let disconnect=MenuControl("Disconnect All",frame:NSRect(x:112,y:16,width:MenuControl.intrinsicWidth("Disconnect All"),height:24),radius:6)
            disconnect.identifier=NSUserInterfaceItemIdentifier("confirm_disconnect_all")
            disconnect.style = .destructive
            disconnect.toolTip = "Disconnect all Agent Apps and keep Cito running"
            disconnect.pressed = { [weak self] in
                guard let self = self else { return }
                let purge = self.uninstallPurgeData
                self.uninstallConfirmOpen = false
                self.uninstallPurgeData = false
                self.emit(["op": "setup_disconnect_all", "purge_data": purge])
                self.rebuildSettingsPanel()
            }
            confirmBox.addSubview(disconnect)
            layoutBar(in: confirmBox, insetLeft: 12, insetRight: 12, y: 14, height: 24, leading: [cancelUninstallBtn], trailing: [disconnect])
            overlay.addSubview(confirmBox); root.addSubview(overlay)
        }

        window.contentView = root
        if addHubFormOpen && focusedId == nil, let form=root.subviews.compactMap({($0 as? NSScrollView)?.documentView}).flatMap({$0.subviews}).first(where:{$0.identifier?.rawValue=="add_hub_form"}) { form.scrollToVisible(form.bounds) }
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
            NSApp.setActivationPolicy(.regular)
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
        if !headless && !settingsVisible && !directoryVisible { NSApp.setActivationPolicy(.accessory) }
        if emitAction { emit(["op": "close_settings"]) }
    }

    // MARK: - Surface 2: Spotlight-Style Network Directory Panel (560 × 440 pt KeyPanel: NSPanel)

    func ensureDirectoryPanel() {
        ensureEditMainMenu()
        if directoryPanel != nil { return }
        let window = KeyPanel(contentRect: NSRect(x: 0, y: 0, width: 560, height: 440),
                              styleMask: [.titled, .fullSizeContentView], backing: .buffered, defer: false)
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        for button in [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton] {
            window.standardWindowButton(button)?.isHidden = true
        }
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

        window.onKey = { [weak self] key in self?.handleDirectoryKey(key) }

        // Top Search Bar (560 × 46 pt, y: 394)
        let searchBar = NSView(frame: NSRect(x: 0, y: 394, width: 560, height: 46))
        searchBar.identifier = NSUserInterfaceItemIdentifier("directory_search_bar")
        let modeTrack = roundedView(NSRect(x:14,y:9,width:174,height:28),radius:8,color:.labelColor.withAppearanceAlpha(0.06),border:.labelColor.withAppearanceAlpha(0.08))
        modeTrack.identifier = NSUserInterfaceItemIdentifier("directory_mode_track")
        searchBar.addSubview(modeTrack)

        let dirPill = MenuControl("Directory", frame: NSRect(x: 16, y: 11, width: 76, height: 24), radius: 7)
        dirPill.identifier = NSUserInterfaceItemIdentifier("dir_mode:directory")
        dirPill.label.font = .systemFont(ofSize: 11.5, weight: .semibold)
        dirPill.pressed = { [weak self] in
            guard let self = self else { return }
            self.switchDirectoryMode("directory")
        }
        dirPill.frame.origin = NSPoint(x:2,y:2)
        modeTrack.addSubview(dirPill)
        directoryModeDirPill = dirPill

        let actTitle = unreadCount > 0 ? "Activity · \(unreadCount)" : "Activity"
        let actPill = MenuControl(actTitle, frame: NSRect(x: 96, y: 11, width: 88, height: 24), radius: 7)
        actPill.identifier = NSUserInterfaceItemIdentifier("dir_mode:activity")
        actPill.label.font = .systemFont(ofSize: 11.5, weight: .semibold)
        actPill.pressed = { [weak self] in
            guard let self = self else { return }
            self.switchDirectoryMode("activity")
        }
        actPill.frame.origin = NSPoint(x:82,y:2)
        modeTrack.addSubview(actPill)
        directoryModeActPill = actPill

        let searchField = NSSearchField(frame: NSRect(x: 192, y: 9, width: 324, height: 28))
        searchField.identifier = NSUserInterfaceItemIdentifier("directory_search")
        searchField.placeholderString = "Search by user, repo, title, host, or #id…"
        searchField.delegate = self
        searchBar.addSubview(searchField)
        directorySearchField = searchField

        let closeBtn = MenuControl("Close", frame: NSRect(x: 524, y: 11, width: 24, height: 24), symbol: "xmark")
        closeBtn.identifier = NSUserInterfaceItemIdentifier("close_directory")
        closeBtn.style = .subtleHeader
        closeBtn.toolTip = "Close Directory"
        closeBtn.pressed = { [weak self] in self?.closeDirectoryPanel(emitAction: true) }
        searchBar.addSubview(closeBtn)
        searchBar.addSubview(roundedView(NSRect(x: 0, y: 0, width: 560, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
        root.addSubview(searchBar)

        // Facet Bar (560 × 32 pt, y: 362)
        let facetBar = NSView(frame: NSRect(x: 0, y: 362, width: 560, height: 32))
        facetBar.identifier = NSUserInterfaceItemIdentifier("directory_facet_bar")
        let facetSeparator = roundedView(NSRect(x: 136, y: 9, width: 1, height: 14), radius: 0, color: .separatorColor)
        facetSeparator.identifier = NSUserInterfaceItemIdentifier("directory_facet_separator")
        facetBar.addSubview(facetSeparator)
        let scopeTrack = roundedView(NSRect(x:16,y:3,width:114,height:26),radius:13,color:.labelColor.withAppearanceAlpha(0.06))
        scopeTrack.identifier = NSUserInterfaceItemIdentifier("directory_scope_track")
        facetBar.addSubview(scopeTrack)
        let scopePills: [(String, String, CGFloat, CGFloat)] = [
            ("dir_scope:all", "All", 16, 38),
            ("dir_scope:mine", "My Macs", 58, 68)
        ]
        for (id, title, x, w) in scopePills {
            let pill = MenuControl(title, frame: NSRect(x: x-16, y: 0, width: w, height: 22), radius: 11)
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
            pill.frame.origin=NSPoint(x:x-14,y:2)
            scopeTrack.addSubview(pill)
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
        directoryFacetControls["dir_harness:all"] = allHarnessesPill

        let activityTrack = roundedView(NSRect(x:16,y:3,width:226,height:26),radius:13,color:.labelColor.withAppearanceAlpha(0.06))
        activityTrack.identifier = NSUserInterfaceItemIdentifier("activity_filter_track")
        activityTrack.isHidden = true
        facetBar.addSubview(activityTrack)
        let actPills: [(String, String, String, CGFloat, CGFloat)] = [
            ("act_filter:me", "me", "For Me", 16, 64),
            ("act_filter:all", "all", "All Activity", 84, 86),
            ("act_filter:unread", "unread", "Unread", 174, 64)
        ]
        for (id, filterVal, title, x, w) in actPills {
            let pill = MenuControl(title, frame: NSRect(x: x-16, y: 0, width: w, height: 22), radius: 11)
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
            pill.frame.origin=NSPoint(x:x-14,y:2)
            activityTrack.addSubview(pill)
            directoryFacetControls[id] = pill
        }
        let markReadBtn = MenuControl("✓ Read All", frame: NSRect(x: 390, y: 5, width: 88, height: 22), radius: 11)
        markReadBtn.identifier = NSUserInterfaceItemIdentifier("activity_mark_all_read")
        markReadBtn.label.font = .systemFont(ofSize: 11, weight: .medium)
        markReadBtn.isHidden = true
        markReadBtn.pressed = { [weak self] in
            guard let self = self else { return }
            self.unreadCount = self.activityFilter == "me" ? max(0,self.unreadCount-self.activityUnreadCount) : 0
            self.activityUnreadCount = 0
            self.serverActivityFilter = nil
            self.activityAllItems = self.activityAllItems.map { item in
                if self.activityFilter == "me" && item.target_harness != "human" { return item }
                return ActivityItem(
                    hub: item.hub, signal_id: item.signal_id, listener_id: item.listener_id,
                    from_user: item.from_user, from_host_name: item.from_host_name,
                    from_harness: item.from_harness, from_title: item.from_title,
                    from_scoped_id: item.from_scoped_id, from_display_address: item.from_display_address, target_display_address: item.target_display_address, message_truncated: item.message_truncated, target_harness: item.target_harness,
                    target_title: item.target_title, target_scoped_id: item.target_scoped_id,
                    message: item.message, held_reason: item.held_reason,
                    in_reply_to: item.in_reply_to, created_at: item.created_at,
                    unread: false, for_me: item.for_me, from_listener_id:item.from_listener_id
                )
            }
            self.updateDirectoryModePills()
            self.applyActivityFilter()
            self.emit(["op": "ack_signals", "ack_all": true,"filter":self.activityFilter])
            self.queryDirectory()
        }
        facetBar.addSubview(markReadBtn)
        directoryFacetControls["activity_mark_all_read"] = markReadBtn
        let clear = MenuControl("Clear",frame:NSRect(x:0,y:5,width:68,height:22),leadingSymbol:"trash",radius:11)
        clear.label.font = .systemFont(ofSize: 11, weight: .medium)
        clear.identifier = NSUserInterfaceItemIdentifier("activity_clear")
        clear.isHidden = true
        clear.style = .destructiveHover
        clear.pressed = { [weak self] in guard let self=self else{return}; self.emit(["op":"clear_activity_history","filter":self.activityFilter]); self.queryDirectory() }
        facetBar.addSubview(clear); directoryFacetControls["activity_clear"] = clear

        facetBar.addSubview(roundedView(NSRect(x: 0, y: 0, width: 560, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
        directoryFacetBar = facetBar
        root.addSubview(facetBar)
        updateDirectoryModePills()
        rebuildDirectoryFacetBar()

        // Virtualized Result List (560 × 334 pt, y: 28)
        let scroll = OverlayScrollView(frame: NSRect(x: 0, y: 28, width: 560, height: 334))
        scroll.scrollerStyle = .overlay
        scroll.identifier = NSUserInterfaceItemIdentifier("directory_scroll")
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = false
        scroll.autohidesScrollers = true
        let table = DirectoryTableView(frame: NSRect(x: 0, y: 0, width: 560, height: 334))
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
        let warning = textLabel("",NSRect(x:16,y:338,width:528,height:20),size:11,color:CitoPalette.secondaryText)
        warning.identifier = NSUserInterfaceItemIdentifier("directory_warning")
        warning.isHidden = true
        root.addSubview(warning)
        directoryWarningLabel = warning

        // Bottom Single-Line ComposerBar (560 × 44 pt, y: 28)
        let composer = roundedView(NSRect(x: 0, y: 28, width: 560, height: 44), radius: 0,
            color: .windowBackgroundColor.withAppearanceAlpha(0.92))
        composer.identifier = NSUserInterfaceItemIdentifier("directory_composer")
        composer.isHidden = true
        let divider=roundedView(NSRect(x:0,y:43,width:560,height:1),radius:0,color:.labelColor.withAppearanceAlpha(0.12))
        divider.identifier=NSUserInterfaceItemIdentifier("composer_divider"); composer.addSubview(divider)

        let targetPill = roundedView(NSRect(x:12,y:10,width:120,height:24),radius:6,color:.labelColor.withAppearanceAlpha(0.08),border:.labelColor.withAppearanceAlpha(0.14))
        targetPill.identifier = NSUserInterfaceItemIdentifier("composer_target")
        let targetLabel=textLabel("",NSRect(x:6,y:4,width:108,height:16),size:11,weight:.medium)
        targetPill.addSubview(targetLabel); composerTargetLabel=targetLabel
        composer.addSubview(targetPill)
        composerTargetPill = targetPill

        let inputField = NSTextField(frame: NSRect(x: 138, y: 10, width: 274, height: 24))
        inputField.identifier = NSUserInterfaceItemIdentifier("composer_input")
        inputField.placeholderString = "Write a message…"
        inputField.isEditable = true
        inputField.isSelectable = true
        inputField.isBezeled = true
        inputField.bezelStyle = .roundedBezel
        inputField.font = .systemFont(ofSize: 12)
        inputField.delegate = self
        composer.addSubview(inputField)
        composerInputField = inputField

        let sendBtn = MenuControl("Send ⏎", frame: NSRect(x: 410, y: 10, width: max(MenuControl.intrinsicWidth("Send ⏎"),MenuControl.intrinsicWidth("Sending…")), height: 24), radius: 6)
        sendBtn.identifier = NSUserInterfaceItemIdentifier("composer_send")
        sendBtn.style = .prominent
        sendBtn.pressed = { [weak self] in self?.submitComposer() }
        composer.addSubview(sendBtn); composerSendButton=sendBtn

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
        let shortcuts = textLabel("↵ Copy Address    ⎋ Close", NSRect(x: 16, y: 6, width: 260, height: 16), size: 11, color: CitoPalette.secondaryText)
        shortcuts.identifier = NSUserInterfaceItemIdentifier("directory_shortcuts")
        directoryShortcutsLabel = shortcuts
        footerBar.addSubview(shortcuts)
        updateDirectoryShortcuts()
        let summary = textLabel("0 of 0 online", NSRect(x: 320, y: 6, width: 224, height: 16), size: 11, color: CitoPalette.secondaryText)
        summary.alignment = .right
        summary.identifier = NSUserInterfaceItemIdentifier("directory_summary")
        footerBar.addSubview(summary)
        directorySummaryLabel = summary
        layoutBar(in: footerBar, insetLeft: 16, insetRight: 16, y: 6, height: 16, flexible: shortcuts, trailing: [summary])
        root.addSubview(footerBar)

        window.contentView = root
        directoryPanel = window
        updateDirectoryFacetStyles()
    }

    func updateDirectoryShortcuts() {
        let canReply = activityFilteredItems.indices.contains(directorySelectedIndex) && activityFilteredItems[directorySelectedIndex].for_me && !(activityFilteredItems[directorySelectedIndex].from_display_address ?? activityFilteredItems[directorySelectedIndex].from_scoped_id ?? "").isEmpty
        directoryShortcutsLabel?.stringValue = !(directoryComposerBar?.isHidden ?? true)
            ? "↵ Send    ⎋ Cancel Reply"
            : (directoryMode == "activity" ? (canReply ? "↵ Reply    ⎋ Close" : "⎋ Close") : "↵ Copy Address    ⎋ Close")
    }

    func updateDirectoryModePills() {
        updateDirectoryShortcuts()
        directoryModeDirPill?.style = directoryMode == "directory" ? .prominent : .subtleHeader
        directoryModeActPill?.style = directoryMode == "activity" ? .prominent : .subtleHeader
        directorySearchField?.placeholderString = directoryMode == "activity" ? "Search messages, people, or sessions…" : "Search by user, repo, title, host, or #id…"
        let actTitle = unreadCount > 0 ? "Activity · \(unreadCount)" : "Activity"
        directoryModeActPill?.label.stringValue = actTitle
        directoryModeActPill?.setAccessibilityLabel(actTitle)
        if let act = directoryModeActPill, let dir = directoryModeDirPill, let search = directorySearchField, let track = act.superview, let bar = track.superview, let close = bar.subviews.first(where: { $0.identifier?.rawValue == "close_directory" }) {
            dir.frame.size.width = MenuControl.intrinsicWidth("Directory", font:dir.label.font ?? .systemFont(ofSize:11.5))
            act.frame.size.width = MenuControl.intrinsicWidth(actTitle, font:act.label.font ?? .systemFont(ofSize:11.5))
            track.frame.size.width = dir.frame.width + act.frame.width + 8
            layoutBar(in:track,insetLeft:2,insetRight:2,y:2,height:24,spacing:4,leading:[dir,act])
            layoutBar(in: bar, insetLeft: 14, y: 9, height: 28, leading: [track], flexible: search, trailing: [close])
        }
    }

    func switchDirectoryMode(_ mode: String) {
        latestDirectoryActionRequestId = nil
        clearDirectoryActionError()
        directoryMode = mode
        updateDirectoryWarnings()
        queryDirectory()
        updateDirectoryModePills()
        updateDirectoryFacetStyles()
        if mode == "activity" {
            applyActivityFilter(revealSelection:true)
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
        let displayTarget="To: \(target)"
        let targetWidth = min(220, MenuControl.intrinsicWidth(displayTarget, font:.systemFont(ofSize:11,weight:.medium), minWidth:120))
        composerTargetPill?.frame.size.width=targetWidth
        composerTargetLabel?.stringValue = displayTarget
        composerTargetLabel?.frame.size.width = targetWidth-12
        composerTargetLabel?.lineBreakMode = .byTruncatingMiddle
        composerTargetLabel?.toolTip = target
        composerInputField?.frame=NSRect(x:18+targetWidth,y:10,width:386-targetWidth,height:24)
        composerTargetPill?.setAccessibilityLabel(target)
        composerTargetPill?.toolTip = target
        directoryComposerBar?.isHidden = false
        updateDirectoryShortcuts()
        updateDirectoryWarnings()
        directoryScrollView?.layoutSubtreeIfNeeded()
        if let table = directoryTableView, directorySelectedIndex < table.numberOfRows { table.scrollRowToVisible(directorySelectedIndex) }
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
        updateDirectoryWarnings()
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
            setComposerStatus("Message is too large to send")
            return
        }
        pendingComposer = PendingComposerSend(text:composerInputField?.stringValue ?? text,
            requestId:requestId, target:composerTarget, hub:composerTargetHub, replySignalId:composerReplySignalId)
        setComposerStatus("")
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
        var harnessPills: [NSView] = directoryFacetControls["dir_harness:all"].map { [$0] } ?? []
        for harness in ordered {
            let visual = resolveHarnessVisual(harness, harnesses: map)
            let title = ordered.count <= 3 ? visual.name.replacingOccurrences(of:" (Experimental)",with:"") : harness
            let width = max(42, ceil(measureTextWidth(title, font: pillFont)) + 20)
            let id = "dir_harness:\(harness)"
            let pill = MenuControl(title, frame: NSRect(x: 0, y: 0, width: width, height: 22), radius: 11)
            pill.identifier = NSUserInterfaceItemIdentifier(id); pill.label.font = pillFont
            pill.pressed = { [weak self] in
                guard let self = self else { return }
                self.directoryHarnessFilter = harness
                self.rebuildDirectoryFacetBar()
                self.applyDirectoryFilter(fromDaemonFrame: false, daemonMatchedCount: nil)
                self.queryDirectory()
            }
            harnessPills.append(pill); directoryFacetControls[id] = pill
        }
        let strip = mountPillStrip(in: facetBar, frame: NSRect(x: 142, y: 3, width: 402, height: 26),
                                  id: "directory_harness_scroll", pills: harnessPills) {
            $0.identifier?.rawValue == "dir_harness:\(directoryHarnessFilter)"
        }
        strip.wantsLayer = true
        strip.backgroundColor = .labelColor.withAppearanceAlpha(0.06)
        strip.drawsBackground = true
        strip.layer?.cornerRadius = 13
        strip.layer?.masksToBounds = true
        directoryDynamicHarnessViews.append(strip)
        updateDirectoryFacetStyles()
    }

    func updateDirectoryFacetStyles() {
        let isActivity = directoryMode == "activity"
        directoryFacetBar?.subviews.first { $0.identifier?.rawValue == "directory_facet_separator" }?.isHidden = isActivity
        directoryFacetBar?.subviews.first { $0.identifier?.rawValue == "directory_scope_track" }?.isHidden = isActivity
        directoryFacetBar?.subviews.first { $0.identifier?.rawValue == "activity_filter_track" }?.isHidden = !isActivity
        for v in directoryDynamicHarnessViews { v.isHidden = isActivity }
        for (id, pill) in directoryFacetControls {
            if id.hasPrefix("act_filter:") {
                pill.isHidden = !isActivity
                pill.style = (id == "act_filter:\(activityFilter)") ? .prominent : .subtleHeader
            } else if id == "activity_mark_all_read" || id == "activity_clear" {
                pill.isHidden = !isActivity
                pill.style = id == "activity_clear" ? .destructiveHover : .neutral
            } else {
                pill.isHidden = isActivity
                let active = (id == "dir_scope:\(directoryScopeFilter)") || (id == "dir_harness:\(directoryHarnessFilter)")
                pill.style = active ? .prominent : .subtleHeader
                if !isActivity && active && id.hasPrefix("dir_harness:") { pill.scrollToVisible(pill.bounds) }
            }
        }
        if isActivity, let bar = directoryFacetBar, let markRead = directoryFacetControls["activity_mark_all_read"], let clear = directoryFacetControls["activity_clear"] {
            layoutBar(in: bar, insetRight: 16, y: 5, height: 22, trailing: [markRead,clear])
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
                title="No matching sessions or people"; subtitle="No active listeners match your current search or filter."
                id="directory_reset_filters"; label="Reset Filters"
                action={ [weak self] in
                    guard let self=self else{return}
                    self.directoryQuery=""; self.directoryScopeFilter="all"; self.directoryHarnessFilter="all"
                    self.directorySearchField?.stringValue=""; self.rebuildDirectoryFacetBar()
                    self.applyDirectoryFilter(fromDaemonFrame:false,daemonMatchedCount:nil)
                    self.queryDirectory()
                }
            } else {
                title="No active listeners"; subtitle="Enroll a coding session with + Listen or connect an Agent App."
                id="dir_empty_open_apps"; label="Open Agent Apps Settings…"
                action={ [weak self] in self?.closeDirectoryPanel(emitAction:true); self?.settingsActiveTab="apps"; self?.openSettingsPanel() }
            }
        } else if !directoryQuery.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty {
            title="No matching messages"; subtitle="No messages match your search."
            id="act_reset_search"; label="Reset Search"
            action={ [weak self] in guard let self=self else{return}; self.directoryQuery=""; self.directorySearchField?.stringValue=""; self.applyActivityFilter(); self.queryDirectory() }
        } else if activityFilter=="me" && activityHasAny {
            title="No messages addressed to you"; subtitle="Messages to your agent sessions are in All Activity."
            id="act_empty_show_all"; label="Show All Activity"
            action={ [weak self] in self?.activityFilter="all"; self?.updateDirectoryFacetStyles(); self?.applyActivityFilter(); self?.queryDirectory() }
        } else if activityFilter=="unread" {
            title="All caught up"; subtitle="You have no unread messages."
            id="act_empty_show_all"; label="Show All Activity"
            action={ [weak self] in self?.activityFilter="all"; self?.activityUnreadCount=0; self?.updateDirectoryFacetStyles(); self?.applyActivityFilter(); self?.queryDirectory() }
        } else {
            title="No recent message activity"; subtitle="Messages sent between your sessions and channels will appear here."
            id="act_empty_open_channels"; label="Configure Human Channels…"
            action={ [weak self] in self?.closeDirectoryPanel(emitAction:true); self?.settingsActiveTab="channels"; self?.openSettingsPanel() }
        }
        let heading = textLabel(title,NSRect(x:20,y:96,width:480,height:22),size:15,weight:.semibold)
        let detail = textLabel(subtitle,NSRect(x:20,y:64,width:480,height:20),size:12,color:CitoPalette.secondaryText)
        heading.alignment = .center; detail.alignment = .center
        box.addSubview(heading); box.addSubview(detail)
        let button=MenuControl(label,frame:NSRect(x:166,y:18,width:188,height:26),radius:6)
        button.identifier=NSUserInterfaceItemIdentifier(id); button.pressed=action; box.addSubview(button)
        root.addSubview(box)
    }
    func applyActivityFilter(revealSelection: Bool = false) {
        let offset = directoryScrollView?.contentView.bounds.origin
        let top = directoryTableView.map { $0.rows(in:$0.visibleRect).location } ?? NSNotFound
        let anchor = activityFilteredItems.indices.contains(top) ? activityFilteredItems[top] : nil
        let deltaY = (offset?.y ?? 0) - (directoryTableView.flatMap { top == NSNotFound ? nil : $0.rect(ofRow:top).minY } ?? 0)
        let selected = activityFilteredItems.indices.contains(directorySelectedIndex) ? activityFilteredItems[directorySelectedIndex] : nil
        let q = directoryQuery.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let serverMatches = serverActivityFilter == activityFilter && serverActivityQuery == directoryQuery
        activityFilteredItems = activityAllItems.filter { item in
            if serverMatches { return true }
            if activityFilter == "me" && item.target_harness != "human" { return false }
            if activityFilter == "unread" && !item.unread { return false }
            if !q.isEmpty {
                let hay = ["@"+item.from_user, item.from_host_name, item.from_title, item.target_title, item.message, item.signal_id, item.listener_id, item.from_harness ?? "",item.target_harness,item.from_scoped_id ?? "",item.from_listener_id ?? "",item.from_display_address ?? "",item.target_display_address ?? "",item.target_harness == "human" ? "You" : ""]
                    .joined(separator: " ").lowercased()
                if !hay.contains(q) { return false }
            }
            return true
        }
        directorySelectedIndex = selected.flatMap { previous in
            activityFilteredItems.firstIndex { $0.hub == previous.hub && $0.signal_id == previous.signal_id }
        } ?? (activityFilteredItems.isEmpty ? 0 : min(max(0, directorySelectedIndex), activityFilteredItems.count - 1))
        let current = activityFilteredItems.indices.contains(directorySelectedIndex) ? activityFilteredItems[directorySelectedIndex] : nil
        let selectionChanged = selected?.hub != current?.hub || selected?.signal_id != current?.signal_id
        updateDirectoryShortcuts()
        updateDirectoryEmptyState()
        let count = serverMatches ? max(activityTotal, activityFilteredItems.count) : activityFilteredItems.count
        directorySummaryLabel?.stringValue = "\(count) message\(count == 1 ? "" : "s") · \(serverMatches ? activityUnreadCount : activityFilteredItems.filter { $0.unread }.count) unread"
        if let table = directoryTableView {
            table.reloadData()
            if !activityFilteredItems.isEmpty {
                table.selectRowIndexes(IndexSet(integer: directorySelectedIndex), byExtendingSelection: false)
                if revealSelection || selectionChanged { table.scrollRowToVisible(directorySelectedIndex) }
            }
            table.layoutSubtreeIfNeeded()
            if !revealSelection && !selectionChanged, let offset = offset {
                let index = anchor.flatMap { previous in activityFilteredItems.firstIndex { $0.hub == previous.hub && $0.signal_id == previous.signal_id } }
                let y = index.map { table.rect(ofRow:$0).minY + deltaY } ?? offset.y
                directoryScrollView?.contentView.scroll(to:NSPoint(x:offset.x,y:y))
                if let scroll = directoryScrollView { scroll.reflectScrolledClipView(scroll.contentView) }
            }
        }
    }

    func seedDirectoryPeersIfEmpty() {
        // Until the first Directory reply, the latest state frame owns these seed rows.
        guard directoryAllPeers.isEmpty || serverDirectoryQuery == nil else { return }
        serverDirectoryQuery = nil
        directoryAllPeers = listeners.map { listener in
            let user = listener.user ?? listener.owner_user ?? hubs.first(where: { $0.name == listener.hub })?.user ?? ""
            let host = (listener.host_name ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            return RemotePeer(hub: listener.hub, listener_id: listener.listener_id,
                public_id: listener.public_id, scoped_id: listener.scoped_id,
                user: user, host_id: listener.host_id ?? "", host_name: host.isEmpty ? "This Mac" : host, harness: listener.harness,
                display_address: listener.display_address, title: listener.title, about: listener.about ?? "",
                scope: listener.scope, last_seen_at: "", session_id: listener.session_id, custom_title: listener.custom_title, cwd: listener.cwd, repo: listener.repo,
                branch: listener.branch, is_self_principal: true, channels: listener.channels)
        } + myRemote
        applyDirectoryFilter(fromDaemonFrame: false, daemonMatchedCount: nil)
    }

    func openDirectoryPanel(mode: String = "directory") {
        ensureDirectoryPanel()
        seedDirectoryPeersIfEmpty()
        directoryVisible = true
        if directoryMode != mode { latestDirectoryActionRequestId = nil; clearDirectoryActionError() }
        directoryMode = mode
        updateDirectoryWarnings()
        queryDirectory(op:"open_directory")
        updateDirectoryModePills()
        updateDirectoryFacetStyles()
        if mode == "activity" {
            applyActivityFilter(revealSelection:true)
        } else {
            applyDirectoryFilter(fromDaemonFrame: false, daemonMatchedCount: nil)
        }
        if !headless, let window = directoryPanel {
            NSApp.setActivationPolicy(.regular)
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
        if !headless && !settingsVisible && !directoryVisible { NSApp.setActivationPolicy(.accessory) }
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
        let offset = directoryScrollView?.contentView.bounds.origin
        let top = directoryTableView.map { $0.rows(in:$0.visibleRect).location } ?? NSNotFound
        let anchorId = directoryFilteredPeers.indices.contains(top) ? directoryFilteredPeers[top].scoped_id : nil
        let deltaY = (offset?.y ?? 0) - (directoryTableView.flatMap { top == NSNotFound ? nil : $0.rect(ofRow:top).minY } ?? 0)
        let previousSelectedId = directoryFilteredPeers.indices.contains(directorySelectedIndex) ? directoryFilteredPeers[directorySelectedIndex].scoped_id : nil
        let sameServerQuery = serverDirectoryQuery == directoryQuery && serverDirectoryScope == directoryScopeFilter && serverDirectoryHarness == directoryHarnessFilter
        func shadowKey(_ peer: RemotePeer) -> String {
            if peer.harness == "human" && peer.is_self_principal == true { return "\(peer.user):human" }
            return "\(peer.user):\(peer.harness):\(peer.session_id ?? peer.listener_id):\(peer.host_id)"
        }
        let sharedKeys = Set(directoryAllPeers.filter { peer in peer.hub != "local" && hubs.contains { $0.name == peer.hub && $0.status == "connected" } }.map(shadowKey))
        let coalescedPeers = directoryAllPeers.filter { $0.hub != "local" || !sharedKeys.contains(shadowKey($0)) }
        directoryFilteredPeers = (fromDaemonFrame || sameServerQuery) ? coalescedPeers : coalescedPeers.filter { matchesDirectoryPeer($0) }
        directoryMatchedCount = daemonMatchedCount ?? (sameServerQuery ? serverDirectoryMatchedCount : directoryFilteredPeers.count)
        if directoryFilteredPeers.isEmpty {
            directorySelectedIndex = 0
        } else if let prev = previousSelectedId, let idx = directoryFilteredPeers.firstIndex(where: { $0.scoped_id == prev }) {
            directorySelectedIndex = idx
        } else {
            directorySelectedIndex = min(max(0, directorySelectedIndex), directoryFilteredPeers.count - 1)
        }
        let currentSelectedId = directoryFilteredPeers.indices.contains(directorySelectedIndex) ? directoryFilteredPeers[directorySelectedIndex].scoped_id : nil
        let selectionChanged = previousSelectedId != currentSelectedId
        updateDirectoryEmptyState()
        directorySummaryLabel?.stringValue = "\(directoryMatchedCount) of \(directoryOnlineCount) online"
        if let table = directoryTableView {
            table.reloadData()
            if !directoryFilteredPeers.isEmpty {
                table.selectRowIndexes(IndexSet(integer: directorySelectedIndex), byExtendingSelection: false)
                if selectionChanged { table.scrollRowToVisible(directorySelectedIndex) }
            }
            table.layoutSubtreeIfNeeded()
            if !selectionChanged, let offset = offset {
                let index = anchorId.flatMap { id in directoryFilteredPeers.firstIndex { $0.scoped_id == id } }
                let y = index.map { table.rect(ofRow:$0).minY + deltaY } ?? offset.y
                directoryScrollView?.contentView.scroll(to:NSPoint(x:offset.x,y:y))
                if let scroll = directoryScrollView { scroll.reflectScrolledClipView(scroll.contentView) }
            }
        }
    }

    func deleteActivity(_ item: ActivityItem) {
        emit(["op":"clear_activity_history","hub":item.hub,"signal_id":item.signal_id,"filter":activityFilter])
    }

    func handleDirectoryKey(_ key: String) {
        if (directoryPanel?.firstResponder as? NSTextInputClient)?.hasMarkedText() == true { return }
        if handleTextEditShortcut(key, in: directoryPanel) { return }
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
        case "command-comma": openSettingsPanel()
        case "delete":
            guard directoryMode == "activity", !(directoryPanel?.firstResponder is NSTextView),
                  activityFilteredItems.indices.contains(directorySelectedIndex) else { return }
            deleteActivity(activityFilteredItems[directorySelectedIndex])
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
        if let field=obj.object as? NSTextField, field.identifier?.rawValue == "host_hub_port" { hostHubPortDraft=field.stringValue; hostHubPortSaved=false; hostHubPortError=nil; if let root=settingsPanel?.contentView { findControl(in:root,id:"apply_host_hub_port")?.isEnabled = !hostHubPortPending && Int(field.stringValue) != settings?.host_hub.port } }
        if let field = obj.object as? NSTextField, let id = field.identifier?.rawValue {
            if id.hasPrefix("channel_field:"),
               let ch=(channelSettingsByHub[effectiveChannelHub] ?? []).first(where:{id.hasPrefix("channel_field:\($0.id):")}),
               let schema=ch.fields.first(where:{id == "channel_field:\(ch.id):\($0.key)"}) {
                let key = "\(effectiveChannelHub)|\(ch.id)"
                channelUiStates[key, default: ChannelUiState()].drafts[schema.key] = field.stringValue
                if ch.pairing_manual_disclosure == true, !ch.configured, let root=settingsPanel?.contentView, let test=findControl(in:root,id:"test_channel:\(ch.id)") {
                    test.isEnabled=ch.fields.filter{$0.scope != "hub" && $0.hidden != true}.contains { f in
                        let value=channelUiStates[key]?.drafts[f.key] ?? f.value ?? ""
                        return !value.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty
                    }
                }
            }
            else if id.hasPrefix("settings_hub_") { addHubDraftGeneration += 1; inviteParseRequest="" }
        }
        syncAddHubFieldsFromControls()
        if let field=obj.object as? NSTextField, field.identifier?.rawValue == "settings_hub_url" {
            let raw=field.stringValue.trimmingCharacters(in:.whitespacesAndNewlines)
            if raw.hasPrefix("cito setup ") || raw.hasPrefix("{") {
                inviteParseRequest=UUID().uuidString
                emit(["op":"paste_hub_invite","invite":raw,"parse_only":true,"request_id":inviteParseRequest])
            }
        }
        settingsFieldControls["settings_hub_name"]?.placeholderString = hubNamePlaceholder
    }

    var hubNamePlaceholder: String {
        addHubUrl.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "e.g. studio or work" : "Default: " + proposedHubName("", url:addHubUrl, existing:settings?.hubs ?? [], creating:!editingExistingHub, original:editingOriginalHub)
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
        if textView.hasMarkedText() { return false }
        if control.identifier?.rawValue.hasPrefix("rename_field:") == true {
            if commandSelector == #selector(NSResponder.cancelOperation(_:)) {
                renamingSessionKey = nil; rebuild(); return true
            }
            if commandSelector == #selector(NSResponder.insertNewline(_:)), let key = renamingSessionKey,
               let listener = listeners.first(where: { "\($0.harness):\($0.session_id)" == key }) {
                let newTitle = textView.string
                guard !newTitle.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, newTitle.unicodeScalars.count <= 128 else { NSSound.beep(); return true }
                emit(["op":"rename_session", "harness":listener.harness, "session_id":listener.session_id, "title":newTitle])
                renamingSessionKey = nil; rebuild(); return true
            }
            return false
        }
        if control.window === settingsPanel {
            if commandSelector == #selector(NSResponder.insertNewline(_:)) { handleSettingsKey("return"); return true }
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
        if item.message_truncated != true { latestDirectoryActionRequestId = nil; clearDirectoryActionError(); copyText(item.message,control:control); return }
        let request = UUID().uuidString
        pendingMessageCopy = PendingMessageCopy(requestId:request,hub:item.hub,signalId:item.signal_id)
        emit(["op":"copy_activity_message","request_id":request,"hub":item.hub,"signal_id":item.signal_id])
    }
    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        guard directoryMode == "activity", row == directorySelectedIndex, activityFilteredItems.indices.contains(row) else { return directoryMode == "activity" ? 60 : 52 }
        let text = activityFilteredItems[row].message
        return activityRowHeight(text, selected: true)
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
            cell.configureActivity(with: item, selected: row == directorySelectedIndex, harnesses: harnessMap, onCopy: { [weak self] control in self?.copyActivity(item,control:control) }, onDelete: { [weak self] in self?.deleteActivity(item) }) { [weak self] in
                guard let self = self else { return }
                self.replyToActivity(item)
            }
            return cell
        }
        guard directoryFilteredPeers.indices.contains(row) else { return nil }
        let peer = directoryFilteredPeers[row]
        cell.configure(with: peer, selected: row == directorySelectedIndex, harnesses: harnessMap, onCopy: { [weak self] text, control in self?.copyText(text,control:control) }) { [weak self] in
            guard let self = self else { return }
            self.openComposer(target: peer.display_address, hub: peer.hub ?? "", replySignalId: nil)
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
                    cell.configureActivity(with:item,selected:index==row,harnesses:harnessMap,onCopy:{ [weak self] c in self?.copyActivity(item,control:c) },onDelete:{ [weak self] in self?.deleteActivity(item) },onReply:{ [weak self] in self?.replyToActivity(item) })
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
        if surface == "banner", effects.count != 1 || effects.first?.material != .popover || (view as? SurfaceView)?.fillColor != .clear { diagnostic("render failed: live banner material mismatch"); return }
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
        // Clip into a separate buffer: drawing a CGImage back into its own backing
        // store aliases pixels. The bitmap CGContext uses pixel coordinates, so
        // scale once to express the rounded path and image rectangle in points.
        guard let captured=bitmap.cgImage,
              let outputBitmap=NSBitmapImageRep(bitmapDataPlanes:nil,pixelsWide:bitmap.pixelsWide,pixelsHigh:bitmap.pixelsHigh,bitsPerSample:8,samplesPerPixel:4,hasAlpha:true,isPlanar:false,colorSpaceName:.deviceRGB,bytesPerRow:0,bitsPerPixel:0),
              let context=NSGraphicsContext(bitmapImageRep:outputBitmap)?.cgContext else { diagnostic("render failed: output bitmap allocation"); return }
        outputBitmap.size=view.bounds.size
        context.clear(CGRect(x:0,y:0,width:outputBitmap.pixelsWide,height:outputBitmap.pixelsHigh))
        context.scaleBy(x:2,y:2)
        let radius=view.layer?.cornerRadius ?? 10
        context.addPath(CGPath(roundedRect:view.bounds,cornerWidth:radius,cornerHeight:radius,transform:nil)); context.clip()
        context.draw(captured,in:view.bounds)
        guard let data=outputBitmap.representation(using:.png,properties:[:]) else { diagnostic("render failed: PNG encoding"); return }
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
        let jargonPattern = "(?i:\\bhandle\\b|human nodes?|every human|each human|\\breceive wakes\\b|\\bwake banners?\\b|\\bcito_listen\\b|\\bhub service:|\\bchannel inventory\\b|\\bui action exceeds\\b)" + (hubs.count <= 1 ? "|on 'local'" : "") + "|local://|Handle:|Human Node|Human listening|Human receiver|All Harnesses|\\bSession\\b|· (none|token|oidc|auth_required|disconnected)\\b|\\bPassive\\b|\\[Passive\\]|^[A-Z0-9_]{3,}:"
        func name(_ view: NSView) -> String {
            view.identifier?.rawValue ?? (view as? NSTextField)?.stringValue ?? String(describing:type(of:view))
        }
        func isDataRow(_ view: NSView) -> Bool {
            var ancestor: NSView? = view
            while let current = ancestor {
                let id = current.identifier?.rawValue ?? ""
                if id == "host_hub_endpoint" || id.hasPrefix("dir_") || id.hasPrefix("act_") || id.hasPrefix("session:") || id.hasPrefix("candidate:") || id.hasPrefix("remote:") || id.hasPrefix("banner_data:") || id.hasPrefix("channel_summary:") || current is DirectoryPeerCellView { return true }
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
            let modalBackground = uninstallConfirmOpen && (view.identifier?.rawValue == "settings_header" || view.identifier?.rawValue == "settings_footer" || view is NSScrollView && view.window === settingsPanel)
            let enabled = enabled && !modalBackground && ((view as? NSControl)?.isEnabled ?? true)
            let base = background ?? resolved(CitoPalette.surfaceBackground, in: view.effectiveAppearance)
            var surface = base
            // Resolve dynamic layer-backed fills in the same appearance as the text.
            if let control = view as? MenuControl { control.applyVisualState() }
            if let fill = view as? SurfaceView { fill.applyLayerColors() }
            let fill: NSColor?
            if let scroll = view as? NSScrollView {
                fill = scroll.drawsBackground ? resolved(scroll.backgroundColor, in: view.effectiveAppearance) : nil
            } else {
                fill = view.layer?.backgroundColor.flatMap { NSColor(cgColor: $0) }
            }
            if let fill = fill {
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
                    truncated = requiredWidth > drawingWidth
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
                if id.hasPrefix("act_title:"), let item = (label.superview as? DirectoryPeerCellView)?.activityItem {
                    // Provenance comes from the rendered cell, including its Hub. Remove
                    // only verified user spans; generated targets and held suffixes remain.
                    let senderTitle = distinctAbout(item.from_title,title:"",user:item.from_user)
                    let senderSpan = "@\(item.from_user)" + (senderTitle.map { " (\($0))" } ?? "") + " → "
                    if generatedText.hasPrefix(senderSpan) {
                        var target = String(generatedText.dropFirst(senderSpan.count))
                        if item.target_harness != "human", target.hasPrefix(item.target_title) {
                            target = String(target.dropFirst(item.target_title.count))
                        }
                        generatedText = "→ " + target
                    }
                }
                let dataJargon = (id == "banner_data:1" && label.stringValue.hasPrefix("[Passive]")) || (id.hasPrefix("act_title:") && generatedText.range(of:jargonPattern,options:.regularExpression) != nil)
                if dataJargon || !data && label.stringValue.range(of:jargonPattern,options:.regularExpression) != nil { jargonLeaks.append(id) }
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
                    let addressMatches = address.map { expected in
                        label == expected || (control.toolTip == expected && label == compactAddressLabel(expected,maxWidth:control.frame.width,font:control.label.font ?? .systemFont(ofSize:11),paddingX:prefix == "copy_dir:" ? 11 : 3))
                    } ?? false
                    let full = address ?? ""
                    if label.isEmpty || !addressMatches || (harness == "human" && !full.contains("@")) || (!hub.isEmpty && hub != defaultHub && !full.hasPrefix("\(hub):")) { addressMismatches.append(id) }
                }
            }
            if visible, let scroll = view as? NSScrollView {
                let defaultLayout: Bool
                switch name(scroll) {
                case "settings_apps_tab": defaultLayout = (settings?.apps.count ?? 0) <= 3
                case "settings_channels_tab": defaultLayout = settings?.channels?.count == 1 && !channelUiStates.values.contains(where: { $0.editingGateway || $0.editingPersonal || $0.manualEntry }) && settings?.hubs.first(where:{$0.name==effectiveChannelHub})?.human_paused != true && (settings?.channels ?? []).allSatisfy { (channelUiStates["\(effectiveChannelHub)|\($0.id)"]?.pairingInstruction ?? "").isEmpty && $0.status != "error" && $0.warning != true && channelUiStates["\(effectiveChannelHub)|\($0.id)"]?.testResult == nil } && (settings?.channels?.first?.fields.count ?? 0) <= 2
                case "settings_hubs_tab": defaultLayout = !addHubFormOpen && (settings?.hubs.count ?? 0) <= 2 && (settings?.hubs ?? []).allSatisfy { $0.status == "connected" && $0.error == nil }
                default: defaultLayout = false
                }
                if defaultLayout && settingsBannerError == nil && (settingsScrollOffsets[name(scroll)] ?? 0) == 0, let doc = scroll.documentView, doc.frame.height > scroll.contentSize.height + 0.75 { clippedViews.append(name(scroll)+": default viewport needs scrolling") }
                if ((scroll.hasVerticalScroller || scroll.hasHorizontalScroller) && ((scroll as? OverlayScrollView)?.nativeScrollerStyle ?? scroll.scrollerStyle) != .overlay) || (scroll.hasHorizontalScroller && scroll.frame.height < 34) { overlappingSiblings.append(name(scroll)+": scroller geometry") }
                if let doc = scroll.documentView {
                    for pill in doc.subviews.compactMap({ $0 as? MenuControl }) where pill.style == .prominent {
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
                let edgeItems = children.filter { child in child.frame.height > 5 && (child is NSControl || ["settings_app_tier:","stratum_hub_status:"].contains { child.identifier?.rawValue.hasPrefix($0) == true }) }
                let id = name(view)
                if ["header_hub_strip", "targetStrip"].contains(id), let scroll = view as? NSScrollView, !children.isEmpty {
                    let trailingInset: CGFloat = id == "header_hub_strip" ? 2 : 0
                    if (children.map { $0.frame.maxX }.max() ?? 0) < scroll.contentSize.width - trailingInset - 2 { clippedViews.append(id + ": trailing pills are not edge anchored") }
                }
                let stratum = ["stratum_hub:","stratum_user:"].contains(where: { id.hasPrefix($0) })
                if ["footer","settings_footer","directory_footer"].contains(id) || stratum || ["settings_hub:","settings_app:"].contains(where: { id.hasPrefix($0) }) || id == "directory_facet_bar" && directoryMode == "activity" {
                    var rows: [(String,CGFloat)] = []
                    if let top = edgeItems.map({ $0.frame.midY }).max() { rows.append(("top",top)) }
                    if stratum, let bottom = edgeItems.map({ $0.frame.midY }).min(), bottom != rows.first?.1 { rows.append(("bottom",bottom)) }
                    for (row,y) in rows {
                        let items = edgeItems.filter { abs($0.frame.midY - y) <= 8 }
                        if (items.map { $0.frame.maxX }.max() ?? 0) < parentBounds.width - 20.5 { clippedViews.append(id + ": " + row + " row is not edge anchored") }
                    }
                }
                for (index, child) in children.enumerated() {
                    let f = frame(child)
                    if view is SurfaceView && (view.layer?.borderWidth ?? 0)>0 && view.frame.width>=60 && view.frame.height>=32
                       && !["settings_panel","directory_panel","banner_root"].contains(id),
                       f.minX<5.5 || f.minY<5.5 || f.maxX>parentBounds.width-5.5 || f.maxY>parentBounds.height-5.5 {
                        clippedViews.append("\(name(view)) > \(name(child)): card padding < 6pt")
                    }
                    if f.minX < -0.5 || f.minY < -0.5 || f.maxX > parentBounds.width + 0.5 || f.maxY > parentBounds.height + 0.5 {
                        clippedViews.append("\(name(view)) > \(name(child))")
                    }
                    for other in children.dropFirst(index+1) where child.identifier?.rawValue != "uninstall_modal_overlay" && other.identifier?.rawValue != "uninstall_modal_overlay" && f.insetBy(dx:0.5,dy:0.5).intersects(frame(other).insetBy(dx:0.5,dy:0.5)) {
                        overlappingSiblings.append("\(name(view)): \(name(child)) / \(name(other))")
                    }
                }
            }
            var result: [String: Any] = ["id": (view as? DirectoryPeerCellView)?.inspectionId ?? view.identifier?.rawValue ?? "", "width": view.frame.width,
                "height": view.frame.height, "x": view.frame.origin.x, "y": view.frame.origin.y,
                "radius": view.layer?.cornerRadius ?? 0, "hidden": view.isHidden,
                "children": childViews.map { project($0, visible: visible, background: surface, opacity: opacity, enabled: enabled) }]
            if let effect=view as? NSVisualEffectView { result["material"]=effect.material.rawValue }
            if view === panel?.contentView {
                let effects=view.subviews.compactMap { $0 as? NSVisualEffectView }
                result["accessibility_role"]=view.accessibilityRole()?.rawValue ?? ""
                result["live_material_valid"]=effects.count == 1 && effects.allSatisfy { $0.material == .popover && !$0.isHidden } && (view as? SurfaceView)?.fillColor == .clear
            }
            result["contrast"] = contrast
            result["background"] = components(fill)
            result["border"] = components(view.layer?.borderColor.flatMap { NSColor(cgColor: $0) })
            result["border_width"] = view.layer?.borderWidth ?? 0
            if view is SurfaceView, view.accessibilityRole() == .staticText || view.identifier?.rawValue == "composer_target", let label=view.subviews.compactMap({$0 as? NSTextField}).first {
                result["text"]=label.stringValue; result["tooltip"]=view.toolTip ?? ""
                result["accessibility_role"]=view.accessibilityRole()?.rawValue ?? ""
            }
            if let image = view as? NSImageView {
                view.effectiveAppearance.performAsCurrentDrawingAppearance { result["tint"] = components(image.contentTintColor) }
            }
            if let scroll = view as? NSScrollView {
                result["scroller_style"] = ((scroll as? OverlayScrollView)?.nativeScrollerStyle ?? scroll.scrollerStyle).rawValue
                result["draws_background"] = scroll.drawsBackground
                result["content_x"] = scroll.contentView.bounds.origin.x
                result["content_y"] = scroll.contentView.bounds.origin.y
            }
            if let cell = view as? DirectoryPeerCellView { result["reuse_identifier"] = cell.identifier?.rawValue ?? "" }
            if let label = view as? NSTextField {
                result["text"] = label is NSSecureTextField ? "••••" : label.stringValue
                result["secure"] = label is NSSecureTextField
                result["placeholder"] = label.placeholderString ?? ""
                result["tooltip"] = label.toolTip ?? ""
                result["label_width"] = label.cell?.drawingRect(forBounds: label.bounds).width ?? label.bounds.width
                result["label_measured_width"] = label.cell?.cellSize.width ?? label.attributedStringValue.size().width
            }
            if let control = view as? NSControl { result["enabled"] = control.isEnabled; result["alpha"] = control.alphaValue }
            if let control = view as? MenuControl {
                result["text"] = control.label.stringValue; result["tooltip"] = control.toolTip ?? ""
                result["accessibility_label"] = control.accessibilityLabel() ?? ""
                result["label_width"] = control.label.frame.width
                result["label_measured_width"] = measureTextWidth(control.label.stringValue,font:control.label.font ?? .systemFont(ofSize:12))
                result["green"] = control.label.textColor == CitoPalette.successText
                result["hovered"] = control.isHovered
                control.effectiveAppearance.performAsCurrentDrawingAppearance { result["foreground"] = components(control.label.textColor) }
                if let symbol = control.symbolName { result["symbol"] = symbol }
            }
            return result
        }
        func checkHitTests(_ view: NSView, root: NSView, surface: String, visible: Bool = true) {
            let visible = visible && !view.isHidden && view.alphaValue > 0.01
            guard visible else { return }
            if let control = view as? MenuControl, control.isEnabled, control.pressed != nil {
                // Scrolling may clip the nominal center onto a hit-test boundary.
                // Sample the interior of the portion the user can actually click.
                let visibleBounds = control.visibleRect
                let center = NSPoint(x:visibleBounds.midX,y:visibleBounds.midY)
                let modal = settingsPanel?.contentView?.subviews.first { $0.identifier?.rawValue == "uninstall_modal_overlay" }
                let inModal = modal.map { control.isDescendant(of:$0) } ?? false
                if !(surface == "settings" && uninstallConfirmOpen && !inModal), !visibleBounds.isEmpty {
                    let point = control.convert(center,to:root)
                    if root.hitTest(point) !== control {
                        overlappingSiblings.append("\(surface) > \(name(control)): occluded in hitTest")
                    }
                }
            }
            for child in view.subviews { checkHitTests(child,root:root,surface:surface,visible:visible) }
        }
        for item in cardMenu.items where !item.isHidden {
            if let root = item.view { checkHitTests(root,root:root,surface:"menu") }
        }
        if let root = settingsPanel?.contentView { checkHitTests(root,root:root,surface:"settings") }
        if let root = directoryPanel?.contentView { checkHitTests(root,root:root,surface:"directory") }
        if let root = panel?.contentView, visible { checkHitTests(root,root:root,surface:"banner") }
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
        if let button = button {
            let title = button.title + button.attributedTitle.string
            if title.unicodeScalars.contains(where: { $0.value > 0x27BF && $0.properties.isEmoji }) {
                jargonLeaks.append("status_button: color emoji")
            }
        }
        let font = button.flatMap { $0.attributedTitle.length > 0 ? $0.attributedTitle.attribute(.font, at: 0, effectiveRange: nil) as? NSFont : nil }
        let traits = font?.fontDescriptor.object(forKey: .traits) as? [NSFontDescriptor.TraitKey: Any]
        let effectiveClipboard = !clipboardString.isEmpty ? clipboardString : (appPasteboard.string(forType: .string) ?? "")
        let snapshot: [String: Any] = ["items": items, "truncated_labels": truncatedLabels, "overlapping_siblings": overlappingSiblings, "clipped_views": clippedViews, "icon_alpha": probes, "count": uniqueSessionCount,
            "home_basename": (NSHomeDirectory() as NSString).lastPathComponent,
            "low_contrast_labels": lowContrastLabels, "jargon_leaks":jargonLeaks, "address_mismatches":addressMismatches, "banner_items":bannerItems, "wake_action_text": wakeActionText,
            "edit_menu": NSApp.mainMenu?.items.first(where: { $0.submenu?.title == "Edit" })?.submenu?.items.map { ["title": $0.title, "key": $0.keyEquivalent, "action": $0.action.map(NSStringFromSelector) ?? "", "shift": $0.keyEquivalentModifierMask.contains(.shift)] as [String: Any] } ?? [],
            "key_panels": [settingsPanel, directoryPanel].compactMap { window -> [String: Any]? in
                guard let window = window else { return nil }
                return ["titled": window.styleMask.contains(.titled), "full_size_content": window.styleMask.contains(.fullSizeContentView),
                    "hidden_title": window.titleVisibility == .hidden, "transparent_titlebar": window.titlebarAppearsTransparent,
                    "hidden_window_buttons": [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton].allSatisfy { window.standardWindowButton($0)?.isHidden != false }]
            },
            "field_editors": [settingsPanel, directoryPanel].compactMap { window -> [String: Any]? in
                guard let editor = window?.firstResponder as? NSTextView else { return nil }
                return ["id": (editor.delegate as? NSTextField)?.identifier?.rawValue ?? "", "selection_length": editor.selectedRange().length,
                    "selection_location": editor.selectedRange().location, "editable": editor.isEditable,
                    "can_undo": editor.undoManager?.canUndo ?? false, "undo_groups": editor.undoManager?.groupingLevel ?? -1,
                    "automatic_groups": editor.undoManager?.groupsByEvent ?? false,
                    "attached": (editor.delegate as? NSTextField)?.currentEditor() === editor]
            },
            "available_count": available.count, "rebuilds": rebuildCount,
            "directory_open": directoryVisible, "directory_mode": directoryMode, "activity_filter": activityFilter,
            "unread_count": unreadCount, "composer_open": !(directoryComposerBar?.isHidden ?? true),
            "composer_target": composerTarget, "composer_reply_signal_id": composerReplySignalId ?? "",
            "directory_online_count": directoryOnlineCount,
            "directory_matched_count": directoryMatchedCount, "directory_query": directoryQuery,
            "directory_scope": directoryScopeFilter, "directory_harness": directoryHarnessFilter,
            "directory_selected_id": selectedId, "directory_selected_index": directorySelectedIndex, "directory_items": directoryItems,
            "directory_selection_visible": directoryTableView.map { table in directorySelectedIndex >= 0 && directorySelectedIndex < table.numberOfRows && table.visibleRect.contains(table.rect(ofRow:directorySelectedIndex)) } ?? false,
            "settings_open": settingsVisible, "settings_tab": settingsActiveTab,
            "settings_launch_at_login": settings?.launch_at_login ?? false, "settings_items": settingsItems,
            "icon_template": statusImage?.isTemplate ?? false, "icon_width": statusImage?.size.width ?? 0,
            "icon_height": statusImage?.size.height ?? 0, "clipboard": effectiveClipboard,"banner_hovered":bannerHovered,"banner_visible":visible,
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
    func scheduleBannerDismiss(_ milliseconds: Int) {
        timer?.cancel(); timer = nil
        guard visible && !bannerHovered else { return }
        let work = DispatchWorkItem { [weak self] in self?.hide(clicked:false) }
        timer = work
        DispatchQueue.main.asyncAfter(deadline:.now() + .milliseconds(milliseconds),execute:work)
    }
    func hide(clicked: Bool) {
        timer?.cancel(); timer = nil
        bannerHovered = false
        (panel?.contentView as? ClickView)?.resetHover()
        let clickedPayload = (clicked && visible) ? payload : nil
        visible = false; panel?.orderOut(nil)
        if let val = clickedPayload {
            if val.target_harness == "human" { openDirectoryForReply(val) }
            else { openActivityForSignal(val) }
        }
        if headless { diagnostic(clicked ? "banner clicked" : "banner hidden") }
    }
    func refreshBannerColors() {
        guard labels.count == 3 else { return }
        accentBar?.fillColor = CitoPalette.warningText
        semanticPill?.fillColor = payload?.effective_wake == false ? CitoPalette.warningSurface : .labelColor.withAppearanceAlpha(0.08)
        semanticPill?.strokeColor = payload?.effective_wake == false ? CitoPalette.warningBorder : nil
        semanticPillLabel?.textColor = payload?.effective_wake == false ? CitoPalette.warningText : CitoPalette.secondaryText
        labels[2].textColor = bannerHovered ? CitoPalette.infoText : CitoPalette.secondaryText
    }
    func show(_ value: Payload) {
        let human = value.target_harness == "human"
        let senderTitle = distinctAbout(value.from_title,title:human ? "" : value.target_title,user:value.from_user)
        let metadata = human
            ? "@\(value.from_user)" + (senderTitle.map { " · \($0)" } ?? "") + " → You"
            : "@\(value.from_user) → \(value.target_title)" + (senderTitle.map { " (\($0))" } ?? "")
        let semanticLabel = !value.effective_wake ? "Queued" : (displayedCount > 1 ? "×\(displayedCount)" : "")
        let actionText = human ? "Reply in Cito ↩" : "View in Activity ↗"
        wakeActionText = actionText
        visible = true
        if panel == nil {
            let window = NSPanel(contentRect:NSRect(x:0,y:0,width:380,height:112),
                                 styleMask:[.borderless,.nonactivatingPanel],backing:.buffered,defer:false)
            window.level = .floating
            window.collectionBehavior = [.canJoinAllSpaces,.fullScreenAuxiliary]
            window.isFloatingPanel = true; window.hidesOnDeactivate = false
            window.isOpaque = false; window.backgroundColor = .clear; window.hasShadow = true
            window.appearance = testAppearance
            let root = ClickView(frame:window.contentView!.bounds)
            root.identifier=NSUserInterfaceItemIdentifier("banner_root")
            root.clicked = { [weak self] in self?.hide(clicked:true) }
            root.appearanceChanged = { [weak self] in self?.refreshBannerColors() }
            root.hoverChanged = { [weak self] hovered in
                guard let self = self else { return }
                self.bannerHovered = hovered
                self.timer?.cancel(); self.timer = nil
                self.refreshBannerColors()
                if !hovered { self.scheduleBannerDismiss(min(2000,self.dismissMs)) }
            }
            root.setAccessibilityElement(true); root.setAccessibilityRole(.group)
            root.layer?.masksToBounds = true
            let effect = NSVisualEffectView(frame:root.bounds)
            effect.material = .popover; effect.blendingMode = .behindWindow; effect.state = .active
            root.addSubview(effect)
            let bar = roundedView(NSRect(x:0,y:0,width:3,height:112),radius:0,color:CitoPalette.warningText)
            bar.identifier = NSUserInterfaceItemIdentifier("banner_accent")
            root.addSubview(bar); accentBar = bar
            let badge = roundedView(NSRect(x:14,y:68,width:30,height:30),radius:8,color:.clear)
            badge.identifier=NSUserInterfaceItemIdentifier("banner_icon_badge")
            let image = NSImageView(frame:badge.bounds)
            badge.addSubview(image); root.addSubview(badge); iconBadge = badge; icon = image
            let frames = [NSRect(x:54,y:82,width:284,height:18),
                          NSRect(x:54,y:42,width:310,height:38),NSRect(x:54,y:13,width:270,height:18)]
            for index in 1...3 {
                let label = NSTextField(wrappingLabelWithString:"")
                label.isSelectable = false
                label.font = .systemFont(ofSize:index == 1 ? 12.5 : (index == 2 ? 13 : 11),weight:index == 1 ? .semibold : (index == 2 ? .regular : .medium))
                label.frame = frames[index-1]; label.maximumNumberOfLines = index == 2 ? 2 : 1
                label.lineBreakMode = .byTruncatingTail
                label.identifier = NSUserInterfaceItemIdentifier(index < 3 ? "banner_data:\(index)" : "banner_chrome:\(index)")
                root.addSubview(label); labels.append(label)
            }
            let close = MenuControl("Close notification",frame:NSRect(x:346,y:82,width:20,height:20),symbol:"xmark")
            close.identifier = NSUserInterfaceItemIdentifier("banner_close"); close.style = .subtleHeader
            close.pressed = { [weak self] in self?.hide(clicked:false) }
            root.addSubview(close)
            window.contentView = root; panel = window
        }
        if let badge = iconBadge, let image = icon {
            let from = value.from_harness.flatMap { $0.isEmpty ? nil : $0 }
            let harness = human ? (from ?? value.target_harness) : value.target_harness
            let visual = resolveHarnessVisual(harness,harnesses:harnessMap)
            configureBadge(badge,icon:image,id:harness,symbol:visual.symbol,title:visual.name,tint:visual.tint,inset:6)
        }
        semanticPill?.removeFromSuperview()
        if !semanticLabel.isEmpty {
            let pill = makePillBadge(semanticLabel,id:"banner_semantic_pill",height:20,textColor:value.effective_wake ? CitoPalette.secondaryText : CitoPalette.warningText,fill:value.effective_wake ? .labelColor.withAppearanceAlpha(0.08) : CitoPalette.warningSurface,minWidth:0)
            pill.frame.origin = NSPoint(x:364-pill.frame.width,y:12)
            panel?.contentView?.addSubview(pill); semanticPill = pill
            semanticPillLabel = pill.subviews.compactMap { $0 as? NSTextField }.first
        } else { semanticPill = nil; semanticPillLabel = nil }
        accentBar?.isHidden = value.effective_wake
        labels[0].stringValue = metadata
        labels[1].stringValue = value.displayMessage
        // Measure the actual preview so a one-line message stays pinned to Line 1.
        let height = min(CGFloat(38),ceil((value.displayMessage as NSString).boundingRect(with:NSSize(width:310,height:38),options:[.usesLineFragmentOrigin,.usesFontLeading],attributes:[.font:labels[1].font!]).height))
        labels[1].frame = NSRect(x:54,y:80-height,width:310,height:height)
        labels[2].stringValue = actionText
        labels[2].frame.size.width = (semanticPill?.frame.minX ?? 370)-60
        refreshBannerColors()
        panel?.contentView?.setAccessibilityLabel("\(metadata). \(semanticLabel). \(value.displayMessage). \(actionText)")
        if headless {
            diagnostic("display " + String(data:try! JSONSerialization.data(withJSONObject:[
                "pid":getpid(),"title":human ? "Direct Message" : value.target_title,"subtitle":value.subtitle,"message":value.message,
                "displayed_count":displayedCount,"display_subtitle":metadata,"display_message":value.displayMessage,
                "semantic_label":semanticLabel,"action_label":actionText],options:[.sortedKeys]),encoding:.utf8)!)
            return
        }
        if let screen = NSScreen.main, let window = panel {
            window.setFrameTopLeftPoint(NSPoint(x:screen.visibleFrame.maxX-396,y:screen.visibleFrame.maxY-16))
        }
        panel?.orderFrontRegardless()
    }

}

let app = NSApplication.shared
let companion = Companion()
app.setActivationPolicy(.accessory)
companion.start()
app.run()
