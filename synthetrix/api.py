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
                    max_items: int | None = None) -> Iterator[dict]:
        """Cursor-walk /models. Yields one model dict at a time.

        Cursor pagination is mandatory: page*limit > 1000 returns 429.
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
        url = f"{self.base_url}/models"
        yielded = 0
        cursor: str | None = None
        while True:
            if cursor:
                params["cursor"] = cursor
            data = self._get(url, params=params)
            items = data.get("items", [])
            if not items:
                break
            for item in items:
                yield item
                yielded += 1
                if max_items and yielded >= max_items:
                    return
            cursor = (data.get("metadata") or {}).get("nextCursor")
            if not cursor:
                break
