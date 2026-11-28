import AppKit

extension RemotePeer {
    var copyFeedbackKey: TransientFeedbackStore.Key { .init(.directoryCopy, scoped_id) }
}

extension ActivityItem {
    var copyFeedbackKey: TransientFeedbackStore.Key { .init(.activityCopy, "\(hub)|\(signal_id)") }

    var replyTarget: String? {
        guard can_reply == true, let from_scoped_id, !from_scoped_id.isEmpty else { return nil }
        return from_scoped_id
    }
}

// AppKit reserves table hits on custom NSControls for row selection. Give the
// existing row actions an ordinary view target without intercepting row hits.
final class DirectoryActionHitView: NSView {
    private let controls: [MenuControl]

    init(_ controls: [MenuControl], frame: NSRect) {
        self.controls = controls
        super.init(frame: frame)
        autoresizingMask = [.width, .height]
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

    func control(at point: NSPoint) -> MenuControl? {
        guard frame.contains(point) else { return nil }
        return controls.first { $0.isEnabled && $0.pressed != nil && $0.hitTest(point) === $0 }
    }

    override func hitTest(_ point: NSPoint) -> NSView? {
        control(at: point) == nil ? nil : self
    }

    override func mouseDown(with event: NSEvent) {
        guard let parent = superview else { return }
        control(at: parent.convert(event.locationInWindow, from: nil))?.mouseDown(with: event)
    }
}

final class DirectoryPeerCellView: NSTableCellView {
    let badgeView: SurfaceView
    let unreadAccentBar = roundedView(.zero, radius: 1.5, color: CitoPalette.infoText)
    let rowDivider = roundedView(.zero, radius: 0, color: .labelColor.withAppearanceAlpha(0.05))
    let deleteControl = MenuControl("Delete message", frame: NSRect(x:0,y:0,width:24,height:22), symbol:"xmark")
    var inspectionId = ""
    private(set) var activityItem: ActivityItem?
    private var copyAction: ((MenuControl) -> Void)?
    private var messageAction: (() -> Void)?
    private var deleteAction: (() -> Void)?
    private var heldBadge: SurfaceView?
    let badgeIcon: NSImageView
    let line1TitleLabel: NSTextField
    let line1RepoLabel: NSTextField
    let line2UserLabel: NSTextField
    let line2MetaLabel: NSTextField
    let msgControl: MenuControl
    let copyControl: MenuControl

    override init(frame frameRect: NSRect) {
        badgeView = roundedView(NSRect(x: 14, y: 12, width: 28, height: 28), radius: 7, color: .clear)
        badgeIcon = NSImageView(frame: NSRect(x: 6, y: 6, width: 16, height: 16))
        badgeView.addSubview(badgeIcon)
        line1TitleLabel = textLabel("", NSRect(x: 52, y: 26, width: 180, height: 18), size: 13, weight: .semibold, color: .labelColor)
        line1RepoLabel = textLabel("", NSRect(x: 232, y: 26, width: 180, height: 18), size: 11.5, weight: .regular, color: CitoPalette.secondaryText)
        line2UserLabel = textLabel("", NSRect(x: 52, y: 7, width: 80, height: 16), size: 11.5, weight: .medium, color: .labelColor)
        line2MetaLabel = textLabel("", NSRect(x: 132, y: 7, width: 220, height: 16), size: 11.5, weight: .regular, color: CitoPalette.secondaryText)
        msgControl = MenuControl("Message", frame: NSRect(x: 356, y: 16, width: 78, height: 22), leadingSymbol: "bubble.left")
        copyControl = MenuControl("", frame: NSRect(x: 440, y: 16, width: 104, height: 22), leadingSymbol: "doc.on.doc")
        super.init(frame: frameRect)
        identifier = NSUserInterfaceItemIdentifier("DirectoryPeerCell")
        addSubview(badgeView)
        unreadAccentBar.identifier = NSUserInterfaceItemIdentifier("unread_accent_bar")
        rowDivider.identifier = NSUserInterfaceItemIdentifier("activity_row_divider")
        addSubview(unreadAccentBar); addSubview(rowDivider)
        deleteControl.style = .subtleHeader
        addSubview(deleteControl)
        addSubview(line1TitleLabel)
        addSubview(line1RepoLabel)
        addSubview(line2UserLabel)
        addSubview(line2MetaLabel)
        addSubview(msgControl)
        addSubview(copyControl)
        addSubview(DirectoryActionHitView([deleteControl, msgControl, copyControl], frame: bounds))
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

    private static func activityMessageWidth(_ rowWidth: CGFloat) -> CGFloat { max(0, rowWidth - 68) }

    static func selectedActivityHeight(_ message: String, rowWidth: CGFloat) -> CGFloat {
        let width = activityMessageWidth(rowWidth)
        let font = NSFont.systemFont(ofSize: 12)
        guard measureTextWidth(message, font: font) > width || message.contains("\n") else { return 60 }
        return min(108, max(60, 44 + measureWrappedHeight(message, width: width, font: font)))
    }

    override func resizeSubviews(withOldSize oldSize: NSSize) {
        super.resizeSubviews(withOldSize: oldSize)
        if oldSize != bounds.size { layoutRowContents() }
    }

    private func layoutRowContents() {
        let right = bounds.width - 16
        if let item = activityItem {
            let height = bounds.height
            let expanded = height > 60
            badgeView.frame.origin.y = height - 40
            deleteControl.frame.origin.y = height - 30
            copyControl.frame.origin.y = height - 30
            msgControl.frame.origin.y = height - 30
            line1TitleLabel.frame.origin.y = height - 28
            line1RepoLabel.frame.origin.y = height - 28
            unreadAccentBar.frame.size.height = max(0, height - 16)
            deleteControl.frame.origin.x = right - deleteControl.frame.width
            copyControl.frame.origin.x = deleteControl.frame.minX - 6 - copyControl.frame.width
            msgControl.frame.origin.x = copyControl.frame.minX - 6 - msgControl.frame.width
            let timeRight = (msgControl.isHidden ? copyControl.frame.minX : msgControl.frame.minX) - 6
            line1RepoLabel.frame.origin.x = timeRight - line1RepoLabel.frame.width
            var titleRight = line1RepoLabel.frame.minX - 6
            if let heldBadge {
                heldBadge.frame.origin.y = height - 27
                heldBadge.frame.origin.x = titleRight - heldBadge.frame.width
                titleRight = heldBadge.frame.minX - 6
            }
            line1TitleLabel.frame.size.width = max(0, titleRight - line1TitleLabel.frame.minX)
            let messageHeight = expanded ? height - 42 : 18
            line2MetaLabel.maximumNumberOfLines = expanded ? 4 : 1
            line2MetaLabel.lineBreakMode = expanded ? .byWordWrapping : .byTruncatingTail
            line2MetaLabel.cell?.truncatesLastVisibleLine = true
            line2MetaLabel.stringValue = expanded ? item.message : item.message.replacingOccurrences(of:"\\s*\\n+\\s*",with:"  ",options:.regularExpression)
            line2MetaLabel.frame = NSRect(x:52,y:height - 34 - messageHeight,width:Self.activityMessageWidth(bounds.width),height:messageHeight)
            rowDivider.frame.size.width = bounds.width
        } else {
            msgControl.frame.origin.x = right - msgControl.frame.width
            copyControl.frame.origin.x = msgControl.frame.minX - 6 - copyControl.frame.width
            let textRight = copyControl.frame.minX - 8
            let left = line1TitleLabel.frame.minX
            let room = max(0, textRight - left)
            line1TitleLabel.frame.size.width = min(room, measureTextWidth(line1TitleLabel.stringValue, font: .systemFont(ofSize: 13, weight: .semibold)) + 4)
            line1RepoLabel.frame.origin.x = line1TitleLabel.frame.maxX
            line1RepoLabel.frame.size.width = max(0, textRight - line1RepoLabel.frame.minX)
            line2UserLabel.frame.size.width = min(room, measureTextWidth(line2UserLabel.stringValue, font: .systemFont(ofSize: 11.5, weight: .medium)) + 2)
            line2MetaLabel.frame.origin.x = line2UserLabel.isHidden ? left : line2UserLabel.frame.maxX
            line2MetaLabel.frame.size.width = max(0, textRight - line2MetaLabel.frame.minX)
        }
    }

    func configure(with peer: RemotePeer, harnesses: [String: HarnessMeta], onCopy: @escaping (MenuControl) -> Void, onMessage: (() -> Void)? = nil) {
        let samePeer = inspectionId == "dir_peer:\(peer.scoped_id)"
        if !samePeer {
            copyControl.setHovered(false); msgControl.setHovered(false); deleteControl.setHovered(false)
            for control in [deleteControl, msgControl, copyControl] { control.invalidatePress() }
        }
        activityItem = nil
        copyAction = onCopy
        messageAction = onMessage
        deleteAction = nil
        heldBadge?.isHidden = true
        badgeView.frame.origin.y = 12
        unreadAccentBar.isHidden = true; deleteControl.isHidden = true; rowDivider.isHidden = true
        if deleteControl.pressed != nil { deleteControl.pressed = nil }
        line1TitleLabel.font = .systemFont(ofSize:13,weight:.semibold)
        line1TitleLabel.textColor = .labelColor
        line2MetaLabel.font = .systemFont(ofSize:11.5)
        line2MetaLabel.textColor = CitoPalette.secondaryText
        msgControl.style = .neutral; copyControl.style = .neutral
        line2MetaLabel.maximumNumberOfLines = 1
        line2MetaLabel.lineBreakMode = .byTruncatingTail
        line2MetaLabel.cell?.truncatesLastVisibleLine = false
        inspectionId = "dir_peer:\(peer.scoped_id)"
        let visual = resolveHarnessVisual(peer.harness, harnesses: harnesses)
        configureBadge(badgeView,icon:badgeIcon,id:peer.harness,symbol:visual.symbol,title:visual.name,tint:visual.tint,inset:6)

        let short = peer.display_address
        let copyAccessibilityLabel = "Copy address \(short)"
        let copyFont = NSFont.monospacedSystemFont(ofSize: 11, weight: .medium)
        let shownAddress = compactAddressLabel(short,maxWidth:190,font:copyFont,paddingX:11)
        let copyWidth = min(190,max(MenuControl.intrinsicWidth("✓ Copied", font:copyFont, leadingSymbol:"doc.on.doc"), MenuControl.intrinsicWidth(shownAddress, font:copyFont, leadingSymbol:"doc.on.doc")))
        copyControl.clearFeedback(title: shownAddress, accessibilityLabel: copyAccessibilityLabel)
        copyControl.isHidden = false
        copyControl.frame = NSRect(x: 0, y: 15, width: copyWidth, height: 22)
        copyControl.setLeadingSymbol("doc.on.doc")
        copyControl.label.font = copyFont
        copyControl.showFeedback(key: peer.copyFeedbackKey)
        copyControl.toolTip = short
        copyControl.identifier = NSUserInterfaceItemIdentifier("copy_dir:\(peer.scoped_id)")
        if !samePeer || copyControl.pressed == nil {
            copyControl.pressed = { [weak self, weak copyControl] in
                if let control = copyControl { self?.copyAction?(control) }
            }
        }

        let msgWidth: CGFloat = MenuControl.intrinsicWidth("Message", leadingSymbol:"bubble.left")
        msgControl.isHidden = false
        msgControl.frame = NSRect(x: 0, y: 15, width: msgWidth, height: 22)
        msgControl.setLeadingSymbol("bubble.left")
        msgControl.label.font = .systemFont(ofSize: 11, weight: .medium)
        msgControl.label.stringValue = "Message"
        msgControl.setAccessibilityLabel("Message")
        msgControl.toolTip = "Message \(peer.display_address)"
        msgControl.identifier = NSUserInterfaceItemIdentifier("msg_dir:\(peer.scoped_id)")
        if !samePeer || (onMessage != nil) != (msgControl.pressed != nil) {
            msgControl.pressed = onMessage == nil ? nil : { [weak self] in self?.messageAction?() }
        }

        let normalizedTitle = peer.use_app_title == true ? visual.name : peer.display_title ?? peer.title
        let displayTitle = peer.harness == "human" && peer.custom_title != true ? "@\(peer.user)" + (peer.is_self_principal == true ? " (You)" : "") : normalizedTitle
        line1TitleLabel.stringValue = displayTitle
        line1TitleLabel.frame = NSRect(x: 52, y: 26, width: 0, height: 18)
        line1TitleLabel.identifier = NSUserInterfaceItemIdentifier("dir_title:\(peer.scoped_id)")

        let branch = (peer.branch ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        let branchSuffix = (!branch.isEmpty && branch != "HEAD") ? " · \(branch)" : ""
        let cwd = peer.cwd ?? ""
        let shortCwd = compactHomePath(cwd)
        let context = (peer.repo ?? "").isEmpty ? (peer.harness == "human" ? "" : shortCwd) : peer.repo ?? ""
        let repoText = (context.isEmpty ? "" : " · \(context)") + branchSuffix
        line1RepoLabel.font = .systemFont(ofSize:11)
        line1RepoLabel.isHidden = false
        line1TitleLabel.toolTip = displayTitle
        line1TitleLabel.lineBreakMode = .byTruncatingMiddle
        line1RepoLabel.toolTip = repoText
        line1RepoLabel.lineBreakMode = .byTruncatingMiddle
        line1RepoLabel.stringValue = repoText
        line1RepoLabel.frame = NSRect(x: 0, y: 26, width: 0, height: 18)
        line1RepoLabel.identifier = NSUserInterfaceItemIdentifier("dir_repo:\(peer.scoped_id)")

        let userText = "@\(peer.user)"
        line2UserLabel.isHidden = peer.harness == "human"
        line2UserLabel.toolTip = peer.harness == "human" ? nil : userText
        line2UserLabel.stringValue = peer.harness == "human" ? "" : userText
        line2UserLabel.frame = NSRect(x: 52, y: 7, width: 0, height: 16)
        line2UserLabel.identifier = NSUserInterfaceItemIdentifier("dir_user:\(peer.scoped_id)")

        let internalTokens = ["mac", "hub", "human", "desktop notifications"]
        var hosts: [String] = []
        for raw in [peer.host_name] + (peer.channels ?? []) {
            let host = cleanHostDisplay(raw)
            if !host.isEmpty && !internalTokens.contains(host.lowercased()) && !hosts.contains(host) { hosts.append(host) }
        }
        let hostText = hosts.joined(separator: ", ")
        let displayAbout = peer.display_about
        let attachmentNames = displayAttachments((peer.attachments ?? []).filter { $0.availability == "ACTIVE" }).map { cleanHostDisplay($0.display_name) }
        let segments = (peer.harness == "human" ? ["Inbox"] + attachmentNames + [displayAbout ?? ""] : [hostText,displayAbout ?? ""]).filter { !$0.isEmpty }
        line2MetaLabel.stringValue = (peer.harness != "human" && !segments.isEmpty ? " · " : "") + segments.joined(separator:" · ")
        line2MetaLabel.frame = NSRect(x: 0, y: 7, width: 0, height: 16)
        line2MetaLabel.identifier = NSUserInterfaceItemIdentifier("dir_meta:\(peer.scoped_id)")
        line2MetaLabel.toolTip = line2MetaLabel.stringValue
        layoutRowContents()
        copyControl.layoutContents(); msgControl.layoutContents()
    }

    func configureActivity(with item: ActivityItem, harnesses: [String: HarnessMeta], canDelete: Bool = true, onCopy: @escaping (MenuControl) -> Void, onDelete: @escaping () -> Void, onReply: @escaping () -> Void) {
        let sameItem = activityItem?.hub == item.hub && activityItem?.signal_id == item.signal_id
        if !sameItem {
            copyControl.setHovered(false); msgControl.setHovered(false); deleteControl.setHovered(false)
            for control in [deleteControl, msgControl, copyControl] { control.invalidatePress() }
        }
        copyAction = onCopy
        deleteAction = onDelete
        messageAction = onReply
        activityItem = item
        inspectionId = "activity_row:\(item.signal_id)"
        let visual = resolveHarnessVisual(item.from_harness ?? item.target_harness, harnesses:harnesses)
        configureBadge(badgeView,icon:badgeIcon,id:item.from_harness ?? item.target_harness,symbol:visual.symbol,title:visual.name,tint:visual.tint,inset:6)
        let canReply = item.replyTarget != nil
        let copyFont = NSFont.systemFont(ofSize: 11, weight: .medium)
        let copyW = max(66, MenuControl.intrinsicWidth("✓ Copied", font:copyFont, leadingSymbol:"doc.on.doc"))
        let replyW = MenuControl.intrinsicWidth("Reply", font:copyFont, leadingSymbol:"arrowshape.turn.up.left")
        unreadAccentBar.isHidden = !item.unread
        unreadAccentBar.frame = NSRect(x:4,y:8,width:3,height:0)
        rowDivider.isHidden = false; rowDivider.frame = NSRect(x:0,y:0,width:bounds.width,height:1)
        deleteControl.isHidden = false
        deleteControl.isEnabled = canDelete
        deleteControl.frame = NSRect(x:0,y:0,width:24,height:22)
        deleteControl.identifier = NSUserInterfaceItemIdentifier("delete_activity:\(item.signal_id)")
        deleteControl.toolTip = "Delete message"
        if !sameItem || deleteControl.pressed == nil {
            deleteControl.pressed = { [weak self] in self?.deleteAction?() }
        }
        msgControl.isHidden = !canReply
        msgControl.style = .neutral
        msgControl.frame = NSRect(x:0,y:0,width:replyW,height:22)
        msgControl.setLeadingSymbol("arrowshape.turn.up.left")
        msgControl.label.stringValue = "Reply"
        msgControl.setAccessibilityLabel("Reply")
        msgControl.toolTip = "Reply"
        msgControl.identifier = NSUserInterfaceItemIdentifier("reply_activity:\(item.signal_id)")
        if !sameItem || canReply != (msgControl.pressed != nil) {
            msgControl.pressed = canReply ? { [weak self] in self?.messageAction?() } : nil
        }
        copyControl.isHidden = false
        copyControl.clearFeedback(title: "Copy", accessibilityLabel: "Copy message")
        copyControl.style = .neutral
        copyControl.frame = NSRect(x:0,y:0,width:copyW,height:22)
        copyControl.setLeadingSymbol("doc.on.doc")
        copyControl.label.font = copyFont
        copyControl.showFeedback(key: item.copyFeedbackKey)
        copyControl.toolTip = "Copy message"
        copyControl.identifier = NSUserInterfaceItemIdentifier("copy_activity_msg:\(item.signal_id)")
        if !sameItem || copyControl.pressed == nil {
            copyControl.pressed = { [weak self, weak copyControl] in
                if let control = copyControl { self?.copyAction?(control) }
            }
        }
        copyControl.applyVisualState()
        let held = ["stale_generation":"App reloaded","driver_unavailable":"App unavailable",
            "session_switched":"Conversation changed","unlistened":"Listening paused",
            "unlistened_before_dispatch":"Listening paused","fenced_before_dispatch":"Connection changed",
            "sender_ack_timeout":"Awaiting acknowledgment"][item.held_reason ?? ""]
        let senderTitle = item.display_from_title
        line1TitleLabel.stringValue = "@\(item.from_user)" + (senderTitle.map { " (\($0))" } ?? "") + " → " + (item.target_harness == "human" ? "You" : item.target_title)
        line1TitleLabel.font = .systemFont(ofSize:12.5,weight:item.unread ? .semibold : .medium)
        line1TitleLabel.textColor = item.unread ? .labelColor : CitoPalette.secondaryText
        line1TitleLabel.frame = NSRect(x:52,y:0,width:0,height:18)
        line1TitleLabel.lineBreakMode = .byTruncatingMiddle
        line1TitleLabel.toolTip = line1TitleLabel.stringValue
        line1TitleLabel.identifier = NSUserInterfaceItemIdentifier("act_title:\(item.signal_id)")
        let time=relativeTime(item.created_at)
        let timeWidth=ceil(measureTextWidth(time,font:.systemFont(ofSize:10.5)))+4
        line1RepoLabel.isHidden=false; line1RepoLabel.stringValue=time; line1RepoLabel.toolTip=nil
        line1RepoLabel.font = .systemFont(ofSize:10.5); line1RepoLabel.textColor=CitoPalette.secondaryText
        line1RepoLabel.frame=NSRect(x:0,y:0,width:timeWidth,height:18)
        line1RepoLabel.identifier=NSUserInterfaceItemIdentifier("act_time:\(item.signal_id)")
        heldBadge?.removeFromSuperview(); heldBadge=nil
        if let reason=held {
            let badge=makePillBadge("Held: " + reason,id:"act_held:\(item.signal_id)",height:16,font:.systemFont(ofSize:8.5,weight:.medium),textColor:CitoPalette.warningText,fill:CitoPalette.warningSurface,border:CitoPalette.warningBorder,paddingX:5,minWidth:0)
            badge.toolTip="Held: " + reason
            addSubview(badge); heldBadge=badge
        }
        line2UserLabel.isHidden = true
        line2MetaLabel.toolTip = item.message
        line2MetaLabel.font = .systemFont(ofSize:12)
        line2MetaLabel.textColor = item.unread ? .labelColor : CitoPalette.secondaryText
        layoutRowContents()
        msgControl.layoutContents(); copyControl.layoutContents()
        line2MetaLabel.identifier = NSUserInterfaceItemIdentifier("act_msg:\(item.signal_id)")
    }
}
