import AppKit
import Darwin

final class Companion: NSObject, NSSearchFieldDelegate, NSTextFieldDelegate, NSWindowDelegate, NSApplicationDelegate {
    private enum QuitState { case running, awaitingDaemon, daemonClosedPipe }
    private var quitState = QuitState.running
    let headless = ProcessInfo.processInfo.environment["CITO_UI_HEADLESS"] == "1"
    var status: NSStatusItem?
    var headlessStatusButton: NSButton?
    weak var menuHubStrip: SegmentedPillStrip?
    let cardMenu: NSMenu = ProcessInfo.processInfo.environment["CITO_UI_HEADLESS"] == "1" ? HeadlessMenu() : NSMenu()
    var hubDrawerVisibility: [String: Bool] = [:]
    var menuScrollOffsets: [String: NSPoint] = [:]
    var menuScrollKeys: [String: String] = [:]
    var menuScrollAnchors: [String: (rowId: String, deltaY: CGFloat)] = [:]
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
    }
    var pendingMessageCopy: PendingMessageCopy?
    var activityTotal: Int?
    var activityHasAny = false
    var activityUnreadCount = 0
    var serverActivityFilter: String?
    var serverActivityQuery = ""
    func copyText(_ text: String, control: MenuControl? = nil, feedback: String = "✓ Copied",
                  key: TransientFeedbackStore.Key? = nil) {
        if let pending = pendingMessageCopy, latestDirectoryActionRequestId == pending.requestId { latestDirectoryActionRequestId = nil }
        pendingMessageCopy = nil
        appPasteboard.clearContents()
        appPasteboard.setString(text, forType: .string)
        if let key {
            self.feedback.set(key, until: Date().addingTimeInterval(1.2))
            control?.showFeedback(feedback, key: key)
        } else { control?.copied(feedback) }
    }
    func readPasteboardString() -> String {
        appPasteboard.string(forType: .string) ?? ""
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
    let feedback = TransientFeedbackStore.shared
    var settingsDeferredTabs: Set<SettingsTab> = []
    var settingsRebuildDeferred: Bool { !settingsDeferredTabs.isEmpty }
    var menuRebuildDeferred = false
    var headlessMenuPanel: KeyWindow?
    var directoryVisible = false
    var directoryQuery = ""
    var directoryScopeFilter = "all" { didSet { if oldValue != directoryScopeFilter { clearDirectoryActionError() } } }
    var directoryHarnessFilter = "all" { didSet { if oldValue != directoryHarnessFilter { clearDirectoryActionError() } } }
    var directorySelectedIndex = 0
    var restoringDirectorySelection = false
    var directoryPanel: KeyWindow?
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
    private func showComposerFailure(_ error: String?, for submission: PendingComposerSend) {
        if composerDraftIdempotencyKey == submission.composerDraftIdempotencyKey {
            showComposerFailure(error)
        } else {
            showComposerDirectoryNotice(error ?? "Send failed.")
        }
    }
    var pendingComposer: PendingComposerSend? { didSet {
        if let send = composerSendButton {
            send.reservedTitles = [("Send ⏎", nil), ("Sending…", nil)]
            if pendingComposer == nil {
                send.clearFeedback(title: "Send ⏎", accessibilityLabel: "Send message (Return)")
                send.isEnabled = true
                send.toolTip = "Send message (Return)"
            } else {
                send.setPending("Sending…")
                send.toolTip = "Sending message…"
                send.setAccessibilityLabel(send.toolTip)
            }
            send.frame.size.width = send.reservedWidth
        }
        layoutComposer()
    } }
    var settings: SettingsPayload? { didSet {
        let subscribed = settingsChannels.subscribedHub
        if let old = oldValue?.hubs.first(where: { $0.name == subscribed }),
           !old.sharesChannelAuthority(with: settings?.hubs.first { $0.name == subscribed }) {
            settingsChannels.subscribedHub = ""
        }
    } }
    var settingsVisible = false
    var settingsActiveTab = "apps" { didSet {
        if oldValue != settingsActiveTab { settingsBannerError = nil; hostPort.error = nil }
    } }
    var settingsPanel: KeyWindow?
    struct PendingHubSave: Equatable {
        let profile: HubFormProfile
        let requestId: String
        let generation: Int
        let nameExplicit: Bool
    }
    struct HubFormState: Equatable {
        enum Mode: Equatable {
            case closed
            case add
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
        var oidcManualExpanded = false

        var hasOIDCDraft: Bool { !issuer.isEmpty || !clientId.isEmpty || !redirectPort.isEmpty }
        var oidcExpanded: Bool {
            if oidcManualExpanded { return true }
            if let initial = initialHub {
                return issuer != initial.issuer || clientId != initial.clientId || redirectPort != initial.redirectPort
            }
            return hasOIDCDraft
        }

        mutating func failSave(_ message: String) {
            pendingSave = nil
            error = message
            if isOpen && auth == "oidc" { oidcManualExpanded = true }
        }

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

    private func rawHTTPHost(_ url: String) -> String? {
        // Do not let Foundation or Network reinterpret an ASCII IPv4 spelling.
        let input = url.unicodeScalars.filter { $0 != "\t" && $0 != "\r" && $0 != "\n" }
        guard let schemeEnd = input.firstIndex(of: ":"),
              ["http", "https"].contains(String(input[..<schemeEnd]).lowercased()) else { return nil }
        let authority = input[input.index(after: schemeEnd)...]
            .drop(while: { $0 == "/" || $0 == "\\" })
            .prefix(while: { $0 != "/" && $0 != "\\" && $0 != "?" && $0 != "#" })
        let hostPort = authority.lastIndex(of: "@").map { authority[authority.index(after: $0)...] } ?? authority
        guard hostPort.first != "[" else { return nil } // IPv6 has no name category.
        let parts = hostPort.split(separator: ":", maxSplits: 1, omittingEmptySubsequences: false)
        if parts.count == 2, !parts[1].isEmpty {
            let port = String(parts[1])
            guard port.utf8.allSatisfy({ (48...57).contains($0) }), UInt16(port) != nil else { return nil }
        }
        guard let rawHost = parts.first, let decoded = String(rawHost).removingPercentEncoding, !decoded.isEmpty,
              !decoded.unicodeScalars.contains(where: { $0.value <= 32 || $0.value == 127 || "%#/:<>?@[\\]^|".unicodeScalars.contains($0) }) else { return nil }
        if decoded.utf8.allSatisfy({ $0 < 128 }) { return decoded.lowercased() }
        // Map IDNA label separators in the raw host before Foundation can reinterpret it.
        let host = String(String.UnicodeScalarView(decoded.unicodeScalars.map { scalar in
            switch scalar {
            case "\u{3002}", "\u{FF0E}", "\u{FF61}": return "."
            default: return scalar
            }
        }))
        if host.utf8.allSatisfy({ $0 < 128 }) { return host.lowercased() }
        // Foundation supplies IDNA conversion for non-ASCII domains only.
        return URL(string: "http://\(host)")?.host?.lowercased()
    }

    private func whatwgIPv4(_ parts: [Substring]) -> UInt32? {
        guard (1...4).contains(parts.count) else { return nil }
        var address: UInt32 = 0
        for (index, part) in parts.enumerated() {
            guard !part.isEmpty else { return nil }
            let hex = part.hasPrefix("0x")
            let digits = hex ? part.dropFirst(2) : part
            let radix = hex ? 16 : part.hasPrefix("0") ? 8 : 10
            guard digits.first != "+", digits.first != "-",
                  let number = digits.isEmpty ? 0 : UInt32(digits, radix: radix) else { return nil }
            if index == parts.count - 1 {
                guard number <= UInt32.max >> (8 * index) else { return nil }
                return address | number
            }
            guard number <= 255 else { return nil }
            address |= number << (8 * (3 - index))
        }
        return nil
    }

    private func automaticHubName(_ url: String) -> String {
        guard let host = rawHTTPHost(url) else { return "remote" }
        var parts = host.split(separator: ".", omittingEmptySubsequences: false)
        // Ignore exactly one final dot before checking whether this is a numeric host.
        if parts.last == "" { parts.removeLast() }
        if let last = parts.last, !last.isEmpty,
           last.utf8.allSatisfy({ (48...57).contains($0) }) ||
           (last.hasPrefix("0x") && last.utf8.dropFirst(2).allSatisfy({ (48...57).contains($0) || (97...102).contains($0) })) {
            // Invalid numeric hosts are not domains (including on overflow).
            guard let ip = whatwgIPv4(parts) else { return "remote" }
            let first = ip >> 24, second = (ip >> 16) & 255
            if first == 10 || (first == 172 && (16...31).contains(second)) || (first == 192 && second == 168) { return "lan" }
            if first == 100 && (64...127).contains(second) { return "tailscale" }
            return "remote"
        }
        let domain = host.trimmingCharacters(in: CharacterSet(charactersIn: "."))
        let labels = domain.split(separator: ".").filter { $0.unicodeScalars.contains { $0.isASCII && CharacterSet.alphanumerics.contains($0) } }
        let genericPrefixes: Set<String> = ["cito", "auth", "sso", "id", "login", "internal", "corp", "www"]
        let label = !domain.hasSuffix(".local") && !domain.hasSuffix(".ts.net") && labels.count >= 3
            ? labels.dropLast().first(where: { !genericPrefixes.contains(String($0)) }) ?? labels[labels.count - 2]
            : labels.first
        return String(label ?? "remote")
    }

    private func normalizeHubName(_ raw: String) -> String {
        // Match Rust's Unicode White_Space trim and preserve valid explicit names.
        let name = raw.replacingOccurrences(of: #"\A\p{White_Space}+|\p{White_Space}+\z"#,
                                            with: "", options: .regularExpression)
        if name == "local" || (name.lowercased() != "local" &&
            name.range(of: #"\A[A-Za-z0-9][A-Za-z0-9_-]{0,31}\z"#, options: .regularExpression) != nil) {
            return name
        }
        let ascii = name.replacingOccurrences(of: "[^A-Za-z0-9]+", with: "-", options: .regularExpression)
        let normalized = String(ascii.drop(while: { $0 == "-" }).prefix(32))
        let slug = normalized.hasSuffix("-") ? String(normalized.dropLast()) : normalized
        return slug.lowercased() == "local" ? "" : slug
    }

    func hubNameProposal(_ raw: String, url: String, existing: [HubConfigItem], original: String = "") -> String {
        if !raw.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return raw == original ? original : normalizeHubName(raw)
        }

        let url = normalizedHubURL(url)
        let slug = automaticHubName(url).lowercased()
            .replacingOccurrences(of: "[^a-z0-9]+", with: "-", options: .regularExpression)
            .trimmingCharacters(in: CharacterSet(charactersIn: "-"))
        let base = slug.isEmpty ? "remote" : String(slug.prefix(32)).trimmingCharacters(in: CharacterSet(charactersIn: "-"))
        var name = base
        var suffix = 2
        while name == "local" || existing.contains(where: { hub in
            hub.name.lowercased() == name && (hub.is_builtin == true ||
                (original.isEmpty ? normalizedHubURL(hub.url) != url : hub.name != original))
        }) {
            let ending = "-\(suffix)"
            name = String(base.prefix(32 - ending.count)).trimmingCharacters(in: CharacterSet(charactersIn: "-")) + ending
            suffix += 1
        }
        return name
    }

    struct HostHubSubmission: Equatable {
        let requestId: String
        let port: Int
    }
    struct HostHubPortState: Equatable {
        var submission: HostHubSubmission?
        var pending: Bool { submission != nil }
        var error: String?
        var draft: String?
    }
    struct HubInviteState: Equatable {
        var requestId: String?
    }
    struct SettingsChannelsState {
        var selectedHub = ""
        var subscribedHub = ""
        var inventory: [String: [ChannelSetupItem]] = [:]
        var errors: [String: String] = [:]
        var editors: [String: ChannelUiState] = [:]
    }
    var hostPort = HostHubPortState()
    var hubInvite = HubInviteState()
    var activeUninstallAlert: NSAlert?
    var settingsChannels = SettingsChannelsState()
    var settingsBannerError: String? { didSet { settingsBannerOwner = nil } }
    var menuBannerError: String? { didSet { menuBannerOwner = nil } }
    enum ActionSurface { case menu, settings }
    struct BannerOwner: Equatable {
        let surface: ActionSurface
        let op: String
        let hub: String
        let target: String
    }
    private var settingsBannerOwner: BannerOwner?
    private var menuBannerOwner: BannerOwner?

    private func actionSurface(for operation: String) -> ActionSurface {
        // Hub authentication is available on both surfaces. Capture tracking at
        // dispatch time, not window visibility when the reply arrives.
        if operation == "hub_action" && menuOpen { return .menu }
        if SettingsTab.affected(by: operation) != nil ||
            ["setup_save_hub", "paste_hub_invite", "set_launch_at_login"].contains(operation) { return .settings }
        return .menu
    }

    private func updateActionBanner(_ message: String?, owner: BannerOwner, advisory: Bool = false) {
        if message == nil && menuBannerOwner == owner { menuBannerError = nil }
        // A committed operation can also report a refresh warning. Keep that
        // advisory readable if its Settings window has already closed.
        let surface: ActionSurface = advisory && !settingsVisible ? .menu : owner.surface
        switch surface {
        case .settings:
            if let message { settingsBannerError = message; settingsBannerOwner = owner }
            else if settingsBannerOwner == owner { settingsBannerError = nil }
        case .menu:
            if let message { menuBannerError = message; menuBannerOwner = owner }
            else if menuBannerOwner == owner { menuBannerError = nil }
        }
    }
    var settingsFieldControls: [String: NSTextField] = [:]
    static let channelResetOps = ["channel_cancel_pair", "channel_unpair", "disconnect_human_channel"]
    static let channelSaveOps = ["channel_save", "save_hub_service"]
    static let channelControlOps = channelResetOps + channelSaveOps + ["channel_pair", "channel_test", "channel_toggle"]
    static let hubOrderOps = ["setup_reorder_hub", "setup_set_default_hub"]
    struct PendingControl {
        struct ChannelSave {
            let scope: String
            let values: [String: String]
        }

        let op: String, hub: String, target: String
        let appCheck: Bool
        let bannerOwner: BannerOwner
        let channelEditor: String?
        // A superseded save still owns its control receipt, but cannot reconcile drafts.
        var channelSave: ChannelSave?
        var channelReset: (hub: Bool, user: Bool, submitted: ChannelUiState)?
    }
    var pendingControls: [String: PendingControl] = [:]
    func isPending(_ op: String, hub: String = "", target: String = "") -> Bool {
        pendingControls.values.contains { $0.op == op && $0.hub == hub && $0.target == target }
    }
    var hostHubAddress = "tailscale"
    var settingsScrollOffsets: [String: CGFloat] = [:]
    var pendingOnboardingMarker: String?
    var settingsKeyMonitor: Any?
    var hasCheckedFirstLaunchOnboarding = false
    var hasRenderedState = false
    var rebuildCount = 0

    func ensureEditMainMenu() {
        let main = NSApp.mainMenu ?? NSMenu()
        if main.items.isEmpty {
            let appItem = NSMenuItem()
            let appMenu = NSMenu(title: "Cito")
            appMenu.addItem(withTitle: "Settings…", action: #selector(openSettingsShortcut), keyEquivalent: ",").target = self
            appMenu.addItem(withTitle: "Directory & Activity…", action: #selector(openDirectoryShortcut), keyEquivalent: "k").target = self
            appMenu.addItem(.separator())
            appMenu.addItem(withTitle: "Hide Cito", action: #selector(NSApplication.hide(_:)), keyEquivalent: "h").target = NSApp
            let hideOthers = appMenu.addItem(withTitle: "Hide Others", action: #selector(NSApplication.hideOtherApplications(_:)), keyEquivalent: "h")
            hideOthers.target = NSApp
            hideOthers.keyEquivalentModifierMask = [.command, .option]
            appMenu.addItem(withTitle: "Show All", action: #selector(NSApplication.unhideAllApplications(_:)), keyEquivalent: "").target = NSApp
            appMenu.addItem(.separator())
            appMenu.addItem(withTitle: "Quit Cito", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q").target = NSApp
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
        if NSApp.windowsMenu == nil {
            let item = NSMenuItem(title: "Window", action: nil, keyEquivalent: "")
            let windows = NSMenu(title: "Window")
            windows.addItem(withTitle: "Close Window", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w")
            windows.addItem(withTitle: "Minimize", action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m")
            windows.addItem(withTitle: "Zoom", action: #selector(NSWindow.performZoom(_:)), keyEquivalent: "")
            windows.addItem(.separator())
            windows.addItem(withTitle: "Bring All to Front", action: #selector(NSApplication.arrangeInFront(_:)), keyEquivalent: "").target = NSApp
            item.submenu = windows; main.addItem(item)
            NSApp.windowsMenu = windows
        }
        if NSApp.mainMenu == nil { NSApp.mainMenu = main }
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        switch quitState {
        case .daemonClosedPipe: return .terminateNow
        case .awaitingDaemon: return .terminateLater
        case .running:
            quitState = .awaitingDaemon
            cardMenu.cancelTracking()
            // Keep the fallback live even if the daemon stops reading stdout.
            // This short pipe write is atomic; no other actions are sent while quitting.
            DispatchQueue.global().async {
                Array("{\"op\":\"quit\"}\n".utf8).withUnsafeBytes { frame in
                    while Darwin.write(STDOUT_FILENO, frame.baseAddress, frame.count) < 0 && errno == EINTR {}
                }
            }
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { [weak self] in
                guard self?.quitState == .awaitingDaemon else { return }
                sender.reply(toApplicationShouldTerminate: true)
            }
            return .terminateLater
        }
    }

    private func finishAfterDaemonEOF() {
        let wasWaiting = quitState == .awaitingDaemon
        quitState = .daemonClosedPipe
        if wasWaiting { NSApp.reply(toApplicationShouldTerminate: true) }
        else { NSApp.terminate(nil) }
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows: Bool) -> Bool {
        let openWindows = [settingsVisible ? settingsPanel : nil, directoryVisible ? directoryPanel : nil].compactMap { $0 }
        let ordered = [sender.keyWindow, sender.mainWindow].compactMap { $0 } + sender.orderedWindows
        if let window = ordered.compactMap({ $0 as? KeyWindow }).first(where: { openWindows.contains($0) }) ?? openWindows.first {
            presentPanel(window)
        } else {
            openSettingsPanel()
        }
        return false
    }

    func restoreAccessoryPolicyIfNoWindows() {
        if !headless && !settingsVisible && !directoryVisible { NSApp.setActivationPolicy(.accessory) }
    }

    func presentPanel(_ window: KeyWindow, focus: (() -> NSView?)? = nil) {
        if !headless && window.isMiniaturized { window.deminiaturize(nil) }
        func orderFront() {
            // Headless panels use the same focus passes without taking over the desktop.
            if !headless {
                NSApp.setActivationPolicy(.regular)
                NSApp.activate(ignoringOtherApps: true)
                window.makeKeyAndOrderFront(nil)
                window.layoutWindowButtons()
            }
            if !(window.firstResponder is NSTextView),
               let view = focus?() ?? (window === self.settingsPanel ? self.hubFormFocusTarget() : nil) {
                window.makeFirstResponder(view)
            }
        }
        orderFront()
        // Status-menu tracking can reclaim key focus as it finishes closing.
        DispatchQueue.main.async { [weak self] in
            guard let self,
                  (window === self.settingsPanel && self.settingsVisible) ||
                  (window === self.directoryPanel && self.directoryVisible) else { return }
            orderFront()
        }
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        guard sender === settingsPanel || sender === directoryPanel else { return true }
        sender.makeFirstResponder(nil)
        if sender === settingsPanel { closeSettingsPanel() }
        else { closeDirectoryPanel() }
        return false
    }

    func allowPanelKey(_ key: String, in window: NSWindow?) -> Bool {
        guard let window, (window.firstResponder as? NSTextInputClient)?.hasMarkedText() == true else { return true }
        switch key {
        case "command-3" where window !== settingsPanel: return false
        case "command-w", "command-comma", "command-k", "command-1", "command-2", "command-3":
            return window.makeFirstResponder(nil)
        default: return false
        }
    }

    func handleTextEditShortcut(_ key: String, in window: NSWindow?) -> Bool {
        guard let editor = window?.firstResponder as? NSTextView else { return false }
        let field = editor.delegate as? NSTextField
        guard headless || (menuOpen && field != nil && field === renameField && field?.enclosingMenuItem?.menu === cardMenu) else { return false }
        let key = key.lowercased()
        guard !editor.hasMarkedText(), editor.isEditable || (editor.isSelectable && ["command-a", "command-c"].contains(key)) else { return false }
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

    @objc func preferredScrollerStyleDidChange(_ notification: Notification) {
        DispatchQueue.main.async { [weak self] in
            self?.rebuildSettingsPanel(preservingEditor: true, scrollerStyle: NSScroller.preferredScrollerStyle)
            self?.rebuild(preservingEditor: true)
        }
    }

    func start() {
        ensureEditMainMenu()
        if !headless {
            NotificationCenter.default.addObserver(self, selector: #selector(preferredScrollerStyleDidChange(_:)),
                name: NSScroller.preferredScrollerStyleDidChangeNotification, object: nil)
        }
        signal(SIGPIPE, SIG_IGN)
        cardMenu.autoenablesItems = false
        cardMenu.delegate = self
        settingsKeyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self = self else { return event }
            let window = event.window ?? NSApp.keyWindow
            if window?.sheetParent === self.settingsPanel && window?.sheetParent != nil { return event }
            let onPanelKey: ((String) -> Void)?
            if self.directoryVisible && window === self.directoryPanel { onPanelKey = self.handleDirectoryKey }
            else if self.settingsVisible && window === self.settingsPanel { onPanelKey = self.handleSettingsKey }
            else { onPanelKey = nil }
            if let key = panelShortcutKey(event, includingNative: self.headless || onPanelKey == nil) {
                if let onPanelKey { onPanelKey(key); return nil }
                if self.handleTextEditShortcut(key, in: window) { return nil }
                if key == "command-comma" { self.openSettingsShortcut(); return nil }
                if key == "command-k" { self.openDirectoryShortcut(); return nil }
                return event
            }
            // AppKit can match punctuation key equivalents with an extra Shift.
            // Do not let a rejected window shortcut fall through to the application menu.
            if event.modifierFlags.contains(.command), let character = panelShortcutCharacter(event),
               [",", "k"].contains(character) || (self.headless && character == "w") { return nil }
            // Text Services owns navigation and commit keys while the field editor is active.
            guard let onPanelKey, !(window?.firstResponder is NSTextView),
                  let key = [53:"escape",126:"up",125:"down",36:"return",76:"return",51:"delete",117:"delete"][event.keyCode] else { return event }
            if key == "delete" && event.isARepeat { return nil }
            if key == "return" && window === self.settingsPanel && event.isARepeat { return nil }
            onPanelKey(key == "return" && event.modifierFlags.contains(.shift) ? "shift-return" : key)
            return nil
        }
        MenuControl.tracksPhysicalPointer = !headless
        feedback.didExpire = { [weak self] actions in
            var tabs: Set<SettingsTab> = []
            if actions.contains(.appCheck) { tabs.insert(.apps) }
            if !actions.isDisjoint(with: [.hostPort, .invite]) { tabs.insert(.hubs) }
            if !actions.isDisjoint(with: [.channelPair, .channelSave]) { tabs.insert(.channels) }
            if !tabs.isEmpty { self?.rebuildSettingsPanel(preservingEditor: true, tabs: tabs) }
        }
        if headless { headlessStatusButton = NSButton(frame: .zero) }
        else {
            status = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
            status?.menu = cardMenu
        }
        updateStatusItem()
        DispatchQueue.global().async { [weak self] in
            var buffer = Data()
            var bytes = [UInt8](repeating: 0, count: 8192)
            let delivered = DispatchSemaphore(value: 0)
            var lines: [Data] = []
            // Keep one bounded batch in flight. The pipe provides backpressure,
            // and tracking-mode delivery preserves IPC order without flooding AppKit.
            func deliver() {
                guard !lines.isEmpty else { return }
                let batch = lines
                lines.removeAll(keepingCapacity: true)
                RunLoop.main.perform(inModes: [.common, .eventTracking]) { [weak self] in
                    defer { delivered.signal() }
                    for line in batch { self?.receive(line) }
                }
                CFRunLoopWakeUp(CFRunLoopGetMain())
                delivered.wait()
            }
            while true {
                let n = Darwin.read(STDIN_FILENO, &bytes, bytes.count)
                if n < 0 && errno == EINTR { continue }
                if n <= 0 {
                    RunLoop.main.perform(inModes: [.common, .eventTracking]) { [weak self] in self?.finishAfterDaemonEOF() }
                    CFRunLoopWakeUp(CFRunLoopGetMain())
                    return
                }
                var start = 0
                for end in 0..<n where bytes[end] == 10 {
                    buffer.append(contentsOf: bytes[start..<end])
                    lines.append(buffer)
                    buffer = Data()
                    start = end + 1
                    if lines.count == 16 { deliver() }
                }
                buffer.append(contentsOf: bytes[start..<n])
                deliver()
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
    func resolvedDefaultHub(requireEnabled: Bool = false, requireConnected: Bool = false) -> String? {
        if hubs.contains(where: { $0.is_default != nil }) {
            return hubs.first(where: {
                $0.is_default == true &&
                (!requireEnabled || $0.enabled != false) &&
                (!requireConnected || $0.status == "connected")
            })?.name
        }
        return settings?.hubs.first(where: {
            $0.is_default &&
            (!requireEnabled || $0.enabled) &&
            (!requireConnected || $0.status == "connected")
        }).flatMap { item in
            (!requireConnected || hubs.contains(where: { $0.name == item.name })) ? item.name : nil
        }
    }
    func refreshChannelSettingsIfNeeded() {
        let target = effectiveChannelHub
        guard settingsVisible, settingsActiveTab == "channels", !target.isEmpty,
              settingsChannels.subscribedHub != target else { return }
        settingsChannels.subscribedHub = target
        emit(["op": "open_settings", "hub": target], userInitiated: false)
    }
    func settingsHub(_ value: SettingsPayload, hub: String?) -> String {
        value.settings_hub ?? hub ?? effectiveChannelHub
    }

    private func settingsFrameHub(_ frame: Frame, pending: PendingControl?) -> String? {
        frame.hub ?? pending.map(\.hub) ?? (frame.op == "state" ? frame.settings?.hubs.first(where: { $0.is_default })?.name ?? frame.hubs?.first(where: { $0.is_default == true })?.name ?? frame.hubs?.first?.name : nil)
    }

    private func channelSaveProjection(_ frame: Frame, for pending: PendingControl) -> PendingControl? {
        guard frame.op == "settings" || (frame.op == "state" && frame.hubs != nil && frame.listeners != nil),
              frame.ok != false, let value = frame.settings,
              settingsHub(value, hub: settingsFrameHub(frame, pending: pending)) == pending.hub,
              channelEditorKey(hub: pending.hub, principal: channelPrincipal(for: pending.hub, in: value), channel: pending.target) == pending.channelEditor,
              value.channels?.contains(where: { $0.id == pending.target }) == true else { return nil }
        return pending
    }

    private func settingsProjectionChanged(_ value: SettingsPayload, hub: String?) -> Bool {
        let target = settingsHub(value, hub: hub)
        return value != settings || settingsChannels.errors[target] != value.channel_inventory_error ||
            (value.channels.map { settingsChannels.inventory[target] != $0 } ?? false)
    }

    private func updateActivity(_ frame: Frame) -> Bool {
        guard let items = frame.activity else { return false }
        let total = frame.activity_total
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
                  (frame.op == "settings" && frame.source_op == nil && pending.op == "set_host_hub" && request == hostPort.submission?.requestId) else { return false }
        }
        pendingControls.removeValue(forKey: request)
        if pending.appCheck, pending.op == "setup_apply", ["state", "settings"].contains(frame.op),
           frame.ok == true, frame.authority_changed != true {
            feedback.set(.init(.appCheck, pending.target), until: Date().addingTimeInterval(1.5))
        }
        guard isChannel, let key = pending.channelEditor else { return true }
        let hub = pending.hub
        if pending.op == "channel_pair", settingsChannels.editors[key]?.pairing == .requested(request) {
            settingsChannels.editors[key]?.pairing = nil
        }
        if (frame.ok == false || frame.op == "error"), let save = pending.channelSave,
           let mounted = mountedChannelsSettings, mounted.hub == hub {
            mounted.refreshSaveControl(channel: pending.target, scope: save.scope)
        }
        guard frame.authority_changed != true, frame.op != "error", frame.ok == true || frame.committed == true else { return true }
        let fields = settingsChannels.inventory[hub]?.first { $0.id == pending.target }?.fields ?? []
        if let reset = pending.channelReset, var ui = settingsChannels.editors[key] {
            let sent = reset.submitted
            let samePair = ui.pairing?.id == sent.pairing?.id && (sent.pairing != nil ||
                (ui.pairingInstruction, ui.pairingCommand, ui.pairingURL) == (sent.pairingInstruction, sent.pairingCommand, sent.pairingURL))
            let unchanged = ui == sent
            if samePair {
                if let id = ui.pairing?.id { pendingControls.removeValue(forKey: id) }
                ui.pairing = nil
                ui.clearPairingInstructions()
                feedback.expire(.init(.channelPair, key))
            }
            for field in fields where (reset.hub && field.scope == "hub") || (reset.user && field.scope == "user") {
                let draftKey = channelDraftKey(scope: field.scope, key: field.key)
                if ui.drafts[draftKey] == sent.drafts[draftKey] { ui.drafts.removeValue(forKey: draftKey) }
            }
            if unchanged {
                if reset.hub { ui.resetGateway() }
                if reset.user {
                    ui.resetPersonal()
                    feedback.expire(channelSaveFeedbackKey(key, scope: "user"))
                }
            }
            settingsChannels.editors[key] = ui
        }
        if let save = pending.channelSave, channelSaveProjection(frame, for: pending) == nil {
            // Settings never echo submitted secrets. Keep non-secret drafts
            // until a fresh projection reconciles their values.
            for field in fields where field.type == "secret" && field.scope == save.scope {
                let draftKey = channelDraftKey(scope: field.scope, key: field.key)
                if let sent = save.values[field.key], settingsChannels.editors[key]?.drafts[draftKey] == sent {
                    settingsChannels.editors[key]?.drafts.removeValue(forKey: draftKey)
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
        let pending = pendingControls[request]
        var settled = settleControlReceipt(frame, receiptOnly: true)
        let settledControl = settled
        var routedToForm = false
        let operation = frame.source_op ?? frame.action ?? (settled ? pending?.op : nil) ?? ""
        let failed = frame.ok == false || frame.op == "error"
        let message = frame.warning ?? frame.error
        if let pending = hubForm.pendingSave, request == pending.requestId,
           operation.isEmpty || ["setup_save_hub", "paste_hub_invite"].contains(operation) {
            if failed { hubForm.failSave(message ?? "Hub save failed."); routedToForm = true }
            else if frame.authority_changed != true && (frame.committed == true || frame.ok == true) {
                finishHubSave(pending, savedName: frame.hub)
            } else { hubForm.pendingSave = nil }
            settled = true
        }
        if hubInvite.requestId != nil && request == hubInvite.requestId && (operation.isEmpty || ["setup_save_hub", "paste_hub_invite"].contains(operation)) {
            hubInvite.requestId = nil; settled = true
        }
        if request == hostPort.submission?.requestId && (operation.isEmpty || operation == "set_host_hub") {
            hostPort.submission = nil; settled = true
            if failed {
                hostPort.error = message ?? "Port update failed."
                routedToForm = true
            }
        }
        if settled && !routedToForm && operation != "channel_test" {
            if settledControl, let pending {
                if let message, !message.isEmpty { updateActionBanner(message, owner: pending.bannerOwner, advisory: !failed) }
                else if !failed && (frame.ok == true || frame.committed == true) { updateActionBanner(nil, owner: pending.bannerOwner) }
            } else if let message, !message.isEmpty {
                settingsBannerError = message
            }
        }
        return settled
    }

    private func testPanelFocus(_ data: Data) -> Bool {
        guard headless, let command = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              command["op"] as? String == "test_present_panel_focus" else { return false }
        guard settingsVisible, let panel = settingsPanel,
              let target = settingsFieldControls["settings_hub_url"],
              let alternate = settingsFieldControls["settings_hub_name"] else {
            diagnostic("present-panel-focus missing settings form")
            return true
        }
        panel.recalculateKeyViewLoop()
        panel.makeFirstResponder(nil)
        _ = applicationShouldHandleReopen(NSApp, hasVisibleWindows: false)
        let initialFocus = target.currentEditor() != nil && panel.firstResponder === target.currentEditor()

        // Change the responder in the same turn, before the deferred presentation pass.
        let preserveEditor = command["preserve_editor"] as? Bool == true
        panel.makeFirstResponder(preserveEditor ? alternate : nil)
        let editor = alternate.currentEditor() as? NSTextView
        let selection = NSRange(location: 0, length: 1)
        editor?.setSelectedRange(selection)
        let prepared = preserveEditor ? editor != nil && panel.firstResponder === editor : panel.firstResponder === panel
        DispatchQueue.main.async {
            let deferredFocus = preserveEditor
                ? editor != nil && panel.firstResponder === editor && alternate.currentEditor() === editor && editor?.selectedRange() == selection
                : target.currentEditor() != nil && panel.firstResponder === target.currentEditor()
            diagnostic("present-panel-focus \(preserveEditor) \(initialFocus) \(prepared) \(deferredFocus)")
        }
        return true
    }

    func receive(_ data: Data) {
        if handleHeadlessTestCommand(data) || testPanelFocus(data) { return }
        guard let frame = try? JSONDecoder().decode(Frame.self, from: data) else { diagnostic("invalid frame"); return }
        if frame.op == "reopen_ui" {
            _ = applicationShouldHandleReopen(NSApp, hasVisibleWindows: false)
            return
        }
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
            if hubForm.hasOIDCDraft { hubForm.oidcManualExpanded = true }
            rebuildSettingsPanel(revealAddHub: true, syncFields: false); return
        }
        let previousSettings = settingsPanel == nil ? nil : settingsPresentation()
        var revealHubError = false
        var revealPairingChannel: String?
        defer {
            refreshChannelSettingsIfNeeded()
            if let previousSettings {
                let current = settingsPresentation()
                let tabs = current.changedTabs(from: previousSettings)
                if !tabs.isEmpty || current.banner != previousSettings.banner || current.launchAtLogin != previousSettings.launchAtLogin {
                    let closedForm = previousSettings.hubForm.isOpen && !current.hubForm.isOpen && settingsActiveTab == "hubs"
                    // Legacy form errors without a submission still reveal the full layout.
                    let editor = settingsPanel?.firstResponder as? NSTextView
                    let field = editor?.delegate as? NSTextField
                    let revealLegacyHubError = revealHubError && previousSettings.hubForm.pendingSave == nil &&
                        settingsActiveTab == "hubs" && field?.identifier?.rawValue.hasPrefix("settings_hub_") == true &&
                        editor?.hasMarkedText() != true
                    if previousSettings.hostPort.pending && !current.hostPort.pending && (current.host?.error != nil || current.hostPort.error != nil) {
                        refreshMountedHostPortFeedback()
                    }
                    rebuildSettingsPanel(preservingEditor: !closedForm && !revealLegacyHubError,
                        revealAddHub: revealHubError, revealChannel: revealPairingChannel, revealPairing: revealPairingChannel != nil,
                        syncFields: false, tabs: tabs)
                }
            }
        }
        // Receipt-only settlements never project settings or results from an
        // obsolete authority. They still own the matching request's pending UI.
        if (frame.op == "settings" && frame.settings == nil) ||
           (frame.authority_changed == true && ["settings", "pair_code", "channel_test_result"].contains(frame.op)) {
            let hubSaveRequest = hubForm.pendingSave?.requestId
            let settled = settleReceiptOnly(frame)
            if settled {
                rebuild(preservingEditor: true)
                refreshDirectoryViews()
                revealHubError = frame.ok == false && hubSaveRequest != nil && hubForm.pendingSave == nil
            }
            return
        }
        if frame.op == "settings" && frame.request_id == nil {
            let unread = frame.unread_count ?? unreadCount
            let online = frame.online_count ?? directoryOnlineCount
            let hasAny = frame.activity_has_any ?? activityHasAny
            if (unread, online, hasAny) != (unreadCount, directoryOnlineCount, activityHasAny) {
                (unreadCount, directoryOnlineCount, activityHasAny) = (unread, online, hasAny)
                rebuild(preservingEditor: true)
                refreshDirectoryViews()
            }
        }
        let pendingReceipt = frame.request_id.flatMap { pendingControls[$0] }
        let receiptHub = frame.hub ?? pendingReceipt?.hub
        let projectedSave = pendingReceipt.flatMap { $0.channelSave == nil ? nil : channelSaveProjection(frame, for: $0) }
        let settledControl = settleControlReceipt(frame)
        let previousMenuBanner = menuBannerError
        let previousSettingsBanner = settingsBannerError
        let receiptOwner = settledControl ? pendingReceipt?.bannerOwner : nil
        if let receiptOwner, receiptOwner.op != "channel_test", frame.ok != false, frame.op != "error",
           frame.ok == true || frame.committed == true {
            let warning = frame.warning.flatMap { $0.isEmpty ? nil : $0 }
            if receiptOwner.op != "set_host_hub" || warning == nil || warning != frame.settings?.host_hub.error {
                updateActionBanner(warning, owner: receiptOwner, advisory: warning != nil)
            }
        } else if frame.op == "state", frame.request_id == nil, frame.ok != false, let message = frame.error ?? frame.warning, !message.isEmpty {
            if settingsVisible && settingsPanel?.isMiniaturized != true { settingsBannerError = message }
            else { menuBannerError = message }
        }
        let menuBannerChanged = menuBannerError != previousMenuBanner
        let settingsBannerChanged = settingsBannerError != previousSettingsBanner
        let settingsHubHint = settingsFrameHub(frame, pending: settledControl && pendingReceipt.map({ Self.channelControlOps.contains($0.op) }) == true ? pendingReceipt : nil)
        var nextSettings = frame.settings
        if frame.op == "state", let defaultHub = frame.hub, !defaultHub.isEmpty,
           var value = nextSettings, value.hubs.contains(where: { $0.name == defaultHub }) {
            for index in value.hubs.indices {
                value.hubs[index].is_default = (value.hubs[index].name == defaultHub)
            }
            nextSettings = value
        }
        let actionFailure = (frame.op == "error" || frame.ok == false) && directoryActionOps.contains(frame.source_op ?? "")
        if actionFailure, let requestId = frame.request_id, !requestId.isEmpty,
           requestId != latestDirectoryActionRequestId {
            if requestId == pendingMessageCopy?.requestId { pendingMessageCopy = nil }
            return
        }
        if frame.op == "activity_message" {
            guard let pending = pendingMessageCopy, frame.request_id == pending.requestId else { return }
            guard let message = frame.message, frame.error == nil, frame.ok != false else {
                showMessageCopyError(frame.error ?? "Message retrieval failed.")
                return
            }
            if frame.request_id == latestDirectoryActionRequestId { clearDirectoryActionError() }
            let index = directoryMode == "activity" ? activityFilteredItems.firstIndex { $0.hub == pending.hub && $0.signal_id == pending.signalId } : nil
            let cell = index.flatMap { directoryTableView?.view(atColumn:0,row:$0,makeIfNecessary:false) as? DirectoryPeerCellView }
            let sameItem = cell?.activityItem?.hub == pending.hub && cell?.activityItem?.signal_id == pending.signalId
            copyText(message, control: sameItem ? cell?.copyControl : nil,
                     key: .init(.activityCopy, "\(pending.hub)|\(pending.signalId)"))
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
                else if directoryComposerBar?.isHidden == false { composerSendButton?.copied("✓ Sent") }
            } else {
                showComposerFailure(frame.error, for: pending)
            }
            if let warning = frame.warning, !warning.isEmpty {
                showComposerDirectoryNotice(warning)
            }
            return
        }
        let activityPresenceChanged = frame.activity_has_any.map { $0 != activityHasAny } ?? false
        if frame.op == "settings", let value = nextSettings, frame.ok == false {
            acceptSettings(value, hub: settingsHubHint, sourceOp: frame.source_op)
        }
        if frame.op == "error" || (frame.ok == false && frame.op != "channel_test_result") {
            let sourceOp = frame.source_op ?? frame.action ?? (settledControl ? pendingReceipt?.op : nil) ?? ""
            let clipboardJoin = hubInvite.requestId != nil && frame.request_id == hubInvite.requestId
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
               requestId != hostPort.submission?.requestId && !settledControl { return }
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
            let isPortFailure = (sourceOp == "set_host_hub" || sourceOp.isEmpty) &&
                hostPort.submission != nil && frame.request_id == hostPort.submission?.requestId
            if isPortFailure {
                hostPort.submission = nil
                if settings?.host_hub.error != error { hostPort.error = error.isEmpty ? "Port update failed." : error }
            }
            if (sourceOp.isEmpty || ["paste_hub_invite", "setup_save_hub"].contains(sourceOp)) && (frame.request_id == nil || clipboardJoin) {
                hubInvite.requestId = nil
                feedback.expire(.init(.invite))
            }
            if isAddHubError { hubForm.failSave(error.isEmpty ? "Hub save failed." : error) }
            let composerSubmission = pendingComposer.flatMap { submission in
                (sourceOp.isEmpty || sourceOp == "send_signal") && frame.request_id == submission.requestId ? submission : nil
            }
            if let composerSubmission {
                pendingComposer = nil
                if !error.isEmpty { showComposerFailure(error, for: composerSubmission) }
            } else if error.isEmpty || isAddHubError || isPortFailure { }
            else if ["query_directory", "open_directory", "directory"].contains(sourceOp) || (frame.op == "directory" && sourceOp.isEmpty) {
                if frame.request_id == nil || frame.request_id == directoryRequestId { directoryQueryError = error; updateDirectoryWarnings() }
            }
            else if directoryActionOps.contains(sourceOp) {
                directoryActionError = error; directoryActionErrorSourceOp = sourceOp; updateDirectoryWarnings()
            }
            else if sourceOp == "channel_test" || (sourceOp == "set_host_hub" && settings?.host_hub.error == error) { }
            else if let receiptOwner { updateActionBanner(error, owner: receiptOwner) }
            else if (settingsVisible && settingsPanel?.isMiniaturized != true) || hubForm.pendingSave != nil || clipboardJoin || sourceOp == "paste_hub_invite" { settingsBannerError = error }
            else { menuBannerError = error }
            diagnostic("error \(error)")
            rebuild(preservingEditor: true)
            revealHubError = isAddHubError
            return
        }
        // State receipts still pass through normal failure and Directory action
        // correlation above; only successful commits may omit their projection.
        if frame.op == "state", frame.ok == true, frame.committed == true,
           frame.request_id != nil, frame.hubs == nil || frame.listeners == nil {
            settleReceiptOnly(frame)
            rebuild(preservingEditor: true)
            refreshDirectoryViews()
            return
        }
        if ["state", "directory"].contains(frame.op) { settleDirectoryActionSuccess(frame) }
        if frame.op == "settings", let value = nextSettings {
            let hubSaveSucceeded = frame.ok == true && ["setup_save_hub", "paste_hub_invite"].contains(frame.source_op ?? "")
            if hubInvite.requestId != nil && frame.request_id == hubInvite.requestId && hubSaveSucceeded {
                hubInvite.requestId = nil
                feedback.set(.init(.invite), until: Date().addingTimeInterval(1.2))
            }
            acceptSettings(value, hub: settingsHubHint, channelSave: settledControl ? projectedSave : nil, savedRequest: frame.request_id, sourceOp: frame.source_op, confirmHubSave: hubSaveSucceeded, savedHub: frame.hub)
            if receiptOwner == nil, let warning = frame.warning, !warning.isEmpty, frame.source_op != "channel_test",
               !(frame.source_op == "set_host_hub" && value.host_hub.error == warning) { settingsBannerError = warning }
            if menuBannerChanged { rebuild(preservingEditor: true) }
            return
        }
        if frame.op == "pair_code", frame.authority_changed != true, let id = frame.channel_id {
            let hub = receiptHub ?? effectiveChannelHub
            let key = channelEditorKey(hub: hub, channel: id)
            guard let request = frame.request_id, let pending = pendingReceipt,
                  pending.op == "channel_pair", pending.hub == hub, pending.target == id,
                  pending.channelEditor == key, var ui = settingsChannels.editors[key], ui.pairing == .requested(request) else { return }
            ui.pairing = .displayed(request)
            ui.pairingInstruction = frame.instruction ?? frame.code ?? ""
            ui.pairingCommand = frame.pairing_command ?? frame.instruction
            ui.pairingURL = frame.pairing_url
            if let command = frame.pairing_command { copyText(command, key: .init(.channelPair, key)) }
            settingsChannels.editors[key] = ui
            revealPairingChannel = hub == effectiveChannelHub ? id : nil
            return
        }
        if frame.op == "channel_test_result", frame.authority_changed != true, let id = frame.channel_id {
            guard frame.request_id == nil || settledControl else { return }
            let key = channelEditorKey(hub: receiptHub ?? effectiveChannelHub, channel: id)
            guard frame.request_id == nil || pendingReceipt?.channelEditor == key else { return }
            settingsChannels.editors[key, default: ChannelUiState()].testResult = (ok: frame.ok == true, detail: frame.detail ?? frame.error ?? (frame.ok == true ? "Test sent" : "Test failed"))
            settingsChannels.editors[key, default: ChannelUiState()].testResultIsLatest = true
            return
        }
        if frame.op == "state", var h = frame.hubs, let l = frame.listeners {
            if let defaultHub = frame.hub, !defaultHub.isEmpty, h.contains(where: { $0.name == defaultHub }) {
                for index in h.indices {
                    h[index].is_default = (h[index].name == defaultHub)
                }
            }
            if let hasAny = frame.activity_has_any { activityHasAny = hasAny }
            let a = frame.available ?? []
            let nextDiscovery = frame.discovery ?? discovery
            let discoveryChanged = discovery != nextDiscovery
            discovery = nextDiscovery
            let r = (frame.my_remote_listeners ?? []).filter { $0.harness != "human" }
            let onlineCount = frame.directory_online_count ?? max(directoryOnlineCount, r.count)
            let directoryRemoteChanged = r != myRemote || onlineCount != directoryOnlineCount
            let nextUnread = frame.unread_count ?? unreadCount
            let activityProjectionChanged = updateActivity(frame)
            let activityChanged = activityPresenceChanged || nextUnread != unreadCount || activityProjectionChanged
            unreadCount = nextUnread
            let directoryMembershipChanged = h != hubs || l != listeners
            let nextHarnesses = frame.harnesses ?? harnesses
            let harnessesChanged = nextHarnesses != harnesses
            if harnessesChanged { harnesses = nextHarnesses; directoryDirty = true }
            // An unchanged save acknowledgement still needs to refresh the settings feedback.
            let settingsChanged = nextSettings.map { settingsProjectionChanged($0, hub: settingsHubHint) } ?? false
            let settingsReceipt = (hostPort.pending && frame.request_id == hostPort.submission?.requestId) || (settledControl && projectedSave != nil)
            if hasRenderedState && !settledControl && !discoveryChanged && !harnessesChanged && !settingsChanged && !settingsReceipt && !menuBannerChanged && !settingsBannerChanged && !activityChanged && !directoryMembershipChanged && a == available && !directoryRemoteChanged {
                updateStatusItem()
            } else {
                hubs = h; listeners = l; available = a; myRemote = r; directoryOnlineCount = onlineCount
                if (settingsChanged || settingsReceipt), let value = nextSettings ?? settings {
                    acceptSettings(value, hub: settingsHubHint, channelSave: settledControl ? projectedSave : nil, savedRequest: frame.request_id, sourceOp: frame.source_op)
                }
                seedDirectoryPeersIfEmpty()
                hasRenderedState = true
                rebuild(preservingEditor: true)
                refreshDirectoryViews()
                if directoryVisible && (directoryMembershipChanged || directoryRemoteChanged) { queryDirectory() }
            }
            if !hasCheckedFirstLaunchOnboarding, let s = settings, !s.apps.isEmpty,
               !s.apps.contains(where: { $0.status == "error" && $0.detail == "Setup inspection pending" }) {
                hasCheckedFirstLaunchOnboarding = true
                if !headless && !s.apps.contains(where: { $0.isConfigured || $0.status == "error" }) {
                    let rawStateDir = ProcessInfo.processInfo.environment["CITO_STATE_DIR"] ?? (NSHomeDirectory() + "/.cito")
                    let expandedStateDir = rawStateDir.hasPrefix("~/")
                        ? (NSHomeDirectory() + "/" + String(rawStateDir.dropFirst(2)))
                        : (rawStateDir == "~" ? NSHomeDirectory() : rawStateDir)
                    let markerPath = expandedStateDir + "/onboarding-shown"
                    if !FileManager.default.fileExists(atPath: markerPath) {
                        pendingOnboardingMarker = markerPath
                        openSettingsPanel(tab: "apps")
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
        }
    }

    func emit(_ action: [String: Any], userInitiated: Bool = true, appCheck: Bool = false) {
        guard quitState == .running else { return }
        let operation = action["op"] as? String ?? ""
        if operation == "quit" { NSApp.terminate(nil); return }
        let surface = actionSurface(for: operation)
        if userInitiated && surface == .menu && menuBannerOwner == nil && menuBannerError != nil &&
            !["query_directory", "refresh_available", "close_settings", "close_directory"].contains(operation) {
            menuBannerError = nil; rebuild(preservingEditor: true)
        }
        var outgoing = action
        let tracked = Self.channelControlOps + Self.hubOrderOps + ["set_host_hub", "human_inbox_update", "unlisten_all", "setup_apply", "setup_remove_app", "setup_remove_hub", "set_scope", "set_auto_listen", "unlisten_session", "set_listen_mode", "rename_session", "hub_action", "setup_toggle_hub", "set_launch_at_login", "setup_disconnect_all", "listen_session"]
        if tracked.contains(operation) || directoryActionOps.contains(operation) {
            outgoing["request_id"] = (action["request_id"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? UUID().uuidString
        }
        guard let data = try? JSONSerialization.data(withJSONObject: outgoing, options: [.sortedKeys]), data.count <= 1_048_576 else {
            let message = "This action is too large to send. Shorten the input and try again."
            if operation == "setup_save_hub" { hubForm.failSave(message) }
            else if operation == "set_host_hub" { hostPort.submission = nil; hostPort.error = message }
            else if operation == "paste_hub_invite" { hubInvite.requestId = nil; hubForm.parseRequest = ""; settingsBannerError = message }
            else if operation == "send_signal" { pendingComposer = nil; setComposerStatus(message) }
            else if surface == .settings { settingsBannerError = message }
            else { menuBannerError = message }
            rebuild(preservingEditor: true); if settingsPanel != nil { rebuildSettingsPanel(preservingEditor: true) }
            return
        }
        if tracked.contains(operation), let request = outgoing["request_id"] as? String {
            let channel = outgoing["channel_id"] as? String
            let hub = outgoing["hub"] as? String ?? (channel == nil ? "" : effectiveChannelHub)
            let session = (outgoing["session_id"] as? String).map { "\(outgoing["harness"] as? String ?? ""):\($0)" }
            let hostTarget = action["auth_mode"] != nil ? "access" : (request == hostPort.submission?.requestId ? "port" : "sharing")
            let inboxTarget = action["paused"] != nil ? "paused" : "scope"
            let controlTarget = operation == "set_host_hub" ? hostTarget : (operation == "human_inbox_update" ? inboxTarget : "")
            let target = channel ?? session ?? outgoing["harness"] as? String ?? controlTarget
            let bannerTarget = outgoing["listener_id"] as? String ?? outgoing["action"] as? String ?? target
            let owner = BannerOwner(surface: surface, op: operation, hub: hub, target: bannerTarget)
            let editor = channel.map { channelEditorKey(hub: hub, channel: $0) }
            var pending = PendingControl(op: operation, hub: hub, target: target, appCheck: appCheck, bannerOwner: owner, channelEditor: editor)
            if let key = editor {
                if operation == "channel_pair" { settingsChannels.editors[key, default: ChannelUiState()].pairing = .requested(request) }
                if operation == "channel_test" { settingsChannels.editors[key, default: ChannelUiState()].testResultIsLatest = false }
                if Self.channelSaveOps.contains(operation), let values = outgoing["values"] as? [String: Any] {
                    let scope = outgoing["scope"] as? String ?? (operation == "save_hub_service" ? "hub" : "user")
                    for old in Array(pendingControls.keys) where pendingControls[old].map({ $0.channelEditor == key && $0.channelSave?.scope == scope }) == true {
                        pendingControls[old]?.channelSave = nil
                    }
                    let submitted = values.compactMapValues { value in
                        (value as? String) ?? (value as? Bool).map(String.init)
                    }
                    pending.channelSave = .init(scope: scope, values: submitted)
                }
                if Self.channelResetOps.contains(operation) {
                    let resetHub = outgoing["reset_hub"] as? Bool == true
                    let resetUser = outgoing["reset_user"] as? Bool == true
                    var submitted = settingsChannels.editors[key] ?? ChannelUiState()
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
            let key = channelEditorKey(hub: outgoing["hub"] as? String ?? effectiveChannelHub, channel: id)
            var removedTestResult = false
            if operation != "channel_test" {
                removedTestResult = settingsChannels.editors[key]?.testResult != nil
                settingsChannels.editors[key, default: ChannelUiState()].testResult = nil
            }
            let saveControl = operation == "channel_save" ? "save_channel" : "save_hub_service"
            if !removedTestResult && Self.channelSaveOps.contains(operation),
               outgoing["inbound_mode"] == nil, outgoing["display_label"] == nil,
               let root = settingsPanel?.contentView,
               let save: MenuControl = findView(in:root,id:"\(saveControl):\(id)") {
                // Pending feedback changes no fields; retain the editor's undo stack.
                save.setPending("Saving…")
            } else if settingsPanel != nil, hubMatchesMountedChannels(outgoing) { rebuildSettingsPanel(tabs: [.channels]) }
        }
        if (Self.hubOrderOps + ["set_host_hub", "hub_action", "setup_toggle_hub", "setup_remove_hub", "human_inbox_update", "unlisten_all", "setup_apply", "setup_remove_app", "listen_session"]).contains(operation) {
            rebuild(preservingEditor: true)
            if settingsPanel != nil, let tab = SettingsTab.affected(by: operation),
               tab != .channels || hubMatchesMountedChannels(outgoing) {
                rebuildSettingsPanel(preservingEditor: true, tabs: [tab])
            }
        }
        if operation == "open_settings", let hub = outgoing["hub"] as? String {
            settingsChannels.subscribedHub = hub
        }
        FileHandle.standardOutput.write(data + Data([10]))
    }
    func controlTextDidEndEditing(_ obj: Notification) {
        // Wait until AppKit releases the shared field editor before replacing views.
        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }
            self.flushDeferredSettings()
            if self.menuRebuildDeferred { self.rebuild(preservingEditor: true) }
        }
    }

    func controlTextDidChange(_ obj: Notification) {
        if let field = obj.object as? NSSearchField, field === directorySearchField {
            updateDirectoryQuery(field.stringValue)
            return
        }
        if let field = obj.object as? NSTextField, field === composerInputField { composerDraftIdempotencyKey = UUID().uuidString }
        if let field = obj.object as? NSTextField, field.identifier?.rawValue == "host_hub_port" {
            hostPort.draft = field.stringValue
            feedback.expire(.init(.hostPort))
            hostPort.error = nil
            refreshMountedHostPortFeedback()
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
        if commandSelector == #selector(NSResponder.insertNewline(_:)) {
            if control === directorySearchField { return true }
            handleDirectoryKey(NSApp.currentEvent?.modifierFlags.contains(.shift) == true ? "shift-return" : "return")
            return true
        }
        if commandSelector == #selector(NSResponder.cancelOperation(_:)) { handleDirectoryKey("escape"); return true }
        return false
    }
}
