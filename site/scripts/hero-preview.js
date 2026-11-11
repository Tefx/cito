(() => {
  // 1-click Copy Command Pills
  const copyButtons = document.querySelectorAll("[data-copy-command]");
  for (const btn of copyButtons) {
    btn.addEventListener("click", async () => {
      const command = btn.getAttribute("data-copy-command") || "brew install --cask tefx/tap/cito";
      const badge = btn.querySelector("[data-copy-badge]");
      if (btn.disabled) return;
      btn.disabled = true;
      if (badge) badge.setAttribute("aria-live", "polite");
      try {
        if (!navigator.clipboard?.writeText) throw new Error("Clipboard unavailable");
        await navigator.clipboard.writeText(command);
        if (badge) badge.textContent = "Copied ✓";
      } catch {
        if (badge) badge.textContent = "Copy failed";
      } finally {
        btn.disabled = false;
      }
    });
  }

  // Tabbed workflow step previews (Moments 01-03)
  for (const preview of document.querySelectorAll("[data-surface-preview]")) {
    const tabs = Array.from(preview.querySelectorAll("[data-preview-tab]"));
    const panels = Array.from(preview.querySelectorAll("[data-preview-panel]"));

    function activateStep(tab) {
      const target = tab.getAttribute("data-preview-tab");
      for (const item of tabs) {
        const selected = item === tab;
        item.setAttribute("aria-selected", String(selected));
        item.tabIndex = selected ? 0 : -1;
      }
      for (const panel of panels) {
        panel.hidden = panel.getAttribute("data-preview-panel") !== target;
      }
    }

    for (const [index, tab] of tabs.entries()) {
      tab.tabIndex = tab.getAttribute("aria-selected") === "true" ? 0 : -1;
      tab.addEventListener("click", () => activateStep(tab));
      tab.addEventListener("keydown", (event) => {
        if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
        event.preventDefault();
        const nextIndex =
          event.key === "Home" ? 0 :
          event.key === "End" ? tabs.length - 1 :
          (index + (event.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
        tabs[nextIndex].focus();
        activateStep(tabs[nextIndex]);
      });
    }
  }

  // Interactive 2-Scene Hero Stage Switcher
  const tabs = Array.from(document.querySelectorAll("[data-scene-target]"));
  const panels = document.querySelectorAll("[data-scene-panel]");
  const statusEl = document.querySelector("[data-scene-status]");

  const statusByScene = {
    "agent-to-agent": "Scene 1 · Direct cross-harness wake (Claude Code → Codex)",
    "agent-to-you": "Scene 2 · Menu bar inline reply & automatic Telegram read sync"
  };

  function activateScene(sceneId) {
    for (const tab of tabs) {
      const isMatch = tab.getAttribute("data-scene-target") === sceneId;
      tab.setAttribute("aria-selected", isMatch ? "true" : "false");
      tab.tabIndex = isMatch ? 0 : -1;
    }
    for (const panel of panels) {
      const isMatch = panel.getAttribute("data-scene-panel") === sceneId;
      if (isMatch) {
        panel.removeAttribute("hidden");
      } else {
        panel.setAttribute("hidden", "");
      }
    }
    if (statusEl && statusByScene[sceneId]) {
      statusEl.textContent = statusByScene[sceneId];
    }
  }

  for (const [index, tab] of tabs.entries()) {
    tab.tabIndex = tab.getAttribute("aria-selected") === "true" ? 0 : -1;
    tab.addEventListener("click", () => {
      const target = tab.getAttribute("data-scene-target");
      if (target) activateScene(target);
    });
    tab.addEventListener("keydown", event => {
      if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
      event.preventDefault();
      const nextIndex =
        event.key === "Home" ? 0 :
        event.key === "End" ? tabs.length - 1 :
        (index + (event.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
      tabs[nextIndex].focus();
      activateScene(tabs[nextIndex].getAttribute("data-scene-target"));
    });
  }
})();
