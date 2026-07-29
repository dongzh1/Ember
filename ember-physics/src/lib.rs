//! Low-overhead, server-side rigid-body physics for Ember.
//!
//! This crate deliberately has no dependency on Pumpkin internals. The server
//! integration can therefore translate positions at the boundary without
//! coupling the physics engine to upstream entity or world implementation.

mod voxel;

use std::collections::HashMap;

use rapier3d::prelude::{
    BroadPhaseBvh, CCDSolver, ColliderBuilder, ColliderHandle, ColliderSet, FixedJointBuilder,
    ImpulseJointHandle, ImpulseJointSet, IntegrationParameters, IslandManager, MultibodyJointSet,
    NarrowPhase, PhysicsPipeline, Pose, QueryFilter, Ray, RigidBodyBuilder, RigidBodyHandle,
    RigidBodySet, Rotation as RapierRotation, Vector as RapierVector,
};

pub use voxel::{VoxelError, VoxelRegion};

/// A vector in Minecraft world coordinates, measured in blocks.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    #[must_use]
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }
}

/// A normalized `(x, y, z, w)` rotation quaternion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rotation {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Rotation {
    pub const IDENTITY: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        w: 1.0,
    };
}

impl Default for Rotation {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Position and rotation produced after a simulation step.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Transform {
    pub position: Vec3,
    pub rotation: Rotation,
}

/// Collision geometry attached to a dynamic body.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Cuboid { half_extents: Vec3 },
    Ball { radius: f32 },
    CapsuleY { half_height: f32, radius: f32 },
    CylinderY { half_height: f32, radius: f32 },
}

/// How the solver moves a body.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BodyKind {
    #[default]
    Dynamic,
    Kinematic,
}

/// Description used to create a dynamic rigid body.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodySpec {
    pub kind: BodyKind,
    pub transform: Transform,
    pub shape: Shape,
    pub mass: f32,
    pub friction: f32,
    pub restitution: f32,
    pub linear_damping: f32,
    pub angular_damping: f32,
    pub continuous_collision_detection: bool,
    pub can_sleep: bool,
}

impl BodySpec {
    #[must_use]
    pub const fn cuboid(position: Vec3, half_extents: Vec3) -> Self {
        Self {
            kind: BodyKind::Dynamic,
            transform: Transform {
                position,
                rotation: Rotation::IDENTITY,
            },
            shape: Shape::Cuboid { half_extents },
            mass: 1.0,
            friction: 0.5,
            restitution: 0.0,
            linear_damping: 0.0,
            angular_damping: 0.0,
            continuous_collision_detection: false,
            can_sleep: true,
        }
    }

    #[must_use]
    pub const fn ball(position: Vec3, radius: f32) -> Self {
        Self {
            shape: Shape::Ball { radius },
            ..Self::cuboid(position, Vec3::new(radius, radius, radius))
        }
    }

    #[must_use]
    pub const fn mass(mut self, mass: f32) -> Self {
        self.mass = mass;
        self
    }

    #[must_use]
    pub const fn friction(mut self, friction: f32) -> Self {
        self.friction = friction;
        self
    }

    #[must_use]
    pub const fn restitution(mut self, restitution: f32) -> Self {
        self.restitution = restitution;
        self
    }

    #[must_use]
    pub const fn damping(mut self, linear: f32, angular: f32) -> Self {
        self.linear_damping = linear;
        self.angular_damping = angular;
        self
    }

    #[must_use]
    pub const fn ccd(mut self, enabled: bool) -> Self {
        self.continuous_collision_detection = enabled;
        self
    }

    #[must_use]
    pub const fn kinematic(mut self) -> Self {
        self.kind = BodyKind::Kinematic;
        self
    }

    #[must_use]
    pub const fn can_sleep(mut self, enabled: bool) -> Self {
        self.can_sleep = enabled;
        self
    }
}

/// Opaque identifier for a dynamic body.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BodyHandle(RigidBodyHandle);

/// Opaque identifier for a constraint between two bodies.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct JointHandle(ImpulseJointHandle);

/// One collider in a compound body, positioned in body-local coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompoundPart {
    pub transform: Transform,
    pub shape: Shape,
    /// Fraction of the body's configured mass assigned to this part.
    pub mass_fraction: f32,
}

impl CompoundPart {
    #[must_use]
    pub const fn new(position: Vec3, shape: Shape, mass_fraction: f32) -> Self {
        Self {
            transform: Transform {
                position,
                rotation: Rotation::IDENTITY,
            },
            shape,
            mass_fraction,
        }
    }
}

/// Stable identifier chosen by the caller for a chunk or other static region.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StaticRegionId(pub u64);

/// A world-space static cuboid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StaticBox {
    pub center: Vec3,
    pub half_extents: Vec3,
    pub friction: f32,
    pub restitution: f32,
}

/// Result of a world-space ray query.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    pub body: Option<BodyHandle>,
    pub static_region: Option<StaticRegionId>,
    pub position: Vec3,
    pub normal: Vec3,
    pub distance: f32,
}

impl StaticBox {
    #[must_use]
    pub const fn new(center: Vec3, half_extents: Vec3) -> Self {
        Self {
            center,
            half_extents,
            friction: 0.6,
            restitution: 0.0,
        }
    }
}

/// Simulation settings. Defaults match Minecraft's 20 Hz server tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhysicsConfig {
    pub gravity: Vec3,
    pub timestep_seconds: f32,
}

impl Default for PhysicsConfig {
    fn default() -> Self {
        Self {
            gravity: Vec3::new(0.0, -9.81, 0.0),
            timestep_seconds: 1.0 / 20.0,
        }
    }
}

/// One independent rigid-body simulation, normally one per loaded world.
pub struct PhysicsWorld {
    gravity: RapierVector,
    integration: IntegrationParameters,
    pipeline: PhysicsPipeline,
    islands: IslandManager,
    broad_phase: BroadPhaseBvh,
    narrow_phase: NarrowPhase,
    bodies: RigidBodySet,
    colliders: ColliderSet,
    impulse_joints: ImpulseJointSet,
    multibody_joints: MultibodyJointSet,
    ccd_solver: CCDSolver,
    static_regions: HashMap<StaticRegionId, Vec<ColliderHandle>>,
    static_collider_regions: HashMap<ColliderHandle, StaticRegionId>,
}

impl PhysicsWorld {
    #[must_use]
    pub fn new(config: PhysicsConfig) -> Self {
        let integration = IntegrationParameters {
            dt: config.timestep_seconds,
            ..IntegrationParameters::default()
        };
        Self {
            gravity: to_rapier_vec(config.gravity),
            integration,
            pipeline: PhysicsPipeline::new(),
            islands: IslandManager::new(),
            broad_phase: BroadPhaseBvh::new(),
            narrow_phase: NarrowPhase::new(),
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            impulse_joints: ImpulseJointSet::new(),
            multibody_joints: MultibodyJointSet::new(),
            ccd_solver: CCDSolver::new(),
            static_regions: HashMap::new(),
            static_collider_regions: HashMap::new(),
        }
    }

    /// Creates a dynamic body and its collider.
    pub fn spawn(&mut self, spec: BodySpec) -> Option<BodyHandle> {
        if !valid_spec(spec) {
            return None;
        }
        let handle = self.insert_body(spec)?;
        let collider = collider_builder(spec.shape)
            .mass(spec.mass)
            .friction(spec.friction)
            .restitution(spec.restitution)
            .build();
        self.colliders
            .insert_with_parent(collider, handle, &mut self.bodies);
        Some(BodyHandle(handle))
    }

    /// Creates one body with multiple locally positioned collision shapes.
    /// Rapier derives the combined center of mass and inertia tensor.
    pub fn spawn_compound(&mut self, spec: BodySpec, parts: &[CompoundPart]) -> Option<BodyHandle> {
        if !valid_spec(spec) || parts.is_empty() || !valid_compound_parts(parts) {
            return None;
        }
        let handle = self.insert_body(spec)?;
        for part in parts {
            let rotation = to_rapier_rotation(part.transform.rotation)?;
            let collider = collider_builder(part.shape)
                .position(Pose::from_parts(
                    to_rapier_vec(part.transform.position),
                    rotation,
                ))
                .mass(spec.mass * part.mass_fraction)
                .friction(spec.friction)
                .restitution(spec.restitution)
                .build();
            self.colliders
                .insert_with_parent(collider, handle, &mut self.bodies);
        }
        Some(BodyHandle(handle))
    }

    /// Removes a body and every collider or joint attached to it.
    pub fn remove(&mut self, handle: BodyHandle) -> bool {
        self.bodies
            .remove(
                handle.0,
                &mut self.islands,
                &mut self.colliders,
                &mut self.impulse_joints,
                &mut self.multibody_joints,
                true,
            )
            .is_some()
    }

    /// Advances the simulation by one configured fixed timestep.
    pub fn step(&mut self) {
        self.pipeline.step(
            self.gravity,
            &self.integration,
            &mut self.islands,
            &mut self.broad_phase,
            &mut self.narrow_phase,
            &mut self.bodies,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            &mut self.ccd_solver,
            &(),
            &(),
        );
    }

    #[must_use]
    pub fn transform(&self, handle: BodyHandle) -> Option<Transform> {
        let body = self.bodies.get(handle.0)?;
        let position = body.translation();
        let quaternion = body.rotation();
        Some(Transform {
            position: Vec3::new(position.x, position.y, position.z),
            rotation: Rotation {
                x: quaternion.x,
                y: quaternion.y,
                z: quaternion.z,
                w: quaternion.w,
            },
        })
    }

    pub fn apply_impulse(&mut self, handle: BodyHandle, impulse: Vec3) -> bool {
        let Some(body) = self.bodies.get_mut(handle.0) else {
            return false;
        };
        body.apply_impulse(to_rapier_vec(impulse), true);
        true
    }

    pub fn apply_impulse_at_point(
        &mut self,
        handle: BodyHandle,
        impulse: Vec3,
        point: Vec3,
    ) -> bool {
        let Some(body) = self.bodies.get_mut(handle.0) else {
            return false;
        };
        body.apply_impulse_at_point(to_rapier_vec(impulse), to_rapier_vec(point), true);
        true
    }

    pub fn apply_torque_impulse(&mut self, handle: BodyHandle, impulse: Vec3) -> bool {
        let Some(body) = self.bodies.get_mut(handle.0) else {
            return false;
        };
        body.apply_torque_impulse(to_rapier_vec(impulse), true);
        true
    }

    pub fn add_force(&mut self, handle: BodyHandle, force: Vec3) -> bool {
        let Some(body) = self.bodies.get_mut(handle.0) else {
            return false;
        };
        body.add_force(to_rapier_vec(force), true);
        true
    }

    pub fn add_force_at_point(&mut self, handle: BodyHandle, force: Vec3, point: Vec3) -> bool {
        let Some(body) = self.bodies.get_mut(handle.0) else {
            return false;
        };
        body.add_force_at_point(to_rapier_vec(force), to_rapier_vec(point), true);
        true
    }

    /// Applies Archimedes buoyancy and linear fluid drag.
    ///
    /// `submerged_fraction` is in `0..=1`; callers can derive it from block
    /// fluid height or a custom VVE-style medium model.
    pub fn apply_buoyancy(
        &mut self,
        handle: BodyHandle,
        displaced_volume: f32,
        fluid_density: f32,
        submerged_fraction: f32,
        linear_drag: f32,
    ) -> bool {
        if !displaced_volume.is_finite()
            || displaced_volume <= 0.0
            || !fluid_density.is_finite()
            || fluid_density <= 0.0
            || !submerged_fraction.is_finite()
            || !(0.0..=1.0).contains(&submerged_fraction)
            || !linear_drag.is_finite()
            || linear_drag < 0.0
        {
            return false;
        }
        let Some(body) = self.bodies.get_mut(handle.0) else {
            return false;
        };
        let displaced_mass = displaced_volume * fluid_density * submerged_fraction;
        let buoyancy = -self.gravity * displaced_mass;
        let drag = -body.linvel() * linear_drag * submerged_fraction;
        body.add_force(buoyancy + drag, true);
        true
    }

    pub fn set_linear_velocity(&mut self, handle: BodyHandle, velocity: Vec3) -> bool {
        let Some(body) = self.bodies.get_mut(handle.0) else {
            return false;
        };
        body.set_linvel(to_rapier_vec(velocity), true);
        true
    }

    pub fn set_angular_velocity(&mut self, handle: BodyHandle, velocity: Vec3) -> bool {
        let Some(body) = self.bodies.get_mut(handle.0) else {
            return false;
        };
        body.set_angvel(to_rapier_vec(velocity), true);
        true
    }

    pub fn set_gravity_scale(&mut self, handle: BodyHandle, scale: f32) -> bool {
        let Some(body) = self.bodies.get_mut(handle.0) else {
            return false;
        };
        if !scale.is_finite() {
            return false;
        }
        body.set_gravity_scale(scale, true);
        true
    }

    /// Moves a dynamic body immediately or sets a kinematic body's next pose.
    pub fn set_transform(&mut self, handle: BodyHandle, transform: Transform) -> bool {
        if !finite_vec(transform.position) {
            return false;
        }
        let Some(rotation) = to_rapier_rotation(transform.rotation) else {
            return false;
        };
        let Some(body) = self.bodies.get_mut(handle.0) else {
            return false;
        };
        let pose = Pose::from_parts(to_rapier_vec(transform.position), rotation);
        if body.is_kinematic() {
            body.set_next_kinematic_position(pose);
        } else {
            body.set_position(pose, true);
        }
        true
    }

    /// Adds a fixed constraint using local anchor points on both bodies.
    pub fn add_fixed_joint(
        &mut self,
        first: BodyHandle,
        second: BodyHandle,
        first_anchor: Vec3,
        second_anchor: Vec3,
    ) -> Option<JointHandle> {
        self.bodies.get(first.0)?;
        self.bodies.get(second.0)?;
        let joint = FixedJointBuilder::new()
            .local_anchor1(to_rapier_vec(first_anchor))
            .local_anchor2(to_rapier_vec(second_anchor));
        Some(JointHandle(
            self.impulse_joints.insert(first.0, second.0, joint, true),
        ))
    }

    pub fn remove_joint(&mut self, handle: JointHandle) -> bool {
        self.impulse_joints.remove(handle.0, true).is_some()
    }

    /// Casts a normalized world-space ray against dynamic and static geometry.
    #[must_use]
    pub fn cast_ray(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<RayHit> {
        if !finite_vec(origin)
            || !finite_vec(direction)
            || !max_distance.is_finite()
            || max_distance <= 0.0
        {
            return None;
        }
        let direction = to_rapier_vec(direction);
        let length = direction.length();
        if length <= f32::EPSILON {
            return None;
        }
        let ray = Ray::new(to_rapier_vec(origin), direction / length);
        let query = self.broad_phase.as_query_pipeline(
            self.narrow_phase.query_dispatcher(),
            &self.bodies,
            &self.colliders,
            QueryFilter::default(),
        );
        let (collider_handle, intersection) =
            query.cast_ray_and_get_normal(&ray, max_distance, true)?;
        let collider = self.colliders.get(collider_handle)?;
        let point = ray.point_at(intersection.time_of_impact);
        Some(RayHit {
            body: collider.parent().map(BodyHandle),
            static_region: self.static_collider_regions.get(&collider_handle).copied(),
            position: Vec3::new(point.x, point.y, point.z),
            normal: Vec3::new(
                intersection.normal.x,
                intersection.normal.y,
                intersection.normal.z,
            ),
            distance: intersection.time_of_impact,
        })
    }

    #[must_use]
    pub fn linear_velocity(&self, handle: BodyHandle) -> Option<Vec3> {
        let velocity = self.bodies.get(handle.0)?.linvel();
        Some(Vec3::new(velocity.x, velocity.y, velocity.z))
    }

    /// Atomically replaces the static collision geometry for a region.
    pub fn replace_static_boxes(&mut self, region: StaticRegionId, boxes: &[StaticBox]) -> bool {
        if boxes.iter().any(|item| !valid_static_box(*item)) {
            return false;
        }
        self.remove_static_region(region);

        let handles: Vec<_> = boxes
            .iter()
            .map(|item| {
                let collider = ColliderBuilder::cuboid(
                    item.half_extents.x,
                    item.half_extents.y,
                    item.half_extents.z,
                )
                .translation(to_rapier_vec(item.center))
                .friction(item.friction)
                .restitution(item.restitution)
                .build();
                self.colliders.insert(collider)
            })
            .collect();
        for handle in &handles {
            self.static_collider_regions.insert(*handle, region);
        }
        self.static_regions.insert(region, handles);
        true
    }

    /// Greedy-meshes occupied blocks and replaces a region's static geometry.
    pub fn replace_static_voxels(&mut self, region: StaticRegionId, voxels: &VoxelRegion) -> usize {
        let boxes = voxels.merged_boxes();
        let count = boxes.len();
        let replaced = self.replace_static_boxes(region, &boxes);
        debug_assert!(replaced, "voxel meshing must only emit valid boxes");
        count
    }

    pub fn remove_static_region(&mut self, region: StaticRegionId) -> bool {
        let Some(handles) = self.static_regions.remove(&region) else {
            return false;
        };
        for handle in handles {
            self.static_collider_regions.remove(&handle);
            self.colliders
                .remove(handle, &mut self.islands, &mut self.bodies, true);
        }
        true
    }

    #[must_use]
    pub fn body_count(&self) -> usize {
        self.bodies.len()
    }

    #[must_use]
    pub fn collider_count(&self) -> usize {
        self.colliders.len()
    }

    fn insert_body(&mut self, spec: BodySpec) -> Option<RigidBodyHandle> {
        let rotation = to_rapier_rotation(spec.transform.rotation)?;
        let body = match spec.kind {
            BodyKind::Dynamic => RigidBodyBuilder::dynamic(),
            BodyKind::Kinematic => RigidBodyBuilder::kinematic_position_based(),
        }
        .translation(to_rapier_vec(spec.transform.position))
        .rotation(rotation.to_scaled_axis())
        .linear_damping(spec.linear_damping)
        .angular_damping(spec.angular_damping)
        .ccd_enabled(spec.continuous_collision_detection)
        .can_sleep(spec.can_sleep)
        .build();
        Some(self.bodies.insert(body))
    }
}

impl Default for PhysicsWorld {
    fn default() -> Self {
        Self::new(PhysicsConfig::default())
    }
}

const fn to_rapier_vec(value: Vec3) -> RapierVector {
    RapierVector::new(value.x, value.y, value.z)
}

fn to_rapier_rotation(value: Rotation) -> Option<RapierRotation> {
    let quaternion = RapierRotation::from_xyzw(value.x, value.y, value.z, value.w);
    let norm_squared = quaternion.length_squared();
    (norm_squared.is_finite() && norm_squared > f32::EPSILON).then(|| quaternion.normalize())
}

fn valid_spec(spec: BodySpec) -> bool {
    let shape_valid = match spec.shape {
        Shape::Cuboid { half_extents } => positive_vec(half_extents),
        Shape::Ball { radius } => radius.is_finite() && radius > 0.0,
        Shape::CapsuleY {
            half_height,
            radius,
        }
        | Shape::CylinderY {
            half_height,
            radius,
        } => half_height.is_finite() && half_height > 0.0 && radius.is_finite() && radius > 0.0,
    };
    shape_valid
        && finite_vec(spec.transform.position)
        && spec.mass.is_finite()
        && spec.mass > 0.0
        && spec.friction.is_finite()
        && spec.friction >= 0.0
        && spec.restitution.is_finite()
        && spec.restitution >= 0.0
        && spec.linear_damping.is_finite()
        && spec.linear_damping >= 0.0
        && spec.angular_damping.is_finite()
        && spec.angular_damping >= 0.0
}

fn collider_builder(shape: Shape) -> ColliderBuilder {
    match shape {
        Shape::Cuboid { half_extents } => {
            ColliderBuilder::cuboid(half_extents.x, half_extents.y, half_extents.z)
        }
        Shape::Ball { radius } => ColliderBuilder::ball(radius),
        Shape::CapsuleY {
            half_height,
            radius,
        } => ColliderBuilder::capsule_y(half_height, radius),
        Shape::CylinderY {
            half_height,
            radius,
        } => ColliderBuilder::cylinder(half_height, radius),
    }
}

fn valid_compound_parts(parts: &[CompoundPart]) -> bool {
    let mut mass_fraction_sum = 0.0;
    for part in parts {
        let shape_valid = match part.shape {
            Shape::Cuboid { half_extents } => positive_vec(half_extents),
            Shape::Ball { radius } => radius.is_finite() && radius > 0.0,
            Shape::CapsuleY {
                half_height,
                radius,
            }
            | Shape::CylinderY {
                half_height,
                radius,
            } => half_height.is_finite() && half_height > 0.0 && radius.is_finite() && radius > 0.0,
        };
        if !shape_valid
            || !finite_vec(part.transform.position)
            || to_rapier_rotation(part.transform.rotation).is_none()
            || !part.mass_fraction.is_finite()
            || part.mass_fraction <= 0.0
        {
            return false;
        }
        mass_fraction_sum += part.mass_fraction;
    }
    (mass_fraction_sum - 1.0).abs() <= 0.001
}

fn valid_static_box(item: StaticBox) -> bool {
    finite_vec(item.center)
        && positive_vec(item.half_extents)
        && item.friction.is_finite()
        && item.friction >= 0.0
        && item.restitution.is_finite()
        && item.restitution >= 0.0
}

const fn finite_vec(value: Vec3) -> bool {
    value.x.is_finite() && value.y.is_finite() && value.z.is_finite()
}

fn positive_vec(value: Vec3) -> bool {
    finite_vec(value) && value.x > 0.0 && value.y > 0.0 && value.z > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_falls_and_can_be_removed() {
        let mut world = PhysicsWorld::default();
        let body = world
            .spawn(BodySpec::ball(Vec3::new(0.0, 10.0, 0.0), 0.5))
            .expect("valid body");

        world.step();

        assert!(world.transform(body).expect("body exists").position.y < 10.0);
        assert_eq!(world.body_count(), 1);
        assert!(world.remove(body));
        assert_eq!(world.body_count(), 0);
    }

    #[test]
    fn static_floor_stops_a_body() {
        let mut world = PhysicsWorld::default();
        assert!(world.replace_static_boxes(
            StaticRegionId(1),
            &[StaticBox::new(
                Vec3::new(0.0, -0.5, 0.0),
                Vec3::new(8.0, 0.5, 8.0),
            )],
        ));
        let body = world
            .spawn(BodySpec::cuboid(
                Vec3::new(0.0, 3.0, 0.0),
                Vec3::new(0.5, 0.5, 0.5),
            ))
            .expect("valid body");

        for _ in 0..100 {
            world.step();
        }

        let y = world.transform(body).expect("body exists").position.y;
        assert!((0.45..=0.6).contains(&y), "body settled at {y}");
    }

    #[test]
    fn invalid_body_is_rejected_without_mutation() {
        let mut world = PhysicsWorld::default();
        let invalid = BodySpec::ball(Vec3::default(), -1.0);
        assert!(world.spawn(invalid).is_none());
        assert_eq!(world.body_count(), 0);
    }

    #[test]
    fn raycast_reports_dynamic_body_and_static_region() {
        let mut world = PhysicsWorld::new(PhysicsConfig {
            gravity: Vec3::default(),
            ..PhysicsConfig::default()
        });
        let body = world
            .spawn(BodySpec::ball(Vec3::new(0.0, 2.0, 0.0), 0.5))
            .expect("valid body");
        world.step();
        let hit = world
            .cast_ray(Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.0, -1.0, 0.0), 10.0)
            .expect("ray hit");
        assert_eq!(hit.body, Some(body));
        assert_eq!(hit.static_region, None);
    }

    #[test]
    fn fixed_joint_can_be_created_and_removed() {
        let mut world = PhysicsWorld::default();
        let first = world
            .spawn(BodySpec::ball(Vec3::new(0.0, 1.0, 0.0), 0.5))
            .expect("valid body");
        let second = world
            .spawn(BodySpec::ball(Vec3::new(0.0, 2.0, 0.0), 0.5))
            .expect("valid body");
        let joint = world
            .add_fixed_joint(first, second, Vec3::default(), Vec3::default())
            .expect("valid joint");
        assert!(world.remove_joint(joint));
    }

    #[test]
    fn compound_body_builds_multiple_colliders() {
        let mut world = PhysicsWorld::default();
        let parts = [
            CompoundPart::new(
                Vec3::new(-0.5, 0.0, 0.0),
                Shape::Cuboid {
                    half_extents: Vec3::new(0.5, 0.25, 0.25),
                },
                0.5,
            ),
            CompoundPart::new(Vec3::new(0.5, 0.0, 0.0), Shape::Ball { radius: 0.25 }, 0.5),
        ];
        let handle = world.spawn_compound(
            BodySpec::cuboid(Vec3::new(0.0, 2.0, 0.0), Vec3::new(1.0, 1.0, 1.0)),
            &parts,
        );
        assert!(handle.is_some());
        assert_eq!(world.collider_count(), 2);
    }
}
