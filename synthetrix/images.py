"""Shared image-harvest core: download example images + extract workflows.

Images come straight from the model's stored JSON (modelVersions[0].images, up
to 20 per version) — no extra API call. The original file is fetched via its
`original=true` URL; content-type (not the URL extension, which lies) decides the
real format. ComfyUI workflow / A1111 params are recovered from PNG text chunks;
JPEG/WebP originals are re-encoded by CivitAI and carry nothing.
"""
from __future__ import annotations

import json
from pathlib import Path

import requests

from .imgmeta import png_text_chunks, split_meta

EXT = {"image/png": ".png", "image/jpeg": ".jpg", "image/webp": ".webp",
       "video/mp4": ".mp4"}


def make_session(token: str | None) -> requests.Session:
    s = requests.Session()
    if token:
        s.headers["Authorization"] = f"Bearer {token}"
    s.headers["User-Agent"] = "synthetrix-harvester/1.0"
    return s


def _save_one(sess, conn, gallery_root: Path, model_id: int, img: dict,
              include_video: bool, is_starter: int) -> str:
    img_id = img["id"]
    mtype = img.get("type", "image")
    if mtype == "video" and not include_video:
        return "skip-video"
    existing = conn.execute("SELECT local_path FROM images WHERE image_id=?",
                            (img_id,)).fetchone()
    if existing and existing["local_path"] and Path(existing["local_path"]).exists():
        return "exists"

    r = sess.get(img["url"], timeout=120)
    r.raise_for_status()
    data = r.content
    ct = r.headers.get("content-type", "").split(";")[0]
    ext = EXT.get(ct, ".bin")
    mdir = gallery_root / str(model_id)
    mdir.mkdir(parents=True, exist_ok=True)
    fpath = mdir / f"{img_id}{ext}"
    fpath.write_bytes(data)

    wf_path = params_path = None
    has_wf = 0
    if ext == ".png":
        workflow, params = split_meta(png_text_chunks(data))
        if workflow is not None:
            wf_path = mdir / f"{img_id}.workflow.json"
            wf_path.write_text(json.dumps(workflow, indent=1), encoding="utf-8")
            has_wf = 1
        if params:
            params_path = mdir / f"{img_id}.params.txt"
            params_path.write_text(params, encoding="utf-8")

    conn.execute(
        """INSERT INTO images(image_id,model_id,url,media_type,nsfw_level,
               width,height,reactions,local_path,workflow_path,params_path,
               has_workflow,is_starter,status)
           VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?, 'saved')
           ON CONFLICT(image_id) DO UPDATE SET
             local_path=excluded.local_path, workflow_path=excluded.workflow_path,
             params_path=excluded.params_path, has_workflow=excluded.has_workflow,
             status='saved'""",
        (img_id, model_id, img["url"], mtype, img.get("nsfwLevel"),
         img.get("width"), img.get("height"), None,
         str(fpath), str(wf_path) if wf_path else None,
         str(params_path) if params_path else None, has_wf, is_starter),
    )
    conn.commit()
    return "saved+wf" if has_wf else "saved"


def _version_images(conn, model_id: int) -> list[dict]:
    """Read the latest version's embedded images from the stored model JSON."""
    row = conn.execute("SELECT raw FROM models WHERE model_id=?",
                       (model_id,)).fetchone()
    if not row or not row["raw"]:
        return []
    versions = (json.loads(row["raw"]).get("modelVersions") or [])
    return (versions[0].get("images") if versions else []) or []


def harvest_model(conn, sess, gallery_root: Path, model_id: int, *,
                  per: int, include_video: bool, starter: bool = False) -> dict:
    """Save images for one model. starter=True → exactly one preview image."""
    imgs = _version_images(conn, model_id)
    if starter:
        # first still image is the author's hero shot — the list preview
        stills = [i for i in imgs if i.get("type") == "image"]
        take = stills[:1] or imgs[:1]
    else:
        take = imgs[:per]
    tally: dict[str, int] = {}
    for im in take:
        try:
            s = _save_one(sess, conn, gallery_root, model_id, im,
                          include_video, 1 if starter else 0)
        except Exception:
            s = "error"
        tally[s] = tally.get(s, 0) + 1
    return tally
