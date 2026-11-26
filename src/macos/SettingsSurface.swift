import AppKit

private final class SettingsCheckbox: NSButton {
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { false }
    override var canBecomeKeyView: Bool { isEnabled && acceptsFirstResponder && !isHiddenOrHasHiddenAncestor }
}

private final class SettingsScrollView: NSScrollView {
    override func tile() {
        super.tile()
        // Resize the existing document, including while its field editor is mounted.
        if let document = documentView, document.frame.width != contentSize.width {
            document.setFrameSize(NSSize(width: contentSize.width, height: document.frame.height))
        }
    }
}

private final class SettingsHubForm: SurfaceView {
    var equalColumns: (left: [NSView], right: [NSView])? { didSet { layoutEqualColumns() } }

    override func resizeSubviews(withOldSize oldSize: NSSize) {
        super.resizeSubviews(withOldSize: oldSize)
        if bounds.width != oldSize.width { layoutEqualColumns() }
    }

    private func layoutEqualColumns() {
        guard let (left, right) = equalColumns else { return }
        let available = max(0, bounds.width - 28 - 8)
        let leftWidth = floor(available / 2)
        for view in left { view.frame.size.width = leftWidth }
        for view in right {
            view.frame.origin.x = 14 + leftWidth + 8
            view.frame.size.width = available - leftWidth
        }
    }
}

// Reserve wrapping height and intrinsic-width siblings at the narrowest
// viewport this fixed-size panel can have under either scroller style.
private func settingsScrollerAllowance(_ scroll: NSScrollView) -> CGFloat {
    let legacy = NSScroller.scrollerWidth(for: scroll.verticalScroller?.controlSize ?? .regular, scrollerStyle: .legacy)
    return max(0, scroll.contentSize.width - (scroll.bounds.width - legacy))
}

// The mounted document owns its editor routing until AppKit releases it.
final class ChannelsSettingsView: NSView, NSTextFieldDelegate {
    let hub: String
    let channels: [ChannelSetupItem]
    private weak var companion: Companion?

    init(hub: String, channels: [ChannelSetupItem], companion: Companion) {
        self.hub = hub
        self.channels = channels
        self.companion = companion
        super.init(frame: NSRect(x: 0, y: 44, width: 520, height: 368))
    }

    required init?(coder: NSCoder) { nil }

    func channelField(_ id: String) -> (ChannelSetupItem, ChannelFieldItem)? {
        for channel in channels {
            if let field = channel.fields.first(where: { id == "channel_field:\(channel.id):\($0.key)" }) {
                return (channel, field)
            }
        }
        return nil
    }

    func controlTextDidChange(_ notification: Notification) {
        guard let selfUI = companion,
              let field = notification.object as? NSTextField, let id = field.identifier?.rawValue,
              let (channel, schema) = channelField(id) else { return }
        let key = "\(hub)|\(channel.id)"
        selfUI.channelUiStates[key, default: ChannelUiState()].drafts[schema.key] = field.stringValue
        selfUI.channelUiStates[key]?.savedUntil.removeValue(forKey: schema.scope)
        let gateway = schema.scope == "hub"
        if let save: MenuControl = selfUI.findView(in: self, id: (gateway ? "save_hub_service:" : "save_channel:") + channel.id) {
            if selfUI.channelSavePending(gateway ? "save_hub_service" : "channel_save", hub: hub, channel: channel.id, scope: schema.scope) {
                selfUI.pendingLabel(save, true, "Saving…")
            } else {
                let title = gateway ? "Save Gateway Token" : "Save"
                save.clearFeedback(title: title, accessibilityLabel: title)
                save.isEnabled = true
                save.toolTip = title
            }
            save.layoutContents()
        }
        if let test: MenuControl = selfUI.findView(in: self, id: "test_channel:\(channel.id)") {
            let visibleFields = channel.fields.filter { $0.hidden != true && $0.scope != "hub" && selfUI.settingsFieldControls["channel_field:\(channel.id):\($0.key)"] != nil }
            test.isEnabled = !selfUI.isPending("channel_test", hub: hub, target: channel.id) && channel.canSendTest(drafts: selfUI.channelUiStates[key]?.drafts ?? [:], visibleFields: visibleFields)
        }
    }

    func controlTextDidEndEditing(_ notification: Notification) {
        companion?.controlTextDidEndEditing(notification)
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        companion?.control(control, textView: textView, doCommandBy: selector) ?? false
    }
}

extension Companion {
    func pendingLabel(_ control: MenuControl, _ pending: Bool, _ title: String) {
        guard pending else { return }
        let pairedWidth = MenuControl.toggleWidth(control.identifier?.rawValue ?? "", title: control.label.stringValue,
            font: control.label.font ?? .systemFont(ofSize: 11, weight: .medium), paddingX: control.paddingX)
        control.clearFeedback(title: title, accessibilityLabel: title)
        control.isEnabled = false; control.setLeadingSymbol(nil)
        control.toolTip = title
        let right = control.frame.maxX
        control.pendingWidth = max(pairedWidth, control.frame.width, MenuControl.intrinsicWidth(title))
        control.frame.size.width = control.pendingWidth
        control.frame.origin.x = right - control.pendingWidth
    }
    func channelSavePending(_ op: String, hub: String, channel: String, scope: String) -> Bool {
        pendingControls.values.contains { pending in
            guard pending.op == op && pending.hub == hub && pending.target == channel,
                  let sent = pending.channelSave, sent.scope == scope else { return false }
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
        return resolvedDefaultHub(requireEnabled: true) ?? inventory.first ?? "local"
    }
    var mountedChannelsSettings: ChannelsSettingsView? {
        settingsPanel?.contentView?.subviews.lazy.compactMap { ($0 as? NSScrollView)?.documentView as? ChannelsSettingsView }.first
    }
    func clearChannelDrafts(_ channel: ChannelSetupItem, scope: String, hub: String) {
        let key = "\(hub)|\(channel.id)"
        for field in channel.fields where field.scope == scope {
            channelUiStates[key, default: ChannelUiState()].drafts.removeValue(forKey: field.key)
        }
    }
    func finishHubSave(_ pending: PendingHubSave, savedName: String?) {
        hubForm.pendingSave = nil
        if hubForm.generation == pending.generation {
            hubForm = HubFormState()
        } else {
            var profile = pending.profile
            if let savedName { profile.name = savedName }
            let name = hubNameProposal(hubForm.name, url: hubForm.url, existing: settings?.hubs ?? [])
            let automaticDraft = hubForm.name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            let sameURL = normalizedHubURL(hubForm.url) == profile.url
            let identityKnown = savedName != nil || pending.nameExplicit || hubForm.isEditing
            let sameAutomaticHub = automaticDraft && !pending.nameExplicit && sameURL
            if identityKnown && (hubForm.isEditing || sameAutomaticHub || (name == profile.name && (!automaticDraft || sameURL))) {
                if sameAutomaticHub {
                    // The daemon has consumed the blank-name request; retain its chosen identity.
                    hubForm.name = profile.name
                    settingsFieldControls["settings_hub_name"]?.stringValue = profile.name
                }
                hubForm.mode = .edit(profile)
            } else if savedName == nil {
                // The daemon can still allocate an automatic name against its full inventory.
                hubForm.mode = .addWithIncompleteInventory
            }
        }
    }

    @discardableResult
    func acceptSettings(_ value: SettingsPayload, hub: String? = nil, channelSave: PendingControl? = nil, savedRequest: String? = nil, sourceOp: String? = nil, confirmHubSave: Bool = false, savedHub: String? = nil, truncated: Bool = false) -> Bool {
        let previousUi = channelUiStates
        let target = settingsHub(value, hub: hub)
        for old in settings?.hubs ?? [] where !old.sharesChannelAuthority(with: value.hubs.first { $0.name == old.name }) {
            // A reused Hub name cannot carry local edits or in-flight receipts across accounts.
            if settingsActiveTab == "channels", mountedChannelsSettings?.hub == old.name { settingsPanel?.makeFirstResponder(nil) }
            channelUiStates = channelUiStates.filter { !$0.key.hasPrefix(old.name + "|") }
            channelSettingsByHub.removeValue(forKey: old.name)
            channelInventoryErrorsByHub.removeValue(forKey: old.name)
            pendingControls = pendingControls.filter { $0.value.hub != old.name || !Self.channelControlOps.contains($0.value.op) }
        }
        settings = value
        if !selectedChannelHub.isEmpty && !value.hubs.contains(where: { $0.name == selectedChannelHub && $0.enabled }) {
            selectedChannelHub = value.hubs.first(where: { $0.enabled })?.name ?? ""
            if settingsVisible && !selectedChannelHub.isEmpty { emit(["op": "open_settings", "hub": selectedChannelHub], userInitiated:false) }
        }
        if hostPort.pending && savedRequest == hostPort.requestId {
            hostPort.pending = false
            let draftPort = hostPort.draft.flatMap { Int($0.trimmingCharacters(in:.whitespacesAndNewlines)) }
            let saved = value.host_hub.port == hostPort.submittedPort && value.host_hub.error == nil &&
                (hostPort.draft == nil || draftPort == hostPort.submittedPort)
            hostPort.savedUntil = saved ? Date().addingTimeInterval(1.2) : nil
            hostPort.error = nil
            if saved { hostPort.draft = nil; rebuildFeedbackAfter(1.2) }
        }
        if confirmHubSave, let pending = hubForm.pendingSave, savedRequest == pending.requestId {
            let profile = pending.profile
            let candidates = value.hubs.filter { normalizedHubURL($0.url) == profile.url }
            let saved: HubConfigItem?
            if let savedHub { saved = candidates.first { $0.name == savedHub } }
            else if pending.nameExplicit || hubForm.isEditing { saved = candidates.first { $0.name == profile.name } }
            else { saved = candidates.count == 1 && candidates[0].is_builtin != true ? candidates[0] : nil }
            if let error = saved?.error { hubForm.failSave(error) }
            else {
                hubForm.error = nil
                finishHubSave(pending, savedName: savedHub ?? saved?.name)
            }
        }
        var reconciledHub = false
        if hubForm.pendingSave == nil, let initial = hubForm.initialHub,
           let saved = value.hubs.first(where: { $0.name == initial.name && normalizedHubURL($0.url) == initial.url })?.formProfile, saved != initial {
            // A passive canonical refresh also completes a payload-free receipt.
            // Compare each field with the submitted draft to preserve newer edits.
            if hubForm.issuer == initial.issuer, initial.issuer != saved.issuer {
                hubForm.issuer = saved.issuer
                settingsFieldControls["settings_hub_issuer"]?.stringValue = hubForm.issuer
            }
            if hubForm.clientId == initial.clientId, initial.clientId != saved.clientId {
                hubForm.clientId = saved.clientId
                settingsFieldControls["settings_hub_client_id"]?.stringValue = hubForm.clientId
            }
            if hubForm.redirectPort == initial.redirectPort, initial.redirectPort != saved.redirectPort {
                hubForm.redirectPort = saved.redirectPort
                settingsFieldControls["settings_hub_redirect_port"]?.stringValue = hubForm.redirectPort
            }
            hubForm.mode = .edit(saved)
            reconciledHub = true
        }
        let previousChannels = channelSettingsByHub[target]
        channelInventoryErrorsByHub[target] = value.channel_inventory_error
        guard !(truncated && previousChannels != nil), let channels = value.channels else { return reconciledHub }
        channelSettingsByHub[target] = channels
        for channel in channels {
            let key=target + "|" + channel.id
            var ui = channelUiStates[key] ?? ChannelUiState()
            let previous = previousChannels?.first { $0.id == channel.id }
            let preservePairing = sourceOp == "channel_pair" || value.channel_inventory_error != nil
            if sourceOp != "channel_test" && (previous?.status != channel.status || previous?.detail != channel.detail || previous?.warning != channel.warning) {
                ui.testResultIsLatest = false
            }
            // An older acknowledgement cannot clear a newer draft or a failed
            // save, and test/pair result refreshes are not save acknowledgements.
            var acknowledged: (scope: String, values: [String: String], unchanged: Bool)?
            if previousChannels != nil, let pending = channelSave, pending.target == channel.id, pending.hub == target,
               let entry = pending.channelSave {
                let unchanged = channel.fields.filter { $0.scope == entry.scope }.allSatisfy { field in
                    let draft = ui.drafts[field.key]
                    if let sent = entry.values[field.key] { return (draft ?? field.value ?? "") == sent }
                    return draft == nil || draft == (field.value ?? "")
                }
                acknowledged = (scope: entry.scope, values: entry.values, unchanged: unchanged)
            }
            for field in channel.fields {
                if let sent = acknowledged?.values[field.key], ui.drafts[field.key] == sent {
                    ui.drafts.removeValue(forKey: field.key)
                } else if acknowledged?.values[field.key] == nil && field.type != "secret" && ui.drafts[field.key] == (field.value ?? "") {
                    ui.drafts.removeValue(forKey: field.key)
                }
            }
            let personalSave = acknowledged?.scope == "user"
            if channel.configured && channel.status != "pairing_pending" && (personalSave || (!preservePairing &&
                (previous?.configured == false || (!ui.pairingInstruction.isEmpty && (channel.pairing_instruction ?? "").isEmpty)))) {
                if channel.fields.contains(where: { $0.scope == "user" && ui.drafts[$0.key] != nil }) || (personalSave && acknowledged?.unchanged == false) {
                    ui.editingPersonal = true
                } else {
                    ui.resetPersonal(preservePendingPair: ui.waitingForPairCode)
                }
            }
            if let acknowledged {
                if acknowledged.unchanged {
                    ui.savedUntil[acknowledged.scope] = Date().addingTimeInterval(1.5)
                    rebuildFeedbackAfter(1.5)
                } else { ui.savedUntil.removeValue(forKey: acknowledged.scope) }
            }
            if acknowledged?.scope == "hub" && acknowledged?.unchanged == true && (channel.hub_provided || channel.status != "not_configured" || (channel.fields.contains { $0.scope == "hub" } && channel.fields.filter { $0.scope == "hub" }.allSatisfy { $0.configured })) {
                ui.editingGateway = false
            }
            if !preservePairing, let instruction = channel.pairing_instruction, !instruction.isEmpty {
                ui.pairingInstruction = instruction
                ui.pairingCommand = channel.pairing_command ?? channel.pairing_code
                ui.pairingURL = channel.pairing_url
            } else if !preservePairing && channel.status != "pairing_pending" && !ui.waitingForPairCode {
                ui.pairing = nil
                ui.clearPairingInstructions()
            }
            channelUiStates[key] = ui
        }
        return reconciledHub || channelUiStates != previousUi
    }

    private func updateInbox(paused: Bool? = nil, scope: String? = nil, hub: String, revision: Int) {
        guard !isPending("human_inbox_update",hub:hub,target:"paused"),
              !isPending("human_inbox_update",hub:hub,target:"scope") else { return }
        var action: [String:Any]=["op":"human_inbox_update","hub":hub,"expected_control_revision":revision]
        if let paused=paused { action["paused"]=paused }
        if let scope=scope { action["scope"]=scope }
        emit(action)
    }

    // MARK: - Surface 3: Native AppKit Settings Window (520 × 460 pt)

    func ensureSettingsPanel() {
        guard settingsPanel == nil else { return }
        ensureEditMainMenu()
        let window = makeKeyWindow(title: "Settings", width: 520, height: 460, headerHeight: 48, appearance: testAppearance)
        window.delegate = self
        window.onKey = { [weak self] key in self?.handleSettingsKey(key) }
        window.handlesNativeShortcuts = headless
        let root = NSView(frame:NSRect(x:0,y:0,width:520,height:460))
        root.identifier = NSUserInterfaceItemIdentifier("settings_panel")
        let effect = NSVisualEffectView(frame:root.bounds)
        effect.material = .windowBackground; effect.blendingMode = .behindWindow; effect.state = .followsWindowActiveState
        root.addSubview(effect); window.contentView = root
        if !headless { window.restorePlacement(named: "CitoSettingsWindow") }
        window.layoutWindowButtons()
        settingsPanel = window
    }

    func handleSettingsKey(_ key: String) {
        let key = key.lowercased()
        guard allowPanelKey(key, in: settingsPanel), !handleTextEditShortcut(key, in: settingsPanel) else { return }
        switch key {
        case "command-comma": openSettingsPanel()
        case "command-w": if headless && settingsVisible { settingsPanel?.performClose(nil) }
        case "command-k": openDirectoryPanel(mode: "directory")
        case "command-1", "command-2", "command-3":
            guard let tab = ["command-1": "apps", "command-2": "channels", "command-3": "hubs"][key] else { return }
            settingsActiveTab = tab
            if tab == "channels" { refreshChannelSettingsIfNeeded() }
            rebuildSettingsPanel()
        case "escape":
            if settingsActiveTab == "hubs" && hubForm.isOpen { hubForm = HubFormState(); rebuildSettingsPanel() }
            else if settingsActiveTab == "channels", let mounted = mountedChannelsSettings {
                let hub = mounted.hub
                let channels = mounted.channels
                let editingGateway = channels.filter { channelUiStates["\(hub)|\($0.id)"]?.editingGateway == true }
                let editing = editingGateway.isEmpty ? channels.filter { channelUiStates["\(hub)|\($0.id)"]?.editingPersonal == true } : editingGateway
                guard !editing.isEmpty else { closeSettingsPanel(); return }
                for channel in editing {
                    let key = "\(hub)|\(channel.id)"
                    clearChannelDrafts(channel, scope: editingGateway.isEmpty ? "user" : "hub", hub: hub)
                    if editingGateway.isEmpty { channelUiStates[key]?.resetPersonal() }
                    else { channelUiStates[key]?.resetGateway() }
                }
                rebuildSettingsPanel()
            }
            else { closeSettingsPanel() }
        case "return":
            let id = (settingsPanel?.firstResponder as? NSTextView)?.delegate as? NSTextField
            let field=id?.identifier?.rawValue ?? ""
            let actionId: String
            if settingsActiveTab == "hubs" && field == "host_hub_port" { actionId="apply_host_hub_port" }
            else if settingsActiveTab == "channels", let (channel, schema) = mountedChannelsSettings?.channelField(field) {
                actionId=(schema.scope == "hub" ? "save_hub_service:" : "save_channel:")+channel.id
            } else if settingsActiveTab == "hubs" && hubForm.isOpen { actionId="submit_add_hub" }
            else { return }
            if let root=settingsPanel?.contentView, let control: MenuControl = findView(in:root,id:actionId), control.isEnabled { control.pressed?() }
        default: break
        }
    }
    var editingBuiltinHub: Bool {
        hubForm.isEditing && (settings?.hubs.first { $0.name == hubForm.originalName }?.is_builtin ?? hubs.first { $0.name == hubForm.originalName }?.is_builtin) == true
    }
    func syncAddHubFieldsFromControls() {
        guard hubForm.isOpen else { return }
        if editingBuiltinHub, let hub = settings?.hubs.first(where: { $0.name == hubForm.originalName }) { hubForm.name = hub.name; hubForm.url = hub.url }
        if let nameField = settingsFieldControls["settings_hub_name"] { hubForm.name = nameField.stringValue }
        if let urlField = settingsFieldControls["settings_hub_url"] { hubForm.url = urlField.stringValue }
        if let tokenField = settingsFieldControls["settings_hub_token"] { hubForm.token = tokenField.stringValue }
        if let userField = settingsFieldControls["settings_hub_user"] { hubForm.user = userField.stringValue }
        if let issuerField = settingsFieldControls["settings_hub_issuer"] { hubForm.issuer = issuerField.stringValue }
        if let clientField = settingsFieldControls["settings_hub_client_id"] { hubForm.clientId = clientField.stringValue }
        if let portField = settingsFieldControls["settings_hub_redirect_port"] { hubForm.redirectPort = portField.stringValue }
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
        let raw = settingsFieldControls["host_hub_port"]?.stringValue ?? hostPort.draft ?? "\(fallback)"
        guard let port = Int(raw.trimmingCharacters(in:.whitespacesAndNewlines)), (1...65535).contains(port) else {
            if fallbackOnInvalid { return fallback }
            hostPort.error = "Port must be an integer from 1 to 65535."
            hostPort.savedUntil = nil; rebuildSettingsPanel(); return nil
        }
        return port
    }

    func handlePasteHubInvite() {
        guard !hubInvitePending else { return }
        let raw = readPasteboardString().trimmingCharacters(in:.whitespacesAndNewlines)
        let isURL = (raw.hasPrefix("http://") || raw.hasPrefix("https://")) && URL(string:raw)?.host != nil
        let isCLI = raw.hasPrefix("cito setup ") && raw.contains("--url")
        let invite = raw.data(using:.utf8).flatMap { try? JSONSerialization.jsonObject(with:$0) } as? [String:Any]
        let inviteURL = (invite?["url"] as? String).flatMap { URL(string:$0) }
        let isJSON = inviteURL?.host != nil && ["http", "https"].contains(inviteURL?.scheme ?? "")
        guard isURL || isCLI || isJSON else {
            settingsBannerError = "Clipboard does not contain a valid Hub invite or http(s):// URL."
            rebuildSettingsPanel(); return
        }
        // One authoritative parser in the daemon handles every invite format.
        // Clipboard text is data, never a shell command to execute.
        settingsBannerError=nil; hubInvitePastedUntil = .distantPast
        hubInviteRequest = UUID().uuidString
        emit(["op":"paste_hub_invite","invite":raw,"request_id":hubInviteRequest])
        rebuildSettingsPanel()
    }

    private func makeSettingsScroll(in root: NSView) -> NSScrollView {
        let scroll = SettingsScrollView(frame: NSRect(x: 0, y: 44, width: root.bounds.width, height: 368 - (settingsBannerError == nil ? 0 : 48)))
        scroll.autoresizingMask = [.width]
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        root.addSubview(scroll)
        return scroll
    }

    func mountSettingsCards(_ content: NSView, on scroll: NSScrollView) {
        let id = content.identifier?.rawValue ?? ""
        let minY = content.subviews.map { $0.frame.minY }.min() ?? 8
        let extra = max(0, 8 - minY)
        for child in content.subviews { child.frame.origin.y += extra }
        content.frame = NSRect(x: 0, y: 0, width: scroll.contentSize.width, height: content.frame.height + extra)
        scroll.identifier = NSUserInterfaceItemIdentifier(id)
        scroll.isHidden = content.isHidden
        content.isHidden = false
        scroll.documentView = content
        let top = max(0, content.frame.height - scroll.contentSize.height)
        scroll.contentView.scroll(to: NSPoint(x: 0, y: top - min(top, settingsScrollOffsets[id] ?? 0)))
        scroll.reflectScrolledClipView(scroll.contentView)
    }

    func rebuildSettingsPanel(preservingEditor: Bool = false, revealAddHub: Bool = false, revealChannel: String? = nil, revealPairing: Bool = false, revealEditorScope: String = "hub", syncFields: Bool = true, scrollerStyle: NSScroller.Style? = nil) {
        if let scrollerStyle {
            // A style notification changes geometry, not the mounted Settings state.
            for case let scroll as SettingsScrollView in settingsPanel?.contentView?.subviews ?? [] {
                scroll.scrollerStyle = scrollerStyle
                scroll.tile()
            }
            return
        }
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
        let previousChannelHub = mountedChannelsSettings?.hub
        let selectedRange = (focused?.value.currentEditor() as? NSTextView)?.selectedRange()
        if syncFields { syncAddHubFieldsFromControls() }
        MenuControl.clearHoverWhere { $0.window === window }
        for case let scroll as NSScrollView in window.contentView?.subviews ?? [] {
            if let id = scroll.identifier?.rawValue {
                settingsScrollOffsets[id] = max(0,(scroll.documentView?.frame.height ?? 0)-scroll.contentSize.height-scroll.contentView.bounds.origin.y)
            }
        }
        settingsFieldControls.removeAll()

        guard let root = window.contentView else { return }
        root.subviews.filter { !($0 is NSVisualEffectView) }.forEach { $0.removeFromSuperview() }
        window.invalidateShadow()

        // Header & Segmented Tab Bar (520 × 48 pt, y: 412)
        let header = NSView(frame: NSRect(x: 0, y: 412, width: 520, height: 48))
        header.identifier = NSUserInterfaceItemIdentifier("settings_header")

        let tabPadding: CGFloat = 8
        let tabWidths = [("Agent Apps","square.grid.2x2"),("Human Channels","bubble.left.and.bubble.right"),("Hubs & Network","network")].map {
            MenuControl.intrinsicWidth($0.0, leadingSymbol:$0.1, paddingX:tabPadding)
        }
        let tabsWidth = tabWidths.reduce(0,+) + 12
        let tabsTrack = makePillTrack(NSRect(x:max(78,floor((520-tabsWidth)/2)),y:10,width:tabsWidth,height:28),radius:9)
        tabsTrack.identifier = NSUserInterfaceItemIdentifier("settings_tab_track")
        header.addSubview(tabsTrack)
        let appsTab = MenuControl("Agent Apps", frame: NSRect(x: 2, y: 2, width: tabWidths[0], height: 24), leadingSymbol: "square.grid.2x2", radius: 7, paddingX: tabPadding)
        appsTab.identifier = NSUserInterfaceItemIdentifier("settings_tab:apps")
        appsTab.style = settingsActiveTab == "apps" ? .prominent : .subtleHeader
        appsTab.pressed = { [weak self] in
            guard let self = self else { return }
            self.settingsActiveTab = "apps"
            self.rebuildSettingsPanel()
        }
        tabsTrack.addSubview(appsTab)

        let channelsTab = MenuControl("Human Channels", frame: NSRect(x: tabWidths[0]+6, y: 2, width: tabWidths[1], height: 24), leadingSymbol: "bubble.left.and.bubble.right", radius: 7, paddingX: tabPadding)
        channelsTab.identifier = NSUserInterfaceItemIdentifier("settings_tab:channels")
        channelsTab.style = settingsActiveTab == "channels" ? .prominent : .subtleHeader
        channelsTab.pressed = { [weak self] in
            guard let self = self else { return }
            self.settingsActiveTab = "channels"
            self.refreshChannelSettingsIfNeeded()
            self.rebuildSettingsPanel()
        }
        tabsTrack.addSubview(channelsTab)

        let hubsTab = MenuControl("Hubs & Network", frame: NSRect(x: tabWidths[0]+tabWidths[1]+10, y: 2, width: tabWidths[2], height: 24), leadingSymbol: "network", radius: 7, paddingX: tabPadding)
        hubsTab.identifier = NSUserInterfaceItemIdentifier("settings_tab:hubs")
        hubsTab.style = settingsActiveTab == "hubs" ? .prominent : .subtleHeader
        hubsTab.pressed = { [weak self] in
            guard let self = self else { return }
            self.settingsActiveTab = "hubs"
            self.rebuildSettingsPanel()
        }
        tabsTrack.addSubview(hubsTab)

        header.addSubview(roundedView(NSRect(x: 0, y: 0, width: 520, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
        root.addSubview(header)
        if let error = settingsBannerError {
            let banner=roundedView(NSRect(x:CitoMetrics.settingsInset,y:368,width:480,height:40),radius:6,color:CitoPalette.dangerText.withAppearanceAlpha(0.10))
            banner.identifier=NSUserInterfaceItemIdentifier("settings_error_banner")
            let label=textLabel(presentationError(error),NSRect(x:10,y:7,width:432,height:26),size:10,color:CitoPalette.dangerText)
            label.maximumNumberOfLines = 2; label.lineBreakMode = .byWordWrapping; label.cell?.wraps = true; label.toolTip = label.stringValue
            label.identifier=NSUserInterfaceItemIdentifier("settings_error_label"); banner.addSubview(label)
            let dismiss=MenuControl("Dismiss",frame:NSRect(x:450,y:10,width:22,height:20),symbol:"xmark")
            dismiss.identifier=NSUserInterfaceItemIdentifier("dismiss_settings_error")
            dismiss.style = .subtleHeader
            dismiss.pressed={ [weak self] in self?.settingsBannerError=nil; self?.rebuildSettingsPanel() }
            banner.addSubview(dismiss); root.addSubview(banner)
        }

        buildAppsTab(in: makeSettingsScroll(in: root))
        let renderedHub = effectiveChannelHub
        buildChannelsTab(in: makeSettingsScroll(in: root), renderedHub: renderedHub)
        buildHubsTab(in: makeSettingsScroll(in: root))

        // Persistent Footer (520 × 44 pt, y: 0)
        let footerBar = NSView(frame: NSRect(x: 0, y: 0, width: 520, height: 44))
        footerBar.identifier = NSUserInterfaceItemIdentifier("settings_footer")
        footerBar.addSubview(roundedView(NSRect(x: 0, y: 43, width: 520, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
        let loginBtn = SettingsCheckbox(checkboxWithTitle: "Start Cito at Login", target: self, action: #selector(toggleLaunchAtLogin(_:)))
        loginBtn.identifier = NSUserInterfaceItemIdentifier("settings_launch_at_login")
        loginBtn.focusRingType = .default
        loginBtn.state = settings?.launch_at_login == true ? .on : .off
        footerBar.addSubview(loginBtn)

        let uninstallBtn = MenuControl("Disconnect Agent Apps…", frame: NSRect(x: 316, y: 10, width: MenuControl.intrinsicWidth("Disconnect Agent Apps…", paddingX: 12), height: 24), radius: 6, paddingX: 12)
        uninstallBtn.identifier = NSUserInterfaceItemIdentifier("open_uninstall_confirm")
        uninstallBtn.style = .destructiveHover
        uninstallBtn.pressed = { [weak self] in self?.presentUninstallAlert() }
        footerBar.addSubview(uninstallBtn)
        layoutBar(in: footerBar, insetLeft: 20, insetRight: 20, y: 10, height: 24, leading: [loginBtn], trailing: [uninstallBtn])
        root.addSubview(footerBar)

        window.invalidateShadow()
        if let id = focusedId, let field = settingsFieldControls[id],
           !(id.hasPrefix("channel_field:") || id.hasPrefix("pairing_instruction:")) || previousChannelHub == renderedHub {
            window.makeFirstResponder(field)
            if let range = selectedRange, let editor = field.currentEditor() as? NSTextView { editor.setSelectedRange(range) }
        }
        if revealAddHub && hubForm.isOpen,
           let scroll: NSScrollView = findView(in: root, id: "settings_hubs_tab"),
           let form: NSView = findView(in: scroll, id: "add_hub_form") {
            form.scrollToVisible(NSRect(x: 0, y: 0, width: form.bounds.width, height: 44))
            scroll.reflectScrolledClipView(scroll.contentView)
        }
        if let channel = revealChannel,
           let scroll: NSScrollView = findView(in: root, id: "settings_channels_tab"),
           let card: NSView = findView(in: scroll, id: "settings_channel:\(channel)") {
            root.layoutSubtreeIfNeeded()
            let viewportHeight = scroll.contentView.bounds.height
            var target = card.bounds
            if target.height > viewportHeight {
                target.origin.y = max(0, card.bounds.height - viewportHeight)
                target.size.height = viewportHeight
                if revealPairing {
                    if let instruction: NSTextField = findView(in: card, id: "pairing_instruction:\(channel)"),
                       let copy: MenuControl = findView(in: card, id: "copy_pair_command:\(channel)") {
                        target = card.convert(instruction.bounds, from: instruction)
                            .union(card.convert(copy.bounds, from: copy))
                    }
                } else {
                    let saveId = (revealEditorScope == "hub" ? "save_hub_service:" : "save_channel:") + channel
                    if let save: MenuControl = findView(in: card, id: saveId), !save.isHidden,
                       let editor = save.superview {
                        let fields = editor.subviews.compactMap { $0 as? NSTextField }
                            .filter { $0.isEditable && !$0.isHidden }
                        if !fields.isEmpty {
                            target = fields.reduce(card.convert(save.bounds, from: save)) { rect, field in
                                rect.union(card.convert(field.bounds, from: field))
                            }
                        }
                    } else if revealEditorScope == "user",
                              let manual: MenuControl = findView(in: card, id: "manual_channel:\(channel)"),
                              let copy: MenuControl = findView(in: card, id: "copy_pair_command:\(channel)") {
                        // Collapsing manual entry removes Save, not the pairing actions.
                        target = card.convert(manual.bounds, from: manual)
                            .union(card.convert(copy.bounds, from: copy))
                    }
                }
                target = target.intersection(card.bounds)
                // The card is non-flipped: keep the action row if the region exceeds the viewport.
                target.size.height = min(target.height, viewportHeight)
            }
            card.scrollToVisible(target)
            scroll.reflectScrolledClipView(scroll.contentView)
        }
    }

    func buildAppsTab(in scroll: NSScrollView) {
        let cardWidth = scroll.contentSize.width - 32
        let allowance = settingsScrollerAllowance(scroll)
        let appsView = NSView(frame: NSRect(x: 0, y: 0, width: scroll.contentSize.width, height: 368))
        appsView.identifier = NSUserInterfaceItemIdentifier("settings_apps_tab")
        appsView.isHidden = settingsActiveTab != "apps"

        let resolvedApps: [AppSetupItem] = settings?.apps ?? harnesses.map { h in
            AppSetupItem(id: h.id, display_name: h.display_name, tier: h.tier,
                configured: false, status: "not_configured", detail: "Click Set Up to connect", paths: [])
        }
        appsView.addSubview(textLabel("AGENT APPS", NSRect(x:16,y:338,width:240,height:14),size:10,weight:.semibold,color:CitoPalette.secondaryText))
        let map = harnessMap
        let intro = textLabel("Connect your coding apps, then choose sessions to listen.",NSRect(x:16,y:316,width:cardWidth,height:16),size:11,color:CitoPalette.secondaryText)
        intro.autoresizingMask = [.width]
        appsView.addSubview(intro)
        var appTop: CGFloat = 304
        for app in resolvedApps {
            let cardHeight: CGFloat = 70
            appTop -= cardHeight
            let card = roundedView(NSRect(x:16,y:appTop,width:cardWidth,height:cardHeight),radius:10,
                color:.labelColor.withAppearanceAlpha(0.045),border:.labelColor.withAppearanceAlpha(0.08))
            card.autoresizingMask = [.width]
            card.identifier = NSUserInterfaceItemIdentifier("settings_app:\(app.id)")
            let barY: CGFloat = 36
            card.addSubview(makeHarnessBadge(NSRect(x:12,y:barY-2,width:28,height:28),radius:7,iconInset:6,harness:app.id,harnesses:map))
            let titleW = min(app.isConfigured ? 184 : 180, measureTextWidth(app.display_name,font:.systemFont(ofSize:13,weight:.semibold)) + 2)
            let title = textLabel(app.display_name,NSRect(x:50,y:barY+3.5,width:titleW,height:17),size:13,weight:.semibold)
            title.toolTip = app.display_name; title.lineBreakMode = .byTruncatingMiddle
            card.addSubview(title)
            let tier: NSView?
            if app.tier == "experimental" {
                let badge = makePillBadge("Experimental", id: "settings_app_tier:\(app.id)", textColor: CitoPalette.warningText, fill: CitoPalette.warningSurface, border: CitoPalette.warningBorder, minWidth:0)
                card.addSubview(badge); tier = badge
            } else { tier = nil }
            let statusText = app.isConfigured ? "Connected" : (app.status == "error" ? "Inspection failed" : (app.status == "repair_needed" ? "Repair Needed" : "Not Set Up"))
            let statusColor = app.status == "error" ? CitoPalette.dangerText : app.isConfigured ? CitoPalette.successText : CitoPalette.warningText
            let dot=roundedView(NSRect(x:50,y:16,width:6,height:6),radius:3,color:statusColor)
            dot.identifier=NSUserInterfaceItemIdentifier("settings_app_status_dot:\(app.id)"); card.addSubview(dot)
            let detailText = presentationError(app.detail).replacingOccurrences(of:"^(Connected|Disconnected) · ",with:"",options:.regularExpression)
            let status=textLabel(statusText + (detailText.isEmpty || detailText == statusText ? "" : " · " + detailText),NSRect(x:62,y:10,width:cardWidth-74,height:18),size:11,color:statusColor)
            status.autoresizingMask = [.width]
            status.identifier=NSUserInterfaceItemIdentifier("settings_app_status:\(app.id)"); status.lineBreakMode = .byTruncatingMiddle
            status.toolTip=([detailText]+app.paths).filter { !$0.isEmpty }.joined(separator:"\n"); card.addSubview(status)
            let appActionWidth = max(76, MenuControl.intrinsicWidth("Disconnect"))
            let actionTitle=app.isConfigured || app.status == "error" ? "Check & Repair" : (app.status == "repair_needed" ? "Repair" : "Set Up")
            let btn = MenuControl(actionTitle,frame:NSRect(x:0,y:0,width:MenuControl.intrinsicWidth(actionTitle),height:24))
            btn.identifier = NSUserInterfaceItemIdentifier("setup_app:\(app.id)")
            btn.style = app.isConfigured ? .neutral : .prominent
            if let until = appCheckedUntil[app.id], Date() < until { btn.copied("✓ Checked", for: max(0.01, until.timeIntervalSinceNow)) }
            let appPending = isPending("setup_apply",target:app.id) || isPending("setup_remove_app",target:app.id)
            btn.isEnabled = !appPending
            pendingLabel(btn,isPending("setup_apply",target:app.id),actionTitle == "Check & Repair" ? "Checking…" : "Setting Up…")
            btn.pressed = { [weak self] in
                self?.emit(["op":"setup_apply","harness":app.id], appCheck: app.isConfigured)
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
            layoutBar(in:card,insetLeft:50,y:barY,height:24,spacing:barSpacing,trailing:trailing)
            let right = (trailing.first?.frame.minX ?? cardWidth - 12) - allowance - barSpacing
            let tierWidth = tier.map { $0.frame.width + barSpacing } ?? 0
            title.frame.size.width = max(0,min(titleW,right - title.frame.minX - tierWidth))
            if let tier { tier.frame.origin = NSPoint(x: title.frame.maxX + barSpacing, y: barY + (24 - tier.frame.height) / 2) }
            appsView.addSubview(card); appTop -= 8
        }
        mountSettingsCards(appsView, on: scroll)

    }

    func buildChannelsTab(in scroll: NSScrollView, renderedHub: String) {
        let cardWidth = scroll.contentSize.width - 32
        let stratumWidth = cardWidth - 24
        let stratumTextWidth = stratumWidth - 24
        let allowance = settingsScrollerAllowance(scroll)
        let resolvedChannels = channelSettingsByHub[renderedHub] ?? []
        let visibleChannels = resolvedChannels.filter { $0.can_configure_hub == true || $0.hub_ready == true }
        let channelsView = ChannelsSettingsView(hub: renderedHub, channels: resolvedChannels, companion: self)
        channelsView.frame.size.width = scroll.contentSize.width
        channelsView.identifier = NSUserInterfaceItemIdentifier("settings_channels_tab")
        channelsView.isHidden = settingsActiveTab != "channels"
        let inboxHub = renderedHub
        let inboxConfig = settings?.hubs.first { $0.name == inboxHub }
        let inbox = inboxConfig == nil ? hubs.first { $0.name == inboxHub } : nil
        let inboxPaused = inboxConfig?.human_paused ?? inbox?.human_paused ?? false
        let inboxUser = inboxConfig?.user ?? inbox?.user ?? "local"
        let inboxAttachments = displayAttachments(inboxConfig?.attachments ?? inbox?.attachments ?? [])
        let inboxCurrent = (inboxConfig?.status ?? inbox?.status) == "connected" &&
            (inboxConfig?.human_state_status ?? inbox?.human_state_status) == "current"
        let scopeValue = inboxConfig?.human_scope ?? inbox?.human_scope ?? "self"
        let revision = inboxConfig?.human_control_revision ?? inbox?.human_control_revision ?? 1
        let pausePending = isPending("human_inbox_update",hub:inboxHub,target:"paused")
        let scopePending = isPending("human_inbox_update",hub:inboxHub,target:"scope")
        // Both actions share a revision; only the submitted action shows progress.
        let inboxEnabled = inboxCurrent && !pausePending && !scopePending
        let externalNames = inboxAttachments.filter { $0.location == "hub" }.map { cleanHostDisplay($0.display_name) }
        let deliveryNames = externalNames + ["Activity"] + inboxAttachments.filter { $0.location == "device" }.map { cleanHostDisplay($0.display_name) }
        let inboxSummary = inboxPaused ? (externalNames.isEmpty ? "Inbox alerts paused" : "Inbox paused · Activity & " + externalNames.joined(separator:" · ") + " paused") : deliveryNames.joined(separator:" · ")
        let inboxBar = roundedView(NSRect(x:16,y:308,width:cardWidth,height:48),radius:CitoMetrics.cardRadius,color:inboxPaused ? CitoPalette.warningSurface : .labelColor.withAppearanceAlpha(0.045))
        inboxBar.autoresizingMask = [.width]
        inboxBar.identifier = NSUserInterfaceItemIdentifier("inbox_control_bar")
        let inboxTitle = textLabel("You (@\(inboxUser))",NSRect(x:12,y:30,width:220,height:12),size:11,weight:.semibold)
        inboxTitle.identifier = NSUserInterfaceItemIdentifier("inbox_control_title"); inboxTitle.toolTip = inboxTitle.stringValue
        let inboxDetail = textLabel(inboxSummary,NSRect(x:12,y:5,width:stratumWidth,height:24),size:10,color:inboxPaused ? CitoPalette.warningText : CitoPalette.secondaryText)
        inboxDetail.identifier = NSUserInterfaceItemIdentifier("inbox_control_summary")
        inboxDetail.toolTip = inboxSummary + "\nExternal channels deliver alerts even when this Mac is offline."
        inboxDetail.setAccessibilityLabel(inboxSummary)
        inboxBar.addSubview(inboxTitle); inboxBar.addSubview(inboxDetail)
        let inboxToggle = MenuControl(inboxPaused ? "Resume Inbox" : "Pause Inbox",frame:NSRect(x:0,y:22,width:MenuControl.intrinsicWidth("Resume Inbox"),height:22))
        inboxToggle.identifier = NSUserInterfaceItemIdentifier("toggle_inbox")
        inboxToggle.isEnabled = inboxEnabled
        inboxToggle.toolTip = inboxPaused ? "Resume your shared inbox on \(inboxHub), preserving descriptions and channel bindings" : "Pause inbox alerts on \(inboxHub) across all devices and external channels"
        inboxToggle.pressed = { [weak self] in self?.updateInbox(paused:!inboxPaused,hub:inboxHub,revision:revision) }
        pendingLabel(inboxToggle,pausePending,inboxPaused ? "Resuming…" : "Pausing…")
        inboxBar.addSubview(inboxToggle)
        var inboxActions: [NSView] = []
        if let id = inboxConfig?.human_listener_id ?? inbox?.human_listener_id,
           (inboxConfig?.supports_workspace_scope ?? inbox?.supports_workspace_scope) == true,
           (inboxConfig?.human_can_set_scope ?? inbox?.human_can_set_scope) == true {
            let scope = makeScopeControl(hub:inboxHub,user:inboxUser,scope:scopeValue,id:id,y:22)
            scope.isEnabled = inboxEnabled
            pendingLabel(scope,scopePending,"Updating…")
            scope.pressed = { [weak self] in self?.updateInbox(scope:scopeValue == "workspace" ? "self" : "workspace",hub:inboxHub,revision:revision) }
            inboxBar.addSubview(scope); inboxActions.append(scope)
        }
        inboxActions.append(inboxToggle)
        layoutBar(in:inboxBar,y:13,height:22,trailing:inboxActions)
        let textWidth = max(0,(inboxActions.map { $0.frame.minX }.min() ?? cardWidth - 12) - 10 - 12)
        let summaryHeight = max(12, measureWrappedHeight(inboxSummary, width: max(1, textWidth - allowance), font: .systemFont(ofSize: 10)) + 2)
        let titleHeight: CGFloat = summaryHeight > 14 ? 12 : 14
        let stackGap: CGFloat = summaryHeight > 14 ? 0 : 1
        let inboxBarHeight = max(48, 20 + titleHeight + stackGap + summaryHeight)
        inboxBar.frame = NSRect(x: 16, y: 356 - inboxBarHeight, width: cardWidth, height: inboxBarHeight)
        for action in inboxActions { action.frame.origin.y = (inboxBarHeight - action.frame.height) / 2 }
        let stackY = (inboxBarHeight-titleHeight-summaryHeight-stackGap)/2
        inboxTitle.frame = NSRect(x:12,y:stackY+summaryHeight+stackGap,width:textWidth,height:titleHeight)
        inboxTitle.lineBreakMode = .byTruncatingMiddle
        inboxTitle.autoresizingMask = [.width]
        inboxDetail.autoresizingMask = [.width]
        inboxDetail.frame = NSRect(x:12,y:stackY,width:textWidth,height:summaryHeight)
        inboxDetail.maximumNumberOfLines = 0; inboxDetail.lineBreakMode = .byWordWrapping; inboxDetail.cell?.wraps = true
        channelsView.addSubview(inboxBar)
        var cursorY = inboxBar.frame.minY - 8
        let channelHubs = settings?.hubs.filter { $0.enabled != false }.map { $0.name } ?? hubs.filter { $0.enabled != false }.map { $0.name }
        if channelHubs.count > 1 { channelsView.addSubview(textLabel("Target Hub", NSRect(x: 16, y: cursorY-22, width: 78, height: 14), size: 10, weight: .semibold, color: CitoPalette.secondaryText)) }
        var channelPills: [NSView] = []
        for name in channelHubs {
            let shown = compactAddressLabel(name, maxWidth:cardWidth-86-allowance, font:.systemFont(ofSize:11,weight:.medium), paddingX:11)
            let width = max(64, MenuControl.intrinsicWidth(shown))
            let pill = MenuControl(shown, frame: NSRect(x: 0, y: 3, width: width, height: 22), radius: 11)
            pill.toolTip = name; pill.setAccessibilityLabel(name)
            pill.identifier = NSUserInterfaceItemIdentifier("channel_hub_selector:\(name)")
            pill.style = renderedHub == name ? .prominent : .subtleHeader
            pill.pressed = { [weak self] in
                guard let self = self else { return }
                self.selectedChannelHub = name
                self.emit(["op": "open_settings", "hub": name])
                self.rebuildSettingsPanel()
            }
            channelPills.append(pill)
        }
        if channelHubs.count > 1 {
            mountPillStrip(in: channelsView, frame: NSRect(x: 98, y: cursorY-26, width: cardWidth - 82 - allowance, height: 26),
                           id: "channel_hub_strip", spacing: 6, trackRadius: 13, pills: channelPills) {
                $0.identifier?.rawValue == "channel_hub_selector:\(renderedHub)"
            }
            cursorY -= 36
        }

        if let error = channelInventoryErrorsByHub[renderedHub], !error.isEmpty {
            let message = presentationError(error)
            let height = max(16, measureWrappedHeight(message, width: cardWidth - allowance, font: .systemFont(ofSize: 11)))
            let diagnostic = textLabel(message, NSRect(x:16,y:cursorY-height-4,width:cardWidth,height:height),size:11,color:CitoPalette.dangerText)
            diagnostic.autoresizingMask = [.width]
            diagnostic.identifier = NSUserInterfaceItemIdentifier("channel_inventory_error")
            diagnostic.maximumNumberOfLines = 0; diagnostic.lineBreakMode = .byWordWrapping
            diagnostic.toolTip = message
            channelsView.addSubview(diagnostic)
            cursorY -= height + 12
        }
        if !resolvedChannels.isEmpty && visibleChannels.isEmpty && channelInventoryErrorsByHub[renderedHub] == nil {
            let notice = textLabel("External channels are managed by your '\(renderedHub)' Hub administrator. None are enabled yet.", NSRect(x:16,y:cursorY-42,width:cardWidth,height:36),size:11,color:CitoPalette.secondaryText)
            notice.autoresizingMask = [.width]
            notice.identifier = NSUserInterfaceItemIdentifier("channels_managed_notice")
            notice.maximumNumberOfLines = 0; notice.lineBreakMode = .byWordWrapping
            channelsView.addSubview(notice); cursorY -= 48
        }
        var chCursorY: CGFloat = cursorY
        for ch in visibleChannels {
            let visibleFields = ch.fields.filter { $0.hidden != true }
            let channelKey = "\(renderedHub)|\(ch.id)"
            let channelUi = channelUiStates[channelKey] ?? ChannelUiState()
            let instruction = channelUi.pairingInstruction
            let instructionFont = NSFont.monospacedSystemFont(ofSize: 10.5, weight: .semibold)
            let instructionHeight = instruction.isEmpty ? 0 : max(16, measureWrappedHeight(instruction, width: stratumTextWidth - allowance, font: instructionFont) + 4)
            let guide = (ch.setup_guide ?? []).joined(separator:"\n")
            let hubFields = visibleFields.filter { $0.scope == "hub" && ch.can_configure_hub == true }
            let disclosure = ch.can_edit_personal == true && ch.pairing_supported && ch.can_pair == true && ch.pairing_manual_disclosure == true
            let manualExpanded = channelUi.manualEntry
            let userFields = visibleFields.filter { $0.scope != "hub" }
            let formFields = ch.can_edit_personal != true || (disclosure && !manualExpanded) ? [] : userFields
            let hasHubScopeFields = ch.has_hub_service == true
            let showHubStratum = hasHubScopeFields && (ch.can_configure_hub == true || ch.error_origin == "hub")
            let hubFieldsH = CGFloat(hubFields.count) * 26
            let userFieldsH = CGFloat(formFields.count) * 26 + (disclosure ? 24 : 0)
            let gatewayActive = ch.hub_ready == true
            let hubGatewayExpanded = ch.can_configure_hub == true && (!gatewayActive || channelUi.editingGateway)
            let isOutboundOnly = ch.inbound_mode == "outbound_only"
            let showInboundToggle = ch.supports_outbound_only == true && ch.can_configure_hub == true && gatewayActive && !hubGatewayExpanded
            let personalCollapsed = gatewayActive && ch.configured && !channelUi.editingPersonal && instruction.isEmpty
            let reusableBinding = !personalCollapsed && gatewayActive && instruction.isEmpty && !manualExpanded ? ch.reusable_binding : nil
            let guideH: CGFloat = !gatewayActive && !guide.isEmpty && !hubFields.isEmpty ? max(13, measureWrappedHeight(guide, width: stratumTextWidth - allowance, font: .systemFont(ofSize: 10)) + 2) : 0
            let currentUser=settings?.hubs.first(where:{$0.name==renderedHub})?.user ?? hubs.first(where:{$0.name==renderedHub})?.user ?? "local"
            let gatewayRole = !hubFields.isEmpty ? (guideH > 0 ? "" : "Configured once by the Hub owner. Shared by everyone on this Hub.") :
                (hasHubScopeFields ? (ch.hub_provided ? "Provided by '\(renderedHub)' Hub administrator — no gateway setup needed on your Mac." : "Not yet configured by the '\(renderedHub)' Hub administrator.") : "No shared gateway credentials required for this channel.")
            let gatewayRoleH = gatewayRole.isEmpty ? 0 : max(13, measureWrappedHeight(gatewayRole, width: stratumTextWidth - allowance, font: .systemFont(ofSize: 10)))
            let gatewayHasClear = ch.can_clear_hub == true
            let cleanConnectedState = personalCollapsed && !hubGatewayExpanded
            let gatewayStatusVisible = !(cleanConnectedState && ch.error_origin != "hub" && !isOutboundOnly)
            let serviceSuffix = ch.service_label.flatMap { $0.isEmpty ? nil : " · \($0)" } ?? ""
            let hubTitleText = (ch.configured || !showHubStratum ? "Hub Service" : "1. Shared Hub Service") + serviceSuffix
            let sharedOutbound = isOutboundOnly && gatewayActive && ch.error_origin != "hub"
            let gatewayStatusText = ch.error_origin == "hub" ? "Service needs attention" :
                (sharedOutbound ? "Outbound Only (Shared Bot)" : (gatewayActive ? "Service Ready" : "Service Token Required"))
            let gatewayStatusColor = ch.error_origin == "hub" ? CitoPalette.dangerText :
                (sharedOutbound ? CitoPalette.infoText : (gatewayActive ? CitoPalette.successText : CitoPalette.secondaryText))
            let gatewayStatus = statusLabel(gatewayStatusText,frame:NSRect(x:0,y:0,width:164,height:14),color:gatewayStatusColor,filled:gatewayActive && !sharedOutbound,size:9)
            let titleWidth = stratumWidth - 2 * CitoMetrics.cardInset
            let titleFont = NSFont.systemFont(ofSize:9.5,weight:.bold)
            let titleSpace = titleWidth - allowance - (gatewayStatusVisible ? gatewayStatus.frame.width + CitoMetrics.gap : 0)
            let separateTitle = showHubStratum && !serviceSuffix.isEmpty && measureTextWidth(hubTitleText, font:titleFont) > titleSpace
            let titleHeight = separateTitle ? max(14, measureWrappedHeight(hubTitleText, width:titleWidth - allowance, font:titleFont) + 2) : 14
            let extraTitleH = separateTitle ? titleHeight + CitoMetrics.gap : 0
            let twoRowGatewayTop = !hubGatewayExpanded && (showInboundToggle || (!serviceSuffix.isEmpty && (!hubFields.isEmpty || gatewayHasClear)))
            let hubStratumH: CGFloat = !showHubStratum ? 0 : (hubGatewayExpanded ? 36 + gatewayRoleH + guideH + hubFieldsH + (!hubFields.isEmpty || gatewayHasClear ? 32 : 0) : (twoRowGatewayTop ? 64 : 36))
            let collapsedSummary = Date() < (channelUi.savedUntil["user"] ?? .distantPast) ? "✓ Saved" : "Paired · \(ch.display_label ?? userFields.first?.value ?? "")"
            let collapsedSummaryHeight = max(16, measureWrappedHeight(collapsedSummary, width: stratumTextWidth - allowance, font: .systemFont(ofSize: 10)))
            let userStratumH: CGFloat = !gatewayActive ? 0 : (personalCollapsed ? 44 + collapsedSummaryHeight : 40 + userFieldsH + (instructionHeight > 0 ? instructionHeight + 34 : 0) + 34)
            let channelResult = (ch.warning == true || ch.status == "error") && !channelUi.testResultIsLatest ? nil : channelUi.testResult
            let showDetail = ch.status == "error" || ch.warning == true || channelResult != nil
            let detail = presentationError(channelResult?.detail ?? ch.detail)
            let detailHeight = showDetail ? max(14, measureWrappedHeight(detail, width: cardWidth - 58 - allowance, font: .systemFont(ofSize: 10.5))) : 0
            let headerHeight: CGFloat = showDetail ? 38 + detailHeight : 38
            let unavailable = ch.status == "error" && ch.fields.isEmpty
            let recoveryHeight: CGFloat = ch.status == "error" ? 34 : 0
            let cardHeight: CGFloat = unavailable ? max(90, headerHeight + 40) : headerHeight + hubStratumH + extraTitleH + (showHubStratum && gatewayActive ? CitoMetrics.gap : 0) + userStratumH + recoveryHeight + 8
            chCursorY -= cardHeight
            let chCard = roundedView(NSRect(x: 16, y: chCursorY, width: cardWidth, height: cardHeight), radius: 10,
                color: .labelColor.withAppearanceAlpha(0.045), border: .labelColor.withAppearanceAlpha(0.08))
            chCard.autoresizingMask = [.width]
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
            let badgeStatus = channelResult?.ok == false ? "error" : (ch.status == "ready" ?
                (ch.inbox_paused == true ? "inbox_paused" : (isOutboundOnly ? "outbound_only" : "ready")) : ch.status)
            switch badgeStatus {
            case "ready": chStatusText = "Connected"; chStatusColor = CitoPalette.successText
            case "outbound_only": chStatusText = "Outbound Only"; chStatusColor = CitoPalette.infoText
            case "paused": chStatusText = "Paused"; chStatusColor = CitoPalette.warningText
            case "inbox_paused": chStatusText = "Inbox paused"; chStatusColor = CitoPalette.warningText
            case "pairing_pending": chStatusText = "◌ Pairing…"; chStatusColor = CitoPalette.infoText
            case "error": chStatusText = "⚠ Error"; chStatusColor = CitoPalette.dangerText
            default: chStatusText = ch.hub_provided || ch.status == "hub_ready" ? "Account Not Paired" : "Not Set Up"; chStatusColor = CitoPalette.secondaryText
            }
            let chStatusLabel = statusLabel(chStatusText,frame:NSRect(x:240,y:cardHeight-26,width:226,height:16),color:chStatusColor,filled:ch.status == "ready" && badgeStatus != "outbound_only")
            chStatusLabel.identifier = NSUserInterfaceItemIdentifier("settings_channel_status:\(ch.id)")
            chCard.addSubview(chStatusLabel)
            layoutBar(in:chCard,insetLeft:44,insetRight:14,y:cardHeight-30,height:16,flexible:chTitle,trailing:[chStatusLabel])

            let chDetail = textLabel(detail, NSRect(x: 44, y: cardHeight - 32 - detailHeight, width: cardWidth - 58, height: detailHeight), size: 10.5, color:channelResult?.ok == false || ch.status == "error" ? CitoPalette.dangerText : (ch.warning == true ? CitoPalette.warningText : CitoPalette.secondaryText))
            chDetail.maximumNumberOfLines=0; chDetail.lineBreakMode = .byWordWrapping; chDetail.cell?.wraps = true; chDetail.toolTip = chDetail.stringValue
            chDetail.autoresizingMask = [.width]
            chDetail.identifier = NSUserInterfaceItemIdentifier("settings_channel_detail:\(ch.id)")
            if showDetail { chCard.addSubview(chDetail) }
            let chId = ch.id
            let collectValues: (String) -> [String: String] = { [weak self] scopeFilter in
                guard let self = self else { return [:] }
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

            if ch.status == "error" {
                let recoveryBar = NSView(frame:NSRect(x:0,y:unavailable ? 8 : cardHeight-headerHeight-28,width:chCard.bounds.width,height:22))
                recoveryBar.autoresizingMask = [.width]
                recoveryBar.identifier = NSUserInterfaceItemIdentifier("channel_recovery_bar:\(ch.id)")
                var recovery:[NSView]=[]
                let actions = [("Retry","retry",true),("Change Token…","change",ch.can_configure_hub == true),("Clear Token","clear",ch.can_clear_hub == true),("Send Test","test",ch.can_test == true && ch.configured),("Unpair","unpair",ch.can_unpair == true)]
                for (title,id,allowed) in actions where allowed {
                    let button=MenuControl(title,frame:NSRect(x:0,y:8,width:MenuControl.intrinsicWidth(title,font:.systemFont(ofSize:10)),height:22))
                    button.label.font = .systemFont(ofSize:10)
                    button.identifier=NSUserInterfaceItemIdentifier("recover_channel:\(ch.id):\(id)")
                    button.style = ["clear","unpair"].contains(id) ? .destructiveHover : .neutral
                    let recoveryOp = ["clear":"disconnect_human_channel", "test":"channel_test", "unpair":"channel_unpair"][id]
                    if let op = recoveryOp { pendingLabel(button,isPending(op,hub:renderedHub,target:ch.id),id == "test" ? "Testing…" : id == "clear" ? "Clearing…" : "Unpairing…") }
                    button.pressed={ [weak self] in guard let self=self else{return}; switch id {
                    case "clear": self.emit(["op":"disconnect_human_channel","hub":renderedHub,"channel_id":ch.id,"reset_hub":true,"reset_user":false])
                    case "test": self.emit(["op":"channel_test","hub":renderedHub,"channel_id":ch.id,"values":collectValues("user").merging(collectValues("hub")) { _, new in new }])
                    case "unpair": self.emit(["op":"channel_unpair","hub":renderedHub,"channel_id":ch.id,"reset_user":true])
                    default:
                        self.channelUiStates[channelKey, default: ChannelUiState()].editingGateway = id == "change"
                        self.emit(["op":"open_settings","hub":renderedHub])
                        if id == "change" { self.rebuildSettingsPanel(revealChannel: ch.id, revealPairing: false) }
                    } }
                    recoveryBar.addSubview(button); recovery.append(button)
                }
                layoutBar(in:recoveryBar,insetLeft:12,insetRight:12,y:0,height:22,spacing:4,trailing:recovery)
                chCard.addSubview(recoveryBar)
                if unavailable {
                    channelsView.addSubview(chCard)
                    chCursorY -= 10
                    continue
                }
            }

            let hubStratum = roundedView(NSRect(x:12,y:8+userStratumH+(gatewayActive ? 6 : 0),width:stratumWidth,height:hubStratumH + extraTitleH),radius:8,
                color:.labelColor.withAppearanceAlpha(0.035),border:nil)
            hubStratum.autoresizingMask = [.width]
            hubStratum.identifier=NSUserInterfaceItemIdentifier("stratum_hub:\(ch.id)")
            var gatewayTop: [NSView] = [], gatewayLeading: [NSView] = [], gatewayTrailing: [NSView] = []
            var personalActions: [NSView] = [], pairingActions: [NSView] = []
            let hubTitle=textLabel(hubTitleText,NSRect(x:CitoMetrics.cardInset,y:hubStratum.bounds.height-11-titleHeight,width:titleWidth,height:titleHeight),size:9.5,weight:.bold,color:CitoPalette.secondaryText)
            if separateTitle {
                hubTitle.maximumNumberOfLines = 0; hubTitle.lineBreakMode = .byCharWrapping; hubTitle.cell?.wraps = true
            }
            hubTitle.autoresizingMask = [.width]
            hubTitle.identifier=NSUserInterfaceItemIdentifier("stratum_hub_title:\(ch.id)")
            hubTitle.toolTip = serviceSuffix.isEmpty ? gatewayRole : [hubTitleText, gatewayRole].filter { !$0.isEmpty }.joined(separator:"\n")
            hubStratum.addSubview(hubTitle)
            gatewayStatus.identifier=NSUserInterfaceItemIdentifier("stratum_hub_status:\(ch.id)")
            gatewayStatus.isHidden = !gatewayStatusVisible
            hubStratum.addSubview(gatewayStatus)
            if gatewayHasClear {
                let clear=MenuControl("Clear Token",frame:NSRect(x:0,y:7,width:MenuControl.intrinsicWidth("Clear Token", font:.systemFont(ofSize:10.5)),height:22),radius:6)
                clear.label.font = .systemFont(ofSize:10.5)
                clear.style = .destructiveHover
                clear.identifier=NSUserInterfaceItemIdentifier("clear_channel_service:\(ch.id)")
                clear.pressed={ [weak self] in guard let self=self else{return}; self.emit(["op":"disconnect_human_channel","hub":renderedHub,"channel_id":ch.id,"reset_hub":true,"reset_user":false]) }
                pendingLabel(clear,isPending("disconnect_human_channel",hub:renderedHub,target:ch.id),"Clearing…")
                hubStratum.addSubview(clear)
                if hubGatewayExpanded { gatewayLeading.append(clear) } else { gatewayTop.append(clear) }
            }
            if hubGatewayExpanded && gatewayRoleH > 0 {
                let gatewayDescription=textLabel(gatewayRole,NSRect(x:12,y:hubStratumH-30-gatewayRoleH,width:stratumTextWidth,height:gatewayRoleH),size:10,color:CitoPalette.secondaryText)
                gatewayDescription.maximumNumberOfLines=0; gatewayDescription.lineBreakMode = .byWordWrapping
                gatewayDescription.autoresizingMask = [.width]
                hubStratum.addSubview(gatewayDescription)
            }
            if hubGatewayExpanded && guideH>0 {
                let box=textLabel(guide,NSRect(x:12,y:hubStratumH-30-gatewayRoleH-guideH,width:stratumTextWidth,height:guideH),size:10,color:CitoPalette.secondaryText)
                box.identifier=NSUserInterfaceItemIdentifier("channel_setup_guide:\(ch.id)")
                box.maximumNumberOfLines=0; box.lineBreakMode = .byWordWrapping
                box.autoresizingMask = [.width]
                hubStratum.addSubview(box)
            }
            let userStratum=roundedView(NSRect(x:12,y:8,width:stratumWidth,height:userStratumH),radius:8,
                color:.labelColor.withAppearanceAlpha(0.035),border:nil)
            userStratum.autoresizingMask = [.width]
            userStratum.identifier=NSUserInterfaceItemIdentifier("stratum_user:\(ch.id)")
            let userTitle=textLabel((ch.configured || !showHubStratum ? "" : "2. ") + "Your Personal Account (@\(currentUser))",NSRect(x:12,y:userStratumH-21,width:stratumTextWidth,height:13),size:9.5,weight:.bold,color:CitoPalette.secondaryText)
            userTitle.lineBreakMode = .byTruncatingMiddle; userTitle.toolTip = userTitle.stringValue
            userTitle.autoresizingMask = [.width]
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
                    inputField.autoresizingMask = [.width]
                    inputField.delegate = channelsView
                    inputField.toolTip=f.help_text; container.addSubview(inputField)
                    fieldY-=26
                }
            }
            if showHubStratum { chCard.addSubview(hubStratum) }
            if gatewayActive { chCard.addSubview(userStratum) }
            func gate(_ control: MenuControl) { control.isEnabled=gatewayActive }
            if !instruction.isEmpty {
                let instrLabel = textLabel(instruction, NSRect(x: 12, y: userStratumH - 40 - CGFloat(formFields.count) * 26 - instructionHeight, width: stratumTextWidth, height: instructionHeight), size: 10.5, weight: .semibold, color: CitoPalette.infoText)
                instrLabel.autoresizingMask = [.width]
                instrLabel.maximumNumberOfLines = 0
                instrLabel.lineBreakMode = .byWordWrapping
                instrLabel.isSelectable = true
                instrLabel.toolTip = instruction
                instrLabel.font = instructionFont
                let instructionId = "pairing_instruction:\(ch.id)"
                instrLabel.identifier = NSUserInterfaceItemIdentifier(instructionId)
                instrLabel.delegate = self
                settingsFieldControls[instructionId] = instrLabel
                userStratum.addSubview(instrLabel)
            }

            if !hubFields.isEmpty {
                if hubGatewayExpanded {
                    let save = MenuControl("Save Gateway Token",frame:NSRect(x:304,y:6,width:140,height:24))
                    save.identifier=NSUserInterfaceItemIdentifier("save_hub_service:\(ch.id)"); save.style = .prominent
                    save.pressed={ [weak self] in guard let self=self else{return}; self.emit(["op":"save_hub_service","hub":renderedHub,"channel_id":ch.id,"scope":"hub","values":collectValues("hub")]) }
                    pendingLabel(save,channelSavePending("save_hub_service",hub:renderedHub,channel:ch.id,scope:"hub"),"Saving…")
                    hubStratum.addSubview(save); gatewayTrailing.append(save)
                    if gatewayActive {
                        let cancel=MenuControl("Cancel",frame:NSRect(x:232,y:6,width:66,height:24))
                        cancel.identifier=NSUserInterfaceItemIdentifier("cancel_edit_hub_service:\(ch.id)")
                        cancel.pressed={ [weak self] in self?.clearChannelDrafts(ch, scope:"hub", hub:renderedHub); self?.channelUiStates[channelKey]?.resetGateway(); self?.rebuildSettingsPanel() }
                        hubStratum.addSubview(cancel); gatewayTrailing.insert(cancel, at: 0)
                    }
                } else {
                    let edit=MenuControl("Change Token…",frame:NSRect(x:0,y:7,width:MenuControl.intrinsicWidth("Change Token…", font:.systemFont(ofSize:10.5)),height:22))
                    edit.style = .neutral
                    edit.label.font = .systemFont(ofSize:10.5)
                    edit.identifier=NSUserInterfaceItemIdentifier("edit_hub_service:\(ch.id)")
                    edit.pressed={ [weak self] in self?.channelUiStates[channelKey, default: ChannelUiState()].editingGateway = true; self?.rebuildSettingsPanel(revealChannel: ch.id, revealPairing: false) }
                    hubStratum.addSubview(edit); gatewayTop.insert(edit, at: 0)
                }
            }
            if showInboundToggle {
                let title = isOutboundOnly ? "Enable Replies" : "Use Outbound-Only"
                let toggle = MenuControl(title, frame:NSRect(x:0,y:7,width:0,height:22))
                toggle.identifier = NSUserInterfaceItemIdentifier("toggle_channel_inbound_mode:\(ch.id)")
                pendingLabel(toggle,isPending("save_hub_service",hub:renderedHub,target:ch.id),"Applying…")
                toggle.pressed = { [weak self] in
                    guard let self, !self.isPending("save_hub_service",hub:renderedHub,target:chId) else { return }
                    self.emit(["op":"save_hub_service","hub":renderedHub,"channel_id":chId,
                               "inbound_mode":isOutboundOnly ? "primary" : "outbound_only"])
                }
                hubStratum.addSubview(toggle); gatewayTop.append(toggle)
            }
            if disclosure && !personalCollapsed && gatewayActive {
                let title = manualExpanded ? "▾ Hide manual entry" : "▸ Enter \(userFields.first?.label ?? "account") manually…"
                let width = min(userStratum.bounds.width - 24 - allowance, MenuControl.intrinsicWidth(title, paddingX: 4))
                let manual = MenuControl(title,frame:NSRect(x:12,y:instruction.isEmpty ? 38 : 66,width:width,height:22),paddingX:0)
                manual.label.alignment = .left; manual.layoutContents()
                manual.style = .subtleHeader; manual.identifier=NSUserInterfaceItemIdentifier("manual_channel:\(ch.id)")
                manual.pressed={ [weak self] in guard let self=self else{return}; self.channelUiStates[channelKey, default: ChannelUiState()].manualEntry.toggle(); self.rebuildSettingsPanel(revealChannel: ch.id, revealPairing: false, revealEditorScope: "user") }
                userStratum.addSubview(manual)
            }
            if ch.can_pair == true && (!personalCollapsed || ch.can_edit_personal != true) && gatewayActive && instruction.isEmpty {
                let pairLabel = ch.configured ? "Re-pair…" : (ch.pairing_action_label ?? "Pair Account…")
                let pairBtn = MenuControl(pairLabel, frame: NSRect(x: 12, y: 6, width: MenuControl.intrinsicWidth(pairLabel, leadingSymbol: "link"), height: 24), leadingSymbol: "link", radius: 6)
                pairBtn.toolTip=ch.pairing_help_text
                pairBtn.identifier = NSUserInterfaceItemIdentifier("pair_channel:\(ch.id)")
                pairBtn.pressed = { [weak self] in
                    guard let self = self else { return }
                    self.emit(["op": "channel_pair", "hub": renderedHub, "channel_id": chId, "values": collectValues("user")])
                }
                gate(pairBtn); pendingLabel(pairBtn,channelUi.waitingForPairCode,"Pairing…"); pairBtn.style = ch.configured || reusableBinding != nil ? .neutral : .prominent; userStratum.addSubview(pairBtn); personalActions.append(pairBtn)
            }

            if !instruction.isEmpty {
                let copy=MenuControl("Copy Command",frame:NSRect(x:0,y:0,width:0,height:22),leadingSymbol:"doc.on.doc",radius:6)
                copy.label.font = .systemFont(ofSize:10.5)
                copy.identifier=NSUserInterfaceItemIdentifier("copy_pair_command:\(ch.id)")
                copy.pressed={ [weak self, weak copy] in
                    guard let self = self else { return }
                    self.copyText(channelUi.pairingCommand ?? instruction, control: copy)
                    self.channelUiStates[channelKey, default: ChannelUiState()].pairCopiedUntil = Date().addingTimeInterval(1.2)
                    self.rebuildFeedbackAfter(1.2)
                }
                if Date() < channelUi.pairCopiedUntil { copy.copied(for: max(0.01, channelUi.pairCopiedUntil.timeIntervalSinceNow)) }
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
                    self.emit(["op":"channel_cancel_pair","hub":renderedHub,"channel_id":chId])
                }
                pendingLabel(cancel,isPending("channel_cancel_pair",hub:renderedHub,target:ch.id),"Cancelling…")
                userStratum.addSubview(cancel); pairingActions.append(cancel)
            }
            if ch.configured {
                let toggleTitle=(ch.enabled ? "Pause " : "Resume ") + ch.display_name
                let toggleFont: NSFont = personalCollapsed ? .systemFont(ofSize: 10.5) : .systemFont(ofSize: 11, weight: .medium)
                let toggle=MenuControl(toggleTitle,frame:NSRect(x:130,y:6,width:MenuControl.intrinsicWidth(toggleTitle,font:toggleFont),height:24),radius:6)
                toggle.label.font = toggleFont
                toggle.identifier=NSUserInterfaceItemIdentifier("toggle_channel:\(ch.id)")
                toggle.maximumWidth=170
                toggle.style = ch.enabled ? .neutral : .prominent
                toggle.pressed={ [weak self] in guard let self=self else{return}; self.emit(["op":"channel_toggle","hub":renderedHub,"channel_id":chId,"enabled":!ch.enabled]) }
                pendingLabel(toggle,isPending("channel_toggle",hub:renderedHub,target:ch.id),ch.enabled ? "Pausing…" : "Resuming…")
                userStratum.addSubview(toggle); personalActions.append(toggle)
                let unpair=MenuControl("Unpair",frame:NSRect(x:196,y:6,width:MenuControl.intrinsicWidth("Unpair"),height:24),radius:6)
                unpair.style = .destructiveHover
                unpair.identifier=NSUserInterfaceItemIdentifier("unpair_channel:\(ch.id)")
                unpair.pressed={ [weak self] in guard let self=self else{return}; self.emit(["op":"channel_unpair","hub":renderedHub,"channel_id":chId,"reset_user":true]) }
                unpair.isHidden = ch.can_unpair != true
                pendingLabel(unpair,isPending("channel_unpair",hub:renderedHub,target:ch.id),"Unpairing…")
                userStratum.addSubview(unpair); personalActions.append(unpair)
            }
            let testBtn = MenuControl("Send Test", frame: NSRect(x: 290, y: 6, width: MenuControl.intrinsicWidth("Send Test"), height: 24), radius: 6)
            testBtn.identifier = NSUserInterfaceItemIdentifier("test_channel:\(ch.id)")
            testBtn.pressed = { [weak self] in
                guard let self = self else { return }
                self.emit(["op": "channel_test", "hub": renderedHub, "channel_id": chId, "values": collectValues("user").merging(collectValues("hub")) { _,new in new }])
            }
            testBtn.isHidden = ch.can_test != true || (ch.status == "error" && !ch.configured) || (disclosure && !ch.configured && !manualExpanded)
            testBtn.isEnabled = ch.canSendTest(drafts:channelUi.drafts,visibleFields:formFields)
            pendingLabel(testBtn,isPending("channel_test",hub:renderedHub,target:ch.id),"Testing…")
            userStratum.addSubview(testBtn)

            let saveBtn = MenuControl("Save", frame: NSRect(x: 376, y: 6, width: 68, height: 24), radius: 6)
            saveBtn.identifier = NSUserInterfaceItemIdentifier("save_channel:\(ch.id)")
            saveBtn.pendingWidth = MenuControl.intrinsicWidth("Saving…")
            saveBtn.style = .prominent
            saveBtn.pressed = { [weak self] in
                guard let self = self else { return }
                let values = collectValues("user")
                self.emit(["op": "channel_save", "hub": renderedHub, "channel_id": chId, "enabled": true, "values": values])
            }
            gate(saveBtn)
            if ch.can_edit_personal == true && !personalCollapsed && gatewayActive && (!disclosure || manualExpanded) { userStratum.addSubview(saveBtn) } else { saveBtn.isHidden=true }
            if let until = channelUi.savedUntil["user"], Date() < until { saveBtn.copied("✓ Saved", for: max(0.01, until.timeIntervalSinceNow)) }
            pendingLabel(saveBtn,channelSavePending("channel_save",hub:renderedHub,channel:ch.id,scope:"user"),"Saving…")

            layoutBar(in: hubStratum, y: hubStratumH-29, height: 22, flexible: separateTitle ? nil : hubTitle,
                      trailing: twoRowGatewayTop ? [gatewayStatus] : [gatewayStatus] + gatewayTop)
            if twoRowGatewayTop { layoutBar(in: hubStratum, y: 7, height: 22, trailing: gatewayTop) }
            if hubGatewayExpanded { layoutBar(in: hubStratum, y: 8, height: 24, leading: gatewayLeading, trailing: gatewayTrailing) }
            if personalCollapsed {
                let saved = Date() < (channelUi.savedUntil["user"] ?? .distantPast)
                let summary = textLabel(collapsedSummary, NSRect(x:12,y:38,width:stratumTextWidth,height:collapsedSummaryHeight),size:10,color:saved ? CitoPalette.successText : CitoPalette.secondaryText)
                summary.identifier = NSUserInterfaceItemIdentifier("channel_summary:\(ch.id)"); summary.toolTip = summary.stringValue; summary.lineBreakMode = .byWordWrapping; summary.maximumNumberOfLines = 0
                summary.autoresizingMask = [.width]
                userStratum.addSubview(summary)
                let edit = MenuControl("Edit…",frame:NSRect(x:0,y:6,width:MenuControl.intrinsicWidth("Edit…"),height:24))
                edit.identifier = NSUserInterfaceItemIdentifier("edit_user_channel:\(ch.id)"); edit.style = .neutral
                edit.pressed = { [weak self] in self?.channelUiStates[channelKey, default: ChannelUiState()].editingPersonal = true; self?.channelUiStates[channelKey, default: ChannelUiState()].manualEntry = true; self?.rebuildSettingsPanel(revealChannel: ch.id, revealPairing: false, revealEditorScope: "user") }
                edit.isHidden = ch.can_edit_personal != true
                userStratum.addSubview(edit)
                for case let button as MenuControl in [testBtn, edit] + personalActions {
                    button.label.font = .systemFont(ofSize: 10.5)
                }
                layoutBar(in:userStratum,y:7,height:24,spacing:8,trailing:[testBtn] + personalActions.dropLast() + [edit] + personalActions.suffix(1))
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
                    cancel.pressed={ [weak self] in self?.clearChannelDrafts(ch,scope:"user",hub:renderedHub); self?.channelUiStates[channelKey]?.resetPersonal(); self?.rebuildSettingsPanel() }
                    userStratum.addSubview(cancel); trailing.insert(cancel,at:0)
                }
                if let reuse = reusableBinding, let serviceId = ch.service_identity, !serviceId.isEmpty {
                    let actions = personalActions + trailing
                    layoutBar(in:userStratum,y:8,height:24,trailing:actions)
                    let right = actions.filter { !$0.isHidden }.map { $0.frame.minX - CitoMetrics.gap }.min() ?? (userStratum.bounds.width - CitoMetrics.cardInset)
                    let title = "Link \(reuse.display_label) (from '\(reuse.source_hub)')"
                    let maxWidth = max(0, right - CitoMetrics.cardInset - allowance)
                    let shown = compactAddressLabel(title, maxWidth:maxWidth - 17, font:.systemFont(ofSize:11,weight:.medium), paddingX:11)
                    let link = MenuControl(shown,frame:NSRect(x:0,y:8,width:0,height:24),leadingSymbol:"link")
                    link.maximumWidth = maxWidth
                    link.identifier = NSUserInterfaceItemIdentifier("reuse_channel_binding:\(ch.id)")
                    link.style = .prominent; link.toolTip = title; link.setAccessibilityLabel(title)
                    pendingLabel(link,isPending("channel_save",hub:renderedHub,target:ch.id),"Linking…")
                    link.pressed = { [weak self] in
                        guard let self, !self.isPending("channel_save",hub:renderedHub,target:chId) else { return }
                        self.emit(["op":"channel_save","hub":renderedHub,"channel_id":chId,"scope":"user","enabled":true,
                                   "expected_service_identity":serviceId,"values":reuse.values,"display_label":reuse.display_label])
                    }
                    userStratum.addSubview(link)
                    layoutBar(in:userStratum,insetRight:userStratum.bounds.width-right,y:8,height:24,trailing:[link])
                } else if !trailing.contains(where: { !$0.isHidden }) {
                    layoutBar(in:userStratum,y:8,height:24,trailing:personalActions)
                } else {
                    layoutBar(in:userStratum,y:8,height:24,leading:personalActions,trailing:trailing)
                }
                if !pairingActions.isEmpty { layoutBar(in: userStratum, y: 40, height: 22, trailing: pairingActions) }
            }
            channelsView.addSubview(chCard)
            chCursorY -= 10
        }
        mountSettingsCards(channelsView, on: scroll)

    }

    func buildHubsTab(in scroll: NSScrollView) {
        let cardWidth = scroll.contentSize.width - 32
        let fieldWidth = cardWidth - 28
        let allowance = settingsScrollerAllowance(scroll)
        let hubsView = NSView(frame: NSRect(x: 0, y: 0, width: scroll.contentSize.width, height: 368))
        hubsView.identifier = NSUserInterfaceItemIdentifier("settings_hubs_tab")
        hubsView.isHidden = settingsActiveTab != "hubs"

        let hostHub = settings?.host_hub ?? HostHubSummary(
            enabled: false, running: false, port: 4291,
            endpoint_url: "http://127.0.0.1:4291", token: "",
            invite_command: "cito setup --hub lan --url http://127.0.0.1:4291 --default", auth_mode: nil, error: nil
        )
        let hubError=(hostHub.error?.trimmingCharacters(in:.whitespacesAndNewlines)).flatMap{$0.isEmpty ? nil : $0}
        let hostingUnavailable = !hostHub.enabled || !hostHub.running || hubError != nil
        let hostError = (hostPort.error ?? hubError).flatMap { $0.isEmpty ? nil : $0 }
        let hostExpanded = hostHub.enabled || hostPort.pending || hostPort.draft != nil || hostError != nil
        let hasAddressChoice = hostHub.tailscale_url != nil && hostHub.lan_url != nil && hostHub.tailscale_url != hostHub.lan_url
        let hostNotice = hostError ?? (!hostHub.enabled && hostExpanded ? "Choose Start Sharing to accept connections" : nil)
        let hostNoticeText = hostNotice.map(presentationError)
        let noticeHeight = hostNoticeText.map {
            max(18, measureWrappedHeight($0, width: fieldWidth - allowance, font: .systemFont(ofSize: 11)) + 2)
        } ?? 0
        let hostHeight: CGFloat = hostExpanded ? (hasAddressChoice ? 160 : 120) + (noticeHeight == 0 ? 0 : noticeHeight + 10) : 46
        let hostCard = roundedView(NSRect(x: 16, y: 356 - hostHeight, width: cardWidth, height: hostHeight), radius: 10,
            color: .labelColor.withAppearanceAlpha(0.045), border: .labelColor.withAppearanceAlpha(0.08))
        hostCard.autoresizingMask = [.width]
        hostCard.identifier = NSUserInterfaceItemIdentifier("host_hub_card")
        let hostTitle=textLabel(hostHub.enabled && !hostHub.running && hostError == nil ? "Share This Mac as a Hub · Starting…" : "Share This Mac as a Hub", NSRect(x:14,y:0,width:280,height:17),size:13,weight:.semibold)
        hostTitle.identifier = NSUserInterfaceItemIdentifier("host_hub_title")
        hostCard.addSubview(hostTitle)
        if hostExpanded, let notice = hostNoticeText {
            let subtitle = textLabel(notice, NSRect(x:14,y:hostHeight-34-noticeHeight,width:fieldWidth,height:noticeHeight), size:11, color:hostError == nil ? CitoPalette.secondaryText : CitoPalette.dangerText)
            subtitle.autoresizingMask = [.width]
            subtitle.identifier = NSUserInterfaceItemIdentifier("host_hub_error")
            subtitle.maximumNumberOfLines = 0; subtitle.lineBreakMode = .byWordWrapping; subtitle.cell?.wraps = true
            subtitle.toolTip = notice
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
            guard let self, !self.isPending("set_host_hub", target: "sharing") else { return }
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
        let portField = makeEditableField("host_hub_port",frame:NSRect(x:0,y:12,width:58,height:24),placeholder:"Port",value:hostPort.draft ?? "\(hostHub.port)",accessibilityLabel:"Host Hub port")
        hostCard.addSubview(portLabel); hostCard.addSubview(portField)
        let auth=MenuControl(authMode=="none" ? "Access: Trusted" : "Access: Token",frame:NSRect(x:286,y:12,width:112,height:24),radius:6)
        auth.style = .neutral
        auth.identifier=NSUserInterfaceItemIdentifier("toggle_host_hub_auth")
        // Only Start/Stop owns `enabled`; Access and Apply can run before it settles.
        auth.pressed={ [weak self] in
            guard let self, !self.isPending("set_host_hub", target: "access") else { return }
            guard let port = self.validHostPort(fallback:hostHub.port) else { return }
            self.emit(["op":"set_host_hub","port":port,"auth_mode":authMode=="none" ? "token" : "none"])
        }
        pendingLabel(auth,isPending("set_host_hub",target:"access"),"Applying…")
        hostCard.addSubview(auth)
        let apply=MenuControl("Apply",frame:NSRect(x:394,y:12,width:72,height:24),radius:6)
        apply.identifier=NSUserInterfaceItemIdentifier("apply_host_hub_port")
        apply.isEnabled = !hostPort.pending && Int((hostPort.draft ?? "\(hostHub.port)").trimmingCharacters(in: .whitespacesAndNewlines)) != hostHub.port
        if let until = hostPort.savedUntil, Date() < until { apply.copied("✓ Saved", for: max(0.01, until.timeIntervalSinceNow)) }
        pendingLabel(apply,hostPort.pending,"Applying…")
        apply.pressed={ [weak self] in
            guard let self=self else{return}
            guard let port = self.validHostPort(fallback: hostHub.port) else { return }
            let requestId = UUID().uuidString
            self.hostPort.error=nil; self.hostPort.pending=true; self.hostPort.savedUntil=nil; self.hostPort.submittedPort=port
            self.hostPort.requestId = requestId
            self.emit(["op":"set_host_hub","port":port,"request_id":requestId])
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
        if Date() < hubInvitePastedUntil { pasteBtn.copied("✓ Joined", for: max(0.01, hubInvitePastedUntil.timeIntervalSinceNow)) }
        pendingLabel(pasteBtn,hubInvitePending,"Joining…")
        pasteBtn.pressed = { [weak self] in self?.handlePasteHubInvite() }
        hubsView.addSubview(pasteBtn)

        let addBtn = MenuControl("Add Hub…", frame: NSRect(x: 396, y: 190 + hubOffset, width: 104, height: 22), leadingSymbol: "plus", radius: 6)
        addBtn.identifier = NSUserInterfaceItemIdentifier("open_add_hub")
        addBtn.pressed = { [weak self] in
            guard let self = self else { return }
            let open = self.hubForm.isEditing || !self.hubForm.isOpen
            self.hubForm = HubFormState(mode: open ? .add : .closed)
            self.rebuildSettingsPanel(revealAddHub: open, syncFields: false)
            if let field = self.hubFormFocusTarget() { self.settingsPanel?.makeFirstResponder(field) }
        }
        addBtn.isEnabled = hubForm.pendingSave == nil
        hubsView.addSubview(addBtn)
        layoutBar(in:hubsView,insetLeft:16,insetRight:16,y:190+hubOffset,height:22,flexible:connectedTitle,trailing:[pasteBtn,addBtn])

        if hubForm.isOpen {
            let errorText = hubForm.error.map(presentationError)
            let errorHeight = errorText.map { measureWrappedHeight($0, width: fieldWidth - allowance, font: .systemFont(ofSize: 10)) + 4 } ?? 0
            let errorSpace: CGFloat = errorText == nil ? 0 : errorHeight + 6
            let isOIDC = !editingBuiltinHub && hubForm.auth == "oidc"
            let oidcExpanded = isOIDC && hubForm.oidcExpanded
            let formHeight: CGFloat = (editingBuiltinHub ? 124 : oidcExpanded ? 298 : 252) + errorSpace
            let form = SettingsHubForm(frame: NSRect(x: 16, y: 180 + hubOffset - formHeight, width: cardWidth, height: formHeight), radius: 10,
                color: .labelColor.withAppearanceAlpha(0.045), border: .labelColor.withAppearanceAlpha(0.08))
            form.autoresizingMask = [.width]
            form.identifier = NSUserInterfaceItemIdentifier("add_hub_form")

            func add(_ view: NSView, sizing: NSView.AutoresizingMask = [.width]) {
                view.autoresizingMask = sizing
                form.addSubview(view)
            }
            @discardableResult
            func fieldLabel(_ title: String, _ frame: NSRect, sizing: NSView.AutoresizingMask = [.width], id: String? = nil) -> NSTextField {
                let label = textLabel(title,frame,size:10,weight:.semibold,color:CitoPalette.secondaryText)
                if let id { label.identifier = NSUserInterfaceItemIdentifier(id) }
                add(label, sizing: sizing)
                return label
            }
            if editingBuiltinHub {
                add(textLabel("This Mac (Local Hub)",NSRect(x:14,y:94,width:fieldWidth,height:18),size:12.5,weight:.semibold))
                fieldLabel("Username (Optional)",NSRect(x:14,y:72,width:fieldWidth,height:14))
                add(makeEditableField("settings_hub_user",frame:NSRect(x:14,y:44,width:fieldWidth,height:24),placeholder:"Defaults to your Mac username",value:hubForm.user,accessibilityLabel:"Hub username"))
            } else {
                let nameX = cardWidth - 160
                fieldLabel("Hub URL",NSRect(x:14,y:179,width:nameX-22,height:13))
                fieldLabel("Hub Name (Optional)",NSRect(x:nameX,y:179,width:146,height:13),sizing:[.minXMargin])
                add(makeEditableField("settings_hub_url",frame:NSRect(x:14,y:153,width:nameX-22,height:24),placeholder:"http://192.168.1.10:4291",value:hubForm.url,accessibilityLabel:"Hub URL"))
                let nameField = makeEditableField("settings_hub_name",frame:NSRect(x:nameX,y:153,width:146,height:24),placeholder:hubNamePlaceholder,value:hubForm.name,accessibilityLabel:"Hub name")
                if hubForm.isEditing { nameField.toolTip = hubForm.originalName }
                add(nameField, sizing: [.minXMargin])
                fieldLabel("Access",NSRect(x:14,y:125,width:48,height:14),sizing:[])
                let authOptions = [("none","Trusted Network"),("token","Shared Token"),("oidc","Team Sign-In")]
                let authTrack = makePillTrack(NSRect(x:68,y:118,width:cardWidth-82,height:26),radius:13,bordered:false)
                authTrack.identifier = NSUserInterfaceItemIdentifier("settings_hub_auth_track")
                var authPills: [NSView] = []
                for (mode,label) in authOptions {
                    let pill=MenuControl(label,frame:NSRect(x:0,y:3,width:MenuControl.intrinsicWidth(label),height:22),radius:11)
                    pill.identifier=NSUserInterfaceItemIdentifier("settings_hub_auth:\(mode)")
                    pill.style = hubForm.auth == mode ? .prominent : .subtleHeader
                    pill.pressed={ [weak self] in
                        guard let self=self else{return}
                        if self.hubForm.auth != mode { self.hubForm.generation += 1 }
                        self.hubForm.auth=mode; self.hubForm.authExplicit=true; self.rebuildSettingsPanel(revealAddHub: true)
                    }
                    authTrack.addSubview(pill); authPills.append(pill)
                }
                layoutBar(in: authTrack, insetLeft: 2, insetRight: 2, y: 2, height: 22, spacing: 4, leading: authPills)
                authTrack.frame.size.width = authPills.reduce(CGFloat(4)) { $0 + $1.frame.width } + CGFloat(max(0,authPills.count-1)) * 4
                form.addSubview(authTrack)
                if hubForm.auth == "none" {
                    fieldLabel("Username (Optional)",NSRect(x:14,y:97,width:fieldWidth,height:14))
                    add(makeEditableField("settings_hub_user",frame:NSRect(x:14,y:71,width:fieldWidth,height:24),placeholder:"Defaults to your Mac username",value:hubForm.user,accessibilityLabel:"Hub username"))
                } else if hubForm.auth == "token" {
                    let userX = cardWidth - 184
                    fieldLabel("Shared Access Token",NSRect(x:14,y:97,width:userX-22,height:14))
                    fieldLabel("Username (Optional)",NSRect(x:userX,y:97,width:170,height:14),sizing:[.minXMargin])
                    add(makeEditableField("settings_hub_token",frame:NSRect(x:14,y:71,width:userX-22,height:24),placeholder:hubForm.isEditing ? "•••••• (Saved — enter new token to replace)" : "Paste token from host Mac",value:hubForm.token,secret:true,accessibilityLabel:"Hub token"))
                    add(makeEditableField("settings_hub_user",frame:NSRect(x:userX,y:71,width:170,height:24),placeholder:"e.g. alice",value:hubForm.user,accessibilityLabel:"Hub username"), sizing: [.minXMargin])
                } else if isOIDC {
                    if oidcExpanded {
                        for control in form.subviews { control.frame.origin.y += 62 }
                        fieldLabel("OIDC Issuer URL",NSRect(x:14,y:118,width:fieldWidth,height:14))
                        add(makeEditableField("settings_hub_issuer",frame:NSRect(x:14,y:92,width:fieldWidth,height:24),placeholder:"Auto-discovered",value:hubForm.issuer,accessibilityLabel:"OIDC issuer URL"))
                        let labelFrame = NSRect(x:14,y:70,width:0,height:14)
                        let fieldFrame = NSRect(x:14,y:44,width:0,height:24)
                        let clientLabel = fieldLabel("OIDC Client ID",labelFrame,sizing:[],id:"settings_hub_client_id_label")
                        let portLabel = fieldLabel("Redirect Port",labelFrame,sizing:[],id:"settings_hub_redirect_port_label")
                        let client = makeEditableField("settings_hub_client_id",frame:fieldFrame,placeholder:"Auto-discovered",value:hubForm.clientId,accessibilityLabel:"OIDC client ID")
                        let port = makeEditableField("settings_hub_redirect_port",frame:fieldFrame,placeholder:"Auto-discovered",value:hubForm.redirectPort,accessibilityLabel:"OIDC redirect port")
                        add(client, sizing: []); add(port, sizing: [])
                        form.equalColumns = (left: [clientLabel, client], right: [portLabel, port])
                    } else {
                        let hasIssuer = hubForm.isEditing && !hubForm.issuer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        let issuer = hubForm.issuer.replacingOccurrences(of: "^https?://", with: "", options: .regularExpression)
                        let title = hasIssuer ? "OIDC: \(issuer) · Auto-discovered" : "Sign-in settings (issuer, client ID, port) are auto-discovered from the Hub URL."
                        let summary = textLabel(title, NSRect(x:14,y:92,width:fieldWidth,height:15), size:10.5, color:CitoPalette.secondaryText)
                        summary.identifier = NSUserInterfaceItemIdentifier("hub_oidc_discovery_summary")
                        summary.maximumNumberOfLines = 1
                        summary.lineBreakMode = .byTruncatingMiddle
                        summary.toolTip = hasIssuer ? "Issuer: \(hubForm.issuer)\nClient ID: \(hubForm.clientId)\nRedirect port: \(hubForm.redirectPort)" : title
                        add(summary)
                    }
                    let title = oidcExpanded ? "▾ Hide manual OIDC settings" :
                        hubForm.isEditing ? "▸ Edit OIDC settings manually…" : "▸ Configure OIDC settings manually…"
                    let manual = MenuControl(title, frame: NSRect(x:14,y:oidcExpanded ? 148 : 68,width:MenuControl.intrinsicWidth(title,paddingX:4),height:22), paddingX:0)
                    manual.identifier = NSUserInterfaceItemIdentifier("manual_hub_oidc")
                    manual.style = .subtleHeader
                    manual.label.alignment = .left
                    manual.layoutContents()
                    manual.pressed = { [weak self] in
                        guard let self else { return }
                        self.hubForm.oidcManualExpanded.toggle()
                        self.rebuildSettingsPanel(revealAddHub: true)
                    }
                    form.addSubview(manual)
                }
            }

            if !editingBuiltinHub {
                let captionHeight: CGFloat = oidcExpanded ? 32 : 48
                for control in form.subviews { control.frame.origin.y += captionHeight }
                let visibility = textLabel("Personal Hubs connect your Macs automatically; Team Sign-In (OIDC) Hubs start private to your account (@user) until shared with Team.", NSRect(x: 14, y: oidcExpanded ? 40 : 60, width: fieldWidth, height: captionHeight), size: 11, color: CitoPalette.secondaryText)
                visibility.identifier = NSUserInterfaceItemIdentifier("hub_visibility_caption")
                visibility.maximumNumberOfLines = 3
                visibility.cell?.wraps = true
                add(visibility)
            }
            // Keep the error between the fields/caption and the fixed action row.
            for control in form.subviews { control.frame.origin.y += errorSpace }
            if let error = errorText {
                let label=textLabel(error,NSRect(x:14,y:40,width:fieldWidth,height:errorHeight),size:10,color:CitoPalette.dangerText)
                label.identifier=NSUserInterfaceItemIdentifier("add_hub_error"); label.toolTip=error
                label.maximumNumberOfLines = 0; label.lineBreakMode = .byWordWrapping; label.cell?.wraps = true
                add(label)
            }
            let cancelBtn = MenuControl("Cancel", frame: NSRect(x: 306, y: 12, width: 72, height: 24), radius: 6)
            cancelBtn.identifier = NSUserInterfaceItemIdentifier("cancel_add_hub")
            cancelBtn.pressed = { [weak self] in
                guard let self = self else { return }
                self.hubForm = HubFormState()
                self.rebuildSettingsPanel()
            }
            form.addSubview(cancelBtn)

            let submitBtn = MenuControl(hubForm.isEditing ? (editingBuiltinHub ? "Save" : "Save Hub") : "Join Hub", frame: NSRect(x: 386, y: 12, width: 80, height: 24), radius: 6)
            submitBtn.identifier = NSUserInterfaceItemIdentifier("submit_add_hub")
            submitBtn.style = .prominent
            pendingLabel(submitBtn,hubForm.pendingSave != nil,hubForm.isEditing ? "Saving…" : "Adding…")
            submitBtn.pressed = { [weak self] in
                guard let self = self else { return }
                self.syncAddHubFieldsFromControls()
                let url = normalizedHubURL(self.hubForm.url)
                let knownHubs = self.settings?.hubs ?? []
                let nameExplicit = !self.hubForm.name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                let resetName = !nameExplicit && self.hubForm.isEditing && !self.editingBuiltinHub
                let name = self.hubNameProposal(self.hubForm.name, url: url, existing: knownHubs,
                    original: self.hubForm.originalName)
                let validation: String?
                if name.isEmpty { validation="Hub name must contain a letter or number." }
                else if !self.hubForm.isEditing && knownHubs.contains(where:{$0.name == name && ($0.is_builtin == true || normalizedHubURL($0.url) != url)}) { validation="Hub name already exists. Choose a different name." }
                else if !["http://","https://","local://"].contains(where:{url.hasPrefix($0)}) { validation="Hub URL must start with http:// or https://." }
                else if !self.hubForm.isEditing && self.hubForm.auth == "token" && self.hubForm.token.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty { validation="Shared access token is required." }
                else if self.hubForm.auth == "oidc" && !self.hubForm.redirectPort.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty && !(Int(self.hubForm.redirectPort.trimmingCharacters(in:.whitespacesAndNewlines)).map { (0...65535).contains($0) } ?? false) {
                    validation="Redirect port must be an integer from 0 to 65535."
                    self.hubForm.oidcManualExpanded = true
                } else { validation=nil }
                if let error=validation { self.hubForm.error=error; self.rebuildSettingsPanel(revealAddHub: true); return }
                var action: [String: Any] = [
                    "op": "setup_save_hub",
                    "name": name,
                    "url": url,
                    "auth_mode": self.hubForm.auth,
                    "set_default": !self.hubForm.isEditing
                ]
                if nameExplicit { action["name_explicit"] = true }
                if resetName { action["reset_name"] = true }
                if !self.hubForm.isEditing && !self.hubForm.authExplicit && self.hubForm.auth == "none" { action.removeValue(forKey:"auth_mode") }
                if self.hubForm.isEditing { action["edit"]=true; action["original_name"]=self.hubForm.originalName; action.removeValue(forKey:"set_default") }
                let tok = self.hubForm.token.trimmingCharacters(in: .whitespacesAndNewlines)
                let usr = self.hubForm.user.trimmingCharacters(in: .whitespacesAndNewlines)
                let iss = self.hubForm.issuer.trimmingCharacters(in: .whitespacesAndNewlines)
                let cid = self.hubForm.clientId.trimmingCharacters(in: .whitespacesAndNewlines)
                if self.hubForm.auth == "token", !tok.isEmpty { action["token"] = tok }
                if self.hubForm.auth == "none" || self.hubForm.auth == "token" { if !usr.isEmpty { action["user"]=usr } else if self.hubForm.isEditing { action["user"]=NSNull() } }
                if self.hubForm.auth == "oidc" {
                    if !iss.isEmpty { action["issuer"] = iss } else if self.hubForm.isEditing { action["issuer"] = NSNull() }
                    if !cid.isEmpty { action["client_id"] = cid } else if self.hubForm.isEditing { action["client_id"] = NSNull() }
                    if let port = Int(self.hubForm.redirectPort.trimmingCharacters(in:.whitespacesAndNewlines)) { action["redirect_port"] = port }
                    else if self.hubForm.isEditing { action["redirect_port"] = NSNull() }
                    // A URL-only edit must discover the destination's metadata,
                    // while deliberate overrides and explicit clears retain priority.
                    if let initial = self.hubForm.initialHub, url != initial.url || !knownHubs.contains(where: { $0.formProfile == initial }) {
                        if iss == initial.issuer.trimmingCharacters(in: .whitespacesAndNewlines) { action.removeValue(forKey:"issuer") }
                        if cid == initial.clientId.trimmingCharacters(in: .whitespacesAndNewlines) { action.removeValue(forKey:"client_id") }
                        if self.hubForm.redirectPort.trimmingCharacters(in: .whitespacesAndNewlines) == initial.redirectPort.trimmingCharacters(in: .whitespacesAndNewlines) { action.removeValue(forKey:"redirect_port") }
                    }
                }
                var submittedName = name
                if case .addWithIncompleteInventory = self.hubForm.mode, !nameExplicit {
                    submittedName = ""
                    action.removeValue(forKey: "name")
                }
                let profile = HubFormProfile(name: submittedName, url: url, issuer: self.hubForm.issuer,
                    clientId: self.hubForm.clientId, redirectPort: self.hubForm.redirectPort)
                let pending = PendingHubSave(profile: profile, requestId: UUID().uuidString, generation: self.hubForm.generation, nameExplicit: nameExplicit)
                self.hubForm.pendingSave=pending; self.hubForm.error=nil
                action["request_id"]=pending.requestId
                self.emit(action)
                self.rebuildSettingsPanel(revealAddHub: self.hubForm.error != nil)
            }
            form.addSubview(submitBtn)
            layoutBar(in: form, insetRight: 14, y: 10, height: 24, trailing: [cancelBtn,submitBtn])
            hubsView.addSubview(form)
        } else {
            let resolvedHubs: [HubConfigItem] = settings?.hubs ?? hubs.map { h in
                HubConfigItem(name: h.name, url: "",
                    auth_mode: h.auth_mode, enabled: h.enabled != false, is_default: h.is_default == true, status: h.status, user: h.user, error: h.error, issuer: nil, client_id: nil, is_builtin:h.is_builtin, supports_workspace_scope:h.supports_workspace_scope, can_login:h.can_login)
            }
            var rowTop = 180 + hubOffset
            let canReorder = resolvedHubs.count > 1
            for (index, hub) in resolvedHubs.enumerated() {
                let local=hub.is_builtin == true
                let needsSignIn = hub.can_login == true
                let authTitle = ["none":"Trusted Network","token":"Shared Token","oidc":"Team Sign-In"][hub.auth_mode] ?? "Network"
                let endpoint = hub.is_builtin == true ? "On this Mac" : hub.url.replacingOccurrences(of:"^https?://",with:"",options:.regularExpression)
                let detail = hub.error.map(presentationError) ?? (hub.status == "connected" ? "" : (["auth_required":"Sign-in required","disconnected":"Offline","connecting":"Connecting…"][hub.status] ?? "Unavailable"))
                let subtitle=local ? "Built-in on this Mac · Signed in as @\(hub.user ?? "local")" : "\(endpoint) · \(authTitle)" + (hub.user.map { " · @\($0)" } ?? "") + (detail.isEmpty ? "" : " · \(detail)")
                let row = roundedView(NSRect(x:16,y:0,width:cardWidth,height:0),radius:9,color:.labelColor.withAppearanceAlpha(0.05),border:.labelColor.withAppearanceAlpha(0.10))
                row.autoresizingMask = [.width]
                let hubName = hub.name
                var orderActions: [NSView] = []
                for (direction, symbol, title, available) in [("up", "arrow.up", "Move Up", index > 0),
                    ("down", "arrow.down", "Move Down", index < resolvedHubs.count - 1)] where canReorder {
                    let button = MenuControl(title,frame:NSRect(x:0,y:0,width:22,height:22),symbol:symbol)
                    button.identifier = NSUserInterfaceItemIdentifier("move_hub_\(direction):\(hubName)")
                    button.isEnabled = available && !isPending("setup_reorder_hub",hub:hubName)
                    button.pressed = { [weak self] in
                        guard let self, !self.isPending("setup_reorder_hub",hub:hubName) else { return }
                        self.emit(["op":"setup_reorder_hub","hub":hubName,"direction":direction])
                    }
                    row.addSubview(button); orderActions.append(button)
                }
                if hub.enabled && !hub.is_default {
                    let button = MenuControl("Set Default",frame:NSRect(x:0,y:0,width:0,height:22),paddingX:6)
                    button.identifier = NSUserInterfaceItemIdentifier("set_default_hub:\(hubName)")
                    pendingLabel(button,isPending("setup_set_default_hub",hub:hubName),"Applying…")
                    button.pressed = { [weak self] in
                        guard let self, !self.isPending("setup_set_default_hub",hub:hubName) else { return }
                        self.emit(["op":"setup_set_default_hub","hub":hubName])
                    }
                    row.addSubview(button); orderActions.append(button)
                }
                layoutBar(in:row,y:7,height:22,trailing:orderActions)
                let subtitleWidth = (orderActions.first.map { $0.frame.minX - CitoMetrics.gap } ?? cardWidth - 14) - 28
                let subtitleHeight = max(14, measureWrappedHeight(subtitle, width: subtitleWidth - allowance, font: .systemFont(ofSize: 10.5)) + 2)
                let lowerRowHeight = max(subtitleHeight, orderActions.isEmpty ? 0 : 22)
                let rowHeight = lowerRowHeight + 42
                rowTop -= rowHeight
                row.frame = NSRect(x:16,y:rowTop,width:cardWidth,height:rowHeight)
                rowTop -= 6
                for action in orderActions { action.frame.origin.y = 7 + (lowerRowHeight - action.frame.height) / 2 }
                row.identifier = NSUserInterfaceItemIdentifier("settings_hub:\(hub.name)")
                row.addSubview(roundedView(NSRect(x:CitoMetrics.cardInset,y:rowHeight-24,width:8,height:8),radius:4,color:statusColor(hub.enabled ? hub.status : "disconnected")))
                let nameLabel = textLabel(hub.name,NSRect(x:28,y:rowHeight-24,width:0,height:16),size:12.5,weight:.semibold)
                nameLabel.identifier = NSUserInterfaceItemIdentifier("settings_hub_name:\(hub.name)")
                nameLabel.lineBreakMode = .byTruncatingMiddle; nameLabel.toolTip = hub.name
                row.addSubview(nameLabel)
                let subtitleLabel = textLabel(subtitle,NSRect(x:28,y:7+(lowerRowHeight-subtitleHeight)/2,width:subtitleWidth,height:subtitleHeight),size:10.5,color:CitoPalette.secondaryText)
                subtitleLabel.maximumNumberOfLines=0; subtitleLabel.lineBreakMode = .byWordWrapping
                subtitleLabel.toolTip = (local ? "Built-in on this Mac" : presentationError(hub.url)) + (hub.error.map { "\n" + presentationError($0) } ?? "")
                subtitleLabel.autoresizingMask = [.width]
                row.addSubview(subtitleLabel)

                let edit=MenuControl("Edit",frame:NSRect(x:0,y:rowHeight-25,width:52,height:22),radius:6)
                edit.identifier=NSUserInterfaceItemIdentifier("edit_hub:\(hub.name)")
                edit.style = .neutral
                edit.pressed={ [weak self] in
                    guard let self=self else{return}
                    self.hubForm = HubFormState(editing: hub)
                    self.rebuildSettingsPanel(revealAddHub: true, syncFields: false)
                    if let field = self.hubFormFocusTarget() { self.settingsPanel?.makeFirstResponder(field) }
                }
                row.addSubview(edit)
                if needsSignIn {
                    let loginBtn = MenuControl("Sign In…", frame: NSRect(x: 184, y: rowHeight-25, width: 70, height: 22), radius: 6)
                    loginBtn.identifier = NSUserInterfaceItemIdentifier("login_hub:\(hub.name)")
                    pendingLabel(loginBtn,isPending("hub_action",hub:hub.name),"Signing In…")
                    let hubName = hub.name
                    loginBtn.pressed = { [weak self] in
                        guard let self = self, !self.isPending("hub_action", hub: hubName) else { return }
                        self.emit(["op": "hub_action", "hub": hubName, "action": "login"])
                    }
                    row.addSubview(loginBtn)
                }

                let toggleBtn = MenuControl(hub.enabled ? "Disable" : "Enable", frame: NSRect(x: 0, y: rowHeight-25, width: 72, height: 22), radius: 6)
                toggleBtn.identifier = NSUserInterfaceItemIdentifier("toggle_hub:\(hub.name)")
                toggleBtn.style = hub.enabled ? .neutral : .prominent
                pendingLabel(toggleBtn,isPending("setup_toggle_hub",hub:hub.name),"Applying…")
                let nextEnabled = !hub.enabled
                toggleBtn.pressed = { [weak self] in
                    guard let self = self, !self.isPending("setup_toggle_hub", hub: hubName) else { return }
                    self.emit(["op": "setup_toggle_hub", "hub": hubName, "enabled": nextEnabled])
                }
                toggleBtn.isHidden = resolvedHubs.filter { $0.enabled }.count <= 1 && hub.enabled
                if !toggleBtn.isHidden { row.addSubview(toggleBtn) }

                if hub.is_builtin != true {
                    let removeBtn = MenuControl("Remove", frame: NSRect(x: 396, y: rowHeight-25, width: 72, height: 22), radius: 6)
                    removeBtn.identifier = NSUserInterfaceItemIdentifier("remove_hub:\(hub.name)")
                    removeBtn.style = .destructiveHover
                    pendingLabel(removeBtn,isPending("setup_remove_hub",hub:hub.name),"Removing…")
                    removeBtn.pressed = { [weak self] in
                        guard let self = self, !self.isPending("setup_remove_hub", hub: hubName) else { return }
                        self.emit(["op": "setup_remove_hub", "hub": hubName])
                    }
                    row.addSubview(removeBtn)
                }

                let signIn = row.subviews.filter { $0.identifier?.rawValue == "login_hub:\(hub.name)" }
                let remove = row.subviews.filter { $0.identifier?.rawValue == "remove_hub:\(hub.name)" }
                let actions = signIn + [edit,toggleBtn] + remove
                let inset: CGFloat = 26
                let spacing = CitoMetrics.gap
                let y = rowHeight-31
                layoutBar(in:row,y:y,height:22,trailing:actions)
                let right = actions.filter { !$0.isHidden }.map { $0.frame.minX - spacing }.min() ?? row.bounds.maxX - CitoMetrics.cardInset
                let badge = hub.is_default ? makePillBadge("Default",id:"default_hub:\(hub.name)",height:18,textColor:CitoPalette.infoText,fill:.labelColor.withAppearanceAlpha(0.06),paddingX:6,minWidth:0) : nil
                let room = right - inset - allowance - (badge.map { $0.frame.width + spacing } ?? 0)
                nameLabel.frame = NSRect(x:inset,y:y+3,width:max(0,min(room,measureTextWidth(hub.name,font:.systemFont(ofSize:12.5,weight:.semibold))+4)),height:16)
                if let badge {
                    badge.frame.origin = NSPoint(x:nameLabel.frame.maxX+spacing,y:y+2)
                    row.addSubview(badge)
                }
                hubsView.addSubview(row)
            }
        }
        mountSettingsCards(hubsView, on: scroll)

    }

    @objc private func toggleLaunchAtLogin(_ sender: NSButton) {
        let enabled = sender.state == .on
        // The settings receipt, not the local click, owns the committed value.
        sender.state = settings?.launch_at_login == true ? .on : .off
        emit(["op": "set_launch_at_login", "enabled": enabled])
    }

    private func presentUninstallAlert() {
        guard let window = settingsPanel, activeUninstallAlert == nil else { return }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "Disconnect Agent Apps?"
        alert.informativeText = "This disconnects all Agent Apps from Cito. Local data and Hub settings are kept unless you choose to delete them."
        let cancel = alert.addButton(withTitle: "Cancel")
        cancel.identifier = NSUserInterfaceItemIdentifier("cancel_uninstall_all")
        cancel.keyEquivalent = "\u{1b}"
        let confirm = alert.addButton(withTitle: "Disconnect Apps")
        confirm.identifier = NSUserInterfaceItemIdentifier("confirm_disconnect_all")
        confirm.keyEquivalent = ""
        confirm.hasDestructiveAction = true
        confirm.toolTip = "Disconnect all Agent Apps and keep Cito running"
        alert.showsSuppressionButton = true
        if let purge = alert.suppressionButton {
            purge.identifier = NSUserInterfaceItemIdentifier("uninstall_purge_data")
            purge.title = "Also delete local data and Hub settings"
            purge.state = .off
            purge.target = self
            purge.action = #selector(updateUninstallPurge(_:))
        }
        guard window.makeFirstResponder(nil) else { return }
        activeUninstallAlert = alert
        if headless {
            alert.window.appearance = testAppearance
            alert.layout()
            alert.window.contentView?.identifier = NSUserInterfaceItemIdentifier("uninstall_alert_sheet")
            for button in alert.buttons {
                button.target = self
                button.action = #selector(pressOffscreenAlertButton(_:))
            }
        } else {
            alert.beginSheetModal(for: window) { [weak self, weak alert] response in
                if let alert { self?.finishUninstallAlert(alert, confirmed: response == .alertSecondButtonReturn) }
            }
        }
    }

    @objc private func updateUninstallPurge(_ sender: NSButton) {
        guard let alert = activeUninstallAlert, sender === alert.suppressionButton else { return }
        let purge = sender.state == .on
        let confirm = alert.buttons[1]
        confirm.title = purge ? "Disconnect & Delete Data" : "Disconnect Apps"
        confirm.toolTip = purge ? "Disconnect all Agent Apps and delete local data and Hub settings" : "Disconnect all Agent Apps and keep Cito running"
        alert.layout()
    }

    @objc private func pressOffscreenAlertButton(_ sender: NSButton) {
        guard let alert = activeUninstallAlert else { return }
        finishUninstallAlert(alert, confirmed: sender === alert.buttons[1])
    }

    private func finishUninstallAlert(_ alert: NSAlert, confirmed: Bool) {
        guard activeUninstallAlert === alert else { return }
        activeUninstallAlert = nil
        if confirmed {
            emit(["op": "setup_disconnect_all", "purge_data": alert.suppressionButton?.state == .on])
        }
    }

    func hubFormFocusTarget() -> NSTextField? {
        guard settingsActiveTab == "hubs", hubForm.isOpen else { return nil }
        return settingsFieldControls[editingBuiltinHub ? "settings_hub_user" : "settings_hub_url"]
    }

    func openSettingsPanel(tab: String? = nil, channelHub: String? = nil) {
        ensureSettingsPanel()
        let previousTab = settingsActiveTab
        let previousHub = effectiveChannelHub
        if let channelHub { selectedChannelHub = channelHub }
        settingsActiveTab = tab ?? previousTab
        if !settingsVisible || settingsActiveTab != previousTab || effectiveChannelHub != previousHub {
            settingsVisible = true
            rebuildSettingsPanel()
        }
        emit(["op": "open_settings", "hub": effectiveChannelHub])
        if !headless, let window = settingsPanel {
            presentPanel(window)
            if window.isVisible, let markerPath = pendingOnboardingMarker {
                try? FileManager.default.createDirectory(atPath: (markerPath as NSString).deletingLastPathComponent, withIntermediateDirectories: true)
                try? "1\n".write(toFile: markerPath, atomically: true, encoding: .utf8)
                pendingOnboardingMarker = nil
            }
        }
    }

    func closeSettingsPanel() {
        MenuControl.clearHoverWhere { $0.window === self.settingsPanel }
        settingsVisible = false
        subscribedChannelHub = ""
        settingsBannerError = nil; hostPort.error = nil
        if let alert = activeUninstallAlert {
            finishUninstallAlert(alert, confirmed: false)
            if !headless, settingsPanel?.attachedSheet === alert.window {
                settingsPanel?.endSheet(alert.window, returnCode: .abort)
            }
        }
        settingsDirty = true
        settingsPanel?.orderOut(nil)
        restoreAccessoryPolicyIfNoWindows()
        emit(["op": "close_settings"])
    }

    var hubNamePlaceholder: String {
        if case .addWithIncompleteInventory = hubForm.mode { return "Assigned automatically" }
        if !hubForm.isEditing && hubForm.url.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return "e.g. studio or work" }
        return "Default: " + hubNameProposal("", url: hubForm.url, existing: settings?.hubs ?? [], original: hubForm.originalName)
    }
}
