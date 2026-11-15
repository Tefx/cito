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
    let id: String
    let name: String
    let symbol: String
    let tint: NSColor
    let tier: String
    let order: Int
}
func resolveHarnessVisual(_ harness: String, harnesses: [String: HarnessMeta]) -> HarnessVisual {
    if let meta = harnesses[harness] {
        return HarnessVisual(
            id: meta.id,
            name: meta.display_name,
            symbol: meta.sf_symbol,
            tint: CitoPalette.badgeFill(meta.tint),
            tier: meta.tier,
            order: meta.order
        )
    }
    if harness == "human" {
        return HarnessVisual(
            id: "human",
            name: "Human",
            symbol: "person.crop.circle.fill",
            tint: CitoPalette.badgeFill("human"),
            tier: "stable",
            order: -1
        )
    }
    return HarnessVisual(
        id: harness,
        name: harness.capitalized,
        symbol: "bolt.horizontal.fill",
        tint: CitoPalette.badgeFill("blue"),
        tier: "custom",
        order: 3
    )
}

struct ChannelAttachment: Decodable, Equatable {
    let channel_id: String, location: String, display_name: String, availability: String
    let is_local: Bool
    let host_id: String?, plugin_id: String?
}

struct DiscoveryState: Decodable, Equatable {
    let harness: String, status: String
    let error: String?
}
struct HubCapabilities: Decodable, Equatable {
    let status: String?, auth_valid: Bool?, rpc_status: String?, sse_live: Bool?, observation_current: Bool?
}
struct Hub: Decodable, Equatable {
    let name: String, status: String, auth_mode: String
    let user: String?, error: String?
    let enabled: Bool?, auto_listen: Bool?
    let human_paused: Bool?, human_about: String?
    var human_listener_id: String? = nil
    var human_control_revision: Int? = nil
    var human_availability: String? = nil
    var human_scope: String? = nil
    var human_state_status: String? = nil
    var attachments: [ChannelAttachment]? = nil
    var capabilities: HubCapabilities? = nil
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
    let host_id: String?
    let channels: [String]?
    let attachments: [ChannelAttachment]?
    var is_self_principal: Bool? = nil
    var can_set_scope: Bool? = nil
    var can_pin: Bool? = nil
    var can_rename: Bool? = nil
    var can_stop: Bool? = nil
}
struct Candidate: Decodable, Equatable {
    let harness: String, session_id: String, title: String, cwd: String
    let branch: String?, endpoint_pid: Int?, updated_at_ms: Double?
    let listening_hubs: [String]?
    let custom_title: Bool?
    let needs_reload: Bool?
    let reload_hint: String?

    // Discovery timestamps do not affect any rendered candidate field.
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.harness == rhs.harness && lhs.session_id == rhs.session_id && lhs.title == rhs.title && lhs.cwd == rhs.cwd &&
        lhs.branch == rhs.branch && lhs.endpoint_pid == rhs.endpoint_pid && lhs.listening_hubs == rhs.listening_hubs &&
        lhs.custom_title == rhs.custom_title && lhs.needs_reload == rhs.needs_reload && lhs.reload_hint == rhs.reload_hint
    }
}
struct RemotePeer: Decodable, Equatable {
    let hub: String?
    let listener_id: String, public_id: String, scoped_id: String
    let user: String, host_id: String, host_name: String, harness: String
    let display_address: String
    let title: String, about: String, scope: String
    let last_seen_at: String?
    var session_id: String? = nil
    var custom_title: Bool? = nil
    let cwd: String?, repo: String?, branch: String?
    let is_self_principal: Bool?
    let channels: [String]?
    var attachments: [ChannelAttachment]? = nil
    var can_set_scope: Bool? = nil
    var can_pin: Bool? = nil
    var can_rename: Bool? = nil
    var can_stop: Bool? = nil

    // Hub lease refreshes leave the displayed peer unchanged.
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.hub == rhs.hub && lhs.listener_id == rhs.listener_id && lhs.public_id == rhs.public_id && lhs.scoped_id == rhs.scoped_id &&
        lhs.user == rhs.user && lhs.host_id == rhs.host_id && lhs.host_name == rhs.host_name && lhs.harness == rhs.harness &&
        lhs.display_address == rhs.display_address && lhs.title == rhs.title && lhs.about == rhs.about && lhs.scope == rhs.scope &&
        lhs.session_id == rhs.session_id && lhs.custom_title == rhs.custom_title && lhs.cwd == rhs.cwd && lhs.repo == rhs.repo &&
        lhs.branch == rhs.branch && lhs.is_self_principal == rhs.is_self_principal && lhs.channels == rhs.channels && lhs.attachments == rhs.attachments &&
        lhs.can_set_scope == rhs.can_set_scope && lhs.can_pin == rhs.can_pin && lhs.can_rename == rhs.can_rename && lhs.can_stop == rhs.can_stop
    }
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
    let target_scoped_id: String
    let message: String
    let held_reason: String?
    let in_reply_to: String?
    let created_at: String
    let unread: Bool
    let for_me: Bool
    var from_listener_id: String? = nil
    var can_reply: Bool? = nil
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
    let redacted: Bool?
    let configured_placeholder: String?
    let help_text: String?
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
    var delivery_enabled: Bool? = nil
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
    let installed: Bool
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
struct HubConfigItem: Decodable, Equatable {
    let name: String
    let url: String
    let auth_mode: String
    let enabled: Bool
    let auto_listen: Bool?
    let is_default: Bool
    let status: String
    let user: String?
    let error: String?
    let issuer: String?
    let client_id: String?
    var human_paused: Bool? = nil
    var human_about: String? = nil
    var human_state_status: String? = nil
    var capabilities: HubCapabilities? = nil
    var is_builtin: Bool? = nil
    var supports_workspace_scope: Bool? = nil
    var can_login: Bool? = nil
}
struct SettingsPayload: Decodable, Equatable {
    let apps: [AppSetupItem]
    let channel_hub: String?
    let settings_hub: String?
    let channels: [ChannelSetupItem]?
    var channel_inventory_error: String? = nil
    let hubs: [HubConfigItem]
    let host_hub: HostHubSummary
    let launch_at_login: Bool
}
struct Frame: Decodable {
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
    let composer_error: String?
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
    let activity_all_total: Int?
    let activity_has_any: Bool?
    let activity_unread_count: Int?
    let activity_filter: String?
    let activity_query: String?
    let message: String?
    let part: Int?
    let complete: Bool?
}

struct ChannelUiState: Equatable {
    var editingGateway = false
    var editingPersonal = false
    var manualEntry = false
    var drafts: [String: String] = [:]
    var pendingPairRequest: String?
    var pairingInstruction = ""
    var pairingCommand: String?
    var pairingURL: String?
    var pairCopiedUntil = Date.distantPast
    var savedUntil: [String: Date] = [:]
    var testResult: (ok: Bool, detail: String)?
    var testResultIsLatest = false

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.editingGateway == rhs.editingGateway && lhs.editingPersonal == rhs.editingPersonal && lhs.manualEntry == rhs.manualEntry &&
        lhs.drafts == rhs.drafts && lhs.pendingPairRequest == rhs.pendingPairRequest && lhs.pairingInstruction == rhs.pairingInstruction &&
        lhs.pairingCommand == rhs.pairingCommand && lhs.pairingURL == rhs.pairingURL && lhs.pairCopiedUntil == rhs.pairCopiedUntil &&
        lhs.savedUntil == rhs.savedUntil && lhs.testResult?.ok == rhs.testResult?.ok && lhs.testResult?.detail == rhs.testResult?.detail &&
        lhs.testResultIsLatest == rhs.testResultIsLatest
    }

    mutating func resetPersonal(preservePendingPair: Bool = false) {
        editingPersonal = false
        manualEntry = false
        if !preservePendingPair { pendingPairRequest = nil }
        pairingInstruction = ""
        pairingCommand = nil
        pairingURL = nil
        pairCopiedUntil = .distantPast
        savedUntil.removeValue(forKey: "user")
    }
    mutating func resetGateway() {
        editingGateway = false
        savedUntil.removeValue(forKey: "hub")
    }
    mutating func resetAll() { self = ChannelUiState() }
}
