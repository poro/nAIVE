use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug)]
pub enum SceneError {
    IoError(std::io::Error),
    ParseError(serde_yaml::Error),
    InheritanceCycle(String),
    MissingParent(String),
}

impl std::fmt::Display for SceneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoError(e) => write!(f, "IO error: {}", e),
            Self::ParseError(e) => write!(f, "YAML parse error: {}", e),
            Self::InheritanceCycle(id) => write!(f, "Inheritance cycle detected at entity '{}'", id),
            Self::MissingParent(id) => write!(f, "Entity extends missing parent '{}'", id),
        }
    }
}

// --- Serde types matching PRD scene YAML schema ---

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SceneFile {
    pub name: String,
    #[serde(default)]
    pub settings: SceneSettings,
    #[serde(default)]
    pub entities: Vec<EntityDef>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SceneSettings {
    #[serde(default = "default_ambient")]
    pub ambient_light: [f32; 3],
    #[serde(default)]
    pub fog: Option<FogSettings>,
    #[serde(default = "default_gravity")]
    pub gravity: [f32; 3],
}

fn default_ambient() -> [f32; 3] {
    [0.1, 0.1, 0.1]
}

fn default_gravity() -> [f32; 3] {
    [0.0, -9.81, 0.0]
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FogSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub color: [f32; 3],
    #[serde(default)]
    pub density: f32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EntityDef {
    pub id: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub extends: Option<String>,
    #[serde(default)]
    pub components: ComponentMap,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ComponentMap {
    #[serde(default)]
    pub transform: Option<TransformDef>,
    #[serde(default)]
    pub mesh_renderer: Option<MeshRendererDef>,
    #[serde(default)]
    pub camera: Option<CameraDef>,
    #[serde(default)]
    pub point_light: Option<PointLightDef>,
    #[serde(default)]
    pub gaussian_splat: Option<GaussianSplatDef>,
    /// Absorbs unknown component types for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TransformDef {
    #[serde(default)]
    pub position: [f32; 3],
    #[serde(default)]
    pub rotation: [f32; 3],
    #[serde(default = "default_scale")]
    pub scale: [f32; 3],
}

fn default_scale() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MeshRendererDef {
    pub mesh: String,
    pub material: String,
    #[serde(default = "default_true")]
    pub cast_shadows: bool,
    #[serde(default = "default_true")]
    pub receive_shadows: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CameraDef {
    #[serde(default = "default_fov")]
    pub fov: f32,
    #[serde(default = "default_near")]
    pub near: f32,
    #[serde(default = "default_far")]
    pub far: f32,
    #[serde(default = "default_role")]
    pub role: String,
}

fn default_fov() -> f32 {
    75.0
}
fn default_near() -> f32 {
    0.1
}
fn default_far() -> f32 {
    100.0
}
fn default_role() -> String {
    "main".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PointLightDef {
    #[serde(default = "default_white")]
    pub color: [f32; 3],
    #[serde(default = "default_intensity")]
    pub intensity: f32,
    #[serde(default = "default_range")]
    pub range: f32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GaussianSplatDef {
    pub source: String,
}

fn default_white() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}
fn default_intensity() -> f32 {
    1.0
}
fn default_range() -> f32 {
    10.0
}

/// Load and parse a scene YAML file, resolving entity inheritance.
pub fn load_scene(path: &Path) -> Result<SceneFile, SceneError> {
    let contents = std::fs::read_to_string(path).map_err(SceneError::IoError)?;
    let mut scene: SceneFile = serde_yaml::from_str(&contents).map_err(SceneError::ParseError)?;
    scene.entities = resolve_inheritance(&scene.entities)?;
    Ok(scene)
}

/// Resolve `extends` references: merge parent components into child.
/// Child fields override parent fields.
fn resolve_inheritance(entities: &[EntityDef]) -> Result<Vec<EntityDef>, SceneError> {
    let entity_map: HashMap<&str, &EntityDef> =
        entities.iter().map(|e| (e.id.as_str(), e)).collect();

    let mut resolved = Vec::with_capacity(entities.len());

    for entity in entities {
        if let Some(parent_id) = &entity.extends {
            let parent = entity_map
                .get(parent_id.as_str())
                .ok_or_else(|| SceneError::MissingParent(parent_id.clone()))?;

            // Detect simple self-reference cycle
            if parent.id == entity.id {
                return Err(SceneError::InheritanceCycle(entity.id.clone()));
            }

            let merged = merge_entity(parent, entity);
            resolved.push(merged);
        } else {
            resolved.push(entity.clone());
        }
    }

    Ok(resolved)
}

/// Merge parent entity components into child. Child fields win.
fn merge_entity(parent: &EntityDef, child: &EntityDef) -> EntityDef {
    let mut merged = child.clone();
    merged.extends = None; // resolved

    // Merge components: if child has a component, use it; otherwise inherit from parent
    if merged.components.transform.is_none() {
        merged.components.transform = parent.components.transform.clone();
    }
    if merged.components.mesh_renderer.is_none() {
        merged.components.mesh_renderer = parent.components.mesh_renderer.clone();
    }
    if merged.components.camera.is_none() {
        merged.components.camera = parent.components.camera.clone();
    }
    if merged.components.point_light.is_none() {
        merged.components.point_light = parent.components.point_light.clone();
    }
    if merged.components.gaussian_splat.is_none() {
        merged.components.gaussian_splat = parent.components.gaussian_splat.clone();
    }

    // Merge extra components from parent that child doesn't have
    for (key, value) in &parent.components.extra {
        merged
            .components
            .extra
            .entry(key.clone())
            .or_insert_with(|| value.clone());
    }

    // Merge tags
    if merged.tags.is_empty() {
        merged.tags = parent.tags.clone();
    }

    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_scene() {
        let yaml = r#"
name: "Test Scene"
settings:
  ambient_light: [0.2, 0.2, 0.25]
entities:
  - id: main_camera
    components:
      transform:
        position: [0, 2, -5]
      camera:
        fov: 75
        role: main
  - id: cube_01
    components:
      transform:
        position: [0, 0, 0]
      mesh_renderer:
        mesh: assets/meshes/cube.gltf
        material: assets/materials/default.yaml
  - id: sun_light
    components:
      transform:
        position: [5, 10, -3]
      point_light:
        color: [1.0, 0.95, 0.9]
        intensity: 10.0
        range: 50.0
"#;
        let scene: SceneFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(scene.name, "Test Scene");
        assert_eq!(scene.entities.len(), 3);
        assert!(scene.entities[0].components.camera.is_some());
        assert!(scene.entities[1].components.mesh_renderer.is_some());
        assert!(scene.entities[2].components.point_light.is_some());
    }

    #[test]
    fn test_inheritance() {
        let yaml = r#"
name: "Inheritance Test"
entities:
  - id: torch_base
    components:
      transform:
        position: [0, 0, 0]
      mesh_renderer:
        mesh: assets/meshes/torch.gltf
        material: assets/materials/default.yaml
      point_light:
        color: [1.0, 0.7, 0.3]
        intensity: 8.0
        range: 6.0
  - id: torch_02
    extends: torch_base
    components:
      transform:
        position: [3, 0, 0]
"#;
        let scene: SceneFile = serde_yaml::from_str(yaml).unwrap();
        let resolved = resolve_inheritance(&scene.entities).unwrap();

        assert_eq!(resolved.len(), 2);
        // torch_02 should inherit mesh_renderer and point_light from torch_base
        let torch_02 = &resolved[1];
        assert!(torch_02.components.mesh_renderer.is_some());
        assert!(torch_02.components.point_light.is_some());
        // But have its own position
        assert_eq!(
            torch_02.components.transform.as_ref().unwrap().position,
            [3.0, 0.0, 0.0]
        );
    }

    #[test]
    fn test_unknown_components_ignored() {
        let yaml = r#"
name: "Forward Compat"
entities:
  - id: player
    components:
      transform:
        position: [0, 0, 0]
      rigid_body:
        type: dynamic
        mass: 70.0
      script:
        source: logic/player.lua
"#;
        let scene: SceneFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(scene.entities.len(), 1);
        assert!(scene.entities[0].components.transform.is_some());
        // rigid_body and script should be in extra
        assert!(scene.entities[0].components.extra.contains_key("rigid_body"));
        assert!(scene.entities[0].components.extra.contains_key("script"));
    }
}
