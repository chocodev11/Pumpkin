use std::{cell::RefCell, sync::Arc};

use pumpkin_data::{
    BlockDirection, block_properties::blocks_movement, dimension::Dimension, entity::EntityPose,
};
use pumpkin_util::{
    GameMode,
    math::{boundingbox::BoundingBox, position::BlockPos, vector2::Vector2, vector3::Vector3},
};
use pumpkin_world::{
    biome::position_finder::FittestPositionFinder,
    chunk::ChunkHeightmapType,
    generation::{
        biome_coords,
        noise::router::multi_noise_sampler::{MultiNoiseSampler, MultiNoiseSamplerBuilderOptions},
    },
    world_info::RespawnData,
};
use rand::{RngExt, rng};

use crate::{entity::Entity, world::World};

const INITIAL_SPAWN_CHUNK_RADIUS: i32 = 5;
const INITIAL_SPAWN_CHUNK_DIAMETER: i32 = 11;
const ABSOLUTE_MAX_ATTEMPTS: i32 = 1024;

pub struct InitialSpawnResult {
    pub climate_chunk: Vector2<i32>,
    pub spawn: BlockPos,
}

pub fn default_spawn_world(world: &World) -> Option<Arc<World>> {
    let server = world.server.upgrade()?;
    let dimension = Dimension::from_name(world.level_info.load().spawn_dimension())
        .copied()
        .unwrap_or(Dimension::OVERWORLD);
    Some(server.get_world_from_dimension(&dimension))
}

pub async fn effective_default_spawn(world: &World) -> RespawnData {
    let stored_spawn = world.level_info.load().spawn.clone();
    let Some(spawn_world) = default_spawn_world(world) else {
        return stored_spawn;
    };

    let (contains_spawn, center_x, center_z) = {
        let border = spawn_world.worldborder.lock().await;
        (
            border.contains_block(stored_spawn.pos.0.x, stored_spawn.pos.0.z),
            border.center_x.floor() as i32,
            border.center_z.floor() as i32,
        )
    };

    if contains_spawn {
        stored_spawn
    } else {
        let y = get_heightmap_height(
            &spawn_world,
            ChunkHeightmapType::MotionBlocking,
            center_x,
            center_z,
        )
        .await;
        stored_spawn.with_pos(BlockPos::new(center_x, y, center_z))
    }
}

pub async fn compute_initial_world_spawn(world: &World) -> InitialSpawnResult {
    let climate_chunk = find_climate_spawn_chunk(world);
    let chunk_center_x = (climate_chunk.x << 4) + 8;
    let chunk_center_z = (climate_chunk.y << 4) + 8;

    let mut spawn_y = world.level.world_gen.spawn_height();
    if spawn_y < world.get_bottom_y() {
        spawn_y = get_heightmap_height(
            world,
            ChunkHeightmapType::WorldSurface,
            chunk_center_x,
            chunk_center_z,
        )
        .await;
    }

    let mut spawn = BlockPos::new(chunk_center_x, spawn_y, chunk_center_z);
    for offset in spiral_offsets(INITIAL_SPAWN_CHUNK_DIAMETER) {
        if offset.x.abs() > INITIAL_SPAWN_CHUNK_RADIUS
            || offset.y.abs() > INITIAL_SPAWN_CHUNK_RADIUS
        {
            continue;
        }

        if let Some(valid_spawn) = get_spawn_pos_in_chunk(
            world,
            Vector2::new(climate_chunk.x + offset.x, climate_chunk.y + offset.y),
        )
        .await
        {
            spawn = valid_spawn;
            break;
        }
    }

    InitialSpawnResult {
        climate_chunk,
        spawn,
    }
}

pub async fn find_world_spawn_position(world: &World, spawn_suggestion: BlockPos) -> Vector3<f64> {
    if !world.dimension.has_skylight || default_gamemode(world).await == GameMode::Adventure {
        return fixup_spawn_height(world, spawn_suggestion).await;
    }

    let mut radius = world.level_info.load().game_rules.respawn_radius.max(0) as i32;
    let distance_to_border = {
        let border = world.worldborder.lock().await;
        border
            .distance_to_border(
                f64::from(spawn_suggestion.0.x),
                f64::from(spawn_suggestion.0.z),
            )
            .floor() as i32
    };

    if distance_to_border < radius {
        radius = distance_to_border;
    }
    if distance_to_border <= 1 {
        radius = 1;
    }

    let square_side = radius * 2 + 1;
    let candidate_count =
        ((square_side as i64 * square_side as i64).min(i64::from(ABSOLUTE_MAX_ATTEMPTS))) as i32;
    let coprime = get_coprime(candidate_count);
    let offset = rng().random_range(0..candidate_count);

    for candidate_index in 0..candidate_count {
        let (target_x, target_z) =
            candidate_position(spawn_suggestion, radius, offset, coprime, candidate_index);
        if let Some(spawn_pos) = get_overworld_respawn_pos(world, target_x, target_z).await
            && no_collision_no_liquid(world, spawn_pos).await
        {
            return bottom_center(spawn_pos);
        }
    }

    fixup_spawn_height(world, spawn_suggestion).await
}

async fn default_gamemode(world: &World) -> GameMode {
    let Some(server) = world.server.upgrade() else {
        return GameMode::Survival;
    };
    server.defaultgamemode.lock().await.gamemode
}

fn find_climate_spawn_chunk(world: &World) -> Vector2<i32> {
    let generation_settings =
        pumpkin_data::chunk_gen_settings::GenerationSettings::from_dimension(&world.dimension);
    if generation_settings.spawn_target.is_empty() {
        return Vector2::new(0, 0);
    }

    let sampler = RefCell::new(MultiNoiseSampler::generate(
        &world.level.world_gen.base_router.multi_noise,
        &MultiNoiseSamplerBuilderOptions::new(0, 0, 0),
    ));

    let best_position = FittestPositionFinder::find_best_spawn_position(
        generation_settings.spawn_target,
        &|block_x, block_z| {
            sampler
                .borrow_mut()
                .sample(
                    biome_coords::from_block(block_x),
                    0,
                    biome_coords::from_block(block_z),
                )
                .convert_to_list()
        },
    );

    Vector2::new(best_position.x >> 4, best_position.y >> 4)
}

async fn get_spawn_pos_in_chunk(world: &World, chunk_pos: Vector2<i32>) -> Option<BlockPos> {
    let min_x = chunk_pos.x << 4;
    let min_z = chunk_pos.y << 4;

    for x in min_x..=min_x + 15 {
        for z in min_z..=min_z + 15 {
            if let Some(spawn_pos) = get_overworld_respawn_pos(world, x, z).await {
                return Some(spawn_pos);
            }
        }
    }

    None
}

async fn get_overworld_respawn_pos(world: &World, x: i32, z: i32) -> Option<BlockPos> {
    let top_y = if world.dimension.has_ceiling {
        world.level.world_gen.spawn_height()
    } else {
        get_heightmap_height(world, ChunkHeightmapType::MotionBlocking, x, z).await
    };

    if top_y < world.get_bottom_y() {
        return None;
    }

    if !world.dimension.has_ceiling {
        let surface = get_heightmap_height(world, ChunkHeightmapType::WorldSurface, x, z).await;
        if surface <= top_y && surface > ocean_floor_height(world, x, z, surface).await {
            return None;
        }
    }

    for y in (world.get_bottom_y()..=top_y + 1).rev() {
        let pos = BlockPos::new(x, y, z);
        if world.get_fluid(&pos).await != &pumpkin_data::fluid::Fluid::EMPTY {
            break;
        }

        if world
            .get_block_state(&pos)
            .await
            .is_side_solid(BlockDirection::Up)
        {
            return Some(BlockPos::new(x, y + 1, z));
        }
    }

    None
}

async fn ocean_floor_height(world: &World, x: i32, z: i32, surface: i32) -> i32 {
    for y in (world.get_bottom_y()..surface).rev() {
        let pos = BlockPos::new(x, y, z);
        let (block, state) = world.get_block_and_state(&pos).await;
        if blocks_movement(state, block.id) {
            return y + 1;
        }
    }

    world.get_bottom_y()
}

async fn fixup_spawn_height(world: &World, spawn_pos: BlockPos) -> Vector3<f64> {
    let mut y = spawn_pos.0.y;
    while !no_collision_no_liquid(world, BlockPos::new(spawn_pos.0.x, y, spawn_pos.0.z)).await
        && y < world.get_top_y()
    {
        y += 1;
    }

    y -= 1;

    while y > world.get_bottom_y()
        && no_collision_no_liquid(world, BlockPos::new(spawn_pos.0.x, y, spawn_pos.0.z)).await
    {
        y -= 1;
    }

    bottom_center(BlockPos::new(spawn_pos.0.x, y + 1, spawn_pos.0.z))
}

async fn no_collision_no_liquid(world: &World, pos: BlockPos) -> bool {
    let dimensions = Entity::get_entity_dimensions(EntityPose::Standing);
    let box_pos = BoundingBox::new_from_pos(
        f64::from(pos.0.x) + 0.5,
        f64::from(pos.0.y),
        f64::from(pos.0.z) + 0.5,
        &dimensions,
    );

    world.is_space_empty(box_pos).await && !bounding_box_has_fluid(world, box_pos).await
}

async fn bounding_box_has_fluid(world: &World, bounding_box: BoundingBox) -> bool {
    let min = bounding_box.min_block_pos();
    let max = bounding_box.max_block_pos();

    for x in min.0.x..=max.0.x {
        for y in min.0.y..=max.0.y {
            for z in min.0.z..=max.0.z {
                let pos = BlockPos::new(x, y, z);
                let (fluid, state) = world.get_fluid_and_fluid_state(&pos).await;
                if fluid != &pumpkin_data::fluid::Fluid::EMPTY
                    && f64::from(state.height) >= bounding_box.min.y
                {
                    return true;
                }
            }
        }
    }

    false
}

fn bottom_center(pos: BlockPos) -> Vector3<f64> {
    Vector3::new(
        f64::from(pos.0.x) + 0.5,
        f64::from(pos.0.y),
        f64::from(pos.0.z) + 0.5,
    )
}

const fn get_coprime(candidate_count: i32) -> i32 {
    if candidate_count <= 16 {
        candidate_count - 1
    } else {
        17
    }
}

fn candidate_position(
    spawn_suggestion: BlockPos,
    radius: i32,
    offset: i32,
    coprime: i32,
    candidate_index: i32,
) -> (i32, i32) {
    let square_side = radius * 2 + 1;
    let value = (offset + coprime * candidate_index)
        % ((square_side * square_side).min(ABSOLUTE_MAX_ATTEMPTS));
    let delta_x = value % square_side;
    let delta_z = value / square_side;

    (
        spawn_suggestion.0.x + delta_x - radius,
        spawn_suggestion.0.z + delta_z - radius,
    )
}

fn spiral_offsets(width: i32) -> Vec<Vector2<i32>> {
    let mut offsets = Vec::with_capacity((width * width) as usize);
    let mut x = 0;
    let mut z = 0;
    let mut dx = 0;
    let mut dz = -1;

    for _ in 0..width * width {
        offsets.push(Vector2::new(x, z));
        if x == z || (x < 0 && x == -z) || (x > 0 && x == 1 - z) {
            let old_dx = dx;
            dx = -dz;
            dz = old_dx;
        }
        x += dx;
        z += dz;
    }

    offsets
}

async fn get_heightmap_height(world: &World, heightmap: ChunkHeightmapType, x: i32, z: i32) -> i32 {
    let chunk = world.level.get_chunk(Vector2::new(x >> 4, z >> 4)).await;
    chunk
        .heightmap
        .lock()
        .unwrap()
        .get(heightmap, x, z, world.min_y)
}

#[cfg(test)]
mod tests {
    use pumpkin_util::math::{position::BlockPos, vector2::Vector2};

    use super::{candidate_position, get_coprime, spiral_offsets};

    #[test]
    fn spiral_offsets_match_vanilla_order() {
        let expected = vec![
            Vector2::new(0, 0),
            Vector2::new(1, 0),
            Vector2::new(1, 1),
            Vector2::new(0, 1),
            Vector2::new(-1, 1),
            Vector2::new(-1, 0),
            Vector2::new(-1, -1),
            Vector2::new(0, -1),
            Vector2::new(1, -1),
            Vector2::new(2, -1),
            Vector2::new(2, 0),
            Vector2::new(2, 1),
        ];

        assert_eq!(spiral_offsets(11)[..expected.len()], expected);
    }

    #[test]
    fn respawn_candidate_permutation_matches_vanilla_math() {
        let spawn = BlockPos::new(100, 64, -20);
        let radius = 2;
        let candidate_count = ((radius * 2 + 1) * (radius * 2 + 1)).min(1024);
        let coprime = get_coprime(candidate_count);
        let offset = 7;
        let expected = vec![(100, -21), (102, -18), (99, -19), (101, -21), (98, -22)];

        let actual = (0..expected.len() as i32)
            .map(|index| candidate_position(spawn, radius, offset, coprime, index))
            .collect::<Vec<_>>();

        assert_eq!(actual, expected);
    }
}
