# Synthetrix

CivitAI checkpoint/LoRA harvester — a Rust/egui desktop app over a local SQLite
manifest. Three tabs map to the harvest workflow; a single native binary owns the
network, downloads, hashing, and PNG-workflow extraction (no Python at runtime).

Bootstrapped from rust-skeleton (inherits the MSI installer + self-update).

## Tabs

| Tab | Reads | Does |
|-----|-------|------|
| **Fetcher** | CivitAI Red (network) | Sync available models (Checkpoint/LoRA/LoCon/TI) + one cover image each into the manifest |
| **Picker** | Local manifest | Browse with metadata + per-model state badge (listed / downloaded / active / 🔒locked); single + batch download / download+hotload |
| **Manifest** | Local manifest | Downloaded vs locked-active-on-NVMe; hotload, lock/unlock, evict; **audit/heal** the registry vs disk |
| Settings | — | Storage paths, token, crawl knobs |

## State model

```
LISTED ──download──▶ DOWNLOADED ──hotload──▶ ACTIVE(NVMe) ──lock──▶ 🔒LOCKED
 (Fetcher)            (vault, HDD)            (Manifest)
```

Downloads verify SHA256 against the catalog hash. Hotload = copy HDD→NVMe.
Eviction is blocked while a replica is locked (manual lock+evict policy).
Audit reconciles the manifest against disk (missing vault/NVMe files, orphans);
Heal resets vanished rows so they can be re-fetched (never auto-deletes orphans).

## Manifest

SQLite at `<catalog_dir>/catalog.sqlite` (default `H:/Models/.civitai`), the same
schema the Python harvester writes — the two share the registry. Tables:
models / versions / files (+ `locked`) / images (+ `is_starter`) / `reflog`
(state-transition log). Example images + extracted ComfyUI `workflow.json` /
A1111 `params.txt` land under `<gallery_root>/<model_id>/`.

## Build / run

```sh
cargo run                 # debug
cargo build --release     # release exe
cargo test                # db round-trip test
```

Set the CivitAI token in Settings (or via `$CIVITAI_TOKEN`); a Red-opted-in
account is required for NSFW + creator-gated downloads.

## Architecture notes

- **Worker thread** owns the `rusqlite::Connection` and `reqwest::blocking`
  client; the UI sends `Cmd`s and receives `Event`s over channels, so the DB is
  never shared across threads and the UI never blocks. The worker calls
  `ctx.request_repaint()` after each event.
- **egui borrow dance:** tab render fns read app state via shared borrows and
  buffer actions in `RefCell<Vec<Cmd>>`, drained into worker commands after the
  UI closures close.
- Cover images render via `egui_extras` `file://` loaders (cached).
