# Correlations & knowledge transfers — AIProd ⇄ synthetrix

**Written:** 2026-06-28 · **Source thread:** AIProd (`I:\AIProd`), the ComfyUI
provisioning + CivitAI-integration work. This file carries findings from that
thread into synthetrix. It is a knowledge artifact — recommendations for this
thread to apply, not edits already made to your code.

---

## The two systems are complementary, not redundant

| | **synthetrix** (this repo) | **AIProd `ComfyUI/provision/`** |
|---|---|---|
| Job | **Discovery / harvest** — crawl CivitAI's frontier, vault on HDD, promote on demand | **Curated baseline** — provision a known-good working set into one ComfyUI |
| Selection | bulk top-N per (type×base×ranking), browse the index, pick | hand-picked tiered manifest (`--tier core\|standard\|full`) |
| Sources | CivitAI only (Checkpoint/LORA/LoCon/TI) | **HuggingFace + CivitAI** (dual `source` backend) |
| Storage | HDD vault → SHA256 → NVMe promote | flat into `models/…`, idempotent `.provisioned/` markers |
| Strength | breadth, integrity, usage docs, NSFW/Red | base diffusion models, VAEs, encoders, video/audio/3D (the utility layer you skip) |

**They cover each other's gaps.** Your README explicitly says "VAE/Controlnet/
Upscaler are utility… grab those on demand, not in bulk." AIProd's HuggingFace
manifest **is** that on-demand utility layer (FLUX VAE, text encoders, ControlNet
Union, SeedVR2, Wan/LTX video, ACE-Step audio, TripoSR/Hunyuan3D). Conversely,
synthetrix is the LoRA/checkpoint breadth AIProd's hand-picked manifest can't match.

---

## Knowledge transfers (priority-ranked)

### 1. 🔴 CRITICAL — `fetch.py` loses the token on the S3 redirect

`fetch.py:download()` authenticates with the **`Authorization: Bearer` header only**.
CivitAI 302-redirects every download to a different host (S3/CDN), and **`requests`
deletes the `Authorization` header on any cross-host redirect** — verified in
`requests.sessions.SessionRedirectMixin.rebuild_auth` (`should_strip_auth` →
`del headers["Authorization"]`). So for token-gated / NSFW / early-access models the
credential never reaches the hop that needs it; you get a 401, or worse a 200 that
is actually a small HTML login page written to disk as a `.safetensors`.

CivitAI's own download guide and every working bulk-downloader use the **`?token=`
query param** instead — it's in the URL, so it survives the redirect. AIProd's
`provision.py` does exactly this. **Fix:**

```python
# fetch.py — in download(), before requests.get:
url = row["download_url"]
if token:
    sep = "&" if "?" in url else "?"
    url = f"{url}{sep}token={token}"
# keep the Bearer header too (harmless first-hop); the query param is what works.
with requests.get(url, headers=headers, stream=True, timeout=120) as r:
    ...
```

Also add a **content-type guard** so a stripped-auth HTML error never lands as a
model blob:
```python
ct = r.headers.get("Content-Type", "")
if "text/html" in ct:
    raise RuntimeError("got HTML (auth/redirect failure), not a model file")
```
Your SHA256 verify already catches *corruption*, but a gated model has **no hash in
the catalog** for some early-access files, so `_verify` returns `True` on the HTML —
the content-type guard is the backstop.

### 2. 🟠 HIGH — capture `allowCommercialUse`; pick commercial-safe

You already store the full model JSON in `models.raw`, and the `/models` response
carries top-level **`allowCommercialUse`** (array, e.g. `["Image","Rent","Sell"]`),
`allowNoCredit`, `allowDerivatives`. It's just not extracted to a queryable column,
so `pick.py` can't filter on it. For a user who does commercial/agency work this is
the single most important pick-time filter.

> Gotcha AIProd hit: `allowCommercialUse` lives on **`/models`**, NOT on
> `/model-versions/{id}`. You crawl `/models`, so you already have it — a
> version-only fetcher would miss it. Lucky architecture; capitalize on it.

**Transfer:** add `allow_commercial_use TEXT` to `models`, populate from
`m.get("allowCommercialUse")` in `upsert_model`, surface it in the `picks` view, and
add `pick.py --commercial-only` (keep rows whose list contains `Image`/`Sell`).
AIProd's verdict: treat every CivitAI asset as license-unverified until this is read.

### 3. 🟠 HIGH — IP-risk flag for Pony/Illustrious character LoRAs

AIProd deliberately **skipped a popular Disney-Princess SDXL LoRA** despite a
permissive `allowCommercialUse` flag — character/celebrity/brand LoRAs encode IP the
license can't grant. Your crawl set (Pony, Illustrious) is dense with exactly these.
**Transfer:** a tag/name heuristic column (`ip_risk`) flagging known franchises/
celebrity names, surfaced in `pick.py`, so commercial picks can exclude them.

### 4. 🟡 MEDIUM — VRAM-fit awareness for the 16GB target

The workstation was re-certified this cycle: **RTX 5060 Ti 16GB + 128GB RAM**
(Blackwell sm_120) — *not* the 8GB the old docs claimed. AIProd's
`ComfyUI/MODELS.md` is the authoritative 16GB run table. Relevance to harvesting:
- Full **FLUX.1 checkpoints are ~22GB fp16** → need **fp8/GGUF** to run at 16GB.
  You already store `files.fp` and `files.format` (from file `metadata`) — surface
  them in `picks` so you promote the *runnable* variant, not the fp16 you can't load.
- **128GB RAM** makes MoE/CPU-offload nearly free (a 30B MoE with 23% spilled to RAM
  still beats a dense 24B fully in VRAM — measured in the AIProd thread). So "won't
  fit in 16GB VRAM" is rarely a hard no; it's a speed tradeoff.
- **Blackwell sm_120 needs CUDA 12.8+ torch** in ComfyUI — a stale portable build
  fails at model load. (Doesn't affect synthetrix's download path; affects the
  ComfyUI that consumes the promoted blobs.)

### 5. 🟡 MEDIUM — three ComfyUI model roots now exist; reconcile

There are now **three** ComfyUI model locations across the two threads:

| Path | Owner | State |
|------|-------|-------|
| `E:/model loader/ComfyUI/models` | synthetrix `nvme_root` (promote target) | configured |
| `I:\ComfyUI` | AIProd actual install | exists but **empty** (no models/, no launcher) |
| `D:/ComfyUI` | AIProd `provision.py` default `--comfy-root` | default only |

Pick one canonical ComfyUI install and point both tools at it (synthetrix
`nvme_root` + `provision.py --comfy-root`). Otherwise harvested LoRAs and
provisioned base models land in different trees and no single ComfyUI sees both.

### 6. 🟡 MEDIUM — `harvest_images.py` workflows → AIProd `workflows/`

Your `images` table (with `workflow_path`, `params_path`, `has_workflow`) and the
README reference `harvest_images.py` — but **that script isn't in the repo yet**;
the schema is ready, the extractor is the missing piece. When you build it
(workflows live in PNG `tEXt`/`iTXt` chunks; JPEG/WebP re-encodes strip them),
the extracted ComfyUI workflow JSON is directly reusable by AIProd's
`ComfyUI/workflows/` library and the MOAR "Forge". AIProd's `ComfyUI/NODES.md`
lists the custom nodes those harvested workflows will require to run.

### 7. 🟢 LOW — leech `blog.comfy.org`

AIProd planted standing TODOs to watch **https://blog.comfy.org/** for new model/
node drops (FLUX.2 Klein, Z-Image, LTX-2, Wan, Qwen-Image, ACE-Step, Hunyuan3D).
Your `Newest/Month` crawl catches new CivitAI *uploads* but not first-class ComfyUI
*node support* — the blog is the earliest signal for the latter. Complementary watch.

---

## Bidirectional integration opportunity

- **synthetrix → AIProd:** export `pick.py` results as an AIProd manifest fragment
  (`source: civitai`, `version_id`), or let `provision.py` gain a `source: synthetrix`
  backend that promotes from your vault instead of re-downloading. The vault becomes
  AIProd's CivitAI cache.
- **AIProd → synthetrix:** adopt the HuggingFace `source` backend for the utility
  layer (VAEs, encoders, ControlNet) you currently "grab on demand" by hand, and the
  `--tier` concept for a curated "known-good baseline" subset of the harvest.

## Pointers into the AIProd thread (`I:\AIProd`)
- `eval_civitai.md` — CivitAI eval: `.com`/`.red` split, REST API, the `?token=`
  gotcha, per-model licensing, deepfake/reputational note. Verdict: source from `.com`.
- `ComfyUI/provision/` — `provision.py` (dual HF+CivitAI, `?token=` handled),
  `manifest.json` (25-asset tiered catalog).
- `ComfyUI/MODELS.md` — the 16GB capability map.
- `ComfyUI/HOSTING.md` — local-first ≤16GB policy + Blackwell/CUDA-12.8 caveat.
