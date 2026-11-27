import AppKit

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
        selfUI.settingsChannels.editors[key, default: ChannelUiState()].drafts[schema.key] = field.stringValue
        selfUI.settingsChannels.editors[key]?.savedUntil.removeValue(forKey: schema.scope)
        refreshSaveControl(channel: channel.id, scope: schema.scope)
        if let test: MenuControl = selfUI.findView(in: self, id: "test_channel:\(channel.id)") {
            let visibleFields = channel.fields.filter { $0.hidden != true && $0.scope != "hub" && selfUI.settingsFieldControls["channel_field:\(channel.id):\($0.key)"] != nil }
            test.isEnabled = !selfUI.isPending("channel_test", hub: hub, target: channel.id) && channel.canSendTest(drafts: selfUI.settingsChannels.editors[key]?.drafts ?? [:], visibleFields: visibleFields)
        }
    }

    func refreshSaveControl(channel: String, scope: String) {
        let gateway = scope == "hub"
        let id = (gateway ? "save_hub_service:" : "save_channel:") + channel
        guard let companion, let save: MenuControl = companion.findView(in: self, id: id) else { return }
        if companion.channelSavePending(gateway ? "save_hub_service" : "channel_save", hub: hub, channel: channel, scope: scope) {
            companion.pendingLabel(save, true, "Saving…")
        } else {
            let title = gateway ? "Save Gateway Token" : "Save"
            save.clearFeedback(title: title, accessibilityLabel: title)
            save.isEnabled = true
            save.toolTip = title
        }
        save.layoutContents()
    }

    func controlTextDidEndEditing(_ notification: Notification) {
        companion?.controlTextDidEndEditing(notification)
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        companion?.control(control, textView: textView, doCommandBy: selector) ?? false
    }
}

struct ChannelsPresentation: Equatable {
    let hub: String
    let enabledHubs: [String]
    let inventory: [ChannelSetupItem]
    let error: String?
    let editors: [String: ChannelUiState]
    let user: String
    let paused: Bool
    let attachments: [ChannelAttachment]
    let current: Bool
    let scope: String
    let revision: Int
    let scopeListener: String?
}

extension Companion {
    func hubMatchesMountedChannels(_ action: [String: Any]) -> Bool {
        (action["hub"] as? String ?? effectiveChannelHub) == effectiveChannelHub
    }

    func channelsPresentation() -> ChannelsPresentation {
        let hub = effectiveChannelHub
        let config = settings?.hubs.first { $0.name == hub }
        let runtime = config == nil ? hubs.first { $0.name == hub } : nil
        let inventory = settingsChannels.inventory[hub] ?? []
        let scopeEnabled = (config?.supports_workspace_scope ?? runtime?.supports_workspace_scope) == true &&
            (config?.human_can_set_scope ?? runtime?.human_can_set_scope) == true
        return ChannelsPresentation(hub: hub, enabledHubs: enabledChannelHubs, inventory: inventory,
            error: settingsChannels.errors[hub], editors: settingsChannels.editors.filter { $0.key.hasPrefix(hub + "|") },
            user: config?.user ?? runtime?.user ?? "local", paused: config?.human_paused ?? runtime?.human_paused ?? false,
            attachments: config?.attachments ?? runtime?.attachments ?? [],
            current: (config?.status ?? runtime?.status) == "connected" && (config?.human_state_status ?? runtime?.human_state_status) == "current",
            scope: config?.human_scope ?? runtime?.human_scope ?? "self", revision: config?.human_control_revision ?? runtime?.human_control_revision ?? 1,
            scopeListener: scopeEnabled ? config?.human_listener_id ?? runtime?.human_listener_id : nil)
    }

    func reconcileChannelSettings(_ value: SettingsPayload, target: String, channelSave: PendingControl?, sourceOp: String?, truncated: Bool) {
        let previousChannels = settingsChannels.inventory[target]
        settingsChannels.errors[target] = value.channel_inventory_error
        guard !(truncated && previousChannels != nil), let channels = value.channels else { return }
        settingsChannels.inventory[target] = channels
        for channel in channels {
            let key=target + "|" + channel.id
            var ui = settingsChannels.editors[key] ?? ChannelUiState()
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
                    rebuildFeedbackAfter(1.5, tab: .channels)
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
            settingsChannels.editors[key] = ui
        }
    }

    func channelSavePending(_ op: String, hub: String, channel: String, scope: String) -> Bool {
        pendingControls.values.contains { pending in
            guard pending.op == op && pending.hub == hub && pending.target == channel,
                  let sent = pending.channelSave, sent.scope == scope else { return false }
            let fields = settingsChannels.inventory[hub]?.first { $0.id == channel }?.fields ?? []
            let drafts = settingsChannels.editors["\(hub)|\(channel)"]?.drafts ?? [:]
            return fields.filter { $0.scope == scope }.allSatisfy { field in
                (drafts[field.key] ?? field.value ?? "") == (sent.values[field.key] ?? field.value ?? "")
            }
        }
    }

    var enabledChannelHubs: [String] {
        settings?.hubs.filter(\.enabled).map(\.name) ?? hubs.filter { $0.enabled != false }.map(\.name)
    }
    var effectiveChannelHub: String {
        let inventory = enabledChannelHubs
        if !settingsChannels.selectedHub.isEmpty { return inventory.contains(settingsChannels.selectedHub) ? settingsChannels.selectedHub : inventory.first ?? "local" }
        return resolvedDefaultHub(requireEnabled: true) ?? inventory.first ?? "local"
    }
    var mountedChannelsSettings: ChannelsSettingsView? {
        settingsPanel?.contentView?.subviews.lazy.compactMap { ($0 as? NSScrollView)?.documentView as? ChannelsSettingsView }.first
    }
    func clearChannelDrafts(_ channel: ChannelSetupItem, scope: String, hub: String) {
        let key = "\(hub)|\(channel.id)"
        for field in channel.fields where field.scope == scope {
            settingsChannels.editors[key, default: ChannelUiState()].drafts.removeValue(forKey: field.key)
        }
    }
    private func updateInbox(paused: Bool? = nil, scope: String? = nil, hub: String, revision: Int) {
        guard !isPending("human_inbox_update",hub:hub,target:"paused"),
              !isPending("human_inbox_update",hub:hub,target:"scope") else { return }
        var action: [String:Any]=["op":"human_inbox_update","hub":hub,"expected_control_revision":revision]
        if let paused=paused { action["paused"]=paused }
        if let scope=scope { action["scope"]=scope }
        emit(action)
    }

    func buildChannelsTab(in scroll: NSScrollView, renderedHub: String) {
        let cardWidth = scroll.contentSize.width - 32
        let stratumWidth = cardWidth - 24
        let stratumTextWidth = stratumWidth - 24
        let allowance = settingsScrollerAllowance(scroll)
        let inbox = channelsPresentation()
        let resolvedChannels = inbox.inventory
        let visibleChannels = resolvedChannels.filter { $0.can_configure_hub == true || $0.hub_ready == true }
        let channelsView = ChannelsSettingsView(hub: renderedHub, channels: resolvedChannels, companion: self)
        channelsView.frame.size.width = scroll.contentSize.width
        channelsView.identifier = NSUserInterfaceItemIdentifier(SettingsTab.channels.scrollId)
        channelsView.isHidden = settingsActiveTab != "channels"
        let inboxHub = renderedHub
        let inboxPaused = inbox.paused
        let inboxUser = inbox.user
        let inboxAttachments = displayAttachments(inbox.attachments)
        let scopeValue = inbox.scope
        let revision = inbox.revision
        let pausePending = isPending("human_inbox_update",hub:inboxHub,target:"paused")
        let scopePending = isPending("human_inbox_update",hub:inboxHub,target:"scope")
        // Both actions share a revision; only the submitted action shows progress.
        let inboxEnabled = inbox.current && !pausePending && !scopePending
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
        if let id = inbox.scopeListener {
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
        let channelHubs = inbox.enabledHubs
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
                self.settingsChannels.selectedHub = name
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

        if let message = inbox.error, !message.isEmpty {
            let height = max(16, measureWrappedHeight(message, width: cardWidth - allowance, font: .systemFont(ofSize: 11)))
            let diagnostic = textLabel(message, NSRect(x:16,y:cursorY-height-4,width:cardWidth,height:height),size:11,color:CitoPalette.dangerText)
            diagnostic.autoresizingMask = [.width]
            diagnostic.identifier = NSUserInterfaceItemIdentifier("channel_inventory_error")
            diagnostic.maximumNumberOfLines = 0; diagnostic.lineBreakMode = .byWordWrapping
            diagnostic.toolTip = message
            channelsView.addSubview(diagnostic)
            cursorY -= height + 12
        }
        if !resolvedChannels.isEmpty && visibleChannels.isEmpty && inbox.error == nil {
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
            let channelUi = inbox.editors[channelKey] ?? ChannelUiState()
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
            let currentUser = inbox.user
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
            let channelResult = ch.warning == true && !channelUi.testResultIsLatest ? nil : channelUi.testResult
            let showDetail = ch.warning == true || channelResult != nil
            let detail = channelResult?.detail ?? ch.detail
            let detailHeight = showDetail ? max(14, measureWrappedHeight(detail, width: cardWidth - 58 - allowance, font: .systemFont(ofSize: 10.5))) : 0
            let headerHeight: CGFloat = showDetail ? 38 + detailHeight : 38
            let cardHeight: CGFloat = headerHeight + hubStratumH + extraTitleH + (showHubStratum && gatewayActive ? CitoMetrics.gap : 0) + userStratumH + 8
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

            let chDetail = textLabel(detail, NSRect(x: 44, y: cardHeight - 32 - detailHeight, width: cardWidth - 58, height: detailHeight), size: 10.5, color:channelResult?.ok == false ? CitoPalette.dangerText : (ch.warning == true ? CitoPalette.warningText : CitoPalette.secondaryText))
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
                    let draft=self.settingsChannels.editors[channelKey]?.drafts[f.key]
                    if f.type == "secret" && f.configured {
                        if let d=draft, !d.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty, d != (f.value ?? "") { vals[f.key]=d }
                    } else {
                        vals[f.key]=draft ?? self.settingsFieldControls[fid]?.stringValue ?? f.value ?? ""
                    }
                }
                return vals
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
                        cancel.pressed={ [weak self] in self?.clearChannelDrafts(ch, scope:"hub", hub:renderedHub); self?.settingsChannels.editors[channelKey]?.resetGateway(); self?.rebuildSettingsPanel() }
                        hubStratum.addSubview(cancel); gatewayTrailing.insert(cancel, at: 0)
                    }
                } else {
                    let edit=MenuControl("Change Token…",frame:NSRect(x:0,y:7,width:MenuControl.intrinsicWidth("Change Token…", font:.systemFont(ofSize:10.5)),height:22))
                    edit.style = .neutral
                    edit.label.font = .systemFont(ofSize:10.5)
                    edit.identifier=NSUserInterfaceItemIdentifier("edit_hub_service:\(ch.id)")
                    edit.pressed={ [weak self] in self?.settingsChannels.editors[channelKey, default: ChannelUiState()].editingGateway = true; self?.rebuildSettingsPanel(revealChannel: ch.id, revealPairing: false) }
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
                manual.pressed={ [weak self] in guard let self=self else{return}; self.settingsChannels.editors[channelKey, default: ChannelUiState()].manualEntry.toggle(); self.rebuildSettingsPanel(revealChannel: ch.id, revealPairing: false, revealEditorScope: "user") }
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
                    self.settingsChannels.editors[channelKey, default: ChannelUiState()].pairCopiedUntil = Date().addingTimeInterval(1.2)
                    self.rebuildFeedbackAfter(1.2, tab: .channels)
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
            testBtn.isHidden = ch.can_test != true || (disclosure && !ch.configured && !manualExpanded)
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
                edit.pressed = { [weak self] in self?.settingsChannels.editors[channelKey, default: ChannelUiState()].editingPersonal = true; self?.settingsChannels.editors[channelKey, default: ChannelUiState()].manualEntry = true; self?.rebuildSettingsPanel(revealChannel: ch.id, revealPairing: false, revealEditorScope: "user") }
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
                    cancel.pressed={ [weak self] in self?.clearChannelDrafts(ch,scope:"user",hub:renderedHub); self?.settingsChannels.editors[channelKey]?.resetPersonal(); self?.rebuildSettingsPanel() }
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

}
