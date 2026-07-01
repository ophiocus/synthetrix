#!/usr/bin/env python3
"""Generalized vault ingester: register non-Red CivitAI / HuggingFace / local
model files (already sitting in the vault) into the Synthetrix catalog.

Difference from ingest_provisioned.py:
  - registers ONE model row PER FILE (not one blob per subdir)
  - walks the vault tree RECURSIVELY (handles nested named subfolders like
    checkpoints/sdxl-1.0/x.safetensors)
  - DEDUP-AWARE: skips any file already present in the catalog (by basename,
    or by sha256 when --hash), so it never splits the identity of a model the
    CivitAI harvest already indexed
  - provenance-tagged via models.tags + raw json (huggingface | civitai | local)

DRY-RUN by default. Pass --commit to actually write.

Origin: prototyped in the lore-bible session (2026-06-30) to absorb the sunset
tinyforge models; adopted here as the tracked "ingest non-Red CivitAI /
HuggingFace / local" tool.
"""
import argparse, hashlib, json, sqlite3, os
from pathlib import Path

MODEL_EXTS = {".safetensors", ".ckpt", ".pt", ".pth", ".bin", ".gguf", ".sft"}

# top-level vault subdir -> catalog model "type"
TYPE_MAP = {
    "checkpoints": "Checkpoint",
    "loras": "LORA",
    "controlnet": "Controlnet",
    "clip_vision": "ClipVision",
    "clip": "CLIP",
    "text_encoders": "TextEncoder",
    "ipadapter": "IPAdapter",
    "vae": "VAE",
    "vae_approx": "VAE",
    "embeddings": "TextualInversion",
    "upscale_models": "Upscaler",
    "diffusion_models": "Checkpoint",
    "unet": "Checkpoint",
    "background_removal": "Other",
    "style_models": "Other",
}

# basename / path hints that are clearly HuggingFace / official / comfy-builtin
HF_HINTS = ("sd_xl_base", "sd_xl_refiner", "v1-5-pruned", "flux1-schnell",
            "clip-vit", "diffusion_pytorch_model", "ip-adapter", "birefnet",
            "hunyuan_3d", "taesd", "taef1", "umt5", "wan2")


def classify_source(rel: str) -> str:
    low = rel.lower()
    if low.startswith("vae_approx/") or "taesd" in low or "taef1" in low:
        return "comfy-builtin"
    if any(h in low for h in HF_HINTS):
        return "huggingface"
    if low.startswith("loras/") or low.startswith("embeddings/"):
        return "civitai"
    return "local"


def neg_id(seed: str) -> int:
    h = int(hashlib.sha1(seed.encode()).hexdigest()[:12], 16)
    return -(h % 1_000_000_000 + 1)


def sha256_file(p: Path) -> str:
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--vault", default=r"H:\Models")
    ap.add_argument("--nvme", default=r"E:\model loader\ComfyUI\models")
    ap.add_argument("--catalog", default=r"H:\Models\.civitai\catalog.sqlite")
    ap.add_argument("--scan-root", default=r"H:\Models",
                    help="walk this tree (relative paths resolved against --vault)")
    ap.add_argument("--only-manifest", default="",
                    help="JSON list of {rel} to restrict to (e.g. catalog_missing.json)")
    ap.add_argument("--hash", action="store_true", help="compute sha256 (slow, integrity)")
    ap.add_argument("--commit", action="store_true", help="actually write (default dry-run)")
    args = ap.parse_args()

    vault = Path(args.vault)
    nvme = Path(args.nvme)
    con = sqlite3.connect(
        f"file:{args.catalog}?mode=ro" if not args.commit else args.catalog,
        uri=not args.commit,
    )
    con.row_factory = sqlite3.Row

    # build dedup index: basename(lower) + sha set
    have_name, have_sha = set(), set()
    for r in con.execute("SELECT name, local_path, sha256 FROM files"):
        if r["name"]:
            have_name.add(r["name"].lower())
        if r["local_path"]:
            have_name.add(os.path.basename(r["local_path"]).lower())
        if r["sha256"]:
            have_sha.add(r["sha256"].lower())

    # candidate file list
    rels = []
    if args.only_manifest:
        for it in json.load(open(args.only_manifest)):
            rels.append(it["rel"])
    else:
        for dp, _, fs in os.walk(args.scan_root):
            if os.sep + ".cache" in dp or os.sep + ".civitai" in dp:
                continue
            for f in fs:
                if os.path.splitext(f)[1].lower() in MODEL_EXTS:
                    rels.append(os.path.relpath(os.path.join(dp, f), vault).replace("\\", "/"))

    plan, skipped = [], 0
    for rel in sorted(set(rels)):
        name = os.path.basename(rel)
        top = rel.split("/")[0]
        vp = vault / rel.replace("/", os.sep)
        if not vp.exists():
            continue
        sha = sha256_file(vp) if args.hash else ""
        if name.lower() in have_name or (sha and sha.lower() in have_sha):
            skipped += 1
            continue
        npth = nvme / rel.replace("/", os.sep)
        status = "promoted" if npth.exists() else "downloaded"
        plan.append({
            "rel": rel, "name": name, "type": TYPE_MAP.get(top, "Other"),
            "source": classify_source(rel), "status": status,
            "gb": round(vp.stat().st_size / 1024 / 1024 / 1024, 2),
            "sha": sha, "vault": str(vp),
            "nvme": str(npth) if npth.exists() else None,
        })

    # report grouped by source
    from collections import Counter, defaultdict
    bysrc = defaultdict(list)
    for p in plan:
        bysrc[p["source"]].append(p)
    print(f"{'COMMIT' if args.commit else 'DRY-RUN'} | candidates={len(rels)} "
          f"already-in-catalog(skipped)={skipped} to-register={len(plan)}")
    print("=" * 72)
    for src in sorted(bysrc):
        items = bysrc[src]
        tot = sum(i["gb"] for i in items)
        print(f"\n### source={src}  ({len(items)} files, {tot:.2f} GB)")
        for i in sorted(items, key=lambda x: -x["gb"]):
            print(f"  [{i['status']:10}] {i['type']:13} {i['gb']:6.2f}GB  {i['rel']}")
    print("\n" + "=" * 72)
    print("type histogram:", dict(Counter(p["type"] for p in plan)))
    print("source histogram:", dict(Counter(p["source"] for p in plan)))

    if not args.commit:
        print("\n(dry-run; no rows written. add --commit to write, --hash to verify sha256)")
        return

    # ---- WRITE PATH (only with --commit) ----
    con.execute("PRAGMA busy_timeout=30000;")  # wait if the harvester holds the write lock
    pr = dn = 0
    for p in plan:
        mid = neg_id(p["source"] + ":" + p["rel"])
        vid = mid - 1
        raw = json.dumps({"id": mid, "name": p["name"], "source": p["source"],
                          "ingested_local": True, "vault_rel": p["rel"]})
        con.execute(
            """INSERT INTO models(model_id,name,type,nsfw,creator,tags,downloads,
              rating,thumbs_up,comments,cover_url,raw) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)
              ON CONFLICT(model_id) DO UPDATE SET name=excluded.name, raw=excluded.raw""",
            (mid, p["name"], p["type"], 0, p["source"],
             json.dumps([p["source"], "ingested-local"]), 0, 0.0, 0, 0, None, raw))
        con.execute(
            """INSERT INTO versions(version_id,model_id,name,base_model,published_at,
              trained_words,description,downloads,version_idx) VALUES(?,?,?,?,?,?,?,?,?)
              ON CONFLICT(version_id) DO UPDATE SET description=excluded.description""",
            (vid, mid, p["name"], None, None, None,
             f"Ingested from vault ({p['source']})", 0, 0))
        fid = neg_id(f"{p['source']}:file:{p['rel']}")
        fmt = "GGUF" if p["name"].lower().endswith(".gguf") else "SafeTensor"
        con.execute(
            """INSERT INTO files(file_id,version_id,name,type,size_kb,download_url,
              sha256,autov2,fp,format,is_primary,local_path,nvme_path,locked,status)
              VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
              ON CONFLICT(file_id) DO UPDATE SET size_kb=excluded.size_kb,
              sha256=excluded.sha256, local_path=excluded.local_path,
              nvme_path=excluded.nvme_path, status=excluded.status""",
            (fid, vid, p["name"], "Model", p["gb"] * 1024 * 1024, "", p["sha"] or None,
             (p["sha"][:10] if p["sha"] else None), None, fmt, 1,
             p["vault"], p["nvme"], 0, p["status"]))
        con.execute("INSERT INTO reflog(file_id,model_id,action,detail) VALUES(?,?,?,?)",
                    (fid, mid, "ingest-local", f"{p['source']}:{p['rel']} -> {p['status']}"))
        pr += p["status"] == "promoted"
        dn += p["status"] == "downloaded"
    con.commit()
    con.close()
    print(f"\nWROTE {len(plan)} models: {pr} promoted, {dn} downloaded.")


if __name__ == "__main__":
    main()
