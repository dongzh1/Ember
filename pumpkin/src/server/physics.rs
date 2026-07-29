// EMBER start - native rigid-body physics
//! Native rigid-body service and Pumpkin terrain adapter.
//!
//! Rapier and its public handles live in the isolated `ember-physics` crate.
//! This module owns only configuration/preset resolution and conversion of
//! loaded Minecraft chunk sections into cached static colliders.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use ember_physics::{
    BodyHandle, BodySpec, CompoundPart, PhysicsConfig, PhysicsWorld, Rotation, Shape, StaticBox,
    StaticRegionId, Transform, Vec3, VoxelRegion,
};
use pumpkin_config::{
    LoadConfiguration, PhysicsMaterialConfig, PhysicsMaterialListConfig, PhysicsPartShapeConfig,
    PhysicsPresetConfig, PhysicsPresetListConfig, PhysicsShapeConfig, PhysicsSystemConfig,
};
use pumpkin_data::BlockState;
use pumpkin_util::math::{position::BlockPos, vector2::Vector2};
use rustc_hash::FxHashSet;

use crate::world::World;

const SERVER_TICKS_PER_SECOND: u32 = 20;
const SECTION_SIZE: usize = 16;
const SECTION_VOLUME: usize = SECTION_SIZE * SECTION_SIZE * SECTION_SIZE;

#[derive(Debug, thiserror::Error)]
pub enum PhysicsError {
    #[error("the physics system is disabled (set enabled = true in physics/physics.toml)")]
    Disabled,
    #[error("unknown physics preset: {0}")]
    UnknownPreset(String),
    #[error("unknown physics material: {0}")]
    UnknownMaterial(String),
    #[error("the world reached max_bodies_per_world ({0})")]
    BodyLimit(usize),
    #[error("the rigid-body description is invalid")]
    InvalidBody,
}

/// Immutable definitions shared by all worlds.
pub struct PhysicsRegistry {
    config: PhysicsSystemConfig,
    materials: HashMap<String, PhysicsMaterialConfig>,
    presets: HashMap<String, PhysicsPresetConfig>,
}

impl PhysicsRegistry {
    #[must_use]
    pub fn load() -> Arc<Self> {
        let root = std::env::current_dir().expect("Failed to get current directory");
        let config = PhysicsSystemConfig::load(&root);
        let material_list = PhysicsMaterialListConfig::load(&root);
        let preset_list = PhysicsPresetListConfig::load(&root);
        Self::from_configs(config, material_list, preset_list)
    }

    #[must_use]
    pub fn disabled() -> Arc<Self> {
        Self::from_configs(
            PhysicsSystemConfig::default(),
            PhysicsMaterialListConfig::default(),
            PhysicsPresetListConfig::default(),
        )
    }

    fn from_configs(
        config: PhysicsSystemConfig,
        material_list: PhysicsMaterialListConfig,
        preset_list: PhysicsPresetListConfig,
    ) -> Arc<Self> {
        let materials: HashMap<_, _> = material_list
            .materials
            .into_iter()
            .map(|material| (material.id.clone(), material))
            .collect();
        for preset in &preset_list.presets {
            assert!(
                materials.contains_key(&preset.material),
                "physics preset '{}' references unknown material '{}'",
                preset.id,
                preset.material
            );
        }
        let presets = preset_list
            .presets
            .into_iter()
            .map(|preset| (preset.id.clone(), preset))
            .collect();
        Arc::new(Self {
            config,
            materials,
            presets,
        })
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.config.enabled
    }

    #[must_use]
    pub const fn config(&self) -> &PhysicsSystemConfig {
        &self.config
    }

    pub fn preset_ids(&self) -> impl Iterator<Item = &str> {
        self.presets.keys().map(String::as_str)
    }

    fn resolve_preset(&self, id: &str, position: Vec3) -> Result<ResolvedPreset, PhysicsError> {
        let preset = self
            .presets
            .get(id)
            .ok_or_else(|| PhysicsError::UnknownPreset(id.to_string()))?;
        let material = self
            .materials
            .get(&preset.material)
            .ok_or_else(|| PhysicsError::UnknownMaterial(preset.material.clone()))?;
        let shape = match &preset.shape {
            PhysicsShapeConfig::Cuboid { half_extents } => Shape::Cuboid {
                half_extents: Vec3::new(half_extents[0], half_extents[1], half_extents[2]),
            },
            PhysicsShapeConfig::Ball { radius } => Shape::Ball { radius: *radius },
            PhysicsShapeConfig::CapsuleY {
                half_height,
                radius,
            } => Shape::CapsuleY {
                half_height: *half_height,
                radius: *radius,
            },
            PhysicsShapeConfig::CylinderY {
                half_height,
                radius,
            } => Shape::CylinderY {
                half_height: *half_height,
                radius: *radius,
            },
            PhysicsShapeConfig::Compound { .. } => Shape::Cuboid {
                half_extents: Vec3::new(0.5, 0.5, 0.5),
            },
        };
        let mut spec = BodySpec::cuboid(position, Vec3::new(0.5, 0.5, 0.5));
        spec.shape = shape;
        spec.mass = preset.mass;
        spec.friction = material.friction;
        spec.restitution = material.restitution;
        spec.linear_damping = material.linear_damping;
        spec.angular_damping = material.angular_damping;
        spec.continuous_collision_detection =
            preset.ccd || self.config.continuous_collision_detection;
        spec.can_sleep = self.config.sleeping_enabled;
        if let PhysicsShapeConfig::Compound { parts } = &preset.shape {
            let parts = parts
                .iter()
                .map(|part| CompoundPart {
                    transform: Transform {
                        position: Vec3::new(part.offset[0], part.offset[1], part.offset[2]),
                        rotation: Rotation {
                            x: part.rotation[0],
                            y: part.rotation[1],
                            z: part.rotation[2],
                            w: part.rotation[3],
                        },
                    },
                    shape: part_shape(&part.shape),
                    mass_fraction: part.mass_fraction,
                })
                .collect();
            Ok(ResolvedPreset::Compound(spec, parts))
        } else {
            Ok(ResolvedPreset::Simple(spec))
        }
    }
}

enum ResolvedPreset {
    Simple(BodySpec),
    Compound(BodySpec, Vec<CompoundPart>),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SectionKey {
    chunk_x: i32,
    section_y: i32,
    chunk_z: i32,
}

#[derive(Default)]
struct TerrainState {
    active_chunks: FxHashSet<Vector2<i32>>,
    regions: HashMap<SectionKey, StaticRegionId>,
    queued: HashSet<SectionKey>,
    rebuild_queue: VecDeque<SectionKey>,
    next_region_id: u64,
}

impl TerrainState {
    fn enqueue(&mut self, key: SectionKey) {
        if self.queued.insert(key) {
            self.rebuild_queue.push_back(key);
        }
    }

    fn take_rebuilds(&mut self, count: usize) -> Vec<SectionKey> {
        let mut result = Vec::with_capacity(count);
        for _ in 0..count {
            let Some(key) = self.rebuild_queue.pop_front() else {
                break;
            };
            self.queued.remove(&key);
            result.push(key);
        }
        result
    }

    fn region_for(&mut self, key: SectionKey) -> StaticRegionId {
        if let Some(region) = self.regions.get(&key) {
            return *region;
        }
        self.next_region_id = self.next_region_id.wrapping_add(1).max(1);
        let region = StaticRegionId(self.next_region_id);
        self.regions.insert(key, region);
        region
    }
}

/// Per-Minecraft-world physics state.
pub struct PhysicsManager {
    registry: Arc<PhysicsRegistry>,
    engine: Mutex<PhysicsWorld>,
    terrain: Mutex<TerrainState>,
    tick_accumulator: std::sync::atomic::AtomicU32,
}

impl PhysicsManager {
    #[must_use]
    pub fn new(registry: Arc<PhysicsRegistry>) -> Self {
        let config = registry.config();
        Self {
            engine: Mutex::new(PhysicsWorld::new(PhysicsConfig {
                gravity: Vec3::new(config.gravity[0], config.gravity[1], config.gravity[2]),
                timestep_seconds: 1.0 / f32::from(config.simulation_hz),
            })),
            registry,
            terrain: Mutex::new(TerrainState::default()),
            tick_accumulator: std::sync::atomic::AtomicU32::new(0),
        }
    }

    #[must_use]
    pub fn enabled(&self) -> bool {
        self.registry.enabled()
    }

    /// Gives built-in Ember systems direct access to the complete engine API.
    pub fn with_world<R>(&self, operation: impl FnOnce(&mut PhysicsWorld) -> R) -> R {
        operation(&mut self.lock_engine())
    }

    pub fn spawn(&self, spec: BodySpec) -> Result<BodyHandle, PhysicsError> {
        if !self.enabled() {
            return Err(PhysicsError::Disabled);
        }
        let mut engine = self.lock_engine();
        if engine.body_count() >= self.registry.config.max_bodies_per_world {
            return Err(PhysicsError::BodyLimit(
                self.registry.config.max_bodies_per_world,
            ));
        }
        engine.spawn(spec).ok_or(PhysicsError::InvalidBody)
    }

    pub fn spawn_preset(&self, id: &str, position: Vec3) -> Result<BodyHandle, PhysicsError> {
        if !self.enabled() {
            return Err(PhysicsError::Disabled);
        }
        let preset = self.registry.resolve_preset(id, position)?;
        let mut engine = self.lock_engine();
        if engine.body_count() >= self.registry.config.max_bodies_per_world {
            return Err(PhysicsError::BodyLimit(
                self.registry.config.max_bodies_per_world,
            ));
        }
        match preset {
            ResolvedPreset::Simple(spec) => engine.spawn(spec),
            ResolvedPreset::Compound(spec, parts) => engine.spawn_compound(spec, &parts),
        }
        .ok_or(PhysicsError::InvalidBody)
    }

    pub fn remove(&self, handle: BodyHandle) -> bool {
        self.lock_engine().remove(handle)
    }

    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        let engine = self.lock_engine();
        (engine.body_count(), engine.collider_count())
    }

    /// Marks the containing terrain section for a cheap deferred rebuild.
    pub fn mark_block_dirty(&self, position: BlockPos) {
        if !self.enabled() || !self.registry.config.terrain.enabled {
            return;
        }
        let key = SectionKey {
            chunk_x: position.0.x.div_euclid(SECTION_SIZE as i32),
            section_y: position.0.y.div_euclid(SECTION_SIZE as i32),
            chunk_z: position.0.z.div_euclid(SECTION_SIZE as i32),
        };
        let mut terrain = lock(&self.terrain);
        if terrain.regions.contains_key(&key) {
            terrain.enqueue(key);
        }
    }

    pub fn tick(&self, world: &World) {
        if !self.enabled() {
            return;
        }
        if self.registry.config.terrain.enabled {
            self.sync_terrain(world);
        }

        let hz = u32::from(self.registry.config.simulation_hz);
        let previous = self
            .tick_accumulator
            .fetch_add(hz, std::sync::atomic::Ordering::Relaxed);
        let total = previous + hz;
        let steps = total / SERVER_TICKS_PER_SECOND;
        self.tick_accumulator.store(
            total % SERVER_TICKS_PER_SECOND,
            std::sync::atomic::Ordering::Relaxed,
        );
        if steps > 0 {
            let mut engine = self.lock_engine();
            for _ in 0..steps {
                engine.step();
            }
        }
    }

    fn sync_terrain(&self, world: &World) {
        let active = world.active_chunks.load();
        let generation =
            pumpkin_data::chunk_gen_settings::GenerationSettings::from_dimension(&world.dimension);
        let min_section = i32::from(generation.shape.min_y).div_euclid(SECTION_SIZE as i32);
        let section_count = usize::from(generation.shape.height) / SECTION_SIZE;
        let max_cached = self.registry.config.terrain.max_cached_sections;

        let (removed_regions, rebuilds) = {
            let mut terrain = lock(&self.terrain);
            let removed_chunks: Vec<_> =
                terrain.active_chunks.difference(&active).copied().collect();
            let mut removed_regions = Vec::new();
            for chunk in removed_chunks {
                for section_offset in 0..section_count {
                    let key = SectionKey {
                        chunk_x: chunk.x,
                        section_y: min_section + section_offset as i32,
                        chunk_z: chunk.y,
                    };
                    if let Some(region) = terrain.regions.remove(&key) {
                        removed_regions.push(region);
                    }
                    terrain.queued.remove(&key);
                }
            }
            let rebuild_queue = std::mem::take(&mut terrain.rebuild_queue);
            terrain.rebuild_queue = rebuild_queue
                .into_iter()
                .filter(|key| terrain.queued.contains(key))
                .collect();

            let added_chunks: Vec<_> = active.difference(&terrain.active_chunks).copied().collect();
            for chunk in added_chunks {
                for section_offset in 0..section_count {
                    if terrain.regions.len() + terrain.queued.len() >= max_cached {
                        break;
                    }
                    terrain.enqueue(SectionKey {
                        chunk_x: chunk.x,
                        section_y: min_section + section_offset as i32,
                        chunk_z: chunk.y,
                    });
                }
            }
            terrain.active_chunks.clone_from(&active);
            let rebuilds = terrain.take_rebuilds(self.registry.config.terrain.rebuilds_per_tick);
            (removed_regions, rebuilds)
        };

        let mut engine = self.lock_engine();
        for region in removed_regions {
            engine.remove_static_region(region);
        }
        for key in rebuilds {
            let Some(boxes) =
                build_section_boxes(world, key, self.registry.config.terrain.detailed_shapes)
            else {
                // Active chunks can be selected before asynchronous loading
                // finishes. Retry instead of leaving a collision hole.
                lock(&self.terrain).enqueue(key);
                continue;
            };
            let region = lock(&self.terrain).region_for(key);
            engine.replace_static_boxes(region, &boxes);
        }
    }

    fn lock_engine(&self) -> MutexGuard<'_, PhysicsWorld> {
        lock(&self.engine)
    }
}

fn build_section_boxes(
    world: &World,
    key: SectionKey,
    detailed_shapes: bool,
) -> Option<Vec<StaticBox>> {
    let chunk_position = Vector2::new(key.chunk_x, key.chunk_z);
    let chunk = world.level.loaded_chunks.get(&chunk_position)?;
    let origin = [
        key.chunk_x * SECTION_SIZE as i32,
        key.section_y * SECTION_SIZE as i32,
        key.chunk_z * SECTION_SIZE as i32,
    ];
    let mut full_blocks = vec![false; SECTION_VOLUME];
    let mut detailed = Vec::new();

    for y in 0..SECTION_SIZE {
        for z in 0..SECTION_SIZE {
            for x in 0..SECTION_SIZE {
                let world_y = origin[1] + y as i32;
                let Some(state_id) = chunk.section.get_block_absolute_y(x, world_y, z) else {
                    continue;
                };
                let state = BlockState::from_id(state_id);
                if state.is_full_cube() && state.collision_shapes.len() == 1 {
                    full_blocks[(y * SECTION_SIZE + z) * SECTION_SIZE + x] = true;
                } else if detailed_shapes {
                    append_detailed_shapes(&mut detailed, state, origin, x, y, z);
                }
            }
        }
    }

    let voxels = VoxelRegion::new(origin, [SECTION_SIZE; 3], full_blocks)
        .expect("fixed-size section voxel data must be valid");
    let mut boxes = voxels.merged_boxes();
    boxes.extend(detailed);
    Some(boxes)
}

fn append_detailed_shapes(
    output: &mut Vec<StaticBox>,
    state: &'static BlockState,
    origin: [i32; 3],
    x: usize,
    y: usize,
    z: usize,
) {
    for shape in state.get_block_collision_shapes() {
        let size_x = (shape.max.x - shape.min.x) as f32;
        let size_y = (shape.max.y - shape.min.y) as f32;
        let size_z = (shape.max.z - shape.min.z) as f32;
        if size_x <= 0.0 || size_y <= 0.0 || size_z <= 0.0 {
            continue;
        }
        output.push(StaticBox::new(
            Vec3::new(
                origin[0] as f32 + x as f32 + (shape.min.x + shape.max.x) as f32 * 0.5,
                origin[1] as f32 + y as f32 + (shape.min.y + shape.max.y) as f32 * 0.5,
                origin[2] as f32 + z as f32 + (shape.min.z + shape.max.z) as f32 * 0.5,
            ),
            Vec3::new(size_x * 0.5, size_y * 0.5, size_z * 0.5),
        ));
    }
}

const fn part_shape(shape: &PhysicsPartShapeConfig) -> Shape {
    match shape {
        PhysicsPartShapeConfig::Cuboid { half_extents } => Shape::Cuboid {
            half_extents: Vec3::new(half_extents[0], half_extents[1], half_extents[2]),
        },
        PhysicsPartShapeConfig::Ball { radius } => Shape::Ball { radius: *radius },
        PhysicsPartShapeConfig::CapsuleY {
            half_height,
            radius,
        } => Shape::CapsuleY {
            half_height: *half_height,
            radius: *radius,
        },
        PhysicsPartShapeConfig::CylinderY {
            half_height,
            radius,
        } => Shape::CylinderY {
            half_height: *half_height,
            radius: *radius,
        },
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_registry_rejects_spawns() {
        let manager = PhysicsManager::new(PhysicsRegistry::disabled());
        assert!(matches!(
            manager.spawn_preset("block", Vec3::default()),
            Err(PhysicsError::Disabled)
        ));
    }

    #[test]
    fn section_region_ids_are_stable() {
        let mut terrain = TerrainState::default();
        let key = SectionKey {
            chunk_x: -1,
            section_y: 2,
            chunk_z: 3,
        };
        assert_eq!(terrain.region_for(key), terrain.region_for(key));
    }
}
// EMBER end
