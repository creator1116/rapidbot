//! Item stack decoding against bytes written by vanilla's own codecs
//! (`tools/extract_items.py`).

use rapidbot_world::item::{EquipmentSlot, ItemData, ItemStack};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

fn vectors() -> Vec<(String, Vec<u8>)> {
    include_str!("data/item_vectors.txt")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| {
            let (snbt, hex) = l.split_once('\t').unwrap();
            (snbt.to_owned(), unhex(hex))
        })
        .collect()
}

fn stack(snbt_start: &str) -> ItemStack {
    let (_, bytes) = vectors().into_iter().find(|(s, _)| s.starts_with(snbt_start)).expect("vector");
    ItemStack::decode(&mut bytes.as_slice()).unwrap().unwrap()
}

/// Every component type has a layout, and every item's defaults decode
/// (loading asserts that nothing is left over).
#[test]
fn every_component_type_is_known() {
    let data = ItemData::get();
    assert_eq!(data.component_types.len(), 122);
    assert_eq!(data.items.len(), 1658);
    assert_eq!(data.undecodable_types(), Vec::<&str>::new());
    for (id, name) in data.component_types.iter().enumerate() {
        let mut bytes = Vec::new();
        rapidbot_buf::write_var_int(&mut bytes, 1);
        rapidbot_buf::write_var_int(&mut bytes, data.item_id("minecraft:stone").unwrap() as i32);
        // One removed component of this type: decodes whatever the type,
        // so this only checks the ID range.
        bytes.extend([0, 1, id as u8]);
        assert!(ItemStack::decode(&mut bytes.as_slice()).is_ok(), "{name}");
    }
}

/// Each vanilla-encoded stack is consumed exactly, alone and back to back.
#[test]
fn vanilla_stacks_decode_to_their_end() {
    let vectors = vectors();
    assert!(vectors.len() >= 40);
    let mut all = Vec::new();
    for (snbt, bytes) in &vectors {
        let mut buf = bytes.as_slice();
        let stack = ItemStack::decode(&mut buf).unwrap_or_else(|e| panic!("{snbt}: {e}"));
        assert!(stack.is_some(), "{snbt}");
        assert!(buf.is_empty(), "{} bytes left after {snbt}", buf.len());
        all.extend_from_slice(bytes);
    }
    let mut buf = all.as_slice();
    for (snbt, _) in &vectors {
        ItemStack::decode(&mut buf).unwrap_or_else(|e| panic!("{snbt}: {e}"));
    }
    assert!(buf.is_empty());
}

#[test]
fn empty_stack() {
    assert_eq!(ItemStack::decode(&mut [0u8].as_slice()).unwrap(), None);
}

#[test]
fn defaults_and_patches() {
    let stone = stack("{id:'minecraft:stone'");
    assert_eq!((stone.name(), stone.count, stone.max_stack_size()), ("minecraft:stone", 64, 64));
    assert!(!stone.is_damageable());

    let sword = stack("{id:'minecraft:diamond_sword'");
    assert_eq!(sword.damage(), 5);
    assert_eq!(sword.max_damage(), 1561);
    assert_eq!(sword.max_stack_size(), 1);
    // Unbreakable items are not damageable.
    assert!(!sword.is_damageable());
    let mut levels: Vec<i32> = sword.enchantments().into_iter().map(|(_, level)| level).collect();
    levels.sort();
    assert_eq!(levels, [3, 5]);

    let stick = stack("{id:'minecraft:stick'");
    assert_eq!(stick.max_stack_size(), 16);
}

/// The `tool` component of a vanilla-encoded pickaxe.
#[test]
fn tool_rules() {
    let pick = stack("{id:'minecraft:iron_pickaxe'");
    let tool = pick.tool().unwrap();
    assert_eq!(tool.rules.len(), 3);
    assert_eq!(tool.default_mining_speed, 1.5);
    assert_eq!(tool.damage_per_block, 2);
    assert!(!tool.can_destroy_blocks_in_creative);

    let registry = rapidbot_world::Registry::get();
    let stone = registry.block_by_name("minecraft:stone").unwrap().id;
    let cobweb = registry.block_by_name("minecraft:cobweb").unwrap().id;
    let sand = registry.block_by_name("minecraft:sand").unwrap().id;
    let mut tags = rapidbot_world::tags::Tags::default();
    tags.insert("minecraft:block", "minecraft:mineable/pickaxe", stone);
    assert_eq!(tool.mining_speed(stone, &tags), 6.0);
    assert!(tool.is_correct_for_drops(stone, &tags));
    assert_eq!(tool.mining_speed(cobweb, &tags), 15.0);
    assert!(!tool.is_correct_for_drops(cobweb, &tags));
    assert_eq!(tool.mining_speed(sand, &tags), 1.5);

    // The default pickaxe: a tag rule from the item's own components.
    let data = ItemData::get();
    let diamond = ItemStack::new(data.item_id("minecraft:diamond_pickaxe").unwrap(), 1);
    assert_eq!(diamond.tool().unwrap().mining_speed(stone, &tags), 8.0);
    assert!(ItemStack::same_item_same_components(Some(&diamond), Some(&diamond.clone())));
    assert!(!ItemStack::same_item_same_components(Some(&diamond), Some(&pick)));
}

/// `LivingEntity.canGlideUsing`.
#[test]
fn gliders() {
    let data = ItemData::get();
    let elytra = ItemStack::new(data.item_id("minecraft:elytra").unwrap(), 1);
    assert!(elytra.can_glide_in(EquipmentSlot::Chest));
    assert!(!elytra.can_glide_in(EquipmentSlot::Head));
    assert_eq!(elytra.max_damage(), 432);

    // One use left: it would break, so it does not glide.
    let worn_out = stack("{id:'minecraft:elytra',count:1,components:{'minecraft:damage':431}}");
    assert!(!worn_out.can_glide_in(EquipmentSlot::Chest));
    // The glider component removed.
    let plain = stack("{id:'minecraft:elytra',count:1,components:{'!minecraft:glider'");
    assert!(!plain.can_glide_in(EquipmentSlot::Chest));
    // A chestplate given the component.
    let leather = stack("{id:'minecraft:leather_chestplate'");
    assert!(leather.can_glide_in(EquipmentSlot::Chest));

    let chestplate = ItemStack::new(data.item_id("minecraft:diamond_chestplate").unwrap(), 1);
    assert_eq!(chestplate.equip_slot(), Some(EquipmentSlot::Chest));
    assert!(!chestplate.can_glide_in(EquipmentSlot::Chest));
}
