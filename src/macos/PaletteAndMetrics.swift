import AppKit

enum CitoMetrics {
    static let gap: CGFloat = 6
    static let menuWidth: CGFloat = 380
    static let menuInset: CGFloat = 12
    static let menuRailWidth = menuWidth - 2 * menuInset
    static let settingsWidth: CGFloat = 520
    static let settingsInset: CGFloat = 20
    static let cardInset: CGFloat = 12
    static let headerInset: CGFloat = 14
}

func normalizedHubURL(_ raw: String) -> String {
    let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
    return trimmed.hasSuffix("://") ? trimmed : trimmed.replacingOccurrences(of: "/+$", with: "", options: .regularExpression)
}

func proposedHubName(_ raw: String, url: String, existing: [HubConfigItem], creating: Bool, original: String = "") -> String {
    let url = normalizedHubURL(url)
    let automatic = raw.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty
    var source = raw
    if automatic {
        let host = URLComponents(string:url)?.host ?? ""
        let labels = host.split(separator:".")
        if labels.count == 4 && labels.allSatisfy({ Int($0).map { (0...255).contains($0) } ?? false }) {
            let ip = labels.compactMap { Int($0) }
            source = ip[0] == 10 || ip[0] == 192 && ip[1] == 168 || ip[0] == 172 && (16...31).contains(ip[1]) ? "lan" : (ip[0] == 100 && (64...127).contains(ip[1]) ? "tailscale" : "remote")
        } else if !host.contains(":"), let first = labels.first(where: { $0.unicodeScalars.contains { $0.isASCII && CharacterSet.alphanumerics.contains($0) } }) {
            source = String(first)
        } else { source = "remote" }
    }
    var slug = ""
    for scalar in source.unicodeScalars {
        if scalar.isASCII && CharacterSet.alphanumerics.contains(scalar) {
            if slug.count < 32 { slug += String(scalar).lowercased() }
        } else if !slug.isEmpty && !slug.hasSuffix("-") && slug.count < 32 { slug += "-" }
    }
    slug = slug.trimmingCharacters(in:CharacterSet(charactersIn:"-"))
    if automatic {
        if slug.isEmpty { slug = "remote" }
        let base = slug
        var suffix = 2
        while slug == "local" || existing.contains(where:{(creating || $0.name != original) && $0.name == slug && normalizedHubURL($0.url) != url}) {
            let ending = "-\(suffix)"
            slug = String(base.prefix(32-ending.count)).trimmingCharacters(in:CharacterSet(charactersIn:"-")) + ending
            suffix += 1
        }
    }
    return automatic && !creating && !original.isEmpty ? original : slug
}

extension NSColor {
    // Applying alpha eagerly snapshots semantic RGB values; defer it until appearance resolution.
    func withAppearanceAlpha(_ alpha: CGFloat) -> NSColor {
        NSColor(name: nil) { appearance in
            var resolved = self
            appearance.performAsCurrentDrawingAppearance { resolved = self.withAlphaComponent(alpha) }
            return resolved
        }
    }
}

// Semantic foregrounds retain WCAG AA contrast on both translucent surface backdrops.
enum CitoPalette {
    private static func adaptive(_ light: NSColor, _ dark: NSColor) -> NSColor {
        NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? dark : light
        }
    }
    static let secondaryText = adaptive(
        NSColor(srgbRed: 0.28, green: 0.29, blue: 0.31, alpha: 1),
        NSColor(srgbRed: 0.78, green: 0.79, blue: 0.82, alpha: 1))
    static let successText = adaptive(
        NSColor(srgbRed: 0.04, green: 0.43, blue: 0.16, alpha: 1),
        NSColor(srgbRed: 0.28, green: 0.85, blue: 0.44, alpha: 1))
    static let warningText = adaptive(
        NSColor(srgbRed: 0.61, green: 0.26, blue: 0.00, alpha: 1),
        NSColor(srgbRed: 1.00, green: 0.70, blue: 0.25, alpha: 1))
    static let dangerText = adaptive(
        NSColor(srgbRed: 0.74, green: 0.10, blue: 0.14, alpha: 1),
        NSColor(srgbRed: 1.00, green: 0.44, blue: 0.42, alpha: 1))
    static let infoText = adaptive(
        NSColor(srgbRed: 0.00, green: 0.34, blue: 0.75, alpha: 1),
        NSColor(srgbRed: 0.40, green: 0.74, blue: 1.00, alpha: 1))
    static let copiedFill = adaptive(
        NSColor(srgbRed: 0.04, green: 0.43, blue: 0.16, alpha: 0.14),
        NSColor(srgbRed: 0.19, green: 0.82, blue: 0.35, alpha: 0.22))
    static let copiedBorder = adaptive(
        NSColor(srgbRed: 0.04, green: 0.43, blue: 0.16, alpha: 0.34),
        NSColor(srgbRed: 0.19, green: 0.82, blue: 0.35, alpha: 0.45))
    static let warningFill = adaptive(
        NSColor(srgbRed: 0.64, green: 0.28, blue: 0.00, alpha: 0.14),
        NSColor(srgbRed: 1.00, green: 0.62, blue: 0.04, alpha: 0.18))
    // Warning chips use an opaque text-surface base, avoiding stacked tint losses.
    static let warningSurface = NSColor(name: nil) { appearance in
        var color = NSColor.clear
        appearance.performAsCurrentDrawingAppearance {
            let fill = warningFill.usingColorSpace(.sRGB)
            color = surfaceBackground.blended(withFraction: fill?.alphaComponent ?? 0,
                of: fill?.withAlphaComponent(1) ?? .clear) ?? surfaceBackground
        }
        return color
    }
    static let warningBorder = adaptive(
        NSColor(srgbRed: 0.64, green: 0.28, blue: 0.00, alpha: 0.32),
        NSColor(srgbRed: 1.00, green: 0.62, blue: 0.04, alpha: 0.40))
    static let surfaceBackground = adaptive(
        NSColor(srgbRed: 236/255, green: 239/255, blue: 243/255, alpha: 1),
        NSColor(srgbRed: 30/255, green: 32/255, blue: 36/255, alpha: 1))
    // Filled controls keep white titles, so their fill stays dark in either appearance.
    static let prominentFill = NSColor(srgbRed: 0, green: 87.0/255, blue: 191.0/255, alpha: 1)
    static let destructiveFill = NSColor(srgbRed: 189.0/255, green: 25.0/255, blue: 36.0/255, alpha: 1)
    // Opaque badge fills guarantee white-symbol contrast in both appearances.
    static func badgeFill(_ name: String) -> NSColor {
        switch name.lowercased() {
        case "green", "human": return NSColor(srgbRed:0.04,green:0.43,blue:0.16,alpha:1)
        case "teal": return NSColor(srgbRed:0.00,green:0.45,blue:0.48,alpha:1)
        case "orange", "amber", "yellow": return NSColor(srgbRed:0.66,green:0.36,blue:0.00,alpha:1)
        case "red": return destructiveFill
        case "pink": return NSColor(srgbRed:0.72,green:0.18,blue:0.39,alpha:1)
        case "purple", "indigo": return NSColor(srgbRed:0.40,green:0.29,blue:0.72,alpha:1)
        case "gray": return NSColor(srgbRed:0.43,green:0.44,blue:0.46,alpha:1)
        default: return prominentFill
        }
    }
}

/// Compact only the chip presentation; callers retain the full address for copying.
func compactAddressLabel(_ address: String, maxWidth: CGFloat, font: NSFont, paddingX: CGFloat) -> String {
    if MenuControl.intrinsicWidth(address,font:font,leadingSymbol:"doc.on.doc",paddingX:paddingX) <= maxWidth { return address }
    for count in stride(from:max(1,address.count-1),through:1,by:-1) {
        let text = String(address.prefix((count+1)/2)) + "…" + String(address.suffix(count/2))
        if MenuControl.intrinsicWidth(text,font:font,leadingSymbol:"doc.on.doc",paddingX:paddingX) <= maxWidth { return text }
    }
    return "…"
}

func compactSessionPath(_ shortCwd: String, branchSuffix: String, maxWidth: CGFloat = 244,
                        font: NSFont = .systemFont(ofSize: 10.5)) -> String {
    let full = shortCwd + branchSuffix
    guard measureTextWidth(full, font: font) > maxWidth else { return full }
    let segments = shortCwd.split(separator: "/")
    guard segments.count >= 3, let leaf = segments.last else { return full }
    let prefix = shortCwd.hasPrefix("~") ? "~/…/" : "/…/"
    let parentAndLeaf = prefix + segments.suffix(2).joined(separator: "/") + branchSuffix
    if measureTextWidth(parentAndLeaf, font: font) <= maxWidth { return parentAndLeaf }
    return prefix + leaf + branchSuffix
}

func activityRowHeight(_ text: String, selected: Bool) -> CGFloat {
    let font = NSFont.systemFont(ofSize: 12)
    guard selected && (measureTextWidth(text, font: font) > 492 || text.contains("\n")) else { return 60 }
    let wrappedRect = (text as NSString).boundingRect(with: NSSize(width: 492, height: CGFloat.greatestFiniteMagnitude),
        options: [.usesLineFragmentOrigin, .usesFontLeading], attributes: [.font: font])
    return min(108, max(60, 44 + ceil(wrappedRect.height)))
}

func cleanHostDisplay(_ raw: String) -> String {
    raw.replacingOccurrences(of: "\\.local\\b", with: "", options: [.regularExpression, .caseInsensitive])
}
func isGenericSessionLabel(_ raw: String, user: String, homeBasename: String = (NSHomeDirectory() as NSString).lastPathComponent) -> Bool {
    let label = raw.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    let identity = label.hasPrefix("@") ? String(label.dropFirst()) : label
    let homeBasename = homeBasename.lowercased()
    let user = user.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    return label.isEmpty || identity == user || identity == homeBasename
        || label == "you (@\(user))" || label == "@\(user) (me)"
        || ["agent", "agent session", "session", "cli session", "human", "human node", "human channel", "human receiver", "you", "coding session", "default"].contains(label)
}
func nodeDisplayTitle(_ title: String, harness: String, user: String, about: String? = nil, harnessName: String? = nil, customTitle: Bool = false) -> String {
    if customTitle { return title }
    let raw = title.trimmingCharacters(in: .whitespacesAndNewlines)
    if harness == "human" && (raw.isEmpty || raw == "Human" || raw == "@\(user)" || raw == "@\(user) (Me)" || raw.hasPrefix("Human Node")) {
        return "You (@\(user))"
    }
    // A meaningful title remains authoritative even when its session lives at home.
    if harness != "human" && isGenericSessionLabel(raw, user: user) {
        return distinctAbout(about, title: raw, user: user) ?? (harnessName ?? harness)
    }
    return title
}
func presentationError(_ raw: String) -> String {
    var message = raw.trimmingCharacters(in: .whitespacesAndNewlines)
    // Unwrap transport/plugin diagnostics before rendering their human message.
    while !message.isEmpty {
        if message.hasPrefix("CONTROL_REVISION_CONFLICT:") { return "Inbox changed on another device. Review the current state and try again." }
        if message.range(of:"(?i:SQLITE_|sqlite3|\\(SQLite\\b|unable to open database|database is locked|database disk image is malformed|no such table:)",options:.regularExpression) != nil {
            return "App database is unavailable. Check the app and its data location."
        }
        if message.hasPrefix("{") || message.hasPrefix("[") {
            guard let data = message.data(using:.utf8), let object = try? JSONSerialization.jsonObject(with:data) as? [String:Any] else { return "The request failed. Try again." }
            let error = object["error"] as? [String:Any]
            let detail = object["message"] as? String ?? error?["message"] as? String ?? object["error"] as? String
            guard let detail = detail, !detail.isEmpty, detail != message else { return "The request failed. Try again." }
            message = detail; continue
        }
        let stripped = message.replacingOccurrences(of:"^(?:[a-z][a-z0-9_-]*|[A-Z][A-Z0-9_]{2,}|[0-9]{3,}|(?i:Discovery rejected|Badge rejected|Error)):(?:\\s+|$)",with:"",options:.regularExpression)
        if stripped == message { break }
        message = stripped
    }
    message = message.replacingOccurrences(of:"\\b(?:[A-Z][A-Z0-9_]{2,}|[0-9]{3,}):(?:\\s+|$)",with:"",options:.regularExpression)
        .replacingOccurrences(of: "\\s*\\(os error [0-9]+\\)$", with: "", options: .regularExpression)
    if message.hasPrefix("Hub URL must use") { return "Enter a Hub URL using HTTP or HTTPS, without credentials, query, or fragment." }
    if message.hasPrefix("CITO_NATS_URL") { return "Enter a valid Hub URL (http://ip:4291 or https://host)." }
    let bindPrefix = "Cannot bind host hub listener: "
    if message.hasPrefix(bindPrefix) {
        let reason = String(message.dropFirst(bindPrefix.count))
        return "Cannot start sharing: " + reason
    }
    for (token, copy) in [("DISCONNECTED_RETAINED", "Temporarily offline"), ("UNLISTENED", "Not listening"), ("EXPIRED", "Offline"), ("ACTIVE", "Active"), ("CITO_OIDC_ISSUER", "OIDC issuer URL"), ("CITO_OIDC_CLIENT_ID", "OIDC client ID"), ("CITO_HUB_TOKEN", "shared access token"), ("CITO_AUTH_MODE", "access mode")] {
        message = message.replacingOccurrences(of:"\\b" + token + "\\b",with:copy,options:.regularExpression)
    }
    message = message.replacingOccurrences(of:"(?i:\\bchannel inventory\\b)",with:"Channel list",options:.regularExpression)
        .replacingOccurrences(of:"local://",with:"Built-in on this Mac")
        .replacingOccurrences(of:" (http://127.0.0.1 is allowed for tests)",with:"")
    return message
}
func distinctAbout(_ raw: String?, title: String, user: String, otherTitle: String? = nil) -> String? {
    let value = (raw ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
    // Descriptions use the daemon/UI process HOME; discovered folder titles use the account home.
    return isGenericSessionLabel(value, user:user, homeBasename:((ProcessInfo.processInfo.environment["HOME"] ?? "") as NSString).lastPathComponent)
        || [title, otherTitle ?? ""].contains { value.lowercased() == $0.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() } ? nil : value
}

func diagnostic(_ message: String) {
    FileHandle.standardError.write(Data(("cito-ui: " + message + "\n").utf8))
}

func displayAttachments(_ attachments: [ChannelAttachment]) -> [ChannelAttachment] {
    var unique: [String: ChannelAttachment] = [:]
    for item in attachments where ["ACTIVE", "DISCONNECTED_RETAINED"].contains(item.availability) {
        let key = item.location + "|" + cleanHostDisplay(item.display_name)
        if let old = unique[key], (old.is_local ? 2 : 0) + (old.availability == "ACTIVE" ? 1 : 0) >= (item.is_local ? 2 : 0) + (item.availability == "ACTIVE" ? 1 : 0) { continue }
        unique[key] = item
    }
    return unique.values.sorted { ($0.location, $0.display_name) < ($1.location, $1.display_name) }
}

func relativeTime(_ timestamp: String) -> String {
    let formatter = ISO8601DateFormatter()
    formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    let date = formatter.date(from: timestamp) ?? {
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.date(from: timestamp)
    }()
    guard let date = date else { return "Just now" }
    let seconds = max(0, Date().timeIntervalSince(date))
    if seconds < 60 { return "Just now" }
    if seconds < 3600 { return "\(Int(seconds / 60))m ago" }
    if seconds < 86400 { return "\(Int(seconds / 3600))h ago" }
    return "\(Int(seconds / 86400))d ago"
}
