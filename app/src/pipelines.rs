//! Composite pipelines (Phase 5). A pipeline is an ordered job graph over the
//! media bus: a notion flows through stages (2D image → 3D mesh → engine place,
//! or text → voice) with each stage handed to a backend. Synthetrix records the
//! run + per-stage state in `project.sqlite` (pipelines table) so a composite
//! build is reproducible and auditable, not a fire-and-forget script.
//!
//! Live compute lives in the backends; this module owns the *shape* of a build
//! and the serialised stage state. Stages whose backend isn't configured are
//! recorded as `blocked` with a clear reason rather than silently skipped.

use serde::{Deserialize, Serialize};

/// What a stage produces / the backend family it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StageKind {
    /// text → image (local ComfyUI)
    Image,
    /// image → textured 3D mesh (Tripo)
    Mesh,
    /// text → voice line (ElevenLabs)
    Voice,
    /// copy the latest asset into the engine tree under a topic
    Place,
}

impl StageKind {
    pub fn glyph(&self) -> &'static str {
        match self {
            StageKind::Image => "🖼",
            StageKind::Mesh => "🧊",
            StageKind::Voice => "🎙",
            StageKind::Place => "📦",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            StageKind::Image => "image",
            StageKind::Mesh => "mesh",
            StageKind::Voice => "voice",
            StageKind::Place => "place",
        }
    }
}

/// One stage template within a pipeline definition.
#[derive(Clone, Debug)]
pub struct StageDef {
    pub kind: StageKind,
    pub backend: &'static str,
    /// For Place stages: the engine topic (Characters/Props/Weapons/…).
    pub topic: &'static str,
}

/// A named, ordered composite build.
#[derive(Clone, Debug)]
pub struct PipelineDef {
    pub name: &'static str,
    pub description: &'static str,
    pub stages: Vec<StageDef>,
}

/// The built-in pipeline catalogue. Each is a canonical prompt→asset route the
/// IP forge knows how to run end-to-end.
pub fn builtin() -> Vec<PipelineDef> {
    vec![
        PipelineDef {
            name: "Character → 3D",
            description: "Concept image → textured mesh → placed under Characters.",
            stages: vec![
                StageDef {
                    kind: StageKind::Image,
                    backend: "comfy_local",
                    topic: "",
                },
                StageDef {
                    kind: StageKind::Mesh,
                    backend: "tripo",
                    topic: "",
                },
                StageDef {
                    kind: StageKind::Place,
                    backend: "",
                    topic: "Characters",
                },
            ],
        },
        PipelineDef {
            name: "Prop → 3D",
            description: "Prop/weapon concept → mesh → placed under Props.",
            stages: vec![
                StageDef {
                    kind: StageKind::Image,
                    backend: "comfy_local",
                    topic: "",
                },
                StageDef {
                    kind: StageKind::Mesh,
                    backend: "tripo",
                    topic: "",
                },
                StageDef {
                    kind: StageKind::Place,
                    backend: "",
                    topic: "Props",
                },
            ],
        },
        PipelineDef {
            name: "Concept art",
            description: "Single high-quality concept image into the asset vault.",
            stages: vec![StageDef {
                kind: StageKind::Image,
                backend: "comfy_local",
                topic: "",
            }],
        },
        PipelineDef {
            name: "Voice line",
            description: "Text → spoken line (ElevenLabs) into the asset vault.",
            stages: vec![StageDef {
                kind: StageKind::Voice,
                backend: "elevenlabs",
                topic: "",
            }],
        },
    ]
}

pub fn by_name(name: &str) -> Option<PipelineDef> {
    builtin().into_iter().find(|p| p.name == name)
}

/// Runtime state of one stage, serialised into the pipelines.stages JSON column.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StageState {
    pub kind: StageKind,
    pub backend: String,
    pub topic: String,
    /// pending | running | done | blocked | failed
    pub status: String,
    pub detail: String,
    /// asset id produced by this stage (0 if none yet).
    pub asset_id: i64,
}

impl StageState {
    pub fn from_def(d: &StageDef) -> Self {
        Self {
            kind: d.kind,
            backend: d.backend.to_string(),
            topic: d.topic.to_string(),
            status: "pending".into(),
            detail: String::new(),
            asset_id: 0,
        }
    }
}

/// Expand a definition into its initial per-stage state list.
pub fn initial_states(def: &PipelineDef) -> Vec<StageState> {
    def.stages.iter().map(StageState::from_def).collect()
}

pub fn to_json(states: &[StageState]) -> String {
    serde_json::to_string(states).unwrap_or_else(|_| "[]".into())
}

pub fn from_json(s: &str) -> Vec<StageState> {
    serde_json::from_str(s).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_wellformed() {
        let all = builtin();
        assert!(all.len() >= 4);
        // every mesh/place chain must start from an image (nothing to mesh otherwise)
        for p in &all {
            if p.stages.iter().any(|s| s.kind == StageKind::Mesh) {
                assert_eq!(p.stages[0].kind, StageKind::Image, "{}", p.name);
            }
        }
    }

    #[test]
    fn stage_state_roundtrips() {
        let def = by_name("Character → 3D").expect("known pipeline");
        let states = initial_states(&def);
        assert_eq!(states.len(), 3);
        assert_eq!(states[0].status, "pending");
        let json = to_json(&states);
        let back = from_json(&json);
        assert_eq!(back.len(), 3);
        assert_eq!(back[1].kind, StageKind::Mesh);
        assert_eq!(back[2].topic, "Characters");
    }
}
