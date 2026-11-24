import AppKit
import Darwin

final class Companion: NSObject, NSSearchFieldDelegate, NSTextFieldDelegate {
    let headless = ProcessInfo.processInfo.environment["CITO_UI_HEADLESS"] == "1"
    var status: NSStatusItem?
    var headlessStatusButton: NSButton?
    var hoverEventMonitor: Any?
    let cardMenu: NSMenu = ProcessInfo.processInfo.environment["CITO_UI_HEADLESS"] == "1" ? HeadlessMenu() : NSMenu()
    var hubDrawerVisibility: [String: Bool] = [:]
    var menuScrollOffsets: [String: NSPoint] = [:]
    var menuScrollKeys: [String: String] = [:]
    var menuScrollAnchors: [String: (rowId: String, deltaY: CGFloat)] = [:]
    var statusImage: NSImage?
    static let statusIcons: [StatusIconState: NSImage] = [
        .idle: makeCitoStatusIcon(.idle), .active: makeCitoStatusIcon(.active), .warning: makeCitoStatusIcon(.warning)
    ]
    var lastStatusPresentation: (icon: StatusIconState, unread: Int, tooltip: String)?
    var statusUpdateCount = 0
    var menuOpen = false
    var menuDirty = true
    var directoryDirty = true
    var settingsDirty = true
    var settingsRebuildCount = 0
    var directoryReloadCount = 0
    // Inspection flushes views without opening surfaces or emitting visibility IPC.
    var renderingForInspection = false
    var testAppearance: NSAppearance?
    var hubs: [Hub] = []
    var listeners: [Listener] = []
    var available: [Candidate] = []
    var discovery: [DiscoveryState] = []
    var myRemote: [RemotePeer] = []
    var harnesses: [HarnessMeta] = []
    var harnessMap: [String: HarnessMeta] { Dictionary(uniqueKeysWithValues: harnesses.map { ($0.id, $0) }) }
    var selectedTargetHub: String = ""
    var renamingSessionKey: String?
    weak var renameField: NSTextField?
    var myRemoteCollapsed = false
    var collapsedGroups = Set<String>()
    var autoCollapsedRecentGroups = Set<String>()
    var directoryWarnings: [String] = []
    var directoryQueryError: String?
    var directoryActionError: String?
    var directoryActionErrorSourceOp: String?
    var directoryActionWarning: String?
    var latestDirectoryActionRequestId: String?
    let directoryActionOps = ["clear_activity_history", "ack_signals", "copy_activity_message"]
    var directoryWarningLabel: NSTextField?
    var directoryRequestId = ""
    var composerDraftIdempotencyKey = UUID().uuidString
    struct PendingMessageCopy {
        let requestId: String
        let hub: String
        let signalId: String
        var buffer = ""
        var nextPart = 0
    }
    var pendingMessageCopy: PendingMessageCopy?
    var activityTotal = 0
    var activityHasAny = false
    var activityUnreadCount = 0
    var serverActivityFilter: String?
    var serverActivityQuery = ""
    func copyText(_ text: String, control: MenuControl? = nil, feedback: String = "✓ Copied") {
        if let pending = pendingMessageCopy, latestDirectoryActionRequestId == pending.requestId { latestDirectoryActionRequestId = nil }
        pendingMessageCopy = nil
        clipboardString = text
        appPasteboard.clearContents()
        appPasteboard.setString(text, forType: .string)
        control?.copied(feedback)
    }
    func readPasteboardString() -> String {
        // The pasteboard remains authoritative after a copy in another application.
        let systemText = appPasteboard.string(forType: .string) ?? ""
        return systemText.isEmpty && headless ? clipboardString : systemText
    }
    var directoryOnlineCount = 0
    var directoryMatchedCount = 0
    var unreadCount = 0
    var directoryMode = "directory"
    var activityFilter = "me" { didSet { if oldValue != activityFilter { activityClearConfirm = false; clearDirectoryActionError() } } }
    var activityAllItems: [ActivityItem] = []
    var activityFilteredItems: [ActivityItem] = []
    var directoryAllPeers: [RemotePeer] = []
    var directoryFilteredPeers: [RemotePeer] = []
    var directoryCopiedUntil: [String: Date] = [:]
    var activityCopiedUntil: [String: Date] = [:]
    var settingsRebuildDeferred = false
    var menuRebuildDeferred = false
    var headlessMenuPanel: KeyPanel?
    var directoryVisible = false
    var directoryQuery = ""
    var directoryScopeFilter = "all" { didSet { if oldValue != directoryScopeFilter { clearDirectoryActionError() } } }
    var directoryHarnessFilter = "all" { didSet { if oldValue != directoryHarnessFilter { clearDirectoryActionError() } } }
    var directorySelectedIndex = 0
    var directoryPanel: KeyPanel?
    var directorySearchField: NSSearchField?
    var directoryModeDirPill: MenuControl?
    var directoryModeActPill: MenuControl?
    var directoryTableView: NSTableView?
    var directoryScrollView: NSScrollView?
    var directorySummaryLabel: NSTextField?
    var directoryShortcutsLabel: NSTextField?
    var serverDirectoryQuery: String?
    var serverDirectoryScope = "all"
    var serverDirectoryHarness = "all"
    var serverDirectoryMatchedCount = 0
    var activityClearConfirm = false
    var directoryFacetBar: NSView?
    var directoryDynamicHarnessViews: [NSView] = []
    var lastDirectoryFacetSignature: [(id: String, display_name: String)]?
    var directoryFacetControls: [String: MenuControl] = [:]
    var directoryComposerBar: NSView?
    var composerTargetPill: SurfaceView?
    var composerTargetLabel: NSTextField?
    var composerSendButton: MenuControl?
    var composerError = ""
    var composerInputField: NSTextField?
    var composerTarget = ""
    var composerTargetHub = ""
    var composerReplySignalId: String?
    var composerErrorLabel: NSTextField?
    struct PendingComposerSend {
        let composerDraftIdempotencyKey: String
        let text: String
        let requestId: String
        let target: String
        let hub: String
        let replySignalId: String?
    }
    var pendingComposer: PendingComposerSend? { didSet {
        composerSendButton?.feedbackTimer?.invalidate(); composerSendButton?.feedbackTimer=nil
        composerSendButton?.isEnabled = pendingComposer == nil
        composerSendButton?.label.stringValue = pendingComposer == nil ? "Send ⏎" : "Sending…"
        composerSendButton?.toolTip = pendingComposer == nil ? "Send message (Return)" : "Sending message…"
        composerSendButton?.setAccessibilityLabel(composerSendButton?.toolTip)
        if let send = composerSendButton { send.frame.size.width = max(MenuControl.intrinsicWidth("Sending…"), MenuControl.intrinsicWidth("Send ⏎")) }
        layoutComposer()
        composerSendButton?.layoutContents()
    } }
    var settings: SettingsPayload?
    var settingsVisible = false
    var settingsActiveTab = "apps" { didSet {
        if oldValue != settingsActiveTab { settingsBannerError = nil; hostPort.error = nil }
    } }
    var settingsPanel: KeyPanel?
    struct PendingHubSave {
        let profile: HubFormProfile
        let requestId: String
        let generation: Int
    }
    struct HubFormState {
        enum Mode {
            case closed
            case add, addWithIncompleteInventory
            case edit(HubFormProfile)
        }

        var mode: Mode
        var isOpen: Bool { if case .closed = mode { return false }; return true }
        var isEditing: Bool { if case .edit = mode { return true }; return false }
        var originalName: String { initialHub?.name ?? "" }
        var initialHub: HubFormProfile? { if case let .edit(hub) = mode { return hub }; return nil }
        var error: String?
        var parseRequest = ""
        var pendingSave: PendingHubSave?
        var generation = 0
        var name = ""
        var url = ""
        var auth = "none"
        var authExplicit = false
        var token = ""
        var user = ""
        var issuer = ""
        var clientId = ""
        var redirectPort = ""

        init(mode: Mode = .closed) { self.mode = mode }
        init(editing hub: HubConfigItem) {
            mode = .edit(hub.formProfile)
            name = hub.name
            url = hub.url
            auth = hub.auth_mode
            authExplicit = true
            user = hub.user ?? ""
            issuer = hub.issuer ?? ""
            clientId = hub.client_id ?? ""
            redirectPort = hub.redirect_port.map(String.init) ?? ""
        }
    }
    var hubForm = HubFormState()
    struct HostHubPortState {
        var pending = false
        var savedUntil: Date?
        var error: String?
        var draft: String?
        var submittedPort: Int?
        var requestId = ""
    }
    var hostPort = HostHubPortState()
    var settingsBannerError: String?
    var menuBannerError: String?
    var hubInvitePending: Bool { !hubInviteRequest.isEmpty }
    var hubInviteRequest = ""
    var hubInvitePastedUntil = Date.distantPast
    var settingsFieldControls: [String: NSTextField] = [:]
    var channelUiStates: [String: ChannelUiState] = [:]
    var appCheckedUntil: [String: Date] = [:]
    static let channelResetOps = ["channel_cancel_pair", "channel_unpair", "disconnect_human_channel"]
    static let channelSaveOps = ["channel_save", "save_hub_service"]
    static let channelControlOps = channelResetOps + channelSaveOps + ["channel_pair", "channel_test", "channel_toggle"]
    struct PendingControl {
        struct ChannelSave {
            let scope: String
            let values: [String: String]
        }

        let op: String, hub: String, target: String
        let appCheck: Bool
        // A superseded save still owns its control receipt, but cannot reconcile drafts.
        var channelSave: ChannelSave?
        var channelReset: (hub: Bool, user: Bool, submitted: ChannelUiState)?
    }
    var pendingControls: [String: PendingControl] = [:]
    func isPending(_ op: String, hub: String = "", target: String = "") -> Bool {
        pendingControls.values.contains { $0.op == op && $0.hub == hub && $0.target == target }
    }
    var hostHubAddress = "tailscale"
    var selectedChannelHub = ""
    var channelSettingsByHub: [String: [ChannelSetupItem]] = [:]
    var channelInventoryErrorsByHub: [String: String] = [:]
    var settingsScrollOffsets: [String: CGFloat] = [:]
    var pendingOnboardingMarker: String?
    var settingsKeyMonitor: Any?
    var uninstallConfirmOpen = false
    var uninstallPurgeData = false
    var hasCheckedFirstLaunchOnboarding = false
    var clipboardString = ""
    var hasRenderedState = false
    var pendingMutation = false
    var rebuildCount = 0

    func ensureEditMainMenu() {
        let main = NSApp.mainMenu ?? NSMenu()
        if main.items.isEmpty {
            let appItem = NSMenuItem()
            let appMenu = NSMenu(title: "Cito")
            let quit = NSMenuItem(title: "Quit Cito", action: #selector(quitMenu), keyEquivalent: "q")
            quit.target = self; appMenu.addItem(quit)
            appItem.submenu = appMenu; main.addItem(appItem)
        }
        if !main.items.contains(where: { $0.submenu?.title == "Edit" }) {
            let item = NSMenuItem(title: "Edit", action: nil, keyEquivalent: "")
            let edit = NSMenu(title: "Edit")
            for (title, action, key) in [("Undo", "undo:", "z"), ("Redo", "redo:", "Z"),
                ("Cut", "cut:", "x"), ("Copy", "copy:", "c"), ("Paste", "paste:", "v"), ("Select All", "selectAll:", "a")] {
                let command = NSMenuItem(title: title, action: NSSelectorFromString(action), keyEquivalent: key)
                command.keyEquivalentModifierMask = title == "Redo" ? [.command, .shift] : [.command]
                edit.addItem(command)
            }
            item.submenu = edit; main.addItem(item)
        }
        if NSApp.mainMenu == nil { NSApp.mainMenu = main }
    }

    func handleTextEditShortcut(_ key: String, in window: NSWindow?) -> Bool {
        guard !(uninstallConfirmOpen && window === settingsPanel),
              let editor = window?.firstResponder as? NSTextView else { return false }
        let key = key.lowercased()
        guard !editor.hasMarkedText(), editor.isEditable || (editor.isSelectable && ["command-a", "command-c"].contains(key)) else { return false }
        let field = editor.delegate as? NSTextField
        func changeText(_ change: () -> Void) {
            editor.allowsUndo = true
            editor.breakUndoCoalescing()
            let undo = editor.undoManager
            let groupsByEvent = undo?.groupsByEvent ?? true
            // Own only this gesture's group; never close AppKit's event group directly.
            undo?.groupsByEvent = false
            undo?.beginUndoGrouping()
            defer {
                editor.breakUndoCoalescing()
                undo?.endUndoGrouping()
                undo?.groupsByEvent = groupsByEvent
            }
            change()
        }
        switch key {
        case "command-v":
            let text = readPasteboardString()
            clipboardString = text
            changeText { editor.insertText(text, replacementRange: editor.selectedRange()) }
        case "command-a": editor.selectAll(nil); return true
        case "command-c", "command-x":
            guard !(field is NSSecureTextField) else { return true }
            let range = editor.selectedRange()
            guard range.length > 0 else { return true }
            copyText((editor.string as NSString).substring(with: range))
            if key == "command-c" { return true }
            changeText {
                if editor.shouldChangeText(in: range, replacementString: "") {
                    editor.replaceCharacters(in: range, with: "")
                    editor.didChangeText()
                }
            }
        case "command-z": editor.breakUndoCoalescing(); editor.undoManager?.undo()
        case "shift-command-z": editor.breakUndoCoalescing(); editor.undoManager?.redo()
        default: return false
        }
        if let field = field, field.stringValue != editor.string {
            field.validateEditing()
            field.delegate?.controlTextDidChange?(Notification(name: NSControl.textDidChangeNotification, object: field))
        }
        return true
    }

    func start() {
        ensureEditMainMenu()
        signal(SIGPIPE, SIG_IGN)
        cardMenu.autoenablesItems = false
        cardMenu.delegate = self
        settingsKeyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self = self else { return event }
            let window = event.window ?? NSApp.keyWindow
            if self.uninstallConfirmOpen && window === self.settingsPanel && event.keyCode == 48 { return nil }
            if (window?.firstResponder as? NSTextInputClient)?.hasMarkedText() == true { return event }
            if let key = panelShortcutKey(event), self.handleTextEditShortcut(key, in: window) { return nil }
            // Text Services must see navigation and commit keys before our shortcuts.
            if window?.firstResponder is NSTextView && [126,125,36,76,53,51,117].contains(Int(event.keyCode)) { return event }
            let command = event.modifierFlags.intersection([.command, .shift, .option, .control]) == [.command]
            if command && event.charactersIgnoringModifiers == "," { self.openSettingsShortcut(); return nil }
            if command && event.charactersIgnoringModifiers == "k" { self.openDirectoryShortcut(); return nil }
            let key: String?
            if command && event.charactersIgnoringModifiers == "w" { key = "command-w" }
            else if [36,76].contains(Int(event.keyCode)) && event.modifierFlags.contains(.shift) { key="shift-return" }
            else { key = [53:"escape",126:"up",125:"down",36:"return",76:"return",51:"delete",117:"delete"][Int(event.keyCode)] }
            if let key = key {
                if self.directoryVisible && window === self.directoryPanel { self.handleDirectoryKey(key); return nil }
                if self.settingsVisible && window === self.settingsPanel { self.handleSettingsKey(key); return nil }
            }
            return event
        }
        MenuControl.tracksPhysicalPointer = !headless
        if headless { headlessStatusButton = NSButton(frame: .zero) }
        else {
            status = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
            status?.menu = cardMenu
            hoverEventMonitor = NSEvent.addLocalMonitorForEvents(matching: [.mouseMoved, .leftMouseDragged, .scrollWheel, .mouseExited, .leftMouseDown, .rightMouseDown]) { event in
                MenuControl.reconcileActiveHover()
                return event
            }
        }
        updateStatusItem()
        DispatchQueue.global().async { [weak self] in
            var buffer = Data()
            var dropping = false
            var bytes = [UInt8](repeating: 0, count: 8192)
            while true {
                let n = Darwin.read(STDIN_FILENO, &bytes, bytes.count)
                if n < 0 && errno == EINTR { continue }
                if n <= 0 { exit(0) }
                for byte in bytes.prefix(n) {
                    if byte == 10 {
                        let line = buffer; buffer.removeAll(keepingCapacity: true)
                        if dropping { dropping = false; continue }
                        RunLoop.main.perform(inModes: [.common, .eventTracking]) { [weak self] in self?.receive(line) }
                    } else if !dropping {
                        buffer.append(byte)
                        if buffer.count > 1_048_576 { diagnostic("oversized frame"); buffer.removeAll(); dropping = true }
                    }
                }
            }
        }
    }
    func findView<T: NSView>(in view: NSView, id: String, as type: T.Type = T.self) -> T? {
        guard !view.isHidden else { return nil }
        if view.identifier?.rawValue == id, let match = view as? T { return match }
        if let table = view as? NSTableView {
            for row in 0..<min(table.numberOfRows, 100) {
                if let cell = table.view(atColumn: 0, row: row, makeIfNecessary: true),
                   let match = findView(in: cell, id: id, as: type) { return match }
            }
            return nil
        }
        let children = (view as? NSScrollView)?.documentView.map { [$0] } ?? view.subviews
        return children.lazy.compactMap { self.findView(in: $0, id: id, as: type) }.first
    }
    func settingsHub(_ value: SettingsPayload, hub: String?) -> String {
        value.settings_hub ?? hub ?? effectiveChannelHub
    }

    private func settingsFrameHub(_ frame: Frame, pending: PendingControl?) -> String? {
        frame.hub ?? pending.map(\.hub) ?? (frame.op == "state" ? frame.settings?.hubs.first(where: { $0.is_default })?.name ?? frame.hubs?.first?.name : nil)
    }

    private func channelSaveProjection(_ frame: Frame, for pending: PendingControl) -> PendingControl? {
        guard frame.op == "settings" || (frame.op == "state" && frame.hubs != nil && frame.listeners != nil),
              frame.ok != false, let value = frame.settings,
              settingsHub(value, hub: settingsFrameHub(frame, pending: pending)) == pending.hub,
              value.channels?.contains(where: { $0.id == pending.target }) == true else { return nil }
        return pending
    }

    private func mergedSettings(_ observed: SettingsPayload, hub: String?, frame: Frame, preserveChannels: Bool) -> SettingsPayload {
        var merged = observed
        if frame.preservesCollection("apps") || frame.preservesCollection("settings.apps"), let current = settings { merged.apps = current.apps }
        if frame.preservesCollection("settings.hubs") || (frame.hubs == nil && frame.preservesCollection("hubs")), let current = settings {
            merged.hubs = current.hubs.map { old in
                guard var updated = observed.hubs.first(where: { $0.name == old.name }) else { return old }
                updated.attachments = old.attachments ?? updated.attachments
                return updated
            }
        }
        let target = settingsHub(merged, hub: hub)
        if preserveChannels, let old = settings?.hubs.first(where: { $0.name == target }),
           old.sharesChannelAuthority(with: merged.hubs.first { $0.name == target }),
           let cached = channelSettingsByHub[target] { merged.channels = cached }
        return merged
    }

    private func settingsProjectionChanged(_ value: SettingsPayload, hub: String?) -> Bool {
        let target = settingsHub(value, hub: hub)
        return value != settings || channelInventoryErrorsByHub[target] != value.channel_inventory_error ||
            (value.channels.map { channelSettingsByHub[target] != $0 } ?? false)
    }

    private func updateActivity(_ frame: Frame) -> Bool {
        guard !frame.preservesCollection("activity"), let items = frame.activity else { return false }
        let total = frame.activity_total ?? activityTotal
        let unread = frame.activity_unread_count ?? frame.unread_count ?? activityUnreadCount
        let query = frame.activity_query ?? ""
        let changed = activityAllItems != items || activityTotal != total || activityUnreadCount != unread ||
            serverActivityFilter != frame.activity_filter || serverActivityQuery != query
        (activityAllItems, activityTotal, activityUnreadCount, serverActivityFilter, serverActivityQuery) =
            (items, total, unread, frame.activity_filter, query)
        return changed
    }

    private func settleControlReceipt(_ frame: Frame, receiptOnly: Bool = false) -> Bool {
        // A pair code supplies the instructions; settings carries the final receipt.
        if frame.op == "pair_code" && !receiptOnly { return false }
        guard let request = frame.request_id, let pending = pendingControls[request] else { return false }
        let operation = frame.source_op ?? frame.action ?? ""
        let isChannel = Self.channelControlOps.contains(pending.op)
        if isChannel || receiptOnly {
            let impliedOp = frame.op == "pair_code" ? "channel_pair" : frame.op == "channel_test_result" ? "channel_test" : ""
            let observedOp = operation.isEmpty ? impliedOp : operation
            guard frame.hub == nil || frame.hub == pending.hub,
                  frame.channel_id == nil || frame.channel_id == pending.target,
                  frame.channel == nil || frame.channel == pending.target,
                  observedOp == pending.op || (observedOp.isEmpty && (receiptOnly || frame.op == "error" || frame.ok == false)) else { return false }
        } else {
            guard frame.op != "channel_test_result",
                  frame.source_op == pending.op || frame.op == "error" ||
                  (frame.op == "settings" && frame.source_op == nil && pending.op == "set_host_hub" && request == hostPort.requestId) else { return false }
        }
        pendingControls.removeValue(forKey: request)
        if pending.appCheck, pending.op == "setup_apply", ["state", "settings"].contains(frame.op),
           frame.ok == true, frame.authority_changed != true {
            appCheckedUntil[pending.target] = Date().addingTimeInterval(1.5)
            rebuildFeedbackAfter(1.5)
        }
        guard isChannel else { return true }
        let hub = pending.hub
        let key = "\(hub)|\(pending.target)"
        if pending.op == "channel_pair", channelUiStates[key]?.pairing == .requested(request) {
            channelUiStates[key]?.pairing = nil
        }
        guard frame.authority_changed != true, frame.op != "error", frame.ok == true || frame.committed == true else { return true }
        let fields = channelSettingsByHub[hub]?.first { $0.id == pending.target }?.fields ?? []
        if let reset = pending.channelReset, var ui = channelUiStates[key] {
            let sent = reset.submitted
            let samePair = ui.pairing?.id == sent.pairing?.id && (sent.pairing != nil ||
                (ui.pairingInstruction, ui.pairingCommand, ui.pairingURL) == (sent.pairingInstruction, sent.pairingCommand, sent.pairingURL))
            let unchanged = ui == sent
            if samePair {
                if let id = ui.pairing?.id { pendingControls.removeValue(forKey: id) }
                ui.pairing = nil
                ui.clearPairingInstructions()
            }
            for field in fields where (reset.hub && field.scope == "hub") || (reset.user && field.scope == "user") {
                if ui.drafts[field.key] == sent.drafts[field.key] { ui.drafts.removeValue(forKey: field.key) }
            }
            if unchanged {
                if reset.hub { ui.resetGateway() }
                if reset.user { ui.resetPersonal() }
            }
            channelUiStates[key] = ui
        }
        if let save = pending.channelSave, channelSaveProjection(frame, for: pending) == nil {
            // Settings never echo submitted secrets. Keep non-secret drafts
            // until a fresh projection reconciles their values.
            for field in fields where field.type == "secret" {
                if let sent = save.values[field.key], channelUiStates[key]?.drafts[field.key] == sent {
                    channelUiStates[key]?.drafts.removeValue(forKey: field.key)
                }
            }
        }
        return true
    }

    @discardableResult
    private func settleDirectoryActionSuccess(_ frame: Frame) -> Bool {
        guard let request = frame.request_id, request == latestDirectoryActionRequestId,
              frame.ok != false, frame.error == nil else { return false }
        latestDirectoryActionRequestId = nil
        clearDirectoryActionError()
        if let warning = frame.warning, !warning.isEmpty {
            directoryActionWarning = warning
            updateDirectoryWarnings()
        }
        return true
    }

    @discardableResult
    func settleReceiptOnly(_ frame: Frame) -> Bool {
        guard let request = frame.request_id else { return false }
        if settleDirectoryActionSuccess(frame) { return true }
        let pendingOperation = pendingControls[request]?.op
        var settled = settleControlReceipt(frame, receiptOnly: true)
        var routedToForm = false
        let operation = frame.source_op ?? frame.action ?? (settled ? pendingOperation : nil) ?? ""
        let failed = frame.ok == false || frame.op == "error"
        let message = frame.warning ?? frame.error
        if let pending = hubForm.pendingSave, request == pending.requestId,
           operation.isEmpty || ["setup_save_hub", "paste_hub_invite"].contains(operation) {
            hubForm.pendingSave = nil
            if failed { hubForm.error = message ?? "Hub save failed."; routedToForm = true }
            else if frame.authority_changed != true && (frame.committed == true || frame.ok == true) {
                finishHubSave(pending)
            }
            settled = true
        }
        if !hubInviteRequest.isEmpty && request == hubInviteRequest && (operation.isEmpty || ["setup_save_hub", "paste_hub_invite"].contains(operation)) {
            hubInviteRequest = ""; settled = true
        }
        if request == hostPort.requestId && (operation.isEmpty || operation == "set_host_hub") {
            hostPort.pending = false; hostPort.requestId = ""; settled = true
        }
        if settled && !routedToForm && operation != "channel_test", let message = message, !message.isEmpty {
            if settingsVisible { settingsBannerError = message } else { menuBannerError = message }
        }
        return settled
    }

    func receive(_ data: Data) {
        if handleHeadlessTestCommand(data) { return }
        guard let frame = try? JSONDecoder().decode(Frame.self, from: data) else { diagnostic("invalid frame"); return }
        if frame.op == "hub_invite" {
            guard !hubForm.parseRequest.isEmpty, frame.request_id == hubForm.parseRequest, let cfg = frame.config else { return }
            hubForm.parseRequest=""; hubForm.error=nil
            hubForm.url=cfg.url ?? ""; hubForm.name=cfg.name ?? ""
            hubForm.auth=cfg.authMode ?? cfg.auth_mode ?? "none"
            hubForm.authExplicit = cfg.authMode != nil || cfg.auth_mode != nil
            hubForm.token=cfg.token ?? ""; hubForm.user=cfg.user ?? ""
            hubForm.issuer=cfg.issuer ?? cfg.oidcIssuer ?? ""
            hubForm.clientId=cfg.clientId ?? cfg.client_id ?? cfg.oidcClientId ?? ""
            hubForm.redirectPort=(cfg.redirect_port ?? cfg.redirectPort ?? cfg.oidcRedirectPort)?.text ?? ""
            rebuildSettingsPanel(syncFields: false); return
        }
        // Receipt-only settlements never project settings or results from an
        // obsolete authority. They still own the matching request's pending UI.
        if (frame.op == "settings" && frame.settings == nil) ||
           (frame.authority_changed == true && ["settings", "pair_code", "channel_test_result"].contains(frame.op)) {
            let settled = settleReceiptOnly(frame)
            if settled {
                rebuild(preservingEditor: true)
                refreshDirectoryViews()
                if settingsVisible { rebuildSettingsPanel() }
            }
            return
        }
        // Passive collection shedding cannot replace a complete cache. Scalar
        // observations and correlated receipts still advance independently.
        let preserveDirectoryRows = frame.op == "directory" && frame.preservesCollection("peers")
        if preserveDirectoryRows || (frame.op == "settings" && frame.request_id == nil) {
            let unread = frame.unread_count ?? unreadCount
            let online = frame.online_count ?? directoryOnlineCount
            let hasAny = frame.activity_has_any ?? activityHasAny
            if (unread, online, hasAny) != (unreadCount, directoryOnlineCount, activityHasAny) {
                (unreadCount, directoryOnlineCount, activityHasAny) = (unread, online, hasAny)
                rebuild(preservingEditor: true)
                refreshDirectoryViews()
            }
            if preserveDirectoryRows {
                if updateActivity(frame) { refreshDirectoryViews() }
                return
            }
        }
        let pendingReceipt = frame.request_id.flatMap { pendingControls[$0] }
        let receiptHub = frame.hub ?? pendingReceipt?.hub
        let projectedSave = pendingReceipt.flatMap { $0.channelSave == nil ? nil : channelSaveProjection(frame, for: $0) }
        let settledControl = settleControlReceipt(frame)
        let previousMenuBanner = menuBannerError
        let previousSettingsBanner = settingsBannerError
        if frame.op == "state", (frame.request_id == nil || settledControl), let message = frame.error ?? frame.warning, !message.isEmpty {
            if settingsVisible { settingsBannerError = message } else { menuBannerError = message }
        }
        let menuBannerChanged = menuBannerError != previousMenuBanner
        let settingsBannerChanged = settingsBannerError != previousSettingsBanner
        let preserveSettingsChannels = frame.preservesCollection("channels") || frame.preservesCollection("settings.channels")
        let settingsHubHint = settingsFrameHub(frame, pending: settledControl && pendingReceipt.map({ Self.channelControlOps.contains($0.op) }) == true ? pendingReceipt : nil)
        let nextSettings = frame.settings.map { mergedSettings($0, hub: settingsHubHint, frame: frame, preserveChannels: preserveSettingsChannels) }
        let actionFailure = (frame.op == "error" || frame.ok == false) && directoryActionOps.contains(frame.source_op ?? "")
        if actionFailure, let requestId = frame.request_id, !requestId.isEmpty,
           requestId != latestDirectoryActionRequestId {
            if requestId == pendingMessageCopy?.requestId { pendingMessageCopy = nil }
            return
        }
        if frame.op == "activity_message" {
            guard var pending = pendingMessageCopy, frame.request_id == pending.requestId else { return }
            if let message = frame.message, frame.part == pending.nextPart, frame.error == nil, frame.ok != false {
                pending.buffer += message; pending.nextPart += 1
                if frame.complete != false {
                    if frame.request_id == latestDirectoryActionRequestId { clearDirectoryActionError() }
                    let index = directoryMode == "activity" ? activityFilteredItems.firstIndex { $0.hub == pending.hub && $0.signal_id == pending.signalId } : nil
                    let cell = index.flatMap { directoryTableView?.view(atColumn:0,row:$0,makeIfNecessary:false) as? DirectoryPeerCellView }
                    let sameItem = cell?.activityItem?.hub == pending.hub && cell?.activityItem?.signal_id == pending.signalId
                    activityCopiedUntil["\(pending.hub)|\(pending.signalId)"] = Date().addingTimeInterval(1.2)
                    copyText(pending.buffer, control:sameItem ? cell?.copyControl : nil)
                }
                else { pendingMessageCopy = pending }
            }
            else { showMessageCopyError(frame.error ?? "Message retrieval failed.") }
            return
        }
        if frame.op == "error", let pending = pendingMessageCopy, frame.request_id == pending.requestId {
            showMessageCopyError(frame.error ?? "Message retrieval failed."); return
        }
        if frame.op == "composer_result" {
            guard let pending = pendingComposer, frame.request_id == pending.requestId else { return }
            pendingComposer = nil
            if frame.ok == true {
                if composerDraftIdempotencyKey == pending.composerDraftIdempotencyKey && composerTarget == pending.target && composerTargetHub == pending.hub && composerReplySignalId == pending.replySignalId && composerInputField?.stringValue == pending.text { closeComposer() }
                else { setComposerStatus(""); composerSendButton?.copied("✓ Sent") }
            } else {
                showComposerFailure(frame.error)
            }
            if let warning = frame.warning, !warning.isEmpty {
                showComposerDirectoryNotice(warning)
            }
            return
        }
        let activityPresenceChanged = frame.activity_has_any.map { $0 != activityHasAny } ?? false
        if frame.op == "settings", let value = nextSettings, frame.ok == false {
            acceptSettings(value, hub: settingsHubHint, savedRequest: frame.request_id, sourceOp: frame.source_op, truncated: preserveSettingsChannels)
        }
        if frame.op == "error" || (frame.ok == false && frame.op != "channel_test_result") {
            let sourceOp = frame.source_op ?? frame.action ?? (settledControl ? pendingReceipt?.op : nil) ?? ""
            let clipboardJoin = hubInvitePending && frame.request_id == hubInviteRequest
            if sourceOp == "paste_hub_invite", let request = frame.request_id {
                guard clipboardJoin || (!hubForm.parseRequest.isEmpty && request == hubForm.parseRequest) ||
                      (hubForm.pendingSave != nil && request == hubForm.pendingSave?.requestId) else { return }
                if !hubForm.parseRequest.isEmpty && request == hubForm.parseRequest { hubForm.parseRequest=""; return }
            }
            // A command may emit both its result and a trailing generic error.
            // Neither can settle a retry or overwrite its feedback.
            if sourceOp == "send_signal",
               (pendingComposer == nil || frame.request_id != pendingComposer?.requestId) { return }
            if sourceOp == "set_host_hub", let requestId = frame.request_id,
               requestId != hostPort.requestId && !settledControl { return }
            let isAddHubOp = ["setup_save_hub", "paste_hub_invite"].contains(sourceOp) || (hubForm.pendingSave != nil && frame.request_id == hubForm.pendingSave?.requestId)
            if sourceOp == "setup_save_hub", let requestId = frame.request_id,
               requestId != hubForm.pendingSave?.requestId && !clipboardJoin { return }
            // Only the matching save result owns an active form submission.
            // Untagged invite/transport failures remain visible in the settings banner.
            let isAddHubError: Bool
            if let pending = hubForm.pendingSave {
                isAddHubError = isAddHubOp && frame.request_id == pending.requestId
            } else {
                isAddHubError = hubForm.isOpen && isAddHubOp && !clipboardJoin && sourceOp != "paste_hub_invite"
            }
            let error = frame.error ?? frame.warning ?? ""
            if ["switch_hub", "set_scope", "listen_session", ""].contains(sourceOp) { pendingMutation = false }
            if (sourceOp == "set_host_hub" || sourceOp.isEmpty) &&
               frame.request_id == hostPort.requestId {
                hostPort.pending = false
                hostPort.requestId = ""
            }
            if (sourceOp.isEmpty || ["paste_hub_invite", "setup_save_hub"].contains(sourceOp)) && (frame.request_id == nil || clipboardJoin) {
                hubInviteRequest=""; hubInvitePastedUntil = .distantPast
            }
            if isAddHubError { hubForm.pendingSave = nil }
            let isComposerError = (sourceOp.isEmpty || sourceOp == "send_signal") &&
                pendingComposer != nil && frame.request_id == pendingComposer?.requestId
            if isComposerError { pendingComposer = nil }
            if error.isEmpty { }
            else if isComposerError { showComposerFailure(error) }
            else if isAddHubError { hubForm.error = error }
            else if ["query_directory", "open_directory", "directory"].contains(sourceOp) || (frame.op == "directory" && sourceOp.isEmpty) {
                if frame.request_id == nil || frame.request_id == directoryRequestId { directoryQueryError = error; updateDirectoryWarnings() }
            }
            else if directoryVisible && directoryActionOps.contains(sourceOp) {
                directoryActionError = error; directoryActionErrorSourceOp = sourceOp; updateDirectoryWarnings()
            }
            else if sourceOp == "channel_test" || (sourceOp == "set_host_hub" && presentationError(settings?.host_hub.error ?? "") == presentationError(error)) { }
            else if settingsVisible || hubForm.pendingSave != nil || clipboardJoin || sourceOp == "paste_hub_invite" { settingsBannerError = error }
            else { menuBannerError = error }
            diagnostic("error \(error)")
            rebuild(preservingEditor: true)
            if settingsPanel != nil { rebuildSettingsPanel() }
            return
        }
        // State receipts still pass through normal failure and Directory action
        // correlation above; only successful commits may omit their projection.
        if frame.op == "state", frame.ok == true, frame.committed == true,
           frame.request_id != nil, frame.hubs == nil || frame.listeners == nil {
            settleReceiptOnly(frame)
            rebuild(preservingEditor: true)
            refreshDirectoryViews()
            if settingsVisible { rebuildSettingsPanel() }
            return
        }
        if ["state", "directory"].contains(frame.op) { settleDirectoryActionSuccess(frame) }
        if frame.op == "settings", let value = nextSettings {
            let changed = settingsProjectionChanged(value, hub: settingsHubHint)
            let receipt = settledControl || frame.request_id != nil || frame.source_op != nil
            let hubSaveSucceeded = frame.ok == true && ["setup_save_hub", "paste_hub_invite"].contains(frame.source_op ?? "")
            if hubInvitePending && frame.request_id == hubInviteRequest && hubSaveSucceeded {
                hubInviteRequest=""; hubInvitePastedUntil=Date().addingTimeInterval(1.2); rebuildFeedbackAfter(1.2)
            }
            let uiChanged = acceptSettings(value, hub: settingsHubHint, channelSave: settledControl ? projectedSave : nil, savedRequest: frame.request_id, sourceOp: frame.source_op, confirmHubSave: hubSaveSucceeded, truncated: preserveSettingsChannels)
            if let warning = frame.warning, !warning.isEmpty, frame.source_op != "channel_test",
               !(frame.source_op == "set_host_hub" && presentationError(value.host_hub.error ?? "") == presentationError(warning)) { settingsBannerError = warning }
            if changed || receipt || uiChanged || frame.request_id == hostPort.requestId || settingsBannerError != previousSettingsBanner { rebuildSettingsPanel(preservingEditor: !receipt) }
            return
        }
        if frame.op == "pair_code", frame.authority_changed != true, let id = frame.channel_id {
            let hub = receiptHub ?? effectiveChannelHub
            let key="\(hub)|\(id)"
            guard let request = frame.request_id, let pending = pendingReceipt,
                  pending.op == "channel_pair", pending.hub == hub, pending.target == id,
                  var ui = channelUiStates[key], ui.pairing == .requested(request) else { return }
            ui.pairing = .displayed(request)
            ui.pairingInstruction = frame.instruction ?? frame.code ?? ""
            ui.pairingCommand = frame.pairing_command ?? frame.instruction
            ui.pairingURL = frame.pairing_url
            if let command = frame.pairing_command { copyText(command); ui.pairCopiedUntil = Date().addingTimeInterval(1.2); rebuildFeedbackAfter(1.2) }
            channelUiStates[key] = ui
            if settingsPanel != nil { rebuildSettingsPanel() }
            return
        }
        if frame.op == "channel_test_result", frame.authority_changed != true, let id = frame.channel_id {
            guard frame.request_id == nil || settledControl else { return }
            let key = "\(receiptHub ?? effectiveChannelHub)|\(id)"
            channelUiStates[key, default: ChannelUiState()].testResult = (ok: frame.ok == true, detail: frame.detail ?? frame.error ?? (frame.ok == true ? "Test sent" : "Test failed"))
            channelUiStates[key, default: ChannelUiState()].testResultIsLatest = true
            if settingsPanel != nil { rebuildSettingsPanel() }
            return
        }
        if frame.op == "state", let observedHubs = frame.hubs, let observedListeners = frame.listeners {
            let h = hasRenderedState && frame.preservesCollection("hubs") ? hubs.map { old in
                guard var updated = observedHubs.first(where: { $0.name == old.name }) else { return old }
                updated.attachments = old.attachments ?? updated.attachments
                return updated
            } : observedHubs
            let l = hasRenderedState && frame.preservesCollection("listeners") ? listeners : observedListeners
            if let hasAny = frame.activity_has_any { activityHasAny = hasAny }
            let a = hasRenderedState && frame.preservesCollection("available") ? available : frame.available ?? []
            let nextDiscovery = hasRenderedState && frame.preservesCollection("discovery") ? discovery : frame.discovery ?? discovery
            let discoveryChanged = discovery != nextDiscovery
            discovery = nextDiscovery
            let r = hasRenderedState && frame.preservesCollection("my_remote_listeners") ? myRemote : (frame.my_remote_listeners ?? []).filter { $0.harness != "human" }
            let onlineCount = frame.directory_online_count ?? max(directoryOnlineCount, r.count)
            let directoryRemoteChanged = r != myRemote || onlineCount != directoryOnlineCount
            let nextUnread = frame.unread_count ?? unreadCount
            let activityProjectionChanged = updateActivity(frame)
            let activityChanged = activityPresenceChanged || nextUnread != unreadCount || activityProjectionChanged
            unreadCount = nextUnread
            let directoryMembershipChanged = h != hubs || l != listeners
            let nextHarnesses = hasRenderedState && frame.preservesCollection("harnesses") ? harnesses : frame.harnesses ?? harnesses
            let harnessesChanged = nextHarnesses != harnesses
            if harnessesChanged { harnesses = nextHarnesses }
            // An unchanged save acknowledgement still needs to refresh the settings feedback.
            let settingsChanged = nextSettings.map { settingsProjectionChanged($0, hub: settingsHubHint) } ?? false
            let settingsReceipt = (hostPort.pending && frame.request_id == hostPort.requestId) || (settledControl && projectedSave != nil)
            if (settingsChanged || settingsReceipt), let value = nextSettings ?? settings {
                acceptSettings(value, hub: settingsHubHint, channelSave: settledControl ? projectedSave : nil, savedRequest: frame.request_id, sourceOp: frame.source_op, truncated: preserveSettingsChannels)
            }
            if hasRenderedState && !settledControl && !discoveryChanged && !pendingMutation && !harnessesChanged && !settingsChanged && !settingsReceipt && !menuBannerChanged && !settingsBannerChanged && !activityChanged && !directoryMembershipChanged && a == available && !directoryRemoteChanged {
                updateStatusItem()
            } else {
                hubs = h; listeners = l; available = a; myRemote = r; directoryOnlineCount = onlineCount
                seedDirectoryPeersIfEmpty()
                hasRenderedState = true; pendingMutation = false
                rebuild(preservingEditor: true)
                refreshDirectoryViews()
                if directoryVisible && (directoryMembershipChanged || directoryRemoteChanged) { queryDirectory() }
                if settingsChanged || settingsReceipt || settingsBannerChanged || settledControl || directoryMembershipChanged {
                    rebuildSettingsPanel(preservingEditor: frame.request_id == nil && frame.source_op == nil)
                }
            }
            if !hasCheckedFirstLaunchOnboarding, let s = settings, !s.apps.isEmpty {
                hasCheckedFirstLaunchOnboarding = true
                if !headless && !s.apps.contains(where: { $0.isConfigured || $0.status == "error" }) {
                    let rawStateDir = ProcessInfo.processInfo.environment["CITO_STATE_DIR"] ?? (NSHomeDirectory() + "/.cito")
                    let expandedStateDir = rawStateDir.hasPrefix("~/")
                        ? (NSHomeDirectory() + "/" + String(rawStateDir.dropFirst(2)))
                        : (rawStateDir == "~" ? NSHomeDirectory() : rawStateDir)
                    let markerPath = expandedStateDir + "/onboarding-shown"
                    if !FileManager.default.fileExists(atPath: markerPath) {
                        pendingOnboardingMarker = markerPath
                        settingsActiveTab = "apps"
                        openSettingsPanel()
                    }
                }
            }
            if headless { diagnostic("state hubs=\(h.count) listeners=\(l.count)" + (a.isEmpty ? "" : " available=\(a.count)")) }
        } else if frame.op == "directory", let peers = frame.peers {
            guard frame.request_id == directoryRequestId && frame.query == directoryQuery && frame.scope == directoryScopeFilter && frame.harness == directoryHarnessFilter else { return }
            if let hasAny = frame.activity_has_any { activityHasAny = hasAny }
            let menuChanged = activityPresenceChanged || (frame.unread_count ?? unreadCount) != unreadCount ||
                (frame.online_count ?? peers.count) != directoryOnlineCount
            let activityProjectionChanged = updateActivity(frame)
            let rowsChanged = activityPresenceChanged || (frame.unread_count ?? unreadCount) != unreadCount ||
                serverDirectoryQuery == nil || serverDirectoryQuery != directoryQuery ||
                serverDirectoryScope != directoryScopeFilter || serverDirectoryHarness != directoryHarnessFilter ||
                directoryAllPeers != peers || serverDirectoryMatchedCount != (frame.matched_count ?? peers.count) ||
                directoryOnlineCount != (frame.online_count ?? peers.count) ||
                activityProjectionChanged
            let warningsChanged = directoryQueryError != nil || (frame.warnings ?? directoryWarnings) != directoryWarnings
            directoryWarnings = frame.warnings ?? directoryWarnings
            directoryQueryError = nil
            if warningsChanged { updateDirectoryWarnings() }
            directoryAllPeers = peers
            serverDirectoryQuery = directoryQuery
            serverDirectoryScope = directoryScopeFilter
            serverDirectoryHarness = directoryHarnessFilter
            serverDirectoryMatchedCount = frame.matched_count ?? peers.count
            directoryOnlineCount = frame.online_count ?? peers.count
            if let u = frame.unread_count { unreadCount = u }
            if rowsChanged { refreshDirectoryViews() }
            if hasRenderedState && (menuChanged || settledControl) { rebuild(preservingEditor: true) }
            if headless { diagnostic("directory peers=\(peers.count) matched=\(directoryMatchedCount) online=\(directoryOnlineCount)") }
        } else if frame.op == "state" {
            if menuBannerChanged { rebuild(preservingEditor: true) }
            if settingsBannerChanged { rebuildSettingsPanel(preservingEditor: frame.request_id == nil) }
        }
    }

    func emit(_ action: [String: Any], userInitiated: Bool = true, appCheck: Bool = false) {
        let operation = action["op"] as? String ?? ""
        let passive = ["query_directory", "refresh_available"].contains(operation)
        if userInitiated && !passive && menuBannerError != nil { menuBannerError = nil; rebuild(preservingEditor: true) }
        var outgoing = action
        let tracked = Self.channelControlOps + ["set_host_hub", "human_inbox_update", "unlisten_all", "setup_apply", "setup_remove_app", "setup_remove_hub", "set_scope", "set_auto_listen", "unlisten_session", "set_listen_mode", "rename_session", "hub_action", "setup_toggle_hub", "set_launch_at_login", "setup_disconnect_all", "listen_session"]
        if tracked.contains(operation) || directoryActionOps.contains(operation) {
            outgoing["request_id"] = (action["request_id"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? UUID().uuidString
        }
        guard let data = try? JSONSerialization.data(withJSONObject: outgoing, options: [.sortedKeys]), data.count <= 1_048_576 else {
            let message = "This action is too large to send. Shorten the input and try again."
            if operation == "setup_save_hub" { hubForm.pendingSave = nil; hubForm.error = message }
            else if operation == "set_host_hub" { hostPort.pending = false; hostPort.error = message }
            else if operation == "paste_hub_invite" { hubInviteRequest = ""; hubForm.parseRequest = ""; settingsBannerError = message }
            else if operation == "send_signal" { pendingComposer = nil; setComposerStatus(message) }
            else { settingsBannerError = message; menuBannerError = message }
            rebuild(preservingEditor: true); if settingsPanel != nil { rebuildSettingsPanel(preservingEditor: true) }
            return
        }
        if tracked.contains(operation), let request = outgoing["request_id"] as? String {
            let channel = outgoing["channel_id"] as? String
            let hub = outgoing["hub"] as? String ?? (channel == nil ? "" : effectiveChannelHub)
            let session = (outgoing["session_id"] as? String).map { "\(outgoing["harness"] as? String ?? ""):\($0)" }
            let hostTarget = action["auth_mode"] != nil ? "access" : (request == hostPort.requestId ? "port" : "sharing")
            let inboxTarget = action["paused"] != nil ? "paused" : "scope"
            let controlTarget = operation == "set_host_hub" ? hostTarget : (operation == "human_inbox_update" ? inboxTarget : "")
            let target = channel ?? session ?? outgoing["harness"] as? String ?? controlTarget
            var pending = PendingControl(op: operation, hub: hub, target: target, appCheck: appCheck)
            if let channel {
                let key = "\(hub)|\(channel)"
                if operation == "channel_pair" { channelUiStates[key, default: ChannelUiState()].pairing = .requested(request) }
                if operation == "channel_test" { channelUiStates[key, default: ChannelUiState()].testResultIsLatest = false }
                if Self.channelSaveOps.contains(operation), let values = outgoing["values"] as? [String: String] {
                    let scope = outgoing["scope"] as? String ?? (operation == "save_hub_service" ? "hub" : "user")
                    for old in Array(pendingControls.keys) where pendingControls[old].map({ $0.hub == hub && $0.target == target && $0.channelSave?.scope == scope }) == true {
                        pendingControls[old]?.channelSave = nil
                    }
                    pending.channelSave = .init(scope: scope, values: values)
                }
                if Self.channelResetOps.contains(operation) {
                    let resetHub = outgoing["reset_hub"] as? Bool == true
                    let resetUser = outgoing["reset_user"] as? Bool == true
                    var submitted = channelUiStates[key] ?? ChannelUiState()
                    submitted.testResult = nil
                    pending.channelReset = (hub: resetHub, user: resetUser, submitted: submitted)
                }
            }
            pendingControls[request] = pending
        }
        if directoryActionOps.contains(operation), let request = outgoing["request_id"] as? String {
            // Full-message copy already owns this ID through its multipart buffer.
            latestDirectoryActionRequestId = request
            clearDirectoryActionError()
        }
        if Self.channelControlOps.contains(operation), let id = outgoing["channel_id"] as? String {
            let key = "\(outgoing["hub"] as? String ?? effectiveChannelHub)|\(id)"
            var removedTestResult = false
            if operation != "channel_test" {
                removedTestResult = channelUiStates[key]?.testResult != nil
                channelUiStates[key, default: ChannelUiState()].testResult = nil
            }
            if !removedTestResult && Self.channelSaveOps.contains(operation),
               let root = settingsPanel?.contentView,
               let save: MenuControl = findView(in:root,id:operation == "channel_save" ? "save_channel:\(id)" : "save_hub_service:\(id)") {
                // Pending feedback changes no fields; retain the editor's undo stack.
                pendingLabel(save,true,"Saving…"); save.layoutContents()
            } else if settingsPanel != nil { rebuildSettingsPanel() }
        }
        if ["set_host_hub", "hub_action", "setup_toggle_hub", "setup_remove_hub", "human_inbox_update", "unlisten_all", "setup_apply", "setup_remove_app", "listen_session"].contains(operation) {
            rebuild(preservingEditor: true); if settingsPanel != nil { rebuildSettingsPanel(preservingEditor: true) }
        }
        FileHandle.standardOutput.write(data + Data([10]))
    }
    func controlTextDidEndEditing(_ obj: Notification) {
        // Wait until AppKit releases the shared field editor before replacing views.
        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }
            if self.settingsRebuildDeferred { self.rebuildSettingsPanel(preservingEditor: true) }
            if self.menuRebuildDeferred { self.rebuild(preservingEditor: true) }
        }
    }

    func controlTextDidChange(_ obj: Notification) {
        if uninstallConfirmOpen, let control = obj.object as? NSControl, control.window === settingsPanel { return }
        if let field = obj.object as? NSSearchField, field === directorySearchField {
            activityClearConfirm = false; clearDirectoryActionError(); updateDirectoryFacetStyles()
            directoryQuery = field.stringValue
            if directoryMode == "activity" {
                applyActivityFilter()
            } else {
                applyDirectoryFilter()
            }
            queryDirectory()
            return
        }
        if let field = obj.object as? NSTextField, field === composerInputField { composerDraftIdempotencyKey = UUID().uuidString }
        if let field = obj.object as? NSTextField, field.identifier?.rawValue == "host_hub_port" {
            hostPort.draft = field.stringValue
            hostPort.savedUntil = nil
            hostPort.error = nil
            if let root = settingsPanel?.contentView, let apply: MenuControl = findView(in: root, id: "apply_host_hub_port") {
                let title = hostPort.pending ? "Applying…" : "Apply"
                apply.clearFeedback(title: title, accessibilityLabel: title)
                apply.toolTip = title
                apply.isEnabled = !hostPort.pending && Int(field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)) != settings?.host_hub.port
                apply.layoutContents()
            }
        }
        if let field = obj.object as? NSTextField, let id = field.identifier?.rawValue, id.hasPrefix("settings_hub_") {
            hubForm.generation += 1; hubForm.parseRequest=""
            syncAddHubFieldsFromControls()
            let raw = field.stringValue.trimmingCharacters(in:.whitespacesAndNewlines)
            if id == "settings_hub_url" && (raw.hasPrefix("cito setup ") || raw.hasPrefix("{")) {
                hubForm.parseRequest=UUID().uuidString
                emit(["op":"paste_hub_invite","invite":raw,"parse_only":true,"request_id":hubForm.parseRequest])
            }
            settingsFieldControls["settings_hub_name"]?.placeholderString = hubNamePlaceholder
        }
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
        if textView.hasMarkedText() { return false }
        if control.identifier?.rawValue.hasPrefix("rename_field:") == true {
            if commandSelector == #selector(NSResponder.cancelOperation(_:)) {
                renamingSessionKey = nil; rebuild(); return true
            }
            if commandSelector == #selector(NSResponder.insertNewline(_:)), let key = renamingSessionKey,
               let listener = listeners.first(where: { "\($0.harness):\($0.session_id)" == key }) {
                let newTitle = textView.string
                guard !newTitle.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, newTitle.unicodeScalars.count <= 128 else {
                    menuBannerError = "Title must contain 1 to 128 characters."
                    renameField?.textColor = CitoPalette.dangerText
                    renameField?.toolTip = menuBannerError
                    renameField?.setAccessibilityHelp(menuBannerError)
                    rebuild(preservingEditor: true); return true
                }
                emit(["op":"rename_session", "harness":listener.harness, "session_id":listener.session_id, "title":newTitle])
                renamingSessionKey = nil; rebuild(); return true
            }
            return false
        }
        if control.window === settingsPanel {
            if uninstallConfirmOpen {
                if commandSelector == #selector(NSResponder.cancelOperation(_:)) { handleSettingsKey("escape") }
                return true
            }
            if commandSelector == #selector(NSResponder.insertNewline(_:)) { handleSettingsKey("return"); return true }
            if commandSelector == #selector(NSResponder.cancelOperation(_:)) {
                handleSettingsKey("escape")
                return true
            }
            return false
        }
        if commandSelector == #selector(NSResponder.moveDown(_:)) || commandSelector == #selector(NSResponder.moveUp(_:)) {
            guard control !== composerInputField else { return false }
            handleDirectoryKey(commandSelector == #selector(NSResponder.moveDown(_:)) ? "down" : "up")
            return true
        }
        if commandSelector == #selector(NSResponder.insertNewline(_:)) { handleDirectoryKey(NSApp.currentEvent?.modifierFlags.contains(.shift) == true ? "shift-return" : "return"); return true }
        if commandSelector == #selector(NSResponder.cancelOperation(_:)) { handleDirectoryKey("escape"); return true }
        return false
    }
}
