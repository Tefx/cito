import AppKit
import QuartzCore

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
    let title = unreadCount > 0 ? (unreadCount > 99 ? " 99+" : " \(unreadCount)") : ""
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
        let pairedWidth = MenuControl.toggleWidth(id, title:control.label.stringValue, font: control.label.font ?? .systemFont(ofSize:11,weight:.medium))
        control.frame.size.width = min(control.maximumWidth, max(reservedWidth, pairedWidth, MenuControl.intrinsicWidth(control.label.stringValue,
            font:control.label.font ?? .systemFont(ofSize:11,weight:.medium),
            leadingSymbol:control.leadingSymbolView?.isHidden == false ? control.symbolName : nil,
            leadingDot:control.leadingDotView?.isHidden == false,
            paddingX: id.hasPrefix("copy:") || id.hasPrefix("copy_remote:") ? 3 : (id == "open_settings" ? 6 : (id.hasPrefix("scope:") ? 9 : (id == "open_uninstall_confirm" ? 12 : 11))))))
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
    pill.setAccessibilityElement(true); pill.setAccessibilityRole(.staticText); pill.setAccessibilityLabel(text)
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
final class MenuTitleField: NSTextField {
    override var stringValue: String { didSet { (superview as? MenuControl)?.setAccessibilityLabel(stringValue) } }
}
final class MenuControl: NSControl {
    let label: NSTextField
    enum Style { case neutral, prominent, subtleHeader, destructiveHover, destructive }
    override var frame: NSRect { didSet { if oldValue.size != frame.size { layoutContents() } } }
    static func intrinsicWidth(_ title: String, font: NSFont = .systemFont(ofSize: 11, weight: .medium),
                               leadingSymbol: String? = nil, leadingDot: Bool = false, paddingX: CGFloat = 11, minWidth: CGFloat = 0) -> CGFloat {
        max(minWidth, ceil(measureTextWidth(title, font: font)) + paddingX * 2 + (leadingSymbol == nil ? 0 : 17) + (leadingDot ? 12 : 0))
    }
    static func toggleWidth(_ id: String, title: String, font: NSFont = .systemFont(ofSize:11,weight:.medium)) -> CGFloat {
        let states: [(String,String?)]
        let padding: CGFloat
        if id.hasPrefix("scope:") { states = [("Self","lock.fill"),("Team","person.2.fill")]; padding = 9 }
        else if id == "toggle_auto_listen" { states = [("Auto-Listen",nil),("Auto-Listen: On",nil)]; padding = 11 }
        else if id.hasPrefix("toggle_hub:") { states = [("Enable",nil),("Disable",nil)]; padding = 11 }
        else if id == "toggle_host_hub" { states = [("Start Sharing",nil),("Stop Sharing",nil)]; padding = 11 }
        else if id == "toggle_host_hub_auth" { states = [("Access: Trusted",nil),("Access: Token",nil)]; padding = 11 }
        else if id.hasPrefix("toggle_channel:") {
            let name=String(title.dropFirst(title.hasPrefix("Pause ") ? 6 : 7))
            states = [("Pause " + name,nil),("Resume " + name,nil)]; padding = 11
        }
        else { return 0 }
        return states.map { intrinsicWidth($0.0,font:font,leadingSymbol:$0.1,paddingX:padding) }.max() ?? 0
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
    var maximumWidth: CGFloat = .greatestFiniteMagnitude
    var feedbackTimer: Timer?
    var restingTitle: String?
    var restingAccessibilityLabel: String?
    private(set) var isHovered = false
    private var trackingArea: NSTrackingArea?
    static weak var activeHovered: MenuControl?
    private static var hoverWatchTimer: Timer?
    // Headless hooks exercise logical transitions without a visible window or physical pointer.
    static var tracksPhysicalPointer = true
    override var wantsUpdateLayer: Bool { true }
    init(_ title: String, frame: NSRect, symbol: String? = nil, leadingSymbol: String? = nil, color: NSColor = .labelColor, radius: CGFloat = 6, paddingX: CGFloat = 4) {
        self.paddingX = paddingX
        label = MenuTitleField(labelWithString:title)
        symbolName = leadingSymbol ?? symbol
        super.init(frame: frame)
        wantsLayer = true; layer?.cornerRadius = radius
        layer?.cornerCurve = .continuous
        label.font = .systemFont(ofSize:11,weight:.medium)
        label.lineBreakMode = .byTruncatingTail; label.maximumNumberOfLines = 1
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
        toolTip = title
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
            fill = CitoPalette.destructiveFill
            border = .white.withAlphaComponent(isHovered ? 0.28 : 0.18)
            foreground = .white
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
        if feedbackTimer == nil {
            restingTitle = label.stringValue
            restingAccessibilityLabel = accessibilityLabel()
        }
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
            self.setAccessibilityLabel(self.restingAccessibilityLabel)
            self.restingTitle = nil
            self.restingAccessibilityLabel = nil
            self.layoutContents()
            self.applyVisualState()
        }
        feedbackTimer = restore
        applyVisualState()
        RunLoop.main.add(restore, forMode: .common); RunLoop.main.add(restore, forMode: .eventTracking)
    }
}

final class ModalOverlay: NSView {
    override func hitTest(_ point: NSPoint) -> NSView? { guard frame.contains(point) else{return nil}; return super.hitTest(point) ?? self }
    override func mouseDown(with event: NSEvent) {}
}
final class FlippedView: NSView {
    override var isFlipped: Bool { true }
}
