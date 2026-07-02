"""Launch / warm up / stop the local ComfyUI on the managed venv.

No `requests` dependency (urllib only) so it runs under a bare system Python.
"""
from __future__ import annotations

import json
import os
import subprocess
import time
import urllib.request
import urllib.error
from pathlib import Path

from .profile import venv_python


def base_url(ccfg: dict) -> str:
    return f"http://{ccfg['host']}:{ccfg['port']}"


def reachable(ccfg: dict, timeout: float = 3.0) -> tuple[bool, str | None]:
    """(up, comfyui_version) via /system_stats."""
    try:
        with urllib.request.urlopen(base_url(ccfg) + "/system_stats", timeout=timeout) as r:
            d = json.load(r)
            ver = (d.get("system") or {}).get("comfyui_version")
            return True, ver
    except Exception:
        return False, None


def is_running(ccfg: dict) -> bool:
    return reachable(ccfg, timeout=2.0)[0]


def start(ccfg: dict, extra_flags: list[str] | None = None) -> tuple[bool, str]:
    """Spawn ComfyUI detached on the managed venv. Returns (spawned, message).
    Does NOT wait for readiness - call warmup() for that."""
    root = Path(ccfg["comfy_root"])
    py = venv_python(ccfg["venv"])
    if not py.exists():
        return False, f"venv python missing: {py}"
    if not (root / "main.py").exists():
        return False, f"main.py missing under {root}"
    args = [str(py), "main.py",
            "--port", str(ccfg["port"]), "--listen", ccfg["host"]]
    args += list(extra_flags or [])
    args += list(ccfg.get("launch_args") or [])
    log = open(root / "synthetrix-comfy.log", "ab", buffering=0)
    # Isolate from any user-site torch that could shadow the venv's (the
    # ENABLE_USER_SITE=True risk the rules engine flags).
    env = dict(os.environ, PYTHONNOUSERSITE="1")
    kw = dict(cwd=str(root), stdout=log, stderr=log, stdin=subprocess.DEVNULL,
              close_fds=True, env=env)
    if os.name == "nt":
        # The server MUST outlive comfyctl. DETACHED_PROCESS + NEW_PROCESS_GROUP
        # detach the console/signals; CREATE_BREAKAWAY_FROM_JOB is the crucial one
        # - it escapes the launcher's job object so a parent teardown (a shell that
        # spawned comfyctl, a job-killing harness) can't take the server with it.
        DETACHED, NEW_GROUP, BREAKAWAY = 0x00000008, 0x00000200, 0x01000000
        try:
            subprocess.Popen(args, creationflags=DETACHED | NEW_GROUP | BREAKAWAY, **kw)
        except OSError:
            # job doesn't permit breakaway - fall back to plain detach
            subprocess.Popen(args, creationflags=DETACHED | NEW_GROUP, **kw)
    else:
        try:
            subprocess.Popen(args, start_new_session=True, **kw)
        except Exception as e:
            return False, f"spawn failed: {e}"
    return True, f"launched: {' '.join(args)}"


def warmup(ccfg: dict, extra_flags: list[str] | None = None,
           timeout: float | None = None, launch: bool = True) -> tuple[bool, str]:
    """Ensure ComfyUI is up. If down and launch=True, start it and poll until it
    answers /system_stats (or timeout). Returns (up, message). Cold boots with many
    custom-node packs can take 2-3 min, so the default window is generous."""
    if timeout is None:
        timeout = float(ccfg.get("warmup_timeout", 240))
    up, ver = reachable(ccfg)
    if up:
        return True, f"already running (ComfyUI {ver})"
    if not launch:
        return False, "not running (launch disabled)"
    spawned, msg = start(ccfg, extra_flags)
    if not spawned:
        return False, msg
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        time.sleep(2.0)
        up, ver = reachable(ccfg, timeout=2.0)
        if up:
            waited = int(timeout - (deadline - time.monotonic()))
            return True, f"warmed up in ~{waited}s (ComfyUI {ver})"
    return False, f"launched but did not answer :{ccfg['port']} within {int(timeout)}s (see synthetrix-comfy.log)"


def object_counts(ccfg: dict) -> dict[str, int]:
    """How many models ComfyUI currently exposes for the common loaders."""
    out = {}
    probes = {"checkpoints": ("CheckpointLoaderSimple", "ckpt_name"),
              "loras": ("LoraLoader", "lora_name"),
              "vae": ("VAELoader", "vae_name")}
    for key, (node, field) in probes.items():
        try:
            with urllib.request.urlopen(base_url(ccfg) + f"/object_info/{node}", timeout=8) as r:
                d = json.load(r)
            enum = d[node]["input"]["required"][field][0]
            out[key] = len(enum) if isinstance(enum, list) else 0
        except Exception:
            out[key] = -1  # unknown / node missing
    return out
