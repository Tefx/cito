import AppKit

extension Companion: NSMenuDelegate {
    var uniqueSessionCount: Int {
        Dictionary(grouping: listeners.filter { $0.harness != "human" && (effectiveTargetHub.isEmpty || $0.hub == effectiveTargetHub) }, by: { $0.harness })
            .values.reduce(0) { $0 + Set($1.map { $0.session_id }).count }
    }
    var humanListening: Bool { hubs.first { $0.name == effectiveTargetHub }?.human_availability == "ACTIVE" }

    func statusColor(_ status: String) -> NSColor {
        status == "connected" ? CitoPalette.successText : status == "connecting" ? CitoPalette.warningText : status == "auth_required" ? CitoPalette.dangerText : CitoPalette.secondaryText
    }
    var effectiveTargetHub: String {
        if !selectedTargetHub.isEmpty && hubs.contains(where: { $0.name == selectedTargetHub }) {
            return selectedTargetHub
        }
        return resolvedDefaultHub(requireEnabled: true, requireConnected: true)
            ?? hubs.first(where: { $0.status == "connected" })?.name
            ?? hubs.first?.name ?? ""
    }

    func updateMenuErrorRow() {
        if let item = cardMenu.items.first(where: { $0.view?.identifier?.rawValue == "menu_error_row" }) {
            cardMenu.removeItem(item)
        }
        guard let message = menuBannerError else { return }
        let labelHeight = max(30, measureWrappedHeight(message, width: 320, font: .systemFont(ofSize: 10)))
        let row = NSView(frame:NSRect(x:0,y:0,width:380,height:labelHeight+6))
        row.identifier = NSUserInterfaceItemIdentifier("menu_error_row")
        row.appearance = testAppearance
        let label = textLabel(message,NSRect(x:12,y:3,width:320,height:labelHeight),size:10,color:CitoPalette.dangerText)
        label.maximumNumberOfLines = 0; label.lineBreakMode = .byWordWrapping; label.cell?.wraps = true; label.toolTip = message
        row.addSubview(label)
        let dismiss = MenuControl("", frame:NSRect(x:340,y:row.bounds.midY-11,width:26,height:22), symbol:"xmark")
        dismiss.identifier = NSUserInterfaceItemIdentifier("dismiss_menu_error")
        dismiss.style = .subtleHeader
        dismiss.toolTip = "Dismiss error"; dismiss.setAccessibilityLabel("Dismiss error")
        dismiss.pressed = { [weak self] in self?.menuBannerError = nil; self?.rebuild() }
        row.addSubview(dismiss)
        let item = NSMenuItem(); item.view = row
        cardMenu.insertItem(item, at: min(1, cardMenu.numberOfItems))
    }

    func menuWillOpen(_ menu: NSMenu) {
        MenuControl.clearHoverWhere { $0.enclosingMenuItem != nil || ($0.window !== self.directoryPanel && $0.window !== self.settingsPanel) }
        menuOpen = true
        MenuControl.menuTracking = true
        emit(["op": "open_menu"], userInitiated: false)
        if menuDirty { rebuild() }
    }
    func menuDidClose(_ menu: NSMenu) {
        menuOpen = false
        MenuControl.menuTracking = false
        if !expandedSessionActions.isEmpty {
            expandedSessionActions.removeAll()
            menuDirty = true
        }
        if renamingSessionKey != nil {
            cancelSessionRename()
            menuDirty = true
        }
        emit(["op": "close_menu"], userInitiated: false)
        MenuControl.clearHoverWhere { $0.enclosingMenuItem != nil || ($0.window !== self.directoryPanel && $0.window !== self.settingsPanel) }
    }

    var statusTooltip: String {
        let attention=hubs.contains { $0.status != "connected" || $0.error != nil }
        let count=uniqueSessionCount
        var detail = attention ? "Hub connection needs attention"
            : count > 0 ? "\(count) session\(count == 1 ? "" : "s") listening"
            : humanListening ? "Personal inbox active"
            : hubs.first { $0.name == effectiveTargetHub }?.human_paused == true ? "Inbox paused" : "Not listening"
        if unreadCount > 0 { detail += " · \(unreadCount) unread" }
        return "Cito · " + detail
    }

    func updateStatusItem() {
        let warning = hubs.contains { $0.status != "connected" || $0.error != nil }
        let icon: StatusIconState = warning ? .warning : (listeners.contains { $0.harness != "human" } || unreadCount > 0) ? .active : .idle
        let tooltip = statusTooltip
        guard lastStatusPresentation?.icon != icon || lastStatusPresentation?.unread != unreadCount || lastStatusPresentation?.tooltip != tooltip,
              let button = status?.button ?? headlessStatusButton else { return }
        if !headless && lastStatusPresentation?.unread != unreadCount {
            NSApp.dockTile.badgeLabel = unreadCount > 0 ? String(unreadCount) : nil
        }
        formatStatusButton(button, unreadCount: unreadCount, image: Self.statusIcons[icon])
        button.toolTip = tooltip
        button.setAccessibilityLabel(tooltip)
        lastStatusPresentation = (icon, unreadCount, tooltip)
        statusUpdateCount += 1
    }
    private func cancelSessionRename() {
        renamingSessionKey = nil
        renameField?.window?.makeFirstResponder(nil)
        renameField?.removeFromSuperview()
        renameField = nil
        menuRebuildDeferred = false
    }

    private func makeAddressCopyControl(_ address: String, id: String, authority: String,
                                        fingerprintOnly: Bool = false, maxWidth: CGFloat = 220) -> MenuControl {
        let font = NSFont.monospacedSystemFont(ofSize: 10.5, weight: .medium)
        let symbol = "doc.on.doc"
        let relative = menuAddressLabel(address, hub: authority, fingerprintOnly: fingerprintOnly)
        let title = compactAddressLabel(relative, maxWidth: maxWidth, font: font, paddingX: 3, leadingSymbol: symbol)
        let copy = MenuControl(title, frame: NSRect(x: 0, y: 0, width: 0, height: 20), leadingSymbol: symbol, paddingX: 3)
        copy.identifier = NSUserInterfaceItemIdentifier(id)
        copy.label.font = font; copy.label.alignment = .left
        copy.maximumWidth = maxWidth
        copy.reservedTitles = [(title, symbol), ("✓ Copied", nil)]
        copy.frame.size.width = copy.reservedWidth
        copy.toolTip = address; copy.setAccessibilityLabel(address)
        let key = TransientFeedbackStore.Key(.menuCopy, id, authority: authority)
        copy.showFeedback(key: key)
        copy.pressed = { [weak self, weak copy] in self?.copyText(address, control: copy, key: key) }
        return copy
    }

    private func mountSessionActions(in card: NSView, listener: Listener, pinApplies: Bool, pinned: Bool,
                                     workspaceSupported: Bool, activityTimed: Bool) {
        let id = listener.scoped_id
        let height = CitoMetrics.sessionActionsHeight
        let hairline = roundedView(NSRect(x: 0, y: height - 1, width: card.bounds.width, height: 1),
                                   radius: 0, color: CitoPalette.channelCardBorder)
        hairline.identifier = NSUserInterfaceItemIdentifier("session_actions_divider:\(id)")
        card.addSubview(hairline)
        func action(_ title: String, _ name: String, symbol: String? = nil) -> MenuControl {
            let control = MenuControl(title, frame: NSRect(x: 0, y: 0, width: 0, height: 16), leadingSymbol: symbol)
            control.identifier = NSUserInterfaceItemIdentifier("\(name):\(id)")
            card.addSubview(control)
            return control
        }
        var leading: [NSView] = []
        var trailing: [NSView] = []
        if listener.can_rename == true {
            let rename = action("Rename", "rename")
            rename.toolTip = "Rename this session"
            rename.isEnabled = renamingSessionKey != id
            rename.pressed = { [weak self] in
                guard let self else { return }
                self.cancelSessionRename()
                self.renamingSessionKey = id
                self.rebuild(revealSession: id)
            }
            leading.append(rename)
        }
        if pinApplies {
            let pin = action(pinned ? "Pinned" : "Pin", "pin", symbol: pinned ? "pin.fill" : "pin")
            let lifetime = activityTimed ? "Unpinned sessions auto-retire after the idle timeout." : "Unpinned sessions stay listening while their endpoint is available; no idle timeout applies."
            pin.toolTip = (pinned ? "Pinned. Unpin to follow Auto-Listen. " : "Pin this session. ") + lifetime + " Pinned sessions stay listening when Auto-Listen is turned off."
            pin.setAccessibilityLabel(pinned ? "Unpin session" : "Pin session")
            pin.pressed = { [weak self] in self?.emit(["op":"set_listen_mode","hub":listener.hub,"listener_id":listener.listener_id,"listen_mode":pinned ? "auto" : "manual"]) }
            leading.append(pin)
        }
        if workspaceSupported && listener.can_set_scope == true {
            var scope = listener.scope
            let control = action("Only Me", "scope", symbol: "lock.fill")
            control.reservedTitles = [("Only Me", "lock.fill"), ("Everyone", "person.2.fill")]
            func update(_ control: MenuControl) {
                let shared = scope == "workspace"
                control.label.stringValue = shared ? "Everyone" : "Only Me"
                control.setLeadingSymbol(shared ? "person.2.fill" : "lock.fill")
                control.setAccessibilityLabel(control.label.stringValue)
                control.toolTip = shared
                    ? "Visibility: Everyone — anyone connected to '\(listener.hub)' can discover and message this session. Click to restrict to Only Me."
                    : "Visibility: Only Me — only your own sessions and devices on '\(listener.hub)' can message this session. Click to share with everyone."
            }
            update(control)
            control.pressed = { [weak self, weak control] in
                guard let self, let control else { return }
                scope = scope == "workspace" ? "self" : "workspace"
                update(control)
                self.emit(["op":"set_scope","hub":listener.hub,"listener_id":listener.listener_id,"scope":scope])
            }
            leading.append(control)
        }
        if listener.can_stop == true {
            let stop = action("Stop", "stop")
            stop.style = .destructiveHover
            stop.toolTip = "Stop only this session and keep it stopped. Auto-Listen on \(listener.hub) stays unchanged."
            stop.pressed = { [weak self] in self?.emit(["op":"unlisten_session","hub":listener.hub,"harness":listener.harness,"session_id":listener.session_id]) }
            trailing.append(stop)
        }
        layoutBar(in: card, y: 0, height: height, leading: leading, trailing: trailing)
    }

    private func captureScrollOffsets() {
        for item in cardMenu.items {
            let scroll = (item.view as? NSScrollView) ?? item.view?.subviews.compactMap { $0 as? NSScrollView }.first
            if let scroll = scroll, let id = scroll.identifier?.rawValue,
               ["active_scroll", "my_remote_scroll", "available_scroll"].contains(id) {
                let key = "\(id):\(renderedScrollHub)"
                let offset = scroll.contentView.bounds.origin
                menuScrollOffsets[key] = offset
                let top = scroll.documentView?.subviews.first(where: { $0.frame.maxY > offset.y })
                menuScrollAnchors[key] = top.flatMap { row in
                    row.identifier.map { ($0.rawValue, offset.y - row.frame.minY) }
                }
            }
        }
    }

    func rebuild(preservingEditor: Bool = false, scrollerStyle: NSScroller.Style? = nil, revealSession: String? = nil) {
        updateStatusItem()
        menuDirty = true
        let targetHub = effectiveTargetHub
        let orderedListeners = listeners.filter { $0.harness != "human" && (targetHub.isEmpty || $0.hub == targetHub) }
        if targetHub != renderedScrollHub { expandedSessionActions.removeAll() }
        if !expandedSessionActions.isEmpty { expandedSessionActions.formIntersection(orderedListeners.map(\.scoped_id)) }
        if let key = renamingSessionKey, !orderedListeners.contains(where: { $0.scoped_id == key && $0.can_rename == true }) {
            cancelSessionRename()
        }
        guard menuOpen || renderingForInspection else { return }
        if preservingEditor, renamingSessionKey != nil, renameField?.currentEditor() != nil {
            updateMenuErrorRow()
            menuRebuildDeferred = true
            return
        }
        menuRebuildDeferred = false
        menuDirty = false
        MenuControl.clearHoverWhere { $0.enclosingMenuItem != nil || ($0.window !== self.directoryPanel && $0.window !== self.settingsPanel) }
        rebuildCount += 1
        let menu = cardMenu
        let existingRenameField = renamingSessionKey == nil ? nil : renameField
        let renameEditor = existingRenameField?.currentEditor() as? NSTextView
        let renameSelection = renameEditor?.selectedRange()
        let renameDraft = renameEditor?.string ?? existingRenameField?.stringValue
        captureScrollOffsets()
        let hubStripHandoff = menuHubStrip?.capture()
        menu.removeAllItems()
        renderedScrollHub = targetHub
        func makeMenuScroll(height: CGFloat) -> NSScrollView {
            let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: height))
            if let scrollerStyle { scroll.scrollerStyle = scrollerStyle }
            scroll.drawsBackground = false
            scroll.hasVerticalScroller = true
            return scroll
        }
        func host(_ view: NSView, id: String) -> NSMenuItem {
            view.identifier = NSUserInterfaceItemIdentifier(id)
            let item = NSMenuItem()
            if view is NSScrollView {
                // Menu roots use the same unflipped coordinate space as their host.
                let root = NSView(frame:NSRect(origin:.zero,size:view.frame.size))
                root.appearance = testAppearance
                root.addSubview(view); item.view = root
            } else { view.appearance = testAppearance; item.view = view }
            menu.addItem(item)
            let key = "\(id):\(targetHub)"
            if let scroll = view as? NSScrollView, let offset = menuScrollOffsets[key], let doc = scroll.documentView {
                scroll.layoutSubtreeIfNeeded()
                let anchoredY = menuScrollAnchors[key].flatMap { anchor in doc.subviews.first { $0.identifier?.rawValue == anchor.rowId }.map { $0.frame.minY + anchor.deltaY } } ?? offset.y
                scroll.contentView.scroll(to:NSPoint(x:max(0,min(offset.x,doc.frame.width-scroll.contentSize.width)),y:max(0,min(anchoredY,doc.frame.height-scroll.contentSize.height))))
                scroll.reflectScrolledClipView(scroll.contentView)
            }
            return item
        }
        func hostRows(_ rows: [(id: String, view: NSView)], in scroll: NSScrollView?, id: String) {
            guard let scroll else {
                for (id, row) in rows { _ = host(row, id: id) }
                return
            }
            let height = rows.reduce(CGFloat(0)) { $0 + $1.view.frame.height }
            let doc = FlippedView(frame: NSRect(x: 0, y: 0, width: scroll.contentSize.width, height: height))
            var y: CGFloat = 0
            for (id, row) in rows {
                row.identifier = NSUserInterfaceItemIdentifier(id)
                row.frame.origin.y = y
                doc.addSubview(row)
                y += row.frame.height
            }
            scroll.documentView = doc
            _ = host(scroll, id: id)
        }
        let btnFont = NSFont.systemFont(ofSize: 11, weight: .medium)
        let header = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 40))
        header.addSubview(textLabel("Cito", NSRect(x: 12, y: 12, width: 32, height: 18), size: 13, weight: .bold))
        let humanHub = hubs.first { $0.name == targetHub }
        let compactHeader = hubs.count > 1 && humanHub?.status == "connected" && humanHub?.human_paused != true && humanHub?.error == nil
        let hubGap: CGFloat = 4
        func hubPillWidth(_ title: String) -> CGFloat { measureTextWidth(title, font: btnFont) + 24 }
        let hubPillNaturalWidths = hubs.map { hubPillWidth($0.name) }
        let totalHubPillsWidth = PillTrackGeometry.trailingWidth(widths: hubPillNaturalWidths, spacing: hubGap)
        let pairWidth = PillTrackGeometry.trailingWidth(widths: Array(hubPillNaturalWidths.sorted(by: >).prefix(2)), spacing: hubGap)
        let subWidth: CGFloat
        if compactHeader {
            subWidth = 0
        } else {
            let subtitleText: String
            if humanHub?.status == "auth_required" { subtitleText = "Sign in required" }
            else if ["unreachable","error","disconnected"].contains(humanHub?.status ?? "") { subtitleText = "Hub unavailable" }
            else if humanHub?.status != "connected" { subtitleText = "Connecting…" }
            else if humanHub?.human_paused == true { subtitleText = "Inbox paused" }
            else if humanHub?.error != nil { subtitleText = "Hub needs attention" }
            else if !orderedListeners.isEmpty { subtitleText = "Sessions listening" }
            else if humanListening { subtitleText = "Personal inbox active" }
            else { subtitleText = "Not listening" }
            let limit: CGFloat = hubs.count > 1 ? min(246, max(84, 368 - 54 - pairWidth)) : 246
            subWidth = min(limit, measureTextWidth(subtitleText, font: .systemFont(ofSize: 11.5)) + 8)
            let label = textLabel(subtitleText, NSRect(x: 50, y: 12, width: subWidth, height: 17), size: 11.5, color: CitoPalette.secondaryText)
            label.identifier = NSUserInterfaceItemIdentifier("header_subtitle")
            label.lineBreakMode = .byTruncatingMiddle; label.toolTip = subtitleText
            header.addSubview(label)
        }
        _ = host(header, id: "header")
        updateMenuErrorRow()
        var drawerItems: [String: NSMenuItem] = [:]
        func needsAttention(_ hub: Hub) -> Bool {
            (hub.status != "connected" && hub.status != "connecting") || hub.error != nil
        }
        let showHubControls = hubs.count != 1 || hubs.contains {
            !($0.is_builtin == true && $0.status == "connected" && $0.error == nil)
        }
        for hub in hubs where showHubControls {
            let drawer = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 32))
            let loginRun = { self.emit(["op": "hub_action", "hub": hub.name, "action": hub.status == "connected" ? "switch_account" : "login"]) }
            let reconnectRun = { self.emit(["op": "hub_action", "hub": hub.name, "action": "reconnect"]) }
            let disconnectRun = { self.cardMenu.cancelTracking(); self.openSettingsPanel(tab: "hubs") }
            let buttons: [(String, String, Bool, () -> Void)]
            switch hub.status {
            case "connected" where hub.error != nil:
                buttons = [("Reconnect", "reconnect", false, reconnectRun), ("Hub Settings…", "open_hub_settings", false, disconnectRun)]
            case "connected" where hub.auth_mode == "oidc":
                buttons = [("Switch Account", "login", false, loginRun), ("Hub Settings…", "open_hub_settings", false, disconnectRun)]
            case "connected":
                buttons = [("Hub Settings…", "open_hub_settings", false, disconnectRun)]
            case "connecting":
                buttons = [("Hub Settings…", "open_hub_settings", false, disconnectRun)]
            case "auth_required":
                buttons = (hub.can_login == true ? [("Sign In…", "login", true, loginRun)] : []) + [("Hub Settings…", "open_hub_settings", false, disconnectRun)]
            default:
                buttons = [("Connect", "reconnect", true, reconnectRun)] + (hub.can_login == true ? [("Sign In…", "login", false, loginRun)] : []) + [("Hub Settings…", "open_hub_settings", false, disconnectRun)]
            }
            var drawerControls: [NSView] = []
            for (title, action, primary, run) in buttons {
                let width = MenuControl.intrinsicWidth(title, font: btnFont)
                let control = MenuControl(title, frame: NSRect(x: 0, y: 5, width: width, height: 22))
                control.label.font = btnFont
                control.style = primary ? .prominent : .neutral
                control.identifier = NSUserInterfaceItemIdentifier("\(action):\(hub.name)")
                control.pressed = run; drawer.addSubview(control); drawerControls.append(control)
            }
            let autoExpand = needsAttention(hub)
            let status: String
            if let error = hub.error { status = error }
            else if hub.status == "connected" { status = hub.auth_mode == "oidc" ? "Connected as \(hub.user ?? "user")" : "Connected · \(hub.user ?? "local")" }
            else if hub.status == "connecting" { status = "Connecting to hub…" }
            else if hub.status == "auth_required" { status = "Authentication required" }
            else { status = "Offline · Disconnected" }
            let statusText = autoExpand && hub.auth_mode == "oidc" ? "\(hub.name) · \(status)" : status
            let detail = textLabel(statusText, NSRect(x: 12, y: 8, width: 40, height: 16),
                color: hub.error != nil || hub.status == "auth_required" ? .labelColor : CitoPalette.secondaryText)
            detail.lineBreakMode = .byTruncatingMiddle
            detail.toolTip = statusText; drawer.addSubview(detail)
            let detailWidth = drawer.bounds.width - 24 - drawerControls.reduce(CGFloat(0)) { $0 + $1.frame.width + 4 }
            let detailBounds = NSRect(x: 0, y: 0, width: max(0, detailWidth), height: detail.bounds.height)
            let drawingWidth = detail.cell?.drawingRect(forBounds: detailBounds).width ?? detailBounds.width
            if hub.error != nil || detail.requiredSingleLineWidth > drawingWidth {
                detail.identifier = NSUserInterfaceItemIdentifier("hub_error:\(hub.name)")
                detail.maximumNumberOfLines = 0; detail.lineBreakMode = .byWordWrapping
                let width = CitoMetrics.menuWidth - 24
                let height = measureWrappedHeight(statusText, width: width, font: detail.font ?? .systemFont(ofSize: 11))
                detail.frame = NSRect(x: 12, y: 32, width: width, height: height)
                drawer.frame.size.height = height + 40
                layoutBar(in: drawer, y: 5, height: 22, spacing: 4, trailing: drawerControls)
            } else {
                layoutBar(in: drawer, y: 5, height: 22, spacing: 4, flexible: detail, trailing: drawerControls)
            }
            let item = host(drawer, id: "drawer:\(hub.name)")
            item.isHidden = !(hubDrawerVisibility[hub.name] ?? autoExpand)
            drawerItems[hub.name] = item
        }
        let stripX = subWidth == 0 ? 48 : max(138, 54 + subWidth)
        let stripWidth = max(44, 368 - stripX)
        let compactChips = totalHubPillsWidth > stripWidth
        let fullCap = stripWidth - 2 * PillTrackGeometry.inset
        let geometry = CitoMetrics.hubSegments
        var hubPills: [MenuControl] = []
        for (index, hub) in hubs.enumerated() where showHubControls {
            let isSingleDefault = hubs.count == 1 && hub.is_builtin == true
            let title: String
            if isSingleDefault {
                switch hub.status {
                case "connected": title = hub.user ?? "Connected"
                case "connecting": title = "Connecting…"
                case "auth_required": title = "Sign In"
                default: title = "Offline"
                }
            } else {
                title = hub.name
            }
            let natural = isSingleDefault ? hubPillWidth(title) : hubPillNaturalWidths[index]
            let width = compactHeader || compactChips ? natural : min(natural, fullCap)
            let pill = MenuControl(title, frame: NSRect(x: 0, y: 0, width: width, height: geometry.pillHeight), radius: geometry.pillRadius)
            pill.identifier = NSUserInterfaceItemIdentifier("hub:\(hub.name)")
            if hubs.count == 1 { pill.style = hub.status == "connected" && hub.error == nil ? .prominent : .neutral }
            pill.label.font = btnFont
            pill.label.lineBreakMode = .byTruncatingMiddle
            pill.segmentIndicatorColor = CitoPalette.segmentInk(statusColor(hub.status))
            let dot = roundedView(.zero, radius: 3, color: pill.segmentIndicatorColor)
            pill.addSubview(dot)
            pill.segmentIndicator = dot
            pill.toolTip = isSingleDefault ? (hub.status == "connected" ? "Connected · \(hub.user ?? "local")" : title) : "\(hub.name) · \(["connected":"Connected", "auth_required":"Sign-in required", "disconnected":"Offline", "connecting":"Connecting…"][hub.status] ?? "Unavailable")"
            pill.setAccessibilityLabel(pill.toolTip ?? hub.name)
            pill.pressed = { [weak self, weak menu] in
                guard let self = self, let drawer = drawerItems[hub.name] else { return }
                if self.hubs.count > 1 {
                    let alreadySelected = self.effectiveTargetHub == hub.name
                    self.selectedTargetHub = hub.name
                    if alreadySelected {
                        self.hubDrawerVisibility[hub.name] = drawer.isHidden
                    } else if needsAttention(hub) {
                        self.hubDrawerVisibility[hub.name] = true
                    }
                    self.rebuild(preservingEditor: false)
                    return
                }
                drawer.isHidden = !drawer.isHidden
                self.hubDrawerVisibility[hub.name] = !drawer.isHidden
                menu?.update()
                MenuControl.reconcileActiveHover()
            }
            hubPills.append(pill)
        }
        if !hubPills.isEmpty {
            menuHubStrip = SegmentedPillStrip.mount(in: header,
                frame: NSRect(x: stripX, y: 7, width: stripWidth, height: geometry.trackHeight),
                id: "header_hub_strip", alignment: .trailing, spacing: hubGap, geometry: geometry, pills: hubPills,
                selected: hubPills.first { $0.identifier?.rawValue == "hub:\(targetHub)" }, handoff: hubStripHandoff)
        }
        var activeRows: [(id: String, view: NSView)] = []
        let map = harnessMap
        let myRemote = self.myRemote.filter { targetHub.isEmpty || $0.hub == nil || $0.hub == targetHub }
        let targetHubObj = humanHub
        if targetHubObj?.human_paused == true {
            let notice = MenuControl("Inbox paused · Resume in Settings…", frame:NSRect(x:12,y:3,width:356,height:24), leadingSymbol:"pause.circle", radius:6)
            notice.identifier = NSUserInterfaceItemIdentifier("inbox_paused_notice")
            notice.style = .subtleHeader
            notice.toolTip = "Inbox alerts are paused on \(targetHub). Open Human Channels in Settings to resume."
            notice.setAccessibilityLabel(notice.toolTip)
            notice.pressed = { [weak self, weak menu] in
                guard let self = self else { return }
                menu?.cancelTracking(); self.openSettingsPanel(tab: "channels", channelHub: targetHub)
            }
            let row = NSView(frame:NSRect(x:0,y:0,width:380,height:30)); row.addSubview(notice)
            _ = host(row,id:"inbox_notice_row")
        }
        let agentHeader=NSView(frame:NSRect(x:0,y:0,width:380,height:22))
        let sectionTitle = orderedListeners.isEmpty ? "LISTENING SESSIONS" : "LISTENING SESSIONS · \(orderedListeners.count)"
        agentHeader.addSubview(textLabel(sectionTitle,NSRect(x:16,y:3,width:344,height:16),size:10,weight:.semibold,color:CitoPalette.secondaryText))
        _ = host(agentHeader,id:"listening_sessions_header")
        let scrollThreshold = myRemoteCollapsed && availableCollapsed ? 5 : 3
        let activeScroll = orderedListeners.count > scrollThreshold ? makeMenuScroll(height: CitoMetrics.sessionViewportHeight) : nil
        let activeWidth = activeScroll?.contentSize.width ?? CitoMetrics.menuWidth
        for listener in orderedListeners {
            let id = listener.scoped_id
            let authority = targetHub.isEmpty ? hubs.first { $0.name == listener.hub } : targetHubObj
            let pinned = listener.can_pin == true && listener.listen_mode != "auto"
            let pinApplies = listener.can_pin == true && (authority?.auto_listen == true || !pinned)
            let workspaceSupported = authority?.auth_mode == "oidc" && authority?.supports_workspace_scope == true
            let workspace = workspaceSupported && listener.scope == "workspace"
            let expanded = expandedSessionActions.contains(id)
            let stripHeight = expanded ? CitoMetrics.sessionActionsHeight : 0
            let cardHeight = CitoMetrics.sessionCardHeight + stripHeight
            let row = NSView(frame: NSRect(x: 0, y: 0, width: activeWidth, height: cardHeight + CitoMetrics.sessionRowGap))
            let card = roundedView(NSRect(x: CitoMetrics.menuInset, y: CitoMetrics.sessionRowGap / 2,
                width: activeWidth - 2 * CitoMetrics.menuInset, height: cardHeight), radius: CitoMetrics.cardRadius,
                color: CitoPalette.channelCardFill, border: CitoPalette.channelCardBorder)
            card.identifier = NSUserInterfaceItemIdentifier("session_card:\(id)")
            row.addSubview(card)
            activeRows.append((id: "session:\(id)", view: row))
            let titleX: CGFloat = 42
            let badge = makeHarnessBadge(NSRect(x: 10, y: stripHeight + 9, width: 24, height: 24),
                radius: 6, iconInset: 5, harness: listener.harness, harnesses: map)
            badge.identifier = NSUserInterfaceItemIdentifier("session_badge:\(id)")
            card.addSubview(badge)

            let moreTitle = expanded ? "Hide session actions" : "Show session actions"
            let more = MenuControl(moreTitle, frame: NSRect(x: 0, y: 0, width: 22, height: 22), symbol: "ellipsis")
            more.identifier = NSUserInterfaceItemIdentifier("more:\(id)")
            more.style = .subtleHeader
            more.pressed = { [weak self] in
                guard let self else { return }
                if self.expandedSessionActions.remove(id) == nil { self.expandedSessionActions.insert(id) }
                self.rebuild(revealSession: id)
            }
            let copy = makeAddressCopyControl(listener.display_address, id: "copy:\(id)", authority: listener.hub,
                fingerprintOnly: listener.is_self_principal == true, maxWidth: 136)
            copy.style = .subtleHeader
            card.addSubview(copy)
            card.addSubview(more)
            layoutBar(in: card, y: stripHeight + 14, height: 22, spacing: 4, trailing: [copy, more])
            let textWidth = copy.frame.minX - 4 - titleX

            let title = listener.use_app_title == true ? map[listener.harness]?.display_name ?? listener.harness : listener.display_title ?? listener.title
            let cwd = compactHomePath(listener.cwd)
            let branch = (listener.branch ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            let branchSuffix = branch.isEmpty || branch == "HEAD" ? "" : " · \(branch)"
            let about = listener.display_about.flatMap { $0.isEmpty ? nil : $0 }
            let detail = cwd + branchSuffix + (about.map { " — \($0)" } ?? "")
            let tooltip = ([title, listener.cwd, listener.branch, about, listener.display_address] as [String?])
                .compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: "\n")
            let path = textLabel(detail, NSRect(x: titleX, y: stripHeight + 6, width: textWidth, height: 14),
                size: 10.5, color: CitoPalette.secondaryText)
            path.identifier = NSUserInterfaceItemIdentifier("session_detail:\(id)")
            path.maximumNumberOfLines = 1; path.lineBreakMode = .byTruncatingTail; path.toolTip = tooltip
            card.addSubview(path)

            if renamingSessionKey == id {
                let field = existingRenameField ?? NSTextField(string: renameDraft ?? listener.title)
                field.frame = NSRect(x: titleX, y: stripHeight + 20, width: textWidth, height: 16)
                field.identifier = NSUserInterfaceItemIdentifier("rename_field:\(id)")
                field.setAccessibilityLabel("Conversation title")
                field.delegate = self; field.font = .systemFont(ofSize: 12.5)
                if let draft = renameDraft, field.stringValue != draft { field.stringValue = draft }
                card.addSubview(field); renameField = field
                if existingRenameField == nil || renameSelection != nil {
                    RunLoop.main.perform(inModes: [.common, .eventTracking]) { [weak self, weak field] in
                        guard let self, let field, self.renameField === field, field.currentEditor() == nil else { return }
                        field.window?.makeFirstResponder(field)
                        if let selection = renameSelection { (field.currentEditor() as? NSTextView)?.setSelectedRange(selection) }
                        else { field.selectText(nil) }
                    }
                }
            } else {
                let indicators: [(kind: String, symbol: String, label: String, tint: NSColor)] =
                    (pinned ? [("pinned", "pin.fill", "Pinned", CitoPalette.secondaryText)] : []) +
                    (workspace ? [("workspace", "person.2.fill", "Visible to everyone", CitoPalette.infoText)] : [])
                let titleFont = NSFont.systemFont(ofSize: 12.5, weight: .semibold)
                let titleWidth = min(textWidth - CGFloat(indicators.count) * 14, measureTextWidth(title, font: titleFont) + 4)
                let label = textLabel(title, NSRect(x: titleX, y: stripHeight + 20, width: titleWidth, height: 16),
                    size: 12.5, weight: .semibold)
                label.identifier = NSUserInterfaceItemIdentifier("session_title:\(id)")
                label.maximumNumberOfLines = 1; label.lineBreakMode = .byTruncatingTail; label.toolTip = tooltip
                card.addSubview(label)
                for (index, indicator) in indicators.enumerated() {
                    let icon = NSImageView(frame: NSRect(x: label.frame.maxX + 4 + CGFloat(index) * 14,
                        y: stripHeight + 23, width: 10, height: 10))
                    icon.identifier = NSUserInterfaceItemIdentifier("session_\(indicator.kind):\(id)")
                    icon.image = NSImage(systemSymbolName: indicator.symbol, accessibilityDescription: indicator.label)
                    icon.contentTintColor = indicator.tint
                    icon.toolTip = indicator.label
                    card.addSubview(icon)
                }
            }
            if expanded {
                mountSessionActions(in: card, listener: listener, pinApplies: pinApplies, pinned: pinned,
                    workspaceSupported: workspaceSupported, activityTimed: map[listener.harness]?.idle_timeout_ms != nil)
            }
        }
        hostRows(activeRows, in: activeScroll, id: "active_scroll")
        if let revealSession, let activeScroll,
           let row = activeRows.first(where: { $0.id == "session:\(revealSession)" })?.view {
            // Restore the existing anchor first, then expose the entire changed row.
            let clip = activeScroll.contentView
            let viewport = clip.bounds
            let y = min(max(viewport.minY, row.frame.maxY - viewport.height), row.frame.minY)
            clip.scroll(to: NSPoint(x: viewport.minX, y: y))
            activeScroll.reflectScrolledClipView(clip)
        }
        if orderedListeners.isEmpty {
                let row = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 92))
                let card = roundedView(NSRect(x: CitoMetrics.menuInset, y: 3, width: CitoMetrics.menuRailWidth, height: 86), radius: 10, color: .labelColor.withAppearanceAlpha(0.045))
                let icon = NSImageView(frame: NSRect(x: 12, y: 22, width: 24, height: 24))
                icon.image = NSImage(systemSymbolName: "waveform.slash", accessibilityDescription: "No active listeners")
                icon.contentTintColor = CitoPalette.secondaryText; card.addSubview(icon)
                card.addSubview(textLabel("No agent sessions listening", NSRect(x: 44, y: 57, width: 304, height: 18), size: 13, weight: .semibold))
                let subtitle = !(settings?.apps.contains(where:{$0.isConfigured}) ?? true) ? "Connect your coding apps in Settings to start" : "Open a coding session in your app or configure Human Channels"
                let emptyHint = textLabel(subtitle, NSRect(x:44,y:32,width:304,height:26),size:10,color:CitoPalette.secondaryText)
                emptyHint.maximumNumberOfLines = 2; emptyHint.lineBreakMode = .byWordWrapping
                card.addSubview(emptyHint)
                let apps=MenuControl("Set Up Agent Apps…",frame:NSRect(x:44,y:8,width:148,height:22),radius:6)
                apps.identifier=NSUserInterfaceItemIdentifier("empty_open_setup")
                apps.pressed={ [weak self] in self?.cardMenu.cancelTracking(); self?.openSettingsPanel(tab: "apps") }
                card.addSubview(apps)
                let channels=MenuControl("Human Channels…",frame:NSRect(x:198,y:8,width:148,height:22),radius:6)
                channels.identifier=NSUserInterfaceItemIdentifier("empty_open_channels")
                channels.pressed={ [weak self] in self?.cardMenu.cancelTracking(); self?.openSettingsPanel(tab: "channels") }
                card.addSubview(channels)
                row.addSubview(card); _ = host(row, id: "empty")
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
                self.rebuild(preservingEditor: false)
            }
            remoteHeader.addSubview(toggleRemote)
            _ = host(remoteHeader, id: "my_remote_header")
            if !myRemoteCollapsed {
                var remoteRows: [(id: String, view: NSView)] = []
                let remoteScroll = myRemote.count > 3 ? makeMenuScroll(height: 102) : nil
                let remoteWidth = remoteScroll?.contentSize.width ?? CitoMetrics.menuWidth
                for peer in myRemote {
                    let row = NSView(frame: NSRect(x: 0, y: 0, width: remoteWidth, height: 34))
                    let rowId = "remote:\(peer.scoped_id)"
                    let hostIcon = NSImageView(frame: NSRect(x: 16, y: 10, width: 14, height: 14))
                    hostIcon.image = NSImage(systemSymbolName: "desktopcomputer", accessibilityDescription: "Remote host")
                    hostIcon.contentTintColor = CitoPalette.secondaryText
                    row.addSubview(hostIcon)
                    let copy = makeAddressCopyControl(peer.display_address, id: "copy_remote:\(peer.scoped_id)", authority: targetHub)
                    row.addSubview(copy)
                    let cleanHost = cleanHostDisplay(peer.host_name)
                    let hostText = cleanHost.isEmpty ? peer.host_name : cleanHost
                    let branch = (peer.branch ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
                    let branchSuffix = (!branch.isEmpty && branch != "HEAD") ? " · \(branch)" : ""
                    let displayTitle = peer.use_app_title == true ? map[peer.harness]?.display_name ?? peer.harness : peer.display_title ?? peer.title
                    let info = textLabel("\(hostText) · \(displayTitle)\(branchSuffix)",
                        NSRect(x: 36, y: 8, width: 0, height: 18), size: 12, weight: .regular, color: .labelColor)
                    info.lineBreakMode = .byTruncatingMiddle
                    info.toolTip = peer.display_address + (peer.display_about.map { " — \($0)" } ?? "")
                    row.addSubview(info)
                    layoutBar(in: row, insetLeft: 16, y: 7, height: 20, leading: [hostIcon], flexible: info, trailing: [copy])
                    remoteRows.append((id: rowId, view: row))
                }
                hostRows(remoteRows, in: remoteScroll, id: "my_remote_scroll")
            }
        }
        // Zone 3 — Available sessions on the network selected in the header.
        let visibleAvailable = hubs.count > 1 && !targetHub.isEmpty
            ? available.filter { !($0.listening_hubs ?? []).contains(targetHub) }
            : available
        if !visibleAvailable.isEmpty || !targetHub.isEmpty {
            let availHeader = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 26))
            availHeader.addSubview(roundedView(NSRect(x: 12, y: 24, width: 356, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
            let toggleAvailable = MenuControl("AVAILABLE SESSIONS · \(visibleAvailable.count)",
                frame: NSRect(x: 0, y: 0, width: 0, height: 22),
                leadingSymbol: availableCollapsed ? "chevron.right" : "chevron.down", color: CitoPalette.secondaryText, radius: 6)
            toggleAvailable.identifier = NSUserInterfaceItemIdentifier("toggle_available")
            toggleAvailable.style = .subtleHeader
            toggleAvailable.label.font = .systemFont(ofSize: 10, weight: .semibold)
            toggleAvailable.label.alignment = .left
            toggleAvailable.pressed = { [weak self] in
                guard let self = self else { return }
                self.availableCollapsed.toggle()
                self.rebuild(preservingEditor: false)
            }
            let autoListen = targetHubObj?.auto_listen ?? false
            let autoTitle = autoListen ? "Auto-Listen: On" : "Auto-Listen"
            let auto = MenuControl(autoTitle, frame: NSRect(x: 0, y: 0, width: 0, height: 18), radius: 9)
            auto.reservedTitles = [("Auto-Listen", nil), ("Auto-Listen: On", nil)]
            auto.identifier = NSUserInterfaceItemIdentifier("toggle_auto_listen")
            auto.style = autoListen ? .prominent : .neutral
            auto.toolTip = "Automatically listen to eligible sessions on \(targetHub). Turning Auto-Listen off stops unpinned auto-listened sessions and keeps pinned sessions listening."
            auto.isEnabled = !targetHub.isEmpty
            auto.pressed = { [weak self] in self?.emit(["op": "set_auto_listen", "hub": targetHub, "enabled": !autoListen]) }
            availHeader.addSubview(toggleAvailable)
            availHeader.addSubview(auto)
            layoutBar(in: availHeader, y: 1, height: 22, leading: [toggleAvailable], trailing: [auto])
            _ = host(availHeader, id: "available_header")
            let discoveryBusy = discovery.contains { $0.status == "scanning" || $0.status == "error" }
            for item in discovery where item.status == "scanning" || item.status == "error" {
                let visual = resolveHarnessVisual(item.harness,harnesses:map)
                let text = item.status == "scanning" ? "\(visual.name) · Scanning sessions…" : "\(visual.name) · " + (item.error ?? "Session discovery failed. Try refreshing.")
                let label = textLabel(text,NSRect(x:46,y:4,width:322,height:26),size:10.5,color:item.status == "error" ? CitoPalette.dangerText : CitoPalette.secondaryText)
                label.maximumNumberOfLines = 0; label.lineBreakMode = .byWordWrapping; label.cell?.wraps = true; label.toolTip = text
                label.frame.size.height = max(26, measureWrappedHeight(text, width: label.bounds.width, font: label.font!))
                let row = NSView(frame:NSRect(x:0,y:0,width:CitoMetrics.menuWidth,height:label.frame.height + 8))
                row.addSubview(label); _ = host(row,id:"discovery:\(item.harness)")
            }
            if !availableCollapsed && !discoveryBusy && visibleAvailable.isEmpty && !orderedListeners.isEmpty {
                let emptyTarget = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 28))
                let label = textLabel(autoListen ? "Auto-Listen is on — open sessions connect automatically" : "All sessions listening on \(targetHub)",
                    NSRect(x: 24, y: 6, width: 332, height: 16), size: 11.5, weight: .regular, color: CitoPalette.secondaryText)
                label.lineBreakMode = .byTruncatingMiddle; label.toolTip = label.stringValue
                emptyTarget.addSubview(label)
                _ = host(emptyTarget, id: "available_empty_target")
            }
            let reloadCount = visibleAvailable.count(where: { $0.needs_reload == true })
            if autoListen && reloadCount > 0 {
                let feedback = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 38))
                let sessions = reloadCount == 1 ? "1 session" : "\(reloadCount) sessions"
                let caption = textLabel("Run /reload in \(sessions) to finish connecting with Auto-Listen.", NSRect(x: 24, y: 4, width: 332, height: 30), size: 11, color: CitoPalette.secondaryText)
                caption.maximumNumberOfLines = 2
                caption.cell?.wraps = true
                feedback.addSubview(caption)
                _ = host(feedback, id: "auto_listen_reload_status")
            }
        }
        if !availableCollapsed && !visibleAvailable.isEmpty {
            let groupsByHarness = Dictionary(grouping: visibleAvailable, by: \.harness)
            let orderedHarnesses = groupsByHarness.keys.sorted { a, b in
                let va = resolveHarnessVisual(a, harnesses: map)
                let vb = resolveHarnessVisual(b, harnesses: map)
                return va.order != vb.order ? va.order < vb.order : a < b
            }
            func candidateIsOpen(_ candidate: Candidate) -> Bool {
                candidate.needs_reload == true || candidate.endpoint_pid != nil || map[candidate.harness]?.pid_bound == false
            }
            let groups = orderedHarnesses.map { harness in
                let key = "\(targetHub):\(harness)"
                return (harness: harness, items: groupsByHarness[harness]!, key: key, isCollapsed: collapsedGroups.contains(key),
                    visual: resolveHarnessVisual(harness, harnesses: map))
            }
            let groupHeight: CGFloat = 28
            let candidateBaseHeight: CGFloat = 44
            let maxAvailableHeight: CGFloat = 200
            let actionPadding: CGFloat = 7.5
            let reloadFont = NSFont.systemFont(ofSize: 10)
            let reloadWidth = MenuControl.intrinsicWidth("/reload", font: reloadFont, leadingSymbol: "doc.on.doc", paddingX: actionPadding)
            let listenWidth = MenuControl.intrinsicWidth("Listen", leadingSymbol: "plus", paddingX: actionPadding)
            let textInset = CitoMetrics.menuInset + max(reloadWidth, listenWidth) + 54
            func candidateHint(_ candidate: Candidate, harnessName: String) -> String? {
                guard candidate.needs_reload == true else { return nil }
                return candidate.reload_hint ?? "Run /reload in \(harnessName) to enable 1-click listen"
            }
            func reloadHintHeight(_ hint: String?, textWidth: CGFloat) -> CGFloat {
                hint.map { max(14, measureWrappedHeight($0, width: textWidth, font: reloadFont)) } ?? 0
            }
            // Measure the inline stack first. Only an overflowing stack reserves room for a scroller.
            var inlineHeight = CGFloat(groups.count) * groupHeight
            for group in groups where !group.isCollapsed {
                inlineHeight += group.items.reduce(CGFloat(0)) { height, candidate in
                    height + candidateBaseHeight + reloadHintHeight(candidateHint(candidate, harnessName: group.visual.name),
                        textWidth: CitoMetrics.menuWidth - textInset)
                }
            }
            let scroll = inlineHeight > maxAvailableHeight ? makeMenuScroll(height: maxAvailableHeight) : nil
            let candidateWidth = scroll?.contentSize.width ?? CitoMetrics.menuWidth
            let actionRight = candidateWidth - CitoMetrics.menuInset
            let textWidth = candidateWidth - textInset
            let wrapWidth = textWidth - (scroll.map(settingsScrollerAllowance) ?? 0)
            var availableRows: [(id: String, view: NSView)] = []
            for (harness, items, groupKey, isCollapsed, visual) in groups {
                let openCount = items.filter { candidateIsOpen($0) }.count
                let noun = openCount == items.count ? "open" : openCount == 0 ? "recent" : "sessions"
                let groupTitle = "\(visual.name) · \(items.count) \(noun)"
                let groupRow = NSView(frame: NSRect(x: 0, y: 0, width: candidateWidth, height: groupHeight))
                let groupControl = MenuControl(groupTitle, frame: NSRect(x: CitoMetrics.menuInset, y: 2, width: candidateWidth - 2 * CitoMetrics.menuInset, height: 24), radius: 6)
                groupControl.identifier = NSUserInterfaceItemIdentifier("toggle_group:\(harness)")
                groupControl.toolTip = visual.tier == "experimental" ? "\(groupTitle) (Experimental)" : groupTitle
                groupControl.setAccessibilityLabel(groupControl.toolTip)
                groupControl.style = .subtleHeader
                groupControl.symbolName = isCollapsed ? "chevron.right" : "chevron.down"
                let chev = NSImageView(frame: NSRect(x: 2, y: 8, width: 8, height: 8))
                chev.image = NSImage(systemSymbolName: isCollapsed ? "chevron.right" : "chevron.down", accessibilityDescription: nil)
                chev.contentTintColor = CitoPalette.secondaryText
                groupControl.addSubview(chev)
                groupControl.accessorySymbolView = chev
                let badge = makeHarnessBadge(NSRect(x: 14, y: 3, width: 18, height: 18),
                    radius: 5, iconInset: 3, harness: harness, harnesses: map)
                badge.toolTip = nil
                for child in badge.subviews { child.toolTip = nil }
                groupControl.addSubview(badge)
                groupControl.label.frame = NSRect(x: 34, y: 4, width: groupControl.bounds.width - 46, height: 16)
                groupControl.label.font = .systemFont(ofSize: 11.5, weight: .semibold)
                groupControl.label.alignment = .left
                groupControl.label.lineBreakMode = .byTruncatingMiddle
                groupControl.pressed = { [weak self] in
                    guard let self = self else { return }
                    if self.collapsedGroups.contains(groupKey) { self.collapsedGroups.remove(groupKey) }
                    else { self.collapsedGroups.insert(groupKey) }
                    self.rebuild(preservingEditor: false)
                }
                groupRow.addSubview(groupControl)
                availableRows.append((id: "group:\(harness)", view: groupRow))
                if !isCollapsed {
                    for c in items {
                        let hintText = candidateHint(c, harnessName: visual.name)
                        let hintHeight = reloadHintHeight(hintText, textWidth: wrapWidth)
                        let rowHeight = candidateBaseHeight + hintHeight
                        let row = NSView(frame: NSRect(x: 0, y: 0, width: candidateWidth, height: rowHeight))
                        let shortCwd = compactHomePath(c.cwd)
                        let branch = (c.branch ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
                        let branchSuffix = (!branch.isEmpty && branch != "HEAD") ? " · \(branch)" : ""
                        let candidateTitle = c.generic_title == true || c.title_fallback_hubs?.contains(targetHub) == true ? visual.name : c.title
                        let info = textLabel(candidateTitle, NSRect(x:46,y:rowHeight-21,width:textWidth,height:16),size:12,weight:.medium,color:.labelColor)
                        info.lineBreakMode = .byTruncatingTail
                        let location = textLabel(compactSessionPath(shortCwd, branchSuffix: branchSuffix, maxWidth:textWidth),NSRect(x:46,y:hintText != nil ? hintHeight+7 : 6,width:textWidth,height:14),size:10.5,color:CitoPalette.secondaryText)
                        location.lineBreakMode = .byTruncatingMiddle
                        location.toolTip = "\(shortCwd)\(branchSuffix)"
                        location.identifier = NSUserInterfaceItemIdentifier("candidate_cwd:\(c.harness):\(c.session_id)")
                        row.addSubview(location)
                        info.toolTip = "\(candidateTitle) — \(c.cwd)"
                        info.identifier=NSUserInterfaceItemIdentifier("candidate_title:\(c.harness):\(c.session_id)")
                        row.addSubview(info)
                        availableRows.append((id: "candidate:\(c.harness):\(c.session_id)", view: row))
                        if let hintText {
                            let hint=textLabel(hintText,NSRect(x:46,y:5,width:textWidth,height:hintHeight),size:10,color:CitoPalette.warningText)
                            hint.maximumNumberOfLines = 0; hint.lineBreakMode = .byWordWrapping; hint.cell?.wraps = true
                            hint.toolTip=hint.stringValue
                            hint.identifier=NSUserInterfaceItemIdentifier("candidate_hint:\(c.harness):\(c.session_id)")
                            row.addSubview(hint)
                            let reload = MenuControl("/reload", frame: NSRect(x: actionRight - reloadWidth, y: floor((rowHeight-22)/2), width: reloadWidth, height: 22), leadingSymbol: "doc.on.doc", paddingX: actionPadding)
                            reload.label.font = reloadFont; reload.layoutContents()
                            let reloadID = "reload:\(c.harness):\(c.session_id)"
                            let copyKey = TransientFeedbackStore.Key(.menuCopy, reloadID, authority: targetHub)
                            reload.identifier = NSUserInterfaceItemIdentifier(reloadID)
                            reload.toolTip = "Copy /reload to paste in \(visual.name)"
                            reload.showFeedback(key: copyKey)
                            reload.pressed = { [weak self, weak reload] in
                                guard let self else { return }
                                self.copyText("/reload", control: reload, key: copyKey)
                                self.emit(["op":"refresh_available"])
                            }
                            row.addSubview(reload)
                            continue
                        }
                        let listenBtn = MenuControl("Listen", frame: NSRect(x: actionRight - listenWidth, y: floor((rowHeight-22)/2), width: listenWidth, height: 22), leadingSymbol: "plus", paddingX: actionPadding)
                        listenBtn.identifier = NSUserInterfaceItemIdentifier("listen:\(c.harness):\(c.session_id)")
                        listenBtn.isEnabled = !targetHub.isEmpty && targetHubObj?.status != "auth_required" && candidateIsOpen(c)
                        if targetHubObj?.status == "auth_required" {
                            listenBtn.toolTip = "Sign in to \(targetHub) to listen"
                        } else if !candidateIsOpen(c) {
                            listenBtn.toolTip = "Open this session to listen"
                        } else {
                            listenBtn.toolTip = "Start listening in \(candidateTitle)"
                        }
                        if isPending("listen_session", hub: targetHub, target: "\(c.harness):\(c.session_id)") { listenBtn.setPending("Listening…") }
                        info.frame.size.width = min(textWidth, listenBtn.frame.minX - info.frame.minX - 8)
                        location.frame.size.width = info.frame.width
                        let listenRun = { [weak self, weak listenBtn] in
                            guard let self = self else { return }
                            listenBtn?.setPending("Listening…")
                            var action: [String: Any] = ["op": "listen_session", "harness": c.harness, "session_id": c.session_id, "cwd": c.cwd, "title": c.custom_title == true ? c.title : candidateTitle]
                            if let pid = c.endpoint_pid { action["endpoint_pid"] = pid }
                            action["hub"] = targetHub
                            self.emit(action)
                        }
                        listenBtn.pressed = listenRun; row.addSubview(listenBtn)
                    }
                }
            }
            hostRows(availableRows, in: scroll, id: "available_scroll")
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
                control.toolTip = tip + " · " + trailing
                if mode == "activity" && unreadCount > 0 { control.normalColor=CitoPalette.infoText }
                control.label.font = .systemFont(ofSize: 11, weight: .medium)
                control.label.alignment = .left
                control.layoutContents()
                control.label.frame = NSRect(x: 23, y: 6, width: 76, height: 16)
                let badge = textLabel(trailing, NSRect(x: 100, y: 6, width: 68, height: 16), size: 10, color: mode == "activity" && unreadCount > 0 ? CitoPalette.infoText : CitoPalette.secondaryText)
                badge.alignment = .right; badge.lineBreakMode = .byTruncatingMiddle
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
        if !orderedListeners.isEmpty {
            let stopAll = MenuControl("Stop All", frame: NSRect(x: 12, y: 6, width: MenuControl.intrinsicWidth("Stop All",leadingSymbol:"stop.fill"), height: 24), leadingSymbol: "stop.fill")
            stopAll.identifier = NSUserInterfaceItemIdentifier("stop_all")
            stopAll.reservedTitles = [("Stop All", "stop.fill"), ("Stopping…", nil)]
            stopAll.style = .subtleHeader
            stopAll.toolTip = "Stop all agent sessions on \(targetHub) and disable Auto-Listen"
            if isPending("unlisten_all", hub: targetHub) { stopAll.setPending("Stopping…") }
            stopAll.pressed = { self.emit(["op": "unlisten_all", "hub":targetHub]) }
            footer.addSubview(stopAll)
        }
        let openSettings = MenuControl("Settings…", frame: NSRect(x: 168, y: 6, width: MenuControl.intrinsicWidth("Settings…", leadingSymbol: "gearshape", paddingX: 6), height: 24), leadingSymbol: "gearshape", paddingX: 6)
        openSettings.identifier = NSUserInterfaceItemIdentifier("open_settings")
        openSettings.style = .subtleHeader
        openSettings.toolTip = "Settings… (⌘,)"
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
        let stopAllControls = footer.subviews.filter { $0.identifier?.rawValue == "stop_all" }
        layoutBar(in: footer, y: 6, height: 24, leading: stopAllControls + [openSettings], trailing: [quit])
        let footerItem = host(footer, id: "footer")
        footerItem.target = self; footerItem.action = #selector(quitMenu)
        footerItem.keyEquivalent = "q"; footerItem.keyEquivalentModifierMask = [.command]
        let directoryShortcut = NSMenuItem(title:"Directory…",action:#selector(openDirectoryShortcut),keyEquivalent:"k")
        directoryShortcut.target = self; directoryShortcut.keyEquivalentModifierMask = [.command]
        directoryShortcut.isHidden = true; directoryShortcut.allowsKeyEquivalentWhenHidden = true
        menu.addItem(directoryShortcut)
    }
    @objc func openDirectoryShortcut() {
        cardMenu.cancelTracking()
        if activeUninstallAlert == nil { openDirectoryPanel(mode:"directory") }
    }
    @objc func openSettingsShortcut() { cardMenu.cancelTracking(); openSettingsPanel() }
    @objc func quitMenu() { emit(["op": "quit"]) }
}
