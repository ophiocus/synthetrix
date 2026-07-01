# Changelog

All notable changes to Synthetrix are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
the project adheres to [Semantic Versioning](https://semver.org/). The desktop
app's runtime version is derived from the latest `v*` git tag (`app/build.rs` →
`APP_VERSION`); 4-part tags (`v0.1.0.NNN`) carry an optional build number.

## [Unreleased]

## [0.1.6] - 2026-07-01

### Changed
- **Brand identity: official Synthetrix logo + app icon.** Locked the cyberpunk
  serpent-crest emblem — an interlocking magenta **S** monogram set in a machined
  cyan/magenta bezel with cardinal facets — as the app's logo. Replaced
  `app/assets/icon.ico` (embedded via `winres`) so the Windows build and MSI ship
  the new icon, added `app/assets/logo.png`, and put it at the top of the README.
  Fittingly, the mark was generated on Synthetrix's own local ComfyUI pipeline.

## [0.1.5] - 2026-06-30

### Fixed
- **App wouldn't launch — instant stack-overflow crash (0xC00000FD) on startup.**
  Root cause was a third-party graphics **capture hook**, not our code: OBS's
  Vulkan implicit layer `VK_LAYER_OBS_HOOK`
  (`C:\ProgramData\obs-studio-hook\graphics-hook64.dll`, registered globally so
  the Vulkan loader injects it into every GPU app) overflowed the stack during
  graphics init on the current NVIDIA driver (32.0.15.9579). The OpenGL path was
  hooked too (crash surfaced in `nvoglv64.dll`), so an empty eframe window
  reproduced it. Fixes, all self-contained in the binary (no user action, no
  overlay changes needed):
  - render through **wgpu → Vulkan** instead of OpenGL/glow (glow is hooked; DX12
    surface creation fails on this driver);
  - at startup, opt our process out of the OBS Vulkan layer via its manifest
    `disable_environment` key (`DISABLE_VULKAN_OBS_CAPTURE=1`) and pin
    `WGPU_BACKEND=vulkan` — both only when unset, so a user can still override.

## [0.1.4] - 2026-06-30

### Added
- **Orchestrator overhaul — Synthetrix is now a per-IP production cockpit.** The
  model harvester grew into the authoritative dashboard + digital vault for a game
  IP (MOAR / DISCARDED), switchable IDE-style from a top-bar project switcher. The
  global model vault (`catalog.sqlite`) stays shared; everything IP-scoped lives in
  a per-IP `project.sqlite` at `<lore_root>/.synthetrix/`. Seven additive phases:
  - **P0 Project workspace** — `Project` registry + switcher + per-IP DB + Dashboard.
  - **P1 Service router + Forge core** — `backends/` media bus (Backend trait +
    local-ComfyUI text→image); Forge tab; per-IP asset vault with provenance
    sidecars; jobs/assets in `project.sqlite`.
  - **P2 Multi-modal Asset Manager** — register/browse images/video/audio/meshes
    per IP; vault scan; opinionated engine placement by topic.
  - **P3 Prompt Storage Matrix** — per-entity prompt rows + CRUD editor +
    `prompts.md` import; positive-anchoring lint; feeds the Forge.
  - **P4 Lore subsystem** — indexes the lore-bible repo (title/summary/vocab) into
    `lore_index`; Lore tab with kind filters, search, and a read-only reader.
  - **P5 Composite pipelines** — named build graphs (Character→3D, Prop→3D, Concept
    art, Voice line) over the bus; Tripo (image→GLB) + ElevenLabs (text→MP3)
    backends, config-gated; Forge burst (×N seeds); per-stage run tracking.
  - **P6 Release authority** — freezes (model-layer sha256 snapshot) + ship-cuts
    (freeze + full asset reproducibility trail); manifests exported to
    `<lore_root>/.synthetrix/releases/`.

### Fixed
- **Params→workflow lost the model when A1111 gave only a hash.** Captured params
  almost always name the checkpoint by `Model hash:` (AutoV2), not a filename, so a
  synthesized workflow fell back to a `model.safetensors` placeholder → empty
  checkpoint in ComfyUI. `convert` now captures the hash and emits a
  `ckpt_name: "hash:<autov2>"` sentinel, and `comfy::resolve_model` resolves it via
  the catalog (`autov2` / `sha256[:10]`, 16k+ files carry it) to the real file —
  using it if installed, else hotloading from the vault by its known `local_path`.
  The structural transform was already correct (sampler/scheduler/cfg/seed/size/
  prompts); only the model reference was dropped. Stale synthesized `*.workflow.json`
  caches were cleared so they regenerate with the sentinel.

## [0.1.3] - 2026-06-30

### Changed
- **Self-updater brought to TinyBooth parity.** Periodic background re-check
  (5-min `RECHECK_INTERVAL` + `maybe_spawn_recheck`, woken via
  `request_repaint_after`) so a freshly-published release surfaces mid-session,
  not only at startup; `git_update::render` now returns a `#[must_use]` close
  signal and the app shuts down cleanly through `ViewportCommand::Close` on
  install (was a raw `process::exit`, so Drops/config-save run before the MSI
  swaps the exe); the version-label click always forces a fresh round-trip; added
  `is_newer` unit tests. The CI/MSI/release chain was already at parity (root
  `release.yml` builds + publishes the MSI that the updater pulls).
- "Open workflow in ComfyUI" now **resolves the workflow's real model** instead of
  substituting a compatible one. Order: keep it if installed → match an installed
  file under a near-identical name (camelCase/digit-aware token overlap, e.g.
  `2758_hinaAsianFlux1-krea-dev…` → installed `2758FluxAsianUtopian…`) → **hotload
  the model from the H:\Models vault** and rewrite the reference to the real
  filename. The vault search spans `checkpoints`/`diffusion_models`/`unet` (model
  files are often mis-filed) and hotloads into the subdir the *loader* reads from
  (a `UNETLoader` / "Load Diffusion Model" → `diffusion_models`, even if the file
  sat under `checkpoints`). Only when the model is genuinely absent does it
  leave ComfyUI's honest "missing model" (no silent swap to a wrong checkpoint).
  The bridge calls `refreshComboInNodes` so a just-hotloaded model validates.

## [0.1.2] - 2026-06-30

### Added
- **"Open workflow in ComfyUI"** button in the silverbox status bar — *programmatically*
  loads the image's workflow into the running ComfyUI (no copy/paste). It ensures the
  PNG carries the workflow (embeds a `tEXt` chunk if missing), uploads it via
  `POST /upload/image`, and opens `…/?synflow=…`, which the bundled frontend bridge
  feeds to ComfyUI's own `app.handleFile` — the same path as dragging the image onto
  the canvas. Disabled when the image has no workflow.
- **ComfyUI frontend bridge** (`comfyui/synthetrix_open.js`) — reads `?synflow=`,
  fetches the uploaded image, and calls `handleFile` to drop it onto the canvas.
  Loads even under `--disable-all-custom-nodes`; no server restart.
- `pngmeta::insert_text_chunk` + `has_embedded_workflow` (PNG `tEXt` writer with
  CRC-32, round-trip tested); new `comfy` module (embed → upload → open).

### Fixed
- "Open workflow in ComfyUI" opened the graph with a **missing/empty checkpoint** —
  the workflow referenced a model ComfyUI doesn't have. Now repoints
  `CheckpointLoaderSimple` / `UNETLoader` to an installed model for **both harvested
  and synthesized** graphs (the patched workflow is re-embedded into the image,
  stripping the original chunk so it wins), choosing an **architecture-compatible**
  match (a Flux graph → an installed Flux model, not SDXL). If nothing compatible is
  installed it leaves ComfyUI's honest "missing model" rather than loading an
  incompatible checkpoint.

## [0.1.1] - 2026-06-29

### Added
- **Manifest registry lifecycle** — Audit (vault / NVMe / orphan scan) and Heal
  (reset vanished rows for re-fetch; adopt loose files by filename); Hotload
  (Promote) / Evict / Lock with eviction blocked while a replica is locked.
- **Recover orphans** — identify untracked vault files by SHA256 via CivitAI's
  by-hash endpoint (`civitai::model_version_by_hash` + `model_by_id`) and adopt
  matches into the catalog (`db::adopt_by_hash`).
- **Example-image + embedded-workflow harvest** (`harvest_images` / `harvest_all`)
  — pull per-model gallery images on download and extract ComfyUI `workflow` /
  `prompt` + A1111 `parameters` from PNG text chunks; `db::downloaded_model_ids`.
- **Parallel cover-thumbnail fetch pool** in the Picker; thumbs/likes sort option.
- **Manifest silverbox (full-size image overlay).** Clicking any captured image in
  the Manifest strip opens a resizable full-size overlay.
- **ⓘ info button** on each captured image — replaces the old WF / A1 click badges.
  Opens the silverbox's "Workflow + Params" view (node graph beside the A1111 text)
  and, if exactly one side is present, **synthesizes the missing side and caches it**
  next to the image (`{stem}.workflow.json` / `{stem}.params.txt`).
- **`convert.rs`** — bidirectional ComfyUI-graph ⇄ A1111-params conversion
  (handles UI and API workflow formats; A1111 sampler→Comfy sampler+scheduler map).
- **`ingest_provisioned.py`** — registers provisioner/HuggingFace models into the
  manifest as tracked rows (HF-namespaced negative ids, `status=promoted`, vault
  `local_path` + NVMe `nvme_path`, sha256). Closes the gap where the CivitAI-bound
  recover/heal/audit can't adopt a HF model (e.g. WAN 2.2 via AIProd `provision.py`).

### Fixed
- **`fetch.py` lost the CivitAI token on download.** CivitAI 302-redirects to S3
  and `requests` strips the `Authorization` header across hosts, so token-gated
  files 401 (or save an HTML login page as a `.safetensors`). Send the token as a
  `?token=` query param so it survives the redirect, plus an HTML content-type
  guard that rejects a non-model response.

### Docs
- **`CONSOLIDATION.md`** — proposed "one vault, one runtime" plan: collapse the
  scattered ComfyUI/model folders into VAULT (`H:\Models`, HDD) + RUNTIME (NVMe),
  with the Synthetrix app as the download→vault→promote→evict bridge.

### CI
- **Build/test CI** (`.github/workflows/ci.yml`) — `cargo fmt --check` /
  `clippy -D warnings` / `cargo test` on push + PR to `master` (pinned toolchain
  1.93.1; mirrors the TinyBooth gate pattern). Crate lives in `app/`, so steps run
  there.
- **Relocated the tag-release workflow to the repo root** (`.github/workflows/
  release.yml`). It previously sat under `app/.github/workflows/`, where GitHub
  Actions never reads it — so it had never run. Now verifies tag↔`Cargo.toml`,
  builds, packages the MSI (WiX 3.11), and publishes the GitHub release.

## [0.1.0] - 2026-06-28

### Added
- **Python harvester** — index → pick → fetch pipeline over the CivitAI REST API.
  - `build_index.py`: cursor-paginated curated crawl (top-N per type × base ×
    ranking) into `H:/Models/.civitai/catalog.sqlite` + usage-doc sidecars.
  - `pick.py`: browse/filter the catalog `picks` view.
  - `fetch.py`: SHA256-verified download to the HDD vault, optional NVMe promote,
    auto-harvest of example images + embedded ComfyUI/A1111 workflows on download.
  - `harvest_images.py`: batch image/workflow harvest (downloaded models by default).
  - Crawl types: Checkpoint, LORA, LoCon, TextualInversion.
- **Synthetrix desktop app** (Rust + egui, bootstrapped from rust-skeleton).
  - Three tabs over the shared SQLite manifest: **Fetcher** (sync the list from
    CivitAI Red), **Picker** (metadata + per-model state badges + single/batch
    download/hotload), **Manifest** (downloaded vs locked-active-on-NVMe;
    hotload/lock/evict; audit/heal the registry against disk).
  - Rust-native engine: reqwest crawl + streamed SHA256 download, PNG text-chunk
    workflow extraction, rusqlite manifest (adds `locked` flag + `reflog` log).
  - Manual lock+evict NVMe policy; eviction blocked while a replica is locked.
  - Lazy CDN cover thumbnails in the Picker; live sync dashboard (progress bar,
    running counts, scrolling per-combo log).

### Performance
- Catalog opened in WAL mode with `synchronous=NORMAL`; sync now batches each API
  page in a single transaction — removes the per-row fsync storm that made bulk
  sync crawl on HDD-hosted catalogs (minutes of disk → ~network-bound).

### Notes
- `AIPROD_CORRELATIONS.md` — knowledge artifact cross-referencing the AIProd
  ComfyUI provisioning work (complementary curated-baseline system).

[Unreleased]: https://github.com/ophiocus/synthetrix/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/ophiocus/synthetrix/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/ophiocus/synthetrix/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/ophiocus/synthetrix/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/ophiocus/synthetrix/releases/tag/v0.1.0
