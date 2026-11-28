import AppKit

enum SettingsTab: String, CaseIterable {
    case apps, channels, hubs

    var scrollId: String { "settings_\(rawValue)_tab" }

    static func affected(by operation: String) -> Self? {
        if Companion.channelControlOps.contains(operation) || operation == "human_inbox_update" { return .channels }
        if Companion.hubOrderOps.contains(operation) { return .hubs }
        switch operation {
        case "setup_apply", "setup_remove_app", "setup_disconnect_all": return .apps
        case "set_host_hub", "hub_action", "setup_toggle_hub", "setup_remove_hub": return .hubs
        default: return nil
        }
    }
}

private final class SettingsCheckbox: NSButton {
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { false }
    override var canBecomeKeyView: Bool { isEnabled && acceptsFirstResponder && !isHiddenOrHasHiddenAncestor }
}

private final class SettingsScrollView: NSScrollView {
    override func tile() {
        super.tile()
        // Resize the existing document, including while its field editor is mounted.
        if let document = documentView {
            let height = (document as? ChannelsSettingsView)?.documentHeight(for: contentSize.height) ?? document.frame.height
            let size = NSSize(width: contentSize.width, height: height)
            if document.frame.size != size { document.setFrameSize(size) }
        }
    }
}

private final class SettingsPillClipView: NSClipView {
    override func hitTest(_ point: NSPoint) -> NSView? {
        if let hit = super.hitTest(point) { return hit }
        // A sibling Settings document scroll can leave AppKit's clip hit test
        // without a target even though the pills still occupy this viewport.
        let local = convert(point, from: superview)
        guard !isHiddenOrHasHiddenAncestor, bounds.contains(local) else { return nil }
        return documentView?.hitTest(local)
    }
}

func mountSettingsPills(in container: NSView, frame: NSRect, id: String, geometry: PillTrackGeometry,
                       pills: [MenuControl], selected: MenuControl?, handoff: SegmentedPillStrip.Handoff?) {
    let strip = SegmentedPillStrip.mount(in: container, frame: frame, id: id, spacing: 4,
        geometry: geometry, pills: pills, selected: selected, handoff: handoff)
    let document = strip.documentView
    let offset = strip.contentView.bounds.origin
    let clip = SettingsPillClipView(frame: strip.contentView.frame)
    // NSScrollView reads its background from the clip view. Carry over the
    // translucent fill without adding a second layer above the moving thumb.
    clip.backgroundColor = strip.backgroundColor
    clip.drawsBackground = strip.drawsBackground
    strip.contentView = clip
    strip.documentView = document
    clip.scroll(to: offset)
    strip.reflectScrolledClipView(clip)
}

// Reserve wrapping height and intrinsic-width siblings at the narrowest
// viewport this fixed-size panel can have under either scroller style.
func settingsScrollerAllowance(_ scroll: NSScrollView) -> CGFloat {
    let legacy = NSScroller.scrollerWidth(for: scroll.verticalScroller?.controlSize ?? .regular, scrollerStyle: .legacy)
    return max(0, scroll.contentSize.width - (scroll.bounds.width - legacy))
}

// Compare the presentation inputs around a received frame, not its envelope:
// a receipt may carry a full projection, or no projection at all.
struct SettingsPresentation {
    var apps: [AppSetupItem]
    var harnesses: [HarnessMeta]
    var appChecks: [String: Date]
    var hubRows: [HubConfigItem]
    var host: HostHubSummary?
    var hubForm: Companion.HubFormState
    var hostPort: Companion.HostHubPortState
    var inviteRequestId: String?
    var hostPortFeedback: Date?
    var inviteFeedback: Date?
    var channels: ChannelsPresentation
    var mountedChannels: ChannelsPresentation?
    var pending: [SettingsTab: Set<String>]
    var banner: String?
    var launchAtLogin: Bool

    func changedTabs(from old: Self) -> Set<SettingsTab> {
        var tabs: Set<SettingsTab> = []
        if apps != old.apps || harnesses != old.harnesses || appChecks != old.appChecks { tabs.insert(.apps) }
        if hubRows != old.hubRows || host != old.host || hubForm != old.hubForm || hostPort != old.hostPort ||
            inviteRequestId != old.inviteRequestId || hostPortFeedback != old.hostPortFeedback || inviteFeedback != old.inviteFeedback {
            tabs.insert(.hubs)
        }
        if channels != old.channels || mountedChannels != old.mountedChannels { tabs.insert(.channels) }
        for tab in SettingsTab.allCases where pending[tab] != old.pending[tab] { tabs.insert(tab) }
        return tabs
    }
}

extension Companion {
    func settingsPresentation() -> SettingsPresentation {
        let channel = channelsPresentation()
        let mountedHub = mountedChannelsSettings?.hub
        let mounted = mountedHub.flatMap { $0 == channel.hub ? nil : channelsPresentation(hub: $0) }
        var pending: [SettingsTab: Set<String>] = [:]
        for (id, control) in pendingControls {
            guard let tab = SettingsTab.affected(by: control.op),
                  tab != .channels || control.hub == channel.hub || control.hub == mountedHub else { continue }
            pending[tab, default: []].insert(id)
        }
        return SettingsPresentation(apps: resolvedSettingsApps, harnesses: harnesses, appChecks: feedback.deadlines(for: .appCheck),
            hubRows: resolvedSettingsHubs, host: settings?.host_hub, hubForm: hubForm,
            hostPort: hostPort, inviteRequestId: hubInvite.requestId,
            hostPortFeedback: feedback.deadline(.init(.hostPort)), inviteFeedback: feedback.deadline(.init(.invite)),
            channels: channel, mountedChannels: mounted, pending: pending, banner: settingsBannerError, launchAtLogin: settings?.launch_at_login == true)
    }

    func acceptSettings(_ value: SettingsPayload, hub: String? = nil, channelSave: PendingControl? = nil, savedRequest: String? = nil, sourceOp: String? = nil, confirmHubSave: Bool = false, savedHub: String? = nil) {
        let target = settingsHub(value, hub: hub)
        for old in settings?.hubs ?? [] where !old.sharesChannelAuthority(with: value.hubs.first { $0.name == old.name }) {
            // A reused Hub name cannot carry local edits or in-flight receipts across accounts.
            if settingsActiveTab == "channels", mountedChannelsSettings?.hub == old.name { settingsPanel?.makeFirstResponder(nil) }
            for channel in settingsChannels.inventory[old.name] ?? [] {
                let key = channelEditorKey(hub: old.name, principal: old.user ?? "local", channel: channel.id)
                feedback.expire(channelSaveFeedbackKey(key, scope: "hub"))
            }
            for key in settingsChannels.editors.keys.filter({ $0.hasPrefix(old.name + "|") }) {
                settingsChannels.editors.removeValue(forKey: key)
                feedback.expire(.init(.channelPair, key))
                feedback.expire(channelSaveFeedbackKey(key, scope: "user"))
            }
            settingsChannels.inventory.removeValue(forKey: old.name)
            settingsChannels.errors.removeValue(forKey: old.name)
            pendingControls = pendingControls.filter { $0.value.hub != old.name || !Self.channelControlOps.contains($0.value.op) }
        }
        settings = value
        if !settingsChannels.selectedHub.isEmpty && !value.hubs.contains(where: { $0.name == settingsChannels.selectedHub && $0.enabled }) {
            settingsChannels.selectedHub = value.hubs.first(where: { $0.enabled })?.name ?? ""
            if settingsVisible && !settingsChannels.selectedHub.isEmpty { emit(["op": "open_settings", "hub": settingsChannels.selectedHub], userInitiated:false) }
        }
        reconcileHubSettings(value, savedRequest: savedRequest, confirmHubSave: confirmHubSave, savedHub: savedHub)
        reconcileChannelSettings(value, target: target, channelSave: channelSave, sourceOp: sourceOp)
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
            else if settingsActiveTab == "channels", let mounted = mountedChannelsSettings { cancelChannelEditing(in: mounted) }
            else { closeSettingsPanel() }
        case "return":
            if let focused = settingsPanel?.firstResponder as? MenuControl {
                if !focused.isHiddenOrHasHiddenAncestor { focused.performClick(nil) }
                return
            }
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

    private var settingsContentTop: CGFloat { settingsBannerError == nil ? 412 : 364 }

    private func makeSettingsScroll(in root: NSView) -> NSScrollView {
        let scroll = SettingsScrollView(frame: NSRect(x: 0, y: 44, width: root.bounds.width, height: settingsContentTop - 44))
        scroll.autoresizingMask = [.width]
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        root.addSubview(scroll)
        return scroll
    }

    private func settingsScrollKey(for content: NSView?) -> String {
        let id = content?.identifier?.rawValue ?? ""
        if let channels = content as? ChannelsSettingsView { return "\(id):\(channels.hub)" }
        return id
    }

    func mountSettingsCards(_ content: NSView, on scroll: NSScrollView) {
        let id = content.identifier?.rawValue ?? ""
        if let channels = content as? ChannelsSettingsView {
            for child in channels.subviews { child.autoresizingMask.insert(.minYMargin) }
            channels.setFrameSize(NSSize(width: scroll.contentSize.width, height: channels.documentHeight(for: scroll.contentSize.height)))
        } else {
            let minY = content.subviews.map { $0.frame.minY }.min() ?? 8
            let extra = max(0, 8 - minY)
            for child in content.subviews { child.frame.origin.y += extra }
            content.frame = NSRect(x: 0, y: 0, width: scroll.contentSize.width, height: content.frame.height + extra)
        }
        scroll.identifier = NSUserInterfaceItemIdentifier(id)
        scroll.isHidden = content.isHidden
        content.isHidden = false
        scroll.documentView = content
        let top = max(0, content.frame.height - scroll.contentSize.height)
        scroll.contentView.scroll(to: NSPoint(x: 0, y: top - min(top, settingsScrollOffsets[settingsScrollKey(for: content)] ?? 0)))
        scroll.reflectScrolledClipView(scroll.contentView)
    }

    func flushDeferredSettings() {
        guard settingsRebuildDeferred else { return }
        rebuildSettingsPanel(preservingEditor: true, syncFields: false, tabs: settingsDeferredTabs)
    }

    func rebuildSettingsPanel(preservingEditor: Bool? = nil, revealAddHub: Bool = false, revealChannel: String? = nil, revealPairing: Bool = false, revealEditorScope: String = "hub", syncFields: Bool = true, scrollerStyle: NSScroller.Style? = nil, tabs: Set<SettingsTab>? = nil) {
        if let scrollerStyle {
            // A style notification changes geometry, not the mounted Settings state.
            for case let scroll as SettingsScrollView in settingsPanel?.contentView?.subviews ?? [] {
                scroll.scrollerStyle = scrollerStyle
                scroll.tile()
            }
            return
        }
        guard settingsVisible || renderingForInspection, let window = settingsPanel, let root = window.contentView else {
            settingsDirty = true
            return
        }
        let allTabs = Set(SettingsTab.allCases)
        let full = tabs == nil || settingsDirty
        var targets = full ? allTabs : tabs ?? []
        let active = SettingsTab(rawValue: settingsActiveTab)
        let focusedField = settingsFieldControls.values.first { $0.currentEditor() != nil && $0.currentEditor() === window.firstResponder }
        let channelFocus = active == .channels && targets.contains(.channels) ? mountedChannelsSettings?.settlementFocus : nil
        let hubFocus = active == .hubs && targets.contains(.hubs) ? hubSettlementFocus : nil
        let settlementFocus = channelFocus ?? hubFocus
        if settlementFocus != nil, focusedField != nil { window.makeFirstResponder(nil) }
        // The emitter's tab-only channel repaint is a pending-state update,
        // not an instruction to replace a live field editor.
        let preserve = (preservingEditor ?? (tabs == [.channels])) || (!syncFields && tabs != nil)
        if preserve, settlementFocus == nil, let active, targets.contains(active), focusedField != nil {
            settingsDeferredTabs.insert(active)
            targets.remove(active)
            if active == .hubs {
                refreshMountedHubFormFeedback()
                refreshMountedHostPortFeedback()
            } else if active == .channels {
                mountedChannelsSettings?.refreshMountedActions()
            }
        }
        settingsDirty = false
        settingsDeferredTabs.subtract(targets)
        let replaceChrome = full && targets == allTabs
        let stripIDs = ["settings_tab_track", "channel_hub_strip", "host_hub_addr_track", "settings_hub_auth_track"]
        let focusedSegment = window.firstResponder as? MenuControl
        let handoffs = Dictionary(uniqueKeysWithValues: stripIDs.compactMap { id -> (String, SegmentedPillStrip.Handoff)? in
            let strip: SegmentedPillStrip? = findView(in: root, id: id)
            return strip.map { (id, $0.capture()) }
        })
        let replacingEditor = active.map { targets.contains($0) } == true
        let focused = replacingEditor ? settingsFieldControls.first { $0.value.currentEditor() != nil && $0.value.currentEditor() === window.firstResponder } : nil
        let focusedId = focused?.key
        let previousChannelHub = mountedChannelsSettings?.hub
        let selectedRange = (focused?.value.currentEditor() as? NSTextView)?.selectedRange()
        if syncFields && targets.contains(.hubs) { syncAddHubFieldsFromControls() }
        for case let scroll as SettingsScrollView in root.subviews {
            guard let tab = SettingsTab.allCases.first(where: { $0.scrollId == scroll.identifier?.rawValue }), targets.contains(tab) else { continue }
            settingsScrollOffsets[settingsScrollKey(for: scroll.documentView)] = max(0, (scroll.documentView?.frame.height ?? 0) - scroll.contentSize.height - scroll.contentView.bounds.origin.y)
            MenuControl.clearHoverWhere { $0.isDescendant(of: scroll) }
            settingsFieldControls = settingsFieldControls.filter { !$0.value.isDescendant(of: scroll) }
        }
        if replaceChrome {
            MenuControl.clearHoverWhere { $0.window === window }
            root.subviews.filter { !($0 is NSVisualEffectView) }.forEach { $0.removeFromSuperview() }
        }

        let header: NSView? = findView(in: root, id: "settings_header")
        if header == nil {
            mountSettingsChrome(in: root, handoff: handoffs["settings_tab_track"])
        }
        let chromeChanged = updateSettingsChrome(in: root)
        if !targets.isEmpty || chromeChanged { settingsRebuildCount += 1 }
        let renderedHub = effectiveChannelHub
        if targets.contains(.channels) {
            if let oldBar: NSView = findView(in: root, id: "settings_channel_hub_bar") {
                MenuControl.clearHoverWhere { $0.isDescendant(of: oldBar) }
                oldBar.removeFromSuperview()
            }
            if active == .channels {
                mountChannelHubBar(in: root, top: settingsContentTop, renderedHub: renderedHub, handoff: handoffs["channel_hub_strip"])
            }
        }
        let hubBar: NSView? = findView(in: root, id: "settings_channel_hub_bar")
        hubBar?.frame.origin.y = settingsContentTop - CitoMetrics.settingsHubBarHeight
        let channelTop = hubBar?.frame.minY ?? settingsContentTop
        for tab in SettingsTab.allCases where targets.contains(tab) {
            let scroll: NSScrollView
            if let existing = root.subviews.compactMap({ $0 as? SettingsScrollView }).first(where: { $0.identifier?.rawValue == tab.scrollId }) {
                scroll = existing
            } else {
                scroll = makeSettingsScroll(in: root)
            }
            layoutSettingsScroll(scroll, top: tab == .channels ? channelTop : settingsContentTop)
            switch tab {
            case .apps: buildAppsTab(in: scroll)
            case .channels: buildChannelsTab(in: scroll, renderedHub: renderedHub)
            case .hubs: buildHubsTab(in: scroll, handoffs: handoffs)
            }
        }
        if chromeChanged {
            for case let scroll as SettingsScrollView in root.subviews {
                layoutSettingsScroll(scroll, top: scroll.identifier?.rawValue == SettingsTab.channels.scrollId ? channelTop : settingsContentTop)
            }
        }
        if !targets.isEmpty || chromeChanged { window.recalculateKeyViewLoop() }
        window.invalidateShadow()
        if let settlementFocus {
            for id in settlementFocus {
                if let control: MenuControl = findView(in: root, id: id), !control.isHiddenOrHasHiddenAncestor, control.isEnabled {
                    control.scrollToVisible(control.bounds)
                    if window.makeFirstResponder(control) { break }
                }
            }
        } else if let id = focusedId, let field = settingsFieldControls[id],
           !(id.hasPrefix("channel_field:") || id.hasPrefix("pairing_instruction:")) || previousChannelHub == renderedHub {
            window.makeFirstResponder(field)
            if let range = selectedRange, let editor = field.currentEditor() as? NSTextView { editor.setSelectedRange(range) }
        } else if let focusedSegment, focusedSegment.segmentArrow != nil, focusedSegment.window !== window,
                  let id = focusedSegment.identifier?.rawValue, let pill: MenuControl = findView(in: root, id: id),
                  pill.visibleRect.width > 0 && !pill.isHiddenOrHasHiddenAncestor {
            window.makeFirstResponder(pill)
        }
        if targets.contains(.hubs) && revealAddHub && hubForm.isOpen,
           let scroll: NSScrollView = findView(in: root, id: "settings_hubs_tab"),
           let form: NSView = findView(in: scroll, id: "add_hub_form") {
            form.scrollToVisible(NSRect(x: 0, y: 0, width: form.bounds.width, height: 44))
            scroll.reflectScrolledClipView(scroll.contentView)
        }
        if targets.contains(.channels), let channel = revealChannel,
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


    private func layoutSettingsScroll(_ scroll: NSScrollView, top: CGFloat) {
        let height = top - scroll.frame.minY
        guard scroll.frame.height != height else { return }
        // Showing or dismissing a banner changes the viewport's top edge. Keep
        // the same document origin (and mounted editor position) where possible.
        let origin = scroll.contentView.bounds.origin
        scroll.setFrameSize(NSSize(width: scroll.frame.width, height: height))
        let maxY = max(0, (scroll.documentView?.frame.height ?? 0) - scroll.contentSize.height)
        scroll.contentView.scroll(to: NSPoint(x: origin.x, y: min(maxY, max(0, origin.y))))
        scroll.reflectScrolledClipView(scroll.contentView)
    }

    private func mountSettingsChrome(in root: NSView, handoff: SegmentedPillStrip.Handoff?) {
        // Header & Segmented Tab Bar (520 × 48 pt, y: 412)
        let header = NSView(frame: NSRect(x: 0, y: 412, width: 520, height: 48))
        header.identifier = NSUserInterfaceItemIdentifier("settings_header")

        let tabPadding: CGFloat = 8
        let options = [(SettingsTab.apps, "Agent Apps", "square.grid.2x2"),
                       (.channels, "Human Channels", "bubble.left.and.bubble.right"),
                       (.hubs, "Hubs & Network", "network")]
        let pills = options.map { tab, title, symbol in
            let width = MenuControl.intrinsicWidth(title, leadingSymbol: symbol, paddingX: tabPadding)
            let pill = MenuControl(title, frame: NSRect(x: 0, y: 0, width: width, height: 24), leadingSymbol: symbol, paddingX: tabPadding)
            pill.identifier = NSUserInterfaceItemIdentifier("settings_tab:\(tab.rawValue)")
            pill.pressed = { [weak self] in
                guard let self, settingsActiveTab != tab.rawValue else { return }
                settingsActiveTab = tab.rawValue
                if tab == .channels { refreshChannelSettingsIfNeeded() }
                rebuildSettingsPanel()
            }
            return pill
        }
        let width = PillTrackGeometry.trailingWidth(widths: pills.map { $0.frame.width }, spacing: 4)
        root.addSubview(header)
        mountSettingsPills(in: header, frame: NSRect(x: max(78, floor((520 - width) / 2)), y: 10, width: width, height: 28),
            id: "settings_tab_track", geometry: PillTrackGeometry(pillHeight: 24, pillRadius: 7), pills: pills,
            selected: pills.first { $0.identifier?.rawValue == "settings_tab:\(settingsActiveTab)" }, handoff: handoff)
        header.addSubview(roundedView(NSRect(x: 0, y: 0, width: 520, height: 1), radius: 0, color: .labelColor.withAppearanceAlpha(0.08)))
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

    }

    private func updateSettingsChrome(in root: NSView) -> Bool {
        if let track: SegmentedPillStrip = findView(in: root, id: "settings_tab_track"),
           let selected: MenuControl = findView(in: track, id: "settings_tab:\(settingsActiveTab)") {
            track.select(selected)
        }
        let login: NSButton? = findView(in: root, id: "settings_launch_at_login")
        let state: NSControl.StateValue = settings?.launch_at_login == true ? .on : .off
        let loginChanged = login?.state != state
        if loginChanged { login?.state = state }
        let existing = root.subviews.first { $0.identifier?.rawValue == "settings_error_banner" }
        let label: NSTextField? = existing.flatMap { findView(in: $0, id: "settings_error_label") }
        let message = settingsBannerError
        guard label?.stringValue != message else { return loginChanged }
        existing?.removeFromSuperview()
        if let message {
            let banner=roundedView(NSRect(x:CitoMetrics.settingsInset,y:368,width:480,height:40),radius:6,color:CitoPalette.dangerText.withAppearanceAlpha(0.10))
            banner.identifier=NSUserInterfaceItemIdentifier("settings_error_banner")
            let label=textLabel(message,NSRect(x:10,y:7,width:432,height:26),size:10,color:CitoPalette.dangerText)
            label.maximumNumberOfLines = 2; label.lineBreakMode = .byWordWrapping
            label.cell?.truncatesLastVisibleLine = true; label.toolTip = label.stringValue
            label.identifier=NSUserInterfaceItemIdentifier("settings_error_label"); banner.addSubview(label)
            let dismiss=MenuControl("Dismiss",frame:NSRect(x:450,y:10,width:22,height:20),symbol:"xmark")
            dismiss.identifier=NSUserInterfaceItemIdentifier("dismiss_settings_error")
            dismiss.style = .subtleHeader
            dismiss.pressed={ [weak self] in self?.settingsBannerError=nil; self?.rebuildSettingsPanel(tabs: []) }
            banner.addSubview(dismiss); root.addSubview(banner)
        }
        return true
    }

    @objc private func toggleLaunchAtLogin(_ sender: NSButton) {
        let enabled = sender.state == .on
        // The settings receipt, not the local click, owns the committed value.
        sender.state = settings?.launch_at_login == true ? .on : .off
        emit(["op": "set_launch_at_login", "enabled": enabled])
    }

    func openSettingsPanel(tab: String? = nil, channelHub: String? = nil) {
        ensureSettingsPanel()
        let previousTab = settingsActiveTab
        let previousHub = effectiveChannelHub
        if let channelHub { settingsChannels.selectedHub = channelHub }
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
        settingsChannels.subscribedHub = ""
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

}
