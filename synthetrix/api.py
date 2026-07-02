"""CivitAI REST API client: cursor pagination, throttling, retry/backoff."""
from __future__ import annotations

import time
from typing import Iterator

import requests


class CivitAIClient:
    def __init__(self, base_url: str, token: str | None,
                 requests_per_min: int = 90, max_retries: int = 5):
        self.base_url = base_url.rstrip("/")
        self.token = token
        self.min_interval = 60.0 / max(1, requests_per_min)
        self.max_retries = max_retries
        self._last = 0.0
        self.session = requests.Session()
        if token:
            self.session.headers["Authorization"] = f"Bearer {token}"
        self.session.headers["User-Agent"] = "synthetrix-harvester/1.0"

    def _throttle(self) -> None:
        wait = self.min_interval - (time.monotonic() - self._last)
        if wait > 0:
            time.sleep(wait)
        self._last = time.monotonic()

    def _get(self, url: str, params: dict | None = None) -> dict:
        for attempt in range(self.max_retries):
            self._throttle()
            try:
                resp = self.session.get(url, params=params, timeout=60)
            except requests.RequestException as exc:
                if attempt == self.max_retries - 1:
                    raise
                time.sleep(2 ** attempt)
                continue
            if resp.status_code == 429:
                backoff = int(resp.headers.get("Retry-After", 2 ** (attempt + 1)))
                time.sleep(backoff)
                continue
            if resp.status_code >= 500:
                time.sleep(2 ** attempt)
                continue
            resp.raise_for_status()
            return resp.json()
        raise RuntimeError(f"Exhausted retries for {url}")

    def iter_models(self, *, types: str, base_models: str | None = None,
                    sort: str = "Most Downloaded", period: str = "AllTime",
                    nsfw: bool = False, page_size: int = 100,
                    max_items: int | None = None, query: str | None = None,
                    tag: str | None = None, username: str | None = None,
                    checkpoint_type: str | None = None) -> Iterator[dict]:
        """Cursor-walk /models. Yields one model dict at a time.

        Cursor pagination is mandatory for the browse sorts: page*limit > 1000
        returns 429. When ``query`` is set the API switches to Meilisearch
        offset pagination (numeric ``nextPage``), which we follow as a fallback.

        Extra filters (all optional, ANDed with types/baseModels):
          * ``query``          — full-text search (Meilisearch)
          * ``tag``            — single tag slug, e.g. "character"
          * ``username``       — restrict to one creator
          * ``checkpoint_type``— Standard | Trained | Merge (Checkpoints only)
        """
        params = {
            "types": types,
            "sort": sort,
            "period": period,
            "nsfw": str(nsfw).lower(),
            "limit": page_size,
        }
        if base_models:
            params["baseModels"] = base_models
        if query:
            params["query"] = query
        if tag:
            params["tag"] = tag
        if username:
            params["username"] = username
        if checkpoint_type:
            params["checkpointType"] = checkpoint_type
        url = f"{self.base_url}/models"
        yielded = 0
        cursor: str | None = None
        page: int | None = None
        while True:
            if cursor:
                params["cursor"] = cursor
            elif page:
                params["page"] = page
            data = self._get(url, params=params)
            items = data.get("items", [])
            if not items:
                break
            for item in items:
                yield item
                yielded += 1
                if max_items and yielded >= max_items:
                    return
            meta = data.get("metadata") or {}
            cursor = meta.get("nextCursor")
            # Meilisearch (query mode) hands back a numeric next page instead.
            next_page = meta.get("nextPage")
            page = next_page if (not cursor and isinstance(next_page, int)) else None
            if not cursor and not page:
                break

    def models_by_ids(self, ids: list[int], chunk: int = 100) -> Iterator[dict]:
        """Batch-fetch known models by id via the /models ?ids= filter.

        The delta-refresh path: one request per <=100 ids re-pulls the full
        model JSON (stats, versions, files, images) for rows already tracked.
        No sort/nsfw filter is sent, so mature rows come back too. Yields the
        model dicts in whatever order the API returns them.
        """
        url = f"{self.base_url}/models"
        for i in range(0, len(ids), chunk):
            batch = ids[i:i + chunk]
            data = self._get(url, params={
                "ids": ",".join(str(x) for x in batch),
                "limit": len(batch),
            })
            for item in data.get("items", []):
                yield item
