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
        if let document = documentView, document.frame.width != contentSize.width {
            document.setFrameSize(NSSize(width: contentSize.width, height: document.frame.height))
        }
    }
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
    var invite: Companion.HubInviteState
    var channels: ChannelsPresentation
    var pending: [SettingsTab: Set<String>]
    var banner: String?
    var launchAtLogin: Bool

    func changedTabs(from old: Self) -> Set<SettingsTab> {
        var tabs: Set<SettingsTab> = []
        if apps != old.apps || harnesses != old.harnesses || appChecks != old.appChecks { tabs.insert(.apps) }
        if hubRows != old.hubRows || host != old.host || hubForm != old.hubForm || hostPort != old.hostPort || invite != old.invite {
            tabs.insert(.hubs)
        }
        if channels != old.channels { tabs.insert(.channels) }
        for tab in SettingsTab.allCases where pending[tab] != old.pending[tab] { tabs.insert(tab) }
        return tabs
    }
}

extension Companion {
    func settingsPresentation() -> SettingsPresentation {
        let channel = channelsPresentation()
        var pending: [SettingsTab: Set<String>] = [:]
        for (id, control) in pendingControls {
            guard let tab = SettingsTab.affected(by: control.op), tab != .channels || control.hub == channel.hub else { continue }
            pending[tab, default: []].insert(id)
        }
        return SettingsPresentation(apps: resolvedSettingsApps, harnesses: harnesses, appChecks: settingsApps.checkedUntil,
            hubRows: resolvedSettingsHubs, host: settings?.host_hub, hubForm: hubForm, hostPort: hostPort, invite: hubInvite,
            channels: channel, pending: pending, banner: settingsBannerError, launchAtLogin: settings?.launch_at_login == true)
    }

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
    func rebuildFeedbackAfter(_ seconds: Double, tab: SettingsTab) {
        RunLoop.main.add(Timer(timeInterval: seconds, repeats: false) { [weak self] _ in
            self?.rebuildSettingsPanel(preservingEditor: true, tabs: [tab])
        }, forMode: .common)
    }

    func acceptSettings(_ value: SettingsPayload, hub: String? = nil, channelSave: PendingControl? = nil, savedRequest: String? = nil, sourceOp: String? = nil, confirmHubSave: Bool = false, savedHub: String? = nil, truncated: Bool = false) {
        let target = settingsHub(value, hub: hub)
        for old in settings?.hubs ?? [] where !old.sharesChannelAuthority(with: value.hubs.first { $0.name == old.name }) {
            // A reused Hub name cannot carry local edits or in-flight receipts across accounts.
            if settingsActiveTab == "channels", mountedChannelsSettings?.hub == old.name { settingsPanel?.makeFirstResponder(nil) }
            settingsChannels.editors = settingsChannels.editors.filter { !$0.key.hasPrefix(old.name + "|") }
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
        reconcileChannelSettings(value, target: target, channelSave: channelSave, sourceOp: sourceOp, truncated: truncated)
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
                let editingGateway = channels.filter { settingsChannels.editors["\(hub)|\($0.id)"]?.editingGateway == true }
                let editing = editingGateway.isEmpty ? channels.filter { settingsChannels.editors["\(hub)|\($0.id)"]?.editingPersonal == true } : editingGateway
                guard !editing.isEmpty else { closeSettingsPanel(); return }
                for channel in editing {
                    let key = "\(hub)|\(channel.id)"
                    clearChannelDrafts(channel, scope: editingGateway.isEmpty ? "user" : "hub", hub: hub)
                    if editingGateway.isEmpty { settingsChannels.editors[key]?.resetPersonal() }
                    else { settingsChannels.editors[key]?.resetGateway() }
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

    func flushDeferredSettings() {
        guard settingsRebuildDeferred else { return }
        rebuildSettingsPanel(preservingEditor: true, syncFields: false, tabs: settingsDeferredTabs)
    }

    func rebuildSettingsPanel(preservingEditor: Bool = false, revealAddHub: Bool = false, revealChannel: String? = nil, revealPairing: Bool = false, revealEditorScope: String = "hub", syncFields: Bool = true, scrollerStyle: NSScroller.Style? = nil, tabs: Set<SettingsTab>? = nil) {
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
        if preservingEditor, let active = SettingsTab(rawValue: settingsActiveTab), targets.contains(active),
           settingsFieldControls.values.contains(where: { $0.currentEditor() != nil && $0.currentEditor() === window.firstResponder }) {
            settingsDeferredTabs.insert(active)
            targets.remove(active)
            if active == .hubs { refreshMountedHubFormFeedback() }
        }
        settingsDirty = false
        settingsDeferredTabs.subtract(targets)
        let replaceChrome = full && targets == allTabs
        let active = SettingsTab(rawValue: settingsActiveTab)
        let replacingEditor = active.map { targets.contains($0) } == true
        let focused = replacingEditor ? settingsFieldControls.first { $0.value.currentEditor() != nil && $0.value.currentEditor() === window.firstResponder } : nil
        let focusedId = focused?.key
        let previousChannelHub = mountedChannelsSettings?.hub
        let selectedRange = (focused?.value.currentEditor() as? NSTextView)?.selectedRange()
        if syncFields && targets.contains(.hubs) { syncAddHubFieldsFromControls() }
        for case let scroll as SettingsScrollView in root.subviews {
            guard let tab = SettingsTab.allCases.first(where: { $0.scrollId == scroll.identifier?.rawValue }), targets.contains(tab) else { continue }
            settingsScrollOffsets[tab.scrollId] = max(0, (scroll.documentView?.frame.height ?? 0) - scroll.contentSize.height - scroll.contentView.bounds.origin.y)
            MenuControl.clearHoverWhere { $0.isDescendant(of: scroll) }
            settingsFieldControls = settingsFieldControls.filter { !$0.value.isDescendant(of: scroll) }
        }
        if replaceChrome {
            MenuControl.clearHoverWhere { $0.window === window }
            root.subviews.filter { !($0 is NSVisualEffectView) }.forEach { $0.removeFromSuperview() }
        }

        if replaceChrome { mountSettingsChrome(in: root) }
        let chromeChanged = updateSettingsChrome(in: root)
        if !targets.isEmpty || chromeChanged { settingsRebuildCount += 1 }
        let renderedHub = effectiveChannelHub
        for tab in SettingsTab.allCases where targets.contains(tab) {
            let scroll = root.subviews.compactMap { $0 as? SettingsScrollView }.first { $0.identifier?.rawValue == tab.scrollId }
                ?? makeSettingsScroll(in: root)
            switch tab {
            case .apps: buildAppsTab(in: scroll)
            case .channels: buildChannelsTab(in: scroll, renderedHub: renderedHub)
            case .hubs: buildHubsTab(in: scroll)
            }
        }

        window.invalidateShadow()
        if let id = focusedId, let field = settingsFieldControls[id],
           !(id.hasPrefix("channel_field:") || id.hasPrefix("pairing_instruction:")) || previousChannelHub == renderedHub {
            window.makeFirstResponder(field)
            if let range = selectedRange, let editor = field.currentEditor() as? NSTextView { editor.setSelectedRange(range) }
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


    private func mountSettingsChrome(in root: NSView) {
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
        let login: NSButton? = findView(in: root, id: "settings_launch_at_login")
        let state: NSControl.StateValue = settings?.launch_at_login == true ? .on : .off
        let loginChanged = login?.state != state
        if loginChanged { login?.state = state }
        let existing = root.subviews.first { $0.identifier?.rawValue == "settings_error_banner" }
        let label: NSTextField? = existing.flatMap { findView(in: $0, id: "settings_error_label") }
        let message = settingsBannerError
        guard label?.stringValue != message else { return loginChanged }
        existing?.removeFromSuperview()
        for case let scroll as SettingsScrollView in root.subviews {
            scroll.setFrameSize(NSSize(width: root.bounds.width, height: message == nil ? 368 : 320))
        }
        if let message {
            let banner=roundedView(NSRect(x:CitoMetrics.settingsInset,y:368,width:480,height:40),radius:6,color:CitoPalette.dangerText.withAppearanceAlpha(0.10))
            banner.identifier=NSUserInterfaceItemIdentifier("settings_error_banner")
            let label=textLabel(message,NSRect(x:10,y:7,width:432,height:26),size:10,color:CitoPalette.dangerText)
            label.maximumNumberOfLines = 2; label.lineBreakMode = .byWordWrapping; label.cell?.wraps = true; label.toolTip = label.stringValue
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
