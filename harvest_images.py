#!/usr/bin/env python3
"""Batch image+workflow harvest (re-run / catch-up tool).

By default this targets only models you've SELECTED FOR DOWNLOAD (i.e. that have
a downloaded/promoted file) — the normal path is automatic: fetch.py pulls the
full image set when you download a model. Use this to re-harvest, change --per,
or backfill. `--all` widens to every catalog model (large: ~2MB x per x models).

Usage:
    python harvest_images.py                      # downloaded models, per=config
    python harvest_images.py --per 10
    python harvest_images.py --type Checkpoint --base "Flux.1 D"
    python harvest_images.py --all --max-models 50    # any catalog model
"""
from __future__ import annotations

import argparse
from pathlib import Path

from synthetrix.catalog import connect
from synthetrix.config import get_token, load_config
from synthetrix.images import harvest_model, make_session


def select_models(conn, args) -> list:
    where, params = ["1=1"], []
    if not args.all:
        where.append("model_id IN (SELECT v.model_id FROM files f "
                     "JOIN versions v ON v.version_id=f.version_id "
                     "WHERE f.status IN ('downloaded','promoted'))")
    if args.type:
        where.append("type = ?"); params.append(args.type)
    if args.base:
        where.append("model_id IN (SELECT model_id FROM versions "
                     "WHERE base_model LIKE ?)"); params.append(f"%{args.base}%")
    if args.search:
        where.append("name LIKE ?"); params.append(f"%{args.search}%")
    sql = (f"SELECT model_id, name, type FROM models WHERE {' AND '.join(where)} "
           f"ORDER BY downloads DESC")
    if args.max_models:
        sql += f" LIMIT {args.max_models}"
    return conn.execute(sql, params).fetchall()


def main() -> None:
    cfg = load_config()
    ic = cfg["images"]
    ap = argparse.ArgumentParser(description="Batch harvest images + workflows.")
    ap.add_argument("--per", type=int, default=ic["per_model"])
    ap.add_argument("--type"); ap.add_argument("--base"); ap.add_argument("--search")
    ap.add_argument("--max-models", type=int)
    ap.add_argument("--all", action="store_true",
                    help="Every catalog model, not just downloaded ones.")
    ap.add_argument("--no-video", action="store_true")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    conn = connect(cfg["storage"]["catalog_dir"])
    models = select_models(conn, args)
    include_video = ic["include_video"] and not args.no_video
    scope = "ALL catalog" if args.all else "downloaded"
    print(f"Plan: {len(models)} {scope} models x up to {args.per} images each "
          f"(video={'on' if include_video else 'off'})")
    if args.dry_run:
        for m in models[:20]:
            print(f"  {m['type']:11} {m['name']}")
        if len(models) > 20:
            print(f"  ... +{len(models)-20} more")
        return
    if not models:
        hint = ("Run build_index.py first." if args.all
                else "No downloaded models yet — fetch.py auto-harvests on "
                     "download, or use --all to backfill the whole catalog.")
        print(hint); return

    token = get_token(required=ic["nsfw"])
    sess = make_session(token)
    gallery_root = Path(cfg["storage"]["gallery_root"])

    totals: dict[str, int] = {}
    for i, m in enumerate(models, 1):
        t = harvest_model(conn, sess, gallery_root, m["model_id"],
                          per=args.per, include_video=include_video)
        for k, v in t.items():
            totals[k] = totals.get(k, 0) + v
        wf = t.get("saved+wf", 0)
        saved = sum(v for k, v in t.items() if k.startswith("saved"))
        print(f"  [{i}/{len(models)}] {m['name'][:48]:48} {saved:>2} imgs, {wf} wf")

    print(f"\nDone. saved={sum(v for k,v in totals.items() if k.startswith('saved'))} "
          f"(workflows={totals.get('saved+wf',0)}), "
          f"existed={totals.get('exists',0)}, "
          f"skipped-video={totals.get('skip-video',0)}, "
          f"errors={totals.get('error',0)}")
    print(f"Gallery: {gallery_root}")


if __name__ == "__main__":
    main()
