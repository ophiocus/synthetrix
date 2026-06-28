#!/usr/bin/env python3
"""Stage 1 — build the curated index.

Cursor-walks the CivitAI API for each (type x base_model x query) combination,
keeps the top-N per combo, writes everything to the SQLite catalog, and emits a
usage-doc sidecar per model's latest version. JSON-only: no model blobs pulled.

Usage:
    python build_index.py                 # full crawl per config.toml
    python build_index.py --type LORA     # restrict types
    python build_index.py --dry-run       # show the plan, hit nothing
"""
from __future__ import annotations

import argparse

from pathlib import Path

from synthetrix.api import CivitAIClient
from synthetrix.catalog import connect, upsert_model, write_usage_doc
from synthetrix.config import get_token, load_config
from synthetrix.images import harvest_model, make_session


def main() -> None:
    cfg = load_config()
    ap = argparse.ArgumentParser(description="Build the CivitAI curated index.")
    ap.add_argument("--type", action="append", dest="types",
                    help="Restrict to these model types (repeatable).")
    ap.add_argument("--top-n", type=int, default=cfg["crawl"]["top_n"])
    ap.add_argument("--no-nsfw", action="store_true", help="Exclude NSFW.")
    ap.add_argument("--no-starter", action="store_true",
                    help="Skip downloading the 1 preview image per model.")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    crawl = cfg["crawl"]
    types = args.types or crawl["types"]
    base_models = crawl["base_models"]
    queries = crawl["query"]
    nsfw = crawl["nsfw"] and not args.no_nsfw

    combos = [(t, b, q) for t in types for b in base_models for q in queries]
    print(f"Plan: {len(combos)} combos "
          f"({len(types)} types x {len(base_models)} base x {len(queries)} queries), "
          f"top {args.top_n} each, nsfw={nsfw}")
    if args.dry_run:
        for t, b, q in combos:
            print(f"  {t:11} | {b:12} | {q['sort']:16} {q['period']}")
        return

    token = get_token(required=nsfw)  # token mandatory only when pulling NSFW
    client = CivitAIClient(
        cfg["api"]["base_url"], token,
        requests_per_min=cfg["api"]["requests_per_min"],
        max_retries=cfg["api"]["max_retries"],
    )
    conn = connect(cfg["storage"]["catalog_dir"])
    catalog_dir = cfg["storage"]["catalog_dir"]
    gallery_root = Path(cfg["storage"]["gallery_root"])
    want_starter = not args.no_starter
    sess = make_session(token) if want_starter else None
    include_video = cfg["images"]["include_video"]

    seen_models: set[int] = set()
    total = starters = 0
    for t, b, q in combos:
        kept = 0
        for m in client.iter_models(
            types=t, base_models=b, sort=q["sort"], period=q["period"],
            nsfw=nsfw, page_size=crawl["page_size"], max_items=args.top_n,
        ):
            upsert_model(conn, m)
            versions = m.get("modelVersions") or []
            if m["id"] not in seen_models and versions:
                write_usage_doc(catalog_dir, m, versions[0])
                if want_starter:
                    tally = harvest_model(conn, sess, gallery_root, m["id"],
                                          per=1, include_video=include_video,
                                          starter=True)
                    starters += sum(v for k, v in tally.items()
                                    if k.startswith("saved"))
                seen_models.add(m["id"])
            kept += 1
            total += 1
        print(f"  [{t:11}|{b:12}|{q['sort']:16}{q['period']:8}] {kept:4} models")

    print(f"\nDone. {total} model-rows processed, "
          f"{len(seen_models)} unique models, {starters} starter previews. "
          f"Catalog: {catalog_dir}/catalog.sqlite")
    print("Pick with:  python pick.py --type Checkpoint --base 'Flux.1 D' --limit 20")
    print("Full images+workflows are pulled per-model when you run fetch.py.")


if __name__ == "__main__":
    main()
