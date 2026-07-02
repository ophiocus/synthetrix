"""Individual preflight checks. Each returns a CheckResult; the preflight
orchestrator runs them in order and decides the blocking verdict.
"""
from __future__ import annotations

import sqlite3
from dataclasses import dataclass
from pathlib import Path

from . import launch, paths
from .profile import SystemProfile
from .rules import RuntimeSpec

OK, WARN, FAIL, SKIP = "OK", "WARN", "FAIL", "SKIP"


@dataclass
class CheckResult:
    key: str
    title: str
    status: str          # OK | WARN | FAIL | SKIP
    message: str
    fix: str = ""        # remediation hint (or the comfyctl subcommand that fixes it)
    blocking: bool = False   # a FAIL here should gate related Synthetrix usage


# ---- checks (ordered as the preflight will run them) ------------------------
def check_config(cfg: dict) -> CheckResult:
    st = cfg.get("storage") or {}
    need = ["vault_root", "catalog_dir", "nvme_root"]
    miss = [k for k in need if not st.get(k)]
    if miss:
        return CheckResult("config", "Config source-of-truth", FAIL,
                           f"missing [storage] keys: {miss}", "edit config.toml", True)
    return CheckResult("config", "Config source-of-truth", OK,
                       f"vault={st['vault_root']} nvme={st['nvme_root']}")


def check_gpu(p: SystemProfile) -> CheckResult:
    if not p.gpu.present:
        return CheckResult("gpu", "GPU detected", FAIL, p.gpu.raw or "no NVIDIA GPU",
                           "install NVIDIA driver / check nvidia-smi", True)
    return CheckResult("gpu", "GPU detected", OK,
                       f"{p.gpu.name} {p.gpu.compute_cap} {p.gpu.vram_mb}MB, driver {p.gpu.driver}")


def check_venv(p: SystemProfile) -> CheckResult:
    if not p.install.venv_python_ok:
        return CheckResult("venv", "ComfyUI venv", FAIL,
                           f"venv python missing: {p.install.venv}",
                           "comfyctl provision --venv", True)
    return CheckResult("venv", "ComfyUI venv", OK, f"venv ok ({p.install.venv})")


def check_torch(p: SystemProfile, spec: RuntimeSpec) -> CheckResult:
    if not spec.compatible:
        return CheckResult("torch", "torch <-> GPU compatibility", FAIL,
                           " ".join(spec.reasons),
                           "comfyctl provision --torch", True)
    msg = f"torch {p.torch.version} (cuda {p.torch.cuda_build}); {p.gpu.compute_cap} in arch list"
    if spec.warnings:
        return CheckResult("torch", "torch <-> GPU compatibility", WARN,
                           msg + " | " + " ".join(spec.warnings),
                           "recreate venv with --no-user-site")
    return CheckResult("torch", "torch <-> GPU compatibility", OK, msg)


def check_comfy_install(p: SystemProfile) -> CheckResult:
    if not p.install.main_py:
        return CheckResult("install", "ComfyUI install", FAIL,
                           f"main.py missing under {p.install.comfy_root}",
                           "comfyctl provision --comfy", True)
    return CheckResult("install", "ComfyUI install", OK,
                       f"{p.install.comfy_root} @ {p.install.git_rev}")


def check_paths(cfg: dict, ccfg: dict) -> CheckResult:
    st = cfg.get("storage") or {}
    nvme = st.get("nvme_root", "")
    yaml = str(Path(ccfg["comfy_root"]) / "extra_model_paths.yaml")
    agree, msg = paths.check_agreement(nvme, yaml)
    missing_nvme = paths.dirs_exist(nvme)
    if not agree:
        return CheckResult("paths", "Model-path authority (3-layer)", FAIL, msg,
                           "comfyctl heal --paths (rewrites extra_model_paths.yaml)", True)
    extra = f"; missing nvme subdirs: {missing_nvme}" if missing_nvme else ""
    status = WARN if missing_nvme else OK
    return CheckResult("paths", "Model-path authority (3-layer)", status, msg + extra,
                       "comfyctl heal --paths" if missing_nvme else "")


# ComfyUI custom-node packs commonly required by harvested CivitAI workflows.
# dir-name (in custom_nodes/) -> human label.
RECOMMENDED_NODES = {
    "efficiency-nodes-comfyui": "Efficiency Nodes",
    "ComfyUI-Florence2": "Florence2",
    "ComfyUI-Easy-Use": "Easy-Use",
    "comfyui-art-venture": "ArtVenture",
    "SeargeSDXL": "Searge SDXL",
    "comfyui-various": "JW / various",
    "ComfyMath": "ComfyMath",
    "ComfyUI-Inspire-Pack": "Inspire Pack",
    "ComfyUI_OneButtonPrompt": "OneButtonPrompt",
    "comfyui-lora-tag-loader": "LoRA Tag Loader",
}


def check_custom_nodes(ccfg: dict) -> CheckResult:
    cn = Path(ccfg["comfy_root"]) / "custom_nodes"
    have = {d.name for d in cn.iterdir() if d.is_dir()} if cn.is_dir() else set()
    missing = {k: v for k, v in RECOMMENDED_NODES.items() if k not in have}
    if missing:
        return CheckResult("nodes", "Custom node packs", WARN,
                           f"{len(have)} installed; missing {len(missing)}: "
                           f"{', '.join(missing.values())}",
                           "comfyctl provision --nodes")
    return CheckResult("nodes", "Custom node packs", OK, f"{len(have)} installed, recommended set present")


def check_server(ccfg: dict, spec: RuntimeSpec, launch_ok: bool) -> CheckResult:
    up, msg = launch.warmup(ccfg, extra_flags=spec.launch_flags, launch=launch_ok)
    if not up:
        return CheckResult("server", "ComfyUI server (:%s)" % ccfg["port"], FAIL, msg,
                           "comfyctl launch", True)
    return CheckResult("server", "ComfyUI server (:%s)" % ccfg["port"], OK, msg)


def check_model_visibility(ccfg: dict) -> CheckResult:
    counts = launch.object_counts(ccfg)
    ck = counts.get("checkpoints", -1)
    if ck < 0:
        return CheckResult("visibility", "Model visibility", WARN,
                           "could not query /object_info (server not ready?)", "")
    if ck == 0:
        return CheckResult("visibility", "Model visibility", FAIL,
                           "ComfyUI sees 0 checkpoints - promotes not landing in the read tree",
                           "comfyctl heal --paths; promote a model", True)
    return CheckResult("visibility", "Model visibility", OK,
                       f"ckpt={counts.get('checkpoints')} lora={counts.get('loras')} vae={counts.get('vae')}")


def check_manifest(cfg: dict) -> CheckResult:
    db = Path(cfg.get("storage", {}).get("catalog_dir", "")) / "catalog.sqlite"
    if not db.exists():
        return CheckResult("manifest", "Catalog / manifest audit", FAIL,
                           f"catalog missing: {db}", "run the harvester (build_index.py)", True)
    try:
        con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        integ = con.execute("PRAGMA integrity_check").fetchone()[0]
        nf = con.execute("SELECT COUNT(*) FROM files").fetchone()[0]
        nm = con.execute("SELECT COUNT(*) FROM models").fetchone()[0]
        # sample: promoted rows whose nvme replica is gone
        bad = con.execute(
            "SELECT COUNT(*) FROM files WHERE status='promoted' AND nvme_path IS NOT NULL "
            "AND nvme_path NOT LIKE ''").fetchone()[0]
        con.close()
    except Exception as e:
        return CheckResult("manifest", "Catalog / manifest audit", FAIL,
                           f"catalog read failed: {e}", "", True)
    if integ != "ok":
        return CheckResult("manifest", "Catalog / manifest audit", FAIL,
                           f"integrity_check: {integ}", "", True)
    return CheckResult("manifest", "Catalog / manifest audit", OK,
                       f"integrity ok; {nf} files / {nm} models (promoted rows: {bad})")
