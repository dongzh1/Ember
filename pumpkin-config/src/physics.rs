use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::LoadConfiguration;

/// Server-wide settings for Ember's native rigid-body engine.
///
/// This lives in `physics/physics.toml` so the feature stays independent of
/// upstream Pumpkin configuration. It is disabled by default.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct PhysicsSystemConfig {
    pub enabled: bool,
    /// Fixed simulation frequency. Values below the server tick rate run on
    /// a divisor; values above it use multiple solver substeps per tick.
    pub simulation_hz: u16,
    pub gravity: [f32; 3],
    pub max_bodies_per_world: usize,
    pub sleeping_enabled: bool,
    pub continuous_collision_detection: bool,
    pub terrain: PhysicsTerrainConfig,
}

impl Default for PhysicsSystemConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            simulation_hz: 20,
            gravity: [0.0, -9.81, 0.0],
            max_bodies_per_world: 2_000,
            sleeping_enabled: true,
            continuous_collision_detection: false,
            terrain: PhysicsTerrainConfig::default(),
        }
    }
}

impl LoadConfiguration for PhysicsSystemConfig {
    fn get_path() -> &'static Path {
        Path::new("physics/physics.toml")
    }

    fn validate(&self) {
        assert!(
            (1..=200).contains(&self.simulation_hz),
            "physics simulation_hz must be in 1..=200"
        );
        assert!(
            self.gravity.iter().all(|value| value.is_finite()),
            "physics gravity must contain finite values"
        );
        assert!(
            self.max_bodies_per_world > 0,
            "physics max_bodies_per_world must be positive"
        );
        self.terrain.validate();
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct PhysicsTerrainConfig {
    /// Generate static colliders from active Minecraft chunks.
    pub enabled: bool,
    /// Maximum chunk sections converted during one server tick.
    pub rebuilds_per_tick: usize,
    /// Hard cap preventing terrain collider caches from growing without bound.
    pub max_cached_sections: usize,
    /// Include non-full collision shapes such as slabs and stairs.
    pub detailed_shapes: bool,
}

impl PhysicsTerrainConfig {
    fn validate(&self) {
        assert!(
            self.rebuilds_per_tick > 0,
            "physics terrain rebuilds_per_tick must be positive"
        );
        assert!(
            self.max_cached_sections > 0,
            "physics terrain max_cached_sections must be positive"
        );
    }
}

impl Default for PhysicsTerrainConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rebuilds_per_tick: 8,
            max_cached_sections: 4_096,
            detailed_shapes: true,
        }
    }
}

/// Named physical materials stored in `physics/materials.toml`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PhysicsMaterialConfig {
    pub id: String,
    pub friction: f32,
    pub restitution: f32,
    pub density: f32,
    pub linear_damping: f32,
    pub angular_damping: f32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct PhysicsMaterialListConfig {
    pub materials: Vec<PhysicsMaterialConfig>,
}

impl Default for PhysicsMaterialListConfig {
    fn default() -> Self {
        Self {
            materials: vec![
                PhysicsMaterialConfig {
                    id: "default".to_string(),
                    friction: 0.6,
                    restitution: 0.0,
                    density: 1_000.0,
                    linear_damping: 0.05,
                    angular_damping: 0.05,
                },
                PhysicsMaterialConfig {
                    id: "rubber".to_string(),
                    friction: 1.2,
                    restitution: 0.75,
                    density: 1_100.0,
                    linear_damping: 0.1,
                    angular_damping: 0.1,
                },
                PhysicsMaterialConfig {
                    id: "wood".to_string(),
                    friction: 0.7,
                    restitution: 0.15,
                    density: 700.0,
                    linear_damping: 0.05,
                    angular_damping: 0.08,
                },
            ],
        }
    }
}

impl LoadConfiguration for PhysicsMaterialListConfig {
    fn get_path() -> &'static Path {
        Path::new("physics/materials.toml")
    }

    fn validate(&self) {
        let mut ids = HashSet::new();
        for material in &self.materials {
            assert!(
                !material.id.trim().is_empty() && ids.insert(&material.id),
                "physics material ids must be non-empty and unique"
            );
            assert_non_negative_finite(material.friction, "material friction");
            assert_non_negative_finite(material.restitution, "material restitution");
            assert_positive_finite(material.density, "material density");
            assert_non_negative_finite(material.linear_damping, "material linear_damping");
            assert_non_negative_finite(material.angular_damping, "material angular_damping");
        }
    }
}

/// Configurable collider shapes for preset rigid bodies.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PhysicsShapeConfig {
    Cuboid {
        half_extents: [f32; 3],
    },
    Ball {
        radius: f32,
    },
    CapsuleY {
        half_height: f32,
        radius: f32,
    },
    CylinderY {
        half_height: f32,
        radius: f32,
    },
    Compound {
        parts: Vec<PhysicsCompoundPartConfig>,
    },
}

impl PhysicsShapeConfig {
    fn validate(&self) {
        match self {
            Self::Cuboid { half_extents } => assert!(
                half_extents
                    .iter()
                    .all(|value| value.is_finite() && *value > 0.0),
                "physics cuboid half_extents must be positive and finite"
            ),
            Self::Ball { radius } => assert_positive_finite(*radius, "physics ball radius"),
            Self::CapsuleY {
                half_height,
                radius,
            }
            | Self::CylinderY {
                half_height,
                radius,
            } => {
                assert_positive_finite(*half_height, "physics shape half_height");
                assert_positive_finite(*radius, "physics shape radius");
            }
            Self::Compound { parts } => {
                assert!(!parts.is_empty(), "physics compound shape must have parts");
                let mut mass_fraction = 0.0;
                for part in parts {
                    assert!(
                        part.offset.iter().all(|value| value.is_finite()),
                        "physics compound offsets must be finite"
                    );
                    assert!(
                        part.rotation.iter().all(|value| value.is_finite())
                            && part.rotation.iter().map(|value| value * value).sum::<f32>()
                                > f32::EPSILON,
                        "physics compound rotations must be finite non-zero quaternions"
                    );
                    assert_positive_finite(part.mass_fraction, "compound part mass_fraction");
                    part.shape.validate();
                    mass_fraction += part.mass_fraction;
                }
                assert!(
                    (mass_fraction - 1.0).abs() <= 0.001,
                    "physics compound mass fractions must sum to 1"
                );
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PhysicsCompoundPartConfig {
    pub offset: [f32; 3],
    #[serde(default = "identity_rotation")]
    pub rotation: [f32; 4],
    pub mass_fraction: f32,
    pub shape: PhysicsPartShapeConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PhysicsPartShapeConfig {
    Cuboid { half_extents: [f32; 3] },
    Ball { radius: f32 },
    CapsuleY { half_height: f32, radius: f32 },
    CylinderY { half_height: f32, radius: f32 },
}

impl PhysicsPartShapeConfig {
    fn validate(&self) {
        match self {
            Self::Cuboid { half_extents } => assert!(
                half_extents
                    .iter()
                    .all(|value| value.is_finite() && *value > 0.0),
                "physics compound cuboid half_extents must be positive and finite"
            ),
            Self::Ball { radius } => assert_positive_finite(*radius, "compound ball radius"),
            Self::CapsuleY {
                half_height,
                radius,
            }
            | Self::CylinderY {
                half_height,
                radius,
            } => {
                assert_positive_finite(*half_height, "compound shape half_height");
                assert_positive_finite(*radius, "compound shape radius");
            }
        }
    }
}

const fn identity_rotation() -> [f32; 4] {
    [0.0, 0.0, 0.0, 1.0]
}

/// Named body templates stored in `physics/presets.toml`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PhysicsPresetConfig {
    pub id: String,
    pub material: String,
    pub mass: f32,
    pub shape: PhysicsShapeConfig,
    #[serde(default)]
    pub ccd: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct PhysicsPresetListConfig {
    pub presets: Vec<PhysicsPresetConfig>,
}

impl Default for PhysicsPresetListConfig {
    fn default() -> Self {
        Self {
            presets: vec![
                PhysicsPresetConfig {
                    id: "block".to_string(),
                    material: "default".to_string(),
                    mass: 1.0,
                    shape: PhysicsShapeConfig::Cuboid {
                        half_extents: [0.5, 0.5, 0.5],
                    },
                    ccd: false,
                },
                PhysicsPresetConfig {
                    id: "ball".to_string(),
                    material: "rubber".to_string(),
                    mass: 1.0,
                    shape: PhysicsShapeConfig::Ball { radius: 0.5 },
                    ccd: true,
                },
            ],
        }
    }
}

impl LoadConfiguration for PhysicsPresetListConfig {
    fn get_path() -> &'static Path {
        Path::new("physics/presets.toml")
    }

    fn validate(&self) {
        let mut ids = HashSet::new();
        for preset in &self.presets {
            assert!(
                !preset.id.trim().is_empty() && ids.insert(&preset.id),
                "physics preset ids must be non-empty and unique"
            );
            assert!(
                !preset.material.trim().is_empty(),
                "physics preset material must not be empty"
            );
            assert_positive_finite(preset.mass, "physics preset mass");
            preset.shape.validate();
        }
    }
}

fn assert_positive_finite(value: f32, name: &str) {
    assert!(
        value.is_finite() && value > 0.0,
        "{name} must be positive and finite"
    );
}

fn assert_non_negative_finite(value: f32, name: &str) {
    assert!(
        value.is_finite() && value >= 0.0,
        "{name} must be non-negative and finite"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_physics_config_is_safe_and_disabled() {
        let config = PhysicsSystemConfig::default();
        assert!(!config.enabled);
        config.validate();
    }

    #[test]
    fn default_materials_and_presets_validate() {
        PhysicsMaterialListConfig::default().validate();
        PhysicsPresetListConfig::default().validate();
    }
}
