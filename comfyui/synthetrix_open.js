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
  const params = new URLSearchParams(window.location.search);
  const ref = params.get("synflow");
  if (!ref) return;

  function getApp() {
    return (
      (window.comfyAPI && window.comfyAPI.app && window.comfyAPI.app.app) ||
      window.app ||
      null
    );
  }

  let done = false;
  async function tryLoad() {
    if (done) return true;
    const app = getApp();
    // wait until the app + canvas are ready, then import once
    if (!app || typeof app.handleFile !== "function" || !app.graph) return false;
    done = true;
    try {
      const res = await fetch(ref);
      if (!res.ok) {
        console.error("[Synthetrix] fetch failed", res.status, ref);
        return true;
      }
      const blob = await res.blob();
      const name = params.get("synname") || "synthetrix.png";
      const file = new File([blob], name, { type: blob.type || "image/png" });
      await app.handleFile(file, "synthetrix");
      console.log("[Synthetrix] loaded workflow from", name);
    } catch (e) {
      console.error("[Synthetrix] open-from-image error", e);
    } finally {
      params.delete("synflow");
      params.delete("synname");
      const qs = params.toString();
      history.replaceState(null, "", location.pathname + (qs ? "?" + qs : ""));
    }
    return true;
  }

  // Poll for app readiness up to ~30s (extensions can load before the canvas).
  let tries = 0;
  const timer = setInterval(async () => {
    tries += 1;
    if ((await tryLoad()) || tries > 100) clearInterval(timer);
  }, 300);
})();
