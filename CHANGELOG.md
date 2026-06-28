# Changelog

All notable changes to Synthetrix are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
the project adheres to [Semantic Versioning](https://semver.org/). The desktop
app's runtime version is derived from the latest `v*` git tag (`app/build.rs` →
`APP_VERSION`); 4-part tags (`v0.1.0.NNN`) carry an optional build number.

## [Unreleased]

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

[Unreleased]: https://github.com/ophiocus/synthetrix/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/ophiocus/synthetrix/releases/tag/v0.1.0
