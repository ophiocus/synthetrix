# ComfyUI bridge extension

`synthetrix_open.js` is the frontend bridge that lets the Synthetrix Manifest
silverbox's **"Open workflow in ComfyUI"** button load a workflow into the
*running* ComfyUI.

## How the button works

1. Synthetrix ensures the displayed image is a PNG carrying the workflow (it
   embeds the workflow as a `tEXt` chunk if the image doesn't already have one).
2. It uploads the PNG to ComfyUI via `POST /upload/image`.
3. It opens the browser at `http://127.0.0.1:8188/?synflow=<view-url>&synname=<file>`.
4. **This extension** reads `?synflow=`, fetches the uploaded image, and calls
   ComfyUI's own `app.handleFile(file, "synthetrix")` — the exact import path used
   when you drag an image onto the canvas — so the graph loads automatically.

ComfyUI has no server API / launch arg to open a workflow, which is why this
small client-side bridge is needed.

## Install

Copy this file into the ComfyUI frontend's extensions directory:

```
<comfyui>/.../comfyui_frontend_package/static/extensions/synthetrix/open.js
```

(On the reference workstation that is
`E:\ComfyUI\venv\Lib\site-packages\comfyui_frontend_package\static\extensions\synthetrix\open.js`.)

It loads automatically on the next page load — no custom node (works even with
`--disable-all-custom-nodes`) and no server restart. Re-copy it after a
`comfyui-frontend-package` upgrade, which replaces the static dir.
