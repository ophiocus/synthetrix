"""Probe the machine into a structured SystemProfile.

comfyctl runs under the *system* Python and manages the *target* venv from the
outside (so it can create/repair it). Torch facts are therefore gathered by
shelling out to the venv's own python, never by importing torch here.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass, field, asdict
from pathlib import Path


# ---- resolved runtime layout ------------------------------------------------
# The RUNNABLE ComfyUI is E:\ComfyUI; the model tier it reads (via
# extra_model_paths.yaml base_path) is E:\model loader\ComfyUI\models — a
# separate tree. Keep them distinct.
def comfy_cfg(cfg: dict) -> dict:
    """Resolve the [comfy] runtime block, with defaults, from the loaded config."""
    c = dict(cfg.get("comfy") or {})
    c.setdefault("comfy_root", r"E:\ComfyUI")
    root = Path(c["comfy_root"])
    c.setdefault("venv", str(root / "venv"))
    c.setdefault("host", "127.0.0.1")
    c.setdefault("port", 8188)
    c.setdefault("launch_args", [])          # extra args appended after the rules' args
    return c


def venv_python(venv: str) -> Path:
    v = Path(venv)
    win = v / "Scripts" / "python.exe"
    return win if win.exists() else v / "bin" / "python"


# ---- data model -------------------------------------------------------------
@dataclass
class GpuInfo:
    present: bool = False
    name: str | None = None
    vram_mb: int | None = None
    driver: str | None = None
    compute_cap: str | None = None      # e.g. "sm_120" (derived from "12.0")
    raw: str | None = None


@dataclass
class TorchInfo:
    present: bool = False
    version: str | None = None          # e.g. "2.12.1+cu130"
    cuda_build: str | None = None       # e.g. "13.0"
    is_available: bool = False
    arch_list: list[str] = field(default_factory=list)
    device_name: str | None = None
    device_cap: str | None = None       # e.g. "sm_120"
    enable_user_site: bool | None = None
    py: str | None = None
    error: str | None = None


@dataclass
class InstallInfo:
    comfy_root: str = ""
    main_py: bool = False
    venv: str = ""
    venv_python_ok: bool = False
    git_rev: str | None = None
    frontend_pkg: bool = False
    extra_model_paths: bool = False


@dataclass
class SystemProfile:
    os: str
    system_python: str
    ram_mb: int | None
    disk_free_gb: dict[str, float]
    gpu: GpuInfo
    torch: TorchInfo
    install: InstallInfo

    def to_json(self) -> str:
        return json.dumps(asdict(self), indent=2)


# ---- probes -----------------------------------------------------------------
def _run(args: list[str], timeout: int = 30) -> tuple[int, str, str]:
    try:
        p = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
        return p.returncode, p.stdout.strip(), p.stderr.strip()
    except Exception as e:  # FileNotFoundError, TimeoutExpired, ...
        return 127, "", str(e)


def probe_gpu() -> GpuInfo:
    exe = shutil.which("nvidia-smi")
    if not exe:
        return GpuInfo(present=False, raw="nvidia-smi not found")
    code, out, err = _run([exe, "--query-gpu=name,memory.total,driver_version,compute_cap",
                           "--format=csv,noheader,nounits"])
    if code != 0 or not out:
        return GpuInfo(present=False, raw=err or out or "nvidia-smi failed")
    # first GPU line: "NVIDIA GeForce RTX 5060 Ti, 16311, 595.79, 12.0"
    first = out.splitlines()[0]
    parts = [p.strip() for p in first.split(",")]
    name = parts[0] if parts else None
    vram = int(parts[1]) if len(parts) > 1 and parts[1].isdigit() else None
    driver = parts[2] if len(parts) > 2 else None
    cap = parts[3] if len(parts) > 3 else None
    sm = None
    if cap and "." in cap:
        major, minor = cap.split(".", 1)
        sm = f"sm_{major}{minor}"
    return GpuInfo(present=True, name=name, vram_mb=vram, driver=driver,
                   compute_cap=sm, raw=first)


_TORCH_PROBE = r"""
import sys, json
o = {"present": False}
try:
    import torch
    o["present"] = True
    o["version"] = torch.__version__
    o["cuda_build"] = torch.version.cuda
    o["is_available"] = bool(torch.cuda.is_available())
    try: o["arch_list"] = list(torch.cuda.get_arch_list())
    except Exception: o["arch_list"] = []
    if o["is_available"]:
        o["device_name"] = torch.cuda.get_device_name(0)
        c = torch.cuda.get_device_capability(0)
        o["device_cap"] = "sm_%d%d" % (c[0], c[1])
    o["enable_user_site"] = (sys.flags.no_user_site == 0)
    o["py"] = sys.version.split()[0]
except Exception as e:
    o["error"] = repr(e)
print(json.dumps(o))
"""


def probe_torch(venv: str) -> TorchInfo:
    py = venv_python(venv)
    if not py.exists():
        return TorchInfo(present=False, error=f"venv python missing: {py}")
    code, out, err = _run([str(py), "-c", _TORCH_PROBE], timeout=60)
    if code != 0 or not out:
        return TorchInfo(present=False, error=(err or "torch probe failed")[:400])
    try:
        d = json.loads(out.splitlines()[-1])
    except Exception as e:
        return TorchInfo(present=False, error=f"probe parse: {e}: {out[:200]}")
    defaults = TorchInfo().__dict__
    return TorchInfo(**{k: d.get(k, defaults[k]) for k in defaults})


def probe_install(ccfg: dict) -> InstallInfo:
    root = Path(ccfg["comfy_root"])
    py = venv_python(ccfg["venv"])
    git_rev = None
    if (root / ".git").exists():
        code, out, _ = _run(["git", "-C", str(root), "rev-parse", "--short", "HEAD"])
        git_rev = out if code == 0 else None
    # frontend pkg (where the bridge extension lives)
    fe = list(Path(ccfg["venv"]).glob(
        "Lib/site-packages/comfyui_frontend_package")) if Path(ccfg["venv"]).exists() else []
    return InstallInfo(
        comfy_root=str(root),
        main_py=(root / "main.py").exists(),
        venv=ccfg["venv"],
        venv_python_ok=py.exists(),
        git_rev=git_rev,
        frontend_pkg=bool(fe),
        extra_model_paths=(root / "extra_model_paths.yaml").exists(),
    )


def _disk_free(paths: list[str]) -> dict[str, float]:
    out = {}
    seen = set()
    for p in paths:
        try:
            drive = os.path.splitdrive(os.path.abspath(p))[0] or p
            if drive in seen:
                continue
            seen.add(drive)
            total, used, free = shutil.disk_usage(drive + os.sep if drive.endswith(":") else p)
            out[drive] = round(free / 1024**3, 1)
        except Exception:
            pass
    return out


def _ram_mb() -> int | None:
    try:
        if os.name == "nt":
            import ctypes

            class MS(ctypes.Structure):
                _fields_ = [("dwLength", ctypes.c_ulong), ("dwMemoryLoad", ctypes.c_ulong),
                            ("ullTotalPhys", ctypes.c_ulonglong), ("ullAvailPhys", ctypes.c_ulonglong),
                            ("ullTotalPageFile", ctypes.c_ulonglong), ("ullAvailPageFile", ctypes.c_ulonglong),
                            ("ullTotalVirtual", ctypes.c_ulonglong), ("ullAvailVirtual", ctypes.c_ulonglong),
                            ("ullAvailExtendedVirtual", ctypes.c_ulonglong)]
            m = MS(); m.dwLength = ctypes.sizeof(MS)
            ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(m))
            return int(m.ullTotalPhys / 1024**2)
    except Exception:
        return None
    return None


def probe(cfg: dict) -> SystemProfile:
    ccfg = comfy_cfg(cfg)
    st = cfg.get("storage") or {}
    disk_paths = [ccfg["comfy_root"], st.get("vault_root", "H:/Models"),
                  st.get("nvme_root", "E:/model loader/ComfyUI/models")]
    return SystemProfile(
        os=f"{os.name} {sys.platform}",
        system_python=sys.version.split()[0],
        ram_mb=_ram_mb(),
        disk_free_gb=_disk_free(disk_paths),
        gpu=probe_gpu(),
        torch=probe_torch(ccfg["venv"]),
        install=probe_install(ccfg),
    )
