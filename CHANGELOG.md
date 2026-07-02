# Changelog

All notable changes to Synthetrix are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
the project adheres to [Semantic Versioning](https://semver.org/). The desktop
app's runtime version is derived from the latest `v*` git tag (`app/build.rs` →
`APP_VERSION`); 4-part tags (`v0.1.0.NNN`) carry an optional build number.

## [Unreleased]

## [0.1.21] - 2026-07-01

### Fixed
- **The workflow now shows the model's own file name — the one next to the label.**
  An example image belongs to a specific downloaded model, but its workflow carried
  the *author's* filename in the loader. The primary loader is now forced to the
  manifest row's actual file (`file_name`), routed by model type: a Checkpoint sets
  the checkpoint/UNET loader, a LORA sets the LoRA loader (leaving the base
  checkpoint alone), VAE→VAE, etc. Applied to both the in-app graph and the
  open-in-ComfyUI export, so the label, the file on disk, and the loaded variable
  all match. +2 tests.

## [0.1.20] - 2026-07-01

### Fixed
- **Workflow model names are consistent again (the "FF DOUBLE / FuxCapacity" mess).**
  Two defects: (1) `open_in_comfy` only resolved `CheckpointLoader`/`UNETLoader`, so
  GGUF UNET, VAE, and CLIP (single/dual/triple) loaders kept the *original author's*
  local paths (e.g. `GGUFFlux\Z\WIP\FuxCapacity4.0_Q8_0.gguf`) and showed up
  missing/mismatched in ComfyUI; (2) those foreign subfolder paths were never
  normalized, so the graph, the file on disk, and the loaded variable all read
  differently.
  - Model-name resolution now covers the whole Flux/SDXL loader stack — checkpoints,
    diffusion/UNET incl. GGUF, VAE, CLIP (single/dual/triple incl. GGUF), LoRA,
    ControlNet, upscalers — with per-slot hotload targets.
  - Matching is **basename-aware**: an author's `A\B\name.gguf` resolves to the
    installed/vault `name.gguf` (exact-basename first, then fuzzy tokens), and the
    reference is rewritten to ComfyUI's own spelling.
  - The in-app workflow graph now displays the **bare filename** for model refs
    instead of the author's subfolder path, so label / file / loaded value agree.

### Fixed
- **Runtime tab no longer needs (or mis-uses) a manually-typed path.** `comfyctl.py`
  is now auto-detected at `~/synthetrix` (in addition to the cwd/exe walk), so the
  common clone location just works with the box left blank. A wrong path in the box
  (e.g. a lore/game repo) now *falls through* to auto-detect instead of breaking the
  tab.

### Changed
- **Clarified the location UI.** Renamed "Manager location/root" → "comfyctl location
  / Synthetrix repo folder", added a "(blank = auto-detect)" hint, a **Browse** and
  **Clear** button, and a live ✔/✖ line showing exactly which `comfyctl.py` is in
  use (or that none was found). Explains it wants the Synthetrix repo (with
  build_index.py + app/), not a lore/game repo.

### Changed
- **Runtime provision panel simplified to two buttons: Provision and Dry run.**
  Both act on the full `--all` flow (base → accessories, idempotent) — the granular
  per-step buttons (Manager / Node packs / Torch) are gone; the CLI still exposes
  those flags for surgical use.

## [0.1.17] - 2026-07-01

### Added
- **Lifecycle control now covers accessories, not just the base program.**
  `comfyctl provision` gained `--manager` (installs **ComfyUI-Manager**, the in-UI
  node install/update accessory) and `--all` — an orchestrated bring-up that runs
  base → accessories in dependency order (ComfyUI → venv → torch → ComfyUI-Manager
  → custom-node packs → heal paths), each step idempotent. Preflight/doctor gained a
  dedicated **ComfyUI-Manager** check.
- **Runtime tab provision panel.** A new "Provision (base program + accessories)"
  section with **Provision all**, **Dry-run all**, **ComfyUI-Manager**, **Node
  packs**, and **Torch** buttons — post-install setup without dropping to the CLI.

## [0.1.16] - 2026-07-01

### Added
- **Runtime tab — Synthetrix now controls ComfyUI's lifecycle, not just talks to
  it.** A new 🖥 Runtime tab shells out to `comfyctl` (committed alongside) to run
  the blocking preflight checklist, launch/stop the server on its managed venv with
  hardware-tuned flags, and heal `extra_model_paths.yaml` to the vault/NVMe source
  of truth. The doctor/preflight verdict renders as a colored checklist (OK/WARN/
  FAIL/SKIP) with per-check fixes and a blockers summary; a live `:8188` status dot
  shows whether the server is up. Long ops (a cold launch warms up for minutes) run
  off the UI thread with the result drained back into the tab.
- **`comfyctl` runtime manager committed** (`comfyctl.py` + `synthetrix/comfy/`):
  `probe`/`rules` (machine + hardware→runtime compat), `preflight`/`doctor` (ordered
  blocking checklist), `launch`/`stop` (detached-spawn lifecycle), and
  `provision`/`heal` (venv/torch/nodes/paths). The Runtime tab is its GUI front end.

### Config
- New `comfy_manager_root` (dir holding `comfyctl.py`; empty => auto-detect by
  walking up from cwd/exe) and `python_exe` (default `python`) fields, both
  `#[serde(default)]` so older configs load unchanged.

## [0.1.15] - 2026-07-01

### Fixed
- **"Open workflow in ComfyUI" now reports what actually happened.** When ComfyUI
  wasn't running, `open_in_comfy` uploaded first and errored deep in reqwest — the
  browser never opened and the failure went to `eprintln!` (invisible in a windowed
  app), so the button looked dead. Now it preflights `:8188` and fails fast with an
  actionable message, and the async result (success or the real error) is drained
  back into the lightbox note instead of being swallowed. Errors show amber, success
  green.
- **Windows: the open URL was truncated at `&`.** `cmd start "" <url>` split the
  `?synflow=…&synname=…` URL on the `&`, dropping everything after it. The redundant
  `synname` param is gone (the view URL already carries `filename=…`, and the bridge
  reads the workflow from the file content, not the display name), so the opened URL
  is now a single unbroken `?synflow=`.

## [0.1.14] - 2026-07-01

### Added
- **Fetcher search filters.** `build_index.py` now exposes the `/models` filters
  it wasn't using — `--query` (full-text/Meilisearch), `--tag`, `--username`, and
  `--checkpoint-type` — ANDed onto every crawl pass, so coverage is no longer
  locked to the `type × base_model` grid. `iter_models` follows Meilisearch's
  numeric `nextPage` as a fallback when `query` mode returns no cursor.
- **`--delta` catch-up mode.** CivitAI has no updated-since filter, so the delta
  is client-computed: crawl `Newest` and stop each pass after
  `[crawl.delta] stop_after_known` consecutive already-indexed ids. Cheap way to
  pick up new publishes without a full re-crawl.
- **`--refresh` mode.** Re-pulls the full JSON for every model already in the
  catalog via the `/models ?ids=` filter (100/call, new `CivitAIClient.models_by_ids`),
  freshening stats/versions/files/images for known rows.

### Notes
- Harvester/CLI feature only — no change to the desktop binary's behavior. The
  catalog (`catalog.sqlite`) is the local metadata database; there is no bulk
  CivitAI dump to mirror. Version bumped to carry the tagged source release.

## [0.1.13] - 2026-07-01

### Fixed
- **Upgrades no longer wipe the config (token, tiers, projects).** `Config` now
  deserializes with container-level `#[serde(default)]`, so a `config.json` from an
  older build (missing fields newer builds added) loads cleanly instead of failing
  the whole parse and resetting to defaults. The CivitAI token survives updates.
- **Capture images no longer returns 0 for adopted/reconciled models.** Those were
  stored with a stub raw JSON (no `images`), so there was nothing to pull.
  `harvest_images` now refetches the model JSON from CivitAI by id when the stored
  raw carries no images (real CivitAI ids only; HF/provisioned negative-ids have no
  gallery and are skipped). Already-captured images are still skipped (idempotent).

## [0.1.12] - 2026-07-01

### Fixed
- **Manifest: same-model file rows no longer share expand/collapse.** Multi-file
  models (e.g. a WAN asset's 5 files) keyed their expand toggle off the shared
  model_id, so they opened and closed together. Each file row now keys its own
  file_id (captured images still resolve per-model).
- **Installer WiX shortcut components corrected** (HKCU keypath + directory-path
  target) so the MSI actually builds — ICE38/43/57/69 resolved; validated locally
  with a real `cargo wix` build. (0.1.10/0.1.11 failed to publish on these.)

## [0.1.11] - 2026-07-01

### Changed
- **Lore reader now renders markdown instead of showing raw source.** The Lore
  tab's reader pane displayed the `.md` body as plain monospace text; it now
  renders it as formatted markdown (headings, bold/italic, lists, tables, code)
  via `egui_commonmark`. It's a real markdown client, read-only as before.

## [0.1.10] - 2026-07-01

### Fixed
- **Installer creates Start Menu + Desktop shortcuts.** The MSI laid down the
  binary but no shortcut — so after an upgrade removed the old shortcut the app
  had nowhere to launch from and looked "gone." Added a persisted WiX source
  (`app/wix/main.wxs`) with Start Menu and Desktop shortcuts. The UpgradeCode is
  unchanged (`FE0265EA-…`) so existing installs still upgrade in place.

## [0.1.9] - 2026-07-01

### Fixed
- **"Open workflow in ComfyUI" was grayed out for images without a sidecar.** The
  button only enabled when Synthetrix had a `.workflow.json` (or synthesizable
  A1111 params). But PNGs — especially locally-generated ones — carry the workflow
  *embedded in the file*, which ComfyUI reads directly on open. The button now
  enables for any PNG (or when a sidecar exists); with no sidecar it uploads the
  raw PNG and ComfyUI loads the embedded graph.

## [0.1.8] - 2026-07-01

### Fixed
- **Self-update now relaunches the app after installing.** The updater self-closes
  so the MSI can swap the exe, but it never reopened afterward — leaving the user
  at a closed window. `download_and_install` now runs `msiexec` with `-Wait` and
  then `Start-Process`es the upgraded exe, so the app comes back up on the new
  version automatically.

## [0.1.7] - 2026-07-01

### Changed
- **The logo is now the app's window/taskbar icon.** Set the eframe
  `ViewportBuilder` icon (`main.rs` → `load_window_icon`) from the embedded badge,
  replacing eframe's default glyph in the title bar and taskbar. The logo assets
  (`app/assets/logo.png`/`logo_1024.png`/`icon.ico`) are now **transparent**
  (circular cut) so the mark sits cleanly on any background.

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
