import AppKit

private func channelTestResult(_ channel: ChannelSetupItem, ui: ChannelUiState?) -> (ok: Bool, detail: String)? {
    channel.warning == true && ui?.testResultIsLatest != true ? nil : ui?.testResult
}

private final class InboxScopeItem: NSMenuItem {
    var selected: (() -> Void)?

    @objc func choose(_ sender: NSMenuItem) { selected?() }
}

// The mounted document owns its editor routing until AppKit releases it.
final class ChannelsSettingsView: NSView, NSTextFieldDelegate {
    let hub: String
    let channels: [ChannelSetupItem]
    let principal: String
    var settlementFocus: [String]?
    private weak var companion: Companion?

    init(hub: String, principal: String, channels: [ChannelSetupItem], companion: Companion) {
        self.hub = hub
        self.principal = principal
        self.channels = channels
        self.companion = companion
        super.init(frame: NSRect(x: 0, y: 0, width: 520, height: 368))
    }

    required init?(coder: NSCoder) { nil }

    func documentHeight(for viewportHeight: CGFloat) -> CGFloat {
        let bottomCard = subviews.map { $0.frame.minY }.min() ?? bounds.height
        return max(viewportHeight, bounds.height - bottomCard + 8)
    }

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
        updateDraft(field.stringValue, channel: channel, schema: schema, in: selfUI)
    }

    @objc func toggleBooleanField(_ sender: NSButton) {
        guard let selfUI = companion,
              let id = sender.identifier?.rawValue,
              let (channel, schema) = channelField(id) else { return }
        updateDraft(sender.state == .on ? "true" : "false", channel: channel, schema: schema, in: selfUI)
    }

    private func editorKey(_ id: String, in companion: Companion) -> String {
        companion.channelEditorKey(hub: hub, principal: principal, channel: id)
    }

    private func currentChannel(_ id: String, in companion: Companion) -> ChannelSetupItem? {
        guard principal == companion.channelPrincipal(for: hub) else { return nil }
        return companion.settingsChannels.inventory[hub]?.first { $0.id == id }
    }

    private func updateDraft(_ value: String, channel: ChannelSetupItem, schema: ChannelFieldItem, in companion: Companion) {
        guard currentChannel(channel.id, in: companion) != nil else { return }
        let key = editorKey(channel.id, in: companion)
        let draftKey = companion.channelDraftKey(scope: schema.scope, key: schema.key)
        companion.settingsChannels.editors[key, default: ChannelUiState()].drafts[draftKey] = value
        companion.feedback.expire(companion.channelSaveFeedbackKey(key, scope: schema.scope))
        refreshSaveControl(channel: channel.id, scope: schema.scope)
        refreshTestControl(channel: channel.id, in: companion)
    }

    private func testAllowed(_ channel: ChannelSetupItem, in companion: Companion) -> Bool {
        let fields = channel.fields.filter { field in
            guard field.hidden != true, field.scope != "hub" else { return false }
            let mounted: NSControl? = companion.findView(in: self, id: "channel_field:\(channel.id):\(field.key)")
            return mounted != nil && mounted?.isHidden != true
        }
        let ui = companion.settingsChannels.editors[editorKey(channel.id, in: companion)]
        return (companion.settingsChannels.errors[hub] ?? "").isEmpty && channel.hub_ready == true &&
            channel.canSendTest(drafts: companion.scopedDrafts(for: fields, in: ui), visibleFields: fields)
    }

    private func refreshTestControl(channel id: String, in companion: Companion) {
        guard let test: MenuControl = companion.findView(in: self, id: "test_channel:\(id)") else { return }
        if companion.isPending("channel_test", hub: hub, target: id) {
            test.setPending("Testing…")
        } else {
            test.clearFeedback(title: "Send Test", accessibilityLabel: "Send Test")
            test.toolTip = "Send Test"
            test.isEnabled = currentChannel(id, in: companion).map { testAllowed($0, in: companion) } == true
        }
    }

    func refreshSaveControl(channel id: String, scope: String) {
        let gateway = scope == "hub"
        guard let companion, let save: MenuControl = companion.findView(in: self, id: (gateway ? "save_hub_service:" : "save_channel:") + id) else { return }
        if companion.channelSavePending(gateway ? "save_hub_service" : "channel_save", hub: hub, channel: id, scope: scope) {
            save.setPending("Saving…")
            return
        }
        let title = gateway ? "Save Gateway Token" : "Save"
        let key = companion.channelSaveFeedbackKey(editorKey(id, in: companion), scope: scope)
        if save.feedbackKey != key { save.clearFeedback(title: title, accessibilityLabel: title) }
        save.toolTip = title
        save.isEnabled = currentChannel(id, in: companion).map { saveAllowed($0, scope: scope, in: companion) } == true
        save.showFeedback("✓ Saved", key: key)
    }

    private func saveAllowed(_ channel: ChannelSetupItem, scope: String, in companion: Companion) -> Bool {
        (companion.settingsChannels.errors[hub] ?? "").isEmpty && (scope == "hub" ? channel.can_configure_hub == true :
            channel.hub_ready == true && channel.can_edit_personal == true)
    }

    func refreshMountedActions() {
        guard let companion else { return }
        for channel in channels {
            let id = channel.id
            refreshSaveControl(channel: id, scope: "hub")
            refreshSaveControl(channel: id, scope: "user")
            refreshTestControl(channel: id, in: companion)
            guard let card: NSView = companion.findView(in: self, id: "settings_channel:\(id)"),
                  let field = (window?.firstResponder as? NSTextView)?.delegate as? NSTextField,
                  field.isDescendant(of: card), let current = currentChannel(id, in: companion) else { continue }
            let ui = companion.settingsChannels.editors[editorKey(id, in: companion)]
            let result = channelTestResult(current, ui: ui)
            let error = companion.settingsChannels.errors[hub]
            let detail = error ?? result?.detail ?? (current.warning == true ? current.detail : nil)
            let labelID = "settings_channel_detail:\(id)"
            let existing: NSTextField? = companion.findView(in: card, id: labelID)
            guard let detail, !detail.isEmpty else { existing?.removeFromSuperview(); continue }
            let label = existing ?? textLabel(detail, NSRect(x: 44, y: card.bounds.height - 46, width: card.bounds.width - 58, height: 14), size: 10.5)
            if existing == nil || (label.stringValue != detail &&
                measureWrappedHeight(detail, width: label.bounds.width, font: .systemFont(ofSize: 10.5)) > label.bounds.height) {
                label.maximumNumberOfLines = 1
                label.lineBreakMode = .byTruncatingTail
            }
            label.stringValue = detail
            label.toolTip = detail
            label.textColor = error != nil || result?.ok == false ? CitoPalette.dangerText : (current.warning == true ? CitoPalette.warningText : CitoPalette.secondaryText)
            if existing == nil {
                label.identifier = NSUserInterfaceItemIdentifier(labelID)
                label.autoresizingMask = [.width]
                card.addSubview(label)
            }
        }
    }

    enum Action { case saveGateway, savePersonal, pair, test }

    func submit(_ actionKind: Action, channel id: String) {
        guard let companion, let channel = currentChannel(id, in: companion) else { return }
        let ui = companion.settingsChannels.editors[editorKey(id, in: companion)]
        func values(_ scope: String, drafts: Bool) -> [String: Any] {
            var result: [String: Any] = [:]
            for field in channel.fields where field.hidden != true && field.scope == scope {
                let draft = drafts ? ui?.drafts[companion.channelDraftKey(scope: scope, key: field.key)] : nil
                if field.type == "boolean" {
                    result[field.key] = (draft ?? field.value ?? "").trimmingCharacters(in: .whitespacesAndNewlines).lowercased() == "true"
                } else if field.type == "secret" && field.configured {
                    if let draft, !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, draft != field.value { result[field.key] = draft }
                } else { result[field.key] = draft ?? field.value ?? "" }
            }
            return result
        }
        var action: [String: Any] = ["hub": hub, "channel_id": id]
        switch actionKind {
        case .saveGateway, .savePersonal:
            let scope = actionKind == .saveGateway ? "hub" : "user"
            guard saveAllowed(channel, scope: scope, in: companion) else { return }
            let op = scope == "hub" ? "save_hub_service" : "channel_save"
            guard !companion.channelSavePending(op, hub: hub, channel: id, scope: scope) else { return }
            action["op"] = op
            action["values"] = values(scope, drafts: true)
            if scope == "hub" { action["scope"] = scope }
            else {
                action["enabled"] = true
                if let service = channel.service_identity, !service.isEmpty { action["expected_service_identity"] = service }
            }
        case .pair:
            guard (companion.settingsChannels.errors[hub] ?? "").isEmpty, channel.hub_ready == true, channel.can_pair == true else { return }
            action["op"] = "channel_pair"
            action["values"] = values("user", drafts: true)
        case .test:
            guard testAllowed(channel, in: companion), !companion.isPending("channel_test", hub: hub, target: id) else { return }
            let gateway: MenuControl? = companion.findView(in: self, id: "save_hub_service:\(id)")
            action["op"] = "channel_test"
            action["values"] = values("user", drafts: true).merging(values("hub", drafts: gateway != nil)) { _, saved in saved }
        }
        companion.emit(action)
        if companion.mountedChannelsSettings === self { refreshMountedActions() }
    }

    func settleFocus(channel id: String, scope: String) {
        guard let responder = window?.firstResponder else { return }
        let gateway = scope == "hub"
        let field = (responder as? NSTextView)?.delegate as? NSTextField
        let submittedField = field?.identifier.flatMap { channelField($0.rawValue) }
        let submittedButton = (responder as? NSView)?.identifier?.rawValue == (gateway ? "save_hub_service:" : "save_channel:") + id
        guard submittedButton || (submittedField?.0.id == id && submittedField?.1.scope == scope) else { return }
        settlementFocus = [(gateway ? "edit_hub_service:" : "edit_user_channel:") + id,
                           "test_channel:\(id)", "toggle_channel:\(id)", "settings_tab:channels"]
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
    let feedbackDeadlines: [TransientFeedbackStore.Key: Date]
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

    func channelPrincipal(for hub: String, in payload: SettingsPayload? = nil) -> String {
        let config = (payload ?? settings)?.hubs.first { $0.name == hub }
        let runtime = config == nil ? hubs.first { $0.name == hub } : nil
        return config?.user ?? runtime?.user ?? "local"
    }

    func channelEditorKey(hub: String, principal: String? = nil, channel: String) -> String {
        "\(hub)|\(principal ?? channelPrincipal(for: hub))|\(channel)"
    }

    func channelDraftKey(scope: String, key: String) -> String {
        "\(scope)|\(key)"
    }

    func scopedDrafts(for fields: [ChannelFieldItem], in ui: ChannelUiState?) -> [String: String] {
        guard let ui else { return [:] }
        var result: [String: String] = [:]
        for field in fields {
            if let value = ui.drafts[channelDraftKey(scope: field.scope, key: field.key)] {
                result[field.key] = value
            }
        }
        return result
    }

    private func normalizedChannelFieldValue(_ raw: String?, type: String) -> String {
        let value = raw ?? ""
        guard type == "boolean" else { return value }
        return value.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() == "true" ? "true" : "false"
    }

    func channelsPresentation(hub target: String? = nil) -> ChannelsPresentation {
        let hub = target ?? effectiveChannelHub
        let config = settings?.hubs.first { $0.name == hub }
        let runtime = config == nil ? hubs.first { $0.name == hub } : nil
        let inventory = settingsChannels.inventory[hub] ?? []
        let user = config?.user ?? runtime?.user ?? "local"
        let scopeEnabled = (config?.supports_workspace_scope ?? runtime?.supports_workspace_scope) == true &&
            (config?.human_can_set_scope ?? runtime?.human_can_set_scope) == true
        let editors = settingsChannels.editors.filter { $0.key.hasPrefix("\(hub)|\(user)|") }
        var feedbackDeadlines: [TransientFeedbackStore.Key: Date] = [:]
        for editor in editors.keys {
            for key in [TransientFeedbackStore.Key(.channelPair, editor),
                        channelSaveFeedbackKey(editor, scope: "user"), channelSaveFeedbackKey(editor, scope: "hub")] {
                feedbackDeadlines[key] = feedback.deadline(key)
            }
        }
        return ChannelsPresentation(hub: hub, enabledHubs: enabledChannelHubs, inventory: inventory,
            error: settingsChannels.errors[hub], editors: editors, feedbackDeadlines: feedbackDeadlines,
            user: user, paused: config?.human_paused ?? runtime?.human_paused ?? false,
            attachments: config?.attachments ?? runtime?.attachments ?? [],
            current: (config?.status ?? runtime?.status) == "connected" && (config?.human_state_status ?? runtime?.human_state_status) == "current",
            scope: config?.human_scope ?? runtime?.human_scope ?? "self", revision: config?.human_control_revision ?? runtime?.human_control_revision ?? 1,
            scopeListener: scopeEnabled ? config?.human_listener_id ?? runtime?.human_listener_id : nil)
    }

    func channelSaveFeedbackKey(_ editor: String, scope: String) -> TransientFeedbackStore.Key {
        // A gateway confirmation belongs only to the Save button when its editor survives.
        scope == "hub" ? .init(.control, "gateway-save", authority: editor) : .init(.channelSave, editor)
    }

    func reconcileChannelSettings(_ value: SettingsPayload, target: String, channelSave: PendingControl?, sourceOp: String?) {
        let previousChannels = settingsChannels.inventory[target]
        settingsChannels.errors[target] = value.channel_inventory_error
        guard let channels = value.channels else { return }
        settingsChannels.inventory[target] = channels
        let principal = channelPrincipal(for: target, in: value)
        for channel in channels {
            let key = channelEditorKey(hub: target, principal: principal, channel: channel.id)
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
                    let draft = ui.drafts[channelDraftKey(scope: field.scope, key: field.key)]
                    let current = normalizedChannelFieldValue(draft ?? field.value, type: field.type)
                    if let sent = entry.values[field.key] {
                        return current == normalizedChannelFieldValue(sent, type: field.type)
                    }
                    return draft == nil || current == normalizedChannelFieldValue(field.value, type: field.type)
                }
                acknowledged = (scope: entry.scope, values: entry.values, unchanged: unchanged)
            }
            for field in channel.fields {
                let draftKey = channelDraftKey(scope: field.scope, key: field.key)
                let submitted = acknowledged?.scope == field.scope ? acknowledged?.values[field.key] : nil
                if let sent = submitted, let draft = ui.drafts[draftKey],
                   normalizedChannelFieldValue(draft, type: field.type) == normalizedChannelFieldValue(sent, type: field.type) {
                    ui.drafts.removeValue(forKey: draftKey)
                } else if submitted == nil && field.type != "secret" &&
                            ui.drafts[draftKey] != nil &&
                            normalizedChannelFieldValue(ui.drafts[draftKey], type: field.type) == normalizedChannelFieldValue(field.value, type: field.type) {
                    ui.drafts.removeValue(forKey: draftKey)
                }
            }
            let personalSave = acknowledged?.scope == "user"
            if channel.configured && channel.status != "pairing_pending" && (personalSave || (!preservePairing &&
                (previous?.configured == false || (!ui.pairingInstruction.isEmpty && (channel.pairing_instruction ?? "").isEmpty)))) {
                if channel.fields.contains(where: { $0.scope == "user" && ui.drafts[channelDraftKey(scope: $0.scope, key: $0.key)] != nil }) || (personalSave && acknowledged?.unchanged == false) {
                    ui.editingPersonal = true
                } else {
                    ui.resetPersonal(preservePendingPair: ui.waitingForPairCode)
                    feedback.expire(.init(.channelPair, key))
                    feedback.expire(channelSaveFeedbackKey(key, scope: "user"))
                }
            }
            if let acknowledged, acknowledged.scope == "hub" {
                ui.editingGateway = !acknowledged.unchanged || channel.hub_ready != true
            }
            if !preservePairing, let instruction = channel.pairing_instruction, !instruction.isEmpty {
                ui.pairingInstruction = instruction
                ui.pairingCommand = channel.pairing_command ?? channel.pairing_code
                ui.pairingURL = channel.pairing_url
            } else if !preservePairing && channel.status != "pairing_pending" && !ui.waitingForPairCode {
                ui.pairing = nil
                ui.clearPairingInstructions()
                feedback.expire(.init(.channelPair, key))
            }
            settingsChannels.editors[key] = ui
            if let acknowledged {
                let gateway = acknowledged.scope == "hub"
                let collapsed = gateway ? channel.hub_ready == true && !ui.editingGateway :
                    isPersonalChannelCollapsed(channel, ui: ui)
                if !gateway || !collapsed {
                    feedback.set(channelSaveFeedbackKey(key, scope: acknowledged.scope),
                                 until: acknowledged.unchanged ? Date().addingTimeInterval(1.5) : nil)
                }
                if acknowledged.unchanged && collapsed, settingsVisible, settingsActiveTab == "channels",
                   let mounted = mountedChannelsSettings, mounted.hub == target, mounted.principal == principal {
                    mounted.settleFocus(channel: channel.id, scope: acknowledged.scope)
                }
            }
        }
    }

    private func isPersonalChannelCollapsed(_ channel: ChannelSetupItem, ui: ChannelUiState) -> Bool {
        channel.hub_ready == true && channel.configured && !ui.editingPersonal && ui.pairingInstruction.isEmpty
    }

    func channelSavePending(_ op: String, hub: String, channel: String, scope: String) -> Bool {
        pendingControls.values.contains { pending in
            guard pending.op == op && pending.hub == hub && pending.target == channel,
                  pending.channelEditor == channelEditorKey(hub: hub, channel: channel), let sent = pending.channelSave, sent.scope == scope else { return false }
            let fields = settingsChannels.inventory[hub]?.first { $0.id == channel }?.fields ?? []
            let drafts = settingsChannels.editors[channelEditorKey(hub: hub, channel: channel)]?.drafts ?? [:]
            return fields.filter { $0.scope == scope }.allSatisfy { field in
                let draft = drafts[channelDraftKey(scope: field.scope, key: field.key)]
                return normalizedChannelFieldValue(draft ?? field.value, type: field.type) ==
                    normalizedChannelFieldValue(sent.values[field.key] ?? field.value, type: field.type)
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
    func cancelChannelEditing(in view: ChannelsSettingsView) {
        let hub = view.hub
        let editingGateway = view.channels.filter { settingsChannels.editors[channelEditorKey(hub: hub, channel: $0.id)]?.editingGateway == true }
        let editing = editingGateway.isEmpty
            ? view.channels.filter { settingsChannels.editors[channelEditorKey(hub: hub, channel: $0.id)]?.editingPersonal == true }
            : editingGateway
        guard !editing.isEmpty else { closeSettingsPanel(); return }
        for channel in editing {
            let key = channelEditorKey(hub: hub, channel: channel.id)
            clearChannelDrafts(channel, scope: editingGateway.isEmpty ? "user" : "hub", hub: hub)
            if editingGateway.isEmpty { settingsChannels.editors[key]?.resetPersonal() }
            else { settingsChannels.editors[key]?.resetGateway() }
        }
        rebuildSettingsPanel()
    }

    private func canUnpair(_ channel: ChannelSetupItem) -> Bool {
        if channel.hub_ready == true || channel.pairing_code != nil { return channel.can_unpair == true }
        // The current projection gates can_unpair on shared readiness. Recover
        // it from an independently configured personal binding when possible.
        guard channel.has_hub_service == true, channel.hub_ready == false else { return false }
        let fields = channel.fields.filter { $0.scope == "user" }
        return fields.filter(\.required).allSatisfy(\.configured) &&
            (channel.enabled || channel.display_label?.isEmpty == false || fields.contains(where: \.configured))
    }

    func clearChannelDrafts(_ channel: ChannelSetupItem, scope: String, hub: String) {
        let key = channelEditorKey(hub: hub, channel: channel.id)
        if scope == "user" {
            feedback.expire(.init(.channelPair, key))
            feedback.expire(channelSaveFeedbackKey(key, scope: scope))
        }
        for field in channel.fields where field.scope == scope {
            settingsChannels.editors[key, default: ChannelUiState()].drafts.removeValue(forKey: channelDraftKey(scope: field.scope, key: field.key))
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

    func mountChannelHubBar(in root: NSView, top: CGFloat, renderedHub: String, handoff: SegmentedPillStrip.Handoff?) {
        let hubs = enabledChannelHubs
        guard hubs.count > 1 else { return }
        let height = CitoMetrics.settingsHubBarHeight
        let bar = NSView(frame: NSRect(x: 0, y: top - height, width: root.bounds.width, height: height))
        bar.identifier = NSUserInterfaceItemIdentifier("settings_channel_hub_bar")
        let label = textLabel("Hub", NSRect(x: 16, y: (height - 14) / 2, width: 30, height: 14),
                              size: 11, weight: .semibold, color: CitoPalette.secondaryText)
        label.identifier = NSUserInterfaceItemIdentifier("settings_channel_hub_label")
        bar.addSubview(label)
        bar.addSubview(roundedView(NSRect(x: 0, y: 0, width: bar.bounds.width, height: 1), radius: 0,
                                   color: .labelColor.withAppearanceAlpha(0.08)))
        let stripWidth = bar.bounds.width - 66
        let pills = hubs.map { name in
            let shown = compactAddressLabel(name, maxWidth: stripWidth - 4, font: .systemFont(ofSize: 11, weight: .medium), paddingX: 11, leadingSymbol: nil)
            let pill = MenuControl(shown, frame: NSRect(x: 0, y: 0, width: max(64, MenuControl.intrinsicWidth(shown)), height: 22), radius: 11)
            pill.toolTip = name; pill.setAccessibilityLabel(name)
            pill.identifier = NSUserInterfaceItemIdentifier("channel_hub_selector:\(name)")
            pill.pressed = { [weak self] in
                guard let self else { return }
                settingsChannels.selectedHub = name
                emit(["op": "open_settings", "hub": name])
                rebuildSettingsPanel(preservingEditor: false, tabs: [.channels])
            }
            return pill
        }
        root.addSubview(bar)
        mountSettingsPills(in: bar, frame: NSRect(x: 50, y: (height - 26) / 2, width: stripWidth, height: 26),
            id: "channel_hub_strip", geometry: CitoMetrics.hubSegments, pills: pills,
            selected: pills.first { $0.identifier?.rawValue == "channel_hub_selector:\(renderedHub)" }, handoff: handoff)
    }

    private func makeInboxScopeControl(id: String, inbox: ChannelsPresentation, enabled: Bool, pending: Bool) -> MenuControl {
        let workspace = inbox.scope == "workspace"
        let choices = [("self", "Only Me", "lock", "Your own sessions and Macs on '\(inbox.hub)'"),
                       ("workspace", "Everyone", "person.2", "Any signed-in member on this Hub can find and message you")]
        let (_, title, symbol, detail) = choices[workspace ? 1 : 0]
        let scope = MenuControl(title, frame: NSRect(x: 0, y: 0, width: 0, height: 22), leadingSymbol: symbol, radius: 11, paddingX: 18)
        let hub = inbox.hub, revision = inbox.revision, selected = inbox.scope, user = inbox.user
        let menu = NSMenu()
        menu.autoenablesItems = false
        for (value, title, _, subtitle) in choices {
            let item = InboxScopeItem(title: title, action: #selector(InboxScopeItem.choose(_:)), keyEquivalent: "")
            item.target = item
            item.representedObject = value
            item.state = value == inbox.scope ? .on : .off
            item.isEnabled = enabled
            item.toolTip = subtitle
            if #available(macOS 14.4, *) { item.subtitle = subtitle }
            item.selected = { [weak self, weak scope] in
                guard let self, let scope, scope.window != nil, scope.isEnabled, !scope.isHiddenOrHasHiddenAncestor,
                      settingsVisible, effectiveChannelHub == hub, value != selected else { return }
                // Menu tracking may receive a Settings refresh before selection.
                let current = channelsPresentation(hub: hub)
                guard current.current, current.user == user, current.scopeListener == id, current.revision == revision else { return }
                updateInbox(scope: value, hub: hub, revision: revision)
            }
            menu.addItem(item)
        }
        scope.identifier = NSUserInterfaceItemIdentifier("scope:\(id)")
        scope.isEnabled = enabled
        scope.toolTip = "\(title) — \(detail). Choose who can find and message your inbox."
        scope.menu = menu
        scope.reservedTitles = choices.map { ($0.1, $0.2) } + [("Updating…", nil)]
        scope.frame.size.width = scope.reservedWidth
        if pending { scope.setPending("Updating…") }
        let chevron = NSImageView(frame: NSRect(x: scope.bounds.width - 17, y: 7, width: 9, height: 8))
        chevron.image = NSImage(systemSymbolName: "chevron.down", accessibilityDescription: nil)
        chevron.isHidden = pending
        scope.addSubview(chevron)
        scope.accessorySymbolView = chevron
        scope.label.alignment = .left
        scope.layoutContents()
        if workspace && !pending, let icon = scope.leadingSymbolView {
            icon.symbolConfiguration = NSImage.SymbolConfiguration(paletteColors: [CitoPalette.infoText])
        }
        scope.pressed = { [weak self, weak scope] in
            guard self?.headless == false, let scope else { return }
            menu.popUp(positioning: nil, at: .zero, in: scope)
        }
        return scope
    }

    func buildChannelsTab(in scroll: NSScrollView, renderedHub: String) {
        let cardWidth = scroll.contentSize.width - 32
        let stratumWidth = cardWidth - 24
        let stratumTextWidth = stratumWidth - 24
        let allowance = settingsScrollerAllowance(scroll)
        let inbox = channelsPresentation(hub: renderedHub)
        let resolvedChannels = inbox.inventory
        let inventoryStale = !(inbox.error ?? "").isEmpty
        let visibleChannels = resolvedChannels.filter { $0.can_configure_hub == true || $0.hub_ready == true || canUnpair($0) }
        let channelsView = ChannelsSettingsView(hub: renderedHub, principal: inbox.user, channels: resolvedChannels, companion: self)
        channelsView.frame.size.width = scroll.contentSize.width
        channelsView.identifier = NSUserInterfaceItemIdentifier(SettingsTab.channels.scrollId)
        channelsView.isHidden = settingsActiveTab != "channels"
        let inboxHub = renderedHub
        let inboxPaused = inbox.paused
        let inboxUser = inbox.user
        let revision = inbox.revision
        let pausePending = isPending("human_inbox_update",hub:inboxHub,target:"paused")
        let scopePending = isPending("human_inbox_update",hub:inboxHub,target:"scope")
        // Both actions share a revision; only the submitted action shows progress.
        let inboxEnabled = inbox.current && !pausePending && !scopePending
        let delivery = inboxDeliveryDetail(inbox.attachments)
        let inboxHeight = CitoMetrics.inboxCardHeight
        let inboxBar = roundedView(NSRect(x: 16, y: channelsView.bounds.height - 12 - inboxHeight, width: cardWidth, height: inboxHeight),
            radius: CitoMetrics.cardRadius, color: inboxPaused ? CitoPalette.warningSurface : CitoPalette.channelCardFill,
            border: inboxPaused ? CitoPalette.warningBorder : CitoPalette.channelCardBorder)
        inboxBar.autoresizingMask = [.width]
        inboxBar.identifier = NSUserInterfaceItemIdentifier("inbox_control_bar")
        let dot = roundedView(NSRect(x: 12, y: 30, width: 6, height: 6), radius: 3,
            color: inboxPaused ? CitoPalette.warningText : CitoPalette.successText)
        dot.identifier = NSUserInterfaceItemIdentifier("inbox_control_dot")
        inboxBar.addSubview(dot)
        let inboxTitle = textLabel("Inbox · @\(inboxUser)", .zero, size: 11, weight: .semibold)
        inboxTitle.identifier = NSUserInterfaceItemIdentifier("inbox_control_title")
        inboxTitle.toolTip = inboxTitle.stringValue
        inboxTitle.setAccessibilityLabel(inboxPaused ? inboxTitle.stringValue + " (Paused)" : inboxTitle.stringValue)
        inboxTitle.lineBreakMode = .byTruncatingMiddle
        let inboxDetail = textLabel(delivery.summary, .zero, size: 10, color: inboxPaused ? CitoPalette.warningText : CitoPalette.secondaryText)
        inboxDetail.identifier = NSUserInterfaceItemIdentifier("inbox_control_summary")
        inboxDetail.toolTip = delivery.tooltip
        inboxDetail.setAccessibilityLabel(delivery.summary)
        inboxDetail.maximumNumberOfLines = 1
        inboxDetail.lineBreakMode = .byTruncatingTail
        inboxDetail.cell?.wraps = false
        inboxBar.addSubview(inboxTitle); inboxBar.addSubview(inboxDetail)
        let resumeInboxTitle = "Resume", pauseInboxTitle = "Pause"
        let resumingInboxTitle = "Resuming…", pausingInboxTitle = "Pausing…"
        let inboxToggle = MenuControl(inboxPaused ? resumeInboxTitle : pauseInboxTitle,frame:NSRect(x:0,y:22,width:0,height:22))
        inboxToggle.reservedTitles = [resumeInboxTitle, pauseInboxTitle, resumingInboxTitle, pausingInboxTitle].map { ($0, nil) }
        inboxToggle.identifier = NSUserInterfaceItemIdentifier("toggle_inbox")
        inboxToggle.isEnabled = inboxEnabled
        inboxToggle.toolTip = inboxPaused ? "Resume your shared inbox on \(inboxHub), preserving descriptions and channel bindings" : "Pause inbox alerts on \(inboxHub) across all devices and external channels"
        inboxToggle.pressed = { [weak self] in self?.updateInbox(paused:!inboxPaused,hub:inboxHub,revision:revision) }
        if pausePending { inboxToggle.setPending(inboxPaused ? resumingInboxTitle : pausingInboxTitle) }
        inboxBar.addSubview(inboxToggle)
        var inboxActions: [NSView] = []
        if let id = inbox.scopeListener {
            let scope = makeInboxScopeControl(id: id, inbox: inbox, enabled: inboxEnabled, pending: scopePending)
            inboxBar.addSubview(scope); inboxActions.append(scope)
        }
        inboxActions.append(inboxToggle)
        layoutBar(in: inboxBar, y: (inboxHeight - 22) / 2, height: 22, trailing: inboxActions)
        let textRight = inboxActions[0].frame.minX - 10
        inboxTitle.frame = NSRect(x: 24, y: 26, width: textRight - 24, height: 14)
        inboxTitle.autoresizingMask = [.width]
        inboxDetail.frame = NSRect(x: 12, y: 10, width: textRight - 12, height: 13)
        inboxDetail.autoresizingMask = [.width]
        channelsView.addSubview(inboxBar)

        let section = textLabel("EXTERNAL CHANNELS", NSRect(x: 16, y: inboxBar.frame.minY - 26, width: cardWidth, height: 12),
                                size: 10, weight: .semibold, color: CitoPalette.secondaryText)
        section.identifier = NSUserInterfaceItemIdentifier("external_channels_heading")
        section.autoresizingMask = [.width]
        channelsView.addSubview(section)
        var cursorY = section.frame.minY - 6

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
            let message = "External channels are managed by your '\(renderedHub)' Hub administrator. None are enabled yet."
            let height = max(36, measureWrappedHeight(message, width: cardWidth - allowance, font: .systemFont(ofSize: 11)))
            let notice = textLabel(message, NSRect(x:16,y:cursorY-height-6,width:cardWidth,height:height),size:11,color:CitoPalette.secondaryText)
            notice.autoresizingMask = [.width]
            notice.identifier = NSUserInterfaceItemIdentifier("channels_managed_notice")
            notice.maximumNumberOfLines = 0; notice.lineBreakMode = .byWordWrapping
            channelsView.addSubview(notice); cursorY -= height + 12
        }
        var chCursorY: CGFloat = cursorY
        for ch in visibleChannels {
            let visibleFields = ch.fields.filter { $0.hidden != true }
            let channelKey = channelEditorKey(hub: renderedHub, principal: inbox.user, channel: ch.id)
            let channelUi = inbox.editors[channelKey] ?? ChannelUiState()
            let instruction = channelUi.pairingInstruction
            let instructionFont = NSFont.monospacedSystemFont(ofSize: 10.5, weight: .semibold)
            let instructionHeight = instruction.isEmpty ? 0 : max(16, measureWrappedHeight(instruction, width: stratumTextWidth - allowance, font: instructionFont))
            let guide = (ch.setup_guide ?? []).joined(separator:"\n")
            let hubFields = visibleFields.filter { $0.scope == "hub" && ch.can_configure_hub == true }
            let disclosure = ch.can_edit_personal == true && ch.pairing_supported && ch.can_pair == true && ch.pairing_manual_disclosure == true
            let manualExpanded = channelUi.manualEntry
            let userFields = visibleFields.filter { $0.scope != "hub" }
            let gatewayActive = ch.hub_ready == true
            let canUnpair = canUnpair(ch)
            let showUserStratum = gatewayActive || canUnpair
            let formFields = !gatewayActive || ch.can_edit_personal != true || (disclosure && !manualExpanded) ? [] : userFields
            let hasHubScopeFields = ch.has_hub_service == true
            let showHubStratum = hasHubScopeFields && (ch.can_configure_hub == true || ch.error_origin == "hub")
            let hubFieldsH = CGFloat(hubFields.count) * 26
            let userFieldsH = CGFloat(formFields.count) * 26 + (gatewayActive && disclosure ? 24 : 0)
            let hubGatewayExpanded = ch.can_configure_hub == true && (!gatewayActive || channelUi.editingGateway)
            let isOutboundOnly = ch.inbound_mode == "outbound_only"
            let showInboundToggle = ch.supports_outbound_only == true && ch.can_configure_hub == true && gatewayActive && !hubGatewayExpanded
            let personalCollapsed = isPersonalChannelCollapsed(ch, ui: channelUi)
            let reusableBinding = !personalCollapsed && gatewayActive && instruction.isEmpty && !manualExpanded ? ch.reusable_binding : nil
            let guideH: CGFloat = !gatewayActive && !guide.isEmpty && !hubFields.isEmpty ? max(13, measureWrappedHeight(guide, width: stratumTextWidth - allowance, font: .systemFont(ofSize: 10))) : 0
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
            let titleHeight = separateTitle ? max(14, measureWrappedHeight(hubTitleText, width: titleWidth - allowance, font: titleFont, lineBreakMode: .byCharWrapping)) : 14
            let extraTitleH = separateTitle ? titleHeight + CitoMetrics.gap : 0
            let twoRowGatewayTop = !hubGatewayExpanded && (showInboundToggle || (!serviceSuffix.isEmpty && (!hubFields.isEmpty || gatewayHasClear)))
            let hubStratumH: CGFloat = !showHubStratum ? 0 : (hubGatewayExpanded ? 36 + gatewayRoleH + guideH + hubFieldsH + (!hubFields.isEmpty || gatewayHasClear ? 32 : 0) : (twoRowGatewayTop ? 64 : 36))
            let collapsedSummary = "Paired · \(ch.display_label ?? userFields.first?.value ?? "")"
            let confirmedSummary = collapsedSummary + " · ✓ Saved"
            let collapsedSummaryHeight = max(16, measureWrappedHeight(confirmedSummary, width: stratumTextWidth - allowance, font: .systemFont(ofSize: 10)))
            let userStratumH: CGFloat = !showUserStratum ? 0 : (personalCollapsed ? 44 + collapsedSummaryHeight : 40 + userFieldsH + (instructionHeight > 0 ? instructionHeight + 34 : 0) + 34)
            let channelResult = channelTestResult(ch, ui: channelUi)
            let showDetail = ch.warning == true || channelResult != nil
            let detail = channelResult?.detail ?? ch.detail
            // Keep one diagnostic line available while an editor is mounted.
            let canFocusEditor = (hubGatewayExpanded && !hubFields.isEmpty) ||
                (!personalCollapsed && !formFields.isEmpty) || !instruction.isEmpty
            let detailHeight = showDetail ? max(14, measureWrappedHeight(detail, width: cardWidth - 58 - allowance, font: .systemFont(ofSize: 10.5))) : (canFocusEditor ? 14 : 0)
            let headerHeight: CGFloat = 38 + detailHeight
            let cardHeight: CGFloat = headerHeight + hubStratumH + extraTitleH + (showHubStratum && showUserStratum ? CitoMetrics.gap : 0) + userStratumH + 8
            chCursorY -= cardHeight
            let chCard = roundedView(NSRect(x: 16, y: chCursorY, width: cardWidth, height: cardHeight), radius: 10,
                color: CitoPalette.channelCardFill, border: CitoPalette.channelCardBorder)
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

            let hubStratum = roundedView(NSRect(x:12,y:8+userStratumH+(showUserStratum ? 6 : 0),width:stratumWidth,height:hubStratumH + extraTitleH),radius:8,
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
                let clear=MenuControl("Clear Token",frame:NSRect(x:0,y:7,width:0,height:22),radius:6)
                clear.label.font = .systemFont(ofSize:10.5)
                clear.reservedTitles = [("Clear Token", nil), ("Clearing…", nil)]
                clear.style = .destructiveHover
                clear.identifier=NSUserInterfaceItemIdentifier("clear_channel_service:\(ch.id)")
                clear.isEnabled = !inventoryStale
                clear.pressed={ [weak self] in guard let self=self, (self.settingsChannels.errors[renderedHub] ?? "").isEmpty else{return}; self.emit(["op":"disconnect_human_channel","hub":renderedHub,"channel_id":ch.id,"reset_hub":true,"reset_user":false]) }
                if isPending("disconnect_human_channel", hub: renderedHub, target: ch.id) { clear.setPending("Clearing…") }
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
            // Keep manual fields near Save even when the pairing instructions exceed the viewport.
            let manualBelowInstruction = disclosure && manualExpanded && !instruction.isEmpty && !formFields.isEmpty
            let personalFieldsTop = userStratumH - 40 - (manualBelowInstruction ? instructionHeight + 4 : 0)
            for (container, fields, top) in [(hubStratum,hubGatewayExpanded ? hubFields : [],hubStratumH-34-gatewayRoleH-guideH),(userStratum,gatewayActive && !personalCollapsed ? formFields : [],personalFieldsTop)] {
                let labelW = max(56, min(110, (fields.map { ceil(measureTextWidth($0.label, font: .systemFont(ofSize: 11, weight: .medium))) }.max() ?? 56) + 6))
                let inputX = 12 + labelW + 10
                let inputW = container.bounds.width - 12 - inputX
                var fieldY=top-22
                for f in fields {
                    let fieldId="channel_field:\(ch.id):\(f.key)"
                    let labelView=textLabel(f.label,NSRect(x:12,y:fieldY+3,width:labelW,height:15),size:11,weight:.medium)
                    labelView.toolTip=f.label; container.addSubview(labelView)
                    let initialVal=channelUi.drafts[channelDraftKey(scope: f.scope, key: f.key)] ?? (f.value ?? "")
                    if f.type == "boolean" {
                        let toggle = NSButton(checkboxWithTitle: "", target: channelsView, action: #selector(ChannelsSettingsView.toggleBooleanField(_:)))
                        toggle.frame = NSRect(x: inputX, y: fieldY + 2, width: 22, height: 18)
                        toggle.identifier = NSUserInterfaceItemIdentifier(fieldId)
                        toggle.setAccessibilityLabel(f.label)
                        toggle.toolTip = f.help_text
                        toggle.state = initialVal.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() == "true" ? .on : .off
                        toggle.isEnabled = !inventoryStale
                        container.addSubview(toggle)
                    } else {
                        let inputField=makeEditableField(fieldId,frame:NSRect(x:inputX,y:fieldY,width:inputW,height:22),placeholder:f.type=="secret" && f.configured ? (f.configured_placeholder ?? "•••••••• (Saved — enter new value to replace)") : f.placeholder,value:initialVal,secret:f.type=="secret",accessibilityLabel:f.label)
                        inputField.autoresizingMask = [.width]
                        inputField.delegate = channelsView
                        inputField.toolTip=f.help_text; container.addSubview(inputField)
                    }
                    fieldY-=26
                }
            }
            if showHubStratum { chCard.addSubview(hubStratum) }
            if showUserStratum { chCard.addSubview(userStratum) }
            if !instruction.isEmpty {
                let instructionTop = userStratumH - 40 - (manualBelowInstruction ? 0 : CGFloat(formFields.count) * 26)
                let instrLabel = textLabel(instruction, NSRect(x: 12, y: instructionTop - instructionHeight, width: stratumTextWidth, height: instructionHeight), size: 10.5, weight: .semibold, color: CitoPalette.infoText)
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
                    save.reservedTitles = ["Save Gateway Token", "Saving…", "✓ Saved"].map { ($0, nil) }
                    save.pressed = { [weak channelsView] in channelsView?.submit(.saveGateway, channel: chId) }
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
                    edit.isEnabled = !inventoryStale
                    edit.pressed={ [weak self] in guard let self=self, (self.settingsChannels.errors[renderedHub] ?? "").isEmpty else{return}; self.settingsChannels.editors[channelKey, default: ChannelUiState()].editingGateway = true; self.rebuildSettingsPanel(revealChannel: ch.id, revealPairing: false) }
                    hubStratum.addSubview(edit); gatewayTop.insert(edit, at: 0)
                }
            }
            if showInboundToggle {
                let enableRepliesTitle = "Enable Replies", outboundOnlyTitle = "Use Outbound-Only", applyingInboundTitle = "Applying…"
                let title = isOutboundOnly ? enableRepliesTitle : outboundOnlyTitle
                let toggle = MenuControl(title, frame:NSRect(x:0,y:7,width:0,height:22))
                toggle.reservedTitles = [enableRepliesTitle, outboundOnlyTitle, applyingInboundTitle].map { ($0, nil) }
                toggle.identifier = NSUserInterfaceItemIdentifier("toggle_channel_inbound_mode:\(ch.id)")
                toggle.isEnabled = !inventoryStale
                if isPending("save_hub_service", hub: renderedHub, target: ch.id) { toggle.setPending(applyingInboundTitle) }
                toggle.pressed = { [weak self] in
                    guard let self, (self.settingsChannels.errors[renderedHub] ?? "").isEmpty, !self.isPending("save_hub_service",hub:renderedHub,target:chId) else { return }
                    self.emit(["op":"save_hub_service","hub":renderedHub,"channel_id":chId,
                               "inbound_mode":isOutboundOnly ? "primary" : "outbound_only"])
                }
                hubStratum.addSubview(toggle); gatewayTop.append(toggle)
            }
            if disclosure && !personalCollapsed && gatewayActive {
                let hideTitle = "▾ Hide manual entry"
                let enterTitle = "▸ Enter \(userFields.first?.label ?? "account") manually…"
                let title = manualExpanded ? hideTitle : enterTitle
                let manual = MenuControl(title,frame:NSRect(x:12,y:instruction.isEmpty ? 38 : 66,width:0,height:22),paddingX:0)
                manual.reservedTitles = [(hideTitle, nil), (enterTitle, nil)]
                manual.frame.size.width = min(userStratum.bounds.width - 24 - allowance, manual.reservedWidth + 8)
                manual.label.alignment = .left; manual.layoutContents()
                manual.style = .subtleHeader; manual.identifier=NSUserInterfaceItemIdentifier("manual_channel:\(ch.id)")
                manual.pressed={ [weak self] in guard let self=self else{return}; self.settingsChannels.editors[channelKey, default: ChannelUiState()].manualEntry.toggle(); self.rebuildSettingsPanel(revealChannel: ch.id, revealPairing: false, revealEditorScope: "user") }
                userStratum.addSubview(manual)
            }
            let showPair = ch.can_pair == true && (!personalCollapsed || ch.can_edit_personal != true) && gatewayActive && instruction.isEmpty
            // A collapsed read-only binding can show Test, Re-pair, Pause and Unpair on one row.
            let pairingActionPadding: CGFloat = personalCollapsed && showPair ? CitoMetrics.gap : 11
            if showPair {
                let pairTitle = ch.configured ? "Re-pair…" : (ch.pairing_action_label ?? "Pair Account…")
                let pairBtn = MenuControl(pairTitle, frame: NSRect(x: 12, y: 6, width: 0, height: 24), leadingSymbol: "link", radius: 6, paddingX: pairingActionPadding)
                if personalCollapsed { pairBtn.label.font = .systemFont(ofSize: 10.5) }
                pairBtn.reservedTitles = [(pairTitle, "link"), ("Pairing…", nil)]
                pairBtn.toolTip = ch.pairing_help_text
                pairBtn.identifier = NSUserInterfaceItemIdentifier("pair_channel:\(ch.id)")
                pairBtn.pressed = { [weak channelsView] in channelsView?.submit(.pair, channel: chId) }
                pairBtn.isEnabled = !inventoryStale
                if channelUi.waitingForPairCode { pairBtn.setPending("Pairing…") }
                pairBtn.style = ch.configured || reusableBinding != nil ? .neutral : .prominent
                userStratum.addSubview(pairBtn); personalActions.append(pairBtn)
            }

            if !instruction.isEmpty {
                let copy=MenuControl("Copy Command",frame:NSRect(x:0,y:0,width:0,height:22),leadingSymbol:"doc.on.doc",radius:6)
                copy.label.font = .systemFont(ofSize:10.5)
                copy.identifier=NSUserInterfaceItemIdentifier("copy_pair_command:\(ch.id)")
                copy.reservedTitles = [("Copy Command", "doc.on.doc"), ("✓ Copied", nil)]
                let key = TransientFeedbackStore.Key(.channelPair, channelKey)
                copy.pressed = { [weak self, weak copy] in
                    self?.copyText(channelUi.pairingCommand ?? instruction, control: copy, key: key)
                }
                copy.showFeedback(key: key)
                if let raw=channelUi.pairingURL, let url=URL(string:raw) {
                    let open=MenuControl(ch.pairing_open_url_label ?? "Open Pairing Link ↗",frame:NSRect(x:12,y:42,width:150,height:22))
                    open.identifier=NSUserInterfaceItemIdentifier("open_pair_url:\(ch.id)"); open.style = .prominent
                    open.pressed={ if !usesPrivatePasteboard { NSWorkspace.shared.open(url) } }
                    userStratum.addSubview(open); pairingActions.append(open)
                }
                userStratum.addSubview(copy); pairingActions.append(copy)
                let cancel=MenuControl("Cancel",frame:NSRect(x:386,y:42,width:0,height:22),radius:6)
                cancel.reservedTitles = [("Cancel", nil), ("Cancelling…", nil)]
                cancel.identifier=NSUserInterfaceItemIdentifier("cancel_pair_channel:\(ch.id)")
                cancel.isEnabled = !inventoryStale
                cancel.pressed={ [weak self] in
                    guard let self=self, (self.settingsChannels.errors[renderedHub] ?? "").isEmpty else{return}
                    self.emit(["op":"channel_cancel_pair","hub":renderedHub,"channel_id":chId])
                }
                if isPending("channel_cancel_pair", hub: renderedHub, target: ch.id) { cancel.setPending("Cancelling…") }
                userStratum.addSubview(cancel); pairingActions.append(cancel)
            }
            if ch.configured || canUnpair {
                if ch.configured {
                    let pauseTitle = "Pause " + ch.display_name
                    let resumeTitle = "Resume " + ch.display_name
                    let pausingTitle = "Pausing…"
                    let resumingTitle = "Resuming…"
                    let toggleTitle = ch.enabled ? pauseTitle : resumeTitle
                    let toggleFont: NSFont = personalCollapsed ? .systemFont(ofSize: 10.5) : .systemFont(ofSize: 11, weight: .medium)
                    let toggleWidth: CGFloat = 170
                    func compact(_ title: String) -> String {
                        compactAddressLabel(title, maxWidth: toggleWidth, font: toggleFont, paddingX: 11, leadingSymbol: nil)
                    }
                    let pause = compact(pauseTitle), resume = compact(resumeTitle)
                    let toggle = MenuControl(ch.enabled ? pause : resume, frame: NSRect(x: 130, y: 6, width: 0, height: 24), radius: 6)
                    toggle.label.font = toggleFont
                    toggle.toolTip = toggleTitle; toggle.setAccessibilityLabel(toggleTitle)
                    toggle.reservedTitles = [pause, resume, pausingTitle, resumingTitle].map { ($0, nil) }
                    toggle.identifier=NSUserInterfaceItemIdentifier("toggle_channel:\(ch.id)")
                    toggle.style = ch.enabled ? .neutral : .prominent
                    toggle.isEnabled = !inventoryStale
                    toggle.pressed={ [weak self] in guard let self=self, (self.settingsChannels.errors[renderedHub] ?? "").isEmpty else{return}; self.emit(["op":"channel_toggle","hub":renderedHub,"channel_id":chId,"enabled":!ch.enabled]) }
                    if isPending("channel_toggle", hub: renderedHub, target: ch.id) { toggle.setPending(ch.enabled ? pausingTitle : resumingTitle) }
                    userStratum.addSubview(toggle); personalActions.append(toggle)
                }
                let unpair=MenuControl("Unpair",frame:NSRect(x:196,y:6,width:0,height:24),radius:6,paddingX:pairingActionPadding)
                if personalCollapsed { unpair.label.font = .systemFont(ofSize: 10.5) }
                unpair.reservedTitles = [("Unpair", nil), ("Unpairing…", nil)]
                unpair.style = .destructiveHover
                unpair.identifier=NSUserInterfaceItemIdentifier("unpair_channel:\(ch.id)")
                unpair.pressed={ [weak self] in guard let self=self, (self.settingsChannels.errors[renderedHub] ?? "").isEmpty else{return}; self.emit(["op":"channel_unpair","hub":renderedHub,"channel_id":chId,"reset_user":true]) }
                unpair.isHidden = !canUnpair
                unpair.isEnabled = !inventoryStale
                if isPending("channel_unpair", hub: renderedHub, target: ch.id) { unpair.setPending("Unpairing…") }
                userStratum.addSubview(unpair); personalActions.append(unpair)
            }
            let testBtn = MenuControl("Send Test", frame: NSRect(x: 290, y: 6, width: MenuControl.intrinsicWidth("Send Test"), height: 24), radius: 6)
            testBtn.identifier = NSUserInterfaceItemIdentifier("test_channel:\(ch.id)")
            testBtn.reservedTitles = [("Send Test", nil), ("Testing…", nil)]
            testBtn.pressed = { [weak channelsView] in channelsView?.submit(.test, channel: chId) }
            testBtn.isHidden = !gatewayActive || ch.can_test != true || (disclosure && !ch.configured && !manualExpanded)
            userStratum.addSubview(testBtn)

            let saveBtn = MenuControl("Save", frame: NSRect(x: 376, y: 6, width: 68, height: 24), radius: 6)
            saveBtn.identifier = NSUserInterfaceItemIdentifier("save_channel:\(ch.id)")
            saveBtn.reservedTitles = ["Save", "Saving…", "✓ Saved"].map { ($0, nil) }
            saveBtn.style = .prominent
            saveBtn.pressed = { [weak channelsView] in channelsView?.submit(.savePersonal, channel: chId) }
            if ch.can_edit_personal == true && !personalCollapsed && gatewayActive && (!disclosure || manualExpanded) { userStratum.addSubview(saveBtn) } else { saveBtn.isHidden=true }

            layoutBar(in: hubStratum, y: hubStratumH-29, height: 22, flexible: separateTitle ? nil : hubTitle,
                      trailing: twoRowGatewayTop ? [gatewayStatus] : [gatewayStatus] + gatewayTop)
            if twoRowGatewayTop { layoutBar(in: hubStratum, y: 7, height: 22, trailing: gatewayTop) }
            if hubGatewayExpanded { layoutBar(in: hubStratum, y: 8, height: 24, leading: gatewayLeading, trailing: gatewayTrailing) }
            if personalCollapsed {
                let key = channelSaveFeedbackKey(channelKey, scope: "user")
                let saved = feedback.deadline(key) != nil
                let summary = textLabel(saved ? confirmedSummary : collapsedSummary,
                    NSRect(x: 12, y: 38, width: stratumTextWidth, height: collapsedSummaryHeight), size: 10, color: CitoPalette.secondaryText)
                summary.identifier = NSUserInterfaceItemIdentifier("channel_summary:\(ch.id)")
                summary.toolTip = summary.stringValue; summary.lineBreakMode = .byWordWrapping; summary.maximumNumberOfLines = 0
                if saved {
                    feedback.observe(key) { [weak summary] in
                        summary?.stringValue = collapsedSummary
                        summary?.toolTip = collapsedSummary
                    }
                }
                summary.autoresizingMask = [.width]
                userStratum.addSubview(summary)
                let edit = MenuControl("Edit…",frame:NSRect(x:0,y:6,width:MenuControl.intrinsicWidth("Edit…"),height:24))
                edit.identifier = NSUserInterfaceItemIdentifier("edit_user_channel:\(ch.id)"); edit.style = .neutral
                edit.isEnabled = !inventoryStale
                edit.pressed = { [weak self] in guard let self=self, (self.settingsChannels.errors[renderedHub] ?? "").isEmpty else{return}; self.settingsChannels.editors[channelKey, default: ChannelUiState()].editingPersonal = true; self.settingsChannels.editors[channelKey, default: ChannelUiState()].manualEntry = true; self.rebuildSettingsPanel(revealChannel: ch.id, revealPairing: false, revealEditorScope: "user") }
                edit.isHidden = ch.can_edit_personal != true
                userStratum.addSubview(edit)
                testBtn.label.font = .systemFont(ofSize: 10.5)
                edit.label.font = .systemFont(ofSize: 10.5)
                layoutBar(in:userStratum,y:7,height:24,spacing:8,trailing:[testBtn] + personalActions.dropLast() + [edit] + personalActions.suffix(1))
            } else if showUserStratum {
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
                    link.isEnabled = !inventoryStale
                    link.reservedTitles = [(shown, "link"), ("Linking…", nil)]
                    if isPending("channel_save", hub: renderedHub, target: ch.id) { link.setPending("Linking…") }
                    link.pressed = { [weak self] in
                        guard let self, (self.settingsChannels.errors[renderedHub] ?? "").isEmpty, !self.isPending("channel_save",hub:renderedHub,target:chId) else { return }
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
        channelsView.refreshMountedActions()
        mountSettingsCards(channelsView, on: scroll)

    }

}
