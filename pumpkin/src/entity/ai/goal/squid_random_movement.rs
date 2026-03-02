use std::sync::atomic::Ordering;

use super::{Controls, Goal, GoalFuture, to_goal_ticks};
use crate::entity::ai::goal::ParentHandle;
use crate::entity::mob::Mob;
use crate::entity::mob::squid::SquidEntity;
use rand::RngExt;

/// Always active (canUse → true). Picks random swim directions periodically.
/// When idle too long (noActionTime > 100), zeros movement.
pub struct SquidRandomMovementGoal {
    pub parent: ParentHandle<SquidEntity>,
}

impl Default for SquidRandomMovementGoal {
    fn default() -> Self {
        Self::new()
    }
}

impl SquidRandomMovementGoal {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            parent: ParentHandle::none(),
        }
    }
}

impl Goal for SquidRandomMovementGoal {
    fn can_start<'a>(&'a mut self, _mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async { true })
    }

    fn tick<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            let Some(squid) = self.parent.get() else {
                return;
            };

            let entity = &squid.mob_entity.living_entity.entity;
            let no_action_time = squid.no_action_time.load(Ordering::Relaxed);

            if no_action_time > 100 {
                let mut mv = squid.movement_vector.lock().await;
                *mv = pumpkin_util::math::vector3::Vector3::new(0.0, 0.0, 0.0);
            } else {
                let was_touching_water = entity.was_touching_water.load(Ordering::SeqCst);
                let has_movement = squid.has_movement_vector().await;

                let new_direction = {
                    let mut rng = mob.get_random();
                    let should_pick = rng.random_range(0i32..to_goal_ticks(50)) == 0
                        || !was_touching_water
                        || !has_movement;
                    should_pick.then(|| {
                        let angle = rng.random::<f32>() * (std::f32::consts::PI * 2.0);
                        pumpkin_util::math::vector3::Vector3::new(
                            f64::from(angle.cos() * 0.2),
                            f64::from(-0.1 + rng.random::<f32>() * 0.2),
                            f64::from(angle.sin() * 0.2),
                        )
                    })
                };

                if let Some(dir) = new_direction {
                    let mut mv = squid.movement_vector.lock().await;
                    *mv = dir;
                }
            }
        })
    }

    fn controls(&self) -> Controls {
        Controls::empty()
    }
}
