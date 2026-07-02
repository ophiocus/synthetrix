#!/usr/bin/env python3
"""comfyctl - Synthetrix's ComfyUI runtime manager.

Makes Synthetrix authoritative over the runtime (venv, torch, ComfyUI, custom
nodes, paths, launch) the way it already is over models. The spine is `preflight`
- a blocking startup checklist that warms up the local ComfyUI and gates usage on
any stoppage.

  comfyctl preflight [--no-launch] [--json]   # full checklist (default); warms up ComfyUI
  comfyctl doctor [--json]                     # preflight, read-only (never launches)
  comfyctl probe [--json]                      # machine + runtime inventory
  comfyctl rules                               # hardware -> runtime compat verdict
  comfyctl launch | stop                       # server lifecycle
  comfyctl heal --paths                        # rewrite extra_model_paths.yaml to the SoT
  comfyctl provision [--paths|--nodes|--torch|--venv|--comfy] [--apply]

Exit codes (preflight/doctor): 0 ok/warn, 1 non-blocking fail, 2 BLOCKED.
"""
from __future__ import annotations

import argparse
import sys

from synthetrix.config import load_config
from synthetrix.comfy import preflight as PF, launch as L, provision as PV
from synthetrix.comfy.profile import probe, comfy_cfg
from synthetrix.comfy.rules import evaluate


def _preflight(cfg, args, allow_launch):
    rep = PF.run(cfg, allow_server_launch=allow_launch)
    print(rep.to_json() if args.json else PF.render(rep))
    return 2 if rep.blocked else (1 if rep.overall == "FAIL" else 0)


def cmd_preflight(cfg, args):
    return _preflight(cfg, args, allow_launch=not args.no_launch)


def cmd_doctor(cfg, args):
    return _preflight(cfg, args, allow_launch=False)


def cmd_probe(cfg, args):
    p = probe(cfg)
    if args.json:
        print(p.to_json())
    else:
        print(p.to_json())
    return 0


def cmd_rules(cfg, args):
    p = probe(cfg)
    s = evaluate(p)
    print(f"compatible: {s.compatible}")
    for r in s.reasons:
        print("  reason:", r)
    for w in s.warnings:
        print("  WARN  :", w)
    print(f"provision: {s.provision_index_url}  ({s.provision_pip})")
    print(f"launch_flags: {s.launch_flags}  precision: {s.precision_hint}")
    return 0


def cmd_launch(cfg, args):
    ccfg = comfy_cfg(cfg)
    spec = evaluate(probe(cfg))
    up, msg = L.warmup(ccfg, extra_flags=spec.launch_flags, launch=True)
    print(("UP: " if up else "FAILED: ") + msg)
    return 0 if up else 1


def cmd_stop(cfg, args):
    ccfg = comfy_cfg(cfg)
    ok, msg = PV.stop_server(ccfg)
    print(msg)
    return 0 if ok else 1


def cmd_heal(cfg, args):
    if not args.paths:
        print("nothing to heal (use --paths)")
        return 0
    r = PV.provision_paths(cfg, apply=True)
    print(r)
    return 0


def cmd_provision(cfg, args):
    return PV.run(cfg, args)


def main(argv=None):
    ap = argparse.ArgumentParser(prog="comfyctl", description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("preflight"); p.add_argument("--json", action="store_true")
    p.add_argument("--no-launch", action="store_true", help="don't start ComfyUI, just check")
    p.set_defaults(fn=cmd_preflight)

    p = sub.add_parser("doctor"); p.add_argument("--json", action="store_true")
    p.set_defaults(fn=cmd_doctor)

    p = sub.add_parser("probe"); p.add_argument("--json", action="store_true")
    p.set_defaults(fn=cmd_probe)

    p = sub.add_parser("rules"); p.set_defaults(fn=cmd_rules)
    p = sub.add_parser("launch"); p.set_defaults(fn=cmd_launch)
    p = sub.add_parser("stop"); p.set_defaults(fn=cmd_stop)

    p = sub.add_parser("heal"); p.add_argument("--paths", action="store_true")
    p.set_defaults(fn=cmd_heal)

    p = sub.add_parser("provision")
    for f in ("paths", "nodes", "torch", "venv", "comfy"):
        p.add_argument(f"--{f}", action="store_true")
    p.add_argument("--apply", action="store_true", help="actually run (default: dry-run)")
    p.set_defaults(fn=cmd_provision)

    args = ap.parse_args(argv)
    cfg = load_config()
    return args.fn(cfg, args)


if __name__ == "__main__":
    sys.exit(main())
