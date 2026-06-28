#!/usr/bin/env python3
"""Stage 3 — download chosen files to the HDD vault, then promote to NVMe.

Resolves file_ids from the catalog, streams each to the vault subdir for its
type, verifies SHA256, records local_path, and (optionally) promotes to the
NVMe ComfyUI models tree on demand.

Usage:
    python fetch.py 123456 123457            # download to vault, verify
    python fetch.py 123456 --promote         # also copy/symlink to NVMe
    python fetch.py --promote-only 123456    # already in vault -> NVMe
"""
from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import sys
from pathlib import Path

import requests

from synthetrix.catalog import connect
from synthetrix.config import get_token, load_config
from synthetrix.images import harvest_model, make_session


def _subdir(cfg: dict, model_type: str) -> str:
    return cfg["storage"]["subdir"].get(model_type, "misc")


def _verify(path: Path, sha256: str | None) -> bool:
    if not sha256:
        return True  # nothing to check against
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest().lower() == sha256.lower()


def download(cfg, conn, token, file_id: int) -> Path | None:
    row = conn.execute(
        """SELECT f.*, m.type AS model_type, m.name AS model_name
           FROM files f JOIN versions v ON v.version_id=f.version_id
           JOIN models m ON m.model_id=v.model_id WHERE f.file_id=?""",
        (file_id,),
    ).fetchone()
    if not row:
        print(f"  ! file_id {file_id} not in catalog"); return None

    dest_dir = Path(cfg["storage"]["vault_root"]) / _subdir(cfg, row["model_type"])
    dest_dir.mkdir(parents=True, exist_ok=True)
    dest = dest_dir / (row["name"] or f"{file_id}.bin")

    if row["local_path"] and Path(row["local_path"]).exists():
        print(f"  = {dest.name} already in vault"); return Path(row["local_path"])

    url = row["download_url"]
    headers = {"Authorization": f"Bearer {token}"} if token else {}
    print(f"  v {row['model_name']} -> {dest}")
    tmp = dest.with_suffix(dest.suffix + ".part")
    with requests.get(url, headers=headers, stream=True, timeout=120) as r:
        r.raise_for_status()
        total = int(r.headers.get("Content-Length", 0))
        done = 0
        with open(tmp, "wb") as fh:
            for chunk in r.iter_content(1 << 20):
                fh.write(chunk)
                done += len(chunk)
                if total:
                    pct = 100 * done / total
                    print(f"\r    {pct:5.1f}%  {done>>20}/{total>>20} MB",
                          end="", flush=True)
    print()
    if not _verify(tmp, row["sha256"]):
        tmp.unlink(missing_ok=True)
        print(f"  ! SHA256 mismatch for {dest.name} — discarded"); return None
    tmp.replace(dest)
    conn.execute("UPDATE files SET local_path=?, status='downloaded' WHERE file_id=?",
                 (str(dest), file_id))
    conn.commit()
    print(f"  + verified {dest.name}")
    return dest


def promote(cfg, conn, file_id: int) -> Path | None:
    row = conn.execute(
        """SELECT f.*, m.type AS model_type FROM files f
           JOIN versions v ON v.version_id=f.version_id
           JOIN models m ON m.model_id=v.model_id WHERE f.file_id=?""",
        (file_id,),
    ).fetchone()
    if not row or not row["local_path"] or not Path(row["local_path"]).exists():
        print(f"  ! file_id {file_id} not in vault — download first"); return None
    src = Path(row["local_path"])
    nvme_dir = Path(cfg["storage"]["nvme_root"]) / _subdir(cfg, row["model_type"])
    nvme_dir.mkdir(parents=True, exist_ok=True)
    dst = nvme_dir / src.name
    mode = cfg["storage"]["promote_mode"]
    if dst.exists():
        print(f"  = {dst.name} already on NVMe")
    elif mode == "symlink":
        try:
            os.symlink(src, dst)
            print(f"  -> symlinked {dst}")
        except OSError as e:
            print(f"  ! symlink failed ({e}); falling back to copy")
            shutil.copy2(src, dst)
    else:
        print(f"  -> copying to NVMe {dst}")
        shutil.copy2(src, dst)
    conn.execute("UPDATE files SET nvme_path=?, status='promoted' WHERE file_id=?",
                 (str(dst), file_id))
    conn.commit()
    return dst


def main() -> None:
    cfg = load_config()
    ap = argparse.ArgumentParser(description="Download + promote chosen models.")
    ap.add_argument("file_ids", nargs="*", type=int)
    ap.add_argument("--promote", action="store_true",
                    help="After download, promote to NVMe.")
    ap.add_argument("--promote-only", action="store_true",
                    help="Skip download; promote existing vault files to NVMe.")
    ap.add_argument("--no-images", action="store_true",
                    help="Don't harvest example images+workflows after download.")
    ap.add_argument("--images-count", type=int, default=cfg["images"]["per_model"],
                    help="Example images to harvest per model (default from config).")
    ap.add_argument("--stdin", action="store_true",
                    help="Read file_ids from stdin (e.g. from pick.py --ids-only).")
    args = ap.parse_args()

    ids = list(args.file_ids)
    if args.stdin:
        ids += [int(x) for x in sys.stdin.read().split()]
    if not ids:
        ap.error("no file_ids given")

    conn = connect(cfg["storage"]["catalog_dir"])
    token = get_token(required=not args.promote_only)
    gallery_root = Path(cfg["storage"]["gallery_root"])
    img_sess = make_session(token)
    include_video = cfg["images"]["include_video"]
    harvested: set[int] = set()
    for fid in ids:
        if args.promote_only:
            promote(cfg, conn, fid)
            continue
        if download(cfg, conn, token, fid):
            if args.promote:
                promote(cfg, conn, fid)
            if not args.no_images:
                row = conn.execute(
                    "SELECT v.model_id FROM files f "
                    "JOIN versions v ON v.version_id=f.version_id "
                    "WHERE f.file_id=?", (fid,)).fetchone()
                mid = row["model_id"] if row else None
                if mid and mid not in harvested:
                    tally = harvest_model(conn, img_sess, gallery_root, mid,
                                          per=args.images_count,
                                          include_video=include_video)
                    saved = sum(v for k, v in tally.items()
                                if k.startswith("saved"))
                    wf = tally.get("saved+wf", 0)
                    print(f"  images: {saved} saved ({wf} workflows) for model {mid}")
                    harvested.add(mid)


if __name__ == "__main__":
    main()
