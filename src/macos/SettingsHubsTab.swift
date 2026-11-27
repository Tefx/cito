import AppKit

private final class SettingsHubForm: SurfaceView {
    var equalColumns: (left: [NSView], right: [NSView])? { didSet { layoutEqualColumns() } }

    override func resizeSubviews(withOldSize oldSize: NSSize) {
        super.resizeSubviews(withOldSize: oldSize)
        if bounds.width != oldSize.width { layoutEqualColumns() }
    }

    func setError(_ message: String?, frame: NSRect) {
        let id = NSUserInterfaceItemIdentifier("add_hub_error")
        let existing = subviews.first { $0.identifier == id } as? NSTextField
        guard let message else { existing?.removeFromSuperview(); return }
        let label = existing ?? textLabel(message, frame, size: 10, color: CitoPalette.dangerText)
        label.stringValue = message
        label.toolTip = message
        if existing == nil {
            label.identifier = id
            label.autoresizingMask = [.width]
            label.maximumNumberOfLines = 0
            label.lineBreakMode = .byWordWrapping
            label.cell?.wraps = true
            addSubview(label)
        }
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

extension Companion {
    var resolvedSettingsHubs: [HubConfigItem] {
        if let settings {
            return settings.hubs.map { hub in
                // Settings rows and edit actions do not consume Inbox state.
                HubConfigItem(name: hub.name, url: hub.url, auth_mode: hub.auth_mode, enabled: hub.enabled,
                    is_default: hub.is_default, status: hub.status, user: hub.user, error: hub.error, issuer: hub.issuer,
                    client_id: hub.client_id, is_builtin: hub.is_builtin, can_login: hub.can_login, redirect_port: hub.redirect_port)
            }
        }
        return hubs.map { hub in
            HubConfigItem(name: hub.name, url: "", auth_mode: hub.auth_mode, enabled: hub.enabled != false,
                is_default: hub.is_default == true, status: hub.status, user: hub.user, error: hub.error,
                issuer: nil, client_id: nil, is_builtin: hub.is_builtin, can_login: hub.can_login)
        }
    }

    func reconcileHubSettings(_ value: SettingsPayload, savedRequest: String?, confirmHubSave: Bool, savedHub: String?) {
        if let submission = hostPort.submission, savedRequest == submission.requestId {
            hostPort.submission = nil
            let draftPort = hostPort.draft.flatMap { Int($0.trimmingCharacters(in:.whitespacesAndNewlines)) }
            let saved = value.host_hub.port == submission.port && value.host_hub.error == nil &&
                (hostPort.draft == nil || draftPort == submission.port)
            hostPort.savedUntil = saved ? Date().addingTimeInterval(1.2) : nil
            hostPort.error = nil
            if saved { hostPort.draft = nil; rebuildFeedbackAfter(1.2, tab: .hubs) }
        }
        if confirmHubSave, let pending = hubForm.pendingSave, savedRequest == pending.requestId {
            let profile = pending.profile
            let candidates = value.hubs.filter { normalizedHubURL($0.url) == profile.url }
            let saved: HubConfigItem?
            if let savedHub { saved = candidates.first { $0.name == savedHub } }
            else if pending.nameExplicit || hubForm.isEditing { saved = candidates.first { $0.name == profile.name } }
            else { saved = candidates.count == 1 && candidates[0].is_builtin != true ? candidates[0] : nil }
            hubForm.error = nil
            finishHubSave(pending, savedName: savedHub ?? saved?.name)
        }
        if hubForm.pendingSave == nil, let initial = hubForm.initialHub,
           let saved = value.hubs.first(where: { $0.name == initial.name && normalizedHubURL($0.url) == initial.url })?.formProfile, saved != initial {
            // A passive canonical refresh also completes a payload-free receipt.
            // Compare each field with the submitted draft to preserve newer edits.
            if hubForm.issuer == initial.issuer, initial.issuer != saved.issuer {
                hubForm.issuer = saved.issuer
                updateUnfocusedHubField("settings_hub_issuer", saved.issuer)
            }
            if hubForm.clientId == initial.clientId, initial.clientId != saved.clientId {
                hubForm.clientId = saved.clientId
                updateUnfocusedHubField("settings_hub_client_id", saved.clientId)
            }
            if hubForm.redirectPort == initial.redirectPort, initial.redirectPort != saved.redirectPort {
                hubForm.redirectPort = saved.redirectPort
                updateUnfocusedHubField("settings_hub_redirect_port", saved.redirectPort)
            }
            hubForm.mode = .edit(saved)
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
                    updateUnfocusedHubField("settings_hub_name", profile.name)
                }
                hubForm.mode = .edit(profile)
            } else if savedName == nil {
                // The daemon can still allocate an automatic name against its full inventory.
                hubForm.mode = .addWithIncompleteInventory
            }
        }
    }

    private func updateUnfocusedHubField(_ id: String, _ value: String) {
        if let field = settingsFieldControls[id], field.currentEditor() == nil { field.stringValue = value }
    }

    func refreshMountedHubFormFeedback() {
        guard hubForm.isOpen, let root = settingsPanel?.contentView,
              let submit: MenuControl = findView(in: root, id: "submit_add_hub") else { return }
        if hubForm.pendingSave != nil {
            pendingLabel(submit, true, hubForm.isEditing ? "Saving…" : "Adding…")
        } else {
            let title = hubForm.isEditing ? (editingBuiltinHub ? "Save" : "Save Hub") : "Join Hub"
            submit.clearFeedback(title: title, accessibilityLabel: title)
            submit.isEnabled = true
            submit.toolTip = title
        }
        submit.layoutContents()
        settingsFieldControls["settings_hub_name"]?.placeholderString = hubNamePlaceholder
        if let form = submit.superview as? SettingsHubForm,
           let cancel: MenuControl = findView(in: form, id: "cancel_add_hub") {
            // Use the space beside the actions until the full layout can expand
            // without moving the clip view or detaching the field editor.
            form.setError(hubForm.error, frame: NSRect(x: 14, y: 9, width: cancel.frame.minX - 20, height: 26))
        }
    }

    func refreshMountedHostPortFeedback() {
        guard let root = settingsPanel?.contentView,
              let apply: MenuControl = findView(in: root, id: "apply_host_hub_port"),
              let card = apply.superview, let toggle: MenuControl = findView(in: card, id: "toggle_host_hub") else { return }
        let title = hostPort.pending ? "Applying…" : "Apply"
        apply.clearFeedback(title: title, accessibilityLabel: title)
        apply.toolTip = title
        let port = settings?.host_hub.port ?? 4291
        apply.isEnabled = !hostPort.pending && Int((hostPort.draft ?? "\(port)").trimmingCharacters(in: .whitespacesAndNewlines)) != port
        apply.layoutContents()

        let error = (hostPort.error ?? settings?.host_hub.error).flatMap { $0.isEmpty ? nil : $0 }
        let notice = error ?? (settings?.host_hub.enabled == true ? nil : "Choose Start Sharing to accept connections")
        let existing: NSTextField? = findView(in: card, id: "host_hub_error")
        guard let notice else { existing?.isHidden = true; return }
        let label = existing ?? textLabel(notice, NSRect(x: 14, y: toggle.frame.minY - 14, width: card.bounds.width - 28, height: 14), size: 11)
        label.stringValue = notice
        label.textColor = error == nil ? CitoPalette.secondaryText : CitoPalette.dangerText
        label.toolTip = notice
        label.isHidden = false
        if existing == nil {
            label.identifier = NSUserInterfaceItemIdentifier("host_hub_error")
            label.lineBreakMode = .byTruncatingTail
            label.autoresizingMask = [.width]
            card.addSubview(label)
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
        guard hubInvite.requestId == nil else { return }
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
        settingsBannerError=nil; hubInvite.joinedUntil = .distantPast
        let requestId = UUID().uuidString
        hubInvite.requestId = requestId
        emit(["op":"paste_hub_invite","invite":raw,"request_id":requestId])
        rebuildSettingsPanel()
    }

    func buildHubsTab(in scroll: NSScrollView) {
        let cardWidth = scroll.contentSize.width - 32
        let fieldWidth = cardWidth - 28
        let allowance = settingsScrollerAllowance(scroll)
        let hubsView = NSView(frame: NSRect(x: 0, y: 0, width: scroll.contentSize.width, height: 368))
        hubsView.identifier = NSUserInterfaceItemIdentifier(SettingsTab.hubs.scrollId)
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
        let noticeHeight = hostNotice.map {
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
        if hostExpanded, let notice = hostNotice {
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
        hostStatus.toolTip=hubError ?? (hostHub.running ? "Sharing is running" : hostHub.enabled ? "Sharing is starting" : "Sharing is stopped")
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
            self.hostPort.error = nil; self.hostPort.savedUntil = nil
            self.hostPort.submission = HostHubSubmission(requestId: requestId, port: port)
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
        if Date() < hubInvite.joinedUntil { pasteBtn.copied("✓ Joined", for: max(0.01, hubInvite.joinedUntil.timeIntervalSinceNow)) }
        pendingLabel(pasteBtn,hubInvite.requestId != nil,"Joining…")
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
            let errorText = hubForm.error
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
            form.setError(errorText, frame: NSRect(x: 14, y: 40, width: fieldWidth, height: errorHeight))
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
                // Submission ends the current edit. A later focus belongs to a new editing session.
                self.settingsPanel?.makeFirstResponder(nil)
                self.emit(action)
                self.rebuildSettingsPanel(revealAddHub: self.hubForm.error != nil)
            }
            form.addSubview(submitBtn)
            layoutBar(in: form, insetRight: 14, y: 10, height: 24, trailing: [cancelBtn,submitBtn])
            hubsView.addSubview(form)
        } else {
            let resolvedHubs = resolvedSettingsHubs
            var rowTop = 180 + hubOffset
            let canReorder = resolvedHubs.count > 1
            for (index, hub) in resolvedHubs.enumerated() {
                let local=hub.is_builtin == true
                let needsSignIn = hub.can_login == true
                let authTitle = ["none":"Trusted Network","token":"Shared Token","oidc":"Team Sign-In"][hub.auth_mode] ?? "Network"
                let endpoint = hub.is_builtin == true ? "On this Mac" : hub.url.replacingOccurrences(of:"^https?://",with:"",options:.regularExpression)
                let detail = hub.error ?? (hub.status == "connected" ? "" : (["auth_required":"Sign-in required","disconnected":"Offline","connecting":"Connecting…"][hub.status] ?? "Unavailable"))
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
                subtitleLabel.toolTip = (local ? "Built-in on this Mac" : hub.url) + (hub.error.map { "\n" + $0 } ?? "")
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
    func hubFormFocusTarget() -> NSTextField? {
        guard settingsActiveTab == "hubs", hubForm.isOpen else { return nil }
        return settingsFieldControls[editingBuiltinHub ? "settings_hub_user" : "settings_hub_url"]
    }

    var hubNamePlaceholder: String {
        if case .addWithIncompleteInventory = hubForm.mode { return "Assigned automatically" }
        if !hubForm.isEditing && hubForm.url.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return "e.g. studio or work" }
        return "Default: " + hubNameProposal("", url: hubForm.url, existing: settings?.hubs ?? [], original: hubForm.originalName)
    }
}
