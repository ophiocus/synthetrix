# Consolidation plan — one vault, one runtime

**Status:** proposed (2026-06-28). No files move until signed off.
**Goal:** collapse the scattered ComfyUI / model folders into exactly **two**:

1. **VAULT (HDD)** — authoritative blob store + catalog. Everything downloads here.
2. **RUNTIME (NVMe)** — one ComfyUI binary. Only in-use models live here (hot replicas).

The synthetrix Rust app (`app/`) is the bridge between them — it already implements
`download → vault → SHA256 → promote(copy) → NVMe`, plus `evict / lock / audit /
heal / recover_orphans`. This is a **reconfigure + consolidate**, not a rebuild.

## Locked decisions
- **VAULT = `H:\Models`** (8TB HDD) — the existing synthetrix vault + `.civitai`
  catalog/gallery. Unchanged.
- **RUNTIME = `F:\tinyforge\ComfyUI`** — the portable that is **already
  Blackwell-ready** (`torch 2.12.0+cu130`, sees the RTX 5060 Ti at sm_120). Its
  models tree is `F:\tinyforge\ComfyUI\ComfyUI\models`.
- **Promote mode = copy** (NOT symlink). The vault is on a slow HDD; the NVMe
  replica must be real bytes so ComfyUI loads a 12GB FLUX from NVMe, not through a
  link from the HDD. (Symlink/junction would only make sense if the vault were NVMe.)

## Current mess → disposition

| Location | Role | Action |
|---|---|---|
| `H:\Models` (+ `.civitai`) | synthetrix vault (authoritative) | **KEEP — the vault** |
| `F:\tinyforge\ComfyUI` | portable, cu130, 8 ckpts + 25 LoRAs | **KEEP — the runtime** |
| `E:\model loader\ComfyUI` | Rust app's current `nvme_root` (dead) | retire after repoint |
| `E:\ComfyUI` | git install + custom nodes | harvest custom_nodes list → retire |
| `G:\binaries\…\ComfyUI_cu121_or_cpu` | old portable, CUDA 12.1 | **delete** (no sm_120 kernels) |
| `I:\comfyui`, `E:\flux_comfyui` | empty / stray | delete |
| `G:\…\ComfyUI_workflows`, `H:\Models\workflow\…` | loose workflow/model bundles | vault the models, fold workflows into gallery |

## Target architecture

```
  VAULT (HDD, cold, everything)              RUNTIME (NVMe, hot, in-use only)
  H:\Models\                                 F:\tinyforge\ComfyUI\   (cu130, Blackwell)
   ├─ checkpoints/ loras/ vae/ …              └─ ComfyUI\models\
   └─ .civitai\ catalog.sqlite + gallery          ├─ checkpoints/ loras/ vae/
            ▲   download + SHA256 verify          ├─ diffusion_models/ text_encoders/   ← HF/FLUX layer
            │                                      └─ …
            │                                  ▲   promote (copy) ▼ evict
            └──────── synthetrix Rust app ─────┘
   vault_root = H:/Models
   nvme_root  = F:/tinyforge/ComfyUI/ComfyUI/models
```

**Division of labor inside the runtime:**
- CivitAI **checkpoints + LoRAs** → arrive via vault → promote (Rust app).
- HF **FLUX bases, VAEs, text-encoders, ControlNet, video/audio/3D** → land straight
  in the runtime via AIProd's `provision.py --comfy-root F:/tinyforge/ComfyUI/ComfyUI`.
  (These are the always-needed utility layer CivitAI doesn't carry; no point cold-storing them.)
  Subdir maps don't collide: Rust app uses `checkpoints/`,`loras/`; provisioner uses
  `diffusion_models/`,`text_encoders/`,`vae/`.

## Migration workflow (execution order, each step verifiable)

**Step 0 — back up the catalog.** Copy `H:\Models\.civitai\catalog.sqlite` aside.

**Step 1 — absorb scattered models into the vault (non-destructive).**
Use the Rust app's **`recover_orphans`** over every loose model tree
(`F:\tinyforge\…\models`, `E:\ComfyUI\models`, `G:\…`, `H:\Models\workflow\…`). It
SHA256s each file, matches CivitAI's by-hash endpoint, imports the model into the
catalog, and adopts the file. Result: every loose blob is now *known* and tracked.
Files it can't match (`not on CivitAI`) are listed for manual disposition.

**Step 2 — physically gather blobs into `H:\Models\<subdir>\`.**
Move (not copy) the adopted blobs from the scattered trees into the vault subdirs.
Run the app's **`audit`** → expect `0 missing-vault`. Anything `missing` = a move that
didn't land; re-run.

**Step 3 — repoint the Rust app.** Edit `%APPDATA%\Synthetrix\config.json`:
```json
"vault_root": "H:/Models",
"nvme_root":  "F:/tinyforge/ComfyUI/ComfyUI/models"
```
(Or change the `nvme_root` default in `app/src/config.rs` and rebuild.)

**Step 4 — promote the working set.** In the app, `promote` the checkpoints/LoRAs you
actually use → they copy to the F: runtime. `lock` the ones you never want evicted.

**Step 5 — fold in the HF/FLUX layer.**
`python provision.py --tier standard --comfy-root F:/tinyforge/ComfyUI/ComfyUI`
(or the FLUX-only subset). Now the one runtime has both CivitAI and HF assets.

**Step 6 — retire the dead copies.** After Steps 2 + audit confirm the vault holds
everything: delete `G:\…\ComfyUI_cu121*`, `I:\comfyui`, `E:\flux_comfyui`,
`E:\model loader\ComfyUI`, and `E:\ComfyUI` (save its `custom_nodes` list first — those
node packs may need reinstalling into the F: runtime via ComfyUI-Manager).

## Bug to fix in the same pass — the `?token=` download

`app/src/civitai.rs::download_file` authenticates **Bearer-header only**
(`self.auth(...)` → `bearer_auth`). **reqwest strips the `Authorization` header on
cross-host redirects** — and CivitAI 302s every download to its S3/CDN host. So
gated / NSFW / early-access files lose the token mid-redirect → 401, or a small HTML
login page streamed to disk as a `.safetensors` (and `download_file` would then return
its SHA256 as if valid). Fix — put the token in the URL, which survives the redirect:

```rust
// in download_file, before the request loop:
let url = match &self.token {
    Some(t) => {
        let sep = if url.contains('?') { '&' } else { '?' };
        format!("{url}{sep}token={t}")
    }
    None => url.to_string(),
};
// keep bearer_auth too (harmless on the first, same-host hop)
```
Plus a content-type guard so an HTML error never lands as a model blob:
```rust
let ct = r.headers().get(reqwest::header::CONTENT_TYPE)
    .and_then(|h| h.to_str().ok()).unwrap_or("");
if ct.starts_with("text/html") {
    return Err("got HTML (auth/redirect failure), not a model file".into());
}
```
Same fix applies to the Python prototype `fetch.py`. See `AIPROD_CORRELATIONS.md` §1.

## Custom nodes (don't lose them)
`E:\ComfyUI` carries: ComfyUI-Manager, comfyui_controlnet_aux, ComfyUI_essentials,
ComfyUI_IPAdapter_plus, ComfyUI-Advanced-ControlNet, ComfyUI-Impact-Pack,
ComfyUI-KJNodes, ComfyUI_Comfyroll_CustomNodes, efficiency-nodes-comfyui. Reinstall
these into the F: runtime via ComfyUI-Manager before retiring `E:\ComfyUI`.

## Rollback
The vault is only ever *added to* until Step 6. If anything looks wrong before Step 6,
revert `config.json` and nothing is lost — the scattered copies still exist. Step 6
(deletes) is the only irreversible step and runs last, after audit is clean.

## Open items
- `extra_model_paths.yaml` (optional): point the F: runtime read-only at `H:\Models`
  so ComfyUI can *see* the whole cold vault (slow-load) in addition to hot replicas —
  handy for one-off use of an un-promoted model without a full promote.
- Drive headroom: F: also hosts the WSL vhdx (~477GB on a 2TB NVMe); watch free space
  as hot replicas accumulate. If it tightens, the F: portable is relocatable to I:.
