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
        if oldValue != settingsActiveTab { settingsBannerError = nil; hostHubPortError = nil }
    } }
    var settingsPanel: KeyPanel?
    var addHubFormOpen = false
    var addHubError: String?
    struct PendingHubSave {
        let name: String
        let requestId: String
        let generation: Int
    }
    var pendingHubSave: PendingHubSave?
    var settingsBannerError: String?
    var menuBannerError: String?
    var inviteParseRequest = ""
    var hubInvitePending = false
    var hubInviteRequest = ""
    var hubInvitePastedUntil = Date.distantPast
    var editingExistingHub = false
    var editingOriginalHub = ""
    var hostHubPortPending = false
    var hostHubPortSaved = false
    var hostHubPortSavedUntil = Date.distantPast
    var hostHubPortError: String?
    var hostHubPortDraft: String?
    var submittedHostHubPort: Int?
    var submittedHostHubRequestId = ""
    var addHubName = ""
    var addHubUrl = ""
    var addHubDraftGeneration = 0
    var addHubAuth = "none"
    var addHubToken = ""
    var addHubUser = ""
    var addHubIssuer = ""
    var addHubClientId = ""
    var settingsFieldControls: [String: NSTextField] = [:]
    var channelUiStates: [String: ChannelUiState] = [:]
    var appCheckedUntil: [String: Date] = [:]
    var pendingAppChecks: [String: String] = [:]
    struct PendingControl {
        let op: String, hub: String, target: String
    }
    var pendingControls: [String: PendingControl] = [:]
    func isPending(_ op: String, hub: String = "", target: String = "") -> Bool {
        pendingControls.values.contains { $0.op == op && $0.hub == hub && $0.target == target }
    }
    var hostHubAddress = "tailscale"
    var pendingChannelValues: [String: (hub: String, channelId: String, scope: String, values: [String: String])] = [:]
    var selectedChannelHub = ""
    var channelSettingsByHub: [String: [ChannelSetupItem]] = [:]
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
        guard let editor = window?.firstResponder as? NSTextView else { return false }
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
    func receive(_ data: Data) {
        if handleHeadlessTestCommand(data) { return }
        if let object=(try? JSONSerialization.jsonObject(with:data)) as? [String:Any], object["op"] as? String == "hub_invite" {
            guard !inviteParseRequest.isEmpty, object["request_id"] as? String == inviteParseRequest, let cfg=object["config"] as? [String:Any] else{return}
            inviteParseRequest=""; addHubError=nil
            addHubUrl=cfg["url"] as? String ?? ""; addHubName=cfg["name"] as? String ?? ""
            addHubAuth=cfg["authMode"] as? String ?? cfg["auth_mode"] as? String ?? "none"
            addHubToken=cfg["token"] as? String ?? ""; addHubUser=cfg["user"] as? String ?? ""
            addHubIssuer=cfg["issuer"] as? String ?? cfg["oidcIssuer"] as? String ?? ""; addHubClientId=cfg["clientId"] as? String ?? cfg["client_id"] as? String ?? cfg["oidcClientId"] as? String ?? ""
            settingsFieldControls.removeAll(); rebuildSettingsPanel(); return
        }
        guard let frame = try? JSONDecoder().decode(Frame.self, from: data) else { diagnostic("invalid frame"); return }
        var settledControl = false
        if let request = frame.request_id, let pending = pendingControls[request],
           frame.source_op == pending.op || frame.op == "error" || frame.op == "pair_code" || frame.op == "channel_test_result" {
            pendingControls.removeValue(forKey: request); settledControl = true
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
        if frame.op == "error" || actionFailure {
            if let request = frame.request_id { pendingAppChecks.removeValue(forKey: request) }
            let sourceOp = frame.source_op ?? ""
            if sourceOp == "paste_hub_invite", let request = frame.request_id {
                if request == inviteParseRequest { inviteParseRequest=""; return }
                guard request == hubInviteRequest else { return }
                hubInvitePending = false
            }
            // A command may emit both its result and a trailing generic error.
            // Neither can settle a retry or overwrite its feedback.
            if ["send_signal", "send_reply_from_human"].contains(sourceOp),
               (pendingComposer == nil || frame.request_id != pendingComposer?.requestId) { return }
            if sourceOp == "set_host_hub", let requestId = frame.request_id,
               requestId != submittedHostHubRequestId { return }
            let isAddHubOp = ["setup_save_hub", "save_hub"].contains(sourceOp)
            if isAddHubOp, let requestId = frame.request_id,
               requestId != pendingHubSave?.requestId { return }
            // Only the matching save result owns an active form submission.
            // Untagged invite/transport failures remain visible in the settings banner.
            let isAddHubError: Bool
            if let pending = pendingHubSave {
                isAddHubError = isAddHubOp && frame.request_id == pending.requestId
            } else {
                isAddHubError = addHubFormOpen && (isAddHubOp || sourceOp.isEmpty)
            }
            let error = frame.error ?? "Unknown daemon error"
            if ["switch_hub", "set_scope", "listen_session", ""].contains(sourceOp) { pendingMutation = false }
            if (sourceOp == "set_host_hub" || sourceOp.isEmpty) &&
               frame.request_id == submittedHostHubRequestId {
                hostHubPortPending = false
                submittedHostHubRequestId = ""
            }
            if (["paste_hub_invite", "setup_save_hub", "save_hub"].contains(sourceOp) && frame.request_id == nil) || sourceOp.isEmpty {
                hubInvitePending=false; hubInvitePastedUntil = .distantPast
            }
            if isAddHubError { pendingHubSave = nil }
            let isComposerError = ["send_signal", "send_reply_from_human", ""].contains(sourceOp) &&
                pendingComposer != nil && frame.request_id == pendingComposer?.requestId
            if isComposerError {
                setComposerStatus(error)
                pendingComposer = nil
            } else if isAddHubError { addHubError = error }
            else if ["query_directory", "open_directory"].contains(sourceOp) {
                if frame.request_id == nil || frame.request_id == directoryRequestId { directoryQueryError = error; updateDirectoryWarnings() }
            }
            else if directoryVisible && directoryActionOps.contains(sourceOp) {
                directoryActionError = error; directoryActionErrorSourceOp = sourceOp; updateDirectoryWarnings()
            }
            else if settingsVisible || pendingHubSave != nil { settingsBannerError = error }
            else { menuBannerError = error }
            diagnostic("error \(error)")
            rebuild()
            if settingsPanel != nil { rebuildSettingsPanel(preservingEditor: true) }
            return
        }
        if frame.op == "settings", let value = frame.settings {
            let target = value.settings_hub ?? value.channel_hub ?? frame.hub ?? effectiveChannelHub
            let changed = value != settings || channelSettingsByHub[target] != (value.channels ?? [])
            let receipt = settledControl || (frame.request_id != nil && frame.source_op != nil)
            if let request = frame.request_id, let app = pendingAppChecks.removeValue(forKey: request),
               frame.ok == true && frame.source_op == "setup_apply" {
                appCheckedUntil[app] = Date().addingTimeInterval(1.5)
                rebuildFeedbackAfter(1.5)
            }
            if hubInvitePending && frame.request_id == hubInviteRequest && frame.ok == true && ["setup_save_hub","save_hub"].contains(frame.source_op ?? "") {
                hubInvitePending=false; hubInvitePastedUntil=Date().addingTimeInterval(1.2); rebuildFeedbackAfter(1.2)
            }
            let saved = frame.ok == true && ["channel_save","save_hub_service"].contains(frame.source_op ?? "") ? frame.channel_id : nil
            let uiChanged = acceptSettings(value, hub: frame.hub, savedChannel: saved, savedRequest: frame.request_id, confirmHubSave: frame.ok == true && ["setup_save_hub","save_hub"].contains(frame.source_op ?? ""))
            if changed || receipt || uiChanged || frame.request_id == submittedHostHubRequestId { rebuildSettingsPanel(preservingEditor: true) }
            return
        }
        if frame.op == "pair_code", let id = frame.channel_id {
            let hub = frame.hub ?? effectiveChannelHub
            let key="\(hub)|\(id)"
            guard let request = frame.request_id, var ui = channelUiStates[key], ui.pendingPairRequest == request else { return }
            ui.pendingPairRequest = nil
            ui.pairingInstruction = frame.instruction ?? frame.code ?? ""
            ui.pairingCommand = frame.pairing_command ?? frame.instruction
            ui.pairingURL = frame.pairing_url
            if let command = frame.pairing_command { copyText(command); ui.pairCopiedUntil = Date().addingTimeInterval(1.2); rebuildFeedbackAfter(1.2) }
            channelUiStates[key] = ui
            if settingsPanel != nil { rebuildSettingsPanel(preservingEditor: true) }
            return
        }
        if frame.op == "channel_test_result", let id = frame.channel_id {
            let key = "\(frame.hub ?? effectiveChannelHub)|\(id)"
            channelUiStates[key, default: ChannelUiState()].testResult = (ok: frame.ok == true, detail: frame.detail ?? frame.error ?? (frame.ok == true ? "Test sent" : "Test failed"))
            channelUiStates[key, default: ChannelUiState()].testResultIsLatest = true
            if settingsPanel != nil { rebuildSettingsPanel(preservingEditor: true) }
            return
        }
        if frame.op == "state", let h = frame.hubs, let l = frame.listeners,
           h.allSatisfy({ ["connected", "connecting", "auth_required", "disconnected", "unreachable", "error"].contains($0.status) && ["none", "token", "oidc"].contains($0.auth_mode) }),
           l.allSatisfy({ ["self", "workspace"].contains($0.scope) }) {
            if let hasAny = frame.activity_has_any { activityHasAny = hasAny }
            let a = frame.available ?? []
            let discoveryChanged = discovery != (frame.discovery ?? discovery)
            discovery = frame.discovery ?? discovery
            let r = (frame.my_remote_listeners ?? []).filter { $0.harness != "human" }
            let onlineCount = frame.directory_online_count ?? max(directoryOnlineCount, r.count)
            let nextUnread = frame.unread_count ?? unreadCount
            let nextActivity = frame.activity ?? activityAllItems
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
            let nextHarnesses = frame.harnesses ?? harnesses
            let harnessesChanged = nextHarnesses != harnesses
            if harnessesChanged { harnesses = nextHarnesses }
            let nextSettings = frame.settings ?? settings
            // An unchanged save acknowledgement still needs to refresh the settings feedback.
            let settingsChanged = nextSettings != settings || (hostHubPortPending && frame.request_id == submittedHostHubRequestId)
            if settingsChanged, let value = nextSettings { acceptSettings(value, hub: frame.hub ?? value.hubs.first(where: { $0.is_default })?.name ?? h.first?.name, savedRequest: frame.request_id) }
            if let error = frame.composer_error, !error.isEmpty { setComposerStatus(error) }
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
                    rebuildSettingsPanel(preservingEditor: true)
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
            let scopeChanged = listeners.contains { listener in
                guard listener.harness != "human", hubs.first(where: { $0.name == listener.hub })?.auth_mode != "oidc" else { return false }
                let user = [listener.user, listener.owner_user, hubs.first { $0.name == listener.hub }?.user]
                    .compactMap { $0 }.first { !$0.isEmpty } ?? hubs.first { $0.name == effectiveTargetHub }?.user ?? "local"
                guard !listeners.contains(where: { $0.hub == listener.hub && !($0.user ?? "").isEmpty && $0.user != user }) else { return false }
                return peers.contains(where: { $0.hub == listener.hub && !$0.user.isEmpty && $0.user != user }) !=
                    directoryAllPeers.contains(where: { $0.hub == listener.hub && !$0.user.isEmpty && $0.user != user })
            }
            let menuChanged = activityPresenceChanged || (frame.unread_count ?? unreadCount) != unreadCount ||
                (frame.online_count ?? peers.count) != directoryOnlineCount || scopeChanged
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
            if let act = frame.activity { activityAllItems = act }
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
        if directoryActionOps.contains(operation) {
            // Full-message copy already owns this ID through its multipart buffer.
            let request = (action["request_id"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? UUID().uuidString
            outgoing["request_id"] = request
            latestDirectoryActionRequestId = request
            clearDirectoryActionError()
        }
        if operation == "channel_test", let id = action["channel_id"] as? String {
            let key = "\(action["hub"] as? String ?? effectiveChannelHub)|\(id)"
            channelUiStates[key, default: ChannelUiState()].testResultIsLatest = false
        }
        if operation == "channel_pair", let id=action["channel_id"] as? String {
            let request=UUID().uuidString
            outgoing["request_id"]=request
            channelUiStates["\(action["hub"] as? String ?? effectiveChannelHub)|\(id)", default: ChannelUiState()].pendingPairRequest = request
        }
        if let op = action["op"] as? String, ["channel_save", "save_hub_service"].contains(op),
           let id = action["channel_id"] as? String, let values = action["values"] as? [String: String] {
            let requestId = UUID().uuidString
            outgoing["request_id"] = requestId
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
            switch op {
            case "channel_pair": needsRebuild = true
            case "channel_unpair", "channel_cancel_pair":
                channelUiStates[key, default: ChannelUiState()].resetPersonal()
                needsRebuild = true
            case "clear_channel_service":
                channelUiStates[key, default: ChannelUiState()].resetAll()
                needsRebuild = true
            case "disconnect_human_channel":
                if action["reset_hub"] as? Bool == true { channelUiStates[key, default: ChannelUiState()].resetAll() }
                else { channelUiStates[key, default: ChannelUiState()].resetPersonal() }
                needsRebuild = true
            default: break
            }
            channelUiStates[key, default: ChannelUiState()].testResult = nil
            // Preserve native field editors and undo history when a submission
            // changes no presentation; cancellation must redraw immediately.
            if needsRebuild && settingsPanel != nil { rebuildSettingsPanel() }
        }
        if ["human_inbox_update", "unlisten_all", "setup_apply", "setup_remove_app"].contains(operation) {
            let request = outgoing["request_id"] as? String ?? UUID().uuidString
            outgoing["request_id"] = request
            pendingControls[request] = PendingControl(op: operation, hub: outgoing["hub"] as? String ?? "", target: outgoing["harness"] as? String ?? outgoing["channel_id"] as? String ?? "")
            if settingsPanel != nil { rebuildSettingsPanel() }
            rebuild()
        }
        guard let data = try? JSONSerialization.data(withJSONObject: outgoing, options: [.sortedKeys]) else { return }
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
        if let field=obj.object as? NSTextField, field.identifier?.rawValue == "host_hub_port" { hostHubPortDraft=field.stringValue; hostHubPortSaved=false; hostHubPortError=nil; if let root=settingsPanel?.contentView { findControl(in:root,id:"apply_host_hub_port")?.isEnabled = !hostHubPortPending && Int(field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)) != settings?.host_hub.port } }
        if let field = obj.object as? NSTextField, let id = field.identifier?.rawValue {
            if id.hasPrefix("channel_field:"),
               let ch=(channelSettingsByHub[effectiveChannelHub] ?? []).first(where:{id.hasPrefix("channel_field:\($0.id):")}),
               let schema=ch.fields.first(where:{id == "channel_field:\(ch.id):\($0.key)"}) {
                let key = "\(effectiveChannelHub)|\(ch.id)"
                channelUiStates[key, default: ChannelUiState()].drafts[schema.key] = field.stringValue
                if ch.pairing_manual_disclosure == true, !ch.configured, let root=settingsPanel?.contentView, let test=findControl(in:root,id:"test_channel:\(ch.id)") {
                    test.isEnabled=ch.fields.filter{$0.scope != "hub" && $0.hidden != true}.contains { f in
                        let value=channelUiStates[key]?.drafts[f.key] ?? f.value ?? ""
                        return !value.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty
                    }
                }
            }
            else if id.hasPrefix("settings_hub_") { addHubDraftGeneration += 1; inviteParseRequest="" }
        }
        syncAddHubFieldsFromControls()
        if let field=obj.object as? NSTextField, field.identifier?.rawValue == "settings_hub_url" {
            let raw=field.stringValue.trimmingCharacters(in:.whitespacesAndNewlines)
            if raw.hasPrefix("cito setup ") || raw.hasPrefix("{") {
                inviteParseRequest=UUID().uuidString
                emit(["op":"paste_hub_invite","invite":raw,"parse_only":true,"request_id":inviteParseRequest])
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
            if commandSelector == #selector(NSResponder.insertNewline(_:)) { handleSettingsKey("return"); return true }
            if commandSelector == #selector(NSResponder.cancelOperation(_:)) {
                handleSettingsKey("escape")
                return true
            }
            return false
        }
        if commandSelector == #selector(NSResponder.moveDown(_:)) { handleDirectoryKey("down"); return true }
        if commandSelector == #selector(NSResponder.moveUp(_:)) { handleDirectoryKey("up"); return true }
        if commandSelector == #selector(NSResponder.insertNewline(_:)) { handleDirectoryKey(NSApp.currentEvent?.modifierFlags.contains(.shift) == true ? "shift-return" : "return"); return true }
        if commandSelector == #selector(NSResponder.cancelOperation(_:)) { handleDirectoryKey("escape"); return true }
        return false
    }
}
