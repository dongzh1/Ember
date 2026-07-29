# Ember Physics

An isolated Rapier-based rigid-body layer for Ember. The Pumpkin adapter lives
in `pumpkin/src/server/physics.rs`; existing entity movement is not replaced.
Upstream behavior therefore stays unchanged when physics is disabled.

```rust
use ember_physics::{BodySpec, PhysicsWorld, Vec3};

let mut physics = PhysicsWorld::default();
let body = physics
    .spawn(BodySpec::cuboid(
        Vec3::new(0.0, 10.0, 0.0),
        Vec3::new(0.5, 0.5, 0.5),
    ))
    .expect("valid body");

physics.apply_impulse(body, Vec3::new(0.0, 2.0, 0.0));
physics.step();
let transform = physics.transform(body).expect("body exists");
```

The engine supports:

- dynamic and position-based kinematic bodies;
- cuboid, ball, capsule, cylinder, and compound colliders;
- automatic mass properties and inertia tensors;
- impulses, torque impulses, force-at-point, fixed joints, and ray casts;
- buoyancy/drag responses for custom water, lava, or other media;
- sleeping and per-body continuous collision detection;
- replaceable static regions and greedy voxel-to-AABB meshing.

## Server configuration

Ember creates these files on startup:

- `physics/physics.toml`: master switch, frequency, gravity, limits, and terrain cache;
- `physics/materials.toml`: friction, restitution, density, and damping;
- `physics/presets.toml`: named shapes and their material/mass.

The feature defaults to `enabled = false`. Enable it and restart the server,
then use `/physics status` to inspect each world's body/collider counts.

Built-in Ember systems can spawn a configured preset without depending on
Rapier types:

```rust
let handle = world.physics_manager.spawn_preset(
    "block",
    ember_physics::Vec3::new(0.0, 80.0, 0.0),
)?;
```

Use `world.physics_manager.with_world(...)` for forces, joints, ray queries,
compound bodies, and transform snapshots. A gameplay system remains
responsible for associating each `BodyHandle` with its real or packet-only
Minecraft display entity; this separation prevents the physics layer from
changing vanilla entity semantics.

Terrain collision is generated only for active, already-loaded chunk sections.
Full blocks are greedily merged; slabs, stairs, fences, and other non-full
blocks use Pumpkin's generated collision boxes. Block changes invalidate only
the containing section.
