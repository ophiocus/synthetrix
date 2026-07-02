"""Path authority: the single-source-of-truth check + writer for the model tree.

The bug that started all this: the NVMe tier must agree across THREE layers or a
promoted model is invisible to the running ComfyUI:
  (1) config.toml [storage].nvme_root       (Synthetrix writes promotes here)
  (2) extra_model_paths.yaml base_path      (ComfyUI reads models from here)
  (3) (the Rust app's config default)       - checked by the app, not here
`base_path` in the yaml is the PARENT of the model subdirs, so it should equal
nvme_root with the trailing `/models` removed. This module verifies (1)<->(2)
and can (re)write a correct extra_model_paths.yaml.
"""
from __future__ import annotations

import re
from pathlib import Path

# CivitAI/type -> subdir, plus the extra trees ComfyUI needs.
SUBDIRS = {
    "checkpoints": "checkpoints", "loras": "loras", "embeddings": "embeddings",
    "controlnet": "controlnet", "vae": "vae", "upscale_models": "upscale_models",
    "clip": "clip", "clip_vision": "clip_vision", "text_encoders": "text_encoders",
    "diffusion_models": "diffusion_models", "unet": "unet",
}


def _norm(p: str) -> str:
    return str(p).replace("\\", "/").rstrip("/").lower()


def nvme_base(nvme_root: str) -> str:
    """The yaml base_path = nvme_root minus a trailing '/models' (ComfyUI appends
    the subdir names itself)."""
    n = str(nvme_root).replace("\\", "/").rstrip("/")
    if n.lower().endswith("/models"):
        n = n[: -len("/models")]
    return n + "/"


def read_base_path(yaml_path: str) -> str | None:
    """Light parse: the `base_path:` under a `synthetrix:` block (no yaml dep)."""
    p = Path(yaml_path)
    if not p.exists():
        return None
    in_block = False
    for raw in p.read_text(encoding="utf-8", errors="replace").splitlines():
        if re.match(r"^\S", raw):                       # a top-level key
            in_block = raw.strip().rstrip(":").strip() == "synthetrix"
            continue
        if in_block:
            m = re.match(r"\s*base_path:\s*[\"']?(.+?)[\"']?\s*$", raw)
            if m:
                return m.group(1)
    return None


def check_agreement(nvme_root: str, yaml_path: str) -> tuple[bool, str]:
    """(agree, message) between config nvme_root and the yaml base_path."""
    want = nvme_base(nvme_root)
    have = read_base_path(yaml_path)
    if have is None:
        return False, f"no synthetrix base_path in {yaml_path}"
    if _norm(have) == _norm(want):
        return True, f"base_path == nvme tier ({want})"
    return False, f"MISMATCH: yaml base_path={have!r} but nvme tier implies {want!r}"


def render_yaml(nvme_root: str) -> str:
    base = nvme_base(nvme_root)
    lines = ["# Written by Synthetrix comfyctl - maps ComfyUI to the Synthetrix NVMe tier.",
             "synthetrix:", f'  base_path: "{base}"', "  is_default: false"]
    for key, sub in SUBDIRS.items():
        lines.append(f"  {key}: models/{sub}")
    return "\n".join(lines) + "\n"


def write_yaml(nvme_root: str, yaml_path: str) -> str:
    """(Re)write extra_model_paths.yaml with a correct synthetrix block. Returns the
    path written. Caller decides when this is allowed (heal/provision)."""
    text = render_yaml(nvme_root)
    Path(yaml_path).write_text(text, encoding="utf-8")
    return yaml_path


def dirs_exist(root: str) -> list[str]:
    """Subdirs missing under a model root (vault or nvme tier)."""
    base = Path(root)
    return [s for s in SUBDIRS.values() if not (base / s).is_dir()]
