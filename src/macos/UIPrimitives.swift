import AppKit
import QuartzCore

enum StatusIconState: Hashable { case active, idle, warning }
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
        && control.subviews.allSatisfy({ $0 === control.label || $0 === control.leadingSymbolView || $0 === control.centerSymbolView }) {
        control.frame.size.width = control.reservedWidth
        control.layoutContents()
    }
    var left = insetLeft
    var right = container.bounds.width - insetRight
    func place(_ view: NSView, x: CGFloat) {
        view.frame.origin = NSPoint(x: x, y: y + (height - view.frame.height) / 2)
    }
    for view in leftItems {
        view.autoresizingMask.subtract([.width, .minXMargin])
        place(view, x: left); left += view.frame.width + spacing
    }
    for view in rightItems.reversed() {
        view.autoresizingMask.remove(.width)
        view.autoresizingMask.insert(.minXMargin)
        right -= view.frame.width; place(view, x: right); right -= spacing
    }
    if let view = flexible, !view.isHidden {
        view.autoresizingMask.remove(.minXMargin)
        view.autoresizingMask.insert(.width)
        view.frame = NSRect(x: left, y: y + (height - view.frame.height) / 2,
                            width: max(0, right - left), height: view.frame.height)
    }
}

struct PillTrackGeometry {
    static let inset: CGFloat = 2

    let pillHeight: CGFloat
    let pillRadius: CGFloat

    var trackHeight: CGFloat { pillHeight + 2 * Self.inset }
    var trackRadius: CGFloat { pillRadius + Self.inset }

    static func trailingWidth(widths: [CGFloat], spacing: CGFloat) -> CGFloat {
        guard !widths.isEmpty else { return 0 }
        return widths.reduce(0, +) + CGFloat(widths.count - 1) * spacing + 2 * inset
    }
}

enum PillAlignment { case leading, trailing }

private func placePillStrip(_ scroll: NSScrollView, in container: NSView, frame: NSRect, id: String,
                            alignment: PillAlignment, spacing: CGFloat, trackRadius: CGFloat, pills: [MenuControl]) {
    let visible = pills.filter { !$0.isHidden }
    let totalWidth = PillTrackGeometry.trailingWidth(widths: visible.map { $0.frame.width }, spacing: spacing)
    let trackWidth = min(frame.width, totalWidth)
    let scrollFrame = NSRect(x: alignment == .trailing ? frame.maxX - trackWidth : frame.minX,
                             y: frame.minY, width: trackWidth, height: frame.height)
    scroll.frame = scrollFrame
    scroll.identifier = NSUserInterfaceItemIdentifier(id)
    scroll.drawsBackground = false
    scroll.hasHorizontalScroller = false; scroll.hasVerticalScroller = false
    scroll.wantsLayer = true
    scroll.backgroundColor = .labelColor.withAppearanceAlpha(0.06)
    scroll.drawsBackground = true
    scroll.layer?.cornerRadius = trackRadius
    scroll.layer?.cornerCurve = .continuous
    scroll.layer?.masksToBounds = true
    let doc = NSView(frame: NSRect(x: 0, y: 0, width: max(trackWidth, totalWidth), height: frame.height))
    var cursorX = PillTrackGeometry.inset
    for pill in visible {
        pill.frame.origin = NSPoint(x: cursorX, y: (frame.height - pill.frame.height) / 2)
        doc.addSubview(pill); cursorX += pill.frame.width + spacing
    }
    scroll.documentView = doc; container.addSubview(scroll)
}

/// The scroll document and thumb share a coordinate space, independent of track alignment.
final class SegmentedPillStrip: NSScrollView {
    struct Handoff {
        fileprivate let offset: NSPoint
        fileprivate let selectedID: NSUserInterfaceItemIdentifier?
        fileprivate let focusedID: NSUserInterfaceItemIdentifier?
        fileprivate let rect: NSRect?
        fileprivate let motion: Motion?
    }
    fileprivate struct Motion {
        let from: NSRect
        let to: NSRect
        let start: TimeInterval
        let duration: TimeInterval
        var end: TimeInterval { start + duration }
    }

    private let pills: [MenuControl]
    private let geometry: PillTrackGeometry
    private(set) weak var selected: MenuControl?
    private var overlay: CALayer?
    private var motion: Motion?
    var isSliding: Bool { overlay != nil }

    private init(pills: [MenuControl], geometry: PillTrackGeometry) {
        self.pills = pills; self.geometry = geometry
        super.init(frame: .zero)
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

    static func mount(in container: NSView, frame: NSRect, id: String, alignment: PillAlignment = .leading,
                      spacing: CGFloat = 4, geometry: PillTrackGeometry, pills: [MenuControl],
                      selected: MenuControl?, handoff: Handoff? = nil) -> SegmentedPillStrip {
        let visible = pills.filter { !$0.isHidden }
        let stripWidth = max(0, frame.width)
        let pillLimit = max(0, stripWidth - 2 * PillTrackGeometry.inset)
        for pill in visible {
            pill.maximumWidth = min(pill.maximumWidth, pillLimit)
            if pill.frame.width > pill.maximumWidth {
                pill.label.lineBreakMode = .byTruncatingTail
                if (pill.toolTip ?? "").isEmpty { pill.toolTip = pill.label.stringValue }
            }
            pill.setFrameSize(NSSize(width: min(pill.frame.width, pill.maximumWidth), height: geometry.pillHeight))
            pill.layoutContents()
            pill.layer?.cornerRadius = geometry.pillRadius
        }
        let trackFrame = NSRect(x: frame.minX, y: frame.midY - geometry.trackHeight / 2,
                                width: stripWidth, height: geometry.trackHeight)
        let strip = SegmentedPillStrip(pills: visible, geometry: geometry)
        placePillStrip(strip, in: container, frame: trackFrame, id: id, alignment: alignment,
                       spacing: spacing, trackRadius: geometry.trackRadius, pills: visible)
        if visible.count > 1 {
            for pill in visible {
                let action = pill.pressed
                pill.pressed = { [weak strip, weak pill] in
                    if let pill { strip?.select(pill, userInitiated: true) }
                    action?()
                }
                pill.segmentArrow = { [weak strip, weak pill] direction in
                    guard let strip, let pill, let index = strip.pills.firstIndex(where: { $0 === pill }) else { return }
                    let count = strip.pills.count
                    for step in 1..<count {
                        let next = strip.pills[(index + count + direction * step) % count]
                        if next.isEnabled {
                            next.window?.makeFirstResponder(next)
                            next.performClick(nil)
                            return
                        }
                    }
                }
            }
        }
        if let handoff, let doc = strip.documentView {
            let x = max(0, min(handoff.offset.x, doc.bounds.width - strip.contentView.bounds.width))
            strip.contentView.scroll(to: NSPoint(x: x, y: 0))
            strip.reflectScrolledClipView(strip.contentView)
            if let id = handoff.focusedID, let focused = visible.first(where: { $0.identifier == id }) {
                focused.window?.makeFirstResponder(focused)
            }
        }
        strip.selected = selected
        strip.applySelection()
        if let selected {
            strip.reveal(selected)
            if handoff?.selectedID == selected.identifier, let motion = handoff?.motion {
                if motion.to == selected.frame { strip.slide(motion) }
                else if let rect = handoff?.rect {
                    let now = CACurrentMediaTime()
                    strip.slide(Motion(from: rect, to: selected.frame, start: now, duration: motion.end - now))
                }
            }
        }
        return strip
    }

    func capture() -> Handoff {
        Handoff(offset: contentView.bounds.origin, selectedID: selected?.identifier,
                focusedID: pills.first { $0.window?.firstResponder === $0 }?.identifier,
                rect: thumbRect, motion: motion)
    }

    private var thumbRect: NSRect? {
        guard let mask = overlay?.mask else { return selected?.frame }
        return mask.presentation()?.frame ?? motion?.from
    }

    func select(_ pill: MenuControl, userInitiated: Bool = false) {
        guard pills.contains(where: { $0 === pill }), selected !== pill else { return }
        let previous = thumbRect
        finishSlide()
        selected = pill
        applySelection()
        reveal(pill)
        if userInitiated, let previous {
            slide(Motion(from: previous, to: pill.frame, start: CACurrentMediaTime(), duration: CitoMetrics.segmentSlideDuration))
        }
    }

    private func reveal(_ pill: MenuControl) {
        guard let doc = documentView else { return }
        let target = pill.frame.insetBy(dx: -PillTrackGeometry.inset, dy: 0)
        doc.scrollToVisible(target)
    }

    private func applySelection() {
        for pill in pills {
            let active = pill === selected
            if pills.count > 1 {
                pill.style = active ? .prominent : .subtleHeader
                pill.setAccessibilityRole(.radioButton)
                pill.setAccessibilityValue(active ? 1 : 0)
            }
            pill.applyVisualState()
        }
    }

    private func slide(_ motion: Motion) {
        guard MenuControl.allowsMotion, pills.count > 1, motion.end > CACurrentMediaTime(),
              motion.from != motion.to, let doc = documentView else { return }
        // The opaque mask hides the resting ink exactly where the moving blue fill
        // paints white ink. Neither a color crossfade nor a whole-label switch can
        // maintain contrast as a leading/trailing edge crosses the glyphs.
        doc.wantsLayer = true
        let overlay = CALayer()
        overlay.frame = doc.bounds
        overlay.zPosition = 1
        overlay.backgroundColor = CitoPalette.prominentFill.cgColor
        let mask = CALayer()
        mask.backgroundColor = NSColor.black.cgColor
        mask.cornerRadius = geometry.pillRadius; mask.cornerCurve = .continuous
        mask.frame = motion.to
        overlay.mask = mask
        let border = CALayer()
        border.frame = motion.to
        border.cornerRadius = geometry.pillRadius; border.cornerCurve = .continuous
        border.borderColor = NSColor.white.withAlphaComponent(0.22).cgColor
        border.borderWidth = 1
        overlay.addSublayer(border)
        for pill in pills {
            let label = pill.label
            if !label.isHidden, !label.bounds.isEmpty, let bitmap = label.bitmapImageRepForCachingDisplay(in: label.bounds) {
                let color = label.textColor
                label.textColor = .white
                label.cacheDisplay(in: label.bounds, to: bitmap)
                label.textColor = color
                let ink = CALayer(); ink.frame = doc.convert(label.bounds, from: label)
                ink.contents = bitmap.cgImage
                overlay.addSublayer(ink)
            }
            if let indicator = pill.segmentIndicator {
                let dot = CALayer(); dot.frame = doc.convert(indicator.bounds, from: indicator)
                dot.cornerRadius = indicator.cornerRadiusValue; dot.cornerCurve = .continuous
                dot.backgroundColor = NSColor.white.cgColor
                overlay.addSublayer(dot)
            }
            pill.paintsSegmentThumb = false
        }
        self.overlay = overlay
        self.motion = motion
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        CATransaction.setCompletionBlock { [weak self, weak overlay] in
            if let self, let overlay, self.overlay === overlay { self.finishSlide() }
        }
        doc.layer?.addSublayer(overlay)
        let position = CABasicAnimation(keyPath: "position")
        position.fromValue = NSPoint(x: motion.from.midX, y: motion.from.midY)
        position.toValue = NSPoint(x: motion.to.midX, y: motion.to.midY)
        let bounds = CABasicAnimation(keyPath: "bounds")
        bounds.fromValue = NSRect(origin: .zero, size: motion.from.size)
        bounds.toValue = NSRect(origin: .zero, size: motion.to.size)
        for component in [position, bounds] {
            component.duration = motion.duration
            component.timingFunction = CAMediaTimingFunction(name: .linear)
        }
        let animation = CAAnimationGroup()
        animation.animations = [position, bounds]
        animation.duration = motion.duration
        animation.timingFunction = CAMediaTimingFunction(controlPoints: 0.18, 1, 0.32, 1)
        animation.fillMode = .backwards
        for layer in [mask, border] {
            animation.beginTime = layer.convertTime(motion.start, from: nil)
            layer.add(animation, forKey: "slide")
        }
        CATransaction.commit()
    }

    private func finishSlide() {
        overlay?.removeFromSuperlayer()
        overlay = nil; motion = nil
        for pill in pills { pill.paintsSegmentThumb = true }
    }
}

func textLabel(_ title: String, _ frame: NSRect, size: CGFloat = 11, weight: NSFont.Weight = .regular,
               color: NSColor = .labelColor) -> NSTextField {
    let label = NSTextField(labelWithString: title)
    label.frame = frame; label.font = .systemFont(ofSize: size, weight: weight); label.textColor = color
    label.lineBreakMode = .byTruncatingTail; label.maximumNumberOfLines = 1
    return label
}

extension NSTextField {
    var requiredSingleLineWidth: CGFloat {
        // The native cell can retain a narrower cached width after an appearance update.
        max(cell?.cellSize.width ?? 0, attributedStringValue.size().width)
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
    // A borderless label cell reserves two points on each horizontal edge.
    return ceil((text as NSString).size(withAttributes: [.font: font]).width + 4)
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
    if let override = env["CITO_PLUGIN_DIR"], !override.isEmpty { roots.append(URL(fileURLWithPath: override)) }
    let state = rootURL(env["CITO_STATE_DIR"] ?? NSHomeDirectory()+"/.cito") ?? URL(fileURLWithPath:NSHomeDirectory()+"/.cito")
    roots += [state.appendingPathComponent("plugins"),state.appendingPathComponent("runtime/plugins")]
    if let builtin = env["CITO_BUILTIN_PLUGINS_DIR"].flatMap(rootURL) { roots.append(builtin) }
    let executable = URL(fileURLWithPath:CommandLine.arguments[0]).resolvingSymlinksInPath().deletingLastPathComponent()
    if executable.deletingLastPathComponent().deletingLastPathComponent().pathExtension == "app" {
        roots.append(executable.appendingPathComponent("../../../../Resources/plugins"))
    }
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
                   textColor: NSColor, fill: NSColor, border: NSColor? = nil, paddingX: CGFloat = 8, minWidth: CGFloat = 0) -> SurfaceView {
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

private let panelShortcutKeys: [UInt16: String] = [
    9: "v", 8: "c", 7: "x", 0: "a", 6: "z", 13: "w", 46: "m", 40: "k",
    43: ",", 18: "1", 19: "2", 20: "3"
]
private let panelShortcutCharacters = Set(panelShortcutKeys.values)

func panelShortcutCharacter(_ event: NSEvent) -> String? {
    let raw = event.charactersIgnoringModifiers?.lowercased()
    let character = raw == "，" ? "," : raw
    return character.flatMap { panelShortcutCharacters.contains($0) ? $0 : nil } ?? panelShortcutKeys[event.keyCode]
}

func panelShortcutKey(_ event: NSEvent, includingNative: Bool) -> String? {
    let flags = event.modifierFlags.intersection([.command, .shift, .control, .option])
    guard flags == [.command] || flags == [.command, .shift], let key = panelShortcutCharacter(event) else { return nil }
    if !includingNative && ![",", "k", "1", "2", "3"].contains(key) { return nil }
    if flags.contains(.shift) { return key == "z" ? "shift-command-z" : nil }
    return key == "," ? "command-comma" : "command-" + key
}

func makeKeyWindow(title: String, width: CGFloat, height: CGFloat, headerHeight: CGFloat, appearance: NSAppearance?) -> KeyWindow {
    let window = KeyWindow(contentRect: NSRect(x: 0, y: 0, width: width, height: height),
                           styleMask: [.titled, .closable, .miniaturizable, .fullSizeContentView], backing: .buffered, defer: false)
    window.headerHeight = headerHeight
    window.title = title
    window.titleVisibility = .hidden
    window.titlebarAppearsTransparent = true
    for type in KeyWindow.chromeButtons {
        window.standardWindowButton(type)?.isHidden = false
        window.standardWindowButton(type)?.isEnabled = type != .zoomButton
    }
    window.appearance = appearance
    window.level = .normal
    window.isOpaque = false
    window.backgroundColor = .clear
    window.hasShadow = true
    window.isMovableByWindowBackground = true
    return window
}

final class KeyWindow: NSWindow {
    static let chromeButtons: [NSWindow.ButtonType] = [.closeButton, .miniaturizeButton, .zoomButton]
    var headerHeight: CGFloat?
    var onKey: ((String) -> Void)?
    var handlesNativeShortcuts = false

    func restorePlacement(named name: NSWindow.FrameAutosaveName,
                          visibleFrames: [NSRect] = NSScreen.screens.map(\.visibleFrame),
                          fallback: NSRect? = NSScreen.main?.visibleFrame) {
        let size = frame.size
        let placement: NSRect?
        if setFrameUsingName(name, force: true) {
            // These panels have fixed layouts; restore placement, not an old layout's size.
            let saved = NSRect(x: frame.minX, y: frame.maxY - size.height, width: size.width, height: size.height)
            placement = Self.usablePlacement(saved, visibleFrames: visibleFrames, fallback: fallback)
        } else {
            placement = nil
        }
        // AppKit reapplies the saved frame when the autosave name is registered.
        setFrameAutosaveName(name)
        if let placement {
            if placement != frame { setFrame(placement, display: false) }
        } else {
            center()
        }
    }

    private static func usablePlacement(_ saved: NSRect, visibleFrames: [NSRect], fallback: NSRect?) -> NSRect {
        func coverage(_ area: NSRect) -> CGFloat {
            let overlap = area.intersection(saved)
            return overlap.isEmpty ? 0 : overlap.width * overlap.height
        }
        if visibleFrames.contains(where: { $0.contains(saved) }) { return saved }
        let best = visibleFrames.max { coverage($0) < coverage($1) }
        guard let area = best.flatMap({ coverage($0) > 0 ? $0 : nil }) ?? fallback ?? best else { return saved }
        let width = min(saved.width, area.width), height = min(saved.height, area.height)
        return NSRect(x: max(area.minX, min(saved.minX, area.maxX - width)),
                      y: max(area.minY, min(saved.minY, area.maxY - height)), width: width, height: height)
    }

    func layoutWindowButtons() {
        guard let headerHeight, let content = contentView else { return }
        for (index, type) in Self.chromeButtons.enumerated() {
            guard let button = standardWindowButton(type), let parent = button.superview else { continue }
            let center = NSPoint(x: 14 + CGFloat(index) * 20 + button.frame.width / 2,
                                 y: content.bounds.maxY - headerHeight / 2)
            let point = parent.convert(center, from: content)
            button.setFrameOrigin(NSPoint(x: point.x - button.frame.width / 2, y: point.y - button.frame.height / 2))
        }
    }

    override func becomeKey() {
        super.becomeKey()
        layoutWindowButtons()
    }
    override init(contentRect: NSRect, styleMask style: NSWindow.StyleMask, backing backingStoreType: NSWindow.BackingStoreType, defer flag: Bool) {
        super.init(contentRect: contentRect, styleMask: style, backing: backingStoreType, defer: flag)
        isReleasedWhenClosed = false
        autorecalculatesKeyViewLoop = true
    }
    override func resignKey() {
        super.resignKey()
        MenuControl.clearHoverWhere { $0.window === self }
    }
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
        if let onKey, let key = panelShortcutKey(event, includingNative: handlesNativeShortcuts) {
            onKey(key)
            return true
        }
        return super.performKeyEquivalent(with: event)
    }
}

private enum HoverCoordinator {
    static private var monitor: Any?

    static func update() {
        if MenuControl.tracksPhysicalPointer && MenuControl.activeHovered != nil {
            guard monitor == nil else { return }
            monitor = NSEvent.addLocalMonitorForEvents(matching: [.mouseMoved, .leftMouseDragged, .scrollWheel, .mouseExited, .leftMouseDown, .rightMouseDown]) { event in
                MenuControl.reconcileActiveHover()
                return event
            }
        } else if let installed = monitor {
            monitor = nil
            NSEvent.removeMonitor(installed)
        }
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
                               leadingSymbol: String? = nil, paddingX: CGFloat = 11, minWidth: CGFloat = 0) -> CGFloat {
        max(minWidth, ceil(measureTextWidth(title, font: font)) + paddingX * 2 + (leadingSymbol == nil ? 0 : 17))
    }
    func layoutContents() {
        let font = label.font ?? .systemFont(ofSize: 11)
        let cellH = ceil(label.cell?.cellSize.height ?? (font.ascender - font.descender + font.leading))
        let labelY = floor((bounds.height - cellH) / 2)
        let width = bounds.width
        let symbolSize = min(12, width)
        centerSymbolView?.frame = NSRect(x: floor((width - symbolSize) / 2), y: floor((bounds.height - symbolSize) / 2), width: symbolSize, height: symbolSize)
        if let dot = segmentIndicator {
            let size = min(6, width)
            dot.frame = NSRect(x: min(8, width - size), y: (bounds.height - size) / 2, width: size, height: size)
            label.frame = NSRect(x: min(16, width), y: labelY, width: max(0, width - 20), height: cellH)
        } else if !label.isHidden, let icon = leadingSymbolView, !icon.isHidden {
            let textW = max(0, min(width - 17, ceil(label.requiredSingleLineWidth) + 4))
            let preferredX: CGFloat = label.alignment == .left ? 6 : max(6, floor((width - (textW + 9)) / 2))
            let startX = min(preferredX, width - symbolSize)
            icon.frame = NSRect(x: startX, y: floor((bounds.height - symbolSize) / 2), width: symbolSize, height: symbolSize)
            let textX = min(width, startX + symbolSize + 1)
            label.frame = NSRect(x: textX, y: labelY, width: min(textW, width - textX), height: cellH)
        } else {
            // Leave room for the native cell's text insets, including in fixed-width controls.
            let inset = min(paddingX, width / 2, max(4, floor((width - label.requiredSingleLineWidth) / 2)))
            label.frame = NSRect(x: inset, y: labelY, width: width - 2 * inset, height: cellH)
        }
    }
    let paddingX: CGFloat
    var leadingSymbolView: NSImageView?
    var centerSymbolView: NSImageView?
    var accessorySymbolView: NSImageView? { didSet { applyVisualState() } }
    var symbolName: String?
    private var gestureToken: UInt64 = 0
    var pressed: (() -> Void)? { didSet { invalidatePress() } }
    func invalidatePress() { gestureToken &+= 1; isPressed = false }
    var style: Style = .neutral { didSet { layoutContents(); applyVisualState() } }
    var normalColor: NSColor = .labelColor { didSet { applyVisualState() } }
    var reservedTitles: [(title: String, symbol: String?)] = []
    var maximumWidth: CGFloat = .greatestFiniteMagnitude
    var reservedWidth: CGFloat {
        let font = label.font ?? .systemFont(ofSize: 11, weight: .medium)
        let restingSymbol = leadingSymbolView == nil ? nil : symbolName
        let titles = reservedTitles + [(label.stringValue, leadingSymbolView?.isHidden == false ? restingSymbol : nil)]
            + (restingTitle.map { [($0, restingSymbol)] } ?? [])
        let width = titles.map { Self.intrinsicWidth($0.0, font: font, leadingSymbol: $0.1, paddingX: paddingX) }.max() ?? 0
        return min(maximumWidth, width)
    }
    func setPending(_ title: String) {
        var pendingFrame = frame
        let minimum = max(frame.width, reservedWidth)
        clearFeedback(title: title, accessibilityLabel: title)
        isEnabled = false
        setLeadingSymbol(nil)
        toolTip = title
        pendingFrame.size.width = min(maximumWidth, max(minimum, reservedWidth))
        pendingFrame.origin.x = frame.maxX - pendingFrame.width
        frame = pendingFrame
    }
    private(set) var feedbackKey: TransientFeedbackStore.Key?
    var segmentArrow: ((Int) -> Void)?
    var segmentIndicator: SurfaceView? { didSet { layoutContents(); applyVisualState() } }
    var segmentIndicatorColor: NSColor = .labelColor
    var paintsSegmentThumb = true { didSet { if oldValue != paintsSegmentThumb { applyVisualState() } } }
    var restingTitle: String?
    var restingAccessibilityLabel: String?
    private(set) var isHovered = false
    private(set) var isPressed = false { didSet {
        if oldValue != isPressed {
            layer?.removeAnimation(forKey: "hover-backgroundColor")
            layer?.removeAnimation(forKey: "hover-borderColor")
            applyVisualState()
        }
    } }
    private var trackingArea: NSTrackingArea?
    static weak var activeHovered: MenuControl?
    private static var hoverWatchTimer: Timer?
    static var hoverWatchActive: Bool { hoverWatchTimer != nil }
    static var menuTracking = false { didSet { updateHoverWatchTimer() } }
    // Headless hooks exercise logical transitions without a visible window or physical pointer.
    static var tracksPhysicalPointer = true
    static var headlessReduceMotion: Bool?
    static var allowsMotion: Bool {
        if let headlessReduceMotion { return !headlessReduceMotion }
        return tracksPhysicalPointer && !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
    }
    override var wantsUpdateLayer: Bool { true }
    init(_ title: String, frame: NSRect, symbol: String? = nil, leadingSymbol: String? = nil, color: NSColor = .labelColor, radius: CGFloat = 6, paddingX: CGFloat = 11) {
        self.paddingX = paddingX
        label = MenuTitleField(labelWithString:title)
        symbolName = leadingSymbol ?? symbol
        super.init(frame: frame)
        wantsLayer = true; layer?.cornerRadius = radius
        layer?.cornerCurve = .continuous
        focusRingType = .default
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
    override var acceptsFirstResponder: Bool {
        enclosingMenuItem == nil && isEnabled && pressed != nil && !isHiddenOrHasHiddenAncestor
    }
    override var canBecomeKeyView: Bool { acceptsFirstResponder }
    override var focusRingMaskBounds: NSRect { bounds }
    override func drawFocusRingMask() {
        let radius = layer?.cornerRadius ?? 0
        NSBezierPath(roundedRect: bounds, xRadius: radius, yRadius: radius).fill()
    }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { enclosingMenuItem != nil }
    override func hitTest(_ point: NSPoint) -> NSView? {
        guard !isHiddenOrHasHiddenAncestor && alphaValue > 0.01 else { return nil }
        return frame.contains(point) ? self : nil
    }
    override func mouseDown(with event: NSEvent) {
        guard let window, visibleRect.contains(convert(event.locationInWindow, from: nil)) else { return }
        // Capture the release in the custom view rather than letting the menu select the item.
        trackWindowPress {
            window.nextEvent(matching: [.leftMouseDragged, .leftMouseUp], until: .distantFuture, inMode: .eventTracking, dequeue: true)
        }
    }
    func trackWindowPress(nextEvent: () -> NSEvent?) {
        guard isEnabled, pressed != nil, let trackingWindow = window else { return }
        let capturedToken = gestureToken
        // Ordinary form actions leave the field editor attached. Settlement
        // moves focus explicitly if the action removes the edited form.
        let fieldEditor = (trackingWindow.firstResponder as? NSTextView)?.isFieldEditor == true
        if enclosingMenuItem == nil && (!fieldEditor || segmentArrow != nil) {
            trackingWindow.makeFirstResponder(self)
        }
        isPressed = true
        defer { isPressed = false }
        while let event = nextEvent(), event.windowNumber == trackingWindow.windowNumber,
              window === trackingWindow, isEnabled, !isHiddenOrHasHiddenAncestor, enclosingMenuItem?.isHidden != true {
            let inside = visibleRect.contains(convert(event.locationInWindow, from: nil))
            if event.type == .leftMouseUp {
                isPressed = false
                if capturedToken == gestureToken && inside { performClick(nil) }
                return
            }
            isPressed = capturedToken == gestureToken && inside
        }
    }
    override func keyDown(with event: NSEvent) {
        if enclosingMenuItem == nil, let segmentArrow, [123, 124].contains(event.keyCode),
           event.modifierFlags.intersection([.command, .control, .option, .shift]).isEmpty {
            segmentArrow(event.keyCode == 123 ? -1 : 1)
        } else if enclosingMenuItem == nil, event.characters == " ",
           event.modifierFlags.intersection([.command, .control, .option]).isEmpty {
            if !event.isARepeat { performClick(nil) }
        } else {
            super.keyDown(with: event)
        }
    }
    override func performClick(_ sender: Any?) {
        if isEnabled { pressed?() }
    }
    override func accessibilityPerformPress() -> Bool {
        guard isEnabled, pressed != nil else { return false }
        performClick(nil)
        return true
    }
    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if trackingArea == nil {
            let area = NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self)
            trackingArea = area
            addTrackingArea(area)
        }
        if isHovered { MenuControl.reconcileActiveHover() }
    }
    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window == nil && isHovered { setHovered(false) }
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
        let changed = isHovered != hovered
        isHovered = hovered
        if changed { applyVisualState(animateHover: !isPressed && Self.allowsMotion) }
        HoverCoordinator.update()
        MenuControl.updateHoverWatchTimer()
    }
    static func clearHoverWhere(_ predicate: (MenuControl) -> Bool) {
        if let control = activeHovered, predicate(control) { control.setHovered(false) }
        updateHoverWatchTimer()
    }
    static func reconcileActiveHover() {
        guard let control = activeHovered else { HoverCoordinator.update(); updateHoverWatchTimer(); return }
        guard tracksPhysicalPointer else { return }
        guard let window = control.window, window.isVisible, !control.isHiddenOrHasHiddenAncestor,
              control.enclosingMenuItem?.isHidden != true else { control.setHovered(false); return }
        let windowPoint = window.convertPoint(fromScreen: NSEvent.mouseLocation)
        let localPoint = control.convert(windowPoint, from: nil)
        if !control.visibleRect.contains(localPoint) { control.setHovered(false) }
    }
    private static func updateHoverWatchTimer() {
        // NSMenu tracking can lose exit events. Panels use mouseExited/resignKey.
        guard menuTracking, activeHovered?.enclosingMenuItem != nil else {
            hoverWatchTimer?.invalidate()
            hoverWatchTimer = nil
            return
        }
        guard hoverWatchTimer == nil else { return }
        let timer = Timer(timeInterval: 0.08, repeats: true) { _ in reconcileActiveHover() }
        hoverWatchTimer = timer
        RunLoop.main.add(timer, forMode: .eventTracking)
    }
    override var isEnabled: Bool {
        didSet {
            if oldValue != isEnabled {
                invalidatePress()
                if !isEnabled && isHovered { setHovered(false) }
            }
            applyVisualState()
        }
    }
    func applyVisualState(animateHover: Bool = false) {
        alphaValue = isEnabled ? 1 : 0.45
        let renderedStyle = style == .prominent && !paintsSegmentThumb ? .subtleHeader : style
        let visual = ControlVisuals(style: renderedStyle, normal: normalColor, hovered: isHovered, pressed: isPressed)
        let isCopied = feedbackKey.map(TransientFeedbackStore.shared.contains) == true
        label.textColor = isCopied ? CitoPalette.successText : visual.foreground
        leadingSymbolView?.contentTintColor = visual.symbol
        centerSymbolView?.contentTintColor = visual.symbol
        accessorySymbolView?.contentTintColor = visual.accessory
        segmentIndicator?.fillColor = renderedStyle == .prominent ? .white : segmentIndicatorColor
        effectiveAppearance.performAsCurrentDrawingAppearance {
            guard let layer else { return }
            let presentation = layer.presentation() ?? layer
            let oldFill = presentation.backgroundColor
            let oldBorder = presentation.borderColor
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            // Composite copied ink against a canonical opaque surface, not stacked card tints.
            if isCopied, let fill = CitoPalette.copiedFill.usingColorSpace(.sRGB) {
                let alpha = fill.alphaComponent + (isPressed ? 0.04 : 0)
                layer.backgroundColor = (CitoPalette.surfaceBackground.blended(withFraction: alpha,
                    of: fill.withAlphaComponent(1)) ?? CitoPalette.surfaceBackground).cgColor
            } else {
                layer.backgroundColor = visual.fill.cgColor
            }
            let border = isCopied ? CitoPalette.copiedBorder : visual.border
            layer.borderColor = border?.cgColor
            layer.borderWidth = border == nil ? 0 : 1
            // Destructive hover switches the foreground and fill atomically for contrast.
            if animateHover && style != .destructiveHover {
                for (property, old, new) in [("backgroundColor", oldFill, layer.backgroundColor), ("borderColor", oldBorder, layer.borderColor)] {
                    guard let old, let new, old != new else { continue }
                    let fade = CABasicAnimation(keyPath: property)
                    fade.fromValue = old
                    fade.duration = CitoMetrics.controlHoverDuration
                    layer.add(fade, forKey: "hover-" + property)
                }
            }
            CATransaction.commit()
        }
    }
    override func updateLayer() { applyVisualState() }
    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applyVisualState()
    }
    func clearFeedback(title: String, accessibilityLabel: String?) {
        if let key = feedbackKey {
            TransientFeedbackStore.shared.observe(key, restore: nil)
            if key.action == .control { TransientFeedbackStore.shared.set(key, until: nil) }
            if symbolName != nil { leadingSymbolView?.isHidden = false }
            feedbackKey = nil
        }
        restingTitle = nil
        restingAccessibilityLabel = nil
        label.stringValue = title
        setAccessibilityLabel(accessibilityLabel)
        layoutContents()
        applyVisualState()
    }
    func copied(_ message: String = "✓ Copied", for seconds: TimeInterval = 1.2, key: TransientFeedbackStore.Key? = nil) {
        let key = key ?? feedbackKey ?? .init(.control, UUID().uuidString)
        TransientFeedbackStore.shared.set(key, until: Date().addingTimeInterval(seconds))
        showFeedback(message, key: key)
    }
    func showFeedback(_ message: String = "✓ Copied", key: TransientFeedbackStore.Key) {
        let store = TransientFeedbackStore.shared
        guard store.deadline(key) != nil else { return }
        if let previous = feedbackKey, previous != key {
            clearFeedback(title: restingTitle ?? label.stringValue, accessibilityLabel: restingAccessibilityLabel)
        }
        if feedbackKey == nil {
            restingTitle = label.stringValue
            restingAccessibilityLabel = accessibilityLabel()
        }
        feedbackKey = key
        leadingSymbolView?.isHidden = true
        label.stringValue = message
        layoutContents()
        store.observe(key) { [weak self] in
            guard let self else { return }
            self.clearFeedback(title: self.restingTitle ?? self.label.stringValue, accessibilityLabel: self.restingAccessibilityLabel)
        }
        applyVisualState()
    }
}

private struct ControlVisuals {
    let fill: NSColor
    let border: NSColor?
    let foreground: NSColor
    let symbol: NSColor
    let accessory: NSColor

    init(style: MenuControl.Style, normal: NSColor, hovered: Bool, pressed: Bool) {
        let resolved = style == .destructiveHover && (hovered || pressed) ? MenuControl.Style.destructive : style
        switch resolved {
        case .prominent, .destructive:
            let destructive = resolved == .destructive
            fill = destructive ? (pressed ? CitoPalette.destructivePressedFill : CitoPalette.destructiveFill)
                : (pressed ? CitoPalette.prominentPressedFill : CitoPalette.prominentFill)
            border = .white.withAlphaComponent(hovered ? 0.36 : 0.22)
            foreground = .white; symbol = .white; accessory = .white
        case .subtleHeader:
            fill = pressed || hovered ? .labelColor.withAppearanceAlpha(pressed ? 0.18 : 0.10) : .clear
            border = nil
            foreground = hovered ? .labelColor : normal
            symbol = hovered ? .labelColor : CitoPalette.secondaryText
            accessory = symbol
        case .neutral, .destructiveHover:
            fill = .labelColor.withAppearanceAlpha(pressed ? 0.24 : hovered ? 0.16 : 0.08)
            border = .labelColor.withAppearanceAlpha(pressed ? 0.36 : hovered ? 0.28 : 0.14)
            foreground = hovered ? .labelColor : normal
            symbol = foreground
            accessory = hovered ? .labelColor : CitoPalette.secondaryText
        }
    }
}

final class FlippedView: NSView {
    override var isFlipped: Bool { true }
}
