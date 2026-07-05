# Prompt Generation — lore records → factual prompts

**Status:** proposed (2026-07-05). Detailed design for the `plan → prompt` portion
of [LORE-MATRIX.md](LORE-MATRIX.md) (its Steps 2–3). No code lands until signed off.
**Thesis:** a **factual prompt** is one where *every descriptive clause is a canon
fact* — drawn from a specific lore record, assembled deterministically into the
canonical bracket template, in the IP's controlled vocabulary, with the species/world
**locked constants** always present, and **nothing invented**. The output is not just
a prompt string; it is a prompt string **plus a trace table** proving each clause's
provenance.

---

## Why rehash — the current path is disjoint and manual

| Step | Today | Problem |
|---|---|---|
| Fetch | `lore.rs::scan/extract` indexes each doc to (title, first-paragraph summary, ≤14 bold-span vocab) → `lore_index` | A **browse index**, not a fact model. Physical description, materials, wear, palette, function are lost. |
| Prompt | Human hand-writes `prompts.md` per the bracket template; `worker.rs::import_prompts` → `parse_prompts_md` → `PromptRow.body` | The prompt is **opaque text**. No provenance; can't audit "is every clause factual?"; drift is invisible. |
| Assemble | `I:\Forge::plan.rs` = `fragments.join(", ")` | No lore grounding at all — fragments come from the caller. |
| Factual apparatus | bracket template + controlled vocabulary + locked constants + faction palette | Exists only in **protocol docs + human discipline** (`agent_roles/*-creation-protocol.md`, `concepts/vocabulary.md`, the Inheritor testbed). Nothing enforces it in code. |

Net: there is **no automated lore→prompt path**, and the prompt body carries no
evidence that it is faithful to canon. The rehash makes the factual apparatus a
real subsystem and makes provenance a first-class output.

---

## The rehashed pipeline: F → E → B → A → T

```
  lore-bible repo            project.sqlite                 the factual prompt
  ┌───────────┐   Fetch   ┌──────────────┐  Extract  ┌──────────┐
  │ profile.md│ ────────▶ │ lore_records │ ────────▶ │ FactSet  │  (typed, each fact
  │ world/…   │           │  (structured)│           │          │   sourced file:section)
  │ vocabulary│           └──────────────┘           └────┬─────┘
  └───────────┘                                           │ Bind (notion → template + slots)
                                                          ▼
                                              ┌────────────────────────┐
                                Assemble ────▶│ canonical bracket fill │
                                              │ [Subject][Details] ←facts
                                              │ [Locked constants] ←verbatim
                                              │ [Action/Env/Cine/Light/Tech] ←fixed
                                              │ setting tokens ←palette/year
                                              └───────────┬────────────┘
                                                          ▼ Trace + verify (factual gate)
                                              body + trace-table + negative
                                              (every clause → its source, or REJECTED)
```

### F — Fetch (resolve the record set)
Given `entity` + `notion`, resolve the **primary record** (`<entity>/profile.md`)
plus its **context records** the facts depend on: the entity's faction (palette),
its species/world (locked constants), and `concepts/vocabulary.md` (canonical terms).
Extends today's coarse `lore_index` with a structured pull — same walker (`lore.rs::scan`),
richer extraction.

### E — Extract → FactSet (typed, sourced)
Parse the profile's known sections into a typed `FactSet`. Every field records the
`(rel_path, section)` it came from — this is the trace-table's raw material.

```
FactSet {
  identity:     { name, category, faction, origin }
  function:     String                     // gameplay role
  physical:     { form, dimensions, materials[], markings[], accents[] }
  color:        Palette                    // resolved from faction, not improvised
  wear_state:   Enum(pristine|used|battle_worn|field_modified)
  locked:       [Constant]                 // species/world silhouette — never varies
  lore_hook:    Option<String>
  sources:      { field → (rel_path, section) }   // provenance for every fact above
}
```

Sections map straight off the **Stage-0 profile.md** contract (Identity / Function /
Physical Description / Wear State / Lore Hook). Bold spans already harvested by
`lore.rs::bold_spans` seed the controlled-vocabulary match.

### B — Bind (notion → template + slot map)
The **binding** (from LORE-MATRIX) selects, per notion:
- which **bracket template** (artifact-hero, character-bust-neutral, character-fullbody-hero, environment-keyart…),
- which FactSet fields feed `[Subject]` vs `[Details]`,
- the **fixed** framing/technical brackets (constant per notion = the consistency anchors),
- the negative prompt set.

### A — Assemble (fill the canonical bracket template)
The template is the protocol's, verbatim — only `[Subject]` and `[Details]` vary;
the rest are consistency anchors:

```
[Subject]:        ← FactSet.identity + physical.form + primary materials/dimensions
[Action]:         ← fixed by notion (e.g. "static 3/4 product shot" | "head-and-shoulders bust")
[Environment]:    ← fixed ("pure white seamless, no shadows")
[Cinematography]: ← fixed ("centered, no DoF, clean silhouette for image-to-3D")
[Lighting/Style]: ← fixed ("even flat studio lighting")
[Technical]:      ← fixed ("8K photoreal, optimized for Tripo single-image")
[Details]:        ← FactSet.physical.markings/accents + wear_state
[Locked constants]:← FactSet.locked, injected VERBATIM
[Setting tokens]: ← palette + narrative year + genre (from vocabulary.md)
```

### T — Trace & verify (the factual gate — the point of the whole rehash)
Assembly emits, alongside the body, a **trace table**: one row per clause →
`(fact_field, rel_path, section)`. Then three checks gate it:
1. **Provenance-complete** — any clause with **no source** is a candidate invention → **rejected/flagged**, never silently shipped.
2. **Locked-constants present** — every `FactSet.locked` term appears verbatim, or the prompt fails the gate.
3. **Controlled vocabulary** — a synonym is rewritten to its canonical term via the vocabulary tiebreaker (`lore.rs` already lifts canonical terms); an unknown term is flagged, not passed.

The result stored in `prompts` is therefore an **auditable** artifact: you can ask
"which lore record justifies this clause?" for every clause.

### (Optional) Certify — the Inheritor battery
For **controlled-vocabulary tokens** (axes/modifiers of a population, e.g. caste ×
build), a token may be required to pass the testbed's 6-test battery
(Stability / Discrimination / Cross-talk / Composition / Silhouette-lock / Caste-bias)
and be marked `STABLE` before the assembler will use it. This is the certification
gate that turns a proposed vocabulary into a trusted one.

---

## Data model (extends the existing DB, not a rewrite)

- **`lore_records`** (new, or extend `lore_index`): the structured FactSet per entity,
  JSON-encoded, with a `sources` map. `lore_index` stays as the browse surface.
- **`prompts`** (existing — `project.rs:28`): gains two columns —
  `trace TEXT` (JSON clause→source) and `generated INTEGER` (1 = assembled, 0 =
  hand-authored import). `body` stays the human-readable prompt; `slot` maps to the
  notion; `stage`/`backend`/`model`/`params` unchanged, so the current Prompt-Matrix
  tab and `import_prompts` path keep working.
- **`controlled_vocab`** (new): `canonical_term, synonyms[], axis, status(STABLE|DRIFTING|UNRELIABLE), source`.
- **`locked_constants`** (new): `scope(species|world|faction), term, text` — injected verbatim.
- **`palette`** (new): `faction → {colors[], materials[], finish}` — resolved, never improvised (protocol Reinforcement Rule 5).

## Mapping onto existing Synthetrix code
- `lore.rs::extract` → add a structured `facts(text) -> FactSet` beside the current
  (title, summary, vocab) triple; reuse `bold_spans` for vocab matching.
- `plan.rs` (lifted from `I:\Forge`) → replace `fragments.join(", ")` with
  `assemble(template, factset, vocab, constants) -> (body, trace)`.
- `project.rs` prompts table + `PromptRow` → add `trace`, `generated`; new
  `generate_prompt(entity, notion)` alongside `import_prompts`.
- Prompt-Matrix tab → render the trace inline; flag clauses with no source; let a human
  edit, with edits recorded as a diff against the generated base (drift stays visible).

---

## Worked example — Inheritor `V-LM` (real cell, `inheritor_grid.csv`)

**FactSet** (from `world/the-inheritors.md` + testbed vocab/constants):
- identity: humanoid amphibian alien · caste **Vigil** · build **Lean** · sex Male
- locked (species silhouette, verbatim): *hairless skull with raised cartilage ridge
  crown-to-brow · large wide-set eyes, horizontal slit pupils · small mouth high on
  the face · three vertical breathing slits per neck side · wet-glossy teal skin*
- caste facts: *watcher-defender · pale-blue cheek war-paint · bone-white woven
  shoulder harness with carved bone tabs · alert upright bearing*
- modifiers (neutral baseline): *muted mid-teal skin · low dorsal ridge · moderate
  bone-white eye markings · prime adult · field-worn*

**Assembled body** = the `[Subject]…[Modifiers]` bracket string already in the grid's
`full_prompt` column — but now **generated**, not hand-typed.

**Trace table** (excerpt) — this is the new, essential output:

| Clause | Source |
|---|---|
| "lean-framed male, narrow shoulders" | testbed `inheritor_vocabulary.csv` · build=Lean |
| "horizontal slit pupils … three breathing slits" | `world/the-inheritors.md` · Physiology (locked) |
| "pale-blue war-paint, bone-white harness" | `inheritor_vocabulary.csv` · caste=Vigil |
| "muted mid-teal skin, field-worn" | modifier axis · neutral baseline |

Every clause resolves to a record. If the assembler tried to add "webbed hands" and
no record said so, the gate would flag it — **that** is a factual prompt.

---

## The factual guarantee (invariants the subsystem enforces)
1. **No clause without a source** — provenance-complete or rejected.
2. **Locked constants always present, verbatim** — the silhouette never drifts.
3. **Controlled vocabulary only** — synonyms normalized, unknowns flagged.
4. **Deterministic** — same FactSet + template (+ seed) → same prompt.
5. **Auditable edits** — human overrides recorded as diffs against the generated base.

## Supersedes / cross-refs
- Details LORE-MATRIX.md Steps 2–3 (binding + plan). Consumes the binding layer;
  produces what the burst/grid fires.
- Design canon: `agent_roles/artifact-creation-protocol.md` §Stage 1 (bracket
  template), `agent_roles/character-creation-protocol.md`, `concepts/vocabulary.md`
  (palette + terms), `concepts/inheritor-variation-testbed/` (controlled vocab +
  locked constants + certification battery).
