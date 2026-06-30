# Changelog

All notable changes to Synthetrix are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
the project adheres to [Semantic Versioning](https://semver.org/). The desktop
app's runtime version is derived from the latest `v*` git tag (`app/build.rs` →
`APP_VERSION`); 4-part tags (`v0.1.0.NNN`) carry an optional build number.

## [Unreleased]

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

[Unreleased]: https://github.com/ophiocus/synthetrix/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/ophiocus/synthetrix/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/ophiocus/synthetrix/releases/tag/v0.1.0
