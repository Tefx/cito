import AppKit

// NSTableView retains row-selection hits for custom NSControl subclasses.
// Route embedded menu controls explicitly while preserving native row selection.
final class DirectoryTableView: NSTableView {
    override func hitTest(_ point: NSPoint) -> NSView? {
        let hit = super.hitTest(point)
        guard hit === self else { return hit }
        let local = convert(point, from:superview)
        let index = row(at:local)
        if index >= 0, let row = rowView(atRow:index,makeIfNecessary:false),
           let control = row.hitTest(local) as? MenuControl, control.isEnabled {
            return control
        }
        return hit
    }
}

final class DirectoryPeerCellView: NSTableCellView {
    let badgeView: SurfaceView
    let unreadAccentBar = roundedView(.zero, radius: 1.5, color: CitoPalette.infoText)
    let rowDivider = roundedView(.zero, radius: 0, color: .labelColor.withAppearanceAlpha(0.05))
    let deleteControl = MenuControl("Delete message", frame: NSRect(x:520,y:30,width:24,height:22), symbol:"xmark")
    var inspectionId = ""
    private(set) var activityItem: ActivityItem?
    private var heldBadge: SurfaceView?
    private var isSelectedRow = false
    override var wantsUpdateLayer: Bool { true }
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
        wantsLayer = true
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
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

    private func applySelectionColor() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.backgroundColor = (isSelectedRow ? NSColor.controlAccentColor.withAlphaComponent(0.22) : NSColor.clear).cgColor
        }
    }
    func setSelected(_ selected: Bool) {
        guard isSelectedRow != selected else { return }
        isSelectedRow = selected
        applySelectionColor()
    }
    override func updateLayer() { applySelectionColor() }
    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applySelectionColor()
    }

    func configure(with peer: RemotePeer, selected: Bool, harnesses: [String: HarnessMeta], onCopy: @escaping (String, MenuControl) -> Void, onMessage: (() -> Void)? = nil) {
        let samePeer = inspectionId == "dir_peer:\(peer.scoped_id)"
        let keepCopyFeedback = samePeer && copyControl.feedbackTimer?.isValid == true
        if !samePeer { copyControl.setHovered(false); msgControl.setHovered(false); deleteControl.setHovered(false) }
        activityItem = nil
        heldBadge?.isHidden = true
        badgeView.frame.origin.y = 12
        unreadAccentBar.isHidden = true; deleteControl.isHidden = true; rowDivider.isHidden = true
        line1TitleLabel.font = .systemFont(ofSize:13,weight:.semibold)
        line1TitleLabel.textColor = .labelColor
        line2MetaLabel.font = .systemFont(ofSize:11.5)
        line2MetaLabel.textColor = CitoPalette.secondaryText
        msgControl.style = .neutral; copyControl.style = .neutral
        line2MetaLabel.maximumNumberOfLines = 1
        line2MetaLabel.lineBreakMode = .byTruncatingTail
        inspectionId = "dir_peer:\(peer.scoped_id)"
        isSelectedRow = selected
        frame = NSRect(x: 0, y: 0, width: 560, height: 52)
        applySelectionColor()
        let visual = resolveHarnessVisual(peer.harness, harnesses: harnesses)
        configureBadge(badgeView,icon:badgeIcon,id:peer.harness,symbol:visual.symbol,title:visual.name,tint:visual.tint,inset:6)

        let short = peer.display_address
        let copyFont = NSFont.monospacedSystemFont(ofSize: 11, weight: .medium)
        let shownAddress = compactAddressLabel(short,maxWidth:190,font:copyFont,paddingX:11)
        let copyWidth = min(190,max(MenuControl.intrinsicWidth("✓ Copied", font:copyFont, leadingSymbol:"doc.on.doc"), MenuControl.intrinsicWidth(shownAddress, font:copyFont, leadingSymbol:"doc.on.doc")))
        if !keepCopyFeedback {
            copyControl.feedbackTimer?.invalidate()
            copyControl.feedbackTimer = nil
            copyControl.restingTitle = nil
            copyControl.restingAccessibilityLabel = nil
            copyControl.label.stringValue = shownAddress
        }
        copyControl.isHidden = false
        copyControl.frame = NSRect(x: 544 - MenuControl.intrinsicWidth("Message", leadingSymbol:"bubble.left") - 6 - copyWidth, y: 15, width: copyWidth, height: 22)
        copyControl.setLeadingSymbol("doc.on.doc")
        copyControl.label.font = copyFont
        copyControl.leadingSymbolView?.isHidden = keepCopyFeedback
        copyControl.applyVisualState()
        if keepCopyFeedback { copyControl.restingTitle = shownAddress; copyControl.restingAccessibilityLabel = short }
        else { copyControl.setAccessibilityLabel(short) }
        copyControl.toolTip = short
        copyControl.identifier = NSUserInterfaceItemIdentifier("copy_dir:\(peer.scoped_id)")
        copyControl.pressed = { [weak copyControl] in
            if let control = copyControl { onCopy(peer.display_address, control) }
        }

        let msgWidth: CGFloat = MenuControl.intrinsicWidth("Message", leadingSymbol:"bubble.left")
        msgControl.isHidden = false
        msgControl.frame = NSRect(x: 544 - msgWidth, y: 15, width: msgWidth, height: 22)
        msgControl.setLeadingSymbol("bubble.left")
        msgControl.label.font = .systemFont(ofSize: 11, weight: .medium)
        msgControl.label.stringValue = "Message"
        msgControl.setAccessibilityLabel("Message")
        msgControl.toolTip = "Message \(peer.display_address)"
        msgControl.identifier = NSUserInterfaceItemIdentifier("msg_dir:\(peer.scoped_id)")
        msgControl.pressed = onMessage

        let availWidth = max(120, 544 - copyWidth - msgWidth - 66)
        let titleFont = NSFont.systemFont(ofSize: 13, weight: .semibold)
        let normalizedTitle = nodeDisplayTitle(peer.title, harness: peer.harness, user: peer.user, about: peer.about, harnessName: visual.name, customTitle: peer.custom_title == true)
        let displayTitle = peer.harness == "human" && peer.custom_title != true ? "@\(peer.user)" + (peer.is_self_principal == true ? " (You)" : "") : normalizedTitle
        let titleW = min(availWidth, measureTextWidth(displayTitle, font: titleFont) + 4)
        line1TitleLabel.stringValue = displayTitle
        line1TitleLabel.frame = NSRect(x: 52, y: 26, width: titleW, height: 18)
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
        line1RepoLabel.frame = NSRect(x: 52 + titleW, y: 26, width: max(0, availWidth - titleW), height: 18)
        line1RepoLabel.identifier = NSUserInterfaceItemIdentifier("dir_repo:\(peer.scoped_id)")

        let userText = "@\(peer.user)"
        let userFont = NSFont.systemFont(ofSize: 11.5, weight: .medium)
        let userW = min(availWidth, measureTextWidth(userText, font: userFont) + 2)
        line2UserLabel.isHidden = peer.harness == "human"
        line2UserLabel.stringValue = peer.harness == "human" ? "" : userText
        line2UserLabel.frame = NSRect(x: 52, y: 7, width: userW, height: 16)
        line2UserLabel.identifier = NSUserInterfaceItemIdentifier("dir_user:\(peer.scoped_id)")

        let internalTokens = ["mac", "hub", "human", "desktop notifications"]
        var hosts: [String] = []
        for raw in [peer.host_name] + (peer.channels ?? []) {
            let host = cleanHostDisplay(raw)
            if !host.isEmpty && !internalTokens.contains(host.lowercased()) && !hosts.contains(host) { hosts.append(host) }
        }
        let hostText = hosts.joined(separator: ", ")
        let displayAbout = distinctAbout(peer.about, title:peer.title, user:peer.user, otherTitle:displayTitle)
        let attachmentNames = displayAttachments(peer.attachments ?? []).map { cleanHostDisplay($0.display_name) }
        let segments = (peer.harness == "human" ? ["Inbox"] + attachmentNames + [displayAbout ?? ""] : [hostText,displayAbout ?? ""]).filter { !$0.isEmpty }
        line2MetaLabel.stringValue = (peer.harness != "human" && !segments.isEmpty ? " · " : "") + segments.joined(separator:" · ")
        let metaOffset = peer.harness == "human" ? 0 : userW
        line2MetaLabel.frame = NSRect(x: 52 + metaOffset, y: 7, width: max(0, availWidth - metaOffset), height: 16)
        line2MetaLabel.identifier = NSUserInterfaceItemIdentifier("dir_meta:\(peer.scoped_id)")
        line2MetaLabel.toolTip = line2MetaLabel.stringValue
        copyControl.layoutContents(); msgControl.layoutContents()
    }

    func configureActivity(with item: ActivityItem, selected: Bool, harnesses: [String: HarnessMeta], onCopy: @escaping (MenuControl) -> Void, onDelete: @escaping () -> Void, onReply: @escaping () -> Void) {
        let sameItem = activityItem?.hub == item.hub && activityItem?.signal_id == item.signal_id
        let keepCopyFeedback = sameItem && copyControl.feedbackTimer?.isValid == true
        if !sameItem { copyControl.setHovered(false); msgControl.setHovered(false); deleteControl.setHovered(false) }
        activityItem = item
        inspectionId = "activity_row:\(item.signal_id)"
        isSelectedRow = selected
        let rowHeight = activityRowHeight(item.message, selected: selected)
        let expanded = rowHeight > 60
        frame = NSRect(x:0,y:0,width:560,height:rowHeight)
        applySelectionColor()
        let visual = resolveHarnessVisual(item.from_harness ?? item.target_harness, harnesses:harnesses)
        badgeView.frame.origin.y = rowHeight - 40
        configureBadge(badgeView,icon:badgeIcon,id:item.from_harness ?? item.target_harness,symbol:visual.symbol,title:visual.name,tint:visual.tint,inset:6)
        let canReply = item.can_reply == true
        let copyFont = NSFont.systemFont(ofSize: 11, weight: .medium)
        let copyW = max(66, MenuControl.intrinsicWidth("✓ Copied", font:copyFont, leadingSymbol:"doc.on.doc"))
        let copyX = 514 - copyW
        let replyW = MenuControl.intrinsicWidth("Reply", font:copyFont, leadingSymbol:"arrowshape.turn.up.left")
        let textW = (canReply ? copyX - 6 - replyW : copyX) - 58
        unreadAccentBar.isHidden = !item.unread
        unreadAccentBar.frame = NSRect(x:4,y:8,width:3,height:rowHeight-16)
        rowDivider.isHidden = false; rowDivider.frame = NSRect(x:0,y:0,width:560,height:1)
        deleteControl.isHidden = false
        deleteControl.frame = NSRect(x:520,y:rowHeight-30,width:24,height:22)
        deleteControl.identifier = NSUserInterfaceItemIdentifier("delete_activity:\(item.signal_id)")
        deleteControl.toolTip = "Delete message"
        deleteControl.pressed = onDelete
        msgControl.isHidden = !canReply
        msgControl.style = .neutral
        msgControl.frame = NSRect(x:copyX-6-replyW,y:rowHeight-30,width:replyW,height:22)
        msgControl.setLeadingSymbol("arrowshape.turn.up.left")
        msgControl.label.stringValue = "Reply"
        msgControl.setAccessibilityLabel("Reply")
        msgControl.toolTip = "Reply"
        msgControl.identifier = NSUserInterfaceItemIdentifier("reply_activity:\(item.signal_id)")
        msgControl.pressed = canReply ? onReply : nil
        copyControl.isHidden = false
        if !keepCopyFeedback {
            copyControl.feedbackTimer?.invalidate(); copyControl.feedbackTimer = nil
            copyControl.restingTitle = nil
            copyControl.restingAccessibilityLabel = nil
            copyControl.label.stringValue = "Copy"
        }
        copyControl.style = .neutral
        copyControl.frame = NSRect(x:copyX,y:rowHeight-30,width:copyW,height:22)
        copyControl.setLeadingSymbol("doc.on.doc")
        copyControl.leadingSymbolView?.isHidden = keepCopyFeedback
        copyControl.label.font = copyFont
        if !keepCopyFeedback { copyControl.setAccessibilityLabel("Copy message") }
        copyControl.toolTip = "Copy message"
        copyControl.identifier = NSUserInterfaceItemIdentifier("copy_activity_msg:\(item.signal_id)")
        copyControl.pressed = { [weak copyControl] in if let c = copyControl { onCopy(c) } }
        copyControl.applyVisualState()
        let held = ["stale_generation":"App reloaded","driver_unavailable":"App unavailable",
            "session_switched":"Conversation changed","unlistened":"Listening paused",
            "unlistened_before_dispatch":"Listening paused","fenced_before_dispatch":"Connection changed",
            "sender_ack_timeout":"Awaiting acknowledgment"][item.held_reason ?? ""]
        let senderTitle = distinctAbout(item.from_title,title:"",user:item.from_user)
        line1TitleLabel.stringValue = "@\(item.from_user)" + (senderTitle.map { " (\($0))" } ?? "") + " → " + (item.target_harness == "human" ? "You" : item.target_title)
        line1TitleLabel.font = .systemFont(ofSize:12.5,weight:item.unread ? .semibold : .medium)
        line1TitleLabel.textColor = item.unread ? .labelColor : CitoPalette.secondaryText
        line1TitleLabel.frame = NSRect(x:52,y:rowHeight-28,width:max(0,textW),height:18)
        line1TitleLabel.lineBreakMode = .byTruncatingMiddle
        line1TitleLabel.toolTip = line1TitleLabel.stringValue
        line1TitleLabel.identifier = NSUserInterfaceItemIdentifier("act_title:\(item.signal_id)")
        let time=relativeTime(item.created_at)
        let timeWidth=ceil(measureTextWidth(time,font:.systemFont(ofSize:10.5)))+4
        line1RepoLabel.isHidden=false; line1RepoLabel.stringValue=time
        line1RepoLabel.font = .systemFont(ofSize:10.5); line1RepoLabel.textColor=CitoPalette.secondaryText
        line1RepoLabel.frame=NSRect(x:52+textW-timeWidth,y:rowHeight-28,width:timeWidth,height:18)
        line1RepoLabel.identifier=NSUserInterfaceItemIdentifier("act_time:\(item.signal_id)")
        heldBadge?.removeFromSuperview(); heldBadge=nil
        var heldWidth:CGFloat=0
        if let reason=held {
            let badge=makePillBadge("Held: " + reason,id:"act_held:\(item.signal_id)",height:16,font:.systemFont(ofSize:8.5,weight:.medium),textColor:CitoPalette.warningText,fill:CitoPalette.warningSurface,border:CitoPalette.warningBorder,paddingX:5,minWidth:0)
            heldWidth=min(badge.frame.width,max(0,textW-timeWidth-52))
            badge.frame=NSRect(x:52+textW-timeWidth-heldWidth-6,y:rowHeight-27,width:heldWidth,height:16)
            badge.toolTip="Held: " + reason
            addSubview(badge); heldBadge=badge
        }
        line1TitleLabel.frame.size.width=max(0,textW-timeWidth-heldWidth-12)
        line2UserLabel.isHidden = true
        line2MetaLabel.toolTip = item.message
        line2MetaLabel.stringValue = expanded ? item.message : item.message.replacingOccurrences(of:"\\s*\\n+\\s*",with:"  ",options:.regularExpression)
        line2MetaLabel.maximumNumberOfLines = expanded ? 4 : 1
        line2MetaLabel.lineBreakMode = expanded ? .byWordWrapping : .byTruncatingTail
        line2MetaLabel.font = .systemFont(ofSize:12)
        line2MetaLabel.textColor = item.unread ? .labelColor : CitoPalette.secondaryText
        let messageHeight = expanded ? rowHeight - 42 : 18
        line2MetaLabel.frame = NSRect(x:52,y:rowHeight - 34 - messageHeight,width:CitoMetrics.activityTextWidth,height:messageHeight)
        msgControl.layoutContents(); copyControl.layoutContents()
        line2MetaLabel.identifier = NSUserInterfaceItemIdentifier("act_msg:\(item.signal_id)")
    }
}
