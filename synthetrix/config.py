"""Config + secret loading. Reads config.toml and the CIVITAI_TOKEN secret."""
from __future__ import annotations

import os
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def _load_dotenv(path: Path) -> None:
    """Minimal .env loader (no external dep). Sets os.environ if not already set."""
    if not path.exists():
        return
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, val = line.partition("=")
        os.environ.setdefault(key.strip(), val.strip().strip('"').strip("'"))


def load_config(path: Path | None = None) -> dict:
    cfg_path = path or (ROOT / "config.toml")
    with open(cfg_path, "rb") as fh:
        return tomllib.load(fh)


def get_token(required: bool = True) -> str | None:
    _load_dotenv(ROOT / ".env")
    token = os.environ.get("CIVITAI_TOKEN")
    if required and not token:
        raise SystemExit(
            "No CIVITAI_TOKEN found. Copy .env.example -> .env and add your token "
            "(create one at https://civitai.com/user/account)."
        )
    return token
