import AppKit

struct HarnessMeta: Decodable, Equatable {
    let id: String
    let display_name: String
    let sf_symbol: String
    let tint: String
    let tier: String
    let order: Int
    let kind: String?
    let pid_bound: Bool?
    let idle_timeout_ms: UInt64?
}
struct HarnessVisual {
    let name: String
    let symbol: String
    let tint: NSColor
    let tier: String
    let order: Int
}
func resolveHarnessVisual(_ harness: String, harnesses: [String: HarnessMeta]) -> HarnessVisual {
    if let meta = harnesses[harness] {
        return HarnessVisual(
            name: meta.display_name,
            symbol: meta.sf_symbol,
            tint: CitoPalette.badgeFill(meta.tint),
            tier: meta.tier,
            order: meta.order
        )
    }
    if harness == "human" {
        return HarnessVisual(
            name: "Human",
            symbol: "person.crop.circle.fill",
            tint: CitoPalette.badgeFill("human"),
            tier: "stable",
            order: -1
        )
    }
    return HarnessVisual(
        name: harness.capitalized,
        symbol: "bolt.horizontal.fill",
        tint: CitoPalette.badgeFill("blue"),
        tier: "custom",
        order: 3
    )
}

struct ChannelAttachment: Decodable, Equatable {
    let location: String, display_name: String, availability: String
    let is_local: Bool
    var channel_id: String? = nil
    var host_id: String? = nil
}

struct DiscoveryState: Decodable, Equatable {
    let harness: String, status: String
    let error: String?
}
struct Hub: Decodable, Equatable {
    let name: String, status: String, auth_mode: String
    let user: String?, error: String?
    let enabled: Bool?, auto_listen: Bool?
    let human_paused: Bool?
    var human_listener_id: String? = nil
    var human_can_set_scope: Bool? = nil
    var human_control_revision: Int? = nil
    var human_availability: String? = nil
    var human_scope: String? = nil
    var human_state_status: String? = nil
    var attachments: [ChannelAttachment]? = nil
    var is_default: Bool? = nil
    var is_builtin: Bool? = nil
    var supports_workspace_scope: Bool? = nil
    var can_login: Bool? = nil
}
struct Listener: Decodable, Equatable {
    let hub: String, listener_id: String, public_id: String, scoped_id: String
    let harness: String, session_id: String, title: String, cwd: String, scope: String
    let about: String?, repo: String?, branch: String?
    let custom_title: Bool?
    let listen_mode: String?
    let display_address: String
    let user: String?, owner_user: String?
    let host_name: String?
    let channels: [String]?
    let attachments: [ChannelAttachment]?
    var is_self_principal: Bool? = nil
    var can_set_scope: Bool? = nil
    var can_pin: Bool? = nil
    var can_rename: Bool? = nil
    var can_stop: Bool? = nil
    var display_title: String? = nil
    var use_app_title: Bool? = nil
    var display_about: String? = nil
}
struct Candidate: Decodable, Equatable {
    let harness: String, session_id: String, title: String, cwd: String
    let branch: String?, endpoint_pid: Int?
    let listening_hubs: [String]?
    let custom_title: Bool?
    let needs_reload: Bool?
    let reload_hint: String?
    var generic_title: Bool? = nil
    var title_fallback_hubs: [String]? = nil
}
struct RemotePeer: Decodable, Equatable {
    let hub: String?
    let listener_id: String, public_id: String, scoped_id: String
    let user: String, host_name: String, harness: String
    let display_address: String
    let title: String, about: String
    var custom_title: Bool? = nil
    let cwd: String?, repo: String?, branch: String?
    let is_self_principal: Bool?
    let channels: [String]?
    var attachments: [ChannelAttachment]? = nil
    var display_title: String? = nil
    var use_app_title: Bool? = nil
    var display_about: String? = nil
}
struct ActivityItem: Decodable, Equatable {
    let hub: String
    let signal_id: String
    let listener_id: String
    let from_user: String
    let from_host_name: String
    let from_harness: String?
    let from_title: String
    let from_scoped_id: String?
    let from_display_address: String?
    let target_display_address: String?
    let message_truncated: Bool?
    let target_harness: String
    let target_title: String
    let message: String
    let held_reason: String?
    let created_at: String
    var unread: Bool
    var from_listener_id: String? = nil
    var can_reply: Bool? = nil
    var display_from_title: String? = nil
}
struct ChannelFieldItem: Decodable, Equatable {
    let key: String
    let scope: String
    let label: String
    let type: String
    let required: Bool
    let placeholder: String
    let value: String?
    let configured: Bool
    let hidden: Bool?
    let configured_placeholder: String?
    let help_text: String?
}
struct ReusableChannelBinding: Codable, Equatable {
    let source_hub: String
    let display_label: String
    let values: [String: String]
}
struct ChannelSetupItem: Decodable, Equatable {
    let id: String
    let display_name: String
    let sf_symbol: String
    let tint: String
    let enabled: Bool
    let configured: Bool
    let status: String
    let detail: String
    let display_label: String?
    let hub_provided: Bool
    let pairing_supported: Bool
    let pairing_action_label: String?
    let pairing_code: String?
    var pairing_command: String?
    let pairing_instruction: String?
    var pairing_url: String? = nil
    var pairing_open_url_label: String? = nil
    var pairing_manual_disclosure: Bool? = nil
    var warning: Bool? = nil
    var inbox_paused: Bool? = nil
    let fields: [ChannelFieldItem]
    let setup_guide: [String]?
    let pairing_help_text: String?
    var has_hub_service: Bool? = nil
    var hub_ready: Bool? = nil
    var can_configure_hub: Bool? = nil
    var can_clear_hub: Bool? = nil
    var can_edit_personal: Bool? = nil
    var can_pair: Bool? = nil
    var can_test: Bool? = nil
    var can_unpair: Bool? = nil
    var error_origin: String? = nil
    var supports_outbound_only: Bool? = nil
    var inbound_mode: String? = nil
    var service_label: String? = nil
    var service_identity: String? = nil
    var reusable_binding: ReusableChannelBinding? = nil

    func canSendTest(drafts: [String: String], visibleFields: [ChannelFieldItem]) -> Bool {
        guard can_test == true else { return false }
        let required = visibleFields.filter { $0.required }
        let blanked = required.contains { field in
            drafts[field.key].map { $0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty } == true
        }
        return (configured && !blanked) || required.allSatisfy {
            !(drafts[$0.key] ?? $0.value ?? "").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
    }
}
struct AppSetupItem: Decodable, Equatable {
    let id: String
    let display_name: String
    let tier: String
    let configured: Bool?
    var isConfigured: Bool { configured == true }
    let status: String
    let detail: String
    let paths: [String]
}
struct HostHubSummary: Decodable, Equatable {
    let enabled: Bool
    let running: Bool
    let port: Int
    let endpoint_url: String
    var tailscale_url: String? = nil
    var tailscale_invite_command: String? = nil
    var lan_url: String? = nil
    var lan_invite_command: String? = nil
    let token: String
    let invite_command: String
    let auth_mode: String?
    let error: String?
}
struct HubFormProfile: Equatable {
    var name: String
    let url: String, issuer: String, clientId: String, redirectPort: String
}
struct HubConfigItem: Decodable, Equatable {
    let name: String
    let url: String
    let auth_mode: String
    let enabled: Bool
    var is_default: Bool
    let status: String
    let user: String?
    let error: String?
    let issuer: String?
    let client_id: String?
    var human_paused: Bool? = nil
    var human_listener_id: String? = nil
    var human_can_set_scope: Bool? = nil
    var human_control_revision: Int? = nil
    var human_scope: String? = nil
    var human_state_status: String? = nil
    var attachments: [ChannelAttachment]? = nil
    var is_builtin: Bool? = nil
    var supports_workspace_scope: Bool? = nil
    var can_login: Bool? = nil
    var redirect_port: Int? = nil
    func sharesChannelAuthority(with other: HubConfigItem?) -> Bool {
        guard let other else { return false }
        return url == other.url && user == other.user
    }
    var formProfile: HubFormProfile {
        HubFormProfile(name: name, url: normalizedHubURL(url), issuer: issuer ?? "", clientId: client_id ?? "", redirectPort: redirect_port.map(String.init) ?? "")
    }
}
struct SettingsPayload: Decodable, Equatable {
    var apps: [AppSetupItem]
    let settings_hub: String?
    var channels: [ChannelSetupItem]?
    var channel_inventory_error: String? = nil
    var hubs: [HubConfigItem]
    let host_hub: HostHubSummary
    let launch_at_login: Bool
}
// Invite ports accept both daemon numeric values and clipboard string aliases.
enum InvitePort: Decodable {
    case number(Int), string(String)
    init(from decoder: Decoder) throws {
        let value = try decoder.singleValueContainer()
        if let port = try? value.decode(Int.self) { self = .number(port) }
        else { self = .string(try value.decode(String.self)) }
    }
    var text: String {
        switch self { case .number(let port): return String(port); case .string(let port): return port }
    }
}
struct HubInviteConfig: Decodable {
    let url: String?, name: String?, authMode: String?, auth_mode: String?
    let token: String?, user: String?, issuer: String?, oidcIssuer: String?
    let clientId: String?, client_id: String?, oidcClientId: String?
    let redirect_port: InvitePort?, redirectPort: InvitePort?, oidcRedirectPort: InvitePort?
}
struct Frame: Decodable {
    let config: HubInviteConfig?
    let op: String
    let hub: String?
    let ok: Bool?
    let error: String?
    let warnings: [String]?
    let source_op: String?
    let action: String?
    let channel: String?
    let committed: Bool?
    let warning: String?
    let authority_changed: Bool?
    let request_id: String?
    let channel_id: String?
    let code: String?
    let instruction: String?
    let pairing_command: String?
    let pairing_url: String?
    let detail: String?
    let hubs: [Hub]?
    let listeners: [Listener]?
    let available: [Candidate]?
    let my_remote_listeners: [RemotePeer]?
    let directory_online_count: Int?
    let unread_count: Int?
    let activity: [ActivityItem]?
    let harnesses: [HarnessMeta]?
    let discovery: [DiscoveryState]?
    let settings: SettingsPayload?
    let peers: [RemotePeer]?
    let matched_count: Int?
    let online_count: Int?
    let query: String?
    let scope: String?
    let harness: String?
    let activity_total: Int?
    let activity_has_any: Bool?
    let activity_unread_count: Int?
    let activity_filter: String?
    let activity_query: String?
    let message: String?
}

/// Main-run-loop authority for short-lived feedback, independent of mounted views.
final class TransientFeedbackStore {
    static let shared = TransientFeedbackStore()

    enum Action { case control, menuCopy, directoryCopy, activityCopy, appCheck, hostPort, invite, channelPair, channelSave }
    struct Key: Hashable {
        let action: Action
        let target: String
        let authority: String

        init(_ action: Action, _ target: String = "", authority: String = "") {
            self.action = action; self.target = target; self.authority = authority
        }
    }
    private struct Entry {
        let until: Date
        var observers: [ObjectIdentifier: () -> Void] = [:]
    }
    private var entries: [Key: Entry] = [:]
    private var timer: Timer?
    var didExpire: ((Set<Action>) -> Void)?

    // The deadline schedules expiry; membership changes only when expiry restores the views.
    func deadline(_ key: Key) -> Date? { entries[key]?.until }
    func contains(_ key: Key) -> Bool { deadline(key) != nil }
    func set(_ key: Key, until: Date?) {
        guard let until, until > Date() else { expire(key); return }
        entries[key] = Entry(until: until, observers: entries[key]?.observers ?? [:])
        schedule()
    }
    func observe(_ key: Key, owner: AnyObject, restore: @escaping () -> Void) {
        entries[key]?.observers[ObjectIdentifier(owner)] = restore
    }
    // The channel summary has a single presentation slot per key.
    func observe(_ key: Key, restore: @escaping () -> Void) {
        observe(key, owner: self, restore: restore)
    }
    func unobserve(_ key: Key, owner: AnyObject) {
        entries[key]?.observers.removeValue(forKey: ObjectIdentifier(owner))
    }
    func expire(_ key: Key) {
        if let entry = entries.removeValue(forKey: key) {
            for restore in entry.observers.values { restore() }
        }
        schedule()
    }
    func deadlines(for action: Action) -> [String: Date] {
        entries.reduce(into: [:]) { result, pair in
            if pair.key.action == action { result[pair.key.target] = pair.value.until }
        }
    }
    private func schedule() {
        let next = entries.values.map(\.until).min()
        if let timer, timer.isValid, timer.fireDate == next { return }
        timer?.invalidate()
        timer = nil
        guard let next else { return }
        let timer = Timer(fire: next, interval: 0, repeats: false) { [weak self] _ in self?.expireDue() }
        self.timer = timer
        RunLoop.main.add(timer, forMode: .common)
        RunLoop.main.add(timer, forMode: .eventTracking)
    }
    private func expireDue() {
        let now = Date()
        let expired = entries.filter { $0.value.until <= now }
        for key in expired.keys { entries.removeValue(forKey: key) }
        for entry in expired.values {
            for restore in entry.observers.values { restore() }
        }
        schedule()
        if !expired.isEmpty { didExpire?(Set(expired.keys.map(\.action))) }
    }
}

struct ChannelUiState: Equatable {
    var editingGateway = false
    var editingPersonal = false
    var manualEntry = false
    var drafts: [String: String] = [:]
    enum Pairing: Equatable {
        case requested(String), displayed(String)
        var id: String { switch self { case .requested(let id), .displayed(let id): return id } }
    }
    var pairing: Pairing?
    var waitingForPairCode: Bool { if case .requested = pairing { return true }; return false }
    var pairingInstruction = ""
    var pairingCommand: String?
    var pairingURL: String?
    var testResult: (ok: Bool, detail: String)?
    var testResultIsLatest = false

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.editingGateway == rhs.editingGateway && lhs.editingPersonal == rhs.editingPersonal && lhs.manualEntry == rhs.manualEntry &&
        lhs.drafts == rhs.drafts && lhs.pairing == rhs.pairing && lhs.pairingInstruction == rhs.pairingInstruction &&
        lhs.pairingCommand == rhs.pairingCommand && lhs.pairingURL == rhs.pairingURL &&
        lhs.testResult?.ok == rhs.testResult?.ok && lhs.testResult?.detail == rhs.testResult?.detail &&
        lhs.testResultIsLatest == rhs.testResultIsLatest
    }

    mutating func resetPersonal(preservePendingPair: Bool = false) {
        editingPersonal = false
        manualEntry = false
        if !preservePendingPair { pairing = nil }
        clearPairingInstructions()
    }
    mutating func clearPairingInstructions() {
        pairingInstruction = ""
        pairingCommand = nil
        pairingURL = nil
    }
    mutating func resetGateway() {
        editingGateway = false

    }
}
