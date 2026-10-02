//! Item stacks as the server sends them: `ItemStack.OPTIONAL_STREAM_CODEC`
//! and `DataComponentPatch.STREAM_CODEC`.
//!
//! Component values are not length-prefixed on the wire, so finding where one
//! stack ends needs the layout of every component type. [`Shape`] describes
//! those layouts (one per `DataComponents` entry, from each type's
//! `STREAM_CODEC`); values are kept as raw bytes and only the few the client
//! acts on are parsed. Every item's default components come from the JVM
//! (`tools/extract_items.py`) in the same encoding.

use std::io::Read;
use std::sync::OnceLock;

use rapidbot_buf::{Decode, DecodeError, read_var_int, take};

/// Wire layout of a value.
enum Shape {
    VarInt,
    Int,
    Long,
    Float,
    Double,
    Bool,
    Str,
    /// A network NBT tag (`ByteBufCodecs.fromCodecWithRegistries`, `COMPOUND_TAG`).
    Nbt,
    Uuid,
    Unit,
    Seq(&'static [Shape]),
    /// `ByteBufCodecs.list`/`collection`: count, then elements.
    List(&'static Shape),
    /// `ByteBufCodecs.optional`: a bool, then the value if true.
    Opt(&'static Shape),
    /// `ByteBufCodecs.either`: true then the left value, false then the right.
    Either(&'static Shape, &'static Shape),
    Map(&'static Shape, &'static Shape),
    /// `ByteBufCodecs.fixedSizeList`.
    Fixed(usize, &'static Shape),
    /// `ByteBufCodecs.holder`: registry ID + 1, or 0 and the value inline.
    Holder(&'static Shape),
    /// `ByteBufCodecs.holderSet`: 0 and a tag name, or count + 1 and IDs.
    HolderSet,
    /// A VarInt picking one of the layouts.
    Dispatch(&'static [Shape]),
    /// `ItemStackTemplate.STREAM_CODEC`: item, count, patch.
    Template,
    /// `TypedDataComponent.STREAM_CODEC`: type, then that type's value.
    Typed,
    /// `MobEffectInstance.Details`, which nests itself.
    EffectDetails,
}

use Shape::*;

/// `SoundEvent.STREAM_CODEC`.
static SOUND: Shape = Holder(&Seq(&[Str, Opt(&Float)]));
/// `MobEffectInstance.STREAM_CODEC`.
static EFFECT: Shape = Seq(&[VarInt, EffectDetails]);
/// `ConsumeEffect.STREAM_CODEC`, in `consume_effect_type` registry order.
static CONSUME_EFFECT: Shape = Dispatch(&[
    Seq(&[List(&EFFECT), Float]), // apply_effects
    HolderSet,                    // remove_effects
    Unit,                         // clear_all_effects
    Seq(&[Float, Bool]),          // teleport_randomly
    Holder(&Seq(&[Str, Opt(&Float)])), // play_sound
]);
static ENCHANTMENTS: Shape = Map(&VarInt, &VarInt);
/// `BlockPredicate.STREAM_CODEC`.
static BLOCK_PREDICATE: Shape = Seq(&[
    Opt(&HolderSet),
    Opt(&List(&Seq(&[Str, Either(&Str, &Seq(&[Opt(&Str), Opt(&Str)]))]))),
    Opt(&Nbt),
    List(&Typed),
    // DataComponentPredicate.Single: either kind of type ID, then a tag.
    List(&Seq(&[Bool, VarInt, Nbt])),
]);
static ADVENTURE_PREDICATE: Shape = List(&BLOCK_PREDICATE);
static TRIM_MATERIAL: Shape = Holder(&Seq(&[Str, Nbt]));
static EXPLOSION: Shape = Seq(&[VarInt, List(&Int), List(&Int), Bool, Bool]);
static PROFILE_PROPERTIES: Shape = List(&Seq(&[Str, Str, Opt(&Str)]));
static KINETIC_CONDITION: Shape = Seq(&[VarInt, Float, Float]);
static RESOLVABLE_INT: Shape = Either(&Int, &Str);
static FUEL: Shape = Seq(&[Either(&Int, &Str), Either(&Float, &Str)]);
static SIGN_TEXT: Shape = Seq(&[Fixed(4, &Nbt), Opt(&Fixed(4, &Nbt)), VarInt, Bool]);
static ENTITY_DATA: Shape = Seq(&[VarInt, Nbt]);
static SWING: Shape = Seq(&[VarInt, VarInt]);

/// The layout of a component type, by its name without `minecraft:`.
fn shape_of(name: &str) -> Option<&'static Shape> {
    static USE_EFFECTS: Shape = Seq(&[Bool, Bool, Float]);
    static LORE: Shape = List(&Nbt);
    static ATTRIBUTE_MODIFIERS: Shape = List(&Seq(&[VarInt, Str, Double, VarInt, VarInt, Dispatch(&[Unit, Unit, Nbt])]));
    static CUSTOM_MODEL_DATA: Shape = Seq(&[List(&Float), List(&Bool), List(&Str), List(&Int)]);
    static TOOLTIP_DISPLAY: Shape = Seq(&[Bool, List(&VarInt)]);
    static FOOD: Shape = Seq(&[VarInt, Float, Bool]);
    static CONSUMABLE: Shape = Seq(&[Float, VarInt, Holder(&Seq(&[Str, Opt(&Float)])), Bool, List(&CONSUME_EFFECT)]);
    static USE_COOLDOWN: Shape = Seq(&[Float, Opt(&Str)]);
    static TOOL: Shape = Seq(&[List(&Seq(&[HolderSet, Opt(&Float), Opt(&Bool)])), Float, VarInt, Bool]);
    static WEAPON: Shape = Seq(&[VarInt, Float]);
    static ATTACK_RANGE: Shape = Seq(&[Float, Float, Float, Float, Float, Float]);
    static EQUIPPABLE: Shape = Seq(&[
        VarInt,
        Holder(&Seq(&[Str, Opt(&Float)])),
        Opt(&Str),
        Opt(&Str),
        Opt(&HolderSet),
        Bool,
        Bool,
        Bool,
        Bool,
        Bool,
        Holder(&Seq(&[Str, Opt(&Float)])),
    ]);
    static DEATH_PROTECTION: Shape = List(&CONSUME_EFFECT);
    static BLOCKS_ATTACKS: Shape = Seq(&[
        Float,
        Float,
        List(&Seq(&[Float, Opt(&HolderSet), Float, Float])),
        Seq(&[Float, Float, Float]),
        Opt(&HolderSet),
        Opt(&SOUND),
        Opt(&SOUND),
    ]);
    static PIERCING: Shape = Seq(&[Bool, Bool, Opt(&SOUND), Opt(&SOUND)]);
    static KINETIC: Shape = Seq(&[
        VarInt,
        VarInt,
        Opt(&KINETIC_CONDITION),
        Opt(&KINETIC_CONDITION),
        Opt(&KINETIC_CONDITION),
        Float,
        Float,
        Opt(&SOUND),
        Opt(&SOUND),
    ]);
    static TEMPLATES: Shape = List(&Template);
    static POTION_CONTENTS: Shape = Seq(&[Opt(&VarInt), Opt(&Int), List(&EFFECT), Opt(&Str)]);
    static STEW: Shape = List(&Seq(&[VarInt, VarInt]));
    static WRITABLE_BOOK: Shape = List(&Seq(&[Str, Opt(&Str)]));
    static WRITTEN_BOOK: Shape = Seq(&[Seq(&[Str, Opt(&Str)]), Str, VarInt, List(&Seq(&[Nbt, Opt(&Nbt)])), Bool]);
    static TRIM: Shape = Seq(&[Holder(&Seq(&[Str, Nbt])), Holder(&Seq(&[Str, Nbt, Bool]))]);
    static INSTRUMENT: Shape = Holder(&Seq(&[Holder(&Seq(&[Str, Opt(&Float)])), Float, Float, VarInt, Nbt]));
    static JUKEBOX: Shape = Holder(&Seq(&[Holder(&Seq(&[Str, Opt(&Float)])), Nbt, Float, VarInt]));
    static LODESTONE: Shape = Seq(&[Opt(&Seq(&[Str, Long])), Bool]);
    static FIREWORKS: Shape = Seq(&[VarInt, List(&EXPLOSION)]);
    static PROFILE: Shape = Seq(&[
        Either(&Seq(&[Uuid, Str, List(&Seq(&[Str, Str, Opt(&Str)]))]), &Seq(&[Opt(&Str), Opt(&Uuid), List(&Seq(&[Str, Str, Opt(&Str)]))])),
        Seq(&[Opt(&Str), Opt(&Str), Opt(&Str), Opt(&Bool)]),
    ]);
    static BANNER_PATTERNS: Shape = List(&Seq(&[Holder(&Seq(&[Str, Str])), VarInt]));
    static POT_DECORATIONS: Shape = Seq(&[Opt(&Template), Opt(&Template), Opt(&Template), Opt(&Template)]);
    static CONTAINER: Shape = List(&Opt(&Template));
    static BLOCK_STATE: Shape = Map(&Str, &Str);
    static BEES: Shape = List(&Seq(&[Seq(&[VarInt, Nbt]), VarInt, VarInt]));
    static MOB_VISIBILITY: Shape = Seq(&[HolderSet, Float]);
    static PAINTING: Shape = Holder(&Seq(&[VarInt, VarInt, Str, Opt(&Nbt), Opt(&Nbt)]));
    let _ = &PROFILE_PROPERTIES;

    Some(match name {
        // No network codec: the persistent codec, as a tag.
        "custom_data" | "intangible_projectile" | "map_decorations" | "debug_stick_state" | "recipes" | "lock"
        | "container_loot" => &Nbt,
        "max_stack_size" | "max_damage" | "damage" | "repair_cost" | "enchantable" | "additional_trade_cost"
        | "villager_food" | "map_id" | "map_post_processing" | "ominous_bottle_amplifier" | "rarity" | "dye"
        | "base_color" => &VarInt,
        // Registry holders and enums.
        "damage_type" | "block_transformer" | "provides_pottery_pattern" | "villager/variant" | "wolf/variant"
        | "wolf/sound_variant" | "wolf/collar" | "fox/variant" | "salmon/size" | "parrot/variant"
        | "tropical_fish/pattern" | "tropical_fish/base_color" | "tropical_fish/pattern_color" | "mooshroom/variant"
        | "rabbit/variant" | "pig/variant" | "pig/sound_variant" | "cow/variant" | "cow/sound_variant"
        | "chicken/variant" | "chicken/sound_variant" | "zombie_nautilus/variant" | "frog/variant" | "horse/variant"
        | "llama/variant" | "axolotl/variant" | "cat/variant" | "cat/sound_variant" | "cat/collar" | "sheep/color"
        | "shulker/color" | "cushion/color" => &VarInt,
        "unbreakable" | "creative_slot_lock" | "glider" | "waxed" => &Unit,
        "use_effects" => &USE_EFFECTS,
        "custom_name" | "item_name" | "bucket_entity_data" => &Nbt,
        "minimum_attack_charge" | "potion_duration_scale" => &Float,
        "item_model" | "tooltip_style" | "note_block_sound" => &Str,
        "lore" => &LORE,
        "enchantments" | "stored_enchantments" => &ENCHANTMENTS,
        "can_place_on" | "can_break" => &ADVENTURE_PREDICATE,
        "attribute_modifiers" => &ATTRIBUTE_MODIFIERS,
        "custom_model_data" => &CUSTOM_MODEL_DATA,
        "tooltip_display" => &TOOLTIP_DISPLAY,
        "enchantment_glint_override" => &Bool,
        "food" => &FOOD,
        "consumable" => &CONSUMABLE,
        "use_remainder" | "sulfur_cube_content" => &Template,
        "use_cooldown" => &USE_COOLDOWN,
        "damage_resistant" | "repairable" | "provides_banner_patterns" => &HolderSet,
        "tool" => &TOOL,
        "weapon" => &WEAPON,
        "attack_range" => &ATTACK_RANGE,
        "equippable" => &EQUIPPABLE,
        "death_protection" => &DEATH_PROTECTION,
        "blocks_attacks" => &BLOCKS_ATTACKS,
        "piercing_weapon" => &PIERCING,
        "kinetic_weapon" => &KINETIC,
        "attack_animation" | "interact_animation" => &SWING,
        "dyed_color" => &Int,
        "charged_projectiles" | "bundle_contents" => &TEMPLATES,
        "potion_contents" => &POTION_CONTENTS,
        "suspicious_stew_effects" => &STEW,
        "writable_book_content" => &WRITABLE_BOOK,
        "written_book_content" => &WRITTEN_BOOK,
        "trim" => &TRIM,
        "entity_data" | "block_entity_data" => &ENTITY_DATA,
        "instrument" => &INSTRUMENT,
        "provides_trim_material" => &TRIM_MATERIAL,
        "jukebox_playable" => &JUKEBOX,
        "lodestone_tracker" => &LODESTONE,
        "firework_explosion" => &EXPLOSION,
        "fireworks" => &FIREWORKS,
        "profile" => &PROFILE,
        "banner_patterns" => &BANNER_PATTERNS,
        "pot_decorations" => &POT_DECORATIONS,
        "container" => &CONTAINER,
        "block_state" => &BLOCK_STATE,
        "bees" => &BEES,
        "break_sound" => &SOUND,
        "compostable" => &RESOLVABLE_INT,
        "cooking_fuel" | "brewing_fuel" => &FUEL,
        "mob_visibility" => &MOB_VISIBILITY,
        "painting/variant" => &PAINTING,
        "sign_text_front" | "sign_text_back" => &SIGN_TEXT,
        _ => return None,
    })
}

fn count(buf: &mut &[u8]) -> Result<usize, DecodeError> {
    let n = read_var_int(buf)?;
    usize::try_from(n).map_err(|_| DecodeError::NegativeLength(n))
}

fn skip(shape: &Shape, buf: &mut &[u8], data: &ItemData) -> Result<(), DecodeError> {
    match shape {
        VarInt => {
            read_var_int(buf)?;
        }
        Int | Float => {
            take(buf, 4)?;
        }
        Long | Double => {
            take(buf, 8)?;
        }
        Bool => {
            take(buf, 1)?;
        }
        Str => {
            let len = count(buf)?;
            take(buf, len)?;
        }
        Nbt => {
            rapidbot_nbt::read_network(buf)?;
        }
        Uuid => {
            take(buf, 16)?;
        }
        Unit => {}
        Seq(parts) => {
            for part in *parts {
                skip(part, buf, data)?;
            }
        }
        List(element) => {
            for _ in 0..count(buf)? {
                skip(element, buf, data)?;
            }
        }
        Opt(value) => {
            if bool::decode(buf)? {
                skip(value, buf, data)?;
            }
        }
        Either(left, right) => {
            let side = if bool::decode(buf)? { left } else { right };
            skip(side, buf, data)?;
        }
        Map(key, value) => {
            for _ in 0..count(buf)? {
                skip(key, buf, data)?;
                skip(value, buf, data)?;
            }
        }
        Fixed(n, element) => {
            for _ in 0..*n {
                skip(element, buf, data)?;
            }
        }
        Holder(direct) => {
            if read_var_int(buf)? == 0 {
                skip(direct, buf, data)?;
            }
        }
        HolderSet => {
            let n = read_var_int(buf)? - 1;
            if n == -1 {
                skip(&Str, buf, data)?;
            } else {
                for _ in 0..n {
                    read_var_int(buf)?;
                }
            }
        }
        Dispatch(options) => {
            let index = read_var_int(buf)?;
            let option = usize::try_from(index)
                .ok()
                .and_then(|i| options.get(i))
                .ok_or(DecodeError::InvalidDiscriminant { ty: "component variant", value: index })?;
            skip(option, buf, data)?;
        }
        Template => {
            read_var_int(buf)?;
            read_var_int(buf)?;
            Patch::decode(buf, data)?;
        }
        Typed => {
            data.typed(buf)?;
        }
        EffectDetails => {
            read_var_int(buf)?;
            read_var_int(buf)?;
            take(buf, 3)?;
            if bool::decode(buf)? {
                skip(&EffectDetails, buf, data)?;
            }
        }
    }
    Ok(())
}

/// `DataComponentPatch`: components set on a stack, and default ones removed.
#[derive(Debug, Clone, Default, PartialEq)]
struct Patch {
    /// Component type ID and its encoded value.
    added: Vec<(u32, Vec<u8>)>,
    removed: Vec<u32>,
}

impl Patch {
    fn decode(buf: &mut &[u8], data: &ItemData) -> Result<Patch, DecodeError> {
        let added = count(buf)?;
        let removed = count(buf)?;
        let mut patch = Patch::default();
        for _ in 0..added {
            patch.added.push(data.typed(buf)?);
        }
        for _ in 0..removed {
            let ty = read_var_int(buf)?;
            patch.removed.push(ty as u32);
        }
        Ok(patch)
    }
}

/// `EquipmentSlot`, with the IDs its stream codec uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EquipmentSlot {
    MainHand = 0,
    Feet = 1,
    Legs = 2,
    Chest = 3,
    Head = 4,
    OffHand = 5,
    Body = 6,
    Saddle = 7,
}

impl EquipmentSlot {
    fn from_id(id: i32) -> Option<Self> {
        Some(match id {
            0 => Self::MainHand,
            1 => Self::Feet,
            2 => Self::Legs,
            3 => Self::Chest,
            4 => Self::Head,
            5 => Self::OffHand,
            6 => Self::Body,
            7 => Self::Saddle,
            _ => return None,
        })
    }
}

/// A non-empty `ItemStack`.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemStack {
    /// `minecraft:item` registry ID.
    pub item: u32,
    pub count: i32,
    patch: Patch,
}

impl ItemStack {
    /// A stack of an item with its default components.
    pub fn new(item: u32, count: i32) -> Self {
        Self { item, count, patch: Patch::default() }
    }

    /// `ItemStack.OPTIONAL_STREAM_CODEC`: `None` is the empty stack.
    pub fn decode(buf: &mut &[u8]) -> Result<Option<ItemStack>, DecodeError> {
        let count = read_var_int(buf)?;
        if count <= 0 {
            return Ok(None);
        }
        let item = read_var_int(buf)? as u32;
        let patch = Patch::decode(buf, ItemData::get())?;
        Ok(Some(ItemStack { item, count, patch }))
    }

    /// The item's name, such as `minecraft:elytra`.
    pub fn name(&self) -> &'static str {
        ItemData::get().items.get(self.item as usize).map_or("minecraft:air", |i| i.name.as_str())
    }

    /// The encoded value of a component (`minecraft:damage`), from the
    /// stack's own components or else the item's defaults.
    pub fn component(&self, name: &str) -> Option<&[u8]> {
        let data = ItemData::get();
        let ty = data.component_id(name)?;
        if let Some((_, value)) = self.patch.added.iter().find(|(t, _)| *t == ty) {
            return Some(value);
        }
        if self.patch.removed.contains(&ty) {
            return None;
        }
        let defaults = &data.items.get(self.item as usize)?.components;
        defaults.iter().find(|(t, _)| *t == ty).map(|(_, value)| value.as_slice())
    }

    pub fn has(&self, name: &str) -> bool {
        self.component(name).is_some()
    }

    fn var_int(&self, name: &str) -> Option<i32> {
        read_var_int(&mut self.component(name)?).ok()
    }

    /// `getMaxStackSize`.
    pub fn max_stack_size(&self) -> i32 {
        self.var_int("minecraft:max_stack_size").unwrap_or(1)
    }

    /// `getMaxDamage`.
    pub fn max_damage(&self) -> i32 {
        self.var_int("minecraft:max_damage").unwrap_or(0)
    }

    /// `getDamageValue`.
    pub fn damage(&self) -> i32 {
        self.var_int("minecraft:damage").unwrap_or(0).clamp(0, self.max_damage().max(0))
    }

    /// `isDamageableItem`.
    pub fn is_damageable(&self) -> bool {
        self.has("minecraft:max_damage") && !self.has("minecraft:unbreakable") && self.has("minecraft:damage")
    }

    /// `nextDamageWillBreak`.
    pub fn next_damage_will_break(&self) -> bool {
        self.is_damageable() && self.damage() >= self.max_damage() - 1
    }

    /// The slot of the `equippable` component.
    pub fn equip_slot(&self) -> Option<EquipmentSlot> {
        EquipmentSlot::from_id(self.var_int("minecraft:equippable")?)
    }

    /// `LivingEntity.canGlideUsing`: an elytra (or anything with `glider`)
    /// worn in its slot that is not about to break.
    pub fn can_glide_in(&self, slot: EquipmentSlot) -> bool {
        self.has("minecraft:glider") && self.equip_slot() == Some(slot) && !self.next_damage_will_break()
    }

    /// The `tool` component: how fast this digs what.
    pub fn tool(&self) -> Option<Tool> {
        Tool::decode(&mut self.component("minecraft:tool")?).ok()
    }

    /// `ItemStack.isSameItemSameComponents`.
    pub fn same_item_same_components(a: Option<&ItemStack>, b: Option<&ItemStack>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                if a.item != b.item || a.patch.removed.len() != b.patch.removed.len() || a.patch.added.len() != b.patch.added.len() {
                    return false;
                }
                a.patch.removed.iter().all(|t| b.patch.removed.contains(t)) && a.patch.added.iter().all(|c| b.patch.added.contains(c))
            }
            _ => false,
        }
    }

    /// Enchantment registry IDs (as the server numbers them) and levels.
    pub fn enchantments(&self) -> Vec<(i32, i32)> {
        let Some(mut value) = self.component("minecraft:enchantments") else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let Ok(n) = count(&mut value) else {
            return out;
        };
        for _ in 0..n {
            match (read_var_int(&mut value), read_var_int(&mut value)) {
                (Ok(id), Ok(level)) => out.push((id, level)),
                _ => break,
            }
        }
        out
    }
}

/// A `HolderSet<Block>`: a tag, or block registry IDs.
#[derive(Debug, Clone, PartialEq)]
pub enum BlockSet {
    Tag(String),
    Ids(Vec<u32>),
}

impl BlockSet {
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        let n = read_var_int(buf)? - 1;
        if n == -1 {
            return Ok(BlockSet::Tag(String::decode(buf)?));
        }
        (0..n).map(|_| read_var_int(buf).map(|id| id as u32)).collect::<Result<_, _>>().map(BlockSet::Ids)
    }

    pub fn contains(&self, block: u32, tags: &crate::tags::Tags) -> bool {
        match self {
            BlockSet::Tag(tag) => tags.block_is(tag, block),
            BlockSet::Ids(ids) => ids.contains(&block),
        }
    }
}

/// `Tool.Rule`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolRule {
    pub blocks: BlockSet,
    pub speed: Option<f32>,
    pub correct_for_drops: Option<bool>,
}

/// The `tool` component.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub rules: Vec<ToolRule>,
    pub default_mining_speed: f32,
    pub damage_per_block: i32,
    pub can_destroy_blocks_in_creative: bool,
}

impl Tool {
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        let mut rules = Vec::new();
        for _ in 0..count(buf)? {
            let blocks = BlockSet::decode(buf)?;
            let speed = if bool::decode(buf)? { Some(f32::decode(buf)?) } else { None };
            let correct_for_drops = if bool::decode(buf)? { Some(bool::decode(buf)?) } else { None };
            rules.push(ToolRule { blocks, speed, correct_for_drops });
        }
        Ok(Tool {
            rules,
            default_mining_speed: f32::decode(buf)?,
            damage_per_block: read_var_int(buf)?,
            can_destroy_blocks_in_creative: bool::decode(buf)?,
        })
    }

    /// `getMiningSpeed`: the first rule with a speed that covers the block.
    pub fn mining_speed(&self, block: u32, tags: &crate::tags::Tags) -> f32 {
        self.rules
            .iter()
            .find_map(|r| r.speed.filter(|_| r.blocks.contains(block, tags)))
            .unwrap_or(self.default_mining_speed)
    }

    /// `isCorrectForDrops`.
    pub fn is_correct_for_drops(&self, block: u32, tags: &crate::tags::Tags) -> bool {
        self.rules.iter().find_map(|r| r.correct_for_drops.filter(|_| r.blocks.contains(block, tags))).unwrap_or(false)
    }
}

/// An item and its default components.
pub struct ItemInfo {
    pub name: String,
    components: Vec<(u32, Vec<u8>)>,
}

/// Items and data component types of the supported version.
pub struct ItemData {
    /// `minecraft:data_component_type` names by registry ID.
    pub component_types: Vec<String>,
    shapes: Vec<Option<&'static Shape>>,
    /// `minecraft:item` entries by registry ID.
    pub items: Vec<ItemInfo>,
}

#[derive(serde::Deserialize)]
struct RawItems {
    component_types: Vec<String>,
    items: Vec<RawItem>,
}

#[derive(serde::Deserialize)]
struct RawItem {
    name: String,
    components: String,
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex")).collect()
}

impl ItemData {
    pub fn get() -> &'static ItemData {
        static DATA: OnceLock<ItemData> = OnceLock::new();
        DATA.get_or_init(ItemData::load)
    }

    fn load() -> ItemData {
        let gz: &[u8] = include_bytes!("../data/items.json.gz");
        let mut json = String::new();
        flate2::read::GzDecoder::new(gz).read_to_string(&mut json).expect("embedded item data");
        let raw: RawItems = serde_json::from_str(&json).expect("embedded item data");
        let shapes = raw.component_types.iter().map(|n| shape_of(n.strip_prefix("minecraft:").unwrap_or(n))).collect();
        let mut data = ItemData { component_types: raw.component_types, shapes, items: Vec::new() };
        let items = raw
            .items
            .into_iter()
            .map(|item| {
                let bytes = unhex(&item.components);
                let mut buf = bytes.as_slice();
                let mut components = Vec::new();
                for _ in 0..count(&mut buf).expect("embedded item data") {
                    components.push(data.typed(&mut buf).expect("embedded item data"));
                }
                assert!(buf.is_empty(), "trailing bytes in the components of {}", item.name);
                ItemInfo { name: item.name, components }
            })
            .collect();
        data.items = items;
        data
    }

    /// Component types whose wire layout is not known: a stack carrying
    /// one cannot be decoded. Empty for the supported version.
    pub fn undecodable_types(&self) -> Vec<&str> {
        self.component_types.iter().zip(&self.shapes).filter(|(_, s)| s.is_none()).map(|(n, _)| n.as_str()).collect()
    }

    pub fn component_id(&self, name: &str) -> Option<u32> {
        self.component_types.iter().position(|n| n == name).map(|i| i as u32)
    }

    pub fn item_id(&self, name: &str) -> Option<u32> {
        self.items.iter().position(|i| i.name == name).map(|i| i as u32)
    }

    /// Reads a component type and its value, returning the value's bytes.
    fn typed(&self, buf: &mut &[u8]) -> Result<(u32, Vec<u8>), DecodeError> {
        let ty = read_var_int(buf)?;
        let shape = usize::try_from(ty)
            .ok()
            .and_then(|i| self.shapes.get(i).copied().flatten())
            .ok_or(DecodeError::InvalidDiscriminant { ty: "data component type", value: ty })?;
        let start = *buf;
        skip(shape, buf, self)?;
        Ok((ty as u32, start[..start.len() - buf.len()].to_vec()))
    }
}
