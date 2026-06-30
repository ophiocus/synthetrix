#!/usr/bin/env python3
"""Register provisioner / HuggingFace models into the Synthetrix manifest.

Synthetrix's recovery (recover-orphans / heal) is CivitAI-bound: it identifies
files by SHA256 via CivitAI's by-hash endpoint, or matches an orphan filename to
a row already in the CivitAI catalog. A model placed by AIProd `provision.py`
(HuggingFace) matches neither, so it stays invisible to the Manifest tab and to
lock / evict / audit.

This bridge fills that gap: it scans the vault + NVMe tiers for a provisioned
asset's files and inserts tracked `models / versions / files` rows using
**HF-namespaced negative ids** (so they never collide with CivitAI integer ids),
with `local_path` = vault, `nvme_path` = NVMe replica, and `status` = promoted
when the NVMe replica exists. Re-runnable (upsert).

Usage:
  python ingest_provisioned.py --asset "Wan2.2 TI2V-5B" --subdirs diffusion_models,vae,text_encoders
  python ingest_provisioned.py --asset "Wan2.2 TI2V-5B" --subdirs ... --no-hash   # skip sha256 (fast)
"""
import argparse, hashlib, json, sqlite3, tomllib, sys
from pathlib import Path

MODEL_EXTS = {".safetensors", ".ckpt", ".pt", ".pth", ".bin", ".gguf", ".sft"}


def load_cfg():
    cfg_path = Path(__file__).with_name("config.toml")
    with open(cfg_path, "rb") as f:
        return tomllib.load(f)


def neg_id(seed: str) -> int:
    """Deterministic negative id from a string (HF namespace, no CivitAI clash)."""
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
    ap.add_argument("--asset", required=True, help="display name to group the files under")
    ap.add_argument("--subdirs", required=True,
                    help="comma list of model subdirs to scan (e.g. diffusion_models,vae,text_encoders)")
    ap.add_argument("--base-model", default="WAN 2.2")
    ap.add_argument("--source", default="huggingface")
    ap.add_argument("--primary-match", default="",
                    help="substring marking the primary file (e.g. ti2v); default = first file")
    ap.add_argument("--no-hash", action="store_true", help="skip sha256 (faster; integrity unverified)")
    args = ap.parse_args()

    cfg = load_cfg()
    st = cfg["storage"]
    vault_root = Path(st["vault_root"])
    nvme_root = Path(st["nvme_root"])
    catalog = Path(st["catalog_dir"]) / "catalog.sqlite"
    subdirs = [s.strip() for s in args.subdirs.split(",") if s.strip()]

    # discover the asset's files across the requested subdirs (prefer NVMe view,
    # fall back to vault), de-duplicated by (subdir, name).
    found = {}  # (subdir, name) -> {subdir, name}
    for sub in subdirs:
        for root in (nvme_root, vault_root):
            d = root / sub
            if not d.is_dir():
                continue
            for p in d.iterdir():
                if p.is_file() and p.suffix.lower() in MODEL_EXTS:
                    found.setdefault((sub, p.name), {"subdir": sub, "name": p.name})
    files = sorted(found.values(), key=lambda f: (f["subdir"], f["name"]))
    if not files:
        sys.exit(f"no model files found under {subdirs} in {nvme_root} or {vault_root}")

    mid = neg_id(args.source + ":" + args.asset)
    vid = mid - 1
    raw = json.dumps({"id": mid, "name": args.asset, "source": args.source,
                      "base_model": args.base_model, "provisioned": True})

    conn = sqlite3.connect(catalog)
    conn.execute("PRAGMA journal_mode=WAL;")
    # tolerate either the Rust or Python schema (same columns)
    conn.execute(
        """INSERT INTO models(model_id,name,type,nsfw,creator,tags,downloads,rating,
              thumbs_up,comments,cover_url,raw) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)
           ON CONFLICT(model_id) DO UPDATE SET name=excluded.name, raw=excluded.raw""",
        (mid, args.asset, "diffusion_model", 0, "Comfy-Org/HuggingFace",
         json.dumps(["video", "provisioned"]), 0, 0.0, 0, 0, None, raw),
    )
    conn.execute(
        """INSERT INTO versions(version_id,model_id,name,base_model,published_at,
              trained_words,description,downloads,version_idx) VALUES(?,?,?,?,?,?,?,?,?)
           ON CONFLICT(version_id) DO UPDATE SET base_model=excluded.base_model,
              description=excluded.description""",
        (vid, mid, args.asset, args.base_model, None, None,
         f"Provisioned via AIProd provision.py ({args.source})", 0, 0),
    )

    promoted = downloaded = 0
    for i, f in enumerate(files):
        name, sub = f["name"], f["subdir"]
        vault_p = vault_root / sub / name
        nvme_p = nvme_root / sub / name
        anchor = vault_p if vault_p.exists() else nvme_p  # local_path = authoritative
        size_kb = anchor.stat().st_size / 1024.0 if anchor.exists() else 0.0
        sha = "" if args.no_hash or not anchor.exists() else sha256_file(anchor)
        on_nvme = nvme_p.exists()
        status = "promoted" if on_nvme else "downloaded"
        is_primary = 1 if (args.primary_match and args.primary_match.lower() in name.lower()) \
            else (1 if (not args.primary_match and i == 0) else 0)
        fid = neg_id(f"{args.source}:{args.asset}:{sub}/{name}")
        fmt = "GGUF" if name.lower().endswith(".gguf") else "SafeTensor"
        fp = "fp8" if "fp8" in name.lower() else ("fp16" if "fp16" in name.lower() else None)
        conn.execute(
            """INSERT INTO files(file_id,version_id,name,type,size_kb,download_url,sha256,
                  autov2,fp,format,is_primary,local_path,nvme_path,locked,status)
               VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
               ON CONFLICT(file_id) DO UPDATE SET size_kb=excluded.size_kb,
                  sha256=excluded.sha256, local_path=excluded.local_path,
                  nvme_path=excluded.nvme_path, status=excluded.status""",
            (fid, vid, name, "Model", size_kb, "", sha, None, fp, fmt, is_primary,
             str(vault_p) if vault_p.exists() else None,
             str(nvme_p) if on_nvme else None, 0, status),
        )
        conn.execute(
            "INSERT INTO reflog(file_id,model_id,action,detail) VALUES(?,?,?,?)",
            (fid, mid, "ingest", f"provisioned {args.source}:{sub}/{name} -> {status}"),
        )
        promoted += on_nvme
        downloaded += not on_nvme
        print(f"  [{status:9}] {sub}/{name}  ({size_kb/1048576:.2f} GB)"
              + ("" if sha else "  (no hash)"))

    conn.commit()
    conn.close()
    print(f"\nRegistered '{args.asset}' (model_id {mid}): "
          f"{promoted} promoted, {downloaded} downloaded, {len(files)} files total.")


if __name__ == "__main__":
    main()
