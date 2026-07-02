"""Provision / heal the ComfyUI runtime. Mutating ops are DRY-RUN unless --apply.

Scope (each independently runnable):
  --paths  rewrite extra_model_paths.yaml to the config SoT + make nvme subdirs (SAFE)
  --nodes  git-clone the recommended custom-node packs that are missing
  --torch  (re)install torch from the rules-selected CUDA channel into the venv
  --venv   create the isolated venv + install ComfyUI requirements
  --comfy  git-clone ComfyUI if the install is missing

Heavy ops (venv/torch/nodes) shell out to git/pip; they never touch app/src.
"""
from __future__ import annotations

import subprocess
from pathlib import Path

from . import paths as PA
from .profile import comfy_cfg, venv_python, probe
from .rules import evaluate
from .checks import RECOMMENDED_NODES

# dir-name -> git repo for the recommended packs
NODE_REPOS = {
    "efficiency-nodes-comfyui": "https://github.com/jags111/efficiency-nodes-comfyui",
    "ComfyUI-Florence2": "https://github.com/kijai/ComfyUI-Florence2",
    "ComfyUI-Easy-Use": "https://github.com/yolain/ComfyUI-Easy-Use",
    "comfyui-art-venture": "https://github.com/sipherxyz/comfyui-art-venture",
    "SeargeSDXL": "https://github.com/SeargeDP/SeargeSDXL",
    "comfyui-various": "https://github.com/jamesWalker55/comfyui-various",
    "ComfyMath": "https://github.com/evanspearman/ComfyMath",
    "ComfyUI-Inspire-Pack": "https://github.com/ltdrdata/ComfyUI-Inspire-Pack",
    "ComfyUI_OneButtonPrompt": "https://github.com/AIrjen/OneButtonPrompt",
    "comfyui-lora-tag-loader": "https://github.com/badjeff/comfyui_lora_tag_loader",
}
COMFY_REPO = "https://github.com/comfyanonymous/ComfyUI"


def _do(cmd: list[str], apply: bool, cwd: str | None = None, log: list | None = None) -> bool:
    line = ("RUN  " if apply else "PLAN ") + " ".join(cmd) + (f"   (cwd={cwd})" if cwd else "")
    if log is not None:
        log.append(line)
    print(line)
    if not apply:
        return True
    try:
        p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=1800)
        if p.returncode != 0:
            print("   ! failed:", (p.stderr or p.stdout).strip()[:300])
            return False
        return True
    except Exception as e:
        print("   ! error:", e)
        return False


# ---- individual provisioners ------------------------------------------------
def provision_paths(cfg: dict, apply: bool = False) -> str:
    """Rewrite extra_model_paths.yaml to match nvme_root, and create missing nvme
    subdirs. Safe/idempotent."""
    ccfg = comfy_cfg(cfg)
    nvme = cfg.get("storage", {}).get("nvme_root", "")
    yaml = str(Path(ccfg["comfy_root"]) / "extra_model_paths.yaml")
    agree, msg = PA.check_agreement(nvme, yaml)
    made = []
    if apply:
        PA.write_yaml(nvme, yaml)
        for sub in PA.dirs_exist(nvme):
            Path(nvme, sub).mkdir(parents=True, exist_ok=True)
            made.append(sub)
        return f"wrote {yaml} (base_path -> {PA.nvme_base(nvme)}); created subdirs {made or 'none'}"
    return f"[dry-run] would rewrite {yaml}: {msg}; missing subdirs {PA.dirs_exist(nvme)}"


def provision_nodes(cfg: dict, apply: bool = False) -> str:
    ccfg = comfy_cfg(cfg)
    cn = Path(ccfg["comfy_root"]) / "custom_nodes"
    have = {d.name for d in cn.iterdir() if d.is_dir()} if cn.is_dir() else set()
    missing = [k for k in RECOMMENDED_NODES if k not in have]
    if not missing:
        return "all recommended node packs already present"
    py = venv_python(ccfg["venv"])
    for name in missing:
        url = NODE_REPOS.get(name)
        if not url:
            print("PLAN  (no repo mapped for", name, "- skip)")
            continue
        _do(["git", "clone", "--depth", "1", url, str(cn / name)], apply)
        req = cn / name / "requirements.txt"
        if apply and req.exists():
            _do([str(py), "-m", "pip", "install", "-r", str(req)], apply)
    return f"{'installed' if apply else 'planned'} {len(missing)} node pack(s): {missing}"


def provision_torch(cfg: dict, apply: bool = False) -> str:
    ccfg = comfy_cfg(cfg)
    spec = evaluate(probe(cfg))
    if spec.compatible and apply:
        return "torch already compatible - skipping reinstall (use --venv to force a rebuild)"
    py = venv_python(ccfg["venv"])
    if not py.exists():
        return f"venv missing ({py}); run provision --venv first"
    cmd = [str(py), "-m", "pip", "install", "--force-reinstall",
           "--index-url", spec.provision_index_url or "https://download.pytorch.org/whl/cu128"]
    cmd += (spec.provision_pip or "torch torchvision torchaudio").split()
    ok = _do(cmd, apply)
    return f"{'installed' if apply and ok else 'planned'} torch from {spec.provision_index_url}"


def provision_venv(cfg: dict, apply: bool = False) -> str:
    import sys
    ccfg = comfy_cfg(cfg)
    venv = Path(ccfg["venv"])
    root = Path(ccfg["comfy_root"])
    if venv.exists():
        return f"venv already exists ({venv}); use --torch to fix torch, or delete it to rebuild"
    _do([sys.executable, "-m", "venv", str(venv)], apply)
    py = venv_python(str(venv))
    _do([str(py), "-m", "pip", "install", "--upgrade", "pip"], apply)
    provision_torch(cfg, apply)
    if (root / "requirements.txt").exists():
        _do([str(py), "-m", "pip", "install", "-r", str(root / "requirements.txt")], apply)
    return f"{'created' if apply else 'planned'} venv at {venv} (launch with PYTHONNOUSERSITE=1)"


def provision_comfy(cfg: dict, apply: bool = False) -> str:
    ccfg = comfy_cfg(cfg)
    root = Path(ccfg["comfy_root"])
    if (root / "main.py").exists():
        return f"ComfyUI already present at {root}"
    _do(["git", "clone", COMFY_REPO, str(root)], apply)
    return f"{'cloned' if apply else 'planned'} ComfyUI into {root}"


# ---- lifecycle --------------------------------------------------------------
def stop_server(ccfg: dict) -> tuple[bool, str]:
    """Best-effort: kill whatever listens on the ComfyUI port (Windows netstat)."""
    import os
    if os.name != "nt":
        return False, "stop only implemented for Windows"
    port = str(ccfg["port"])
    try:
        out = subprocess.run(["netstat", "-ano"], capture_output=True, text=True, timeout=15).stdout
    except Exception as e:
        return False, f"netstat failed: {e}"
    pids = set()
    for ln in out.splitlines():
        if f":{port} " in ln and "LISTENING" in ln:
            pids.add(ln.split()[-1])
    if not pids:
        return True, f"nothing listening on :{port}"
    killed = []
    for pid in pids:
        r = subprocess.run(["taskkill", "/PID", pid, "/F"], capture_output=True, text=True)
        if r.returncode == 0:
            killed.append(pid)
    return (bool(killed), f"stopped pid(s) {killed}" if killed else f"failed to kill {pids}")


# ---- CLI dispatch -----------------------------------------------------------
def run(cfg: dict, args) -> int:
    any_flag = any(getattr(args, f) for f in ("paths", "nodes", "torch", "venv", "comfy"))
    if not any_flag:
        print("specify what to provision: --paths --nodes --torch --venv --comfy  [--apply]")
        return 1
    if not args.apply:
        print("=== DRY-RUN (add --apply to execute) ===")
    if args.comfy:
        print(">>", provision_comfy(cfg, args.apply))
    if args.venv:
        print(">>", provision_venv(cfg, args.apply))
    if args.torch:
        print(">>", provision_torch(cfg, args.apply))
    if args.nodes:
        print(">>", provision_nodes(cfg, args.apply))
    if args.paths:
        print(">>", provision_paths(cfg, args.apply))
    return 0
