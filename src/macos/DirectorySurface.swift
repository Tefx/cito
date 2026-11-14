import AppKit

extension Companion: NSTableViewDataSource, NSTableViewDelegate {
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

    func updateDirectoryWarnings() {
        let hasError = directoryError != nil
        let hasWarning = hasError || !directoryWarnings.isEmpty
        directoryWarningLabel?.isHidden = !hasWarning
        directoryWarningLabel?.stringValue = hasError ? (directoryMode == "activity" ? "Cannot update Activity. Please try again." : "Cannot refresh Directory. Showing the last available results.") : "Some Hubs are unavailable. Results may be incomplete."
        // Directory warnings carry a Hub label added by the daemon. Keep that
        // scope while sanitizing only its diagnostic payload.
        directoryWarningLabel?.toolTip = directoryError.map(presentationError) ?? directoryWarnings.map { warning in
            guard let separator = warning.range(of:": ") else { return presentationError(warning) }
            return String(warning[..<separator.lowerBound]) + ": " + presentationError(String(warning[separator.upperBound...]))
        }.joined(separator:"\n")
        directoryWarningLabel?.textColor = hasError ? CitoPalette.dangerText : CitoPalette.secondaryText
        let bottom: CGFloat = directoryComposerBar?.isHidden == false ? 28 + (directoryComposerBar?.frame.height ?? 44) : 28
        directoryScrollView?.frame = NSRect(x:0,y:bottom,width:560,height:362-bottom-(hasWarning ? 28 : 0))
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

    func layoutComposer() {
        guard let bar=directoryComposerBar,let target=composerTargetPill,let input=composerInputField,let send=composerSendButton,let cancel=bar.subviews.first(where:{$0.identifier?.rawValue == "composer_cancel"}) as? MenuControl else{return}
        cancel.label.stringValue=composerReplySignalId == nil ? "Close" : "Cancel"
        cancel.frame.size.width=MenuControl.intrinsicWidth(cancel.label.stringValue)
        cancel.toolTip=composerReplySignalId == nil ? "Close draft (Esc)" : "Cancel reply (Esc)"
        layoutBar(in:bar,insetLeft:16,insetRight:12,y:composerError.isEmpty ? 10 : 26,height:24,spacing:6,leading:[target],flexible:input,trailing:[send,cancel])
        composerErrorLabel?.frame=NSRect(x:16,y:5,width:528,height:15)
    }
    func setComposerStatus(_ message: String) {
        composerError = presentationError(message)
        composerErrorLabel?.stringValue = composerError
        composerErrorLabel?.toolTip = composerError
        composerErrorLabel?.isHidden = composerError.isEmpty
        directoryComposerBar?.frame.size.height = composerError.isEmpty ? 44 : 62
        directoryComposerBar?.subviews.first { $0.identifier?.rawValue == "composer_divider" }?.frame.origin.y = (directoryComposerBar?.frame.height ?? 44)-1
        layoutComposer()
        updateDirectoryWarnings()
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
        let facetSeparator = roundedView(NSRect(x: 151, y: 9, width: 1, height: 14), radius: 0, color: .separatorColor)
        facetSeparator.identifier = NSUserInterfaceItemIdentifier("directory_facet_separator")
        facetBar.addSubview(facetSeparator)
        let scopeTrack = roundedView(NSRect(x:16,y:3,width:129,height:26),radius:13,color:.labelColor.withAppearanceAlpha(0.06))
        scopeTrack.identifier = NSUserInterfaceItemIdentifier("directory_scope_track")
        facetBar.addSubview(scopeTrack)
        let scopePills: [(String, String, CGFloat, CGFloat)] = [
            ("dir_scope:all", "All Nodes", 16, 74),
            ("dir_scope:mine", "Mine", 94, 47)
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
                self.activityClearConfirm=false
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
            let matched = Set(self.activityFilteredItems.map { "\($0.hub):\($0.signal_id)" })
            let matchedUnread = self.serverActivityFilter == self.activityFilter && self.serverActivityQuery == self.directoryQuery
                ? self.activityUnreadCount : self.activityFilteredItems.filter { $0.unread }.count
            self.unreadCount = max(0,self.unreadCount-matchedUnread)
            self.activityUnreadCount = 0
            self.serverActivityFilter = nil
            self.activityAllItems = self.activityAllItems.map { item in
                if !matched.contains("\(item.hub):\(item.signal_id)") { return item }
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
            self.emit(["op": "ack_signals", "ack_all": true,"filter":self.activityFilter,"query":self.directoryQuery])
            self.queryDirectory()
        }
        facetBar.addSubview(markReadBtn)
        directoryFacetControls["activity_mark_all_read"] = markReadBtn
        let clear = MenuControl("Clear…",frame:NSRect(x:0,y:5,width:76,height:22),leadingSymbol:"trash",radius:11)
        clear.label.font = .systemFont(ofSize: 11, weight: .medium)
        clear.identifier = NSUserInterfaceItemIdentifier("activity_clear")
        clear.isHidden = true
        clear.style = .destructiveHover
        clear.pressed = { [weak self] in guard let self=self else{return}; self.activityClearConfirm=true; self.updateDirectoryFacetStyles() }
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

        let cancelComposerBtn = MenuControl("Cancel", frame: NSRect(x: 492, y: 10, width: MenuControl.intrinsicWidth("Cancel"), height: 24), radius: 6)
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
            ? (composerReplySignalId == nil ? "↵ Send    ⎋ Close Draft" : "↵ Send    ⎋ Cancel Reply")
            : (directoryMode == "activity" ? (canReply ? "↵ Reply    ⎋ Close" : "⎋ Close") : "↵ Copy Address   ⇧↵ Message   ⎋ Close")
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
        activityClearConfirm=false
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
        activityClearConfirm=false; updateDirectoryFacetStyles()
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
        layoutComposer()
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
        let strip = mountPillStrip(in: facetBar, frame: NSRect(x: 157, y: 3, width: 387, height: 26),
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
        directoryFacetControls["activity_mark_all_read"]?.isEnabled = (serverActivityFilter == activityFilter && serverActivityQuery == directoryQuery ? activityUnreadCount : activityFilteredItems.filter { $0.unread }.count) > 0
        directoryFacetControls["activity_clear"]?.isEnabled = !activityFilteredItems.isEmpty
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
        guard let bar=directoryFacetBar else{return}
        bar.subviews.filter { $0.identifier?.rawValue.hasPrefix("activity_clear_confirm") == true }.forEach { $0.removeFromSuperview() }
        if isActivity && activityClearConfirm {
            bar.subviews.filter { $0 is MenuControl || $0.identifier?.rawValue == "activity_filter_track" }.forEach { $0.isHidden=true }
            let count=serverActivityFilter==activityFilter && serverActivityQuery==directoryQuery ? activityTotal : activityFilteredItems.count
            let description = directoryQuery.isEmpty ? "Clear \(count) \(activityFilter == "me" ? "For Me" : activityFilter == "all" ? "All Activity" : "Unread") messages?" : "Clear \(count) matching messages?"
            let label=textLabel(description,NSRect(x:16,y:7,width:300,height:18),size:11,weight:.medium)
            label.identifier=NSUserInterfaceItemIdentifier("activity_clear_confirm_label"); label.lineBreakMode = .byTruncatingTail; label.toolTip=description
            let cancel=MenuControl("Cancel",frame:NSRect(x:0,y:5,width:MenuControl.intrinsicWidth("Cancel"),height:22))
            cancel.identifier=NSUserInterfaceItemIdentifier("activity_clear_confirm_cancel")
            cancel.pressed={ [weak self] in self?.activityClearConfirm=false; self?.updateDirectoryFacetStyles() }
            let confirm=MenuControl("Clear",frame:NSRect(x:0,y:5,width:MenuControl.intrinsicWidth("Clear"),height:22))
            confirm.identifier=NSUserInterfaceItemIdentifier("activity_clear_confirm_commit"); confirm.style = .destructive
            confirm.toolTip="Clear only messages matching the current filter and search"
            confirm.pressed={ [weak self] in guard let self=self else{return}; self.activityClearConfirm=false; self.emit(["op":"clear_activity_history","filter":self.activityFilter,"query":self.directoryQuery]); self.queryDirectory(); self.updateDirectoryFacetStyles() }
            for view in [label,cancel,confirm] { bar.addSubview(view) }
            layoutBar(in:bar,insetLeft:16,insetRight:16,y:5,height:22,flexible:label,trailing:[cancel,confirm])
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
        updateDirectoryFacetStyles()
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
        guard serverDirectoryQuery == nil else { return }
        directoryAllPeers = listeners.map { listener in
            let user = listener.user ?? listener.owner_user ?? hubs.first(where: { $0.name == listener.hub })?.user ?? ""
            let host = (listener.host_name ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            return RemotePeer(hub: listener.hub, listener_id: listener.listener_id,
                public_id: listener.public_id, scoped_id: listener.scoped_id,
                user: user, host_id: listener.host_id ?? "", host_name: host.isEmpty ? "This Mac" : host, harness: listener.harness,
                display_address: listener.display_address, title: listener.title, about: listener.about ?? "",
                scope: listener.scope, last_seen_at: "", session_id: listener.session_id, custom_title: listener.custom_title, cwd: listener.cwd, repo: listener.repo,
                branch: listener.branch, is_self_principal: true, channels: listener.channels,
                attachments:listener.attachments ?? (listener.harness == "human" ? hubs.first(where:{$0.name == listener.hub})?.attachments : nil))
        } + myRemote
        applyDirectoryFilter(fromDaemonFrame: false, daemonMatchedCount: nil)
    }

    func openDirectoryPanel(mode: String = "directory") {
        activityClearConfirm = false
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
        activityClearConfirm=false; updateDirectoryFacetStyles()
        MenuControl.clearHoverWhere { $0.window === self.directoryPanel }
        directoryVisible = false
        clearDirectoryActionError()
        directoryPanel?.orderOut(nil)
        if !headless && !settingsVisible && !directoryVisible { NSApp.setActivationPolicy(.accessory) }
        if emitAction { emit(["op": "close_directory"]) }
    }

    func applyDirectoryFilter(fromDaemonFrame: Bool, daemonMatchedCount: Int?) {
        let offset = directoryScrollView?.contentView.bounds.origin
        let top = directoryTableView.map { $0.rows(in:$0.visibleRect).location } ?? NSNotFound
        let anchorId = directoryFilteredPeers.indices.contains(top) ? directoryFilteredPeers[top].scoped_id : nil
        let deltaY = (offset?.y ?? 0) - (directoryTableView.flatMap { top == NSNotFound ? nil : $0.rect(ofRow:top).minY } ?? 0)
        let previousSelectedId = directoryFilteredPeers.indices.contains(directorySelectedIndex) ? directoryFilteredPeers[directorySelectedIndex].scoped_id : nil
        let sameServerQuery = serverDirectoryQuery == directoryQuery && serverDirectoryScope == directoryScopeFilter && serverDirectoryHarness == directoryHarnessFilter
        if serverDirectoryQuery == nil {
            directoryFilteredPeers = directoryAllPeers.filter { peer in
                (directoryScopeFilter != "mine" || peer.is_self_principal == true) && (directoryHarnessFilter == "all" || peer.harness == directoryHarnessFilter) && (directoryQuery.isEmpty || [peer.title, peer.about, peer.display_address, peer.hub ?? "", peer.user, peer.cwd ?? "", peer.repo ?? "", peer.host_name].joined(separator:" ").localizedCaseInsensitiveContains(directoryQuery))
            }
        } else { directoryFilteredPeers = directoryAllPeers }
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
            directoryCopiedUntil[peer.scoped_id] = Date().addingTimeInterval(1.2)
            copyText(peer.display_address)
            if let table = directoryTableView,
               let cell = table.view(atColumn: 0, row: directorySelectedIndex, makeIfNecessary: true) as? DirectoryPeerCellView {
                cell.copyControl.copied()
            }
        case "shift-return":
            guard directoryMode == "directory", directoryComposerBar?.isHidden != false, directoryFilteredPeers.indices.contains(directorySelectedIndex) else{return}
            let peer=directoryFilteredPeers[directorySelectedIndex]
            openComposer(target:peer.display_address,hub:peer.hub ?? "")
        case "command-comma": openSettingsPanel()
        case "delete":
            guard directoryMode == "activity", !(directoryPanel?.firstResponder is NSTextView),
                  activityFilteredItems.indices.contains(directorySelectedIndex) else { return }
            deleteActivity(activityFilteredItems[directorySelectedIndex])
        case "command-w": closeDirectoryPanel(emitAction:true)
        case "escape":
            if activityClearConfirm { activityClearConfirm=false; updateDirectoryFacetStyles(); return }
            if !(directoryComposerBar?.isHidden ?? true) {
                closeComposer()
                return
            }
            closeDirectoryPanel(emitAction: true)
        default:
            break
        }
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
        cell.configure(with: peer, selected: row == directorySelectedIndex, harnesses: harnessMap, onCopy: { [weak self] text, control in
            self?.directoryCopiedUntil[peer.scoped_id] = Date().addingTimeInterval(1.2)
            self?.copyText(text,control:control)
        }) { [weak self] in
            guard let self = self else { return }
            self.openComposer(target: peer.display_address, hub: peer.hub ?? "", replySignalId: nil)
        }
        if Date() < (directoryCopiedUntil[peer.scoped_id] ?? .distantPast) { cell.copyControl.copied() }
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

    func replyToActivity(_ item: ActivityItem) {
        guard item.for_me, let target = item.from_display_address ?? item.from_scoped_id, !target.isEmpty else { return }
        if item.unread { emit(["op":"ack_signals","hub":item.hub,"signal_ids":[item.signal_id]]) }
        openComposer(target: target, hub: item.hub, replySignalId: item.signal_id)
    }
}
