"""Preflight: the ordered, blocking startup checklist.

Runs on Synthetrix startup. Walks from "config" through "manifest audit", warms
up the local ComfyUI as the target, and returns a verdict. Any blocking FAIL
means related Synthetrix usage should be gated until it's resolved - the first
blocker is the priority to fix.

Fundamentals gate the rest: if venv/torch/install/paths fail, the server warmup
and model-visibility checks are SKIPPED (no point launching a broken runtime).
"""
from __future__ import annotations

import json
import time
from dataclasses import dataclass, asdict, field

from . import checks
from .checks import CheckResult, OK, WARN, FAIL, SKIP
from .profile import probe, comfy_cfg
from .rules import evaluate


@dataclass
class PreflightReport:
    overall: str                       # OK | WARN | FAIL | BLOCKED
    blocked: bool
    results: list[CheckResult] = field(default_factory=list)
    blockers: list[str] = field(default_factory=list)
    elapsed_s: float = 0.0

    def to_json(self) -> str:
        d = asdict(self)
        return json.dumps(d, indent=2)


def _skip(key: str, title: str, why: str) -> CheckResult:
    return CheckResult(key, title, SKIP, why)


def run(cfg: dict, allow_server_launch: bool = True) -> PreflightReport:
    t0 = time.monotonic()
    ccfg = comfy_cfg(cfg)
    profile = probe(cfg)
    spec = evaluate(profile)

    results: list[CheckResult] = []
    blocked = False

    # Stage 1: static fundamentals (no side effects).
    for r in (
        checks.check_config(cfg),
        checks.check_gpu(profile),
        checks.check_venv(profile),
        checks.check_torch(profile, spec),
        checks.check_comfy_install(profile),
        checks.check_paths(cfg, ccfg),
        checks.check_custom_nodes(ccfg),
    ):
        results.append(r)
        if r.status == FAIL and r.blocking:
            blocked = True

    # Stage 2: server warmup (mutating) - only if fundamentals are sound.
    if blocked:
        results.append(_skip("server", "ComfyUI server", "skipped - resolve blocking failures first"))
        results.append(_skip("visibility", "Model visibility", "skipped - server not warmed"))
    else:
        srv = checks.check_server(ccfg, spec, allow_server_launch)
        results.append(srv)
        if srv.status == FAIL and srv.blocking:
            blocked = True
        if srv.status == OK:
            vis = checks.check_model_visibility(ccfg)
            results.append(vis)
            if vis.status == FAIL and vis.blocking:
                blocked = True
        else:
            results.append(_skip("visibility", "Model visibility", "skipped - server not up"))

    # Stage 3: manifest audit (independent of the server).
    mani = checks.check_manifest(cfg)
    results.append(mani)
    if mani.status == FAIL and mani.blocking:
        blocked = True

    # verdict
    statuses = [r.status for r in results]
    if blocked:
        overall = "BLOCKED"
    elif FAIL in statuses:
        overall = "FAIL"
    elif WARN in statuses:
        overall = "WARN"
    else:
        overall = "OK"

    blockers = [f"{r.title}: {r.message}" for r in results if r.status == FAIL and r.blocking]
    return PreflightReport(overall=overall, blocked=blocked, results=results,
                           blockers=blockers, elapsed_s=round(time.monotonic() - t0, 1))


# ---- console rendering ------------------------------------------------------
_GLYPH = {OK: "[ OK ]", WARN: "[WARN]", FAIL: "[FAIL]", SKIP: "[skip]"}


def render(report: PreflightReport) -> str:
    lines = [f"Synthetrix preflight - {report.overall}  ({report.elapsed_s}s)"]
    lines.append("=" * 60)
    for r in report.results:
        lines.append(f"{_GLYPH.get(r.status, '[?]')} {r.title}")
        lines.append(f"       {r.message}")
        if r.status in (FAIL, WARN) and r.fix:
            lines.append(f"       fix: {r.fix}")
    lines.append("=" * 60)
    if report.blocked:
        lines.append("BLOCKED - resolve these before using ComfyUI-dependent features:")
        for b in report.blockers:
            lines.append(f"  * {b}")
    else:
        lines.append("Runtime ready." if report.overall in (OK, WARN) else "Non-blocking issues above.")
    return "\n".join(lines)
