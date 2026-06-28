#!/usr/bin/env python3
"""Stage 2 — browse the index and make choices (no downloads).

Queries the local catalog's `picks` view. Print human-readable rows or the
file_ids you'd feed to fetch.py.

Usage:
    python pick.py --type Checkpoint --base "Flux.1 D" --limit 20
    python pick.py --type LORA --sort rating --min-downloads 5000
    python pick.py --search "pixel art" --ids-only
"""
from __future__ import annotations

import argparse

from synthetrix.catalog import connect
from synthetrix.config import load_config

SORTS = {"downloads": "downloads", "rating": "rating", "thumbs": "thumbs_up"}


def main() -> None:
    cfg = load_config()
    ap = argparse.ArgumentParser(description="Browse the curated index.")
    ap.add_argument("--type")
    ap.add_argument("--base", help="Base model substring, e.g. 'Flux.1 D'.")
    ap.add_argument("--search", help="Substring match on model name.")
    ap.add_argument("--sort", choices=SORTS, default="downloads")
    ap.add_argument("--min-downloads", type=int, default=0)
    ap.add_argument("--status", help="indexed | downloaded | promoted")
    ap.add_argument("--limit", type=int, default=25)
    ap.add_argument("--ids-only", action="store_true",
                    help="Print only file_ids (pipe into fetch.py).")
    args = ap.parse_args()

    conn = connect(cfg["storage"]["catalog_dir"])
    where, params = ["1=1"], []
    if args.type:
        where.append("type = ?"); params.append(args.type)
    if args.base:
        where.append("base_model LIKE ?"); params.append(f"%{args.base}%")
    if args.search:
        where.append("model_name LIKE ?"); params.append(f"%{args.search}%")
    if args.status:
        where.append("status = ?"); params.append(args.status)
    if args.min_downloads:
        where.append("downloads >= ?"); params.append(args.min_downloads)

    sql = (f"SELECT * FROM picks WHERE {' AND '.join(where)} "
           f"ORDER BY {SORTS[args.sort]} DESC NULLS LAST LIMIT ?")
    params.append(args.limit)
    rows = conn.execute(sql, params).fetchall()

    if args.ids_only:
        print(" ".join(str(r["file_id"]) for r in rows))
        return

    if not rows:
        print("No matches. Have you run build_index.py yet?")
        return
    print(f"{'file_id':>8}  {'dl':>8}  {'rt':>4}  {'GB':>5}  {'status':<10} "
          f"{'base':<12} name")
    for r in rows:
        gb = (r["size_kb"] or 0) / 1_048_576
        print(f"{r['file_id']:>8}  {r['downloads'] or 0:>8}  "
              f"{(r['rating'] or 0):>4.1f}  {gb:>5.1f}  {r['status']:<10} "
              f"{(r['base_model'] or '')[:12]:<12} {r['model_name']}")
    print(f"\n{len(rows)} rows. Download: "
          f"python fetch.py {rows[0]['file_id']} [more file_ids ...]")


if __name__ == "__main__":
    main()
