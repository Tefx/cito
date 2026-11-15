import AppKit

extension Companion {
    func flushDirtyViews() {
        guard headless else { return }
        renderingForInspection = true
        defer { renderingForInspection = false }
        if menuDirty && hasRenderedState { rebuild() }
        if settingsDirty, settingsPanel != nil { rebuildSettingsPanel(preservingEditor: true) }
        if directoryDirty, directoryPanel != nil { refreshDirectoryViews() }
    }

    func handleHeadlessTestCommand(_ data: Data) -> Bool {
        // Headless hooks exercise the same four-surface control handlers.
        if headless, let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let op = object["op"] as? String {
            if ["test_appearance", "test_inspect", "test_inspect_menu", "test_render_png"].contains(op),
               let name = object["appearance"] as? String, ["light", "dark"].contains(name) {
                testAppearance = NSAppearance(named: name == "light" ? .aqua : .darkAqua)
                for item in cardMenu.items { item.view?.appearance = testAppearance }
                for window in [directoryPanel, settingsPanel].compactMap({ $0 }) {
                    window.appearance = testAppearance
                    window.contentView?.appearance = testAppearance
                }
            }
            if op == "test_open_panel" {
                if object["surface"] as? String == "settings" { openSettingsPanel() }
                else { openDirectoryPanel(mode: object["mode"] as? String ?? "directory") }
                return true
            }
            if op == "test_render_state" {
                let snapshot: [String: Any] = [
                    "menu_open": menuOpen, "menu_dirty": menuDirty, "menu_rebuilds": rebuildCount,
                    "directory_open": directoryVisible, "directory_dirty": directoryDirty, "directory_reloads": directoryReloadCount,
                    "settings_open": settingsVisible, "settings_dirty": settingsDirty, "settings_rebuilds": settingsRebuildCount,
                    "status_updates": statusUpdateCount, "unread_count": unreadCount,
                    "status_image": statusImage.map { String(describing: ObjectIdentifier($0)) } ?? "",
                    "button_tooltip": (status?.button ?? headlessStatusButton)?.toolTip ?? "",
                    "listeners": listeners.count, "directory_peers": directoryAllPeers.count,
                    "settings_launch_at_login": settings?.launch_at_login ?? false,
                    "hover_watch_active": MenuControl.hoverWatchActive,
                    "hovered": MenuControl.activeHovered?.identifier?.rawValue ?? "",
                    "panel_effect_states": [settingsPanel, directoryPanel].compactMap { $0?.contentView }.flatMap { $0.subviews.compactMap { ($0 as? NSVisualEffectView)?.state.rawValue } }
                ]
                diagnostic("render-state " + String(data: try! JSONSerialization.data(withJSONObject: snapshot, options: [.sortedKeys]), encoding: .utf8)!)
                return true
            }
            if op == "test_text_metrics" {
                let strings = object["strings"] as? [String] ?? []
                let widths = [CGFloat(10.5), 11, 12, 13, 15].flatMap { size in
                    [NSFont.Weight.regular, .medium, .semibold, .bold].flatMap { weight in
                        strings.map { text -> [String: Any] in
                            let font = NSFont.systemFont(ofSize: size, weight: weight)
                            let reference = NSTextField(labelWithString: text); reference.font = font
                            return ["text": text, "size": size, "expected": ceil(reference.fittingSize.width), "actual": measureTextWidth(text, font: font)]
                        }
                    }
                }
                let dates = (object["timestamps"] as? [String] ?? []).map { timestamp -> [String: Any] in
                    let reference = ISO8601DateFormatter()
                    reference.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
                    let date = reference.date(from: timestamp) ?? {
                        reference.formatOptions = [.withInternetDateTime]
                        return reference.date(from: timestamp)
                    }()
                    return ["timestamp": timestamp, "expected": date?.timeIntervalSince1970 as Any? ?? NSNull(), "actual": activityDate(timestamp)?.timeIntervalSince1970 as Any? ?? NSNull(), "relative": relativeTime(timestamp)]
                }
                diagnostic("text-metrics " + String(data: try! JSONSerialization.data(withJSONObject: ["widths": widths, "dates": dates], options: [.sortedKeys]), encoding: .utf8)!)
                return true
            }
            if op == "test_hover_exit" {
                if let control = MenuControl.activeHovered,
                   let event = NSEvent.enterExitEvent(with: .mouseExited, location: .zero, modifierFlags: [], timestamp: 0,
                       windowNumber: control.window?.windowNumber ?? 0, context: nil, eventNumber: 0, trackingNumber: 0, userData: nil) {
                    control.mouseExited(with: event)
                }
                return true
            }
            if op == "test_panel_resign_key" {
                (object["surface"] as? String == "directory" ? directoryPanel : settingsPanel)?.resignKey()
                return true
            }
            if op.hasPrefix("test_"), !["test_menu_open", "test_menu_close"].contains(op) { flushDirtyViews() }
            if op == "test_inspect" || op == "test_inspect_menu" { inspectMenu(); return true }
            if op == "test_oracle_calibration", let kind = object["kind"] as? String {
                if kind == "reset" { lastStatusPresentation = nil; rebuild(); if settingsPanel != nil { rebuildSettingsPanel() }; updateDirectoryFacetStyles(); flushDirtyViews(); return true }
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
                    if kind == "status_emoji", let button = status?.button ?? headlessStatusButton { button.attributedTitle = NSAttributedString(string: "🚀3") }
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
                return true
            }
            if op == "test_render_png", let surface = object["surface"] as? String, let path = object["path"] as? String {
                renderPNG(surface: surface, path: path); return true
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
                return true
            }
            if op == "test_tile_scroll_views" {
                func tile(_ view: NSView) {
                    if let scroll = view as? OverlayScrollView { scroll.scrollerStyle = .legacy; scroll.tile() }
                    for child in view.subviews { tile(child) }
                }
                for root in cardMenu.items.compactMap({ $0.view }) + [settingsPanel?.contentView, directoryPanel?.contentView].compactMap({ $0 }) { tile(root) }
                return true
            }
            if op == "test_menu_open" { menuWillOpen(cardMenu); return true }
            if op == "test_menu_close" { menuDidClose(cardMenu); return true }
            if op == "test_appearance" { return true }
            if op == "test_hover", let id = object["id"] as? String, let hovered = object["hovered"] as? Bool {
                for item in cardMenu.items {
                    if let view = item.view, let control = findControl(in: view, id: id) {
                        control.setHovered(hovered)
                        return true
                    }
                }
                if let rootView = directoryPanel?.contentView, let control = findControl(in: rootView, id: id) {
                    control.setHovered(hovered)
                    return true
                }
                if let rootView = settingsPanel?.contentView, let control = findControl(in: rootView, id: id) {
                    control.setHovered(hovered)
                }
                return true
            }
            if op == "test_directory_query", let query = object["query"] as? String {
                if directoryPanel == nil { openDirectoryPanel() }
                if let field = directorySearchField {
                    field.stringValue = query
                    controlTextDidChange(Notification(name:NSControl.textDidChangeNotification,object:field))
                }
                return true
            }
            if ["test_focus_field", "test_settings_input"].contains(op), let id = object["id"] as? String, id.hasPrefix("channel_field:") {
                let parts = id.split(separator: ":")
                if parts.count > 1 { channelUiStates["\(effectiveChannelHub)|\(parts[1])", default: ChannelUiState()].editingPersonal = true; rebuildSettingsPanel() }
            }
            if op == "test_editor_marked", let value = object["value"] as? String {
                let window = object["surface"] as? String == "menu" ? headlessMenuPanel : settingsPanel
                if let editor = window?.firstResponder as? NSTextView {
                    editor.setMarkedText(value, selectedRange:NSRange(location:value.utf16.count,length:0), replacementRange:editor.selectedRange())
                }
                return true
            }
            if op == "test_end_editing" {
                for window in [settingsPanel, headlessMenuPanel].compactMap({ $0 }) { window.makeFirstResponder(nil) }
                return true
            }
            if op == "test_reconfigure_rows", let table = directoryTableView {
                for row in 0..<table.numberOfRows {
                    guard let cell = table.view(atColumn:0,row:row,makeIfNecessary:false) as? DirectoryPeerCellView else { continue }
                    if directoryMode == "activity", activityFilteredItems.indices.contains(row) {
                        let item = activityFilteredItems[row]
                        cell.configureActivity(with:item,selected:row==directorySelectedIndex,harnesses:harnessMap,onCopy:{ [weak self] c in self?.copyActivity(item,control:c) },onDelete:{ [weak self] in self?.deleteActivity(item) },onReply:{ [weak self] in self?.replyToActivity(item) })
                    } else if directoryFilteredPeers.indices.contains(row) {
                        let peer = directoryFilteredPeers[row]
                        cell.configure(with:peer,selected:row==directorySelectedIndex,harnesses:harnessMap,onCopy:{ [weak self] text,c in self?.copyText(text,control:c) },onMessage:{ [weak self] in self?.openComposer(target:peer.display_address,hub:peer.hub ?? "") })
                    }
                }
                return true
            }
            if op == "test_focus_field", let id = object["id"] as? String {
                if id.hasPrefix("rename_field:"), let field = renameField, let root = cardMenu.items.compactMap({ $0.view }).first(where: { findTextField(in:$0,id:id) != nil }) {
                    let panel = headlessMenuPanel ?? KeyPanel(contentRect:NSRect(x:0,y:0,width:380,height:600),styleMask:[.titled],backing:.buffered,defer:false)
                    headlessMenuPanel = panel
                    panel.contentView?.addSubview(root)
                    panel.makeFirstResponder(field); field.selectText(nil)
                    return true
                }
                if id == "directory_table" { directoryPanel?.makeFirstResponder(directoryTableView); return true }
                for window in [settingsPanel, directoryPanel].compactMap({ $0 }) {
                    if let root = window.contentView, let field = findTextField(in: root, id: id), field.isEditable, field.isEnabled {
                        window.makeFirstResponder(field)
                        field.selectText(nil)
                        return true
                    }
                }
                diagnostic("editable field not found: \(id)"); return true
            }
            if op == "test_settings_scroll", let offset = object["offset"] as? Double {
                for case let scroll as NSScrollView in settingsPanel?.contentView?.subviews ?? [] where !scroll.isHidden {
                    let top=max(0,(scroll.documentView?.frame.height ?? 0)-scroll.contentSize.height)
                    scroll.contentView.scroll(to:NSPoint(x:0,y:max(0,min(top,CGFloat(offset)))))
                    scroll.reflectScrolledClipView(scroll.contentView)
                }
                return true
            }
            if op == "test_settings_input", let id = object["id"] as? String, let value = object["value"] as? String {
                if id == "composer_input" {
                    composerDraftIdempotencyKey = UUID().uuidString
                    composerInputField?.stringValue = value
                    return true
                }
                if let field = settingsFieldControls[id] ?? (settingsPanel?.contentView.flatMap { findTextField(in: $0, id: id) }) ?? cardMenu.items.compactMap({ $0.view }).compactMap({ findTextField(in: $0, id: id) }).first {
                    field.stringValue = value
                    controlTextDidChange(Notification(name: NSControl.textDidChangeNotification, object: field))
                }
                syncAddHubFieldsFromControls()
                return true
            }
            if op == "test_composer_input", let value = object["value"] as? String {
                composerDraftIdempotencyKey = UUID().uuidString
                composerInputField?.stringValue = value
                return true
            }
            if op == "test_clipboard", let text = object["text"] as? String {
                if object["system_only"] as? Bool != true { clipboardString = text }
                appPasteboard.clearContents()
                appPasteboard.setString(text, forType: .string)
                return true
            }
            if op == "test_menu_key", let key = object["key"] as? String, let field = renameField {
                let editor = (field.currentEditor() as? NSTextView) ?? NSTextView()
                if field.currentEditor() == nil { editor.string = field.stringValue }
                _ = control(field, textView: editor, doCommandBy: key == "escape" ? #selector(NSResponder.cancelOperation(_:)) : #selector(NSResponder.insertNewline(_:)))
                return true
            }
            if ["test_settings_key", "test_directory_key"].contains(op), let key = object["key"] as? String {
                let window = op == "test_settings_key" ? settingsPanel : directoryPanel
                if object["via"] as? String == "panel" {
                    let character = key.hasSuffix("comma") ? "," : String(key.suffix(1))
                    let flags: NSEvent.ModifierFlags = key.hasPrefix("shift-") ? [.command, .shift] : [.command]
                    if let event = NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: flags, timestamp: 0, windowNumber: window?.windowNumber ?? 0, context: nil, characters: character, charactersIgnoringModifiers: character, isARepeat: false, keyCode: 0) {
                        _ = window?.performKeyEquivalent(with: event)
                    }
                    return true
                }
                if op == "test_settings_key" { handleSettingsKey(key) } else { handleDirectoryKey(key) }
                return true
            }
            if op == "test_expire_copy_feedback", let id = object["id"] as? String {
                let roots = cardMenu.items.compactMap { $0.view } + [settingsPanel?.contentView, directoryPanel?.contentView].compactMap { $0 }
                for root in roots {
                    if let control = findControl(in: root, id: id), let timer = control.feedbackTimer {
                        timer.fire(); timer.invalidate()
                        return true
                    }
                }
                return true
            }
            if op == "test_control", let requestedId = object["id"] as? String {
                let id = requestedId.hasPrefix("target_hub:") ? "hub:" + requestedId.dropFirst("target_hub:".count) : requestedId
                for item in cardMenu.items {
                    if let view = item.view, let control = findControl(in: view, id: id) {
                        _ = control.accessibilityPerformPress()
                        return true
                    }
                }
                if let rootView = directoryPanel?.contentView, let control = findControl(in: rootView, id: id) {
                    _ = control.accessibilityPerformPress()
                    return true
                }
                if let rootView = settingsPanel?.contentView, let control = findControl(in: rootView, id: id) {
                    _ = control.accessibilityPerformPress()
                    return true
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
                return true
            }
        }
        return false
    }

    // Headless inspection projects real AppKit objects; it does not synthesize a parallel menu model.
    func renderPNG(surface: String, path: String) {
        flushDirtyViews()
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
        flushDirtyViews()
        var truncatedLabels: [String] = []
        var overlappingSiblings: [String] = []
        var clippedViews: [String] = []
        var lowContrastLabels: [String] = []
        var jargonLeaks: [String] = []
        var addressMismatches: [String] = []
        let jargonPattern = "(?i:\\bhandle\\b|human nodes?|every human|each human|\\breceive wakes\\b|\\bwake banners?\\b|\\bcito_listen\\b|\\bhub service:|\\bchannel inventory\\b|\\bui action exceeds\\b)" + (hubs.count <= 1 ? "|on 'local'" : "") + "|local://|Handle:|Human Node|Human listening|Human receiver|All Harnesses|\\bSession\\b|· (none|token|oidc|auth_required|disconnected)\\b|\\bPassive\\b|\\[Passive\\]|\\b(?:[A-Z][A-Z0-9_]{2,}|[0-9]{3,}):(?:\\s+|$)|\\b(?:ACTIVE|UNLISTENED|DISCONNECTED_RETAINED|EXPIRED)\\b|\"error_code\"|(?i:SQLITE_|sqlite3|\\(SQLite\\b|unable to open database|database is locked|database disk image is malformed|no such table:)"
        func name(_ view: NSView) -> String {
            view.identifier?.rawValue ?? (view as? NSTextField)?.stringValue ?? String(describing:type(of:view))
        }
        func isDataRow(_ view: NSView) -> Bool {
            var ancestor: NSView? = view
            while let current = ancestor {
                let id = current.identifier?.rawValue ?? ""
                if id == "host_hub_endpoint" || id.hasPrefix("dir_") || id.hasPrefix("act_") || id.hasPrefix("session:") || id.hasPrefix("candidate:") || id.hasPrefix("remote:") || id.hasPrefix("channel_summary:") || current is DirectoryPeerCellView { return true }
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
                    // NSCell can retain a narrower cached width after a color/appearance update.
                    let requiredWidth = max(label.cell?.cellSize.width ?? 0,label.attributedStringValue.size().width)
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
                let dataJargon = id.hasPrefix("act_title:") && generatedText.range(of:jargonPattern,options:.regularExpression) != nil
                if dataJargon || !data && label.stringValue.range(of:jargonPattern,options:.regularExpression) != nil { jargonLeaks.append(id) }
            }
            if visible, let control = view as? MenuControl {
                let id = control.identifier?.rawValue ?? ""
                let scope = ["copy:","copy_remote:","copy_dir:"].first { id.hasPrefix($0) }
                if let prefix = scope {
                    let scoped = String(id.dropFirst(prefix.count))
                    let address = listeners.first { $0.scoped_id == scoped }?.display_address ?? (myRemote + directoryAllPeers).first { $0.scoped_id == scoped }?.display_address
                    let feedback = control.feedbackTimer?.isValid == true
                    let label = feedback ? (control.restingTitle ?? "") : control.label.stringValue
                    if feedback && control.accessibilityLabel() != control.label.stringValue { addressMismatches.append(id + ": stale accessibility label") }
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
                case "settings_channels_tab": defaultLayout = settings?.channel_inventory_error == nil && settings?.channels?.count == 1 && !channelUiStates.values.contains(where: { $0.editingGateway || $0.editingPersonal || $0.manualEntry }) && (settings?.channels ?? []).allSatisfy { (channelUiStates["\(effectiveChannelHub)|\($0.id)"]?.pairingInstruction ?? "").isEmpty && $0.status != "error" && $0.warning != true && channelUiStates["\(effectiveChannelHub)|\($0.id)"]?.testResult == nil } && (settings?.channels?.first?.fields.count ?? 0) <= 2
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
                       && !["settings_panel","directory_panel"].contains(id),
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
            var result: [String: Any] = ["instance": String(describing:ObjectIdentifier(view)), "id": (view as? DirectoryPeerCellView)?.inspectionId ?? view.identifier?.rawValue ?? "", "width": view.frame.width,
                "height": view.frame.height, "x": view.frame.origin.x, "y": view.frame.origin.y,
                "radius": view.layer?.cornerRadius ?? 0, "hidden": view.isHidden,
                "children": childViews.map { project($0, visible: visible, background: surface, opacity: opacity, enabled: enabled) }]
            if let effect=view as? NSVisualEffectView { result["material"]=effect.material.rawValue }
            result["contrast"] = contrast
            result["background"] = components(fill)
            result["border"] = components(view.layer?.borderColor.flatMap { NSColor(cgColor: $0) })
            result["border_width"] = view.layer?.borderWidth ?? 0
            if let tooltip=view.toolTip { result["tooltip"]=tooltip }
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
            "rename_field_identity": renameField.map { String(describing:ObjectIdentifier($0)) } ?? "",
            "low_contrast_labels": lowContrastLabels, "jargon_leaks":jargonLeaks, "address_mismatches":addressMismatches,
            "edit_menu": NSApp.mainMenu?.items.first(where: { $0.submenu?.title == "Edit" })?.submenu?.items.map { ["title": $0.title, "key": $0.keyEquivalent, "action": $0.action.map(NSStringFromSelector) ?? "", "shift": $0.keyEquivalentModifierMask.contains(.shift)] as [String: Any] } ?? [],
            "key_panels": [settingsPanel, directoryPanel].compactMap { window -> [String: Any]? in
                guard let window = window else { return nil }
                return ["titled": window.styleMask.contains(.titled), "full_size_content": window.styleMask.contains(.fullSizeContentView),
                    "hidden_title": window.titleVisibility == .hidden, "transparent_titlebar": window.titlebarAppearsTransparent,
                    "hidden_window_buttons": [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton].allSatisfy { window.standardWindowButton($0)?.isHidden != false }]
            },
            "field_editors": [settingsPanel, directoryPanel, headlessMenuPanel].compactMap { window -> [String: Any]? in
                guard let editor = window?.firstResponder as? NSTextView else { return nil }
                return ["id": (editor.delegate as? NSTextField)?.identifier?.rawValue ?? "", "instance": String(describing:ObjectIdentifier(editor)),
                    "marked": editor.hasMarkedText(), "selection_length": editor.selectedRange().length,
                    "selection_location": editor.selectedRange().location, "editable": editor.isEditable,
                    "can_undo": editor.undoManager?.canUndo ?? false, "undo_groups": editor.undoManager?.groupingLevel ?? -1,
                    "automatic_groups": editor.undoManager?.groupsByEvent ?? false,
                    "attached": (editor.delegate as? NSTextField)?.currentEditor() === editor]
            },
            "activity_copy_remaining": activityCopiedUntil.mapValues { max(0,$0.timeIntervalSinceNow) },
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
            "settings_rebuild_deferred": settingsRebuildDeferred,
            "settings_launch_at_login": settings?.launch_at_login ?? false, "settings_items": settingsItems,
            "icon_template": statusImage?.isTemplate ?? false, "icon_width": statusImage?.size.width ?? 0,
            "icon_height": statusImage?.size.height ?? 0, "clipboard": effectiveClipboard,
            "button_tooltip": button?.toolTip ?? "", "button_accessibility_label": button?.accessibilityLabel() ?? "",
            "button_title": button?.title ?? "", "button_image_position": button?.imagePosition.rawValue ?? -1,
            "button_font_size": font?.pointSize ?? 0, "button_font_weight": traits?[.weight] as? CGFloat ?? 0,
            "button_font_features": (font?.fontDescriptor.object(forKey: .featureSettings) as? [[NSFontDescriptor.FeatureKey: Any]] ?? []).map { feature in
                ["type": feature[.typeIdentifier] as? Int ?? -1, "selector": feature[.selectorIdentifier] as? Int ?? -1]
            },
            "button_digit_widths": font.map { font in ["11111111", "88888888"].map { ($0 as NSString).size(withAttributes: [.font: font]).width } } ?? []]
        diagnostic("menu " + String(data: try! JSONSerialization.data(withJSONObject: snapshot, options: [.sortedKeys]), encoding: .utf8)!)
    }
}
