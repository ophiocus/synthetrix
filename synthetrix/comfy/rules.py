"""Hardware -> runtime compatibility rules engine (data-driven).

Two jobs:
  1. CHECK an existing runtime for compatibility with the detected GPU. The
     authoritative test is *arch-list membership*: the GPU's compute capability
     (e.g. sm_120) must appear in `torch.cuda.get_arch_list()`, AND CUDA must be
     available. This is version-agnostic — cu128 and cu130 both satisfy sm_120,
     so we never hardcode a single "blessed" torch build for the check.
  2. RECOMMEND, for provisioning a fresh/broken venv, a concrete torch build
     (index-url + pip spec) whose wheels are known to carry the GPU's arch, plus
     VRAM-derived launch flags and precision defaults.

Grounded on the reference box: RTX 5060 Ti 16 GB, Blackwell **sm_120**, needing
CUDA >= 12.8 wheels (cu128/cu130 arch lists include sm_120).
"""
from __future__ import annotations

from dataclasses import dataclass, field

from .profile import SystemProfile


# Minimum CUDA toolkit (as float) whose official torch wheels ship each arch.
# Used to pick a provisioning wheel and to sanity-note a mismatch.
ARCH_MIN_CUDA: dict[str, float] = {
    "sm_120": 12.8,   # Blackwell (RTX 50xx)
    "sm_100": 12.8,   # Blackwell datacenter
    "sm_90": 11.8,    # Hopper
    "sm_89": 11.8,    # Ada (RTX 40xx)
    "sm_86": 11.3,    # Ampere (RTX 30xx)
    "sm_80": 11.0,    # Ampere (A100)
    "sm_75": 10.2,    # Turing (RTX 20xx)
}

# Provisioning wheel per CUDA-family floor. index-url pins the wheel channel.
TORCH_CHANNELS: dict[float, dict] = {
    12.8: {"index_url": "https://download.pytorch.org/whl/cu128", "pip": "torch torchvision torchaudio"},
    11.8: {"index_url": "https://download.pytorch.org/whl/cu118", "pip": "torch torchvision torchaudio"},
    11.3: {"index_url": "https://download.pytorch.org/whl/cu113", "pip": "torch torchvision torchaudio"},
}


@dataclass
class RuntimeSpec:
    # compatibility verdict for the CURRENT venv vs the detected GPU
    compatible: bool
    reasons: list[str] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)
    # provisioning recommendation (for create/repair)
    provision_index_url: str | None = None
    provision_pip: str | None = None
    # runtime tuning
    launch_flags: list[str] = field(default_factory=list)
    precision_hint: str = "fp16"
    # venv policy
    want_no_user_site: bool = True


def _provision_for(sm: str | None) -> tuple[str | None, str | None]:
    if not sm:
        return None, None
    floor = ARCH_MIN_CUDA.get(sm, 12.8)
    # choose the closest channel <= a known family floor (>= the arch requirement)
    best = min((c for c in TORCH_CHANNELS if c >= floor), default=12.8)
    ch = TORCH_CHANNELS.get(best, TORCH_CHANNELS[12.8])
    return ch["index_url"], ch["pip"]


def _vram_tuning(vram_mb: int | None) -> tuple[list[str], str]:
    """(launch_flags, precision_hint) from VRAM. Conservative, ComfyUI auto-tunes
    a lot itself, so we only force flags at the extremes."""
    if not vram_mb:
        return [], "fp16"
    gb = vram_mb / 1024
    if gb < 6:
        return ["--lowvram"], "fp8/gguf"
    if gb < 10:
        return ["--normalvram"], "fp8 for >8GB models"
    if gb < 20:
        return [], "fp8 for >12GB models (WAN/Flux), fp16 otherwise"
    return [], "fp16"


def evaluate(profile: SystemProfile) -> RuntimeSpec:
    gpu, t = profile.gpu, profile.torch
    reasons: list[str] = []
    warnings: list[str] = []

    # ---- compatibility check (arch-list membership is authoritative) ----
    compatible = True
    if not gpu.present:
        compatible = False
        reasons.append("No NVIDIA GPU detected (nvidia-smi absent/failed).")
    if not t.present:
        compatible = False
        reasons.append("torch not importable in the venv.")
    elif not t.is_available:
        compatible = False
        reasons.append(f"torch {t.version} reports CUDA unavailable "
                       f"(cuda_build={t.cuda_build}).")
    elif gpu.compute_cap and gpu.compute_cap not in (t.arch_list or []):
        compatible = False
        reasons.append(f"GPU {gpu.compute_cap} NOT in torch arch list "
                       f"{t.arch_list} - kernels won't run on this card. "
                       f"Reinstall torch with CUDA >= "
                       f"{ARCH_MIN_CUDA.get(gpu.compute_cap, 12.8)} wheels.")
    if compatible:
        reasons.append(f"OK: {gpu.name} {gpu.compute_cap} in torch arch list; "
                       f"torch {t.version} (cuda {t.cuda_build}) CUDA-available.")

    # ---- warnings (non-blocking) ----
    if t.present and t.enable_user_site:
        warnings.append("venv has ENABLE_USER_SITE=True - a user-site torch could "
                        "shadow the venv's. Prefer an isolated venv (no_user_site).")
    if gpu.present and gpu.driver:
        try:
            if float(gpu.driver.split(".")[0]) < 525:
                warnings.append(f"driver {gpu.driver} is old for recent CUDA wheels.")
        except Exception:
            pass

    idx, pip = _provision_for(gpu.compute_cap)
    flags, prec = _vram_tuning(gpu.vram_mb)

    return RuntimeSpec(
        compatible=compatible,
        reasons=reasons,
        warnings=warnings,
        provision_index_url=idx,
        provision_pip=pip,
        launch_flags=flags,
        precision_hint=prec,
    )
