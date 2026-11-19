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
    var latestDirectoryActionRequestId: String?
    let directoryActionOps = ["clear_activity_history", "ack_signals", "mark_all_activity_read", "copy_activity_message"]
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
    var lastDirectoryFacetSignature = ""
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
        let name: String
        let url: String
        let requestId: String
        let generation: Int
        let issuer: String, clientId: String, redirectPort: String
    }
    struct HubFormState {
        enum Mode {
            case closed
            case add
            case edit(name: String, initial: HubConfigItem?)
        }

        var mode: Mode
        var isOpen: Bool { if case .closed = mode { return false }; return true }
        var isEditing: Bool { if case .edit = mode { return true }; return false }
        var originalName: String { if case let .edit(name, _) = mode { return name }; return "" }
        var initialHub: HubConfigItem? { if case let .edit(_, initial) = mode { return initial }; return nil }
        var error: String?
        var parseRequest = ""
        var pendingSave: PendingHubSave?
        // A confirmed save can precede its canonical settings projection.
        var awaitingSettings: PendingHubSave?
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
            mode = .edit(name: hub.name, initial: hub)
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
    var hubInvitePending = false
    var hubInviteRequest = ""
    var hubInvitePastedUntil = Date.distantPast
    var settingsFieldControls: [String: NSTextField] = [:]
    var channelUiStates: [String: ChannelUiState] = [:]
    var appCheckedUntil: [String: Date] = [:]
    var pendingAppChecks: [String: String] = [:]
    struct PendingControl {
        let op: String, hub: String, target: String
    }
    var pendingControls: [String: PendingControl] = [:]
    var pendingChannelResets: [String: (all: Bool, submitted: ChannelUiState)] = [:]
    func isPending(_ op: String, hub: String = "", target: String = "") -> Bool {
        pendingControls.values.contains { $0.op == op && $0.hub == hub && $0.target == target }
    }
    var hostHubAddress = "tailscale"
    var pendingChannelValues: [String: (hub: String, channelId: String, scope: String, values: [String: String])] = [:]
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
        guard editor.isEditable, !editor.hasMarkedText() else { return false }
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
        switch key.lowercased() {
        case "command-v":
            // The pasteboard remains authoritative after a copy in another application.
            let systemText = appPasteboard.string(forType: .string) ?? ""
            let text = systemText.isEmpty && headless ? clipboardString : systemText
            clipboardString = text
            changeText { editor.insertText(text, replacementRange: editor.selectedRange()) }
        case "command-a": editor.selectAll(nil); return true
        case "command-c", "command-x":
            guard !(field is NSSecureTextField) else { return true }
            let range = editor.selectedRange()
            guard range.length > 0 else { return true }
            copyText((editor.string as NSString).substring(with: range))
            if key.lowercased() == "command-c" { return true }
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
            controlTextDidChange(Notification(name: NSControl.textDidChangeNotification, object: field))
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
    func findControl(in view: NSView, id: String) -> MenuControl? {
        guard !view.isHidden else { return nil }
        let rawId = view.identifier?.rawValue
        if rawId == id, let control = view as? MenuControl { return control }
        if let table = view as? NSTableView {
            let count = min(table.numberOfRows, 100)
            for r in 0..<count {
                if let cell = table.view(atColumn: 0, row: r, makeIfNecessary: true),
                   let found = findControl(in: cell, id: id) {
                    return found
                }
            }
        }
        let children: [NSView]
        if let scroll = view as? NSScrollView, let doc = scroll.documentView {
            children = [doc]
        } else {
            children = view.subviews
        }
        return children.compactMap { findControl(in: $0, id: id) }.first
    }
    func findTextField(in view: NSView, id: String) -> NSTextField? {
        guard !view.isHidden else { return nil }
        if view.identifier?.rawValue == id, let field = view as? NSTextField { return field }
        let children: [NSView]
        if let scroll = view as? NSScrollView, let doc = scroll.documentView {
            children = [doc]
        } else {
            children = view.subviews
        }
        return children.compactMap { findTextField(in: $0, id: id) }.first
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
        if operation != "channel_test", let warning = frame.warning, !warning.isEmpty { settingsBannerError = warning }
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
        if let pending = hubForm.pendingSave, request == pending.requestId,
           operation.isEmpty || ["setup_save_hub", "save_hub", "paste_hub_invite"].contains(operation) {
            hubForm.pendingSave = nil
            if failed { hubForm.error = message ?? "Hub save failed."; routedToForm = true }
            else if frame.authority_changed != true && (frame.committed == true || frame.ok == true) {
                finishHubSave(pending)
            }
            settled = true
        }
        if !hubInviteRequest.isEmpty && request == hubInviteRequest && (operation.isEmpty || ["setup_save_hub", "save_hub", "paste_hub_invite"].contains(operation)) {
            hubInvitePending = false; settled = true
        }
        if request == hostPort.requestId && (operation.isEmpty || operation == "set_host_hub") {
            hostPort.pending = false; hostPort.requestId = ""; settled = true
        }
        if (operation.isEmpty || operation == "setup_apply"), pendingAppChecks.removeValue(forKey: request) != nil { settled = true }
        if let pending = pendingControls[request], pending.op == operation || operation.isEmpty,
           frame.hub == nil || frame.hub == pending.hub,
           frame.channel_id == nil || frame.channel_id == pending.target,
           frame.channel == nil || frame.channel == pending.target {
            pendingControls.removeValue(forKey: request)
            pendingChannelResets.removeValue(forKey: request)
            settled = true
        }
        if settled && !routedToForm && operation != "channel_test", let message = message, !message.isEmpty { settingsBannerError = message }
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
                if let warning = frame.warning, !warning.isEmpty, frame.source_op != "channel_test", !settingsVisible { menuBannerError = warning }
                rebuild()
                refreshDirectoryViews()
                if settingsVisible { rebuildSettingsPanel() }
            }
            return
        }
        // Passive collection shedding cannot replace a complete cache. Scalar
        // observations and correlated receipts still advance independently.
        let preserveCollections = frame.truncated == true && frame.request_id == nil
        if preserveCollections && ((frame.op == "settings" && settings != nil) || frame.op == "directory") {
            if let unread = frame.unread_count { unreadCount = unread }
            if let unread = frame.activity_unread_count { activityUnreadCount = unread }
            if let online = frame.online_count { directoryOnlineCount = online }
            if let hasAny = frame.activity_has_any { activityHasAny = hasAny }
            rebuild()
            refreshDirectoryViews()
            return
        }
        let receiptHub = frame.hub ?? frame.request_id.flatMap { pendingControls[$0]?.hub }
        var settledControl = frame.op == "pair_code" ? false : settleChannelReceipt(frame)
        if let request = frame.request_id, let pending = pendingControls[request],
           frame.op != "pair_code" && frame.op != "channel_test_result",
           frame.source_op == pending.op || frame.op == "error" ||
           (frame.op == "settings" && frame.source_op == nil && pending.op == "set_host_hub" && request == hostPort.requestId) {
            pendingControls.removeValue(forKey: request)
            pendingChannelResets.removeValue(forKey: request)
            settledControl = true
        }
        if frame.op == "state", (frame.request_id == nil || settledControl), let message = frame.error ?? frame.warning, !message.isEmpty {
            if settingsVisible { settingsBannerError = message } else { menuBannerError = message }
        }
        if frame.ok == false || frame.op == "error", let requestId = frame.request_id {
            pendingChannelValues.removeValue(forKey: requestId)
            for key in Array(channelUiStates.keys) where channelUiStates[key]?.pendingPairRequest == requestId {
                channelUiStates[key]?.pendingPairRequest = nil
            }
        }
        let actionFailure = (frame.op == "error" || frame.ok == false) && directoryActionOps.contains(frame.source_op ?? "")
        if actionFailure, let requestId = frame.request_id, !requestId.isEmpty,
           requestId != latestDirectoryActionRequestId {
            if requestId == pendingMessageCopy?.requestId { pendingMessageCopy = nil }
            return
        }
        if ["state", "directory"].contains(frame.op), frame.ok != false, frame.error == nil,
           let latest = latestDirectoryActionRequestId, frame.request_id == latest {
            clearDirectoryActionError()
            latestDirectoryActionRequestId = nil
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
                if composerTarget == pending.target && composerTargetHub == pending.hub && composerReplySignalId == pending.replySignalId && composerInputField?.stringValue == pending.text { closeComposer() }
                else { setComposerStatus(""); composerSendButton?.copied("✓ Sent") }
            } else {
                setComposerStatus(frame.error ?? "Send failed. Your draft is preserved.")
            }
            return
        }
        let activityPresenceChanged = frame.activity_has_any.map { $0 != activityHasAny } ?? false
        if frame.op == "settings", let value = frame.settings, frame.ok == false {
            acceptSettings(value, hub: frame.hub, savedRequest: frame.request_id, sourceOp: frame.source_op)
        }
        if frame.op == "error" || (frame.ok == false && frame.op != "channel_test_result") {
            if let request = frame.request_id { pendingAppChecks.removeValue(forKey: request) }
            let sourceOp = frame.source_op ?? ""
            if sourceOp == "paste_hub_invite", let request = frame.request_id {
                guard (!hubForm.parseRequest.isEmpty && request == hubForm.parseRequest) ||
                      (!hubInviteRequest.isEmpty && request == hubInviteRequest) ||
                      (hubForm.pendingSave != nil && request == hubForm.pendingSave?.requestId) else { return }
                if !hubForm.parseRequest.isEmpty && request == hubForm.parseRequest { hubForm.parseRequest=""; return }
                if !hubInviteRequest.isEmpty && request == hubInviteRequest { hubInvitePending = false }
            }
            // A command may emit both its result and a trailing generic error.
            // Neither can settle a retry or overwrite its feedback.
            if ["send_signal", "send_reply_from_human"].contains(sourceOp),
               (pendingComposer == nil || frame.request_id != pendingComposer?.requestId) { return }
            if sourceOp == "set_host_hub", let requestId = frame.request_id,
               requestId != hostPort.requestId && !settledControl { return }
            let isAddHubOp = ["setup_save_hub", "save_hub", "paste_hub_invite"].contains(sourceOp) || (hubForm.pendingSave != nil && frame.request_id == hubForm.pendingSave?.requestId)
            if ["setup_save_hub", "save_hub"].contains(sourceOp), let requestId = frame.request_id,
               requestId != hubForm.pendingSave?.requestId { return }
            // Only the matching save result owns an active form submission.
            // Untagged invite/transport failures remain visible in the settings banner.
            let isAddHubError: Bool
            if let pending = hubForm.pendingSave {
                isAddHubError = isAddHubOp && frame.request_id == pending.requestId
            } else {
                isAddHubError = hubForm.isOpen && isAddHubOp && sourceOp != "paste_hub_invite"
            }
            let error = frame.error ?? frame.warning ?? ""
            if ["switch_hub", "set_scope", "listen_session", ""].contains(sourceOp) { pendingMutation = false }
            if (sourceOp == "set_host_hub" || sourceOp.isEmpty) &&
               frame.request_id == hostPort.requestId {
                hostPort.pending = false
                hostPort.requestId = ""
            }
            if (["paste_hub_invite", "setup_save_hub", "save_hub"].contains(sourceOp) && frame.request_id == nil) || (sourceOp.isEmpty && (frame.request_id == nil || (!hubInviteRequest.isEmpty && frame.request_id == hubInviteRequest))) {
                hubInvitePending=false; hubInvitePastedUntil = .distantPast
            }
            if isAddHubError { hubForm.pendingSave = nil }
            let isComposerError = ["send_signal", "send_reply_from_human", ""].contains(sourceOp) &&
                pendingComposer != nil && frame.request_id == pendingComposer?.requestId
            if error.isEmpty { }
            else if isComposerError {
                setComposerStatus(error)
                pendingComposer = nil
            } else if isAddHubError { hubForm.error = error }
            else if ["query_directory", "open_directory", "directory"].contains(sourceOp) || (frame.op == "directory" && sourceOp.isEmpty) {
                if frame.request_id == nil || frame.request_id == directoryRequestId { directoryQueryError = error; updateDirectoryWarnings() }
            }
            else if directoryVisible && directoryActionOps.contains(sourceOp) {
                directoryActionError = error; directoryActionErrorSourceOp = sourceOp; updateDirectoryWarnings()
            }
            else if sourceOp == "channel_test" || (sourceOp == "set_host_hub" && presentationError(settings?.host_hub.error ?? "") == presentationError(error)) { }
            else if settingsVisible || hubForm.pendingSave != nil || sourceOp == "paste_hub_invite" { settingsBannerError = error }
            else { menuBannerError = error }
            diagnostic("error \(error)")
            rebuild()
            if settingsPanel != nil { rebuildSettingsPanel() }
            return
        }
        // State receipts still pass through normal failure and Directory action
        // correlation above; only successful commits may omit their projection.
        if frame.op == "state", frame.ok == true, frame.committed == true,
           frame.request_id != nil, frame.hubs == nil || frame.listeners == nil {
            let receiptSettled = settleReceiptOnly(frame)
            if settledControl || receiptSettled, let warning = frame.warning, !warning.isEmpty {
                if settingsVisible { settingsBannerError = warning } else { menuBannerError = warning }
            }
            rebuild()
            refreshDirectoryViews()
            if settingsVisible { rebuildSettingsPanel() }
            return
        }
        if frame.op == "settings", let value = frame.settings {
            let target = value.settings_hub ?? value.channel_hub ?? frame.hub ?? effectiveChannelHub
            let changed = value != settings || (value.channels.map { channelSettingsByHub[target] != $0 } ?? false)
            let receipt = settledControl || frame.request_id != nil || frame.source_op != nil
            if let request = frame.request_id, let app = pendingAppChecks.removeValue(forKey: request),
               frame.ok == true && frame.source_op == "setup_apply" {
                appCheckedUntil[app] = Date().addingTimeInterval(1.5)
                rebuildFeedbackAfter(1.5)
            }
            if hubInvitePending && !hubInviteRequest.isEmpty && frame.request_id == hubInviteRequest && frame.ok == true && ["setup_save_hub","save_hub","paste_hub_invite"].contains(frame.source_op ?? "") {
                hubInvitePending=false; hubInvitePastedUntil=Date().addingTimeInterval(1.2); rebuildFeedbackAfter(1.2)
            }
            let saved = frame.ok == true && ["channel_save","save_hub_service"].contains(frame.source_op ?? "") ? frame.channel_id : nil
            let uiChanged = acceptSettings(value, hub: frame.hub, savedChannel: saved, savedRequest: frame.request_id, sourceOp: frame.source_op, confirmHubSave: frame.ok == true && ["setup_save_hub","save_hub","paste_hub_invite"].contains(frame.source_op ?? ""))
            if let warning = frame.warning, !warning.isEmpty, frame.source_op != "channel_test",
               !(frame.source_op == "set_host_hub" && presentationError(value.host_hub.error ?? "") == presentationError(warning)) { settingsBannerError = warning }
            if changed || receipt || uiChanged || frame.request_id == hostPort.requestId { rebuildSettingsPanel(preservingEditor: !receipt) }
            return
        }
        if frame.op == "pair_code", frame.authority_changed != true, let id = frame.channel_id {
            let hub = receiptHub ?? effectiveChannelHub
            let key="\(hub)|\(id)"
            guard let request = frame.request_id, var ui = channelUiStates[key], ui.pendingPairRequest == request else { return }
            pendingControls.removeValue(forKey: request)
            ui.pendingPairRequest = nil
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
            let h = preserveCollections && hasRenderedState ? hubs.map { old in
                guard var updated = observedHubs.first(where: { $0.name == old.name }) else { return old }
                updated.attachments = old.attachments
                return updated
            } : observedHubs
            let l = preserveCollections && hasRenderedState ? listeners : observedListeners
            if let hasAny = frame.activity_has_any { activityHasAny = hasAny }
            let a = preserveCollections && hasRenderedState ? available : frame.available ?? []
            let nextDiscovery = preserveCollections && hasRenderedState ? discovery : frame.discovery ?? discovery
            let discoveryChanged = discovery != nextDiscovery
            discovery = nextDiscovery
            let r = preserveCollections && hasRenderedState ? myRemote : (frame.my_remote_listeners ?? []).filter { $0.harness != "human" }
            let onlineCount = frame.directory_online_count ?? max(directoryOnlineCount, r.count)
            let nextUnread = frame.unread_count ?? unreadCount
            let nextActivity = preserveCollections ? activityAllItems : frame.activity ?? activityAllItems
            let activityChanged = activityPresenceChanged || nextUnread != unreadCount || nextActivity != activityAllItems ||
                (frame.activity_total != nil && frame.activity_total != activityTotal) ||
                (frame.activity_unread_count ?? frame.unread_count ?? activityUnreadCount) != activityUnreadCount ||
                frame.activity_filter != serverActivityFilter || (frame.activity_query ?? "") != serverActivityQuery
            activityTotal = frame.activity_total ?? activityTotal
            activityUnreadCount = frame.activity_unread_count ?? frame.unread_count ?? activityUnreadCount
            serverActivityFilter = frame.activity_filter
            serverActivityQuery = frame.activity_query ?? ""
            unreadCount = nextUnread
            activityAllItems = nextActivity
            let directoryMembershipChanged = h != hubs || l != listeners
            let nextHarnesses = preserveCollections && hasRenderedState ? harnesses : frame.harnesses ?? harnesses
            let harnessesChanged = nextHarnesses != harnesses
            if harnessesChanged { harnesses = nextHarnesses }
            let nextSettings = preserveCollections && settings != nil ? settings : frame.settings ?? settings
            // An unchanged save acknowledgement still needs to refresh the settings feedback.
            let settingsChanged = nextSettings != settings || (hostPort.pending && frame.request_id == hostPort.requestId)
            if settingsChanged, let value = nextSettings { acceptSettings(value, hub: frame.hub ?? value.hubs.first(where: { $0.is_default })?.name ?? h.first?.name, savedRequest: frame.request_id) }
            if hasRenderedState && !settledControl && !discoveryChanged && !pendingMutation && !harnessesChanged && !settingsChanged && !activityChanged && h == hubs && l == listeners && a == available && r == myRemote && onlineCount == directoryOnlineCount {
                updateStatusItem()
            } else {
                hubs = h; listeners = l; available = a; myRemote = r; directoryOnlineCount = onlineCount
                seedDirectoryPeersIfEmpty()
                hasRenderedState = true; pendingMutation = false
                rebuild()
                refreshDirectoryViews()
                if directoryVisible && directoryMembershipChanged { queryDirectory() }
                if settingsChanged || settledControl || directoryMembershipChanged {
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
        } else if frame.op == "directory", let observedPeers = frame.peers {
            let peers = preserveCollections ? directoryAllPeers : observedPeers
            guard frame.request_id == directoryRequestId && frame.query == directoryQuery && frame.scope == directoryScopeFilter && frame.harness == directoryHarnessFilter else { return }
            if let hasAny = frame.activity_has_any { activityHasAny = hasAny }
            let menuChanged = activityPresenceChanged || (frame.unread_count ?? unreadCount) != unreadCount ||
                (frame.online_count ?? peers.count) != directoryOnlineCount
            let rowsChanged = activityPresenceChanged || (frame.unread_count ?? unreadCount) != unreadCount ||
                serverDirectoryQuery == nil || serverDirectoryQuery != directoryQuery ||
                serverDirectoryScope != directoryScopeFilter || serverDirectoryHarness != directoryHarnessFilter ||
                directoryAllPeers != peers || serverDirectoryMatchedCount != (frame.matched_count ?? peers.count) ||
                directoryOnlineCount != (frame.online_count ?? peers.count) ||
                (frame.activity ?? activityAllItems) != activityAllItems ||
                (frame.activity_total ?? activityTotal) != activityTotal ||
                (frame.activity_unread_count ?? frame.unread_count ?? activityUnreadCount) != activityUnreadCount ||
                frame.activity_filter != serverActivityFilter || (frame.activity_query ?? "") != serverActivityQuery
            let warningsChanged = directoryQueryError != nil || (frame.warnings ?? directoryWarnings) != directoryWarnings
            activityTotal = frame.activity_total ?? activityTotal
            activityUnreadCount = frame.activity_unread_count ?? frame.unread_count ?? activityUnreadCount
            directoryWarnings = frame.warnings ?? directoryWarnings
            directoryQueryError = nil
            if warningsChanged { updateDirectoryWarnings() }
            serverActivityFilter = frame.activity_filter
            serverActivityQuery = frame.activity_query ?? ""
            directoryAllPeers = peers
            serverDirectoryQuery = directoryQuery
            serverDirectoryScope = directoryScopeFilter
            serverDirectoryHarness = directoryHarnessFilter
            serverDirectoryMatchedCount = frame.matched_count ?? peers.count
            directoryOnlineCount = frame.online_count ?? peers.count
            if !preserveCollections, let act = frame.activity { activityAllItems = act }
            if let u = frame.unread_count { unreadCount = u }
            if rowsChanged { refreshDirectoryViews() }
            if hasRenderedState && (menuChanged || settledControl) { rebuild() }
            if headless { diagnostic("directory peers=\(peers.count) matched=\(directoryMatchedCount) online=\(directoryOnlineCount)") }
        }
    }

    func emit(_ action: [String: Any], userInitiated: Bool = true) {
        let operation = action["op"] as? String ?? ""
        let passive = ["query_directory", "refresh_available"].contains(operation)
        if userInitiated && !passive && menuBannerError != nil { menuBannerError = nil; rebuild() }
        var outgoing = action
        let tracked = ["set_host_hub", "channel_test", "channel_pair", "channel_save", "save_hub_service", "channel_toggle", "channel_unpair", "channel_cancel_pair", "disconnect_human_channel", "clear_channel_service", "human_inbox_update", "unlisten_all", "setup_apply", "setup_remove_app", "setup_remove_hub", "set_scope", "set_auto_listen", "unlisten_session", "set_listen_mode", "rename_session", "hub_action", "setup_toggle_hub", "set_launch_at_login", "setup_disconnect_all", "listen_session"]
        if tracked.contains(operation) || directoryActionOps.contains(operation) {
            outgoing["request_id"] = (action["request_id"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? UUID().uuidString
        }
        guard let data = try? JSONSerialization.data(withJSONObject: outgoing, options: [.sortedKeys]), data.count <= 1_048_576 else {
            let message = "This action is too large to send. Shorten the input and try again."
            if operation == "setup_save_hub" { hubForm.pendingSave = nil; hubForm.error = message }
            else if operation == "set_host_hub" { hostPort.pending = false; hostPort.error = message }
            else if operation == "paste_hub_invite" { hubInvitePending = false; hubForm.parseRequest = ""; settingsBannerError = message }
            else if ["send_signal", "send_reply_from_human"].contains(operation) { pendingComposer = nil; setComposerStatus(message) }
            else { settingsBannerError = message; menuBannerError = message }
            rebuild(); if settingsPanel != nil { rebuildSettingsPanel(preservingEditor: true) }
            return
        }
        if tracked.contains(operation), let request = outgoing["request_id"] as? String {
            let channel = outgoing["channel_id"] as? String
            let hub = outgoing["hub"] as? String ?? (channel == nil ? "" : effectiveChannelHub)
            let session = (outgoing["session_id"] as? String).map { "\(outgoing["harness"] as? String ?? ""):\($0)" }
            let hostTarget = action["auth_mode"] != nil ? "access" : (request == hostPort.requestId ? "port" : "sharing")
            let target = channel ?? session ?? outgoing["harness"] as? String ?? (operation == "set_host_hub" ? hostTarget : "")
            pendingControls[request] = PendingControl(op: operation, hub: hub, target: target)
        }
        if directoryActionOps.contains(operation), let request = outgoing["request_id"] as? String {
            // Full-message copy already owns this ID through its multipart buffer.
            latestDirectoryActionRequestId = request
            clearDirectoryActionError()
        }
        if operation == "channel_test", let id = action["channel_id"] as? String {
            let key = "\(action["hub"] as? String ?? effectiveChannelHub)|\(id)"
            channelUiStates[key, default: ChannelUiState()].testResultIsLatest = false
        }
        if operation == "channel_pair", let id=action["channel_id"] as? String, let request=outgoing["request_id"] as? String {
            channelUiStates["\(action["hub"] as? String ?? effectiveChannelHub)|\(id)", default: ChannelUiState()].pendingPairRequest = request
        }
        if let op = action["op"] as? String, ["channel_save", "save_hub_service"].contains(op),
           let id = action["channel_id"] as? String, let values = action["values"] as? [String: String],
           let requestId = outgoing["request_id"] as? String {
            let hub = action["hub"] as? String ?? effectiveChannelHub
            let scope = (action["scope"] as? String) ?? (op == "save_hub_service" ? "hub" : "user")
            // Supersede only this scope's older intent, never another scope's save.
            pendingChannelValues = pendingChannelValues.filter { $0.value.hub != hub || $0.value.channelId != id || $0.value.scope != scope }
            pendingChannelValues[requestId] = (hub: hub, channelId: id, scope: scope, values: values)
        }
        if let op = action["op"] as? String,
           ["channel_save", "save_hub_service", "channel_pair", "channel_toggle", "channel_unpair", "channel_cancel_pair", "disconnect_human_channel", "clear_channel_service"].contains(op),
           let id = action["channel_id"] as? String {
            let key = "\(action["hub"] as? String ?? effectiveChannelHub)|\(id)"
            var needsRebuild = channelUiStates[key]?.testResult != nil
            needsRebuild = needsRebuild || ["channel_pair", "channel_unpair", "channel_cancel_pair", "clear_channel_service", "disconnect_human_channel"].contains(op)
            channelUiStates[key, default: ChannelUiState()].testResult = nil
            // Preserve native field editors and undo history when a submission
            // changes no presentation; cancellation must redraw immediately.
            if needsRebuild && settingsPanel != nil { rebuildSettingsPanel() }
        }
        if ["channel_test", "channel_pair", "channel_save", "save_hub_service", "channel_toggle", "channel_unpair", "channel_cancel_pair", "disconnect_human_channel", "clear_channel_service"].contains(operation),
           let id = outgoing["channel_id"] as? String, let request = outgoing["request_id"] as? String {
            let hub = outgoing["hub"] as? String ?? effectiveChannelHub
            if ["channel_unpair", "channel_cancel_pair", "clear_channel_service", "disconnect_human_channel"].contains(operation) {
                pendingChannelResets[request] = (all: operation == "clear_channel_service" || outgoing["reset_hub"] as? Bool == true, submitted: channelUiStates["\(hub)|\(id)"] ?? ChannelUiState())
            }
            if ["channel_save", "save_hub_service"].contains(operation),
               let root = settingsPanel?.contentView,
               let save = findControl(in:root,id:operation == "channel_save" ? "save_channel:\(id)" : "save_hub_service:\(id)") {
                // Pending feedback changes no fields; retain the editor's undo stack.
                pendingLabel(save,true,"Saving…"); save.layoutContents()
            } else if settingsPanel != nil { rebuildSettingsPanel() }
        }
        if ["set_host_hub", "hub_action", "setup_toggle_hub", "setup_remove_hub", "human_inbox_update", "unlisten_all", "setup_apply", "setup_remove_app", "listen_session"].contains(operation) {
            rebuild(); if settingsPanel != nil { rebuildSettingsPanel(preservingEditor: true) }
        }
        FileHandle.standardOutput.write(data + Data([10]))
    }
    func controlTextDidEndEditing(_ obj: Notification) {
        // Wait until AppKit releases the shared field editor before replacing views.
        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }
            if self.settingsRebuildDeferred { self.rebuildSettingsPanel(preservingEditor: true) }
            if self.menuRebuildDeferred { self.rebuild() }
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
                applyDirectoryFilter(fromDaemonFrame: false, daemonMatchedCount: nil)
            }
            queryDirectory()
            return
        }
        if let field = obj.object as? NSTextField, field === composerInputField { composerDraftIdempotencyKey = UUID().uuidString }
        if let field=obj.object as? NSTextField, field.identifier?.rawValue == "host_hub_port" { hostPort.draft=field.stringValue; hostPort.savedUntil=nil; hostPort.error=nil; if let root=settingsPanel?.contentView { findControl(in:root,id:"apply_host_hub_port")?.isEnabled = !hostPort.pending && Int(field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)) != settings?.host_hub.port } }
        if let field = obj.object as? NSTextField, let id = field.identifier?.rawValue {
            if id.hasPrefix("channel_field:"),
               let ch=(channelSettingsByHub[effectiveChannelHub] ?? []).first(where:{id.hasPrefix("channel_field:\($0.id):")}),
               let schema=ch.fields.first(where:{id == "channel_field:\(ch.id):\($0.key)"}) {
                let key = "\(effectiveChannelHub)|\(ch.id)"
                channelUiStates[key, default: ChannelUiState()].drafts[schema.key] = field.stringValue
                if let root = settingsPanel?.contentView {
                    let op = schema.scope == "hub" ? "save_hub_service" : "channel_save"
                    let id = schema.scope == "hub" ? "save_hub_service:\(ch.id)" : "save_channel:\(ch.id)"
                    if let save = findControl(in:root,id:id) {
                        if channelSavePending(op,hub:effectiveChannelHub,channel:ch.id,scope:schema.scope) {
                            pendingLabel(save,true,"Saving…")
                        } else {
                            save.feedbackTimer?.invalidate(); save.feedbackTimer=nil
                            save.isEnabled = true
                            let title = schema.scope == "hub" ? "Save Gateway Token" : "Save"
                            save.label.stringValue=title; save.setAccessibilityLabel(title); save.toolTip=title
                        }
                        save.layoutContents()
                    }
                }
                if let root=settingsPanel?.contentView, let test=findControl(in:root,id:"test_channel:\(ch.id)") {
                    let visibleFields = ch.fields.filter { $0.hidden != true && $0.scope != "hub" && settingsFieldControls["channel_field:\(ch.id):\($0.key)"] != nil }
                    test.isEnabled = !isPending("channel_test",hub:effectiveChannelHub,target:ch.id) && ch.canSendTest(drafts:channelUiStates[key]?.drafts ?? [:],visibleFields:visibleFields)
                }
            }
            else if id.hasPrefix("settings_hub_") { hubForm.generation += 1; hubForm.parseRequest="" }
        }
        syncAddHubFieldsFromControls()
        if let field=obj.object as? NSTextField, field.identifier?.rawValue == "settings_hub_url" {
            let raw=field.stringValue.trimmingCharacters(in:.whitespacesAndNewlines)
            if raw.hasPrefix("cito setup ") || raw.hasPrefix("{") {
                hubForm.parseRequest=UUID().uuidString
                emit(["op":"paste_hub_invite","invite":raw,"parse_only":true,"request_id":hubForm.parseRequest])
            }
        }
        settingsFieldControls["settings_hub_name"]?.placeholderString = hubNamePlaceholder
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
                    rebuild(); return true
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
