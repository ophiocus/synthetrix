#!/usr/bin/env python3
"""Stage 1 — build / refresh the curated index.

Three modes, all writing to the same catalog.sqlite metadata database:

  * full crawl (default) — cursor-walk /models for each
    (type x base_model x query x scope) combo, keep the top-N of each.
  * ``--delta``          — a fast Newest-sorted catch-up pass that
    short-circuits once it overlaps the models already in the catalog.
    CivitAI has no updated-since filter, so the delta is client-computed:
    publish order tracks id, so a few pages of Newest surface everything
    added since the last run.
  * ``--refresh``        — re-pull the full JSON for models already tracked,
    batched through the /models ?ids= filter (100/call). This freshens
    stats / versions / files / images for known rows.

Search filters (``--query`` / ``--tag`` / ``--username`` / ``--checkpoint-type``)
are ANDed onto every crawl pass, so coverage isn't locked to the base-model grid.

JSON-only: no model blobs pulled here (fetch.py downloads the actual files).

Usage:
    python build_index.py                       # full crawl per config.toml
    python build_index.py --type LORA           # restrict types
    python build_index.py --delta               # new-publish catch-up
    python build_index.py --refresh             # freshen known rows via ?ids=
    python build_index.py --tag character       # filter every pass by a tag
    python build_index.py --query "cyberpunk"   # full-text targeted crawl
    python build_index.py --dry-run             # show the plan, hit nothing
"""
from __future__ import annotations

import argparse

from pathlib import Path

from synthetrix.api import CivitAIClient
from synthetrix.catalog import (connect, known_model_ids, upsert_model,
                                 write_usage_doc)
from synthetrix.config import get_token, load_config
from synthetrix.images import harvest_model, make_session


def _persist(conn, catalog_dir, m, seen, want_starter, sess, gallery_root,
             include_video):
    """Upsert one model (+ versions/files), write its usage doc once, and pull
    the single starter preview the first time we see it. Returns saved-image
    count from the starter harvest (0 if skipped/duplicate)."""
    upsert_model(conn, m)
    versions = m.get("modelVersions") or []
    saved = 0
    if m["id"] not in seen and versions:
        write_usage_doc(catalog_dir, m, versions[0])
        if want_starter and sess is not None:
            tally = harvest_model(conn, sess, gallery_root, m["id"], per=1,
                                  include_video=include_video, starter=True)
            saved = sum(v for k, v in tally.items() if k.startswith("saved"))
        seen.add(m["id"])
    return saved


def main() -> None:
    cfg = load_config()
    ap = argparse.ArgumentParser(description="Build / refresh the CivitAI curated index.")
    ap.add_argument("--type", action="append", dest="types",
                    help="Restrict to these model types (repeatable).")
    ap.add_argument("--top-n", type=int, default=cfg["crawl"]["top_n"])
    ap.add_argument("--no-nsfw", action="store_true", help="Skip the mature/Red pass.")
    ap.add_argument("--no-sfw", action="store_true", help="Skip the SFW/civit pass.")
    ap.add_argument("--nsfw-only", action="store_true", help="Only the Red pass.")
    ap.add_argument("--sfw-only", action="store_true", help="Only the SFW/civit pass.")
    ap.add_argument("--no-starter", action="store_true",
                    help="Skip downloading the 1 preview image per model.")
    # --- incremental modes -------------------------------------------------
    ap.add_argument("--delta", action="store_true",
                    help="New-publish catch-up: crawl Newest, stop once we "
                         "overlap the catalog.")
    ap.add_argument("--refresh", action="store_true",
                    help="Re-pull known models via the /models ?ids= filter "
                         "(freshen stats/versions/images).")
    ap.add_argument("--delta-stop-after", type=int,
                    help="Delta: stop a pass after N consecutive already-known "
                         "models (default from config).")
    # --- search filters (ANDed onto every pass) ----------------------------
    ap.add_argument("--query", help="Full-text search filter (Meilisearch).")
    ap.add_argument("--tag", help="Restrict to a single tag slug, e.g. 'character'.")
    ap.add_argument("--username", help="Restrict to one creator.")
    ap.add_argument("--checkpoint-type", choices=["Standard", "Trained", "Merge"],
                    help="Checkpoint sub-type filter.")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    crawl = cfg["crawl"]
    types = args.types or crawl["types"]
    base_models = crawl["base_models"]
    filters = dict(query=args.query, tag=args.tag, username=args.username,
                   checkpoint_type=args.checkpoint_type)

    # Scopes: SFW ("civit") and/or mature ("red"). Token mandatory only for red.
    want_sfw = crawl.get("sfw", True) and not args.no_sfw and not args.nsfw_only
    want_nsfw = crawl.get("nsfw", True) and not args.no_nsfw and not args.sfw_only
    scopes = ([("civit", False)] if want_sfw else []) + ([("red", True)] if want_nsfw else [])
    if not scopes:
        scopes = [("civit", False)]

    conn = connect(cfg["storage"]["catalog_dir"])
    catalog_dir = cfg["storage"]["catalog_dir"]
    gallery_root = Path(cfg["storage"]["gallery_root"])
    want_starter = not args.no_starter
    include_video = cfg["images"]["include_video"]

    # -------------------------------------------------------------------
    # REFRESH — re-pull known rows via ?ids=. No crawl, no scopes/combos.
    # -------------------------------------------------------------------
    if args.refresh:
        ids = known_model_ids(conn)
        active = [f"{k}={v}" for k, v in filters.items() if v]
        print(f"Refresh: re-pulling {len(ids)} known models via ?ids= "
              f"(100/call){' | filters ' + ', '.join(active) if active else ''}")
        if args.dry_run:
            print(f"  would issue {(len(ids) + 99) // 100} batched requests")
            return
        token = get_token(required=False)
        client = CivitAIClient(cfg["api"]["base_url"], token,
                               requests_per_min=cfg["api"]["requests_per_min"],
                               max_retries=cfg["api"]["max_retries"])
        sess = make_session(token) if want_starter else None
        seen: set[int] = set()
        starters = 0
        for m in client.models_by_ids(ids):
            starters += _persist(conn, catalog_dir, m, seen, want_starter, sess,
                                  gallery_root, include_video)
        print(f"\nDone. {len(seen)} models refreshed, {starters} starter previews. "
              f"Catalog: {catalog_dir}/catalog.sqlite")
        return

    # -------------------------------------------------------------------
    # CRAWL — full (default) or --delta (Newest + early stop).
    # -------------------------------------------------------------------
    delta_cfg = crawl.get("delta", {})
    if args.delta:
        # Delta overrides the query lenses with a single Newest pass.
        queries = [{"sort": "Newest", "period": delta_cfg.get("period", "Week")}]
        stop_after = args.delta_stop_after or delta_cfg.get("stop_after_known", 25)
        known = set(known_model_ids(conn))
    else:
        queries = crawl["query"]
        stop_after = None
        known = set()

    combos = [(t, b, q) for t in types for b in base_models for q in queries]
    mode = "DELTA" if args.delta else "FULL"
    active = [f"{k}={v}" for k, v in filters.items() if v]
    print(f"Plan [{mode}]: {len(combos)} combos x {len(scopes)} scope(s) "
          f"({len(types)} types x {len(base_models)} base x {len(queries)} queries), "
          f"top {args.top_n} each, scopes={[s[0] for s in scopes]}"
          f"{' | filters ' + ', '.join(active) if active else ''}"
          f"{f' | stop after {stop_after} known' if args.delta else ''}")
    if args.dry_run:
        for t, b, q in combos:
            print(f"  {t:11} | {b:12} | {q['sort']:16} {q['period']}")
        return

    token = get_token(required=want_nsfw)  # token mandatory only for the Red pass
    client = CivitAIClient(cfg["api"]["base_url"], token,
                           requests_per_min=cfg["api"]["requests_per_min"],
                           max_retries=cfg["api"]["max_retries"])
    sess = make_session(token) if want_starter else None

    seen = set()
    total = starters = 0
    for t, b, q in combos:
        for scope_name, scope_nsfw in scopes:
            kept = consec_known = 0
            for m in client.iter_models(
                types=t, base_models=b, sort=q["sort"], period=q["period"],
                nsfw=scope_nsfw, page_size=crawl["page_size"], max_items=args.top_n,
                **filters,
            ):
                is_known = args.delta and m["id"] in known
                starters += _persist(conn, catalog_dir, m, seen, want_starter,
                                     sess, gallery_root, include_video)
                kept += 1
                total += 1
                if args.delta:
                    consec_known = consec_known + 1 if is_known else 0
                    if consec_known >= stop_after:
                        break
            print(f"  [{scope_name:5}|{t:11}|{b:12}|{q['sort']:16}{q['period']:8}] "
                  f"{kept:4} models")

    print(f"\nDone. {total} model-rows processed, "
          f"{len(seen)} unique models, {starters} starter previews. "
          f"Catalog: {catalog_dir}/catalog.sqlite")
    print("Pick with:  python pick.py --type Checkpoint --base 'Flux.1 D' --limit 20")
    print("Full images+workflows are pulled per-model when you run fetch.py.")


if __name__ == "__main__":
    main()
