(() => {
  // 1-click Copy Command Pills
  const copyButtons = document.querySelectorAll("[data-copy-command]");
  for (const btn of copyButtons) {
    btn.addEventListener("click", async () => {
      const command = btn.getAttribute("data-copy-command") || "brew install tefx/tap/cito";
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

  // Tabbed previews reflect the native panel labels; they never change local Cito state.
  for (const preview of document.querySelectorAll("[data-surface-preview]")) {
    const tabs = preview.querySelectorAll("[data-preview-tab]");
    const panels = preview.querySelectorAll("[data-preview-panel]");
    for (const tab of tabs) {
      tab.addEventListener("click", () => {
        const target = tab.getAttribute("data-preview-tab");
        for (const item of tabs) item.setAttribute("aria-selected", String(item === tab));
        for (const panel of panels) panel.hidden = panel.getAttribute("data-preview-panel") !== target;
      });
    }
  }

  // Interactive 2-Scene Hero Stage Switcher
  const tabs = document.querySelectorAll("[data-scene-target]");
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

  for (const tab of tabs) {
    tab.addEventListener("click", () => {
      const target = tab.getAttribute("data-scene-target");
      if (target) activateScene(target);
    });
  }
})();
