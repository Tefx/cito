import AppKit

extension Companion {
    var resolvedSettingsApps: [AppSetupItem] {
        settings?.apps ?? harnesses.map { h in
            AppSetupItem(id: h.id, display_name: h.display_name, tier: h.tier,
                configured: false, status: "not_configured", detail: "Click Set Up to connect", paths: [])
        }
    }

    func buildAppsTab(in scroll: NSScrollView) {
        let cardWidth = scroll.contentSize.width - 32
        let allowance = settingsScrollerAllowance(scroll)
        let appsView = NSView(frame: NSRect(x: 0, y: 0, width: scroll.contentSize.width, height: 368))
        appsView.identifier = NSUserInterfaceItemIdentifier(SettingsTab.apps.scrollId)
        appsView.isHidden = settingsActiveTab != "apps"

        appsView.addSubview(textLabel("AGENT APPS", NSRect(x:16,y:338,width:240,height:14),size:10,weight:.semibold,color:CitoPalette.secondaryText))
        let map = harnessMap
        let intro = textLabel("Connect your coding apps, then choose sessions to listen.",NSRect(x:16,y:316,width:cardWidth,height:16),size:11,color:CitoPalette.secondaryText)
        intro.autoresizingMask = [.width]
        appsView.addSubview(intro)
        var appTop: CGFloat = 304
        for app in resolvedSettingsApps {
            let cardHeight: CGFloat = 70
            appTop -= cardHeight
            let card = roundedView(NSRect(x:16,y:appTop,width:cardWidth,height:cardHeight),radius:10,
                color:.labelColor.withAppearanceAlpha(0.045),border:.labelColor.withAppearanceAlpha(0.08))
            card.autoresizingMask = [.width]
            card.identifier = NSUserInterfaceItemIdentifier("settings_app:\(app.id)")
            let barY: CGFloat = 36
            card.addSubview(makeHarnessBadge(NSRect(x:12,y:barY-2,width:28,height:28),radius:7,iconInset:6,harness:app.id,harnesses:map))
            let titleW = min(app.isConfigured ? 184 : 180, measureTextWidth(app.display_name,font:.systemFont(ofSize:13,weight:.semibold)) + 2)
            let title = textLabel(app.display_name,NSRect(x:50,y:barY+3.5,width:titleW,height:17),size:13,weight:.semibold)
            title.toolTip = app.display_name; title.lineBreakMode = .byTruncatingMiddle
            card.addSubview(title)
            let tier: NSView?
            if app.tier == "experimental" {
                let badge = makePillBadge("Experimental", id: "settings_app_tier:\(app.id)", textColor: CitoPalette.warningText, fill: CitoPalette.warningSurface, border: CitoPalette.warningBorder, minWidth:0)
                card.addSubview(badge); tier = badge
            } else { tier = nil }
            let statusText = app.isConfigured ? "Connected" : (app.status == "error" ? "Inspection failed" : (app.status == "repair_needed" ? "Repair Needed" : "Not Set Up"))
            let statusColor = app.status == "error" ? CitoPalette.dangerText : app.isConfigured ? CitoPalette.successText : CitoPalette.warningText
            let dot=roundedView(NSRect(x:50,y:16,width:6,height:6),radius:3,color:statusColor)
            dot.identifier=NSUserInterfaceItemIdentifier("settings_app_status_dot:\(app.id)"); card.addSubview(dot)
            let detailText = app.detail.replacingOccurrences(of:"^(Connected|Disconnected) · ",with:"",options:.regularExpression)
            let status=textLabel(statusText + (detailText.isEmpty || detailText == statusText ? "" : " · " + detailText),NSRect(x:62,y:10,width:cardWidth-74,height:18),size:11,color:statusColor)
            status.autoresizingMask = [.width]
            status.identifier=NSUserInterfaceItemIdentifier("settings_app_status:\(app.id)"); status.lineBreakMode = .byTruncatingMiddle
            status.toolTip=([detailText]+app.paths).filter { !$0.isEmpty }.joined(separator:"\n"); card.addSubview(status)
            let appActionWidth = max(76, MenuControl.intrinsicWidth("Disconnect"))
            let actionTitle=app.isConfigured || app.status == "error" ? "Check & Repair" : (app.status == "repair_needed" ? "Repair" : "Set Up")
            let btn = MenuControl(actionTitle,frame:NSRect(x:0,y:0,width:MenuControl.intrinsicWidth(actionTitle),height:24))
            btn.identifier = NSUserInterfaceItemIdentifier("setup_app:\(app.id)")
            btn.style = app.isConfigured ? .neutral : .prominent
            if let until = settingsApps.checkedUntil[app.id], Date() < until { btn.copied("✓ Checked", for: max(0.01, until.timeIntervalSinceNow)) }
            let appPending = isPending("setup_apply",target:app.id) || isPending("setup_remove_app",target:app.id)
            btn.isEnabled = !appPending
            pendingLabel(btn,isPending("setup_apply",target:app.id),actionTitle == "Check & Repair" ? "Checking…" : "Setting Up…")
            btn.pressed = { [weak self] in
                self?.emit(["op":"setup_apply","harness":app.id], appCheck: app.isConfigured)
            }
            card.addSubview(btn)
            var trailing: [NSView] = [btn]
            if app.isConfigured {
                let remove = MenuControl("Disconnect",frame:NSRect(x:0,y:0,width:appActionWidth,height:24))
                remove.identifier = NSUserInterfaceItemIdentifier("remove_app:\(app.id)"); remove.style = .destructiveHover
                remove.isEnabled = !appPending
                pendingLabel(remove,isPending("setup_remove_app",target:app.id),"Disconnecting…")
                remove.pressed = { [weak self] in self?.emit(["op":"setup_remove_app","harness":app.id]) }
                card.addSubview(remove); trailing.append(remove)
            }
            let barSpacing: CGFloat = 5
            layoutBar(in:card,insetLeft:50,y:barY,height:24,spacing:barSpacing,trailing:trailing)
            let right = (trailing.first?.frame.minX ?? cardWidth - 12) - allowance - barSpacing
            let tierWidth = tier.map { $0.frame.width + barSpacing } ?? 0
            title.frame.size.width = max(0,min(titleW,right - title.frame.minX - tierWidth))
            if let tier { tier.frame.origin = NSPoint(x: title.frame.maxX + barSpacing, y: barY + (24 - tier.frame.height) / 2) }
            appsView.addSubview(card); appTop -= 8
        }
        mountSettingsCards(appsView, on: scroll)

    }

    func presentUninstallAlert() {
        guard let window = settingsPanel, activeUninstallAlert == nil else { return }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "Disconnect Agent Apps?"
        alert.informativeText = "This disconnects all Agent Apps from Cito. Local data and Hub settings are kept unless you choose to delete them."
        let cancel = alert.addButton(withTitle: "Cancel")
        cancel.identifier = NSUserInterfaceItemIdentifier("cancel_uninstall_all")
        cancel.keyEquivalent = "\u{1b}"
        let confirm = alert.addButton(withTitle: "Disconnect Apps")
        confirm.identifier = NSUserInterfaceItemIdentifier("confirm_disconnect_all")
        confirm.keyEquivalent = ""
        confirm.hasDestructiveAction = true
        confirm.toolTip = "Disconnect all Agent Apps and keep Cito running"
        alert.showsSuppressionButton = true
        if let purge = alert.suppressionButton {
            purge.identifier = NSUserInterfaceItemIdentifier("uninstall_purge_data")
            purge.title = "Also delete local data and Hub settings"
            purge.state = .off
            purge.target = self
            purge.action = #selector(updateUninstallPurge(_:))
        }
        guard window.makeFirstResponder(nil) else { return }
        activeUninstallAlert = alert
        if headless {
            alert.window.appearance = testAppearance
            alert.layout()
            alert.window.contentView?.identifier = NSUserInterfaceItemIdentifier("uninstall_alert_sheet")
            for button in alert.buttons {
                button.target = self
                button.action = #selector(pressOffscreenAlertButton(_:))
            }
        } else {
            alert.beginSheetModal(for: window) { [weak self, weak alert] response in
                if let alert { self?.finishUninstallAlert(alert, confirmed: response == .alertSecondButtonReturn) }
            }
        }
    }

    @objc private func updateUninstallPurge(_ sender: NSButton) {
        guard let alert = activeUninstallAlert, sender === alert.suppressionButton else { return }
        let purge = sender.state == .on
        let confirm = alert.buttons[1]
        confirm.title = purge ? "Disconnect & Delete Data" : "Disconnect Apps"
        confirm.toolTip = purge ? "Disconnect all Agent Apps and delete local data and Hub settings" : "Disconnect all Agent Apps and keep Cito running"
        alert.layout()
    }

    @objc private func pressOffscreenAlertButton(_ sender: NSButton) {
        guard let alert = activeUninstallAlert else { return }
        finishUninstallAlert(alert, confirmed: sender === alert.buttons[1])
    }

    func finishUninstallAlert(_ alert: NSAlert, confirmed: Bool) {
        guard activeUninstallAlert === alert else { return }
        activeUninstallAlert = nil
        if confirmed {
            emit(["op": "setup_disconnect_all", "purge_data": alert.suppressionButton?.state == .on])
        }
    }

}
