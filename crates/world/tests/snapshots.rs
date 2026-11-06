//! The issue #69 acceptance checks for chunk snapshots, through public API
//! only — this file is a downstream crate as far as `rapidbot-world` is
//! concerned, so anything it can reach is part of the published surface.
//!
//! 1. Capture a chunk, update the world, and see the old snapshot unchanged
//!    while the revision advances.
//! 2. Unload and reload, and see the revision move so a cache cannot reuse
//!    the old data.
//! 3. Replace the world, and see the identity change.
//! 4. Keep an `Arc<Chunk>` on another thread while the world moves on.

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use rapidbot_world::chunk::Chunk;
use rapidbot_world::{DimensionHeight, Registry, StateId, World};

fn state(name: &str) -> StateId {
    Registry::get().block_by_name(name).unwrap().default_state
}

fn loaded(height: DimensionHeight) -> World {
    let mut world = World::new(height);
    world.insert_chunk(0, 0, Chunk::empty(24));
    world
}

/// y = 64 is section 8 of an overworld chunk.
const SECTION_OF_Y64: usize = 8;

#[test]
fn a_captured_chunk_does_not_change_while_the_world_does() {
    let stone = state("minecraft:stone");
    let mut world = loaded(DimensionHeight::OVERWORLD);
    let (revision, captured) = world.chunk_snapshot(0, 0).expect("loaded");
    let before = world.observation_id();

    world.set_block_state(1, 64, 1, stone);

    // The captured chunk is still the one that was taken.
    assert_ne!(
        captured.sections[SECTION_OF_Y64].get(1, 0, 1),
        stone,
        "the snapshot kept its contents"
    );

    let (fresh_revision, fresh) = world.chunk_snapshot(0, 0).expect("loaded");
    assert_eq!(fresh.sections[SECTION_OF_Y64].get(1, 0, 1), stone);
    assert!(fresh_revision > revision, "{fresh_revision} > {revision}");
    assert!(world.observation_id() > before, "the world revision moved");
    assert!(
        !Arc::ptr_eq(&captured, &fresh),
        "an update copies the chunk out from under the snapshot"
    );
}

#[test]
fn unloading_and_reloading_invalidates_a_downstream_cache() {
    let mut world = loaded(DimensionHeight::OVERWORLD);
    let (first, captured) = world.chunk_snapshot(0, 0).expect("loaded");
    let loaded_id = world.observation_id();

    world.remove_chunk(0, 0);
    assert!(world.chunk_snapshot(0, 0).is_none(), "unloaded is not air");
    assert!(world.observation_id() > loaded_id);
    assert_eq!(
        captured.sections.len(),
        24,
        "the snapshot is still readable"
    );

    world.insert_chunk(0, 0, Chunk::empty(24));
    let (second, _) = world.chunk_snapshot(0, 0).expect("loaded again");
    assert!(second > first, "a reload is a newer revision");
}

#[test]
fn a_replaced_world_has_its_own_identity() {
    let first = World::new(DimensionHeight::OVERWORLD);
    let second = World::new(DimensionHeight::OVERWORLD);
    assert_ne!(
        first.generation(),
        second.generation(),
        "a cache keyed by the identity cannot mix two levels"
    );
    assert_eq!(first.observation_id(), 0);
}

#[test]
fn a_region_query_returns_shared_chunks() {
    let mut world = loaded(DimensionHeight::OVERWORLD);
    world.insert_chunk(1, 0, Chunk::empty(24));
    world.insert_chunk(5, 5, Chunk::empty(24));

    let (_, live) = world.chunk_snapshot(0, 0).expect("loaded");
    let region = world.chunk_snapshots_in((0, 0), (1, 0));
    assert_eq!(region.len(), 2);
    assert_eq!(region[0].0, (0, 0));
    assert!(Arc::ptr_eq(&region[0].2, &live), "no copy while unchanged");
    assert!(world.chunk_snapshots_in((9, 9), (10, 10)).is_empty());
}

/// The snapshot has to be usable from a worker thread while the bot keeps
/// ticking: `Arc<Chunk>` must move across threads, and reading it there must
/// agree with what was captured.
#[test]
fn a_snapshot_can_be_read_on_another_thread() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Chunk>();
    assert_send_sync::<Arc<Chunk>>();

    let stone = state("minecraft:stone");
    let mut world = loaded(DimensionHeight::OVERWORLD);
    world.set_block_state(2, 64, 3, stone);
    let (_, snapshot) = world.chunk_snapshot(0, 0).expect("loaded");

    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        // Read it while the main thread keeps updating the world.
        tx.send(snapshot.sections[SECTION_OF_Y64].get(2, 0, 3))
            .unwrap();
    });

    world.set_block_state(2, 64, 3, state("minecraft:dirt"));
    assert_eq!(rx.recv().unwrap(), stone, "the worker saw its own chunk");
    worker.join().unwrap();
    assert_eq!(
        world.block_state(2, 64, 3),
        Some(state("minecraft:dirt")),
        "and the world moved on"
    );
}
