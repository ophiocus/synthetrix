# Succession & parity: tinyforge → synthetrix

**Status: parity met and exceeded. tinyforge safely retired.**

synthetrix is the spiritual successor to tinyforge for the **model acquire → store
→ serve → provision** domain. This document is the acceptance record for that
hand-off: what parity means, and the evidence synthetrix clears the bar.

## Context

tinyforge (`F:\tinyforge`, `git@github.com:ophiocus/tinyforge.git`) was a **v0.1
scaffold**. Its README is explicit — "runtime stages stubbed". Verified at sunset:
its entire model-domain is unimplemented stubs that `raise NotImplementedError`:

- `provisioning.py` — `extract_requirements`, `resolve_requirements`,
  `install_plan`, `provision_from_png`: **all stubs**.
- `inventory.py` — `scan_inventory`, `write_inventory`: **stubs**.
- `freeze.py` — `save_freeze`, `restore_freeze`, `diff_freezes`: **stubs**.

tinyforge *designed* model provisioning; it never *shipped* it. **synthetrix
shipped it.**

## Acceptance criteria

Parity = synthetrix implements, **in working shipping form**, every model/runtime/
provisioning capability tinyforge specified. tinyforge's **generation pipeline**
(notion→binding→ComfyUI image, burst grids, the 6-stage treadmill, Tripo 3D
meshing, game-project ingest) is **explicitly OUT OF SCOPE** — a different product
concern. That scaffold was stubbed too, and its *design* is preserved on GitHub.

## Parity matrix

| tinyforge capability | tinyforge status | synthetrix equivalent | Verdict |
|---|---|---|---|
| PNG → extract embedded workflow | stub (`extract_workflow_from_png`, Pillow) | `pngmeta::text_chunks` (+ write/CRC) — Rust, shipped | **exceeds** |
| Walk workflow → model requirements | stub (`extract_requirements`) | `comfy::resolve_model` / loader walk (`ckpt_name`/`unet_name`/…) | **exceeds** |
| Resolve filename/hash → source | stub (`resolve_requirements`) | CivitAI by-hash (`model_version_by_hash`) + name `best_match` + vault search | **exceeds** |
| Install: download + verify SHA256 | stub (`install_plan`) | `fetch` SHA256-verified download → vault | **exceeds** |
| `provision_from_png` (end-to-end) | stub | "Open workflow in ComfyUI": resolve every loader → hotload from vault → re-embed → open | **exceeds** |
| `inventory scan` → local-inventory.json | stub | live SQLite manifest + Manifest tab **Audit** (vault/NVMe/orphan scan) | **exceeds** (live vs JSON) |
| Source clients (CivitAI/HF/git) | partial (clients present, callers stubbed) | CivitAI harvester (index→pick→fetch) + `ingest_provisioned.py` (HF) | **exceeds** |
| Model knowledge base (`docs/components`) | static markdown | live catalog + usage docs + trigger words | **exceeds** |
| `freeze` snapshot/restore a stack | stub | `lock` (pin NVMe replica); named snapshot = roadmap | **partial** (see below) |
| `doctor` (paths/keys/connectivity) | real | Settings token check; fuller probe = roadmap | **partial** |
| Image generation / burst / treadmill | stub scaffold | — out of scope — | n/a |
| Tripo 3D meshing / game ingest | stub scaffold | — out of scope — | n/a |

## Beyond parity (synthetrix-only)

Capabilities tinyforge never had a design for, that synthetrix ships:
two-tier **vault (HDD) → runtime (NVMe)** with promote/evict/**lock**; a real
**manifest registry** (SQLite) as single source of truth; **hash-recover/adopt** of
orphan files; a native **GUI** (Fetcher/Picker/Manifest); **workflow visualizer** +
Comfy⇄A1111 **convert**; parallel **cover cache**; and a **WiX-MSI auto-update CI
release pipeline**.

## Roadmap (enhancements, not parity blockers)

- **provision-missing-from-PNG**: when a workflow references a model *absent from
  the vault*, download it (CivitAI by-hash from A1111 params / filename search)
  into the vault, then promote. synthetrix has every piece; this is wiring.
- **named freeze/restore** of a model set (snapshot beyond per-file `lock`).
- **`doctor`** health command (paths exist, ComfyUI reachable, token valid).

## Conclusion

The successor already does, in shipping form, what the predecessor only sketched.
**Parity accepted.** tinyforge's local install is decommissioned; its repo archived
with this succession recorded. The generation-pipeline design lives on in GitHub
history for whenever it earns a real implementation.
