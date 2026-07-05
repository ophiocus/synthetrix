// Synthetrix → ComfyUI bridge  (source of truth — deploy a copy into ComfyUI).
//
// INSTALL: copy this file to your ComfyUI frontend extensions dir, e.g.
//   <comfyui>/.../comfyui_frontend_package/static/extensions/synthetrix/open.js
// It loads automatically (no custom node, so it works even under
// --disable-all-custom-nodes) and needs no server restart.
//
// When Synthetrix opens ComfyUI with `?synflow=<view-url>&synname=<file>`, this
// fetches that uploaded image and feeds it to ComfyUI's own drag-drop importer
// (`app.handleFile`), which reads the embedded workflow/prompt and loads the
// graph onto the canvas — i.e. it programmatically "drops the image" into the
// running ComfyUI. Then it strips the params so a refresh doesn't re-load.

(function () {
  "use strict";

  function getApp() {
    return (
      (window.comfyAPI && window.comfyAPI.app && window.comfyAPI.app.app) ||
      window.app ||
      null
    );
  }

  let loading = false;

  async function loadFrom(app, ref, name) {
    loading = true;
    try {
      const res = await fetch(ref, { cache: "no-store" });
      if (!res.ok) {
        console.error("[Synthetrix] fetch failed", res.status, ref);
      } else {
        const blob = await res.blob();
        const file = new File([blob], name, { type: blob.type || "image/png" });
        // pick up any model Synthetrix just hotloaded, so validation resolves it
        try {
          if (app.refreshComboInNodes) await app.refreshComboInNodes();
        } catch (e) {}
        await app.handleFile(file, "synthetrix");
        console.log("[Synthetrix] loaded workflow from", name);
      }
    } catch (e) {
      console.error("[Synthetrix] open-from-image error", e);
    } finally {
      // Strip the params so a manual refresh won't reload — and so the NEXT open
      // (a fresh ?synflow=) is seen as new rather than deduped away.
      const p = new URLSearchParams(location.search);
      p.delete("synflow");
      p.delete("synname");
      const qs = p.toString();
      history.replaceState(null, "", location.pathname + (qs ? "?" + qs : ""));
      loading = false;
    }
  }

  // Watch continuously for a ?synflow= param. Synthetrix re-opens the ALREADY-open
  // ComfyUI tab with a new ?synflow= each time; the SPA soft-navigates without a
  // full page reload, so a load-ONCE handler keeps showing the last graph (the
  // "every open shows the previous graph" bug). Polling + stripping the param
  // after each load picks up every open, including re-opening the same image.
  setInterval(async () => {
    if (loading) return;
    const params = new URLSearchParams(location.search);
    const ref = params.get("synflow");
    if (!ref) return;
    const app = getApp();
    if (!app || typeof app.handleFile !== "function" || !app.graph) return; // not ready yet
    const name = params.get("synname") || "synthetrix.png";
    await loadFrom(app, ref, name);
  }, 300);
})();
