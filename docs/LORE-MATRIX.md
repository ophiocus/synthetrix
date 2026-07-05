# The Lore Matrix — the Forge, absorbed into Synthetrix

**Status:** proposed (2026-07-05). No files move until signed off (per `CONSOLIDATION.md` convention).
**Scope decision:** this **supersedes** `docs/PARITY-tinyforge.md`'s ruling that the
generation pipeline (notion → binding → image/mesh, burst grids, the treadmill,
Tripo 3D, game ingest) is *out of scope*. It is now **in scope and integral**:
Synthetrix owns the whole lore-matrix mechanism. "Anything forge" lives here.

---

## What the lore matrix mechanism is

A deterministic bridge from a **game notion** ("I need a full-body hero of this
character") to an **asset on disk** at the path the lore bible already specifies —
resolved from small hand-authored lookup tables, never hand-curated one-offs.

The canonical vocabulary (single name per concept — keep it stable):

| Term | Meaning |
|---|---|
| **notion** | *What* the game needs (e.g. `character-fullbody-hero`, `artifact-hero`, `environment-keyart`). A small fixed catalogue. |
| **binding** | notion → **recipe**: which Stage-2 model/params + optional Stage-4 mesh spec + output paths. Hand-authored, small. |
| **plan** | a binding + assembled prompt fragments → a concrete `GenerationPlan` (prompt, stage2, stage4, out paths). |
| **matrix / grid** | a bounded variation sweep over one notion (e.g. caste × build × sex = 32 cells). Multi-dimensional but declared, never exploded. |
| **burst** | one disciplined firing of a grid through a backend: logged, seeded (`sha256(burst_id+cell_id)`), resumable. |
| **stage** | one hop on the media bus: 2D image → 3D mesh → engine place, or text → voice. |
| **pipeline** | an ordered stage graph for one notion, its run + per-stage state recorded in `project.sqlite`. |
| **treadmill** | the cadenced batch loop over topic lanes (characters/props/weapons/mechs/worlds), 6 stages per batch. |
| **freeze** | a named, reproducible snapshot of the provisioned stack a burst/batch ran against. |
| **scenario** | a Scenario-DSL trial project (`.yml`) — a small multi-notion build used to exercise the ladder end to end. |

The design origin is the skeleton's **Four Matrices + Bindings** (`docs/forge/architecture.md`
in `lore-bible-skeleton`): notion × workflow/model-stack × prompt-map, meshed by a
bindings table. Combinations exist only where a binding declares them — the table
stays small, so it never explodes.

---

## Where "forge" lives today (the round-up)

Three implementations at three maturities, plus the design source. This is
everything that must converge here.

| Home | Lang | What it is | Disposition |
|---|---|---|---|
| **Synthetrix** `C:\Users\Carlos\synthetrix` | Rust | The umbrella app + the substrate (see below). Its `lore.rs` already names "the forge and prompt matrix" as what it feeds. | **THE HOME** — absorb everything into it |
| **`I:\Forge`** | Rust | Standalone "notion → binding → OpenArt 2D + Tripo 3D" orchestrator on `tinyassetcore`. Ships the exact layer Synthetrix lacks: `binding.rs`, `plan.rs`, `scenario.rs`, `bindings.example.toml`, `scenarios/*.yml`. Hosted-first (OpenArt). | **LIFT INTO Synthetrix**, then retire as standalone |
| **`F:\tinyforge`** | Python | Original design of the mechanism (treadmill / burst-fire / round-trip specs) — generation pipeline was always stubbed. Model domain already succeeded by Synthetrix (`docs/PARITY-tinyforge.md`). | **ARCHIVE** — design lineage only; harvest specs, don't run |
| **`lore-bible-skeleton/docs/forge/`** | docs | The generic Four-Matrix architecture + the artifact/character protocols that *define* notions. | **CANON SOURCE** — the spec every game inherits; stays in the skeleton |
| **`tinyassetcore`** `I:\TinyAssetCore` | Rust | Shared crate both `I:\Forge` and the Tiny* apps build on. Already owns the typed **`scenario`** model. | **SHARED TYPES** — the seam for binding/plan/scenario |

---

## What Synthetrix already has vs. what must come in

Verified against `app/src/` on 2026-07-05.

### HAVE (substrate is already here)
- **Lore ingest** — `lore.rs`: indexes the IP's lore-bible git repo into
  `project.sqlite` (`lore_index`); reader + vocabulary tiebreaker so generation
  speaks the IP's canonical terms.
- **Pipelines / stages** — `pipelines.rs`: `StageKind` = Image (ComfyUI) / Mesh
  (Tripo) / Voice (ElevenLabs) / Place (engine tree); run + per-stage state in
  `project.sqlite` (`pipelines` table). This *is* the treadmill's per-asset loop.
- **Backends** — `backends/{comfy_local,tripo,audio}` — live compute already wired.
- **Project DB** — `db.rs` + per-IP `.synthetrix/project.sqlite` (the real
  multidimensional-scenario database the mechanism always wanted).
- **Symbols already present:** `notion` (14 hits), `matrix` (14), `burst` (19),
  `grid` (5), `freeze` (12) across `app.rs / tabs.rs / worker.rs / project.rs`.

### NEED (the "forge" gaps — grep-confirmed absent in Synthetrix)
- **`binding` (0 hits)** — the notion → recipe layer. **Source: `I:\Forge\src\binding.rs`** (`Binding`, `Stage2`, `Stage4`) + `bindings.example.toml`.
- **`plan` assembly** — binding + fragments → `GenerationPlan`. **Source: `I:\Forge\src\plan.rs`**.
- **`scenario` (0 hits)** — the Scenario DSL. **Source: `I:\Forge\src\scenario.rs`** (I/O) + **`tinyassetcore::scenario`** (typed model) + `I:\Forge\scenarios\*.yml`.
- **`treadmill` (0 hits)** — the topic-lane batch cadence over the stage graph. **Source: design in `tinyforge/docs/treadmill.md`**; implement over the existing `pipelines` substrate.
- **Hosted 2D path** — OpenArt-as-backend. `I:\Forge` is OpenArt-first; Synthetrix is ComfyUI-first. Fold OpenArt in as a **backend** peer to `comfy_local`.

### RECONCILE (same word, verify same meaning)
Synthetrix already has `burst` / `grid` / `matrix` / `freeze` symbols, but their
current semantics must be checked against the forge's before merging:
- **`freeze`** — in Synthetrix today = model `lock` / snapshot (per `PARITY`). The
  forge's `freeze` = provisioned-stack snapshot a batch ran against. Likely the
  same idea at different granularity — unify, don't duplicate.
- **`burst` / `grid` / `matrix`** — confirm these are generation-variation
  concepts (not unrelated UI grids) before wiring the binding/plan layer to them.

---

## Target architecture (one subsystem, one DB)

```
  lore-bible repo (IP canon, git)                 Synthetrix (Rust app)
   world/ factions/ characters/ …   ── lore.rs ──▶ project.sqlite: lore_index
   notions defined by the protocols                        │
                                                            ▼
   bindings (notion → recipe)  ────────────────▶  binding + plan   (from I:\Forge)
        │                                                   │
        ▼                                                   ▼
   matrix/grid (variation cells) ─▶ burst (seeded, logged, resumable)
        │                                                   │
        ▼                                                   ▼
   pipeline stages ── Image ─▶ Mesh ─▶ Voice ─▶ Place ──▶ backends/
   (per-asset loop)   Comfy    Tripo   11Labs   engine     comfy_local | openart | tripo | audio
        │                                                   │
        ▼                                                   ▼
   treadmill (topic-lane cadence)                   freeze (reproducible snapshot)
        └──────────── all run/state recorded in project.sqlite ───────────┘
```

One home (Synthetrix), one database (`project.sqlite`), one vocabulary. The Scenario
DSL drives trial builds through the same path (typed model shared via `tinyassetcore`).

---

## Execution plan (proposed — sign off before any move)

Each step is independently verifiable; nothing irreversible until the last.

- **Step 1 — Land the shared types in `tinyassetcore`.** Move `Binding/Stage2/Stage4`
  (+ the `GenerationPlan` shape) beside the existing `tinyassetcore::scenario` so
  Synthetrix and any future consumer share one definition. `I:\Forge` keeps
  compiling against them (it already depends on `tinyassetcore`).
- **Step 2 — Add `binding` + `plan` modules to Synthetrix** (`app/src/binding.rs`,
  `app/src/plan.rs`), thin over the shared types. Load bindings from the IP repo
  (TOML) and resolve a notion → `GenerationPlan`. Closes `binding = 0`.
- **Step 3 — Wire plan → pipeline.** A resolved `GenerationPlan` emits pipeline
  stages onto the existing `pipelines` substrate (Image/Mesh/Place), so a notion
  fires through the backends Synthetrix already has.
- **Step 4 — OpenArt backend.** Add `backends/openart.rs` (lift `I:\Forge`'s hosted
  Stage-2 path) as a peer to `comfy_local`, so a binding can target hosted *or*
  local per the class split (`compare within a class, not across`).
- **Step 5 — Scenario DSL.** Add `app/src/scenario.rs` (lift `I:\Forge`'s loader),
  read `scenarios/*.yml`, run a trial project end-to-end through Steps 2–4.
- **Step 6 — Treadmill.** Implement the topic-lane batch cadence over the pipeline
  substrate; reconcile forge-`freeze` with Synthetrix-`freeze`. Closes `treadmill = 0`.
- **Step 7 — Retire `I:\Forge` as a standalone.** Once Steps 2–6 pass, `I:\Forge`
  is fully subsumed; archive it (its bindings/scenarios become Synthetrix fixtures /
  examples). Update `docs/PARITY-tinyforge.md` to record the scope reversal.

**Rollback:** Steps 1–6 only *add* to Synthetrix and share types; `I:\Forge` keeps
working throughout. Step 7 (archive) is the only irreversible step and runs last,
after the trial scenario is green inside Synthetrix.

---

## Supersedes / cross-refs

- **Supersedes:** `docs/PARITY-tinyforge.md` §Acceptance criteria + §Conclusion
  (the "generation pipeline OUT OF SCOPE / tinyforge safely retired" ruling). Parity
  for the *model domain* still stands; the *generation domain* is now in scope here.
- **Design source:** `lore-bible-skeleton/docs/forge/architecture.md` (Four Matrices),
  `tinyforge/docs/{treadmill,burst-fire-protocol,round-trip-runs}.md` (specs, archived).
- **Adjacent:** `CONSOLIDATION.md` (model vault/runtime — the storage layer this
  generation layer runs on), `AIPROD_CORRELATIONS.md`.
