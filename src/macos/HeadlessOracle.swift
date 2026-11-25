import AppKit

// Record real tracking cancellation without opening a native menu in tests.
final class HeadlessMenu: NSMenu {
    var trackingCancellations = 0
    override func cancelTracking() {
        trackingCancellations += 1
        super.cancelTracking()
    }
}

extension Companion {
    func flushDirtyViews() {
        guard headless else { return }
        renderingForInspection = true
        defer { renderingForInspection = false }
        if menuDirty && hasRenderedState { rebuild(preservingEditor: true) }
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
            if op == "test_key_event", let character = object["character"] as? String {
                let modifiers = object["modifiers"] as? [String] ?? []
                var flags: NSEvent.ModifierFlags = []
                for (name, flag) in [("command", NSEvent.ModifierFlags.command), ("shift", .shift), ("option", .option), ("control", .control), ("capsLock", .capsLock)] where modifiers.contains(name) { flags.insert(flag) }
                if let event = NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: flags, timestamp: 0, windowNumber: 0, context: nil, characters: character, charactersIgnoringModifiers: character, isARepeat: false, keyCode: (object["key_code"] as? UInt16) ?? UInt16.max) {
                    NSApp.sendEvent(event)
                }
                return true
            }
            if op == "test_tab" {
                let window = object["surface"] as? String == "directory" ? directoryPanel : settingsPanel
                if let editor = window?.firstResponder as? NSTextView {
                    if object["backward"] as? Bool == true { editor.insertBacktab(nil) }
                    else { editor.insertTab(nil) }
                }
                return true
            }
            if op == "test_open_panel" {
                if object["surface"] as? String == "settings" { openSettingsPanel() }
                else { openDirectoryPanel(mode: object["mode"] as? String ?? "directory") }
                return true
            }
            if op == "test_render_state" {
                func containsRename(_ view: NSView) -> Bool {
                    view.identifier?.rawValue.hasPrefix("rename_field:") == true || view.subviews.contains(where: containsRename)
                }
                let snapshot: [String: Any] = [
                    "menu_open": menuOpen, "menu_dirty": menuDirty, "menu_rebuilds": rebuildCount,
                    "mounted_rename": cardMenu.items.compactMap { $0.view }.contains(where: containsRename),
                    "directory_open": directoryVisible, "directory_dirty": directoryDirty, "directory_reloads": directoryReloadCount,
                    "settings_open": settingsVisible, "settings_dirty": settingsDirty, "settings_rebuilds": settingsRebuildCount,
                    "status_updates": statusUpdateCount, "unread_count": unreadCount,
                    "status_image": statusImage.map { String(describing: ObjectIdentifier($0)) } ?? "",
                    "button_tooltip": (status?.button ?? headlessStatusButton)?.toolTip ?? "",
                    "listeners": listeners.count, "directory_peers": directoryAllPeers.count,
                    "collection_counts": [
                        "activity": activityAllItems.count, "available": available.count, "discovery": discovery.count,
                        "my_remote_listeners": myRemote.count, "hubs": hubs.count,
                        "settings.apps": settings?.apps.count ?? 0, "settings.hubs": settings?.hubs.count ?? 0,
                        "settings.channels": settings?.channels?.count ?? 0
                    ],
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
                let roots = cardMenu.items.compactMap { $0.view } + [directoryPanel?.contentView, settingsPanel?.contentView].compactMap { $0 }
                roots.lazy.compactMap { self.findView(in: $0, id: id, as: MenuControl.self) }.first?.setHovered(hovered)
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
            if op == "test_editor_marked", let value = object["value"] as? String {
                let window: KeyPanel?
                switch object["surface"] as? String {
                case "menu": window = headlessMenuPanel
                case "directory": window = directoryPanel
                default: window = settingsPanel
                }
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
                        cell.configure(with:peer,selected:row==directorySelectedIndex,harnesses:harnessMap,onCopy:{ [weak self] _,c in self?.copyPeer(peer,control:c) },onMessage:{ [weak self] in self?.openComposer(target:peer.display_address,hub:peer.hub ?? "") })
                    }
                }
                return true
            }
            if (op == "test_focus_field" || (op == "test_settings_input" && object["value"] is String)),
               let id = object["id"] as? String,
               ["settings_hub_issuer", "settings_hub_client_id", "settings_hub_redirect_port"].contains(id),
               settingsActiveTab == "hubs", hubForm.isOpen, hubForm.auth == "oidc", !editingBuiltinHub,
               !uninstallConfirmOpen, !hubForm.oidcManualExpanded {
                hubForm.oidcManualExpanded = true
                rebuildSettingsPanel(syncFields: true)
            }
            if op == "test_focus_field", let id = object["id"] as? String {
                if id.hasPrefix("rename_field:"), let field = renameField, let root = cardMenu.items.compactMap({ $0.view }).first(where: { findView(in:$0,id:id,as:NSTextField.self) != nil }) {
                    let panel = headlessMenuPanel ?? KeyPanel(contentRect:NSRect(x:0,y:0,width:380,height:600),styleMask:[.titled],backing:.buffered,defer:false)
                    headlessMenuPanel = panel
                    panel.contentView?.addSubview(root)
                    panel.makeFirstResponder(field); field.selectText(nil)
                    return true
                }
                if id == "directory_table" { directoryPanel?.makeFirstResponder(directoryTableView); return true }
                for window in [settingsPanel, directoryPanel].compactMap({ $0 }) {
                    if let root = window.contentView, let field: NSTextField = findView(in: root, id: id), field.isSelectable, field.isEnabled {
                        // Hidden panels never receive AppKit's order-front key-loop pass.
                        window.recalculateKeyViewLoop()
                        window.makeFirstResponder(field)
                        field.selectText(nil)
                        if object["caret_only"] as? Bool == true, let editor = field.currentEditor() as? NSTextView {
                            editor.setSelectedRange(NSRange(location: 0, length: 0))
                        }
                        return true
                    }
                }
                diagnostic("selectable field not found: \(id)"); return true
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
                let composerField = id == "composer_input" ? composerInputField : nil
                if let field = composerField ?? settingsFieldControls[id] ?? (settingsPanel?.contentView.flatMap { findView(in: $0, id: id, as: NSTextField.self) }) ?? cardMenu.items.compactMap({ $0.view }).compactMap({ findView(in: $0, id: id, as: NSTextField.self) }).first {
                    field.stringValue = value
                    field.delegate?.controlTextDidChange?(Notification(name: NSControl.textDidChangeNotification, object: field))
                }
                return true
            }
            if op == "test_composer_input", let value = object["value"] as? String {
                if let field = composerInputField {
                    field.stringValue = value
                    controlTextDidChange(Notification(name: NSControl.textDidChangeNotification, object: field))
                }
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
                if object["via"] as? String == "field" {
                    let commands = ["up": #selector(NSResponder.moveUp(_:)), "down": #selector(NSResponder.moveDown(_:)),
                                    "return": #selector(NSResponder.insertNewline(_:)), "escape": #selector(NSResponder.cancelOperation(_:))]
                    let handled: Bool
                    if let editor = window?.firstResponder as? NSTextView, let field = editor.delegate as? NSTextField,
                       field.currentEditor() === editor, let selector = commands[key] {
                        handled = field.delegate?.control?(field, textView: editor, doCommandBy: selector) ?? false
                    } else {
                        handled = false
                    }
                    diagnostic("field-key {\"handled\":\(handled)}")
                    return true
                }
                if object["via"] as? String == "panel" {
                    let character = object["characters"] as? String ?? (key.hasSuffix("comma") ? "," : String(key.suffix(1)))
                    let flags: NSEvent.ModifierFlags = key.hasPrefix("shift-") ? [.command, .shift] : [.command]
                    let keyCode = (object["key_code"] as? UInt16) ?? UInt16.max
                    if let event = NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: flags, timestamp: 0, windowNumber: window?.windowNumber ?? 0, context: nil, characters: character, charactersIgnoringModifiers: character, isARepeat: false, keyCode: keyCode) {
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
                    if let control: MenuControl = findView(in: root, id: id), let timer = control.feedbackTimer {
                        timer.fire(); timer.invalidate()
                        return true
                    }
                }
                return true
            }
            if op == "test_control", let requestedId = object["id"] as? String {
                if requestedId == "close_settings" {
                    if settingsVisible { settingsPanel?.performClose(nil) }
                    return true
                }
                if requestedId == "close_directory" {
                    if directoryVisible { directoryPanel?.performClose(nil) }
                    return true
                }
                let id = requestedId.hasPrefix("target_hub:") ? "hub:" + requestedId.dropFirst("target_hub:".count) : requestedId
                let roots = cardMenu.items.compactMap { $0.view } + [directoryPanel?.contentView, settingsPanel?.contentView].compactMap { $0 }
                if let control = roots.lazy.compactMap({ self.findView(in: $0, id: id, as: MenuControl.self) }).first {
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

    @discardableResult
    func inspectMenu() -> [String: Any] {
        flushDirtyViews()
        var truncatedLabels: [String] = []
        var overlappingSiblings: [String] = []
        var clippedViews: [String] = []
        var tightPadding: [String] = []
        var nonAnchoredActionBars: [String] = []
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
                if id.hasPrefix("act_held:") || id.hasPrefix("candidate_hint:") { return false }
                if id == "host_hub_endpoint" || ["dir_row:", "msg_dir:", "copy_dir:", "act_", "session:", "candidate:", "remote:"].contains(where: { id.hasPrefix($0) }) || current is DirectoryPeerCellView { return true }
                ancestor = current.superview
            }
            return false
        }
        func intentionallyTruncatedLabel(_ label: NSTextField) -> Bool {
            guard !(label.toolTip ?? label.superview?.toolTip ?? "").isEmpty,
                  label.lineBreakMode == .byTruncatingMiddle || label.lineBreakMode == .byTruncatingTail else { return false }
            if isDataRow(label) || name(label) == "hub_oidc_discovery_summary" { return !(label.toolTip ?? "").isEmpty }
            let parentID = label.superview?.identifier?.rawValue
            let title = label.stringValue
            return (hubs.count == 2 && parentID == "hub:\(title)") ||
                ((settings?.hubs.count ?? hubs.count) == 2 && parentID == "settings_hub:\(title)" &&
                 label.identifier?.rawValue == "settings_hub_name:\(title)")
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
            let fill: NSColor?
            if let scroll = view as? NSScrollView {
                fill = scroll.drawsBackground ? resolved(scroll.backgroundColor, in: view.effectiveAppearance) : nil
            } else {
                fill = view.layer?.backgroundColor.flatMap { NSColor(cgColor: $0) }
            }
            let textBackground = (view as? NSTextField).flatMap { $0.drawsBackground ? $0.backgroundColor : nil }
            if let fill = fill {
                surface = blended(resolved(fill, in: view.effectiveAppearance), over: base, opacity: opacity)
            }
            if let fill = textBackground {
                surface = blended(resolved(fill, in: view.effectiveAppearance), over: surface, opacity: opacity)
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
            if visible, enabled, opacity > 0.6, let image = view as? NSImageView, image.image != nil, let tint = image.contentTintColor {
                let foreground = blended(resolved(tint, in: view.effectiveAppearance), over: surface, opacity: opacity)
                let a = luminance(foreground), b = luminance(surface)
                let ratio = (max(a,b)+0.05)/(min(a,b)+0.05)
                contrast = ratio
                if ratio < 3 { lowContrastLabels.append("\(name(image)): \(String(format: "%.2f", Double(ratio)))") }
            }
            if visible, let label = view as? NSTextField, label.isEditable {
                if label.frame.width < 48 || measureTextWidth(label.placeholderString ?? "", font:label.font ?? .systemFont(ofSize:12)) > label.frame.width - 8 { truncatedLabels.append(name(label)) }
            }
            if visible, let label = view as? NSTextField, !label.isEditable {
                let font = label.font ?? .systemFont(ofSize:12)
                let hasTooltip = !(label.toolTip ?? "").isEmpty
                let truncated: Bool
                if label.maximumNumberOfLines == 1 || label.cell?.wraps == false {
                    // Native cell insets and attributed glyph metrics determine
                    // what AppKit can actually draw, not the outer field width.
                    let drawingWidth = label.cell?.drawingRect(forBounds:label.bounds).width ?? label.bounds.width
                    truncated = label.requiredSingleLineWidth > drawingWidth && !intentionallyTruncatedLabel(label)
                } else if name(label).hasPrefix("act_msg:") && hasTooltip {
                    // Expanded Activity messages deliberately cap the preview height.
                    truncated = false
                } else {
                    let drawable = label.cell?.drawingRect(forBounds: label.bounds) ?? label.bounds
                    let height = measureWrappedHeight(label.stringValue, width: drawable.width, font: font)
                    truncated = height > drawable.height
                }
                if truncated { truncatedLabels.append((label.superview as? MenuControl).map { name($0) } ?? name(label)) }
            }
            func hasJargon(_ text: String) -> Bool {
                text.range(of:jargonPattern,options:.regularExpression) != nil
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
                if id == "hub_oidc_discovery_summary", generatedText.hasPrefix("OIDC: ") {
                    // The issuer is server metadata, not UI copy.
                    generatedText = "OIDC · Auto-discovered"
                }
                if hasJargon(data && !id.hasPrefix("act_title:") ? "" : generatedText) { jargonLeaks.append(id) }
            }
            if visible {
                for (source, value) in [("tooltip",view.toolTip),("accessibility label",view.accessibilityLabel())] {
                    guard let value else { continue }
                    // A data label's repeated payload follows the same provenance as its visible text.
                    if let label = view as? NSTextField, !label.isEditable {
                        if value == label.stringValue { continue }
                        if name(label).hasPrefix("act_msg:"),
                           value == (label.superview as? DirectoryPeerCellView)?.activityItem?.message { continue }
                    }
                    if hasJargon(value) { jargonLeaks.append("\(name(view)): \(source)") }
                }
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
                case "settings_channels_tab":
                    let mounted = scroll.documentView as? ChannelsSettingsView
                    let hub = mounted?.hub ?? effectiveChannelHub
                    let channels = mounted?.channels ?? []
                    defaultLayout = channelInventoryErrorsByHub[hub] == nil && channels.count == 1 && channels.allSatisfy { channel in
                        let ui = channelUiStates["\(hub)|\(channel.id)"] ?? ChannelUiState()
                        return !ui.editingGateway && !ui.editingPersonal && !ui.manualEntry && ui.pairingInstruction.isEmpty &&
                            channel.status != "error" && channel.warning != true && ui.testResult == nil && channel.fields.count <= 2
                    }
                case "settings_hubs_tab": defaultLayout = !hubForm.isOpen && (settings?.hubs.count ?? 0) <= 2 && (settings?.hubs ?? []).allSatisfy { $0.status == "connected" && $0.error == nil }
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
                let edgeItems = children.filter { child in child.frame.height > 5 && (child is NSControl || ["settings_app_tier:","stratum_hub_status:","settings_channel_status:","settings_channel_badge:"].contains { child.identifier?.rawValue.hasPrefix($0) == true }) }
                let id = name(view)
                if ["header_hub_strip", "targetStrip"].contains(id), let scroll = view as? NSScrollView, !children.isEmpty {
                    let trailingInset: CGFloat = id == "header_hub_strip" ? 2 : 0
                    if (children.map { $0.frame.maxX }.max() ?? 0) < scroll.contentSize.width - trailingInset - 2 { clippedViews.append(id + ": trailing pills are not edge anchored") }
                }
                let stratum = ["stratum_hub:","stratum_user:"].contains(where: { id.hasPrefix($0) })
                if id.hasPrefix("channel_recovery_bar:") {
                    let recovery = edgeItems.filter { name($0).hasPrefix("recover_channel:") }
                    if let right = recovery.map({ view.convert($0.frame,to:view.superview).maxX }).max(),
                       let card = view.superview, abs(right - (card.bounds.maxX - 12)) > 1.5 {
                        let issue = name(card) + ": recovery row is not edge anchored"
                        nonAnchoredActionBars.append(issue); clippedViews.append(issue)
                    }
                    if recovery.contains(where: { abs($0.frame.midY-parentBounds.midY) > 1.5 }) {
                        let issue = (view.superview.map(name) ?? id) + ": recovery row controls are not vertically centered"
                        nonAnchoredActionBars.append(issue); clippedViews.append(issue)
                    }
                }
                func checkActionRow(_ row: String, items: [NSView], midY: CGFloat, rightInset: CGFloat) {
                    guard !items.isEmpty else { return }
                    if let right = items.map({ $0.frame.maxX }).max(), abs(right - (parentBounds.maxX - rightInset)) > 1.5 {
                        let issue = id + ": " + row + " row is not edge anchored"
                        nonAnchoredActionBars.append(issue); clippedViews.append(issue)
                    }
                    if items.contains(where: { abs($0.frame.midY - midY) > 1.5 }) {
                        let issue = id + ": " + row + " row controls are not vertically centered"
                        nonAnchoredActionBars.append(issue); clippedViews.append(issue)
                    }
                }
                func actionItems(_ ids: Set<String>) -> [NSView] { children.filter { ids.contains(name($0)) } }
                switch id {
                case "host_hub_card":
                    let port = actionItems(["host_hub_port", "apply_host_hub_port", "copy_host_hub_token", "toggle_host_hub_auth"])
                    checkActionRow("sharing", items: actionItems(["host_hub_title", "host_hub_status", "toggle_host_hub"]),
                                   midY: port.isEmpty ? 23 : parentBounds.height - 22, rightInset: 14)
                    checkActionRow("invite", items: actionItems(["host_hub_endpoint", "copy_hub_invite"]), midY: 60, rightInset: 14)
                    checkActionRow("port", items: port, midY: 24, rightInset: 14)
                case "add_hub_form":
                    checkActionRow("submit", items: actionItems(["cancel_add_hub", "submit_add_hub"]), midY: 22, rightInset: 14)
                case "uninstall_confirm_box":
                    checkActionRow("confirm", items: actionItems(["cancel_uninstall_all", "confirm_disconnect_all"]), midY: 26, rightInset: 12)
                default: break
                }
                let pairingActions = id.hasPrefix("stratum_user:") ? edgeItems.filter { item in
                    ["copy_pair_command:", "open_pair_url:", "cancel_pair_channel:"].contains { name(item).hasPrefix($0) }
                } : []
                checkActionRow("pairing", items: pairingActions, midY: 51, rightInset: CitoMetrics.cardInset)
                if ["inbox_control_bar","footer","settings_footer","directory_footer"].contains(id) || stratum || ["settings_channel:","settings_hub:","settings_app:"].contains(where: { id.hasPrefix($0) }) || id == "directory_facet_bar" && directoryMode == "activity" {
                    let barItems = edgeItems.filter { item in
                        if id == "inbox_control_bar" { return item is MenuControl }
                        return !((item as? NSTextField)?.isEditable ?? false) && name(item) != "manual_hub_oidc" && !name(item).hasPrefix("manual_channel:") && !pairingActions.contains { $0 === item }
                    }
                    if id == "inbox_control_bar", barItems.contains(where: { abs($0.frame.midY-parentBounds.midY)>1.5 }) {
                        let issue = id + ": controls are not centered in the bar"
                        nonAnchoredActionBars.append(issue); clippedViews.append(issue)
                    }
                    var rows: [(String,CGFloat)] = []
                    if let top = barItems.map({ $0.frame.midY }).max() { rows.append(("top",top)) }
                    if stratum, let bottom = barItems.map({ $0.frame.midY }).min(), bottom != rows.first?.1 { rows.append(("bottom",bottom)) }
                    for (row,y) in rows {
                        // Cards contain authored title, field, and action rows.
                        // Single-row bars/footers must retain every sibling,
                        // including controls displaced beyond a row tolerance.
                        let singleRow = ["inbox_control_bar","footer","settings_footer","directory_footer","directory_facet_bar"].contains(id)
                        let items = singleRow ? barItems : barItems.filter { abs($0.frame.midY-y) <= 8 }
                        let leadingClearOnly = id.hasPrefix("stratum_hub:") && row == "bottom" && items.count == 1 && name(items[0]).hasPrefix("clear_channel_service:")
                        let unanchored = leadingClearOnly ? abs(items[0].frame.minX - CitoMetrics.cardInset) > 1.5 :
                            (items.map { $0.frame.maxX }.max() ?? 0) < parentBounds.width - 20.5
                        if unanchored {
                            let issue = id + ": " + row + " row is not edge anchored"
                            nonAnchoredActionBars.append(issue); clippedViews.append(issue)
                        }
                        if let minY = items.map({ $0.frame.midY }).min(), let maxY = items.map({ $0.frame.midY }).max(), maxY-minY > 1.5 {
                            let issue = id + ": " + row + " row controls are not vertically centered"
                            nonAnchoredActionBars.append(issue); clippedViews.append(issue)
                        }
                    }
                }
                // Compare authored leaf labels with controls even through nested
                // layout groups, including rounded text/status containers.
                if view is SurfaceView || view is DirectoryPeerCellView || view.identifier != nil {
                    func leaves(_ parent: NSView) -> [NSView] {
                        parent.subviews.filter { !$0.isHidden }.flatMap { child -> [NSView] in
                            if child is NSTextField || child is MenuControl || child is NSButton || child is NSSegmentedControl || child is NSSwitch { return [child] }
                            if child is DirectoryPeerCellView || child is NSScrollView { return [] }
                            return leaves(child)
                        }
                    }
                    let descendants = leaves(view)
                    let labels = descendants.compactMap { $0 as? NSTextField }.filter { !$0.isEditable }
                    let controls = descendants.filter { $0 is MenuControl || $0 is NSButton || $0 is NSSegmentedControl || $0 is NSSwitch }
                    for label in labels {
                        let labelFrame = label.convert(label.bounds,to:view).insetBy(dx:0.5,dy:0.5)
                        for control in controls where labelFrame.intersects(control.convert(control.bounds,to:view).insetBy(dx:0.5,dy:0.5)) {
                            overlappingSiblings.append("\(id): \(name(label)) / \(name(control)) (nested)")
                        }
                    }
                }
                let paddedSurface = (view as? SurfaceView).map { $0.fillColor.alphaComponent > 0 || ($0.layer?.borderWidth ?? 0)>0 } ?? false
                if (paddedSurface || view is DirectoryPeerCellView) && view.frame.width >= 60 && view.frame.height >= 32
                   && !["settings_panel","directory_panel"].contains(id) {
                    func checkPadding(_ child: NSView) {
                        guard !child.isHidden, !(child is NSVisualEffectView) else { return }
                        let terminal = child is NSControl || child is NSImageView || child.subviews.isEmpty
                        let painted = (child as? SurfaceView).map { $0.fillColor.alphaComponent > 0 || ($0.layer?.borderWidth ?? 0) > 0 } ?? false
                        if terminal || painted {
                            let f = child.convert(child.bounds,to:view)
                            if f.width > 5 && f.height > 5, !parentBounds.insetBy(dx:6,dy:6).contains(f) {
                                let issue = "\(id) > \(name(child)): card padding < 6pt"
                                tightPadding.append(issue); clippedViews.append(issue)
                            }
                        }
                        if !terminal { child.subviews.forEach(checkPadding) }
                    }
                    children.forEach(checkPadding)
                }
                for (index, child) in children.enumerated() {
                    let f = frame(child)
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
            result["background"] = components(textBackground ?? fill)
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
                result["editable"] = label.isEditable
                result["wraps"] = label.cell?.wraps ?? false
                result["maximum_lines"] = label.maximumNumberOfLines
                result["placeholder"] = label.placeholderString ?? ""
                result["tooltip"] = label.toolTip ?? ""
                result["label_width"] = label.cell?.drawingRect(forBounds: label.bounds).width ?? label.bounds.width
                result["label_measured_width"] = label.cell?.cellSize.width ?? label.attributedStringValue.size().width
            }
            if let control = view as? NSControl {
                result["enabled"] = control.isEnabled; result["alpha"] = control.alphaValue
                result["accessibility_label"] = control.accessibilityLabel() ?? ""
                result["accessibility_role"] = control.accessibilityRole()?.rawValue ?? ""
                if let value = control.accessibilityValue() as? NSNumber { result["accessibility_value"] = value }
            }
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
            let field = view as? NSTextField
            if let control = view as? NSControl, control.isEnabled,
               (control as? MenuControl)?.pressed != nil || field?.isEditable == true {
                // Sample the visible interior in the root's superview coordinates.
                let bounds = control.bounds.intersection(control.visibleRect)
                if !bounds.isEmpty {
                    let editor = field?.currentEditor()
                    let occluded = [0.15, 0.5, 0.85].contains { fraction in
                        let point = control.convert(NSPoint(x:bounds.minX+bounds.width*fraction,y:bounds.midY),to:root.superview)
                        let hit = root.hitTest(point)
                        // AppKit may host the search field's editable region in a native subview.
                        let nativeSearchHit = control is NSSearchField && hit?.isDescendant(of: control) == true
                        return hit !== control && (editor == nil || hit !== editor) && !nativeSearchHit
                    }
                    if occluded {
                        overlappingSiblings.append("\(surface) > \(name(control)): occluded in hitTest")
                    }
                }
            }
            if field != nil { return }
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
        let now = Date()
        pruneCopyFeedback(at: now)
        let snapshot: [String: Any] = ["items": items, "tight_padding": tightPadding, "non_anchored_action_bars": nonAnchoredActionBars, "truncated_labels": truncatedLabels, "overlapping_siblings": overlappingSiblings, "clipped_views": clippedViews, "icon_alpha": probes, "count": uniqueSessionCount,
            "home_basename": (NSHomeDirectory() as NSString).lastPathComponent,
            "rename_field_identity": renameField.map { String(describing:ObjectIdentifier($0)) } ?? "",
            "tracking_cancellations": (cardMenu as? HeadlessMenu)?.trackingCancellations ?? 0,
            "low_contrast_labels": lowContrastLabels, "jargon_leaks":jargonLeaks, "address_mismatches":addressMismatches,
            "edit_menu": NSApp.mainMenu?.items.first(where: { $0.submenu?.title == "Edit" })?.submenu?.items.map { ["title": $0.title, "key": $0.keyEquivalent, "action": $0.action.map(NSStringFromSelector) ?? "", "shift": $0.keyEquivalentModifierMask.contains(.shift)] as [String: Any] } ?? [],
            "key_panels": [settingsPanel, directoryPanel].compactMap { window -> [String: Any]? in
                guard let window = window else { return nil }
                return ["titled": window.styleMask.contains(.titled), "full_size_content": window.styleMask.contains(.fullSizeContentView),
                    "automatic_key_view_loop": window.autorecalculatesKeyViewLoop,
                    "hidden_title": window.titleVisibility == .hidden, "transparent_titlebar": window.titlebarAppearsTransparent,
                    "closable": window.styleMask.contains(.closable), "miniaturizable": window.styleMask.contains(.miniaturizable),
                    "visible_window_buttons": KeyPanel.chromeButtons.allSatisfy { window.standardWindowButton($0)?.isHidden == false },
                    "close_enabled": window.standardWindowButton(.closeButton)?.isEnabled == true,
                    "miniaturize_enabled": window.standardWindowButton(.miniaturizeButton)?.isEnabled == true,
                    "zoom_disabled": window.standardWindowButton(.zoomButton)?.isEnabled == false,
                    "close_and_minimize_hit_test": [NSWindow.ButtonType.closeButton, .miniaturizeButton].allSatisfy { type in
                        guard let frame = window.contentView?.superview, let button = window.standardWindowButton(type) else { return false }
                        let center = button.convert(NSPoint(x: button.bounds.midX, y: button.bounds.midY), to: frame.superview)
                        return frame.hitTest(center) === button
                    },
                    "window_button_positions": KeyPanel.chromeButtons.compactMap { type -> [CGFloat]? in
                        guard let content = window.contentView, let button = window.standardWindowButton(type) else { return nil }
                        let rect = content.convert(button.bounds, from: button)
                        return [rect.minX, content.bounds.maxY - rect.midY]
                    }]
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
            "directory_copy_remaining": directoryCopiedUntil.mapValues { $0.timeIntervalSince(now) },
            "activity_copy_remaining": activityCopiedUntil.mapValues { $0.timeIntervalSince(now) },
            "hub_attachment_counts": Dictionary(uniqueKeysWithValues: hubs.map { ($0.name, $0.attachments?.count ?? 0) }),
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
            "pending_hub_save": hubForm.pendingSave?.requestId ?? "",
            "hub_invite_pending": hubInvitePending, "host_hub_port_pending": hostPort.pending,
            "pending_app_checks": pendingControls.filter { $0.value.appCheck }.keys.sorted(),
            "pending_channel_saves": pendingControls.filter { $0.value.channelSave != nil }.keys.sorted(),
            "pending_channel_resets": pendingControls.filter { $0.value.channelReset.map { $0.hub || $0.user } == true }.keys.sorted(),
            "pending_controls": pendingControls.keys.sorted(),
            "settings_rebuild_deferred": settingsRebuildDeferred,
            "settings_channel_hub": mountedChannelsSettings?.hub ?? "",
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
        return snapshot
    }
}
