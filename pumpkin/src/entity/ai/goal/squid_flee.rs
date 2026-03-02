use std::sync::atomic::Ordering;

use super::{Controls, Goal, GoalFuture};
use crate::entity::ai::goal::ParentHandle;
use crate::entity::mob::Mob;
use crate::entity::mob::squid::SquidEntity;
use pumpkin_util::math::vector3::Vector3;

/// Flees from the last entity that hurt the squid. Active only when in water
/// and within 10 blocks of the attacker.
pub struct SquidFleeGoal {
    pub parent: ParentHandle<SquidEntity>,
    flee_ticks: i32,
}

impl Default for SquidFleeGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl SquidFleeGoal {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            parent: ParentHandle::none(),
            flee_ticks: 0,
        }
    }
}

impl Goal for SquidFleeGoal {
    fn can_start<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async move {
            let entity = &mob.get_mob_entity().living_entity.entity;
            let in_water = entity.touching_water.load(Ordering::SeqCst);
            if !in_water {
                return false;
            }

            let Some(squid) = self.parent.get() else {
                return false;
            };

            let last_attacker = squid.last_hurt_by_mob.lock().await;
            last_attacker.as_ref().is_some_and(|attacker| {
                let squid_pos = entity.pos.load();
                let attacker_pos = attacker.get_entity().pos.load();
                let dist_sq = squid_pos.sub(&attacker_pos).length_squared();
                dist_sq < 100.0
            })
        })
    }

    fn start<'a>(&'a mut self, _mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            self.flee_ticks = 0;
        })
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn tick<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            self.flee_ticks += 1;

            let Some(squid) = self.parent.get() else {
                return;
            };

            let entity = &mob.get_mob_entity().living_entity.entity;
            let last_attacker = squid.last_hurt_by_mob.lock().await;

            let Some(attacker) = last_attacker.as_ref() else {
                return;
            };

            let squid_pos = entity.pos.load();
            let attacker_pos = attacker.get_entity().pos.load();

            let flee_x = squid_pos.x - attacker_pos.x;
            let flee_y = squid_pos.y - attacker_pos.y;
            let flee_z = squid_pos.z - attacker_pos.z;
            let target_pos = pumpkin_util::math::position::BlockPos::new(
                (squid_pos.x + flee_x) as i32,
                (squid_pos.y + flee_y) as i32,
                (squid_pos.z + flee_z) as i32,
            );

            let world = entity.world.load();
            let block = world.get_block(&target_pos).await;
            let is_water = block == &pumpkin_data::Block::WATER;
            let is_air = block.is_air();

            if is_water || is_air {
                let length = (flee_x * flee_x + flee_y * flee_y + flee_z * flee_z).sqrt();

                let mut flee = Vector3::new(flee_x, flee_y, flee_z);

                if length > 0.0 {
                    let mut avoid_speed: f64 = 3.0;
                    if length > 5.0 {
                        avoid_speed -= (length - 5.0) / 5.0;
                    }

                    if avoid_speed > 0.0 {
                        flee = Vector3::new(
                            flee.x * avoid_speed,
                            flee.y * avoid_speed,
                            flee.z * avoid_speed,
                        );
                    }
                }

                if is_air {
                    flee = Vector3::new(flee.x, 0.0, flee.z);
                }
                let mut mv = squid.movement_vector.lock().await;
                *mv = Vector3::new(flee.x / 20.0, flee.y / 20.0, flee.z / 20.0);
            }

            // TODO: Spawn BUBBLE particle when particle system is available
            // if self.flee_ticks % 10 == 5 { ... }
        })
    }

    fn controls(&self) -> Controls {
        Controls::empty()
    }
}
