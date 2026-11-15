import AppKit

extension Companion: NSMenuDelegate {
    var uniqueSessionCount: Int {
        Dictionary(grouping: listeners.filter { $0.harness != "human" && (effectiveTargetHub.isEmpty || $0.hub == effectiveTargetHub) }, by: { $0.harness })
            .values.reduce(0) { $0 + Set($1.map { $0.session_id }).count }
    }
    var humanListening: Bool { hubs.first { $0.name == effectiveTargetHub }?.human_availability == "ACTIVE" }

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

    func updateMenuErrorRow() {
        if let item = cardMenu.items.first(where: { $0.view?.identifier?.rawValue == "menu_error_row" }) {
            cardMenu.removeItem(item)
        }
        guard let error = menuBannerError else { return }
        let row = NSView(frame:NSRect(x:0,y:0,width:380,height:36))
        row.identifier = NSUserInterfaceItemIdentifier("menu_error_row")
        row.appearance = testAppearance
        let label = textLabel(presentationError(error),NSRect(x:12,y:3,width:320,height:30),size:10,color:CitoPalette.dangerText)
        label.maximumNumberOfLines=2; label.lineBreakMode = .byTruncatingMiddle; label.toolTip=label.stringValue
        row.addSubview(label)
        let dismiss = MenuControl("", frame:NSRect(x:340,y:7,width:26,height:22), symbol:"xmark")
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
        let hadRename = renamingSessionKey != nil
        renamingSessionKey = nil
        if hadRename {
            renameField?.window?.makeFirstResponder(nil)
            renameField?.removeFromSuperview()
            renameField = nil
            menuRebuildDeferred = false
            menuDirty = true
        }
        emit(["op": "close_menu"], userInitiated: false)
        MenuControl.clearHoverWhere { $0.enclosingMenuItem != nil || ($0.window !== self.directoryPanel && $0.window !== self.settingsPanel) }
    }

    var statusTooltip: String {
        let attention=hubs.contains { $0.status != "connected" || $0.error != nil }
        let count=uniqueSessionCount
        var detail=count > 0 ? "\(count) session\(count == 1 ? "" : "s") listening" : (humanListening ? "Personal inbox active" : (hubs.first { $0.name == effectiveTargetHub }?.human_paused == true ? "Inbox paused" : "Not listening"))
        if unreadCount > 0 { detail += " · \(unreadCount) unread" }
        if attention { detail="Hub connection needs attention" }
        return "Cito · " + detail
    }

    func updateStatusItem() {
        let warning = hubs.contains { $0.status != "connected" || $0.error != nil }
        let icon: StatusIconState = warning ? .warning : (listeners.contains { $0.harness != "human" } || unreadCount > 0) ? .active : .idle
        let tooltip = statusTooltip
        guard lastStatusPresentation?.icon != icon || lastStatusPresentation?.unread != unreadCount || lastStatusPresentation?.tooltip != tooltip,
              let button = status?.button ?? headlessStatusButton else { return }
        statusImage = Self.statusIcons[icon]
        formatStatusButton(button, unreadCount: unreadCount, image: statusImage)
        button.toolTip = tooltip
        button.setAccessibilityLabel(tooltip)
        lastStatusPresentation = (icon, unreadCount, tooltip)
        statusUpdateCount += 1
    }
    func scopeToggle(hub: String, user: String, scope: String, id: String, y: CGFloat) -> MenuControl? {
        guard hubs.first(where: { $0.name == hub })?.supports_workspace_scope == true,
              listeners.first(where: { $0.hub == hub && ($0.scoped_id == id || $0.listener_id == id) })?.can_set_scope == true else { return nil }
        let team = scope == "workspace"
        let control = MenuControl(team ? "Team" : "Self", frame:NSRect(x:0,y:y,width:68,height:22), leadingSymbol:team ? "person.2.fill" : "lock.fill")
        control.identifier = NSUserInterfaceItemIdentifier("scope:\(id)")
        updateScopeControl(control, scope:scope, hub:hub, user:user)
        return control
    }

    func updateScopeControl(_ control: MenuControl, scope: String, hub: String, user: String) {
        let team = scope == "workspace"
        control.label.stringValue = team ? "Team" : "Self"
        control.setLeadingSymbol(team ? "person.2.fill" : "lock.fill")
        control.setAccessibilityLabel(control.label.stringValue)
        control.style = team ? .prominent : .neutral
        control.toolTip = team
            ? "Visibility: Team — anyone connected to '\(hub)' can discover and message this node. Click to restrict to Only Me."
            : "Visibility: Only Me (@\(user)) — only your own sessions and devices on '\(hub)' can message this node. Click to share with Team."
    }

    func rebuild() {
        updateStatusItem()
        menuDirty = true
        guard menuOpen || renderingForInspection else { return }
        let count = uniqueSessionCount
        if renamingSessionKey != nil, renameField?.currentEditor() != nil {
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
        let humanHub = hubs.first { $0.name == effectiveTargetHub }
        if humanHub?.status == "auth_required" { subtitleText = "Sign in required" }
        else if ["unreachable","error","disconnected"].contains(humanHub?.status ?? "") { subtitleText = "Hub unavailable" }
        else if humanHub?.status != "connected" { subtitleText = "Connecting…" }
        else if count > 0 { subtitleText = "\(count) session\(count == 1 ? "" : "s") listening" }
        else if humanHub?.human_paused == true { subtitleText = "Inbox paused" }
        else if humanListening { subtitleText = "Personal inbox active" }
        else { subtitleText = "Not listening" }
        let totalHubPillsWidth = hubs.reduce(CGFloat(0)) { $0 + min(180, measureTextWidth($1.name, font: btnFont) + 26) }
            + CGFloat(max(0, hubs.count - 1)) * 4
        let subtitleLimit: CGFloat = hubs.count > 1 ? min(246, 368 - 54 - min(144, totalHubPillsWidth)) : 246
        let subWidth = min(subtitleLimit, ceil(measureTextWidth(subtitleText, font: .systemFont(ofSize: 11.5))) + 8)
        let subLabel = textLabel(subtitleText, NSRect(x: 50, y: 12, width: subWidth, height: 17), size: 11.5, color: CitoPalette.secondaryText)
        subLabel.identifier = NSUserInterfaceItemIdentifier("header_subtitle")
        subLabel.lineBreakMode = .byTruncatingMiddle; subLabel.toolTip = subtitleText
        header.addSubview(subLabel)
        _ = host(header, id: "header")
        updateMenuErrorRow()
        var drawerItems: [String: NSMenuItem] = [:]
        let showHubControls = hubs.count != 1 || hubs.contains {
            !($0.is_builtin == true && $0.status == "connected" && $0.error == nil)
        }
        for hub in hubs where showHubControls {
            let drawer = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 32))
            let loginRun = { self.emit(["op": "hub_action", "hub": hub.name, "action": hub.status == "connected" ? "switch_account" : "login"]) }
            let reconnectRun = { self.emit(["op": "hub_action", "hub": hub.name, "action": "reconnect"]) }
            let disconnectRun = { self.cardMenu.cancelTracking(); self.settingsActiveTab="hubs"; self.openSettingsPanel() }
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
            let detail = textLabel(statusText, NSRect(x: 12, y: 8, width: 40, height: 16),
                color: hub.error != nil || hub.status == "auth_required" ? .labelColor : CitoPalette.secondaryText)
            detail.lineBreakMode = .byTruncatingMiddle
            detail.toolTip = statusText; drawer.addSubview(detail)
            layoutBar(in: drawer, y: 5, height: 22, spacing: 4, flexible: detail, trailing: drawerControls)
            if hub.error != nil || measureTextWidth(statusText,font:detail.font ?? .systemFont(ofSize:11)) > detail.frame.width {
                detail.identifier = NSUserInterfaceItemIdentifier("hub_error:\(hub.name)")
                detail.maximumNumberOfLines = 0; detail.lineBreakMode = .byWordWrapping
                let width = CitoMetrics.menuWidth - 24
                let height = ceil((statusText as NSString).boundingRect(with: NSSize(width: width, height: .greatestFiniteMagnitude), options: [.usesLineFragmentOrigin, .usesFontLeading], attributes: [.font: detail.font ?? NSFont.systemFont(ofSize: 11)]).height) + 4
                detail.frame = NSRect(x: 12, y: 32, width: width, height: height)
                drawer.frame.size.height = height + 40
                layoutBar(in: drawer, y: 5, height: 22, spacing: 4, trailing: drawerControls)
            } else {
                layoutBar(in: drawer, y: 5, height: 22, spacing: 4, flexible: detail, trailing: drawerControls)
            }
            let item = host(drawer, id: "drawer:\(hub.name)")
            let autoExpand = (hub.status != "connected" && hub.status != "connecting") || hub.error != nil
            item.isHidden = !(hubDrawerVisibility[hub.name] ?? autoExpand)
            drawerItems[hub.name] = item
        }
        let stripX = max(138, 54 + subWidth)
        let stripWidth = max(44, 368 - stripX)
        var hubPills: [NSView] = []
        for hub in hubs where showHubControls {
            let color = hub.name == effectiveTargetHub && hub.status == "connected" && hub.error == nil ? NSColor.white : statusColor(hub.status)
            let isSingleDefault = hubs.count == 1 && hub.is_builtin == true
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
                pill.label.lineBreakMode = .byTruncatingMiddle; pill.toolTip = title
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
        let targetHubObj = hubs.first { $0.name == targetHub }
        let targetUser = targetHubObj?.user ?? "local"
        if targetHubObj?.human_paused == true {
            let notice = MenuControl("Inbox paused · Resume in Settings…", frame:NSRect(x:12,y:3,width:356,height:24), leadingSymbol:"pause.circle", radius:6)
            notice.identifier = NSUserInterfaceItemIdentifier("inbox_paused_notice")
            notice.style = .subtleHeader
            notice.toolTip = "Inbox alerts are paused on \(targetHub). Open Human Channels in Settings to resume."
            notice.setAccessibilityLabel(notice.toolTip)
            notice.pressed = { [weak self, weak menu] in
                guard let self = self else { return }
                menu?.cancelTracking(); self.selectedChannelHub = targetHub; self.settingsActiveTab = "channels"; self.openSettingsPanel()
            }
            let row = NSView(frame:NSRect(x:0,y:0,width:380,height:30)); row.addSubview(notice)
            _ = host(row,id:"inbox_notice_row")
        }
        let agentHeader=NSView(frame:NSRect(x:0,y:0,width:380,height:22))
        agentHeader.addSubview(textLabel("LISTENING SESSIONS",NSRect(x:16,y:3,width:344,height:16),size:10,weight:.semibold,color:CitoPalette.secondaryText))
        _ = host(agentHeader,id:"listening_sessions_header")
        let orderedListeners = listeners.filter { $0.harness != "human" }
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
            copy.toolTip = short; copy.identifier = NSUserInterfaceItemIdentifier("copy:\(listener.scoped_id)")
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
            pathText = (!branch.isEmpty && branch != "HEAD") ? "\(cwd) · \(branch)" : cwd
            let path = textLabel(pathText,
                NSRect(x: titleX + copyWidth + 8, y: 8, width: max(0, card.bounds.width - CitoMetrics.cardInset - titleX - copyWidth - 8), height: 16), size: 11, color: CitoPalette.secondaryText)
            path.lineBreakMode = .byTruncatingMiddle; path.toolTip = pathText; path.isHidden = pathText.isEmpty; card.addSubview(path)
            if listener.can_pin == true && (hubs.first(where: { $0.name == listener.hub })?.auto_listen == true || listener.listen_mode == "auto") {
                let pinned = listener.listen_mode != "auto"
                let activityTimed = map[listener.harness]?.idle_timeout_ms != nil
                let pin = MenuControl("Pin", frame:NSRect(x:0,y:cardHeight-28,width:22,height:22), symbol:pinned ? "pin.fill" : "pin")
                pin.identifier = NSUserInterfaceItemIdentifier("pin:\(listener.scoped_id)")
                pin.style = pinned ? .neutral : .subtleHeader
                pin.normalColor = pinned ? CitoPalette.infoText : CitoPalette.secondaryText
                let lifetime = activityTimed ? "Unpinned sessions auto-retire after the idle timeout." : "Unpinned sessions stay listening while their endpoint is available; no idle timeout applies."
                pin.toolTip = (pinned ? "Pinned. Unpin to follow Auto-Listen. " : "Pin this session. ") + lifetime + " Pinned sessions stay listening when Auto-Listen is turned off."
                pin.setAccessibilityLabel(pinned ? "Unpin session" : "Pin session")
                pin.pressed = { [weak self] in self?.emit(["op":"set_listen_mode","hub":listener.hub,"listener_id":listener.listener_id,"listen_mode":pinned ? "auto" : "manual"]) }
                card.addSubview(pin); sessionActions.append(pin)
            }
            if let scopeBtn = scopeToggle(hub:listener.hub,user:user,scope:listener.scope,id:listener.scoped_id,y:cardHeight-28) {
                var scope = listener.scope
                scopeBtn.pressed = { [weak self, weak scopeBtn] in
                    guard let self = self, let scopeBtn = scopeBtn else { return }
                    self.pendingMutation = true
                    scope = scope == "self" ? "workspace" : "self"
                    self.updateScopeControl(scopeBtn, scope:scope, hub:listener.hub, user:user)
                    self.emit(["op":"set_scope","hub":listener.hub,"listener_id":listener.listener_id,"scope":scope])
                }
                card.addSubview(scopeBtn); sessionActions.append(scopeBtn)
            }
            let stop = MenuControl("Stop", frame: NSRect(x: 0, y: cardHeight - 28, width: 48, height: 22))
            stop.toolTip = "Stop only this session and keep it stopped. Auto-Listen on \(listener.hub) stays unchanged."
            stop.style = .neutral; stop.maximumWidth = 48
            stop.identifier = NSUserInterfaceItemIdentifier("stop:\(listener.scoped_id)")
            let unlisten = { self.emit(["op": "unlisten_session", "hub": listener.hub, "harness": listener.harness, "session_id": listener.session_id]) }
            stop.isHidden = listener.can_stop != true
            stop.pressed = unlisten; card.addSubview(stop); sessionActions.append(stop)
            layoutBar(in: card, insetLeft: titleX, y: cardHeight - 28, height: 22, spacing: 4, trailing: sessionActions)
            let rightEdge = sessionActions.filter { !$0.isHidden }.map(\.frame.minX).min() ?? (card.bounds.width - CitoMetrics.cardInset)
            if renamingSessionKey == sessionKey {
                title.isHidden = true
                let field = existingRenameField ?? NSTextField(string: renameDraft ?? listener.title)
                field.frame = NSRect(x: titleX, y: cardHeight - 27, width: max(80, rightEdge - titleX - 6), height: 20)
                field.identifier = NSUserInterfaceItemIdentifier("rename_field:\(listener.scoped_id)")
                field.setAccessibilityLabel("Session title")
                field.delegate = self; field.font = .systemFont(ofSize: 13)
                if let draft = renameDraft, field.stringValue != draft { field.stringValue = draft }
                card.addSubview(field); renameField = field
                if existingRenameField == nil || renameSelection != nil {
                    RunLoop.main.perform(inModes: [.common, .eventTracking]) { [weak self, weak field] in
                        guard let self = self, let field = field, self.renameField === field, field.currentEditor() == nil else { return }
                        field.window?.makeFirstResponder(field)
                        if let selection = renameSelection { (field.currentEditor() as? NSTextView)?.setSelectedRange(selection) }
                        else { field.selectText(nil) }
                    }
                }
            } else {
                let titleW = max(122, min(rightEdge - titleX - 28, ceil(measureTextWidth(displayTitle, font: .systemFont(ofSize: 13, weight: .semibold))) + 4))
                title.frame = NSRect(x: titleX, y: cardHeight - 26, width: titleW, height: 18)
                let renameBtn = MenuControl("Rename session", frame: NSRect(x: titleX + titleW + 6, y: cardHeight - 26, width: 18, height: 18), symbol: "pencil")
                renameBtn.identifier = NSUserInterfaceItemIdentifier("rename:\(listener.scoped_id)")
                renameBtn.style = .subtleHeader; renameBtn.toolTip = "Rename session"
                renameBtn.pressed = { [weak self] in self?.renameField = nil; self?.renamingSessionKey = sessionKey; self?.rebuild() }
                renameBtn.isHidden = listener.can_rename != true
                card.addSubview(renameBtn)
            }
            layoutBar(in: card, insetLeft: titleX, insetRight: CitoMetrics.cardInset, y: 7, height: 19, leading: [copy], flexible: path)
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
                apps.pressed={ [weak self] in self?.cardMenu.cancelTracking(); self?.settingsActiveTab="apps"; self?.openSettingsPanel() }
                card.addSubview(apps)
                let channels=MenuControl("Human Channels…",frame:NSRect(x:198,y:8,width:148,height:22),radius:6)
                channels.identifier=NSUserInterfaceItemIdentifier("empty_open_channels")
                channels.pressed={ [weak self] in self?.cardMenu.cancelTracking(); self?.settingsActiveTab="channels"; self?.openSettingsPanel() }
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
                    copy.toolTip = short
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
                    info.toolTip = peer.display_address + (distinctAbout(peer.about, title:peer.title, user:peer.user, otherTitle:displayTitle).map { " — \($0)" } ?? "")
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
            let autoWidth = MenuControl.toggleWidth("toggle_auto_listen",title:autoTitle)
            let auto = MenuControl(autoTitle, frame: NSRect(x: 368 - autoWidth, y: 2, width: autoWidth, height: 18), radius: 9)
            auto.identifier = NSUserInterfaceItemIdentifier("toggle_auto_listen")
            auto.style = autoListen ? .prominent : .neutral
            auto.toolTip = "Automatically listen to eligible sessions on \(targetHub). Turning Auto-Listen off stops unpinned auto-listened sessions and keeps pinned sessions listening."
            auto.isEnabled = !targetHub.isEmpty
            auto.pressed = { [weak self] in self?.emit(["op": "set_auto_listen", "hub": targetHub, "enabled": !autoListen]) }
            availHeader.addSubview(auto)
            _ = host(availHeader, id: "available_header")
            let visibleAvailable = hubs.count > 1 && !targetHub.isEmpty
                ? available.filter { !($0.listening_hubs ?? []).contains(targetHub) }
                : available
            let discoveryBusy = discovery.contains { $0.status == "scanning" || $0.status == "error" }
            for item in discovery where item.status == "scanning" || item.status == "error" {
                let visual = resolveHarnessVisual(item.harness,harnesses:map)
                let text = item.status == "scanning" ? "\(visual.name) · Scanning sessions…" : "\(visual.name) · " + presentationError(item.error ?? "Session discovery failed. Try refreshing.")
                let label = textLabel(text,NSRect(x:46,y:4,width:322,height:26),size:10.5,color:item.status == "error" ? CitoPalette.dangerText : CitoPalette.secondaryText)
                label.maximumNumberOfLines = 0; label.lineBreakMode = .byWordWrapping; label.cell?.wraps = true; label.toolTip = text
                label.frame.size.height = max(26, ceil((text as NSString).boundingRect(with:NSSize(width:322,height:CGFloat.greatestFiniteMagnitude),options:[.usesLineFragmentOrigin],attributes:[.font:label.font!]).height) + 4)
                let row = NSView(frame:NSRect(x:0,y:0,width:CitoMetrics.menuWidth,height:label.frame.height + 8))
                row.addSubview(label); _ = host(row,id:"discovery:\(item.harness)")
            }
            if !discoveryBusy && visibleAvailable.isEmpty && listeners.contains(where: { $0.harness != "human" }) {
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
            func candidateIsOpen(_ candidate: Candidate) -> Bool {
                candidate.needs_reload == true || candidate.endpoint_pid != nil || map[candidate.harness]?.pid_bound == false
            }
            let groupedCandidates = orderedHarnesses.map { (harness: $0, items: groupsByHarness[$0]!) }
            for (harness, items) in groupedCandidates {
                let recentKey = "\(targetHub):\(harness)"
                if autoListen && items.allSatisfy({ !candidateIsOpen($0) }) && autoCollapsedRecentGroups.insert(recentKey).inserted {
                    collapsedGroups.insert(recentKey)
                }
            }
            let expandedGroupCount = groupedCandidates.filter { !collapsedGroups.contains("\(targetHub):\($0.harness)") }.count
            for (harness, items) in groupedCandidates {
                let groupKey = "\(targetHub):\(harness)"
                let isCollapsed = collapsedGroups.contains(groupKey)
                let visual = resolveHarnessVisual(harness, harnesses: map)
                let openCount = items.filter { candidateIsOpen($0) }.count
                let noun = openCount == items.count ? "open" : openCount == 0 ? "recent" : "sessions"
                let groupTitle = "\(visual.name) · \(items.count) \(noun)"
                let groupRow = NSView(frame: NSRect(x: 0, y: 0, width: CitoMetrics.menuWidth, height: 28))
                let groupControl = MenuControl(groupTitle, frame: NSRect(x: CitoMetrics.menuInset, y: 2, width: CitoMetrics.menuRailWidth, height: 24), radius: 6)
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
                groupControl.label.frame = NSRect(x: 34, y: 4, width: 310, height: 16)
                groupControl.label.font = .systemFont(ofSize: 11.5, weight: .semibold)
                groupControl.label.alignment = .left
                groupControl.label.lineBreakMode = .byTruncatingMiddle
                groupControl.pressed = { [weak self] in
                    guard let self = self else { return }
                    if self.collapsedGroups.contains(groupKey) { self.collapsedGroups.remove(groupKey) }
                    else { self.collapsedGroups.insert(groupKey) }
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
            stopAll.style = .subtleHeader
            stopAll.toolTip = "Stop all agent sessions on \(targetHub) and disable Auto-Listen"
            pendingLabel(stopAll, isPending("unlisten_all", hub: targetHub), "Stopping…")
            stopAll.pressed = { self.emit(["op": "unlisten_all", "hub":targetHub]) }
            footer.addSubview(stopAll)
        }
        let openSettings = MenuControl("Settings…", frame: NSRect(x: 168, y: 6, width: MenuControl.intrinsicWidth("Settings…", leadingSymbol: "gearshape", paddingX: 6), height: 24), leadingSymbol: "gearshape")
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
    @objc func openDirectoryShortcut() { cardMenu.cancelTracking(); openDirectoryPanel(mode:"directory") }
    @objc func openSettingsShortcut() { cardMenu.cancelTracking(); openSettingsPanel() }
    @objc func quitMenu() { emit(["op": "quit"]) }
}
