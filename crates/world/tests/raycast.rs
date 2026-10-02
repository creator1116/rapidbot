//! The block ray cast against vanilla's `BlockGetter.clip`, ray by ray
//! (`tools/gen_clip_vectors.py`).

use rapidbot_world::raycast::{self, Direction};
use rapidbot_world::{DimensionHeight, World};

fn bits(s: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(s, 16).unwrap())
}

#[test]
fn matches_vanilla_bit_for_bit() {
    let mut lines = include_str!("data/clip_vectors.txt").lines().filter(|l| !l.starts_with('#'));
    let palette: Vec<u32> = lines.next().unwrap().split(' ').map(|v| v.parse().unwrap()).collect();

    let mut world = World::new(DimensionHeight { min_y: -64, height: 384 });
    for cx in -1..=1 {
        for cz in -1..=1 {
            world.insert_chunk(cx, cz, rapidbot_world::chunk::Chunk::empty(24));
        }
    }
    for x in 0..6 {
        for y in 0..6 {
            for z in 0..6 {
                world.set_block_state(x, y, z, palette[((x * 7 + y * 13 + z * 31) as usize) % palette.len()]);
            }
        }
    }

    let (mut rays, mut hits) = (0, 0);
    for line in lines {
        let f: Vec<&str> = line.split(' ').collect();
        let from = [bits(f[0]), bits(f[1]), bits(f[2])];
        let to = [bits(f[3]), bits(f[4]), bits(f[5])];
        let hit = raycast::clip(&world, from, to);
        let context = format!("ray {rays}: {from:?} -> {to:?}: {hit:?}");
        assert_eq!(hit.miss, f[6] == "1", "{context}");
        let pos: Vec<i32> = f[7..10].iter().map(|v| v.parse().unwrap()).collect();
        assert_eq!(hit.pos, (pos[0], pos[1], pos[2]), "{context}");
        assert_eq!(hit.direction as usize, f[10].parse::<usize>().unwrap(), "{context}");
        assert_eq!(hit.inside, f[11] == "1", "{context}");
        let location = [bits(f[12]), bits(f[13]), bits(f[14])];
        assert_eq!(hit.location.map(f64::to_bits), location.map(f64::to_bits), "{context}");
        rays += 1;
        hits += usize::from(!hit.miss);
    }
    assert_eq!(rays, 4000);
    assert!(hits > 500, "only {hits} rays hit anything");
}

#[test]
fn nearest_direction() {
    assert_eq!(Direction::approximate_nearest(0.0, 0.0, 0.0), Direction::North);
    assert_eq!(Direction::approximate_nearest(0.3, -0.9, 0.1), Direction::Down);
    assert_eq!(Direction::approximate_nearest(1.0, 0.2, 0.9), Direction::East);
}
