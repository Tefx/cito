import AppKit

extension Companion {
    func pendingLabel(_ control: MenuControl, _ pending: Bool, _ title: String) {
        guard pending else { return }
        control.feedbackTimer?.invalidate(); control.feedbackTimer=nil
        control.isEnabled = false; control.setLeadingSymbol(nil)
        control.label.stringValue = title; control.setAccessibilityLabel(title); control.toolTip = title
        let right = control.frame.maxX
        control.pendingWidth = max(control.frame.width,MenuControl.intrinsicWidth(title))
        control.frame.size.width = control.pendingWidth
        control.frame.origin.x = right - control.pendingWidth
    }
    func channelSavePending(_ op: String, hub: String, channel: String, scope: String) -> Bool {
        pendingControls.contains { request, pending in
            guard pending.op == op && pending.hub == hub && pending.target == channel,
                  let sent = pendingChannelValues[request] else { return false }
            let fields = channelSettingsByHub[hub]?.first { $0.id == channel }?.fields ?? []
            let drafts = channelUiStates["\(hub)|\(channel)"]?.drafts ?? [:]
            return fields.filter { $0.scope == scope }.allSatisfy { field in
                (drafts[field.key] ?? field.value ?? "") == (sent.values[field.key] ?? field.value ?? "")
            }
        }
    }

    func rebuildFeedbackAfter(_ seconds: Double) {
        RunLoop.main.add(Timer(timeInterval: seconds, repeats: false) { [weak self] _ in
            self?.rebuildSettingsPanel(preservingEditor: true)
        }, forMode: .common)
    }

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
    func finishHubSave(_ pending: PendingHubSave) {
        pendingHubSave = nil
        hubSaveAwaitingSettings = nil
        if addHubDraftGeneration == pending.generation {
            addHubFormOpen = false; addHubToken = ""
        } else if editingExistingHub || proposedHubName(addHubName,url:addHubUrl,existing:settings?.hubs ?? [],creating:false,original:pending.name) == pending.name {
            // A retained draft edits the committed Hub, even if its projection
            // was too large for the receipt. Keep the URL and inherited values
            // as a baseline until canonical settings replace them.
            editingExistingHub = true; editingOriginalHub = pending.name
            hubSaveAwaitingSettings = pending
        }
    }

    @discardableResult
    func acceptSettings(_ value: SettingsPayload, hub: String? = nil, savedChannel: String? = nil, savedRequest: String? = nil, confirmHubSave: Bool = false) -> Bool {
        let previousUi = channelUiStates
        settings = value
        if !selectedChannelHub.isEmpty && !value.hubs.contains(where: { $0.name == selectedChannelHub && $0.enabled }) {
            selectedChannelHub = value.hubs.first(where: { $0.enabled })?.name ?? ""
            if settingsVisible && !selectedChannelHub.isEmpty { emit(["op": "open_settings", "hub": selectedChannelHub], userInitiated:false) }
        }
        if hostHubPortPending && savedRequest == submittedHostHubRequestId {
            hostHubPortPending = false
            hostHubPortSaved = value.host_hub.port == submittedHostHubPort && value.host_hub.error == nil && (hostHubPortDraft == nil || Int((hostHubPortDraft ?? "").trimmingCharacters(in:.whitespacesAndNewlines)) == submittedHostHubPort)
            if hostHubPortSaved { hostHubPortSavedUntil = Date().addingTimeInterval(1.2); rebuildFeedbackAfter(1.2) }
            hostHubPortError = nil
            if hostHubPortSaved && Int((hostHubPortDraft ?? "").trimmingCharacters(in:.whitespacesAndNewlines)) == submittedHostHubPort { hostHubPortDraft = nil }
        }
        if confirmHubSave, let pending = pendingHubSave, savedRequest == pending.requestId {
            let saved = value.hubs.first(where: { $0.name == pending.name })
            pendingHubSave = nil
            addHubError = saved?.error
            if saved?.error == nil { finishHubSave(pending) }
        }
        var reconciledHub = false
        if pendingHubSave == nil, let committed = hubSaveAwaitingSettings,
           let saved = value.hubs.first(where: { $0.name == committed.name && normalizedHubURL($0.url) == committed.url }) {
            // A passive canonical refresh also completes a payload-free receipt.
            // Compare each field with the submitted draft to preserve newer edits.
            if addHubIssuer == committed.issuer {
                addHubIssuer = saved.issuer ?? ""
                settingsFieldControls["settings_hub_issuer"]?.stringValue = addHubIssuer
            }
            if addHubClientId == committed.clientId {
                addHubClientId = saved.client_id ?? ""
                settingsFieldControls["settings_hub_client_id"]?.stringValue = addHubClientId
            }
            if addHubRedirectPort == committed.redirectPort {
                addHubRedirectPort = saved.redirect_port.map(String.init) ?? ""
                settingsFieldControls["settings_hub_redirect_port"]?.stringValue = addHubRedirectPort
            }
            editingInitialHub = saved
            hubSaveAwaitingSettings = nil
            reconciledHub = true
        }
        let target = value.settings_hub ?? value.channel_hub ?? hub ?? effectiveChannelHub
        let previousChannels = channelSettingsByHub[target] ?? []
        channelSettingsByHub[target] = value.channels ?? []
        channelInventoryErrorsByHub[target] = value.channel_inventory_error
        for channel in value.channels ?? [] {
            let key=target + "|" + channel.id
            var ui = channelUiStates[key] ?? ChannelUiState()
            let previous = previousChannels.first { $0.id == channel.id }
            if previous?.status != channel.status || previous?.detail != channel.detail || previous?.warning != channel.warning {
                ui.testResultIsLatest = false
            }
            if channel.configured && channel.status != "pairing_pending" &&
                (previous?.configured == false || (!ui.pairingInstruction.isEmpty && (channel.pairing_instruction ?? "").isEmpty)) {
                ui.resetPersonal(preservePendingPair: true)
            }
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
                    // Settings results retire save UI, not a newer pairing intent.
                    if entry.scope == "user" && channel.configured && channel.status != "pairing_pending" { ui.resetPersonal(preservePendingPair: true) }
                    ui.savedUntil[entry.scope] = Date().addingTimeInterval(1.5)
                } else { ui.savedUntil.removeValue(forKey: entry.scope) }
                rebuildFeedbackAfter(1.5)
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
            if let instruction = channel.pairing_instruction, !instruction.isEmpty {
                ui.pairingInstruction = instruction
                ui.pairingCommand = channel.pairing_command ?? channel.pairing_code
                ui.pairingURL = channel.pairing_url
            } else if channel.status != "pairing_pending" && ui.pendingPairRequest == nil {
                ui.pairingInstruction = ""
                ui.pairingCommand = nil
                ui.pairingURL = nil
            }
            channelUiStates[key] = ui
        }
        return reconciledHub || channelUiStates != previousUi
    }

    @discardableResult
    func settleChannelReceipt(_ frame: Frame) -> Bool {
        let operation = frame.source_op ?? frame.action ?? (frame.op == "pair_code" ? "channel_pair" : frame.op == "channel_test_result" ? "channel_test" : "")
        guard let request = frame.request_id,
              let pending = pendingControls[request], pending.op == operation,
              frame.hub == nil || pending.hub == frame.hub,
              pending.target == (frame.channel_id ?? frame.channel ?? "") else { return false }
        let hub = pending.hub
        pendingControls.removeValue(forKey: request)
        let key = "\(hub)|\(pending.target)"
        if let reset = pendingChannelResets.removeValue(forKey: request),
           (frame.ok == true || frame.committed == true), frame.authority_changed != true {
            let unchanged = (channelUiStates[key] ?? ChannelUiState()) == reset.submitted
            let fields = channelSettingsByHub[hub]?.first { $0.id == pending.target }?.fields ?? []
            for field in fields where reset.all || field.scope == "user" {
                if channelUiStates[key]?.drafts[field.key] == reset.submitted.drafts[field.key] {
                    channelUiStates[key]?.drafts.removeValue(forKey: field.key)
                }
            }
            if unchanged {
                if reset.all { channelUiStates[key, default: ChannelUiState()].resetAll() }
                else { channelUiStates[key, default: ChannelUiState()].resetPersonal() }
            }
        }
        if let entry = pendingChannelValues[request], entry.hub == hub, entry.channelId == pending.target,
           ["channel_save", "save_hub_service"].contains(operation), frame.settings == nil || frame.authority_changed == true {
            pendingChannelValues.removeValue(forKey: request)
            // Retire only submitted secrets, which settings never echo. Keep
            // non-secret drafts until fresh settings reconcile their values,
            // and preserve all drafts on a replacement authority.
            if frame.committed == true && frame.authority_changed != true {
                let fields = channelSettingsByHub[hub]?.first { $0.id == pending.target }?.fields ?? []
                for field in fields where field.type == "secret" {
                    if let sent = entry.values[field.key], channelUiStates[key]?.drafts[field.key] == sent {
                        channelUiStates[key]?.drafts.removeValue(forKey: field.key)
                    }
                }
            }
        }
        if operation == "channel_pair", channelUiStates[key]?.pendingPairRequest == request {
            channelUiStates[key]?.pendingPairRequest = nil
        }
        if let warning = frame.warning { settingsBannerError = warning }
        return true
    }

    @discardableResult
    func settleReceiptOnly(_ frame: Frame) -> Bool {
        var settled = settleChannelReceipt(frame)
        var routedToForm = false
        guard let request = frame.request_id else { return settled }
        let operation = frame.source_op ?? frame.action ?? ""
        let failed = frame.ok == false || frame.op == "error"
        let message = frame.warning ?? frame.error
        if let pending = pendingHubSave, request == pending.requestId,
           operation.isEmpty || ["setup_save_hub", "save_hub", "paste_hub_invite"].contains(operation) {
            pendingHubSave = nil
            if failed { addHubError = message ?? "Hub save failed."; routedToForm = true }
            else if frame.authority_changed != true && (frame.committed == true || frame.ok == true) {
                finishHubSave(pending)
            }
            settled = true
        }
        if request == hubInviteRequest && (operation.isEmpty || ["setup_save_hub", "save_hub", "paste_hub_invite"].contains(operation)) {
            hubInvitePending = false; settled = true
        }
        if request == submittedHostHubRequestId && (operation.isEmpty || operation == "set_host_hub") {
            hostHubPortPending = false; submittedHostHubRequestId = ""; settled = true
        }
        if (operation.isEmpty || operation == "setup_apply"), pendingAppChecks.removeValue(forKey: request) != nil { settled = true }
        if let pending = pendingControls[request], pending.op == operation || operation.isEmpty,
           frame.hub == nil || frame.hub == pending.hub,
           frame.channel_id == nil || frame.channel_id == pending.target,
           frame.channel == nil || frame.channel == pending.target {
            pendingControls.removeValue(forKey: request); settled = true
        }
        if settled && !routedToForm, let message = message { settingsBannerError = message }
        return settled
    }

    func updateInbox(paused: Bool? = nil, hub: String, scope: String? = nil) {
        let current=hubs.first { $0.name==hub }
        var action: [String:Any]=["op":"human_inbox_update","hub":hub,"expected_control_revision":current?.human_control_revision ?? 1,"request_id":UUID().uuidString]
        if let paused=paused { action["paused"]=paused }
        if let scope=scope { action["scope"]=scope }
        emit(action)
    }

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
            let root = SurfaceView(frame:NSRect(x:0,y:0,width:520,height:460),radius:14,color:.clear,border:.labelColor.withAppearanceAlpha(0.15))
            root.identifier=NSUserInterfaceItemIdentifier("settings_panel"); root.layer?.masksToBounds=true
            let effect=NSVisualEffectView(frame:root.bounds)
            effect.material = .popover; effect.blendingMode = .behindWindow; effect.state = .followsWindowActiveState
            root.addSubview(effect); window.contentView=root
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
            else if settingsActiveTab == "hubs" && addHubFormOpen { addHubFormOpen = false; inviteParseRequest=""; pendingHubSave=nil; hubSaveAwaitingSettings=nil; rebuildSettingsPanel() }
            else if settingsActiveTab == "channels" && (channelSettingsByHub[effectiveChannelHub] ?? []).contains(where: { channelUiStates["\(effectiveChannelHub)|\($0.id)"]?.editingGateway == true }) {
                for (hub, channels) in channelSettingsByHub where hub == effectiveChannelHub {
                    for ch in channels where channelUiStates["\(hub)|\(ch.id)"]?.editingGateway == true {
                        clearChannelDrafts(ch, scope:"hub", hub:hub)
                        channelUiStates["\(hub)|\(ch.id)"]?.resetGateway()
                    }
                }
                rebuildSettingsPanel()
            }
            else if settingsActiveTab == "channels" && (channelSettingsByHub[effectiveChannelHub] ?? []).contains(where: { channelUiStates["\(effectiveChannelHub)|\($0.id)"]?.editingPersonal == true }) {
                for (hub, channels) in channelSettingsByHub where hub == effectiveChannelHub {
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
            if settingsActiveTab == "hubs" && field == "host_hub_port" { actionId="apply_host_hub_port" }
            else if settingsActiveTab == "channels", field.hasPrefix("channel_field:"), let channel=(channelSettingsByHub[effectiveChannelHub] ?? []).first(where:{field.hasPrefix("channel_field:\($0.id):")}) {
                let scope=channel.fields.first(where:{field=="channel_field:\(channel.id):\($0.key)"})?.scope
                actionId=(scope == "hub" ? "save_hub_service:" : "save_channel:")+channel.id
            } else if settingsActiveTab == "hubs" && addHubFormOpen { actionId="submit_add_hub" }
            else { return }
            if let root=settingsPanel?.contentView, let control=findControl(in:root,id:actionId), control.isEnabled { control.pressed?() }
        default: break
        }
    }
    var editingBuiltinHub: Bool {
        editingExistingHub && (settings?.hubs.first { $0.name == editingOriginalHub }?.is_builtin ?? hubs.first { $0.name == editingOriginalHub }?.is_builtin) == true
    }
    func syncAddHubFieldsFromControls() {
        if editingBuiltinHub, let hub = settings?.hubs.first(where: { $0.name == editingOriginalHub }) { addHubName = hub.name; addHubUrl = hub.url }
        if let nameField = settingsFieldControls["settings_hub_name"] { addHubName = nameField.stringValue }
        if let urlField = settingsFieldControls["settings_hub_url"] { addHubUrl = urlField.stringValue }
        if let tokenField = settingsFieldControls["settings_hub_token"] { addHubToken = tokenField.stringValue }
        if let userField = settingsFieldControls["settings_hub_user"] { addHubUser = userField.stringValue }
        if let issuerField = settingsFieldControls["settings_hub_issuer"] { addHubIssuer = issuerField.stringValue }
        if let clientField = settingsFieldControls["settings_hub_client_id"] { addHubClientId = clientField.stringValue }
        if let portField = settingsFieldControls["settings_hub_redirect_port"] { addHubRedirectPort = portField.stringValue }
    }

    func makeEditableField(_ id: String, frame: NSRect, placeholder: String, value: String, secret: Bool = false, accessibilityLabel: String) -> NSTextField {
        let field: NSTextField = secret ? NSSecureTextField(frame: frame) : NSTextField(frame: frame)
        field.identifier = NSUserInterfaceItemIdentifier(id)
        field.setAccessibilityLabel(accessibilityLabel)
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

    func validHostPort(fallback: Int, fallbackOnInvalid: Bool = false) -> Int? {
        let raw = settingsFieldControls["host_hub_port"]?.stringValue ?? hostHubPortDraft ?? "\(fallback)"
        guard let port = Int(raw.trimmingCharacters(in:.whitespacesAndNewlines)), (1...65535).contains(port) else {
            if fallbackOnInvalid { return fallback }
            hostHubPortError = "Port must be an integer from 1 to 65535."
            hostHubPortSaved = false; rebuildSettingsPanel(); return nil
        }
        return port
    }

    func handlePasteHubInvite() {
        guard !hubInvitePending else { return }
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
        hubInviteRequest = UUID().uuidString
        emit(["op":"paste_hub_invite","invite":raw,"request_id":hubInviteRequest])
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

    func rebuildSettingsPanel(preservingEditor: Bool = false, revealAddHub: Bool = false, syncFields: Bool = true) {
        settingsDirty = true
        guard settingsVisible || renderingForInspection, let window = settingsPanel else { return }
        if preservingEditor, settingsFieldControls.values.contains(where: { $0.currentEditor() === window.firstResponder && $0.currentEditor() != nil }) {
            settingsRebuildDeferred = true
            return
        }
        settingsRebuildDeferred = false
        settingsDirty = false
        settingsRebuildCount += 1
        let focused = settingsFieldControls.first { $0.value.currentEditor() != nil && $0.value.currentEditor() === window.firstResponder }
        let focusedId = focused?.key
        let selectedRange = (focused?.value.currentEditor() as? NSTextView)?.selectedRange()
        if syncFields { syncAddHubFieldsFromControls() }
        MenuControl.clearHoverWhere { $0.window === window }
        for case let scroll as NSScrollView in window.contentView?.subviews ?? [] {
            if let id = scroll.identifier?.rawValue {
                settingsScrollOffsets[id] = max(0,(scroll.documentView?.frame.height ?? 0)-scroll.contentSize.height-scroll.contentView.bounds.origin.y)
            }
        }
        settingsFieldControls.removeAll()

        guard let root=window.contentView as? SurfaceView else { return }
        root.subviews.filter { !($0 is NSVisualEffectView) }.forEach { $0.removeFromSuperview() }
        window.invalidateShadow()

        // Header & Segmented Tab Bar (520 × 48 pt, y: 412)
        let header = NSView(frame: NSRect(x: 0, y: 412, width: 520, height: 48))
        header.identifier = NSUserInterfaceItemIdentifier("settings_header")
        header.alphaValue = uninstallConfirmOpen ? 0.35 : 1
        header.addSubview(textLabel("Cito", NSRect(x: 16, y: 15, width: 38, height: 18), size: 13, weight: .bold))

        let tabWidths = [("Agent Apps","square.grid.2x2"),("Human Channels","bubble.left.and.bubble.right"),("Hubs & Network","network")].map {
            MenuControl.intrinsicWidth($0.0, leadingSymbol:$0.1, paddingX:12)
        }
        let tabsWidth = tabWidths.reduce(0,+) + 12
        let tabsTrack = makePillTrack(NSRect(x:62+floor((400-tabsWidth)/2),y:10,width:tabsWidth,height:28),radius:9)
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

        buildAppsTab(in: root)
        buildChannelsTab(in: root)
        buildHubsTab(in: root)

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
        loginBtn.setAccessibilityRole(.checkBox)
        loginBtn.setAccessibilityValue(launchAtLogin ? 1 : 0)
        loginBtn.label.alignment = .left
        loginBtn.layoutContents()
        loginBtn.pressed = { [weak self] in
            self?.emit(["op": "set_launch_at_login", "enabled": !launchAtLogin])
        }
        footerBar.addSubview(loginBtn)

        let uninstallBtn = MenuControl("Disconnect Agent Apps…", frame: NSRect(x: 316, y: 10, width: MenuControl.intrinsicWidth("Disconnect Agent Apps…", paddingX: 12), height: 24), radius: 6)
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

            confirmBox.addSubview(textLabel("Disconnect Agent Apps",NSRect(x:12,y:82,width:416,height:18),size:12.5,weight:.semibold))
            let purgeCheckbox = MenuControl("Also delete local data and Hub settings",
                frame: NSRect(x: 12, y: 50, width: 416, height: 22),
                leadingSymbol: uninstallPurgeData ? "checkmark.square.fill" : "square", radius: 6)
            purgeCheckbox.identifier = NSUserInterfaceItemIdentifier("uninstall_purge_data")
            purgeCheckbox.style = .subtleHeader
            purgeCheckbox.setAccessibilityRole(.checkBox)
            purgeCheckbox.setAccessibilityValue(uninstallPurgeData ? 1 : 0)
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

            let disconnectTitle=uninstallPurgeData ? "Disconnect & Delete Data" : "Disconnect Apps"
            let disconnect=MenuControl(disconnectTitle,frame:NSRect(x:112,y:16,width:MenuControl.intrinsicWidth(disconnectTitle),height:24),radius:6)
            disconnect.identifier=NSUserInterfaceItemIdentifier("confirm_disconnect_all")
            disconnect.style = .destructive
            disconnect.toolTip = uninstallPurgeData ? "Disconnect all Agent Apps and delete local data and Hub settings" : "Disconnect all Agent Apps and keep Cito running"
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

        window.invalidateShadow()
        if revealAddHub && addHubFormOpen, let form=root.subviews.compactMap({($0 as? NSScrollView)?.documentView}).flatMap({$0.subviews}).first(where:{$0.identifier?.rawValue=="add_hub_form"}) { form.scrollToVisible(form.bounds) }
        if let id = focusedId, let field = settingsFieldControls[id] {
            window.makeFirstResponder(field)
            if let range = selectedRange, let editor = field.currentEditor() as? NSTextView { editor.setSelectedRange(range) }
        }
    }

    func buildAppsTab(in root: NSView) {
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
        appsView.addSubview(textLabel("Connect your coding apps, then choose sessions to listen.",NSRect(x:20,y:316,width:480,height:16),size:11,color:CitoPalette.secondaryText))
        var appTop: CGFloat = 304
        for app in resolvedApps {
            let cardHeight: CGFloat = 70
            appTop -= cardHeight
            let card = roundedView(NSRect(x:20,y:appTop,width:480,height:cardHeight),radius:10,
                color:.labelColor.withAppearanceAlpha(0.045),border:.labelColor.withAppearanceAlpha(0.08))
            card.identifier = NSUserInterfaceItemIdentifier("settings_app:\(app.id)")
            let barY: CGFloat = 36
            card.addSubview(makeHarnessBadge(NSRect(x:12,y:barY-2,width:28,height:28),radius:7,iconInset:6,harness:app.id,harnesses:map))
            let titleW = min(app.isConfigured ? 184 : 180, measureTextWidth(app.display_name,font:.systemFont(ofSize:13,weight:.semibold)) + 2)
            let title = textLabel(app.display_name,NSRect(x:50,y:barY+3.5,width:titleW,height:17),size:13,weight:.semibold)
            title.toolTip = app.display_name; title.lineBreakMode = .byTruncatingMiddle
            card.addSubview(title)
            var leading: [NSView] = [title]
            if app.tier == "experimental" {
                let tier = makePillBadge("Experimental", id: "settings_app_tier:\(app.id)", textColor: CitoPalette.warningText, fill: CitoPalette.warningSurface, border: CitoPalette.warningBorder, minWidth:0)
                card.addSubview(tier); leading.append(tier)
            }
            let statusText = app.isConfigured ? "Connected" : (app.status == "error" ? "Inspection failed" : (app.status == "repair_needed" ? "Repair Needed" : "Not Set Up"))
            let statusColor = app.status == "error" ? CitoPalette.dangerText : app.isConfigured ? CitoPalette.successText : CitoPalette.warningText
            let dot=roundedView(NSRect(x:50,y:16,width:6,height:6),radius:3,color:statusColor)
            dot.identifier=NSUserInterfaceItemIdentifier("settings_app_status_dot:\(app.id)"); card.addSubview(dot)
            let detailText = presentationError(app.detail).replacingOccurrences(of:"^(Connected|Disconnected) · ",with:"",options:.regularExpression)
            let status=textLabel(statusText + (detailText.isEmpty || detailText == statusText ? "" : " · " + detailText),NSRect(x:62,y:10,width:406,height:18),size:11,color:statusColor)
            status.identifier=NSUserInterfaceItemIdentifier("settings_app_status:\(app.id)"); status.lineBreakMode = .byTruncatingMiddle
            status.toolTip=([detailText]+app.paths).filter { !$0.isEmpty }.joined(separator:"\n"); card.addSubview(status)
            let appActionWidth = max(76, MenuControl.intrinsicWidth("Disconnect"))
            let actionTitle=app.isConfigured || app.status == "error" ? "Check & Repair" : (app.status == "repair_needed" ? "Repair" : "Set Up")
            let btn = MenuControl(actionTitle,frame:NSRect(x:0,y:0,width:MenuControl.intrinsicWidth(actionTitle),height:24))
            btn.identifier = NSUserInterfaceItemIdentifier("setup_app:\(app.id)")
            btn.style = app.isConfigured ? .neutral : .prominent
            if Date() < (appCheckedUntil[app.id] ?? .distantPast) { btn.copied("✓ Checked") }
            let appPending = isPending("setup_apply",target:app.id) || isPending("setup_remove_app",target:app.id)
            btn.isEnabled = !appPending
            pendingLabel(btn,isPending("setup_apply",target:app.id),actionTitle == "Check & Repair" ? "Checking…" : "Setting Up…")
            btn.pressed = { [weak self] in
                let request = UUID().uuidString
                if app.isConfigured, let self = self {
                    self.pendingAppChecks = self.pendingAppChecks.filter { $0.value != app.id }
                    self.pendingAppChecks[request] = app.id
                }
                self?.emit(["op":"setup_apply","harness":app.id,"request_id":request])
            }
            card.addSubview(btn)
            var trailing: [NSView] = [btn]
            if app.isConfigured {
                let remove = MenuControl("Disconnect",frame:NSRect(x:0,y:0,width:appActionWidth,height:24))
                remove.identifier = NSUserInterfaceItemIdentifier("remove_app:\(app.id)"); remove.style = .destructiveHover
                remove.isEnabled = !appPending
                pendingLabel(remove,isPending("setup_remove_app",target:app.id),"Disconnecting…")
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

    }

    func buildChannelsTab(in root: NSView) {
        // Tab 2: Human Channels (520 × 368 pt, y: 44)
        let channelsView = NSView(frame: NSRect(x: 0, y: 44, width: 520, height: 368))
        channelsView.identifier = NSUserInterfaceItemIdentifier("settings_channels_tab")
        channelsView.isHidden = settingsActiveTab != "channels"

        let inboxHub = effectiveChannelHub
        let inbox = hubs.first { $0.name == inboxHub }
        let inboxConfig = settings?.hubs.first { $0.name == inboxHub }
        let inboxPaused = inboxConfig?.human_paused ?? inbox?.human_paused ?? false
        let inboxUser = inbox?.user ?? inboxConfig?.user ?? "local"
        let inboxAttachments = displayAttachments(inbox?.attachments ?? [])
        let externalNames = inboxAttachments.filter { $0.location == "hub" }.map { cleanHostDisplay($0.display_name) }
        let deliveryNames = externalNames + ["Activity"] + inboxAttachments.filter { $0.location == "device" }.map { cleanHostDisplay($0.display_name) }
        let inboxSummary = inboxPaused ? (externalNames.isEmpty ? "Inbox alerts paused" : "Inbox paused · Activity & " + externalNames.joined(separator:" · ") + " paused") : deliveryNames.joined(separator:" · ")
        let inboxBar = roundedView(NSRect(x:20,y:308,width:480,height:48),radius:CitoMetrics.cardRadius,color:inboxPaused ? CitoPalette.warningSurface : .labelColor.withAppearanceAlpha(0.045))
        inboxBar.identifier = NSUserInterfaceItemIdentifier("inbox_control_bar")
        let inboxTitle = textLabel("You (@\(inboxUser))",NSRect(x:12,y:30,width:220,height:12),size:11,weight:.semibold)
        inboxTitle.identifier = NSUserInterfaceItemIdentifier("inbox_control_title"); inboxTitle.toolTip = inboxTitle.stringValue
        let inboxDetail = textLabel(inboxSummary,NSRect(x:12,y:5,width:456,height:24),size:10,color:inboxPaused ? CitoPalette.warningText : CitoPalette.secondaryText)
        inboxDetail.identifier = NSUserInterfaceItemIdentifier("inbox_control_summary")
        inboxDetail.toolTip = inboxSummary + "\nExternal channels deliver alerts even when this Mac is offline."
        inboxDetail.setAccessibilityLabel(inboxSummary)
        inboxBar.addSubview(inboxTitle); inboxBar.addSubview(inboxDetail)
        let inboxToggle = MenuControl(inboxPaused ? "Resume Inbox" : "Pause Inbox",frame:NSRect(x:0,y:22,width:MenuControl.intrinsicWidth("Resume Inbox"),height:22))
        inboxToggle.identifier = NSUserInterfaceItemIdentifier("toggle_inbox")
        inboxToggle.isEnabled = (inbox?.status ?? inboxConfig?.status) == "connected" && (inboxConfig == nil || inboxConfig?.status == "connected") && (inboxConfig?.human_state_status ?? inbox?.human_state_status) == "current"
        inboxToggle.toolTip = inboxPaused ? "Resume your shared inbox on \(inboxHub), preserving descriptions and channel bindings" : "Pause inbox alerts on \(inboxHub) across all devices and external channels"
        inboxToggle.pressed = { [weak self] in self?.updateInbox(paused:!inboxPaused,hub:inboxHub) }
        pendingLabel(inboxToggle,isPending("human_inbox_update",hub:inboxHub),inboxPaused ? "Resuming…" : "Pausing…")
        inboxBar.addSubview(inboxToggle)
        var inboxActions: [NSView] = []
        if let id = inbox?.human_listener_id, let scope = scopeToggle(hub:inboxHub,user:inboxUser,scope:inbox?.human_scope ?? "self",id:id,y:22) {
            scope.isEnabled = inboxToggle.isEnabled
            scope.pressed = { [weak self] in self?.updateInbox(hub:inboxHub,scope:inbox?.human_scope == "workspace" ? "self" : "workspace") }
            inboxBar.addSubview(scope); inboxActions.append(scope)
        }
        inboxActions.append(inboxToggle)
        layoutBar(in:inboxBar,y:13,height:22,trailing:inboxActions)
        let textWidth = max(0,(inboxActions.map { $0.frame.minX }.min() ?? 468) - 10 - 12)
        let summaryHeight = min(24,max(12,ceil((inboxSummary as NSString).boundingRect(with:NSSize(width:textWidth,height:CGFloat.greatestFiniteMagnitude),options:[.usesLineFragmentOrigin],attributes:[.font:inboxDetail.font!]).height)))
        let titleHeight: CGFloat = summaryHeight > 14 ? 12 : 14
        let stackGap: CGFloat = summaryHeight > 14 ? 0 : 1
        let stackY = (inboxBar.bounds.height-titleHeight-summaryHeight-stackGap)/2
        inboxTitle.frame = NSRect(x:12,y:stackY+summaryHeight+stackGap,width:textWidth,height:titleHeight)
        inboxTitle.lineBreakMode = .byTruncatingMiddle
        inboxDetail.frame = NSRect(x:12,y:stackY,width:textWidth,height:summaryHeight)
        inboxDetail.maximumNumberOfLines = 2; inboxDetail.lineBreakMode = .byTruncatingTail
        channelsView.addSubview(inboxBar)
        var cursorY: CGFloat = 300
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

        if let error = channelInventoryErrorsByHub[effectiveChannelHub], !error.isEmpty {
            let message = presentationError(error)
            let height = max(16,ceil((message as NSString).boundingRect(with:NSSize(width:480,height:CGFloat.greatestFiniteMagnitude),options:[.usesLineFragmentOrigin,.usesFontLeading],attributes:[.font:NSFont.systemFont(ofSize:11)]).height))
            let diagnostic = textLabel(message, NSRect(x:20,y:cursorY-height-4,width:480,height:height),size:11,color:CitoPalette.dangerText)
            diagnostic.identifier = NSUserInterfaceItemIdentifier("channel_inventory_error")
            diagnostic.maximumNumberOfLines = 0; diagnostic.lineBreakMode = .byWordWrapping
            diagnostic.toolTip = message
            channelsView.addSubview(diagnostic)
            cursorY -= height + 12
        }
        let resolvedChannels = channelSettingsByHub[effectiveChannelHub] ?? []
        let visibleChannels = resolvedChannels.filter { $0.can_configure_hub == true || $0.hub_ready == true }
        if !resolvedChannels.isEmpty && visibleChannels.isEmpty && channelInventoryErrorsByHub[effectiveChannelHub] == nil {
            let notice = textLabel("External channels are managed by your '\(effectiveChannelHub)' Hub administrator. None are enabled yet.", NSRect(x:20,y:cursorY-42,width:480,height:36),size:11,color:CitoPalette.secondaryText)
            notice.identifier = NSUserInterfaceItemIdentifier("channels_managed_notice")
            notice.maximumNumberOfLines = 0; notice.lineBreakMode = .byWordWrapping
            channelsView.addSubview(notice); cursorY -= 48
        }
        var chCursorY: CGFloat = cursorY
        for ch in visibleChannels {
            let visibleFields = ch.fields.filter { $0.hidden != true }
            let channelKey = "\(effectiveChannelHub)|\(ch.id)"
            let channelUi = channelUiStates[channelKey] ?? ChannelUiState()
            let instruction = channelUi.pairingInstruction
            let instructionHeight = instruction.isEmpty ? CGFloat(0) : max(16, ceil((instruction as NSString).boundingRect(with: NSSize(width: 432, height: CGFloat.greatestFiniteMagnitude), options: [.usesLineFragmentOrigin], attributes: [.font: NSFont.monospacedSystemFont(ofSize: 10.5, weight: .semibold)]).height) + 4)
            let guide = (ch.setup_guide ?? []).joined(separator:"\n")
            let hubFields = visibleFields.filter { $0.scope == "hub" && ch.can_configure_hub == true }
            let disclosure = ch.can_edit_personal == true && ch.pairing_supported && ch.pairing_manual_disclosure == true
            let manualExpanded = channelUi.manualEntry
            let userFields = visibleFields.filter { $0.scope != "hub" }
            let formFields = ch.can_edit_personal != true || (disclosure && !manualExpanded) ? [] : userFields
            let hasHubScopeFields = ch.has_hub_service == true
            let showHubStratum = hasHubScopeFields && (ch.can_configure_hub == true || ch.error_origin == "hub")
            let hubFieldsH = CGFloat(hubFields.count) * 26
            let userFieldsH = CGFloat(formFields.count) * 26 + (disclosure ? 24 : 0)
            let gatewayActive = ch.hub_ready == true
            let hubGatewayExpanded = ch.can_configure_hub == true && (!gatewayActive || channelUi.editingGateway)
            let personalCollapsed = gatewayActive && ch.configured && !channelUi.editingPersonal && instruction.isEmpty
            let guideH: CGFloat = !gatewayActive && !guide.isEmpty && !hubFields.isEmpty ? max(13,ceil((guide as NSString).boundingRect(with:NSSize(width:432,height:CGFloat.greatestFiniteMagnitude),options:[.usesLineFragmentOrigin],attributes:[.font:NSFont.systemFont(ofSize:10)]).height)+2) : 0
            let currentUser=settings?.hubs.first(where:{$0.name==effectiveChannelHub})?.user ?? hubs.first(where:{$0.name==effectiveChannelHub})?.user ?? "local"
            let gatewayRole = !hubFields.isEmpty ? (guideH > 0 ? "" : "Configured once by the Hub owner. Shared by everyone on this Hub.") :
                (hasHubScopeFields ? (ch.hub_provided ? "Provided by '\(effectiveChannelHub)' Hub administrator — no gateway setup needed on your Mac." : "Not yet configured by the '\(effectiveChannelHub)' Hub administrator.") : "No shared gateway credentials required for this channel.")
            func roleHeight(_ text: String) -> CGFloat {
                text.isEmpty ? 0 : max(13,ceil((text as NSString).boundingRect(with:NSSize(width:432,height:CGFloat.greatestFiniteMagnitude),options:[.usesLineFragmentOrigin],attributes:[.font:NSFont.systemFont(ofSize:10)]).height))
            }
            let gatewayRoleH=roleHeight(gatewayRole)
            let hubStratumH: CGFloat = !showHubStratum ? 0 : (hubGatewayExpanded ? 36 + gatewayRoleH + guideH + hubFieldsH + (!hubFields.isEmpty ? 32 : 0) : 36)
            let userStratumH: CGFloat = !gatewayActive ? 0 : (personalCollapsed ? 38 : 40 + userFieldsH + (instructionHeight > 0 ? instructionHeight + 34 : 0) + 34)
            let channelResult = (ch.warning == true || ch.status == "error") && !channelUi.testResultIsLatest ? nil : channelUi.testResult
            let showDetail = ch.status == "error" || ch.warning == true || channelResult != nil
            let detailHeight = showDetail ? max(14,ceil((presentationError(channelResult?.detail ?? ch.detail) as NSString).boundingRect(with:NSSize(width:422,height:CGFloat.greatestFiniteMagnitude),options:[.usesLineFragmentOrigin],attributes:[.font:NSFont.systemFont(ofSize:10.5)]).height)) : 0
            let headerHeight: CGFloat = showDetail ? 38 + detailHeight : 38
            let unavailable = ch.status == "error" && ch.fields.isEmpty
            let recoveryHeight: CGFloat = ch.status == "error" ? 34 : 0
            let cardHeight: CGFloat = unavailable ? max(90, headerHeight + 40) : headerHeight + hubStratumH + (showHubStratum && gatewayActive ? CitoMetrics.gap : 0) + userStratumH + recoveryHeight + 8
            chCursorY -= cardHeight
            let chCard = roundedView(NSRect(x: 20, y: chCursorY, width: 480, height: cardHeight), radius: 10,
                color: .labelColor.withAppearanceAlpha(0.045), border: .labelColor.withAppearanceAlpha(0.08))
            chCard.identifier = NSUserInterfaceItemIdentifier("settings_channel:\(ch.id)")

            let chBadge = roundedView(NSRect(x: 12, y: cardHeight - 34, width: 24, height: 24), radius: 6,
                color: CitoPalette.badgeFill(ch.tint), border: .white.withAlphaComponent(0.18))
            let chIcon = NSImageView(frame: NSRect(x: 5, y: 5, width: 14, height: 14))
            configureBadge(chBadge,icon:chIcon,id:ch.id,symbol:ch.sf_symbol,title:ch.display_name,tint:CitoPalette.badgeFill(ch.tint),inset:5)
            chBadge.identifier = NSUserInterfaceItemIdentifier("settings_channel_badge:\(ch.id)")
            chBadge.addSubview(chIcon)
            chCard.addSubview(chBadge)

            let chTitle = textLabel(ch.display_name, NSRect(x: 44, y: cardHeight - 26, width: 180, height: 16), size: 12.5, weight: .semibold)
            chTitle.lineBreakMode = .byTruncatingMiddle; chTitle.toolTip = ch.display_name
            chCard.addSubview(chTitle)
            let chStatusText:String
            let chStatusColor:NSColor
            switch channelResult?.ok == false ? "error" : (ch.status == "ready" && ch.inbox_paused == true ? "inbox_paused" : ch.status) {
            case "ready": chStatusText = "Connected"; chStatusColor = CitoPalette.successText
            case "paused": chStatusText = "Paused"; chStatusColor = CitoPalette.warningText
            case "inbox_paused": chStatusText = "Inbox paused"; chStatusColor = CitoPalette.warningText
            case "pairing_pending": chStatusText = "◌ Pairing…"; chStatusColor = CitoPalette.infoText
            case "error": chStatusText = "⚠ Error"; chStatusColor = CitoPalette.dangerText
            default: chStatusText = ch.hub_provided || ch.status == "hub_ready" ? "Account Not Paired" : "Not Set Up"; chStatusColor = CitoPalette.secondaryText
            }
            let chStatusLabel = statusLabel(chStatusText,frame:NSRect(x:240,y:cardHeight-26,width:226,height:16),color:chStatusColor,filled:ch.status == "ready")
            chStatusLabel.identifier = NSUserInterfaceItemIdentifier("settings_channel_status:\(ch.id)")
            chCard.addSubview(chStatusLabel)
            layoutBar(in:chCard,insetLeft:44,insetRight:14,y:cardHeight-30,height:16,flexible:chTitle,trailing:[chStatusLabel])

            let chDetail = textLabel(presentationError(channelResult?.detail ?? ch.detail), NSRect(x: 44, y: cardHeight - 32 - detailHeight, width: 422, height: detailHeight), size: 10.5, color:channelResult?.ok == false || ch.status == "error" ? CitoPalette.dangerText : (ch.warning == true ? CitoPalette.warningText : CitoPalette.secondaryText))
            chDetail.maximumNumberOfLines=0; chDetail.lineBreakMode = .byWordWrapping; chDetail.toolTip = chDetail.stringValue
            chDetail.identifier = NSUserInterfaceItemIdentifier("settings_channel_detail:\(ch.id)")
            if showDetail { chCard.addSubview(chDetail) }
            if ch.status == "error" {
                var recovery:[NSView]=[]
                let actions = [("Retry","retry",true),("Change Token…","change",ch.can_configure_hub == true),("Clear Token","clear",ch.can_clear_hub == true),("Send Test","test",ch.can_test == true && ch.configured),("Unpair","unpair",ch.can_unpair == true)]
                for (title,id,allowed) in actions where allowed {
                    let button=MenuControl(title,frame:NSRect(x:0,y:8,width:MenuControl.intrinsicWidth(title,font:.systemFont(ofSize:10)),height:22))
                    button.label.font = .systemFont(ofSize:10)
                    button.identifier=NSUserInterfaceItemIdentifier("recover_channel:\(ch.id):\(id)")
                    button.style = ["clear","unpair"].contains(id) ? .destructiveHover : .neutral
                    button.pressed={ [weak self] in guard let self=self else{return}; switch id {
                    case "clear": self.emit(["op":"disconnect_human_channel","hub":self.effectiveChannelHub,"channel_id":ch.id,"reset_hub":true,"reset_user":false])
                    case "test": self.emit(["op":"channel_test","hub":self.effectiveChannelHub,"channel_id":ch.id,"values":self.channelUiStates[channelKey]?.drafts ?? [:]])
                    case "unpair": self.emit(["op":"channel_unpair","hub":self.effectiveChannelHub,"channel_id":ch.id,"reset_user":true])
                    default: self.channelUiStates[channelKey,default:ChannelUiState()].editingGateway = id == "change"; self.emit(["op":"open_settings","hub":self.effectiveChannelHub])
                    } }
                    chCard.addSubview(button); recovery.append(button)
                }
                layoutBar(in:chCard,insetLeft:12,insetRight:12,y:unavailable ? 8 : cardHeight-headerHeight-28,height:22,spacing:4,trailing:recovery)
                if unavailable {
                    channelsView.addSubview(chCard)
                    chCursorY -= 10
                    continue
                }
            }

            let hubStratum = roundedView(NSRect(x:12,y:8+userStratumH+(gatewayActive ? 6 : 0),width:456,height:hubStratumH),radius:8,
                color:.labelColor.withAppearanceAlpha(0.035),border:nil)
            hubStratum.identifier=NSUserInterfaceItemIdentifier("stratum_hub:\(ch.id)")
            var gatewayTop: [NSView] = [], gatewayLeading: [NSView] = [], gatewayTrailing: [NSView] = []
            var personalActions: [NSView] = [], pairingActions: [NSView] = []
            let cleanConnectedState = personalCollapsed && !hubGatewayExpanded
            let hubTitle=textLabel(ch.configured || !showHubStratum ? "Hub Service" : "1. Shared Hub Service",NSRect(x:10,y:hubStratumH-25,width:168,height:14),size:9.5,weight:.bold,color:CitoPalette.secondaryText)
            hubTitle.identifier=NSUserInterfaceItemIdentifier("stratum_hub_title:\(ch.id)")
            if !gatewayRole.isEmpty { hubTitle.toolTip = gatewayRole }
            hubStratum.addSubview(hubTitle)
            let gatewayStatus=statusLabel(ch.error_origin == "hub" ? "Service needs attention" : (gatewayActive ? "Service Ready" : "Service Token Required"),frame:NSRect(x:0,y:hubStratumH-25,width:164,height:14),color:ch.error_origin == "hub" ? CitoPalette.dangerText : (gatewayActive ? CitoPalette.successText : CitoPalette.secondaryText),filled:gatewayActive,size:9)
            gatewayStatus.identifier=NSUserInterfaceItemIdentifier("stratum_hub_status:\(ch.id)")
            gatewayStatus.isHidden = cleanConnectedState && ch.error_origin != "hub"
            hubStratum.addSubview(gatewayStatus)
            if ch.can_clear_hub == true {
                let clear=MenuControl("Clear Token",frame:NSRect(x:0,y:7,width:MenuControl.intrinsicWidth("Clear Token", font:.systemFont(ofSize:10.5)),height:22),radius:6)
                clear.label.font = .systemFont(ofSize:10.5)
                clear.style = .destructiveHover
                clear.identifier=NSUserInterfaceItemIdentifier("clear_channel_service:\(ch.id)")
                clear.pressed={ [weak self] in guard let self=self else{return}; self.emit(["op":"disconnect_human_channel","hub":self.effectiveChannelHub,"channel_id":ch.id,"reset_hub":true,"reset_user":false]) }
                pendingLabel(clear,isPending("disconnect_human_channel",hub:effectiveChannelHub,target:ch.id) || isPending("clear_channel_service",hub:effectiveChannelHub,target:ch.id),"Clearing…")
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
            let userTitle=textLabel((ch.configured || !showHubStratum ? "" : "2. ") + "Your Personal Account (@\(currentUser))",NSRect(x:12,y:userStratumH-21,width:432,height:13),size:9.5,weight:.bold,color:CitoPalette.secondaryText)
            userTitle.lineBreakMode = .byTruncatingMiddle; userTitle.toolTip = userTitle.stringValue
            userTitle.identifier=NSUserInterfaceItemIdentifier("stratum_user_title:\(ch.id)")
            if !personalCollapsed { userStratum.addSubview(userTitle) }
            for (container, fields, top) in [(hubStratum,hubGatewayExpanded ? hubFields : [],hubStratumH-34-gatewayRoleH-guideH),(userStratum,gatewayActive && !personalCollapsed ? formFields : [],userStratumH-40)] {
                let labelW = max(56, min(110, (fields.map { ceil(measureTextWidth($0.label, font: .systemFont(ofSize: 11, weight: .medium))) }.max() ?? 56) + 6))
                let inputX = 12 + labelW + 10
                let inputW = container.bounds.width - 12 - inputX
                var fieldY=top-22
                for f in fields {
                    let fieldId="channel_field:\(ch.id):\(f.key)"
                    let labelView=textLabel(f.label,NSRect(x:12,y:fieldY+3,width:labelW,height:15),size:11,weight:.medium)
                    labelView.toolTip=f.label; container.addSubview(labelView)
                    let initialVal=channelUi.drafts[f.key] ?? (f.value ?? "")
                    let inputField=makeEditableField(fieldId,frame:NSRect(x:inputX,y:fieldY,width:inputW,height:22),placeholder:f.type=="secret" && f.configured ? (f.configured_placeholder ?? "•••••••• (Saved — enter new value to replace)") : f.placeholder,value:initialVal,secret:f.type=="secret",accessibilityLabel:f.label)
                    inputField.toolTip=f.help_text; container.addSubview(inputField)
                    fieldY-=26
                }
            }
            if showHubStratum { chCard.addSubview(hubStratum) }
            if gatewayActive { chCard.addSubview(userStratum) }
            func gate(_ control: MenuControl) { control.isEnabled=gatewayActive }
            if !instruction.isEmpty {
                let instrLabel = textLabel(instruction, NSRect(x: 12, y: userStratumH - 40 - CGFloat(formFields.count) * 26 - instructionHeight, width: 432, height: instructionHeight), size: 10.5, weight: .semibold, color: CitoPalette.infoText)
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
                    pendingLabel(save,channelSavePending("save_hub_service",hub:effectiveChannelHub,channel:ch.id,scope:"hub"),"Saving…")
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
            if disclosure && !personalCollapsed && gatewayActive {
                let manual=MenuControl(manualExpanded ? "▾ Hide manual entry" : "▸ Enter \(userFields.first?.label ?? "account") manually…",frame:NSRect(x:12,y:instruction.isEmpty ? 38 : 66,width:190,height:22),paddingX:0)
                manual.label.alignment = .left; manual.layoutContents()
                manual.style = .subtleHeader; manual.identifier=NSUserInterfaceItemIdentifier("manual_channel:\(ch.id)")
                manual.pressed={ [weak self] in guard let self=self else{return}; self.channelUiStates[channelKey, default: ChannelUiState()].manualEntry.toggle(); self.rebuildSettingsPanel() }
                userStratum.addSubview(manual)
            }
            if ch.can_pair == true && !personalCollapsed && gatewayActive && instruction.isEmpty {
                let pairLabel = ch.configured ? "Re-pair…" : (ch.pairing_action_label ?? "Pair Account…")
                let pairBtn = MenuControl(pairLabel, frame: NSRect(x: 12, y: 6, width: MenuControl.intrinsicWidth(pairLabel, leadingSymbol: "link"), height: 24), leadingSymbol: "link", radius: 6)
                pairBtn.toolTip=ch.pairing_help_text
                pairBtn.identifier = NSUserInterfaceItemIdentifier("pair_channel:\(ch.id)")
                pairBtn.pressed = { [weak self] in
                    guard let self = self else { return }
                    self.emit(["op": "channel_pair", "hub": self.effectiveChannelHub, "channel_id": chId, "values": collectValues("user")])
                }
                gate(pairBtn); pendingLabel(pairBtn,channelUi.pendingPairRequest != nil,"Pairing…"); pairBtn.style = ch.configured ? .neutral : .prominent; userStratum.addSubview(pairBtn); personalActions.append(pairBtn)
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
                pendingLabel(cancel,isPending("channel_cancel_pair",hub:effectiveChannelHub,target:ch.id),"Cancelling…")
                userStratum.addSubview(cancel); pairingActions.append(cancel)
            }
            if ch.configured {
                let toggleTitle=(ch.enabled ? "Pause " : "Resume ") + ch.display_name
                let toggleWidth=MenuControl.intrinsicWidth(toggleTitle,font:.systemFont(ofSize:10.5))
                let toggle=MenuControl(toggleTitle,frame:NSRect(x:130,y:6,width:toggleWidth,height:24),radius:6)
                toggle.identifier=NSUserInterfaceItemIdentifier("toggle_channel:\(ch.id)")
                toggle.maximumWidth=170
                toggle.style = ch.enabled ? .neutral : .prominent
                toggle.pressed={ [weak self] in guard let self=self else{return}; self.emit(["op":"channel_toggle","hub":self.effectiveChannelHub,"channel_id":chId,"enabled":!ch.enabled]) }
                pendingLabel(toggle,isPending("channel_toggle",hub:effectiveChannelHub,target:ch.id),ch.enabled ? "Pausing…" : "Resuming…")
                userStratum.addSubview(toggle); personalActions.append(toggle)
                let unpair=MenuControl("Unpair",frame:NSRect(x:196,y:6,width:MenuControl.intrinsicWidth("Unpair"),height:24),radius:6)
                unpair.style = .destructiveHover
                unpair.identifier=NSUserInterfaceItemIdentifier("unpair_channel:\(ch.id)")
                unpair.pressed={ [weak self] in guard let self=self else{return}; self.emit(["op":"channel_unpair","hub":self.effectiveChannelHub,"channel_id":chId,"reset_user":true]) }
                unpair.isHidden = ch.can_unpair != true
                pendingLabel(unpair,isPending("channel_unpair",hub:effectiveChannelHub,target:ch.id),"Unpairing…")
                userStratum.addSubview(unpair); personalActions.append(unpair)
            }
            let testBtn = MenuControl("Send Test", frame: NSRect(x: 290, y: 6, width: MenuControl.intrinsicWidth("Send Test"), height: 24), radius: 6)
            testBtn.identifier = NSUserInterfaceItemIdentifier("test_channel:\(ch.id)")
            testBtn.pressed = { [weak self] in
                guard let self = self else { return }
                self.emit(["op": "channel_test", "hub": self.effectiveChannelHub, "channel_id": chId, "values": collectValues("user").merging(collectValues("hub")) { _,new in new }])
            }
            testBtn.isHidden = ch.can_test != true || (ch.status == "error" && !ch.configured) || (disclosure && !ch.configured && !manualExpanded)
            testBtn.isEnabled = ch.canSendTest(drafts:channelUi.drafts,visibleFields:formFields)
            pendingLabel(testBtn,isPending("channel_test",hub:effectiveChannelHub,target:ch.id),"Testing…")
            userStratum.addSubview(testBtn)

            let saveBtn = MenuControl("Save", frame: NSRect(x: 376, y: 6, width: 68, height: 24), radius: 6)
            saveBtn.identifier = NSUserInterfaceItemIdentifier("save_channel:\(ch.id)")
            saveBtn.pendingWidth = MenuControl.intrinsicWidth("Saving…")
            saveBtn.style = .prominent
            saveBtn.pressed = { [weak self] in
                guard let self = self else { return }
                let values = collectValues("user")
                self.emit(["op": "channel_save", "hub": self.effectiveChannelHub, "channel_id": chId, "enabled": true, "values": values])
            }
            gate(saveBtn)
            if ch.can_edit_personal == true && !personalCollapsed && gatewayActive && (!disclosure || manualExpanded) { userStratum.addSubview(saveBtn) } else { saveBtn.isHidden=true }
            if Date() < (channelUi.savedUntil["user"] ?? .distantPast) { saveBtn.copied("✓ Saved") }
            pendingLabel(saveBtn,channelSavePending("channel_save",hub:effectiveChannelHub,channel:ch.id,scope:"user"),"Saving…")

            layoutBar(in: hubStratum, y: hubStratumH-29, height: 22, flexible: hubTitle, trailing: [gatewayStatus] + gatewayTop)
            if hubGatewayExpanded { layoutBar(in: hubStratum, y: 8, height: 24, leading: gatewayLeading, trailing: gatewayTrailing) }
            if personalCollapsed {
                let chatId = userFields.first?.value ?? ""
                let saved = Date() < (channelUi.savedUntil["user"] ?? .distantPast)
                let summary = textLabel(saved ? "✓ Saved" : "Paired · \(ch.display_label ?? chatId)", NSRect(x:12,y:10,width:188,height:16),size:10,color:saved ? CitoPalette.successText : CitoPalette.secondaryText)
                summary.identifier = NSUserInterfaceItemIdentifier("channel_summary:\(ch.id)"); summary.toolTip = summary.stringValue; summary.lineBreakMode = .byTruncatingMiddle
                userStratum.addSubview(summary)
                let edit = MenuControl("Edit…",frame:NSRect(x:0,y:6,width:MenuControl.intrinsicWidth("Edit…"),height:24))
                edit.identifier = NSUserInterfaceItemIdentifier("edit_user_channel:\(ch.id)"); edit.style = .neutral
                edit.pressed = { [weak self] in self?.channelUiStates[channelKey, default: ChannelUiState()].editingPersonal = true; self?.channelUiStates[channelKey, default: ChannelUiState()].manualEntry = true; self?.rebuildSettingsPanel() }
                edit.isHidden = ch.can_edit_personal != true
                userStratum.addSubview(edit)
                for case let button as MenuControl in [testBtn, edit] + personalActions {
                    button.label.font = .systemFont(ofSize: 10.5)
                }
                let toggle = personalActions.first { $0.identifier?.rawValue == "toggle_channel:\(ch.id)" }
                let unpair = personalActions.first { $0.identifier?.rawValue == "unpair_channel:\(ch.id)" }
                layoutBar(in:userStratum,y:7,height:24,spacing:8,flexible:summary,trailing:[testBtn] + [toggle].compactMap { $0 } + [edit] + [unpair].compactMap { $0 })
            } else if gatewayActive {
                var topActions: [NSView] = []
                if ch.configured {
                    topActions = personalActions.filter { $0.identifier?.rawValue.hasPrefix("toggle_channel:") == true || $0.identifier?.rawValue.hasPrefix("unpair_channel:") == true }
                    personalActions.removeAll { view in topActions.contains { $0 === view } }
                    layoutBar(in:userStratum,y:userStratumH-31,height:24,flexible:userTitle,trailing:topActions)
                }
                var trailing: [NSView] = [testBtn,saveBtn]
                if channelUi.editingPersonal && ch.configured {
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

    }

    func buildHubsTab(in root: NSView) {
        // Tab 3: Hubs & Network (520 × 368 pt, y: 44)
        let hubsView = NSView(frame: NSRect(x: 0, y: 44, width: 520, height: 368))
        hubsView.identifier = NSUserInterfaceItemIdentifier("settings_hubs_tab")
        hubsView.isHidden = settingsActiveTab != "hubs"

        let hostHub = settings?.host_hub ?? HostHubSummary(
            enabled: false, running: false, port: 4291,
            endpoint_url: "http://127.0.0.1:4291", token: "",
            invite_command: "cito setup --hub lan --url http://127.0.0.1:4291 --default", auth_mode: nil, error: nil
        )
        let hubError=(hostHub.error?.trimmingCharacters(in:.whitespacesAndNewlines)).flatMap{$0.isEmpty ? nil : $0}
        let hostingUnavailable = !hostHub.enabled || !hostHub.running || hubError != nil
        let hostError = (hostHubPortError ?? hubError).flatMap { $0.isEmpty ? nil : $0 }
        let hostExpanded = hostHub.enabled || hostHubPortPending || hostHubPortDraft != nil || hostError != nil
        let hasAddressChoice = hostHub.tailscale_url != nil && hostHub.lan_url != nil && hostHub.tailscale_url != hostHub.lan_url
        let hostNotice = hostError ?? (!hostHub.enabled && hostExpanded ? "Choose Start Sharing to accept connections" : nil)
        let hostHeight: CGFloat = hostExpanded ? (hasAddressChoice ? 160 : 120) + (hostNotice == nil ? 0 : 28) : 46
        let hostCard = roundedView(NSRect(x: CitoMetrics.settingsInset, y: 356 - hostHeight, width: 480, height: hostHeight), radius: 10,
            color: .labelColor.withAppearanceAlpha(0.045), border: .labelColor.withAppearanceAlpha(0.08))
        hostCard.identifier = NSUserInterfaceItemIdentifier("host_hub_card")
        let hostTitle=textLabel(hostHub.enabled && !hostHub.running && hostError == nil ? "Share This Mac as a Hub · Starting…" : "Share This Mac as a Hub", NSRect(x:14,y:0,width:280,height:17),size:13,weight:.semibold)
        hostTitle.identifier = NSUserInterfaceItemIdentifier("host_hub_title")
        hostCard.addSubview(hostTitle)
        if hostExpanded, let error = hostNotice {
            let subtitle = textLabel(presentationError(error), NSRect(x:14,y:hostHeight-52,width:452,height:18), size:11, color:hostError == nil ? CitoPalette.secondaryText : CitoPalette.dangerText)
            subtitle.identifier = NSUserInterfaceItemIdentifier("host_hub_error")
            subtitle.toolTip = presentationError(error)
            hostCard.addSubview(subtitle)
        }

        let toggleTitle = hostHub.enabled ? "Stop Sharing" : "Start Sharing"
        let toggleHostBtn = MenuControl(toggleTitle, frame: NSRect(x:0,y:0,width:MenuControl.intrinsicWidth(toggleTitle),height:24),radius:6)
        toggleHostBtn.identifier = NSUserInterfaceItemIdentifier("toggle_host_hub")
        toggleHostBtn.style = .neutral
        let hostStatus=roundedView(NSRect(x:0,y:0,width:6,height:6),radius:3,color:hubError != nil ? CitoPalette.dangerText : hostHub.running ? CitoPalette.successText : CitoPalette.secondaryText)
        hostStatus.identifier=NSUserInterfaceItemIdentifier("host_hub_status")
        hostStatus.toolTip=hubError.map(presentationError) ?? (hostHub.running ? "Sharing is running" : hostHub.enabled ? "Sharing is starting" : "Sharing is stopped")
        hostStatus.setAccessibilityElement(true); hostStatus.setAccessibilityRole(.staticText); hostStatus.setAccessibilityLabel(hostStatus.toolTip)
        hostCard.addSubview(hostStatus)
        let nextHostEnabled = !hostHub.enabled
        toggleHostBtn.pressed = { [weak self] in
            guard let self=self else{return}
            guard let port = self.validHostPort(fallback:hostHub.port, fallbackOnInvalid:!nextHostEnabled) else { return }
            self.emit(["op": "set_host_hub", "enabled": nextHostEnabled, "port": port])
        }
        pendingLabel(toggleHostBtn,isPending("set_host_hub",target:"sharing"),hostHub.enabled ? "Stopping…" : "Starting…")
        hostCard.addSubview(toggleHostBtn)
        layoutBar(in:hostCard,insetLeft:14,insetRight:14,y:hostExpanded ? hostHeight-34 : 11,height:24,flexible:hostTitle,trailing:[hostStatus,toggleHostBtn])

        if hostExpanded {
        let selectedLan = hostHubAddress == "lan" || hostHub.tailscale_url == nil
        let selectedEndpoint = (selectedLan ? hostHub.lan_url : hostHub.tailscale_url) ?? hostHub.endpoint_url
        let selectedInvite = (selectedLan ? hostHub.lan_invite_command : hostHub.tailscale_invite_command) ?? hostHub.invite_command
        if hasAddressChoice {
            let addressTrack = makePillTrack(NSRect(x:14,y:82,width:0,height:26),radius:8)
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
        copyInvite.isEnabled = hostHub.enabled && hostHub.running
        if !hostHub.enabled { copyInvite.toolTip = "Enable sharing and apply the port before inviting other Macs" }
        else if hostingUnavailable { copyInvite.toolTip = "Hub sharing is unavailable. This invite will work after the server recovers." }
        let inviteCmd = selectedInvite
        copyInvite.pressed = { [weak self, weak copyInvite] in
            self?.copyText(inviteCmd, control:copyInvite, feedback:"✓ Invite Copied")
        }
        hostCard.addSubview(copyInvite); inviteActions.append(copyInvite)
        let portLabel = textLabel("Port:",NSRect(x:0,y:12,width:ceil(measureTextWidth("Port:", font:.systemFont(ofSize:11))) + 4,height:15),size:11)
        let portField = makeEditableField("host_hub_port",frame:NSRect(x:0,y:12,width:58,height:24),placeholder:"Port",value:hostHubPortDraft ?? "\(hostHub.port)",accessibilityLabel:"Host Hub port")
        hostCard.addSubview(portLabel); hostCard.addSubview(portField)
        let auth=MenuControl(authMode=="none" ? "Access: Trusted" : "Access: Token",frame:NSRect(x:286,y:12,width:112,height:24),radius:6)
        auth.style = .neutral
        auth.identifier=NSUserInterfaceItemIdentifier("toggle_host_hub_auth")
        auth.pressed={ [weak self] in
            guard let self=self else{return}
            guard let port = self.validHostPort(fallback:hostHub.port) else { return }
            self.emit(["op":"set_host_hub","enabled":hostHub.enabled,"port":port,"auth_mode":authMode=="none" ? "token" : "none"])
        }
        pendingLabel(auth,isPending("set_host_hub",target:"access"),"Applying…")
        hostCard.addSubview(auth)
        let apply=MenuControl("Apply",frame:NSRect(x:394,y:12,width:72,height:24),radius:6)
        apply.identifier=NSUserInterfaceItemIdentifier("apply_host_hub_port")
        apply.isEnabled = !hostHubPortPending && Int((hostHubPortDraft ?? "\(hostHub.port)").trimmingCharacters(in: .whitespacesAndNewlines)) != hostHub.port
        if hostHubPortSaved && Date() < hostHubPortSavedUntil { apply.copied("✓ Saved") }
        pendingLabel(apply,hostHubPortPending,"Applying…")
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
            self.rebuildSettingsPanel()
        }
        hostCard.addSubview(apply)
        layoutBar(in:hostCard,insetLeft:14,insetRight:14,y:48,height:24,flexible:endpointLabel,trailing:inviteActions)
        let copyToken=MenuControl("Copy Token",frame:NSRect(x:0,y:12,width:90,height:24))
        copyToken.identifier=NSUserInterfaceItemIdentifier("copy_host_hub_token"); copyToken.isHidden=authMode != "token" || hostHub.token.isEmpty
        copyToken.pressed={ [weak self, weak copyToken] in self?.copyText(hostHub.token,control:copyToken) }
        hostCard.addSubview(copyToken)
        layoutBar(in:hostCard,insetLeft:14,insetRight:14,y:12,height:24,leading:[portLabel,portField,apply,copyToken],trailing:[auth])
        }
        hubsView.addSubview(hostCard)
        let hubOffset: CGFloat = hostExpanded ? -(hostHeight - 136) : 80

        let connectedTitle=textLabel("CONNECTED HUBS",NSRect(x:20,y:194+hubOffset,width:180,height:14),size:10,weight:.semibold,color:CitoPalette.secondaryText)
        hubsView.addSubview(connectedTitle)

        let pasteBtn = MenuControl("Join from Clipboard", frame: NSRect(x: 250, y: 190 + hubOffset, width: MenuControl.intrinsicWidth("Join from Clipboard", leadingSymbol:"doc.on.clipboard"), height: 22), leadingSymbol: "doc.on.clipboard", radius: 6)
        pasteBtn.identifier = NSUserInterfaceItemIdentifier("paste_hub_invite")
        if Date() < hubInvitePastedUntil { pasteBtn.copied("✓ Joined") }
        pendingLabel(pasteBtn,hubInvitePending,"Joining…")
        pasteBtn.pressed = { [weak self] in self?.handlePasteHubInvite() }
        hubsView.addSubview(pasteBtn)

        let addBtn = MenuControl("Add Hub…", frame: NSRect(x: 396, y: 190 + hubOffset, width: 104, height: 22), leadingSymbol: "plus", radius: 6)
        addBtn.identifier = NSUserInterfaceItemIdentifier("open_add_hub")
        addBtn.pressed = { [weak self] in
            guard let self = self else { return }
            let wasEditing = self.editingExistingHub
            self.editingExistingHub=false
            self.editingOriginalHub=""; self.editingInitialHub=nil; self.hubSaveAwaitingSettings=nil
            self.addHubName=""; self.addHubUrl=""; self.addHubToken=""; self.addHubUser=""; self.addHubIssuer=""; self.addHubClientId=""; self.addHubRedirectPort=""; self.addHubAuth="none"; self.addHubAuthExplicit=false
            self.addHubDraftGeneration += 1
            self.addHubError=nil; self.pendingHubSave=nil; self.inviteParseRequest=""
            self.addHubFormOpen = wasEditing ? true : !self.addHubFormOpen
            self.rebuildSettingsPanel(revealAddHub: self.addHubFormOpen)
        }
        addBtn.isEnabled = pendingHubSave == nil
        hubsView.addSubview(addBtn)
        layoutBar(in:hubsView,insetLeft:20,insetRight:20,y:190+hubOffset,height:22,flexible:connectedTitle,trailing:[pasteBtn,addBtn])

        if addHubFormOpen {
            let editingLocalHub = editingBuiltinHub
            let errorText = addHubError.map(presentationError)
            let errorHeight: CGFloat = errorText.map { ceil(($0 as NSString).boundingRect(with:NSSize(width:452,height:CGFloat.greatestFiniteMagnitude),options:[.usesLineFragmentOrigin],attributes:[.font:NSFont.systemFont(ofSize:10)]).height) + 4 } ?? 0
            let errorSpace: CGFloat = errorText == nil ? 0 : errorHeight + 6
            let formHeight: CGFloat = (editingLocalHub ? 124 : addHubAuth == "oidc" ? 298 : 252) + errorSpace
            let form = roundedView(NSRect(x: 20, y: 180 + hubOffset - formHeight, width: 480, height: formHeight), radius: 10,
                color: .labelColor.withAppearanceAlpha(0.045), border: .labelColor.withAppearanceAlpha(0.08))
            form.identifier = NSUserInterfaceItemIdentifier("add_hub_form")

            func fieldLabel(_ title: String, _ frame: NSRect) {
                form.addSubview(textLabel(title,frame,size:10,weight:.semibold,color:CitoPalette.secondaryText))
            }
            if editingLocalHub {
                form.addSubview(textLabel("This Mac (Local Hub)",NSRect(x:14,y:94,width:452,height:18),size:12.5,weight:.semibold))
                fieldLabel("Username (Optional)",NSRect(x:14,y:72,width:452,height:14))
                form.addSubview(makeEditableField("settings_hub_user",frame:NSRect(x:14,y:44,width:452,height:24),placeholder:"Defaults to your Mac username",value:addHubUser,accessibilityLabel:"Hub username"))
            } else {
                fieldLabel("Hub URL",NSRect(x:14,y:179,width:298,height:13))
                fieldLabel("Hub Name (Optional)",NSRect(x:320,y:179,width:146,height:13))
                form.addSubview(makeEditableField("settings_hub_url",frame:NSRect(x:14,y:153,width:298,height:24),placeholder:"http://192.168.1.10:4291",value:addHubUrl,accessibilityLabel:"Hub URL"))
                form.addSubview(makeEditableField("settings_hub_name",frame:NSRect(x:320,y:153,width:146,height:24),placeholder:hubNamePlaceholder,value:addHubName,accessibilityLabel:"Hub name"))
                fieldLabel("Access",NSRect(x:14,y:125,width:48,height:14))
                let authOptions = [("none","Trusted Network"),("token","Shared Token"),("oidc","Team Sign-In")]
                let authTrack = makePillTrack(NSRect(x:68,y:118,width:398,height:26),radius:13,bordered:false)
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
                        self.addHubAuth=mode; self.addHubAuthExplicit=true; self.rebuildSettingsPanel()
                    }
                    authTrack.addSubview(pill); authPills.append(pill)
                }
                layoutBar(in: authTrack, insetLeft: 2, insetRight: 2, y: 2, height: 22, spacing: 4, leading: authPills)
                authTrack.frame.size.width = authPills.reduce(CGFloat(4)) { $0 + $1.frame.width } + CGFloat(max(0,authPills.count-1)) * 4
                form.addSubview(authTrack)
                if addHubAuth == "none" {
                    fieldLabel("Username (Optional)",NSRect(x:14,y:97,width:452,height:14))
                    form.addSubview(makeEditableField("settings_hub_user",frame:NSRect(x:14,y:71,width:452,height:24),placeholder:"Defaults to your Mac username",value:addHubUser,accessibilityLabel:"Hub username"))
                } else if addHubAuth == "token" {
                    fieldLabel("Shared Access Token",NSRect(x:14,y:97,width:274,height:14))
                    fieldLabel("Username (Optional)",NSRect(x:296,y:97,width:170,height:14))
                    form.addSubview(makeEditableField("settings_hub_token",frame:NSRect(x:14,y:71,width:274,height:24),placeholder:editingExistingHub ? "•••••• (Saved — enter new token to replace)" : "Paste token from host Mac",value:addHubToken,secret:true,accessibilityLabel:"Hub token"))
                    form.addSubview(makeEditableField("settings_hub_user",frame:NSRect(x:296,y:71,width:170,height:24),placeholder:"e.g. alice",value:addHubUser,accessibilityLabel:"Hub username"))
                } else if addHubAuth == "oidc" {
                    for control in form.subviews { control.frame.origin.y += 46 }
                    fieldLabel("OIDC Issuer URL",NSRect(x:14,y:143,width:452,height:14))
                    form.addSubview(makeEditableField("settings_hub_issuer",frame:NSRect(x:14,y:117,width:452,height:24),placeholder:"Auto-discovered",value:addHubIssuer,accessibilityLabel:"OIDC issuer URL"))
                    fieldLabel("OIDC Client ID",NSRect(x:14,y:97,width:222,height:14))
                    fieldLabel("Redirect Port",NSRect(x:244,y:97,width:222,height:14))
                    form.addSubview(makeEditableField("settings_hub_client_id",frame:NSRect(x:14,y:71,width:222,height:24),placeholder:"Auto-discovered",value:addHubClientId,accessibilityLabel:"OIDC client ID"))
                    form.addSubview(makeEditableField("settings_hub_redirect_port",frame:NSRect(x:244,y:71,width:222,height:24),placeholder:"Auto-discovered",value:addHubRedirectPort,accessibilityLabel:"OIDC redirect port"))
                }
            }

            if !editingBuiltinHub {
                for control in form.subviews { control.frame.origin.y += 48 }
                let visibility = textLabel("Personal Hubs connect your Macs automatically; Team Sign-In (OIDC) Hubs start private to your account (@user) until shared with Team.", NSRect(x: 14, y: 60, width: 452, height: 48), size: 11, color: CitoPalette.secondaryText)
                visibility.identifier = NSUserInterfaceItemIdentifier("hub_visibility_caption")
                visibility.maximumNumberOfLines = 3
                visibility.cell?.wraps = true
                form.addSubview(visibility)
            }
            // Keep the error between the fields/caption and the fixed action row.
            for control in form.subviews { control.frame.origin.y += errorSpace }
            if let error = errorText {
                let label=textLabel(error,NSRect(x:14,y:40,width:452,height:errorHeight),size:10,color:CitoPalette.dangerText)
                label.identifier=NSUserInterfaceItemIdentifier("add_hub_error"); label.toolTip=error
                label.maximumNumberOfLines = 0; label.lineBreakMode = .byWordWrapping; label.cell?.wraps = true
                form.addSubview(label)
            }
            let cancelBtn = MenuControl("Cancel", frame: NSRect(x: 306, y: 12, width: 72, height: 24), radius: 6)
            cancelBtn.identifier = NSUserInterfaceItemIdentifier("cancel_add_hub")
            cancelBtn.pressed = { [weak self] in
                guard let self = self else { return }
                self.addHubFormOpen = false
                self.inviteParseRequest=""
                self.pendingHubSave = nil; self.hubSaveAwaitingSettings = nil
                self.rebuildSettingsPanel()
            }
            form.addSubview(cancelBtn)

            let submitBtn = MenuControl(editingExistingHub ? (editingBuiltinHub ? "Save" : "Save Hub") : "Join Hub", frame: NSRect(x: 386, y: 12, width: 80, height: 24), radius: 6)
            submitBtn.identifier = NSUserInterfaceItemIdentifier("submit_add_hub")
            submitBtn.style = .prominent
            pendingLabel(submitBtn,pendingHubSave != nil,editingExistingHub ? "Saving…" : "Adding…")
            submitBtn.pressed = { [weak self] in
                guard let self = self else { return }
                self.syncAddHubFieldsFromControls()
                let url = normalizedHubURL(self.addHubUrl)
                let knownHubs = self.settings?.hubs ?? []
                let name = proposedHubName(self.addHubName,url:url,existing:knownHubs,creating:!self.editingExistingHub,original:self.editingOriginalHub)
                let validation: String?
                if name.isEmpty { validation="Hub name must contain a letter or number." }
                else if !self.editingExistingHub && knownHubs.contains(where:{$0.name == name && ($0.is_builtin == true || $0.url != url)}) { validation="Hub name already exists. Choose a different name." }
                else if !["http://","https://","local://"].contains(where:{url.hasPrefix($0)}) { validation="Hub URL must start with http:// or https://." }
                else if !self.editingExistingHub && self.addHubAuth == "token" && self.addHubToken.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty { validation="Shared access token is required." }
                else if self.addHubAuth == "oidc" && !self.addHubRedirectPort.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty && !(Int(self.addHubRedirectPort.trimmingCharacters(in:.whitespacesAndNewlines)).map { (0...65535).contains($0) } ?? false) { validation="Redirect port must be an integer from 0 to 65535." }
                else { validation=nil }
                if let error=validation { self.addHubError=error; self.rebuildSettingsPanel(); return }
                var action: [String: Any] = [
                    "op": "setup_save_hub",
                    "name": name,
                    "url": url,
                    "auth_mode": self.addHubAuth,
                    "set_default": !self.editingExistingHub
                ]
                if !self.editingExistingHub && !self.addHubAuthExplicit && self.addHubAuth == "none" { action.removeValue(forKey:"auth_mode") }
                if self.editingExistingHub { action["edit"]=true; action["original_name"]=self.editingOriginalHub; action.removeValue(forKey:"set_default") }
                let tok = self.addHubToken.trimmingCharacters(in: .whitespacesAndNewlines)
                let usr = self.addHubUser.trimmingCharacters(in: .whitespacesAndNewlines)
                let iss = self.addHubIssuer.trimmingCharacters(in: .whitespacesAndNewlines)
                let cid = self.addHubClientId.trimmingCharacters(in: .whitespacesAndNewlines)
                if self.addHubAuth == "token", !tok.isEmpty { action["token"] = tok }
                if self.addHubAuth == "none" || self.addHubAuth == "token" { if !usr.isEmpty { action["user"]=usr } else if self.editingExistingHub { action["user"]=NSNull() } }
                if self.addHubAuth == "oidc" {
                    if !iss.isEmpty { action["issuer"] = iss } else if self.editingExistingHub { action["issuer"] = NSNull() }
                    if !cid.isEmpty { action["client_id"] = cid } else if self.editingExistingHub { action["client_id"] = NSNull() }
                    if let port = Int(self.addHubRedirectPort.trimmingCharacters(in:.whitespacesAndNewlines)) { action["redirect_port"] = port }
                    else if self.editingExistingHub { action["redirect_port"] = NSNull() }
                    // A URL-only edit must discover the destination's metadata,
                    // while deliberate overrides and explicit clears retain priority.
                    let committed = self.hubSaveAwaitingSettings
                    let initial = self.editingInitialHub
                    let initialURL = committed?.url ?? initial.map { normalizedHubURL($0.url) }
                    if committed != nil || (initialURL != nil && url != initialURL) {
                        if iss == (committed?.issuer ?? initial?.issuer ?? "").trimmingCharacters(in: .whitespacesAndNewlines) { action.removeValue(forKey:"issuer") }
                        if cid == (committed?.clientId ?? initial?.client_id ?? "").trimmingCharacters(in: .whitespacesAndNewlines) { action.removeValue(forKey:"client_id") }
                        if self.addHubRedirectPort.trimmingCharacters(in: .whitespacesAndNewlines) == (committed?.redirectPort ?? initial?.redirect_port.map(String.init) ?? "").trimmingCharacters(in: .whitespacesAndNewlines) { action.removeValue(forKey:"redirect_port") }
                    }
                }
                let pending = PendingHubSave(name:name, url:url, requestId:UUID().uuidString, generation:self.addHubDraftGeneration,
                    issuer:self.addHubIssuer, clientId:self.addHubClientId, redirectPort:self.addHubRedirectPort)
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
                HubConfigItem(name: h.name, url: "",
                    auth_mode: h.auth_mode, enabled: h.enabled != false, auto_listen: h.auto_listen, is_default: false, status: h.status, user: h.user, error: h.error, issuer: nil, client_id: nil, capabilities:h.capabilities, is_builtin:h.is_builtin, supports_workspace_scope:h.supports_workspace_scope, can_login:h.can_login)
            }
            var rowTop = 180 + hubOffset
            for hub in resolvedHubs {
                let local=hub.is_builtin == true
                let needsSignIn = hub.can_login == true
                let authTitle = ["none":"Trusted Network","token":"Shared Token","oidc":"Team Sign-In"][hub.auth_mode] ?? "Network"
                let endpoint = hub.is_builtin == true ? "On this Mac" : hub.url.replacingOccurrences(of:"^https?://",with:"",options:.regularExpression)
                let detail = hub.error.map(presentationError) ?? (hub.status == "connected" ? "" : (["auth_required":"Sign-in required","disconnected":"Offline","connecting":"Connecting…"][hub.status] ?? "Unavailable"))
                let subtitle=local ? "Built-in on this Mac · Signed in as @\(hub.user ?? "local")" : "\(endpoint) · \(authTitle)" + (hub.user.map { " · @\($0)" } ?? "") + (detail.isEmpty ? "" : " · \(detail)")
                let subtitleWidth:CGFloat = 438
                let subtitleHeight = max(14,ceil((subtitle as NSString).boundingRect(with:NSSize(width:subtitleWidth,height:10000),options:[.usesLineFragmentOrigin,.usesFontLeading],attributes:[.font:NSFont.systemFont(ofSize:10.5)]).height)+2)
                let rowHeight = max(56,subtitleHeight+42)
                rowTop -= rowHeight
                let row = roundedView(NSRect(x:20,y:rowTop,width:480,height:rowHeight),radius:9,color:.labelColor.withAppearanceAlpha(0.05),border:.labelColor.withAppearanceAlpha(0.10))
                rowTop -= 6
                row.identifier = NSUserInterfaceItemIdentifier("settings_hub:\(hub.name)")
                row.addSubview(roundedView(NSRect(x:CitoMetrics.cardInset,y:rowHeight-24,width:8,height:8),radius:4,color:statusColor(hub.enabled ? hub.status : "disconnected")))
                let nameLabel = textLabel(hub.name,NSRect(x:28,y:rowHeight-24,width:0,height:16),size:12.5,weight:.semibold)
                nameLabel.lineBreakMode = .byTruncatingMiddle; nameLabel.toolTip = hub.name
                row.addSubview(nameLabel)
                let subtitleLabel = textLabel(subtitle,NSRect(x:28,y:9,width:subtitleWidth,height:subtitleHeight),size:10.5,color:CitoPalette.secondaryText)
                subtitleLabel.maximumNumberOfLines=0; subtitleLabel.lineBreakMode = .byWordWrapping
                subtitleLabel.toolTip = (local ? "Built-in on this Mac" : presentationError(hub.url)) + (hub.error.map { "\n" + presentationError($0) } ?? "")
                row.addSubview(subtitleLabel)

                let edit=MenuControl("Edit",frame:NSRect(x:0,y:rowHeight-25,width:52,height:22),radius:6)
                edit.identifier=NSUserInterfaceItemIdentifier("edit_hub:\(hub.name)")
                edit.style = .neutral
                edit.pressed={ [weak self] in
                    guard let self=self else{return}
                    self.addHubName=hub.name; self.addHubUrl=hub.url; self.addHubAuth=hub.auth_mode; self.addHubAuthExplicit=true
                    self.addHubUser=hub.user ?? ""; self.addHubIssuer=hub.issuer ?? ""; self.addHubClientId=hub.client_id ?? ""; self.addHubRedirectPort=hub.redirect_port.map(String.init) ?? ""; self.addHubToken=""
                    self.addHubDraftGeneration += 1
                    self.pendingHubSave=nil; self.addHubError=nil; self.inviteParseRequest=""
                    self.editingExistingHub=true; self.editingOriginalHub=hub.name; self.editingInitialHub=hub; self.hubSaveAwaitingSettings=nil; self.addHubFormOpen=true
                    self.rebuildSettingsPanel(revealAddHub: true)
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

                if hub.is_builtin != true {
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
                let actions=signIn + [edit,toggleBtn] + remove
                var badges:[NSView]=[]
                if hub.is_default {
                    let badge=makePillBadge("Default",id:"default_hub:\(hub.name)",height:18,textColor:CitoPalette.infoText,fill:.labelColor.withAppearanceAlpha(0.06),paddingX:6,minWidth:0)
                    row.addSubview(badge); badges.append(badge)
                }
                let room=440-actions.filter{!$0.isHidden}.reduce(0){$0+$1.frame.width+6}-badges.reduce(0){$0+$1.frame.width+6}
                nameLabel.frame.size.width=max(0,min(room,ceil(measureTextWidth(hub.name,font:.systemFont(ofSize:12.5,weight:.semibold)))+4))
                layoutBar(in:row,insetLeft:26,insetRight:CitoMetrics.cardInset,y:rowHeight-31,height:22,leading:[nameLabel]+badges,trailing:actions)
                hubsView.addSubview(row)
            }
        }
        mountSettingsCards(hubsView, in: root)

    }

    func openSettingsPanel() {
        settingsVisible = true
        ensureSettingsPanel()
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
        settingsBannerError = nil; hostHubPortError = nil
        uninstallConfirmOpen = false; uninstallPurgeData = false
        settingsDirty = true
        settingsPanel?.orderOut(nil)
        if !headless && !settingsVisible && !directoryVisible { NSApp.setActivationPolicy(.accessory) }
        if emitAction { emit(["op": "close_settings"]) }
    }

    var hubNamePlaceholder: String {
        addHubUrl.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "e.g. studio or work" : "Default: " + proposedHubName("", url:addHubUrl, existing:settings?.hubs ?? [], creating:!editingExistingHub, original:editingOriginalHub)
    }
}
