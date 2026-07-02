"""Synthetrix ComfyUI runtime manager (`comfyctl`).

Makes Synthetrix authoritative over the *runtime* (venv, torch, ComfyUI install,
custom nodes, launch, paths) the same way it is authoritative over models (vault,
NVMe tier, catalog). The spine is a blocking **preflight** that runs on startup:
it walks a checklist from "venv exists" all the way to "manifest audit", warms up
the local ComfyUI, and flags any stoppage as a priority gate.

Layout:
  profile.py    - probe the machine (GPU/arch/VRAM/driver, venv torch, install)
  rules.py      - hardware->runtime compatibility rules engine (data-driven)
  checks.py     - individual checks -> CheckResult (OK/WARN/FAIL + fix hint)
  preflight.py  - ordered checklist orchestrator + blocking status
  launch.py     - warmup/start/stop the local ComfyUI on the managed venv
  provision.py  - venv/torch/comfy/node install + heal (mutating; guarded)
  paths.py      - 3-layer path authority + extra_model_paths writer
"""
