(() => {
  // Copy the visible code text, preserving whitespace and decoded HTML entities.
  const copyButtons = document.querySelectorAll("[data-copy-command], [data-copy-code]");
  for (const btn of copyButtons) {
    let resetTimer;
    btn.addEventListener("click", async () => {
      const command = btn.hasAttribute("data-copy-code")
        ? btn.closest(".code-block").querySelector("pre code").textContent
        : btn.getAttribute("data-copy-command") || "brew install --cask tefx/tap/cito";
      const badge = btn.querySelector("[data-copy-badge]");
      if (btn.disabled) return;
      clearTimeout(resetTimer);
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
        resetTimer = setTimeout(() => {
          if (badge) badge.textContent = "Copy";
        }, 2000);
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

})();
