//! Non-player entities: a small deterministic mob population with gravity,
//! block collision and a wander/chase AI.
//!
//! Entities live outside [`World`](crate::World) so the block world can be
//! borrowed mutably while a mob is stepped. Everything here is a pure function
//! of the world seed and the entity ids, so the same seed produces the same
//! mobs in the same places, which keeps tests reproducible. Entity ids are
//! allocated from a separate high range ([`MOB_ID_BASE`]) because player entity
//! ids are handed out sequentially by the network layer.

use std::collections::HashMap;

use crate::encode::BLOCK_AIR;
use crate::World;

/// Player entity ids count up from zero, so mobs start far above them.
pub const MOB_ID_BASE: i32 = 1_000_000;

/// Downward acceleration per tick.
const GRAVITY: f64 = 0.08;
/// Terminal fall speed in blocks per tick.
const MAX_FALL: f64 = 3.0;
/// Horizontal speed while walking, in blocks per tick.
const WALK_SPEED: f64 = 0.12;
/// How far a passive mob may drift from where it was spawned.
const LEASH_RADIUS: f64 = 12.0;
/// The range at which a hostile mob notices a player.
const CHASE_RADIUS: f64 = 16.0;

/// The kinds of mob LoadstoneMC can spawn. The numeric entity type ids are the
/// `minecraft:entity_type` registry protocol ids reported by a real 1.21.11
/// server's data generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKind {
    Pig,
    Cow,
    Sheep,
    Chicken,
    Zombie,
}

impl EntityKind {
    /// Every kind, in a stable order.
    pub const ALL: [EntityKind; 5] = [
        EntityKind::Pig,
        EntityKind::Cow,
        EntityKind::Sheep,
        EntityKind::Chicken,
        EntityKind::Zombie,
    ];

    /// The registry protocol id sent in the `add_entity` packet.
    pub fn entity_type_id(self) -> i32 {
        match self {
            EntityKind::Pig => 100,
            EntityKind::Cow => 30,
            EntityKind::Sheep => 111,
            EntityKind::Chicken => 26,
            EntityKind::Zombie => 150,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            EntityKind::Pig => "minecraft:pig",
            EntityKind::Cow => "minecraft:cow",
            EntityKind::Sheep => "minecraft:sheep",
            EntityKind::Chicken => "minecraft:chicken",
            EntityKind::Zombie => "minecraft:zombie",
        }
    }

    /// Collision-box width in blocks.
    pub fn width(self) -> f64 {
        match self {
            EntityKind::Chicken => 0.4,
            EntityKind::Zombie => 0.6,
            _ => 0.9,
        }
    }

    /// Collision-box height in blocks.
    pub fn height(self) -> f64 {
        match self {
            EntityKind::Chicken => 0.7,
            EntityKind::Pig => 0.9,
            EntityKind::Sheep => 1.3,
            EntityKind::Cow => 1.4,
            EntityKind::Zombie => 1.95,
        }
    }

    pub fn walk_speed(self) -> f64 {
        match self {
            EntityKind::Zombie => 0.14,
            _ => WALK_SPEED,
        }
    }

    /// Hostile mobs walk towards nearby players instead of wandering.
    pub fn hostile(self) -> bool {
        matches!(self, EntityKind::Zombie)
    }
}

/// One simulated mob.
#[derive(Debug, Clone)]
pub struct Entity {
    pub id: i32,
    pub uuid: [u8; 16],
    pub kind: EntityKind,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub vx: f64,
    pub vy: f64,
    pub vz: f64,
    /// Body yaw in degrees (0 faces +Z).
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    /// Whether the last step changed the position or yaw enough to broadcast.
    pub moved: bool,
    home: (f64, f64),
    walking: bool,
    wander_ticks: u32,
    rng: u64,
}

impl Entity {
    fn new(id: i32, kind: EntityKind, x: f64, y: f64, z: f64, seed: u64) -> Self {
        let mut uuid = [0u8; 16];
        let mut state = seed ^ (id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        for chunk in uuid.chunks_mut(8) {
            let value = splitmix64(&mut state).to_be_bytes();
            chunk.copy_from_slice(&value[..chunk.len()]);
        }
        // Stamp RFC 4122 version 4 / variant bits so clients treat it as random.
        uuid[6] = (uuid[6] & 0x0f) | 0x40;
        uuid[8] = (uuid[8] & 0x3f) | 0x80;

        Self {
            id,
            uuid,
            kind,
            x,
            y,
            z,
            vx: 0.0,
            vy: 0.0,
            vz: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
            moved: false,
            home: (x, z),
            walking: false,
            wander_ticks: 0,
            rng: state | 1,
        }
    }

    fn next_u32(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        (x >> 16) as u32
    }

    /// Advances the mob by one server tick (50 ms).
    pub fn step(&mut self, world: &mut World, targets: &[(f64, f64, f64)]) {
        let before = (self.x, self.y, self.z, self.yaw);

        let chase = if self.kind.hostile() {
            self.nearest_target(targets)
        } else {
            None
        };

        if let Some((dx, dz)) = chase {
            self.face(dx, dz);
        } else {
            if self.wander_ticks == 0 {
                let roll = self.next_u32();
                if roll.is_multiple_of(4) {
                    self.walking = false;
                } else {
                    self.walking = true;
                    self.yaw = ((roll >> 8) % 360) as f32;
                }
                self.wander_ticks = 40 + (roll >> 16) % 80;
            }
            self.wander_ticks -= 1;

            // Turn back if we have drifted too far from home.
            let (home_dx, home_dz) = (self.home.0 - self.x, self.home.1 - self.z);
            if home_dx * home_dx + home_dz * home_dz > LEASH_RADIUS * LEASH_RADIUS {
                self.walking = true;
                self.face(home_dx, home_dz);
            }
        }

        let speed = self.kind.walk_speed();
        if self.walking || chase.is_some() {
            let yaw = f64::from(self.yaw).to_radians();
            self.vx = -yaw.sin() * speed;
            self.vz = yaw.cos() * speed;
        } else {
            self.vx = 0.0;
            self.vz = 0.0;
        }

        self.vy = (self.vy - GRAVITY).max(-MAX_FALL);
        self.move_horizontal(world, self.vx, self.vz);
        self.move_vertical(world, self.vy);

        self.moved = (self.x - before.0).abs() > 1e-4
            || (self.y - before.1).abs() > 1e-4
            || (self.z - before.2).abs() > 1e-4
            || (self.yaw - before.3).abs() > 0.5;
    }

    fn face(&mut self, dx: f64, dz: f64) {
        let length = (dx * dx + dz * dz).sqrt();
        if length > 1e-6 {
            // yaw 0 looks along +Z, so the direction is (-sin, cos).
            self.yaw = (-dx / length).atan2(dz / length).to_degrees() as f32;
        }
    }

    fn nearest_target(&self, targets: &[(f64, f64, f64)]) -> Option<(f64, f64)> {
        let mut best = CHASE_RADIUS * CHASE_RADIUS;
        let mut found = None;
        for &(x, _, z) in targets {
            let (dx, dz) = (x - self.x, z - self.z);
            let distance = dx * dx + dz * dz;
            if distance < best {
                best = distance;
                found = Some((dx, dz));
            }
        }
        found
    }

    fn move_horizontal(&mut self, world: &mut World, dx: f64, dz: f64) {
        if dx != 0.0 {
            if self.collides(world, self.x + dx, self.y, self.z) {
                self.vx = 0.0;
            } else {
                self.x += dx;
            }
        }
        if dz != 0.0 {
            if self.collides(world, self.x, self.y, self.z + dz) {
                self.vz = 0.0;
            } else {
                self.z += dz;
            }
        }
    }

    fn move_vertical(&mut self, world: &mut World, dy: f64) {
        if dy == 0.0 {
            return;
        }
        if self.collides(world, self.x, self.y + dy, self.z) {
            if dy < 0.0 {
                self.on_ground = true;
            }
            self.vy = 0.0;
        } else {
            self.y += dy;
            self.on_ground = false;
        }
    }

    /// Whether the mob's collision box overlaps a solid block at `(x, y, z)`,
    /// with `(x, y, z)` the feet position.
    fn collides(&self, world: &mut World, x: f64, y: f64, z: f64) -> bool {
        let half = self.kind.width() / 2.0;
        let min_x = (x - half).floor() as i32;
        let max_x = (x + half).floor() as i32;
        let min_y = y.floor() as i32;
        let max_y = (y + self.kind.height() - 1e-6).floor() as i32;
        let min_z = (z - half).floor() as i32;
        let max_z = (z + half).floor() as i32;

        for bx in min_x..=max_x {
            for by in min_y..=max_y {
                for bz in min_z..=max_z {
                    if world.get_block(bx, by, bz) != BLOCK_AIR {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// The shared set of simulated mobs.
#[derive(Debug)]
pub struct EntityStore {
    next_id: i32,
    mobs: HashMap<i32, Entity>,
}

impl Default for EntityStore {
    fn default() -> Self {
        Self::new()
    }
}

impl EntityStore {
    pub fn new() -> Self {
        Self {
            next_id: MOB_ID_BASE,
            mobs: HashMap::new(),
        }
    }

    /// Spawns the deterministic starting population around the world spawn.
    pub fn populate(&mut self, world: &mut World) {
        if !self.mobs.is_empty() {
            return;
        }
        let (base_x, _, base_z) = world.spawn();
        let seed = world.seed();
        for &(kind, dx, dz) in INITIAL_MOBS {
            let x = base_x + dx;
            let z = base_z + dz;
            let y = f64::from(world.surface_height(x.floor() as i32, z.floor() as i32) + 1);
            self.spawn(kind, x, y, z, seed);
        }
    }

    /// Adds one mob and returns its entity id.
    pub fn spawn(&mut self, kind: EntityKind, x: f64, y: f64, z: f64, seed: u64) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        self.mobs.insert(id, Entity::new(id, kind, x, y, z, seed));
        id
    }

    /// Steps every mob with the block world and the current player positions.
    pub fn tick(&mut self, world: &mut World, targets: &[(f64, f64, f64)]) {
        for mob in self.mobs.values_mut() {
            mob.step(world, targets);
        }
    }

    pub fn get(&self, id: i32) -> Option<&Entity> {
        self.mobs.get(&id)
    }

    pub fn mobs(&self) -> impl Iterator<Item = &Entity> {
        self.mobs.values()
    }

    pub fn len(&self) -> usize {
        self.mobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.mobs.is_empty()
    }
}

/// The starting mobs, as offsets from the world spawn.
const INITIAL_MOBS: &[(EntityKind, f64, f64)] = &[
    (EntityKind::Pig, -4.0, -4.0),
    (EntityKind::Pig, 4.0, 3.0),
    (EntityKind::Cow, -2.0, 5.0),
    (EntityKind::Sheep, 6.0, -2.0),
    (EntityKind::Chicken, -6.0, 1.0),
    (EntityKind::Chicken, 6.0, 2.0),
    (EntityKind::Zombie, 2.0, 6.0),
];

/// SplitMix64, used only to derive deterministic block bytes.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn populated() -> (World, EntityStore) {
        let mut world = World::with_seed(0);
        let mut entities = EntityStore::new();
        entities.populate(&mut world);
        (world, entities)
    }

    #[test]
    fn population_is_deterministic_and_lands_on_the_surface() {
        let (world, entities) = populated();
        assert_eq!(entities.len(), INITIAL_MOBS.len());
        assert_eq!(entities.mobs().map(|m| m.id).min(), Some(MOB_ID_BASE));

        let mut other = World::with_seed(0);
        let mut again = EntityStore::new();
        again.populate(&mut other);
        let mut first: Vec<_> = entities
            .mobs()
            .map(|m| (m.id, m.uuid, m.x, m.y, m.z))
            .collect();
        let mut second: Vec<_> = again
            .mobs()
            .map(|m| (m.id, m.uuid, m.x, m.y, m.z))
            .collect();
        first.sort_by_key(|entry| entry.0);
        second.sort_by_key(|entry| entry.0);
        assert_eq!(first, second);

        for mob in entities.mobs() {
            let surface = world.surface_height(mob.x.floor() as i32, mob.z.floor() as i32) + 1;
            assert_eq!(
                mob.y,
                f64::from(surface),
                "{} not on the surface",
                mob.kind.name()
            );
        }
    }

    #[test]
    fn mobs_settle_on_the_ground_and_stay_near_home() {
        let (mut world, mut entities) = populated();
        for _ in 0..200 {
            entities.tick(&mut world, &[]);
        }
        for mob in entities.mobs() {
            assert!(mob.on_ground, "{} never landed", mob.kind.name());
            let dx = mob.x - mob.home.0;
            let dz = mob.z - mob.home.1;
            assert!(
                dx * dx + dz * dz <= (LEASH_RADIUS + 1.0) * (LEASH_RADIUS + 1.0),
                "{} wandered too far",
                mob.kind.name()
            );
        }
    }

    #[test]
    fn hostile_mobs_walk_towards_a_nearby_player() {
        let mut world = World::with_seed(4);
        let mut entities = EntityStore::new();
        let surface = world.surface_height(8, 8);
        let zombie = entities.spawn(EntityKind::Zombie, 8.5, f64::from(surface + 1), 8.5, 4);

        // Let it settle on the ground before measuring the chase.
        for _ in 0..40 {
            entities.tick(&mut world, &[]);
        }
        let start = entities.get(zombie).unwrap().x;
        let mob_z = entities.get(zombie).unwrap().z;
        let target = [(start + 8.0, f64::from(surface + 1), mob_z)];
        entities.tick(&mut world, &target);
        let mob = entities.get(zombie).unwrap();
        // yaw 0 faces +Z, so walking east is yaw -90 degrees.
        assert!(
            (mob.yaw + 90.0).abs() < 1.0,
            "zombie should face the player, yaw {}",
            mob.yaw
        );
        assert!(mob.vx > 0.0, "zombie should accelerate towards the player");
    }
}
