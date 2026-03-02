use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use crate::entity::ai::goal::ParentHandle;
use crate::entity::ai::goal::squid_flee::SquidFleeGoal;
use crate::entity::ai::goal::squid_random_movement::SquidRandomMovementGoal;
use crate::entity::attributes::AttributeBuilder;
use crate::entity::mob::{Mob, MobEntity};
use crate::entity::{Entity, EntityBase, EntityBaseFuture, NBTStorage, NbtFuture};
use pumpkin_data::attributes::Attributes;
use pumpkin_data::damage::DamageType;
use pumpkin_data::sound::Sound;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::java::client::play::CEntityStatus;
use pumpkin_util::math::vector3::Vector3;
use tokio::sync::Mutex;

pub struct SquidEntity {
    pub mob_entity: MobEntity,
    pub movement_vector: Mutex<Vector3<f64>>,
    pub last_hurt_by_mob: Mutex<Option<Arc<dyn EntityBase>>>,
    pub no_action_time: AtomicI32,
    tentacle_movement: Mutex<f32>,
    tentacle_speed: Mutex<f32>,
    rotate_speed: Mutex<f32>,
}

impl SquidEntity {
    pub async fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);

        let tentacle_speed = {
            let mut rng = rand::rng();
            1.0 / (rand::RngExt::random::<f32>(&mut rng) + 1.0) * 0.2
        };

        let squid = Self {
            mob_entity,
            movement_vector: Mutex::new(Vector3::new(0.0, 0.0, 0.0)),
            last_hurt_by_mob: Mutex::new(None),
            no_action_time: AtomicI32::new(0),
            tentacle_movement: Mutex::new(0.0),
            tentacle_speed: Mutex::new(tentacle_speed),
            rotate_speed: Mutex::new(0.0),
        };

        let mob_arc = Arc::new(squid);

        {
            let mut goal_selector = mob_arc.mob_entity.goals_selector.lock().await;

            let mut random_goal = SquidRandomMovementGoal::new();
            random_goal.parent = unsafe { ParentHandle::new(&mob_arc) };
            goal_selector.add_goal(0, Box::new(random_goal));

            let mut flee_goal = SquidFleeGoal::new();
            flee_goal.parent = unsafe { ParentHandle::new(&mob_arc) };
            goal_selector.add_goal(1, Box::new(flee_goal));
        };

        mob_arc
    }

    #[must_use]
    pub fn create_attributes() -> AttributeBuilder {
        AttributeBuilder::new().add(Attributes::MAX_HEALTH, 10.0)
    }

    pub async fn has_movement_vector(&self) -> bool {
        let mv = self.movement_vector.lock().await;
        mv.length_squared() > 1.0e-5
    }
}

impl NBTStorage for SquidEntity {
    fn write_nbt<'a>(&'a self, nbt: &'a mut NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async move {
            self.mob_entity.living_entity.entity.write_nbt(nbt).await;
        })
    }

    fn read_nbt_non_mut<'a>(&'a self, nbt: &'a NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async move {
            self.mob_entity
                .living_entity
                .entity
                .read_nbt_non_mut(nbt)
                .await;
        })
    }
}

impl Mob for SquidEntity {
    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_tick<'a>(&'a self, _caller: &'a Arc<dyn EntityBase>) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {
            let entity = &self.mob_entity.living_entity.entity;

            self.no_action_time.fetch_add(1, Ordering::Relaxed);

            let mut tentacle_movement = self.tentacle_movement.lock().await;
            let mut tentacle_speed = self.tentacle_speed.lock().await;
            let mut rotate_speed = self.rotate_speed.lock().await;

            *tentacle_movement += *tentacle_speed;

            if *tentacle_movement > std::f32::consts::TAU {
                *tentacle_movement -= std::f32::consts::TAU;
                {
                    let mut rng = rand::rng();
                    if rand::RngExt::random_range(&mut rng, 0i32..10) == 0 {
                        *tentacle_speed = 1.0 / (rand::RngExt::random::<f32>(&mut rng) + 1.0) * 0.2;
                    }
                }

                let world = entity.world.load();
                world
                    .broadcast_packet_all(&CEntityStatus::new(entity.entity_id, 19))
                    .await;
            }

            let in_water = entity.touching_water.load(Ordering::SeqCst);

            if in_water {
                if *tentacle_movement < std::f32::consts::PI {
                    let tentacle_scale = *tentacle_movement / std::f32::consts::PI;

                    if tentacle_scale > 0.75 {
                        let mv = self.movement_vector.lock().await;
                        entity.velocity.store(*mv);
                        *rotate_speed = 1.0;
                    } else {
                        *rotate_speed *= 0.8;
                    }
                } else {
                    let vel = entity.velocity.load();
                    entity
                        .velocity
                        .store(Vector3::new(vel.x * 0.9, vel.y * 0.9, vel.z * 0.9));
                    *rotate_speed *= 0.99;
                }

                let vel = entity.velocity.load();
                let horizontal_dist = vel.x.hypot(vel.z);

                let target_yaw = -(vel.x.atan2(vel.z) as f32) * (180.0 / std::f32::consts::PI);
                let current_yaw = entity.body_yaw.load();
                let new_yaw = current_yaw + (target_yaw - current_yaw) * 0.1;
                entity.body_yaw.store(new_yaw);
                entity.yaw.store(new_yaw);

                let _ = horizontal_dist;
            } else {
                let vel = entity.velocity.load();
                let yd = vel.y - self.get_mob_gravity();
                entity.velocity.store(Vector3::new(0.0, yd * 0.98, 0.0));
            }

            drop(tentacle_movement);
            drop(tentacle_speed);
            drop(rotate_speed);
        })
    }

    fn post_tick(&self) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {})
    }

    fn get_mob_gravity(&self) -> f64 {
        0.08
    }

    fn on_damage<'a>(
        &'a self,
        _damage_type: DamageType,
        source: Option<&'a dyn EntityBase>,
    ) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {
            if let Some(_attacker) = source {
                // TODO: properly track last_hurt_by_mob
            }
            let entity = &self.mob_entity.living_entity.entity;
            entity.play_sound(Sound::EntitySquidSquirt).await;
            // TODO: spawn SQUID_INK particles
        })
    }
}
