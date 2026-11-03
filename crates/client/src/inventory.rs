//! The player's inventory as the server describes it: `Inventory`,
//! `InventoryMenu` and whichever container menu is open.

use rapidbot_buf::{Decode, DecodeError, VarInt};
use rapidbot_protocol::packets::play::serverbound::{
    ClickContainer, ClickContainerButton, ContainerInput, HashedStack,
};
use rapidbot_world::item::{EquipmentSlot, ItemStack};

/// `InventoryMenu` slot numbers (container 0).
pub mod slot {
    pub const CRAFT_RESULT: usize = 0;
    pub const HEAD: usize = 5;
    pub const CHEST: usize = 6;
    pub const LEGS: usize = 7;
    pub const FEET: usize = 8;
    /// First of the 27 main inventory slots.
    pub const MAIN: usize = 9;
    /// First of the nine hotbar slots.
    pub const HOTBAR: usize = 36;
    pub const OFFHAND: usize = 45;
    pub const COUNT: usize = 46;
}

/// The menus the vanilla client knows by `MenuType` registry ID. Most have
/// their own slots first and the player's 36 storage slots afterwards; the
/// crafter is the exception, with its result slot after the player slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuKind {
    Inventory,
    Generic,
    Anvil,
    Beacon,
    BlastFurnace,
    BrewingStand,
    Crafting,
    Enchantment,
    Furnace,
    Grindstone,
    Hopper,
    Lectern,
    Loom,
    Merchant,
    Smithing,
    Smoker,
    CartographyTable,
    Stonecutter,
    Crafter,
}

impl MenuKind {
    fn from_type(menu_type: i32) -> Self {
        match menu_type {
            7 => Self::Crafter,
            8 => Self::Anvil,
            9 => Self::Beacon,
            10 => Self::BlastFurnace,
            11 => Self::BrewingStand,
            12 => Self::Crafting,
            13 => Self::Enchantment,
            14 => Self::Furnace,
            15 => Self::Grindstone,
            16 => Self::Hopper,
            17 => Self::Lectern,
            18 => Self::Loom,
            19 => Self::Merchant,
            21 => Self::Smithing,
            22 => Self::Smoker,
            23 => Self::CartographyTable,
            24 => Self::Stonecutter,
            _ => Self::Generic,
        }
    }

    /// The first menu slot holding the player's 36 storage slots.
    fn player_start(self, own: usize) -> usize {
        if self == Self::Crafter { 9 } else { own }
    }

    /// The container's own slot indices whose `Slot.mayPlace` rejects items.
    /// For the crafter that is own slot 9 (menu slot 45); everywhere else
    /// own and menu indices coincide.
    fn result_slots(self) -> &'static [usize] {
        match self {
            Self::Anvil => &[2],
            Self::BlastFurnace | Self::Furnace | Self::Smoker => &[2],
            Self::Crafting => &[0],
            Self::Grindstone => &[2],
            Self::Loom => &[3],
            Self::Merchant => &[2],
            Self::Smithing => &[3],
            Self::CartographyTable => &[2],
            Self::Stonecutter => &[1],
            Self::Crafter => &[9],
            _ => &[],
        }
    }
}

/// A menu slot: the open container's own storage, or the player's inventory
/// menu slots (which use the numbers in [`slot`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotRef {
    Container(usize),
    Player(usize),
}

/// A container menu other than the player's own (a chest, a furnace).
#[derive(Debug, Clone, Default)]
pub struct Container {
    pub id: i32,
    /// `MenuType` registry ID from `open_screen`.
    pub menu_type: i32,
    /// The container's own slots, in the menu's own-slot order. The player's
    /// 36 storage slots live in [`Inventory`].
    pub slots: Vec<Option<ItemStack>>,
    /// `ContainerData`: menu data slots (`set_data`).
    pub data: Vec<i16>,
    pub state_id: i32,
}

#[derive(Debug, Clone, Default)]
struct QuickCraft {
    /// `quickcraftStatus`: 0 idle, 1 dragging, 2 finishing.
    status: u8,
    /// `quickcraftType`: 0 even, 1 one each, 2 creative clone.
    kind: u8,
    /// `quickcraftSlots`: the menu slots the drag has visited.
    slots: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct Inventory {
    slots: Vec<Option<ItemStack>>,
    /// The stack on the cursor (`AbstractContainerMenu.carried`).
    pub cursor: Option<ItemStack>,
    /// `InventoryMenu.stateId`, echoed in container clicks.
    pub state_id: i32,
    /// `Inventory.selected`: the hotbar slot in hand, 0 to 8.
    pub selected: u8,
    /// `containerMenu` when it is not the inventory menu.
    pub container: Option<Container>,
    /// `Player.hasInfiniteMaterials()` (creative abilities `instabuild`).
    pub creative: bool,
    quick_craft: QuickCraft,
}

impl Default for Inventory {
    fn default() -> Self {
        Self {
            slots: vec![None; slot::COUNT],
            cursor: None,
            state_id: 0,
            selected: 0,
            container: None,
            creative: false,
            quick_craft: QuickCraft::default(),
        }
    }
}

/// `Inventory` index (as in `set_player_inventory`) to menu slot.
fn menu_slot(inventory_index: i32) -> Option<usize> {
    Some(match inventory_index {
        0..=8 => slot::HOTBAR + inventory_index as usize,
        9..=35 => inventory_index as usize,
        36 => slot::FEET,
        37 => slot::LEGS,
        38 => slot::CHEST,
        39 => slot::HEAD,
        40 => slot::OFFHAND,
        _ => return None,
    })
}

/// The menu's own slot count when `MenuType` determines it. Server-sent
/// content overwrites this, but knowing it lets a `set_slot` arriving first
/// land in the right place.
fn initial_menu_slots(menu_type: i32) -> usize {
    match menu_type {
        0..=5 => (menu_type as usize + 1) * 9,
        6 => 9,
        7 => 10,
        8 => 3,
        9 => 1,
        10 => 3,
        11 => 5,
        12 => 10,
        13 => 2,
        14 => 3,
        15 => 3,
        16 => 5,
        17 => 1,
        18 => 4,
        19 => 3,
        20 => 27,
        21 => 4,
        22 => 3,
        23 => 3,
        24 => 2,
        _ => 0,
    }
}

fn is_beacon_payment(stack: &ItemStack) -> bool {
    matches!(
        stack.name(),
        "minecraft:netherite_ingot"
            | "minecraft:emerald"
            | "minecraft:diamond"
            | "minecraft:gold_ingot"
            | "minecraft:iron_ingot"
    )
}

/// `AbstractFurnaceMenu.isFuel`: an item that cooks.
fn is_furnace_fuel(stack: &ItemStack) -> bool {
    stack.has("minecraft:cooking_fuel")
}

/// `FurnaceFuelSlot.mayPlace`: fuel, or an empty bucket.
fn fuel_slot_may_place(stack: &ItemStack) -> bool {
    is_furnace_fuel(stack) || stack.name() == "minecraft:bucket"
}

/// `ItemTags.BREWING_POTION_INPUTS`: the three potion items and the empty
/// bottle. Tipped arrows carry `potion_contents` by default too, so the
/// component cannot stand in for the tag.
fn is_potion_input(stack: &ItemStack) -> bool {
    matches!(
        stack.name(),
        "minecraft:potion"
            | "minecraft:splash_potion"
            | "minecraft:lingering_potion"
            | "minecraft:glass_bottle"
    )
}

fn is_brewing_fuel(stack: &ItemStack) -> bool {
    stack.has("minecraft:brewing_fuel") || stack.name() == "minecraft:blaze_powder"
}

fn is_banner(stack: &ItemStack) -> bool {
    stack.name().ends_with("_banner")
}

fn is_dye(stack: &ItemStack) -> bool {
    stack.name().ends_with("_dye")
}

fn is_banner_pattern(stack: &ItemStack) -> bool {
    stack.name().ends_with("_banner_pattern") || stack.has("minecraft:provides_banner_patterns")
}

fn is_cartography_addition(stack: &ItemStack) -> bool {
    // `CartographyTableMenu` slot 1: plain glass panes only, no dyed ones.
    matches!(
        stack.name(),
        "minecraft:paper" | "minecraft:map" | "minecraft:glass_pane"
    )
}

/// `GrindstoneMenu` input slots: a damageable item or one with
/// enchantments, stored ones included (enchanted books).
fn is_grindstone_input(stack: &ItemStack) -> bool {
    stack.is_damageable() || stack.has_any_enchantments()
}

/// `ScrollWheelHandler.getNextScrollWheelSelection`: one wheel event moves
/// the selection by one slot, wrapping.
fn next_scroll_selection(wheel: f64, current: i32, limit: i32) -> i32 {
    let step = wheel.signum() as i32;
    let mut selected = (current - step).max(-1);
    while selected < 0 {
        selected += limit;
    }
    while selected >= limit {
        selected -= limit;
    }
    selected
}

/// Applies `notches` wheel events to a hotbar slot. Positive means wheel up,
/// which vanilla maps to the previous slot (towards 0).
pub(crate) fn scroll_selection(current: u8, notches: i32) -> u8 {
    let direction = notches.signum() as f64;
    (0..notches.unsigned_abs()).fold(current as i32, |selected, _| {
        next_scroll_selection(direction, selected, 9)
    }) as u8
}

impl Inventory {
    /// A menu slot of the inventory screen; see [`slot`].
    pub fn get(&self, index: usize) -> Option<&ItemStack> {
        self.slots.get(index)?.as_ref()
    }

    /// `getMainHandItem`.
    pub fn held(&self) -> Option<&ItemStack> {
        self.get(slot::HOTBAR + self.selected as usize)
    }

    pub fn offhand(&self) -> Option<&ItemStack> {
        self.get(slot::OFFHAND)
    }

    /// Hotbar slot 0 to 8.
    pub fn hotbar(&self, index: u8) -> Option<&ItemStack> {
        self.get(slot::HOTBAR + index as usize)
    }

    /// `getItemBySlot`.
    pub fn equipped(&self, slot: EquipmentSlot) -> Option<&ItemStack> {
        match slot {
            EquipmentSlot::MainHand => self.held(),
            EquipmentSlot::OffHand => self.offhand(),
            EquipmentSlot::Feet => self.get(slot::FEET),
            EquipmentSlot::Legs => self.get(slot::LEGS),
            EquipmentSlot::Chest => self.get(slot::CHEST),
            EquipmentSlot::Head => self.get(slot::HEAD),
            EquipmentSlot::Body | EquipmentSlot::Saddle => None,
        }
    }

    /// The equipment half of `LivingEntity.canGlide`: something that glides
    /// is worn in its slot.
    pub fn can_glide(&self) -> bool {
        use EquipmentSlot::*;
        [MainHand, OffHand, Feet, Legs, Chest, Head]
            .into_iter()
            .any(|s| self.equipped(s).is_some_and(|i| i.can_glide_in(s)))
    }

    /// Leather boots in the feet slot, which allow walking on powder snow.
    pub fn has_leather_boots(&self) -> bool {
        self.equipped(EquipmentSlot::Feet)
            .is_some_and(|item| item.name() == "minecraft:leather_boots")
    }

    /// The hotbar slot holding an item (`minecraft:diamond_pickaxe`).
    pub fn hotbar_slot_of(&self, name: &str) -> Option<u8> {
        (0..9).find(|i| self.hotbar(*i).is_some_and(|s| s.name() == name))
    }

    /// Items in the inventory screen's slots, with their menu slot.
    pub fn items(&self) -> impl Iterator<Item = (usize, &ItemStack)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.as_ref().map(|s| (i, s)))
    }

    /// How many of an item the 36 storage slots and the offhand hold.
    pub fn count(&self, name: &str) -> i32 {
        self.items()
            .filter(|(i, s)| *i >= slot::MAIN && s.name() == name)
            .map(|(_, s)| s.count)
            .sum()
    }

    fn menu_kind(&self) -> MenuKind {
        self.container
            .as_ref()
            .map_or(MenuKind::Inventory, |c| MenuKind::from_type(c.menu_type))
    }

    /// Where a menu slot lives. In every current menu except the crafter the
    /// container's slots precede the player's storage; the crafter's result
    /// slot (menu slot 45) follows the player's slots.
    fn slot_ref(&self, index: usize) -> Option<SlotRef> {
        let Some(container) = &self.container else {
            return (index < slot::COUNT).then_some(SlotRef::Player(index));
        };
        let kind = MenuKind::from_type(container.menu_type);
        let own = container.slots.len();
        if kind == MenuKind::Crafter {
            return match index {
                0..=8 if index < own => Some(SlotRef::Container(index)),
                9..=44 => Some(SlotRef::Player(slot::MAIN + index - 9)),
                45 if own > 9 => Some(SlotRef::Container(9)),
                _ => None,
            };
        }
        if index < own {
            return Some(SlotRef::Container(index));
        }
        let offset = index.checked_sub(own)?;
        (offset < 36).then_some(SlotRef::Player(slot::MAIN + offset))
    }

    /// `handleContainerContent`.
    pub(crate) fn set_content(&mut self, mut body: &[u8]) -> Result<(), DecodeError> {
        let id = VarInt::decode(&mut body)?.0;
        let state_id = VarInt::decode(&mut body)?.0;
        let count = VarInt::decode(&mut body)?.0.max(0) as usize;
        let mut items = Vec::with_capacity(count.min(256));
        for _ in 0..count {
            items.push(ItemStack::decode(&mut body)?);
        }
        let cursor = ItemStack::decode(&mut body)?;
        if id == 0 {
            // initializeContents
            for (i, item) in items.into_iter().enumerate().take(slot::COUNT) {
                self.slots[i] = item;
            }
            self.cursor = cursor;
            self.state_id = state_id;
            self.quick_craft = QuickCraft::default();
        } else if self.container.as_ref().is_some_and(|c| c.id == id) {
            let own = items.len().saturating_sub(36);
            if let Some(container) = self.container.as_mut() {
                container.slots = vec![None; own];
                container.state_id = state_id;
            }
            for (index, item) in items.into_iter().enumerate() {
                self.set_menu_item(index, item);
            }
            self.cursor = cursor;
        }
        Ok(())
    }

    /// `handleContainerSetSlot`. Returns the container and slot.
    pub(crate) fn set_slot(&mut self, mut body: &[u8]) -> Result<(i32, i16), DecodeError> {
        let id = VarInt::decode(&mut body)?.0;
        let state_id = VarInt::decode(&mut body)?.0;
        let index = i16::decode(&mut body)?;
        let item = ItemStack::decode(&mut body)?;
        let at = usize::try_from(index).ok();
        if id == 0 {
            if let Some(slot) = at.and_then(|i| self.slots.get_mut(i)) {
                *slot = item;
            }
            self.state_id = state_id;
        } else if self.container.as_ref().is_some_and(|c| c.id == id) {
            if let Some(container) = self.container.as_mut() {
                container.state_id = state_id;
            }
            if let Some(index) = at {
                self.set_menu_item(index, item);
            }
        }
        Ok((id, index))
    }

    /// `handleSetPlayerInventory`.
    pub(crate) fn set_player_inventory(&mut self, mut body: &[u8]) -> Result<(), DecodeError> {
        let index = VarInt::decode(&mut body)?.0;
        let item = ItemStack::decode(&mut body)?;
        if let Some(slot) = menu_slot(index) {
            self.slots[slot] = item;
        }
        Ok(())
    }

    /// `handleSetCursorItem`.
    pub(crate) fn set_cursor(&mut self, mut body: &[u8]) -> Result<(), DecodeError> {
        self.cursor = ItemStack::decode(&mut body)?;
        Ok(())
    }

    /// `CrafterMenu.isSlotDisabled`: data slot `own` holds 1 while the grid
    /// slot is switched off.
    fn crafter_slot_disabled(&self, own: usize) -> bool {
        self.container
            .as_ref()
            .is_some_and(|c| c.data.get(own).copied().unwrap_or(0) == 1)
    }

    /// `handleContainerSetData`: `AbstractContainerMenu.setData`.
    pub(crate) fn set_data(&mut self, mut body: &[u8]) -> Result<(), DecodeError> {
        let container_id = VarInt::decode(&mut body)?.0;
        let id = i16::decode(&mut body)?;
        let value = i16::decode(&mut body)?;
        if let Some(container) = self.container.as_mut().filter(|c| c.id == container_id) {
            let Ok(index) = usize::try_from(id) else {
                return Ok(());
            };
            if index >= container.data.len() {
                container.data.resize(index + 1, 0);
            }
            container.data[index] = value;
        }
        Ok(())
    }

    /// `handleSetHeldSlot`.
    pub(crate) fn set_held_slot(&mut self, mut body: &[u8]) -> Result<(), DecodeError> {
        let index = VarInt::decode(&mut body)?.0;
        if (0..9).contains(&index) {
            self.selected = index as u8;
        }
        Ok(())
    }

    /// `handleOpenScreen`: container ID and menu type (the title follows).
    pub(crate) fn open_screen(&mut self, mut body: &[u8]) -> Result<(), DecodeError> {
        let id = VarInt::decode(&mut body)?.0;
        let menu_type = VarInt::decode(&mut body)?.0;
        self.container = Some(Container {
            id,
            menu_type,
            slots: vec![None; initial_menu_slots(menu_type)],
            ..Default::default()
        });
        self.reset_quick_craft();
        Ok(())
    }

    /// `closeContainer`: back to the inventory menu, cursor emptied.
    pub(crate) fn close_container(&mut self) {
        self.container = None;
        self.cursor = None;
    }

    /// The slot `-999` a click can name: the carried stack outside the menu.
    pub const CLICK_OUTSIDE: i16 = -999;

    /// Number of slots in the menu the player currently has open.
    pub fn menu_len(&self) -> usize {
        match &self.container {
            Some(container) if MenuKind::from_type(container.menu_type) == MenuKind::Crafter => 46,
            Some(container) => container.slots.len() + 36,
            None => slot::COUNT,
        }
    }

    /// A menu slot as the server numbers it. With a container open its own
    /// slots come first, then the player's 27 main slots and 9 hotbar slots.
    pub fn menu_item(&self, index: usize) -> Option<&ItemStack> {
        match self.slot_ref(index)? {
            SlotRef::Container(slot) => self.container.as_ref()?.slots.get(slot)?.as_ref(),
            SlotRef::Player(slot) => self.slots.get(slot)?.as_ref(),
        }
    }

    fn set_menu_item(&mut self, index: usize, item: Option<ItemStack>) -> bool {
        match self.slot_ref(index) {
            Some(SlotRef::Container(slot)) => {
                let Some(container) = self.container.as_mut() else {
                    return false;
                };
                let Some(target) = container.slots.get_mut(slot) else {
                    return false;
                };
                *target = item;
                true
            }
            Some(SlotRef::Player(slot)) => {
                let Some(target) = self.slots.get_mut(slot) else {
                    return false;
                };
                *target = item;
                true
            }
            None => false,
        }
    }

    /// `Slot.mayPickup`: only the crafter's result is a
    /// `NonInteractiveResultSlot`.
    fn slot_may_pickup(&self, index: usize) -> bool {
        !(self.menu_kind() == MenuKind::Crafter
            && matches!(self.slot_ref(index), Some(SlotRef::Container(9))))
    }

    /// The menu's slot filters (`Slot.mayPlace`).
    fn slot_accepts(&self, index: usize, stack: &ItemStack) -> bool {
        match self.slot_ref(index) {
            Some(SlotRef::Player(slot)) if self.container.is_none() => match slot {
                slot::CRAFT_RESULT => false,
                1..=4 => true,
                slot::HEAD..=slot::FEET => {
                    let expected = match slot {
                        slot::HEAD => EquipmentSlot::Head,
                        slot::CHEST => EquipmentSlot::Chest,
                        slot::LEGS => EquipmentSlot::Legs,
                        _ => EquipmentSlot::Feet,
                    };
                    stack.equip_slot() == Some(expected)
                }
                _ => true,
            },
            Some(SlotRef::Player(_)) => true,
            Some(SlotRef::Container(own)) => {
                let Some(container) = &self.container else {
                    return false;
                };
                let kind = MenuKind::from_type(container.menu_type);
                if kind.result_slots().contains(&own) {
                    return false;
                }
                match kind {
                    // `CrafterSlot.mayPlace`: a disabled slot takes nothing.
                    MenuKind::Crafter if own < 9 => !self.crafter_slot_disabled(own),
                    MenuKind::Beacon if own == 0 => is_beacon_payment(stack),
                    MenuKind::BlastFurnace | MenuKind::Furnace | MenuKind::Smoker if own == 1 => {
                        fuel_slot_may_place(stack)
                    }
                    MenuKind::BrewingStand => match own {
                        0..=2 => is_potion_input(stack),
                        4 => is_brewing_fuel(stack),
                        _ => true,
                    },
                    MenuKind::Enchantment if own == 1 => stack.name() == "minecraft:lapis_lazuli",
                    MenuKind::Loom => match own {
                        0 => is_banner(stack),
                        1 => is_dye(stack),
                        2 => is_banner_pattern(stack),
                        _ => true,
                    },
                    MenuKind::CartographyTable => match own {
                        0 => stack.has("minecraft:map_id"),
                        1 => is_cartography_addition(stack),
                        _ => true,
                    },
                    MenuKind::Grindstone if own < 2 => is_grindstone_input(stack),
                    _ => true,
                }
            }
            None => false,
        }
    }

    fn slot_max_stack(&self, index: usize, stack: &ItemStack) -> i32 {
        match self.slot_ref(index) {
            Some(SlotRef::Player(slot)) if self.container.is_none() => {
                if (slot::HEAD..=slot::FEET).contains(&slot) {
                    return 1;
                }
            }
            Some(SlotRef::Container(own)) => {
                let kind = self.menu_kind();
                if (kind == MenuKind::Beacon && own == 0)
                    || (kind == MenuKind::Enchantment && own == 0)
                    || (kind == MenuKind::BrewingStand && own <= 2)
                    // `FurnaceFuelSlot.getMaxStackSize`: buckets alone.
                    || (matches!(
                        kind,
                        MenuKind::BlastFurnace | MenuKind::Furnace | MenuKind::Smoker
                    ) && own == 1
                        && stack.name() == "minecraft:bucket")
                {
                    return 1;
                }
            }
            _ => {}
        }
        stack.max_stack_size().max(1)
    }

    fn insert_at(&mut self, index: usize, incoming: &mut ItemStack, amount: i32) -> bool {
        if amount <= 0 || !self.slot_accepts(index, incoming) {
            return false;
        }
        let existing = self.menu_item(index).cloned();
        let max = self.slot_max_stack(index, incoming);
        match existing {
            None => {
                let n = amount.min(max);
                let mut moved = incoming.clone();
                moved.count = n;
                self.set_menu_item(index, Some(moved));
                incoming.count -= n;
                n > 0
            }
            Some(mut existing)
                if ItemStack::same_item_same_components(Some(&existing), Some(incoming)) =>
            {
                let n = amount.min((max - existing.count).max(0));
                if n == 0 {
                    return false;
                }
                existing.count += n;
                self.set_menu_item(index, Some(existing));
                incoming.count -= n;
                true
            }
            Some(_) => false,
        }
    }

    fn take_at(&mut self, index: usize, amount: i32) -> Option<ItemStack> {
        let mut stack = self.menu_item(index)?.clone();
        let n = amount.clamp(1, stack.count);
        let mut taken = stack.clone();
        taken.count = n;
        stack.count -= n;
        self.set_menu_item(index, (stack.count > 0).then_some(stack));
        Some(taken)
    }

    fn pickup(&mut self, slot: i16, secondary: bool) {
        if slot == Self::CLICK_OUTSIDE {
            if let Some(mut carried) = self.cursor.take()
                && secondary
            {
                carried.count -= 1;
                if carried.count > 0 {
                    self.cursor = Some(carried);
                }
            }
            return;
        }
        let Ok(index) = usize::try_from(slot) else {
            return;
        };
        if index >= self.menu_len() {
            return;
        }
        // `doClick`: an unpickable slot with an item ignores the click;
        // an empty one still rejects the carried stack via `mayPlace`.
        if self.menu_item(index).is_some() && !self.slot_may_pickup(index) {
            return;
        }
        match (self.menu_item(index).cloned(), self.cursor.take()) {
            (None, Some(mut carried)) => {
                let amount = if secondary { 1 } else { carried.count };
                if self.insert_at(index, &mut carried, amount) {
                    self.cursor = (carried.count > 0).then_some(carried);
                } else {
                    self.cursor = Some(carried);
                }
            }
            (Some(clicked), None) => {
                let amount = if secondary {
                    (clicked.count + 1) / 2
                } else {
                    clicked.count
                };
                self.cursor = self.take_at(index, amount);
            }
            (Some(clicked), Some(mut carried)) => {
                if ItemStack::same_item_same_components(Some(&clicked), Some(&carried)) {
                    let amount = if secondary { 1 } else { carried.count };
                    if self.insert_at(index, &mut carried, amount) {
                        self.cursor = (carried.count > 0).then_some(carried);
                    } else if !self.slot_accepts(index, &carried) {
                        // An output slot cannot take items, but the same item
                        // can still be pulled from it onto the cursor.
                        let room = (carried.max_stack_size().max(1) - carried.count).max(0);
                        let amount = clicked.count.min(room);
                        if amount > 0
                            && let Some(removed) = self.take_at(index, amount)
                        {
                            carried.count += removed.count;
                        }
                        self.cursor = (carried.count > 0).then_some(carried);
                    } else {
                        self.cursor = Some(carried);
                    }
                } else if self.slot_accepts(index, &carried)
                    && carried.count <= self.slot_max_stack(index, &carried)
                {
                    self.set_menu_item(index, Some(carried));
                    self.cursor = Some(clicked);
                } else {
                    self.cursor = Some(carried);
                }
            }
            (None, None) => {}
        }
    }

    /// Move as much of `stack` as possible into menu slots, in vanilla's
    /// direction (`reverse` fills the hotbar before the main inventory).
    fn move_range(
        &mut self,
        stack: &mut ItemStack,
        range: std::ops::Range<usize>,
        reverse: bool,
    ) -> bool {
        let before = stack.count;
        let mut targets: Vec<usize> = range.collect();
        if reverse {
            targets.reverse();
        }
        for target in targets {
            if stack.count == 0 {
                break;
            }
            self.insert_at(target, stack, stack.count);
        }
        stack.count < before
    }

    fn move_player_storage(&mut self, stack: &mut ItemStack, index: usize, start: usize) -> bool {
        let main = start..start + 27;
        let hotbar = start + 27..start + 36;
        if main.contains(&index) {
            self.move_range(stack, hotbar, false)
        } else if hotbar.contains(&index) {
            self.move_range(stack, main, false)
        } else {
            self.move_range(stack, start..start + 36, false)
        }
    }

    fn reset_quick_craft(&mut self) {
        self.quick_craft.status = 0;
        self.quick_craft.slots.clear();
    }

    fn can_quick_replace(&self, index: usize, carried: &ItemStack) -> bool {
        match self.menu_item(index) {
            None => true,
            Some(existing) => ItemStack::same_item_same_components(Some(existing), Some(carried)),
        }
    }

    /// `ContainerInput.QUICK_CRAFT`: the start, slot-by-slot drag and finish
    /// sequence. The finish uses one slot as an ordinary pickup; otherwise it
    /// spreads the carried stack according to the header's type.
    fn quick_craft(&mut self, slot: i16, button: i8) {
        let expected = self.quick_craft.status;
        let status = (button as u8) & 3;
        self.quick_craft.status = status;
        if ((expected != 1 || status != 2) && expected != status) || self.cursor.is_none() {
            self.reset_quick_craft();
            return;
        }
        match status {
            0 => {
                let kind = ((button as u8) >> 2) & 3;
                if kind <= 2 && (kind != 2 || self.creative) {
                    self.quick_craft.kind = kind;
                    self.quick_craft.status = 1;
                    self.quick_craft.slots.clear();
                } else {
                    self.reset_quick_craft();
                }
            }
            1 => {
                let Ok(index) = usize::try_from(slot) else {
                    return;
                };
                if index >= self.menu_len() {
                    return;
                }
                let Some(carried) = self.cursor.as_ref() else {
                    return;
                };
                let enough = self.quick_craft.kind == 2
                    || carried.count > self.quick_craft.slots.len() as i32;
                if enough
                    && self.can_quick_replace(index, carried)
                    && self.slot_accepts(index, carried)
                    && !self.quick_craft.slots.contains(&index)
                {
                    self.quick_craft.slots.push(index);
                }
            }
            2 => {
                let slots = self.quick_craft.slots.clone();
                if slots.is_empty() {
                    self.reset_quick_craft();
                    return;
                }
                if slots.len() == 1 {
                    let index = slots[0];
                    let kind = self.quick_craft.kind;
                    self.reset_quick_craft();
                    if kind <= 1 {
                        self.pickup(index as i16, kind == 1);
                    }
                    return;
                }
                let Some(source) = self.cursor.clone() else {
                    self.reset_quick_craft();
                    return;
                };
                let mut remaining = source.count;
                let count = slots.len() as i32;
                let max = source.max_stack_size().max(1);
                for index in slots {
                    let Some(current) = self.cursor.as_ref() else {
                        break;
                    };
                    if !self.can_quick_replace(index, current)
                        || !self.slot_accepts(index, current)
                        || (self.quick_craft.kind != 2 && source.count < count)
                    {
                        continue;
                    }
                    let carry = self.menu_item(index).map_or(0, |stack| stack.count);
                    let slot_max = self.slot_max_stack(index, &source).min(max);
                    let place = match self.quick_craft.kind {
                        0 => source.count / count,
                        1 => 1,
                        2 => max,
                        _ => source.count,
                    };
                    let new_count = (place + carry).min(slot_max);
                    remaining -= new_count - carry;
                    if new_count > 0 {
                        let mut placed = source.clone();
                        placed.count = new_count;
                        self.set_menu_item(index, Some(placed));
                    } else {
                        self.set_menu_item(index, None);
                    }
                }
                if remaining > 0 {
                    let mut carried = source;
                    carried.count = remaining;
                    self.cursor = Some(carried);
                } else {
                    self.cursor = None;
                }
                self.reset_quick_craft();
            }
            _ => self.reset_quick_craft(),
        }
    }

    /// `ContainerInput.PICKUP_ALL`: two passes, first not-full stacks and
    /// then full ones, in the button's direction.
    fn pickup_all(&mut self, slot: i16, backwards: bool) {
        let Ok(index) = usize::try_from(slot) else {
            return;
        };
        if self.cursor.is_none() || index >= self.menu_len() {
            return;
        }
        // `doClick`: collection starts from an empty or unpickable slot.
        if self.menu_item(index).is_some() && self.slot_may_pickup(index) {
            return;
        }
        let max = self
            .cursor
            .as_ref()
            .map_or(0, |s| s.max_stack_size().max(1));
        if max <= 0 {
            return;
        }
        let len = self.menu_len();
        for pass in 0..2 {
            let indices: Vec<usize> = if backwards {
                (0..len).rev().collect()
            } else {
                (0..len).collect()
            };
            for i in indices {
                let Some(carried) = self.cursor.as_ref() else {
                    break;
                };
                if carried.count >= max {
                    break;
                }
                let Some(target) = self.menu_item(i) else {
                    continue;
                };
                if !self.slot_may_pickup(i)
                    || !ItemStack::same_item_same_components(Some(target), Some(carried))
                    || (pass == 0 && target.count == target.max_stack_size())
                {
                    continue;
                }
                let amount = target.count.min(max - carried.count);
                if amount <= 0 {
                    continue;
                }
                let Some(removed) = self.take_at(i, amount) else {
                    continue;
                };
                if let Some(cursor) = self.cursor.as_mut() {
                    cursor.count += removed.count;
                }
            }
        }
    }

    /// `ContainerInput.CLONE`: `Slot.safeClone`, a full stack of the clicked
    /// item, only with creative abilities.
    fn clone_slot(&mut self, slot: i16) {
        if !self.creative || self.cursor.is_some() {
            return;
        }
        let Ok(index) = usize::try_from(slot) else {
            return;
        };
        if index >= self.menu_len() || !self.slot_may_pickup(index) {
            return;
        }
        if let Some(stack) = self.menu_item(index).cloned() {
            let mut cloned = stack;
            cloned.count = cloned.max_stack_size().max(1);
            self.cursor = Some(cloned);
        }
    }

    /// `quickMoveStack` for the menus this client opens. The menu's own slot
    /// filters decide where things may go; ambiguous recipe inputs (for
    /// example a furnace's smeltable slot) are left to the server's response.
    fn quick_move(&mut self, slot: i16) {
        let Ok(index) = usize::try_from(slot) else {
            return;
        };
        if index >= self.menu_len() || !self.slot_may_pickup(index) {
            return;
        }
        let Some(mut stack) = self.menu_item(index).cloned() else {
            return;
        };
        let Some(container) = &self.container else {
            self.quick_move_inventory(index, &mut stack);
            self.set_menu_item(index, (stack.count > 0).then_some(stack));
            return;
        };

        let kind = MenuKind::from_type(container.menu_type);
        let own = container.slots.len();
        let player_start = kind.player_start(own);
        let in_player = (player_start..player_start + 36).contains(&index);
        if !in_player {
            // A container slot: output slots go to storage; inputs also go
            // to storage. A quick move from the crafter's result does
            // nothing, matching its non-interactive result slot.
            let result = kind.result_slots().contains(&index);
            let to_player = match kind {
                MenuKind::Lectern => false,
                MenuKind::Crafter => index < 9,
                _ => true,
            };
            if to_player {
                let reverse = result
                    || matches!(
                        kind,
                        MenuKind::Crafter
                            | MenuKind::Generic
                            | MenuKind::Hopper
                            | MenuKind::Beacon
                            | MenuKind::BrewingStand
                            | MenuKind::Enchantment
                    );
                self.move_range(&mut stack, player_start..player_start + 36, reverse);
            }
            self.set_menu_item(index, (stack.count > 0).then_some(stack));
            return;
        }

        // `Some(moved)`: the menu's own quick-move rule claimed the click.
        // When the rule applies but the move fails vanilla returns EMPTY, it
        // does not fall back to the player storage; `None` means the item
        // belongs in the player storage instead.
        let container_move: Option<bool> = match kind {
            // `LecternMenu.quickMoveStack` returns EMPTY for every slot.
            MenuKind::Lectern => Some(false),
            MenuKind::Generic | MenuKind::Hopper => {
                Some(self.move_range(&mut stack, 0..own, false))
            }
            MenuKind::Crafter => Some(self.move_range(&mut stack, 0..9, false)),
            // On failure the crafting menu moves between main and hotbar.
            MenuKind::Crafting => {
                if self.move_range(&mut stack, 1..own, false) {
                    Some(true)
                } else {
                    None
                }
            }
            MenuKind::BlastFurnace | MenuKind::Furnace | MenuKind::Smoker => {
                // The cookable check needs synced recipes; treat non-fuel
                // items as cookable (vanilla checks cookable before fuel).
                if is_furnace_fuel(&stack) {
                    Some(self.move_range(&mut stack, 1..2, false))
                } else {
                    Some(self.move_range(&mut stack, 0..1, false))
                }
            }
            MenuKind::Beacon => {
                // `paymentSlot.mayPlace` is part of the claim: a non-payment
                // item takes the main-to-hotbar fallback instead.
                if stack.count == 1 && self.menu_item(0).is_none() && self.slot_accepts(0, &stack) {
                    Some(self.move_range(&mut stack, 0..1, false))
                } else {
                    None
                }
            }
            MenuKind::BrewingStand => {
                // `BREWING_REAGENTS` is a recipe property set this client
                // does not sync: non-fuel, non-potion items are assumed to
                // be reagents, which vanilla routes to the ingredient slot
                // and returns EMPTY for on a failed move.
                if is_brewing_fuel(&stack) {
                    if self.move_range(&mut stack, 4..5, false) {
                        Some(true)
                    } else {
                        // A fuel that overflows the fuel slot may still be a
                        // reagent (blaze powder).
                        Some(self.move_range(&mut stack, 3..4, false))
                    }
                } else if is_potion_input(&stack) {
                    Some(self.move_range(&mut stack, 0..3, false))
                } else {
                    Some(self.move_range(&mut stack, 3..4, false))
                }
            }
            MenuKind::Enchantment => {
                if stack.name() == "minecraft:lapis_lazuli" {
                    Some(self.move_range(&mut stack, 1..2, false))
                } else if self.menu_item(0).is_none() {
                    // `quickMoveStack` moves exactly one item into the
                    // enchanting slot and leaves the rest in the source.
                    let mut one = stack.clone();
                    one.count = 1;
                    if self.insert_at(0, &mut one, 1) {
                        stack.count -= 1;
                        Some(true)
                    } else {
                        Some(false)
                    }
                } else {
                    Some(false)
                }
            }
            // `canMoveIntoInputSlots` is true for the anvil.
            MenuKind::Anvil => Some(self.move_range(&mut stack, 0..2, false)),
            MenuKind::Smithing => {
                // The input filters come from synced recipe property sets
                // this client does not track; keep the storage fallback.
                if self.move_range(&mut stack, 0..3, false) {
                    Some(true)
                } else {
                    None
                }
            }
            MenuKind::Grindstone => {
                if self.menu_item(0).is_some() && self.menu_item(1).is_some() {
                    None
                } else {
                    Some(self.move_range(&mut stack, 0..2, false))
                }
            }
            MenuKind::Loom => {
                if is_banner(&stack) {
                    Some(self.move_range(&mut stack, 0..1, false))
                } else if is_dye(&stack) {
                    Some(self.move_range(&mut stack, 1..2, false))
                } else if is_banner_pattern(&stack) {
                    Some(self.move_range(&mut stack, 2..3, false))
                } else {
                    None
                }
            }
            MenuKind::CartographyTable => {
                if stack.has("minecraft:map_id") {
                    Some(self.move_range(&mut stack, 0..1, false))
                } else if is_cartography_addition(&stack) {
                    Some(self.move_range(&mut stack, 1..2, false))
                } else {
                    None
                }
            }
            // The stonecutter input check needs synced recipes.
            MenuKind::Stonecutter => Some(self.move_range(&mut stack, 0..1, false)),
            MenuKind::Merchant | MenuKind::Inventory => None,
        };

        if container_move.is_none() {
            self.move_player_storage(&mut stack, index, player_start);
        }
        self.set_menu_item(index, (stack.count > 0).then_some(stack));
    }

    fn quick_move_inventory(&mut self, index: usize, stack: &mut ItemStack) {
        match index {
            slot::CRAFT_RESULT => {
                self.move_range(stack, slot::MAIN..slot::HOTBAR + 9, true);
            }
            1..=4 | slot::HEAD..=slot::FEET => {
                self.move_range(stack, slot::MAIN..slot::HOTBAR + 9, false);
            }
            _ => {
                let armor_slot = match stack.equip_slot() {
                    Some(EquipmentSlot::Head) => Some(slot::HEAD),
                    Some(EquipmentSlot::Chest) => Some(slot::CHEST),
                    Some(EquipmentSlot::Legs) => Some(slot::LEGS),
                    Some(EquipmentSlot::Feet) => Some(slot::FEET),
                    _ => None,
                };
                let equipped = armor_slot
                    .filter(|target| self.menu_item(*target).is_none())
                    .is_some_and(|target| self.move_range(stack, target..target + 1, false));
                let offhand = !equipped
                    && stack.equip_slot() == Some(EquipmentSlot::OffHand)
                    && self.menu_item(slot::OFFHAND).is_none()
                    && self.move_range(stack, slot::OFFHAND..slot::OFFHAND + 1, false);
                if !equipped && !offhand {
                    self.move_player_storage(stack, index, slot::MAIN);
                }
            }
        }
    }

    fn swap(&mut self, slot: i16, button: i8) {
        if !(0..=8).contains(&button) && button != 40 {
            return;
        }
        let Ok(target) = usize::try_from(slot) else {
            return;
        };
        if target >= self.menu_len() {
            return;
        }
        let Some(source) = menu_slot(button as i32) else {
            return;
        };
        let source_stack = self.slots[source].clone();
        let target_stack = self.menu_item(target).cloned();
        match (source_stack, target_stack) {
            (None, None) => {}
            (None, Some(target_stack)) => {
                if !self.slot_may_pickup(target) {
                    return;
                }
                self.slots[source] = Some(target_stack);
                self.set_menu_item(target, None);
            }
            (Some(mut source_stack), None) => {
                if !self.slot_accepts(target, &source_stack) {
                    return;
                }
                let max = self.slot_max_stack(target, &source_stack);
                if source_stack.count > max {
                    let mut moved = source_stack.clone();
                    moved.count = max;
                    source_stack.count -= max;
                    self.set_menu_item(target, Some(moved));
                    self.slots[source] = Some(source_stack);
                } else {
                    self.set_menu_item(target, Some(source_stack));
                    self.slots[source] = None;
                }
            }
            (Some(source_stack), Some(target_stack)) => {
                let max = self.slot_max_stack(target, &source_stack);
                if self.slot_accepts(target, &source_stack)
                    && self.slot_may_pickup(target)
                    && source_stack.count <= max
                    && target_stack.count <= 64
                {
                    self.set_menu_item(target, Some(source_stack));
                    self.slots[source] = Some(target_stack);
                }
            }
        }
    }

    fn throw(&mut self, slot: i16, all: bool) {
        if self.cursor.is_some() {
            return;
        }
        let Ok(index) = usize::try_from(slot) else {
            return;
        };
        if index >= self.menu_len() || !self.slot_may_pickup(index) {
            return;
        }
        let count = if all {
            self.menu_item(index).map_or(0, |s| s.count)
        } else {
            1
        };
        let _ = self.take_at(index, count);
    }

    fn hashed(stack: Option<&ItemStack>) -> HashedStack {
        match stack {
            Some(stack) if stack.patch_is_empty() => HashedStack::Actual {
                item: stack.item,
                count: stack.count,
                added_components: Vec::new(),
                removed_components: Vec::new(),
            },
            _ => HashedStack::Empty,
        }
    }

    /// `MultiPlayerGameMode.handleContainerInput`: apply a menu click locally
    /// and return the packet that describes the prediction. `None` for input
    /// this client does not implement or a button vanilla never sends.
    pub fn click(
        &mut self,
        slot: i16,
        button: i8,
        input: ContainerInput,
    ) -> Option<ClickContainer> {
        if input != ContainerInput::QuickCraft && self.quick_craft.status != 0 {
            self.reset_quick_craft();
        }
        let before: Vec<Option<ItemStack>> = (0..self.menu_len())
            .map(|i| self.menu_item(i).cloned())
            .collect();
        let handled = match input {
            ContainerInput::Pickup if button == 0 || button == 1 => {
                self.pickup(slot, button == 1);
                true
            }
            ContainerInput::QuickMove if button == 0 || button == 1 => {
                self.quick_move(slot);
                true
            }
            ContainerInput::Swap => {
                self.swap(slot, button);
                true
            }
            ContainerInput::Throw if button == 0 || button == 1 => {
                self.throw(slot, button == 1);
                true
            }
            ContainerInput::QuickCraft => {
                self.quick_craft(slot, button);
                true
            }
            ContainerInput::PickupAll if (button == 0 || button == 1) && slot >= 0 => {
                self.pickup_all(slot, button == 1);
                true
            }
            ContainerInput::Clone => {
                self.clone_slot(slot);
                true
            }
            _ => false,
        };
        if !handled {
            return None;
        }
        let changed_slots = (0..self.menu_len())
            .filter_map(|i| {
                let after = self.menu_item(i);
                (before.get(i).and_then(Option::as_ref) != after)
                    .then(|| (i as i16, Self::hashed(after)))
            })
            .collect();
        let (container_id, state_id) = self
            .container
            .as_ref()
            .map_or((0, self.state_id), |c| (c.id, c.state_id));
        Some(ClickContainer {
            container_id,
            state_id,
            slot,
            button,
            input,
            changed_slots,
            carried_item: Self::hashed(self.cursor.as_ref()),
        })
    }

    /// `MultiPlayerGameMode.handleInventoryButtonClick`.
    pub fn click_button(&self, button_id: i32) -> Option<ClickContainerButton> {
        let container = self.container.as_ref()?;
        Some(ClickContainerButton {
            container_id: container.id,
            button_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapidbot_buf::write_var_int;
    use rapidbot_world::item::ItemData;

    fn stack_bytes(name: &str, count: i32) -> Vec<u8> {
        let mut out = Vec::new();
        write_var_int(&mut out, count);
        write_var_int(&mut out, ItemData::get().item_id(name).unwrap() as i32);
        out.extend([0, 0]);
        out
    }

    fn set_slot(container: i32, state: i32, slot: i16, item: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_var_int(&mut out, container);
        write_var_int(&mut out, state);
        out.extend(slot.to_be_bytes());
        out.extend(item);
        out
    }

    #[test]
    fn elytra_in_the_chest_slot_glides() {
        let mut inv = Inventory::default();
        assert!(!inv.can_glide());
        // In the hotbar it does nothing.
        inv.set_slot(&set_slot(0, 1, 36, &stack_bytes("minecraft:elytra", 1)))
            .unwrap();
        assert!(!inv.can_glide());
        assert_eq!(inv.held().unwrap().name(), "minecraft:elytra");
        inv.set_slot(&set_slot(
            0,
            2,
            slot::CHEST as i16,
            &stack_bytes("minecraft:elytra", 1),
        ))
        .unwrap();
        assert!(inv.can_glide());
        assert_eq!(inv.state_id, 2);
        // Taken off again.
        inv.set_slot(&set_slot(0, 3, slot::CHEST as i16, &[0]))
            .unwrap();
        assert!(!inv.can_glide());
    }

    #[test]
    fn leather_boots_in_the_feet_slot_allow_powder_snow() {
        let mut inv = Inventory::default();
        assert!(!inv.has_leather_boots());
        inv.set_slot(&set_slot(
            0,
            1,
            slot::FEET as i16,
            &stack_bytes("minecraft:leather_boots", 1),
        ))
        .unwrap();
        assert!(inv.has_leather_boots());
        // Other armour slots do not count.
        inv.set_slot(&set_slot(0, 2, slot::FEET as i16, &[0]))
            .unwrap();
        inv.set_slot(&set_slot(
            0,
            3,
            slot::HEAD as i16,
            &stack_bytes("minecraft:leather_boots", 1),
        ))
        .unwrap();
        assert!(!inv.has_leather_boots());
    }

    #[test]
    fn content_and_inventory_indices() {
        let mut inv = Inventory::default();
        let mut body = Vec::new();
        write_var_int(&mut body, 0);
        write_var_int(&mut body, 7);
        write_var_int(&mut body, 46);
        for i in 0..46 {
            if i == 38 {
                body.extend(stack_bytes("minecraft:stone", 12));
            } else {
                body.push(0);
            }
        }
        body.push(0);
        inv.set_content(&body).unwrap();
        assert_eq!(inv.state_id, 7);
        assert_eq!(inv.hotbar(2).unwrap().count, 12);
        assert_eq!(inv.hotbar_slot_of("minecraft:stone"), Some(2));
        assert_eq!(inv.count("minecraft:stone"), 12);

        // Inventory index 38 is the chest armour slot, 40 the offhand.
        let mut body = Vec::new();
        write_var_int(&mut body, 38);
        body.extend(stack_bytes("minecraft:elytra", 1));
        inv.set_player_inventory(&body).unwrap();
        assert!(inv.can_glide());
        let mut body = Vec::new();
        write_var_int(&mut body, 40);
        body.extend(stack_bytes("minecraft:shield", 1));
        inv.set_player_inventory(&body).unwrap();
        assert_eq!(inv.offhand().unwrap().name(), "minecraft:shield");

        inv.set_held_slot(&[2]).unwrap();
        assert_eq!(inv.held().unwrap().name(), "minecraft:stone");
        inv.set_held_slot(&[9]).unwrap();
        assert_eq!(inv.selected, 2);
    }

    #[test]
    fn scroll_wheel_moves_one_slot_per_notch_and_wraps() {
        assert_eq!(scroll_selection(0, 1), 8);
        assert_eq!(scroll_selection(8, -1), 0);
        assert_eq!(scroll_selection(4, 2), 2);
        assert_eq!(scroll_selection(0, -2), 2);
        assert_eq!(scroll_selection(5, 10), 4);
        assert_eq!(scroll_selection(5, 0), 5);
    }

    fn item(name: &str, count: i32) -> ItemStack {
        ItemStack::new(ItemData::get().item_id(name).unwrap(), count)
    }

    fn chest() -> Inventory {
        let mut inv = Inventory::default();
        inv.open_screen(&[3, 2, 0]).unwrap();
        let container = inv.container.as_mut().unwrap();
        container.slots = vec![None; 27];
        container.state_id = 4;
        inv
    }

    #[test]
    fn container_pickup_moves_the_cursor_both_ways() {
        let mut inv = chest();
        inv.cursor = Some(item("minecraft:stone", 5));
        let packet = inv.click(0, 0, ContainerInput::Pickup).unwrap();
        assert_eq!(packet.container_id, 3);
        assert_eq!(packet.state_id, 4);
        assert_eq!(packet.changed_slots.len(), 1);
        assert_eq!(packet.changed_slots[0].0, 0);
        assert_eq!(packet.carried_item, HashedStack::Empty);
        assert_eq!(
            inv.container.as_ref().unwrap().slots[0]
                .as_ref()
                .unwrap()
                .count,
            5
        );
        assert!(inv.cursor.is_none());

        let packet = inv.click(0, 1, ContainerInput::Pickup).unwrap();
        assert_eq!(packet.changed_slots[0].0, 0);
        assert_eq!(inv.cursor.as_ref().unwrap().count, 3);
        assert_eq!(
            inv.container.as_ref().unwrap().slots[0]
                .as_ref()
                .unwrap()
                .count,
            2
        );
        assert!(matches!(packet.carried_item, HashedStack::Actual { .. }));
    }

    #[test]
    fn container_quick_move_and_throw() {
        let mut inv = chest();
        inv.container.as_mut().unwrap().slots[0] = Some(item("minecraft:diamond", 5));
        inv.click(0, 0, ContainerInput::QuickMove).unwrap();
        assert!(inv.container.as_ref().unwrap().slots[0].is_none());
        // `ChestMenu.quickMoveStack` moves backwards, filling the hotbar first.
        assert_eq!(inv.hotbar(8).unwrap().count, 5);

        inv.container.as_mut().unwrap().slots[1] = Some(item("minecraft:apple", 4));
        inv.click(1, 1, ContainerInput::Throw).unwrap();
        assert!(inv.container.as_ref().unwrap().slots[1].is_none());
    }

    #[test]
    fn container_swap_and_button_click() {
        let mut inv = chest();
        inv.slots[slot::HOTBAR] = Some(item("minecraft:stone", 3));
        inv.container.as_mut().unwrap().slots[2] = Some(item("minecraft:diamond", 2));
        inv.click(2, 0, ContainerInput::Swap).unwrap();
        assert_eq!(inv.hotbar(0).unwrap().name(), "minecraft:diamond");
        assert_eq!(
            inv.container.as_ref().unwrap().slots[2]
                .as_ref()
                .unwrap()
                .name(),
            "minecraft:stone"
        );
        let packet = inv.click_button(7).unwrap();
        assert_eq!(packet.container_id, 3);
        assert_eq!(packet.button_id, 7);
    }

    #[test]
    fn quick_move_from_player_storage_goes_to_the_container() {
        let mut inv = chest();
        inv.slots[slot::MAIN] = Some(item("minecraft:coal", 8));
        // Container menu slot 27 is the first player main-inventory slot.
        inv.click(27, 0, ContainerInput::QuickMove).unwrap();
        assert!(inv.get(slot::MAIN).is_none());
        assert_eq!(
            inv.container.as_ref().unwrap().slots[0]
                .as_ref()
                .unwrap()
                .count,
            8
        );
    }

    #[test]
    fn clicking_outside_drops_the_cursor() {
        let mut inv = Inventory {
            cursor: Some(item("minecraft:stone", 2)),
            ..Default::default()
        };
        let packet = inv
            .click(Inventory::CLICK_OUTSIDE, 0, ContainerInput::Pickup)
            .unwrap();
        assert!(inv.cursor.is_none());
        assert!(packet.changed_slots.is_empty());
        assert_eq!(packet.carried_item, HashedStack::Empty);
    }

    /// A chest's menu: 27 chest slots, then the player's 27 + 9.
    #[test]
    fn open_container_maps_player_slots() {
        let mut inv = Inventory::default();
        inv.open_screen(&[3, 2, 0]).unwrap();
        let mut body = Vec::new();
        write_var_int(&mut body, 3);
        write_var_int(&mut body, 1);
        write_var_int(&mut body, 63);
        for i in 0..63 {
            match i {
                0 => body.extend(stack_bytes("minecraft:diamond", 5)),
                54 => body.extend(stack_bytes("minecraft:stick", 2)), // first hotbar slot
                _ => body.push(0),
            }
        }
        body.push(0);
        inv.set_content(&body).unwrap();
        let chest = inv.container.as_ref().unwrap();
        assert_eq!(chest.slots.len(), 27);
        assert_eq!(chest.slots[0].as_ref().unwrap().name(), "minecraft:diamond");
        assert_eq!(inv.hotbar(0).unwrap().name(), "minecraft:stick");

        inv.set_slot(&set_slot(3, 2, 27, &stack_bytes("minecraft:apple", 1)))
            .unwrap();
        assert_eq!(inv.get(slot::MAIN).unwrap().name(), "minecraft:apple");
        inv.close_container();
        assert!(inv.container.is_none());
    }

    fn menu(menu_type: i32, slots: usize) -> Inventory {
        let mut inv = Inventory::default();
        inv.open_screen(&[5, menu_type as u8, 0]).unwrap();
        let container = inv.container.as_mut().unwrap();
        container.slots = vec![None; slots];
        container.state_id = 1;
        inv
    }

    #[test]
    fn creative_clone_needs_infinite_materials() {
        let mut inv = chest();
        inv.container.as_mut().unwrap().slots[0] = Some(item("minecraft:diamond", 3));
        let packet = inv.click(0, 2, ContainerInput::Clone).unwrap();
        assert!(packet.changed_slots.is_empty());
        assert!(inv.cursor.is_none());

        inv.creative = true;
        inv.click(0, 2, ContainerInput::Clone).unwrap();
        assert_eq!(inv.cursor.as_ref().unwrap().name(), "minecraft:diamond");
        assert_eq!(inv.cursor.as_ref().unwrap().count, 64);
        // A non-empty cursor is not replaced.
        inv.click(0, 2, ContainerInput::Clone).unwrap();
        assert_eq!(inv.cursor.as_ref().unwrap().count, 64);
    }

    #[test]
    fn pickup_all_takes_partial_stacks_before_full_ones() {
        let mut inv = chest();
        container_set(&mut inv, 0, item("minecraft:stone", 64));
        container_set(&mut inv, 1, item("minecraft:stone", 10));
        inv.cursor = Some(item("minecraft:stone", 1));
        // Slot 2 is empty, so pickup-all can start there.
        let packet = inv.click(2, 0, ContainerInput::PickupAll).unwrap();
        assert_eq!(inv.cursor.as_ref().unwrap().count, 64);
        assert_eq!(container_get(&inv, 1), None);
        assert_eq!(container_get(&inv, 0).unwrap().count, 11);
        assert_eq!(packet.changed_slots.len(), 2);
    }

    #[test]
    fn quick_craft_even_and_one_each() {
        let mut inv = chest();
        inv.cursor = Some(item("minecraft:stone", 64));
        inv.click(0, 0, ContainerInput::QuickCraft).unwrap();
        inv.click(0, 1, ContainerInput::QuickCraft).unwrap();
        inv.click(1, 1, ContainerInput::QuickCraft).unwrap();
        let packet = inv.click(0, 2, ContainerInput::QuickCraft).unwrap();
        assert_eq!(packet.input, ContainerInput::QuickCraft);
        assert_eq!(container_get(&inv, 0).unwrap().count, 32);
        assert_eq!(container_get(&inv, 1).unwrap().count, 32);
        assert!(inv.cursor.is_none());

        let mut inv = chest();
        inv.cursor = Some(item("minecraft:stone", 16));
        // Header 0, type 1: 0 | (1 << 2) = 4.
        inv.click(0, 4, ContainerInput::QuickCraft).unwrap();
        for slot in 0..3 {
            inv.click(slot, 5, ContainerInput::QuickCraft).unwrap();
        }
        inv.click(0, 6, ContainerInput::QuickCraft).unwrap();
        for slot in 0..3 {
            assert_eq!(container_get(&inv, slot).unwrap().count, 1);
        }
        assert_eq!(inv.cursor.as_ref().unwrap().count, 13);
    }

    #[test]
    fn quick_craft_single_slot_is_an_ordinary_pickup() {
        let mut inv = chest();
        inv.cursor = Some(item("minecraft:stone", 7));
        inv.click(0, 0, ContainerInput::QuickCraft).unwrap();
        inv.click(0, 1, ContainerInput::QuickCraft).unwrap();
        inv.click(0, 2, ContainerInput::QuickCraft).unwrap();
        assert_eq!(container_get(&inv, 0).unwrap().count, 7);
        assert!(inv.cursor.is_none());
    }

    #[test]
    fn creative_quick_craft_clones_full_stacks() {
        let mut inv = chest();
        inv.cursor = Some(item("minecraft:diamond", 1));
        // Header 0, type 2: 0 | (2 << 2) = 8.
        inv.click(0, 8, ContainerInput::QuickCraft).unwrap();
        inv.click(0, 9, ContainerInput::QuickCraft).unwrap();
        inv.click(1, 9, ContainerInput::QuickCraft).unwrap();
        inv.click(0, 10, ContainerInput::QuickCraft).unwrap();
        // Not creative: the start was rejected.
        assert!(container_get(&inv, 0).is_none());

        inv.cursor = Some(item("minecraft:diamond", 1));
        inv.creative = true;
        inv.click(0, 8, ContainerInput::QuickCraft).unwrap();
        inv.click(0, 9, ContainerInput::QuickCraft).unwrap();
        inv.click(1, 9, ContainerInput::QuickCraft).unwrap();
        inv.click(0, 10, ContainerInput::QuickCraft).unwrap();
        assert_eq!(container_get(&inv, 0).unwrap().count, 64);
        assert_eq!(container_get(&inv, 1).unwrap().count, 64);
        assert!(inv.cursor.is_none());
    }

    #[test]
    fn result_slots_reject_items_and_furnace_quick_move_routes_inputs() {
        let mut inv = menu(14, 3);
        inv.container.as_mut().unwrap().slots[2] = Some(item("minecraft:iron_ingot", 1));
        inv.cursor = Some(item("minecraft:stone", 1));
        inv.click(2, 0, ContainerInput::Pickup).unwrap();
        assert_eq!(inv.cursor.as_ref().unwrap().name(), "minecraft:stone");
        assert_eq!(
            container_get(&inv, 2).unwrap().name(),
            "minecraft:iron_ingot"
        );

        inv.cursor = None;
        inv.slots[slot::MAIN] = Some(item("minecraft:raw_iron", 4));
        inv.click(3, 0, ContainerInput::QuickMove).unwrap();
        assert_eq!(container_get(&inv, 0).unwrap().name(), "minecraft:raw_iron");
        assert!(inv.get(slot::MAIN).is_none());

        inv.slots[slot::MAIN] = Some(item("minecraft:coal", 4));
        inv.click(3, 0, ContainerInput::QuickMove).unwrap();
        assert_eq!(container_get(&inv, 1).unwrap().name(), "minecraft:coal");
    }

    #[test]
    fn crafter_player_slots_precede_its_result_slot() {
        let mut inv = Inventory::default();
        inv.open_screen(&[5, 7, 0]).unwrap();
        let mut body = Vec::new();
        write_var_int(&mut body, 5);
        write_var_int(&mut body, 1);
        write_var_int(&mut body, 46);
        for i in 0..46 {
            match i {
                0 => body.extend(stack_bytes("minecraft:diamond", 2)),
                9 => body.extend(stack_bytes("minecraft:stick", 3)),
                45 => body.extend(stack_bytes("minecraft:emerald", 1)),
                _ => body.push(0),
            }
        }
        body.push(0);
        inv.set_content(&body).unwrap();
        assert_eq!(inv.menu_len(), 46);
        assert_eq!(inv.menu_item(45).unwrap().name(), "minecraft:emerald");
        assert_eq!(inv.get(slot::MAIN).unwrap().name(), "minecraft:stick");
        // The result slot is non-interactive for quick move.
        inv.click(45, 0, ContainerInput::QuickMove).unwrap();
        assert_eq!(inv.menu_item(45).unwrap().name(), "minecraft:emerald");
        // Player storage quick-moves back into the 3x3 grid.
        inv.click(9, 0, ContainerInput::QuickMove).unwrap();
        assert_eq!(container_get(&inv, 1).unwrap().name(), "minecraft:stick");
    }

    /// `CrafterSlot.mayPlace`: data slots 0..8 switch grid slots off.
    #[test]
    fn crafter_disabled_slots_reject_placement() {
        let mut inv = menu(7, 10);
        inv.cursor = Some(item("minecraft:stone", 4));

        let data = |value: i16| {
            let mut body = Vec::new();
            write_var_int(&mut body, 5);
            body.extend_from_slice(&0i16.to_be_bytes());
            body.extend_from_slice(&value.to_be_bytes());
            body
        };

        inv.set_data(&data(1)).unwrap();
        inv.click(0, 0, ContainerInput::Pickup).unwrap();
        assert_eq!(
            container_get(&inv, 0),
            None,
            "a disabled slot takes nothing"
        );
        assert_eq!(inv.cursor.as_ref().unwrap().count, 4);

        inv.set_data(&data(0)).unwrap();
        inv.click(0, 0, ContainerInput::Pickup).unwrap();
        assert_eq!(container_get(&inv, 0).unwrap().count, 4);
        assert!(inv.cursor.is_none());
    }

    /// `CartographyTableMenu` slot 1 takes plain glass panes, not dyed ones.
    #[test]
    fn cartography_rejects_stained_glass_panes() {
        let inv = menu(23, 3);
        assert!(inv.slot_accepts(1, &item("minecraft:paper", 1)));
        assert!(inv.slot_accepts(1, &item("minecraft:map", 1)));
        assert!(inv.slot_accepts(1, &item("minecraft:glass_pane", 1)));
        assert!(!inv.slot_accepts(1, &item("minecraft:white_stained_glass_pane", 1)));
        assert!(!inv.slot_accepts(0, &item("minecraft:stone", 1)));
    }

    /// When a menu's targeted input move fails, vanilla returns EMPTY; it
    /// does not move the stack between the player's main slots and hotbar.
    #[test]
    fn failed_input_quick_moves_do_not_fall_back() {
        // The clicked menu slot for the inventory's first main slot is the
        // menu's own slot count; the container's slots come first.

        // Loom: the banner slot already holds a different banner.
        let mut loom = menu(18, 4);
        container_set(&mut loom, 0, item("minecraft:red_banner", 1));
        loom.slots[slot::MAIN] = Some(item("minecraft:white_banner", 1));
        loom.click(4, 0, ContainerInput::QuickMove).unwrap();
        assert_eq!(
            loom.get(slot::MAIN).unwrap().name(),
            "minecraft:white_banner"
        );
        assert!(loom.get(slot::HOTBAR).is_none());

        // Cartography: the addition slot already holds a different addition.
        let mut cartography = menu(23, 3);
        container_set(&mut cartography, 1, item("minecraft:paper", 1));
        cartography.slots[slot::MAIN] = Some(item("minecraft:map", 1));
        cartography.click(3, 0, ContainerInput::QuickMove).unwrap();
        assert_eq!(cartography.get(slot::MAIN).unwrap().name(), "minecraft:map");
        assert!(cartography.get(slot::HOTBAR).is_none());

        // Anvil: both inputs are full.
        let mut anvil = menu(8, 3);
        container_set(&mut anvil, 0, item("minecraft:netherite_scrap", 1));
        container_set(&mut anvil, 1, item("minecraft:netherite_scrap", 1));
        anvil.slots[slot::MAIN] = Some(item("minecraft:dirt", 1));
        anvil.click(3, 0, ContainerInput::QuickMove).unwrap();
        assert_eq!(anvil.get(slot::MAIN).unwrap().name(), "minecraft:dirt");
        assert!(anvil.get(slot::HOTBAR).is_none());

        // Lectern: quick move is empty for every slot.
        let mut lectern = menu(17, 1);
        lectern.slots[slot::MAIN] = Some(item("minecraft:writable_book", 1));
        lectern.click(1, 0, ContainerInput::QuickMove).unwrap();
        assert!(lectern.get(slot::MAIN).is_some());
        assert!(lectern.get(slot::HOTBAR).is_none());
    }

    /// `BrewingStandMenu.PotionSlot`: potion items and empty bottles only.
    /// Tipped arrows carry the `potion_contents` default too.
    #[test]
    fn brewing_potion_slots_reject_tipped_arrows() {
        let inv = menu(11, 5);
        for name in [
            "minecraft:potion",
            "minecraft:splash_potion",
            "minecraft:lingering_potion",
            "minecraft:glass_bottle",
        ] {
            assert!(inv.slot_accepts(0, &item(name, 1)), "{name}");
        }
        assert!(!inv.slot_accepts(0, &item("minecraft:tipped_arrow", 1)));
        assert!(!inv.slot_accepts(2, &item("minecraft:tipped_arrow", 1)));
    }

    /// `FurnaceFuelSlot`: empty buckets are placeable, one at a time.
    #[test]
    fn furnace_fuel_slot_takes_buckets_one_at_a_time() {
        let inv = menu(14, 3);
        let bucket = item("minecraft:bucket", 3);
        assert!(inv.slot_accepts(1, &bucket));
        assert_eq!(inv.slot_max_stack(1, &bucket), 1);
        assert!(!inv.slot_accepts(1, &item("minecraft:stone", 3)));
        assert!(inv.slot_accepts(1, &item("minecraft:coal", 3)));
        assert_eq!(inv.slot_max_stack(1, &item("minecraft:coal", 3)), 64);
    }

    /// The crafter's result is a `NonInteractiveResultSlot`: nothing takes
    /// from it, and collection may start there but skips it.
    #[test]
    fn crafter_result_slot_is_non_interactive() {
        let mut inv = menu(7, 10);
        container_set(&mut inv, 9, item("minecraft:diamond", 1));

        inv.pickup(45, false);
        inv.quick_move(45);
        inv.throw(45, false);
        assert!(inv.cursor.is_none());
        assert_eq!(container_get(&inv, 9).unwrap().count, 1);

        let mut creative = menu(7, 10);
        container_set(&mut creative, 9, item("minecraft:diamond", 1));
        creative.creative = true;
        creative.clone_slot(45);
        assert!(creative.cursor.is_none());

        // A swap from the hotbar cannot displace it either.
        let mut inv = menu(7, 10);
        container_set(&mut inv, 9, item("minecraft:diamond", 1));
        inv.slots[slot::HOTBAR] = Some(item("minecraft:stone", 1));
        inv.swap(45, 0);
        assert_eq!(inv.get(slot::HOTBAR).unwrap().name(), "minecraft:stone");
        assert_eq!(container_get(&inv, 9).unwrap().name(), "minecraft:diamond");

        // Pickup-all may start from the result, but only collects elsewhere.
        let mut inv = menu(7, 10);
        container_set(&mut inv, 9, item("minecraft:diamond", 1));
        container_set(&mut inv, 0, item("minecraft:diamond", 2));
        inv.cursor = Some(item("minecraft:diamond", 1));
        inv.pickup_all(45, false);
        assert_eq!(container_get(&inv, 9).unwrap().count, 1);
        assert!(container_get(&inv, 0).is_none());
        assert_eq!(inv.cursor.as_ref().unwrap().count, 3);
    }

    /// `BeaconMenu`: only a single payment item claims the payment slot;
    /// anything else takes the main-to-hotbar fallback.
    #[test]
    fn beacon_payment_only_for_single_payment_items() {
        // The beacon's first menu slot is the payment slot, so its first
        // player slot (the inventory's main slot 9) is menu slot 1.
        let mut inv = menu(9, 1);
        inv.slots[slot::MAIN] = Some(item("minecraft:diamond", 1));
        inv.click(1, 0, ContainerInput::QuickMove).unwrap();
        assert_eq!(container_get(&inv, 0).unwrap().name(), "minecraft:diamond");

        // Two payment items do not fit the single payment slot.
        let mut inv = menu(9, 1);
        inv.slots[slot::MAIN] = Some(item("minecraft:emerald", 2));
        inv.click(1, 0, ContainerInput::QuickMove).unwrap();
        assert_eq!(container_get(&inv, 0), None);
        assert_eq!(inv.get(slot::HOTBAR).unwrap().name(), "minecraft:emerald");

        // A non-payment item falls back to the hotbar, not the slot.
        let mut inv = menu(9, 1);
        inv.slots[slot::MAIN] = Some(item("minecraft:stone", 1));
        inv.click(1, 0, ContainerInput::QuickMove).unwrap();
        assert_eq!(container_get(&inv, 0), None);
        assert_eq!(inv.get(slot::HOTBAR).unwrap().name(), "minecraft:stone");
    }

    /// `BrewingStandMenu`: fuel goes to the fuel slot, bottles to the
    /// bottle slots, and the rest is treated as a reagent.
    #[test]
    fn brewing_quick_move_routes_fuel_bottles_and_reagents() {
        // The brewing stand's five own slots come first, so the inventory's
        // first main slot is menu slot 5.
        // Fuel fits the fuel slot.
        let mut inv = menu(11, 5);
        inv.slots[slot::MAIN] = Some(item("minecraft:blaze_powder", 4));
        inv.click(5, 0, ContainerInput::QuickMove).unwrap();
        assert_eq!(
            container_get(&inv, 4).unwrap().name(),
            "minecraft:blaze_powder"
        );

        // Fuel overflows into the ingredient slot (blaze powder is a
        // reagent), then stops without touching the player's storage.
        let mut inv = menu(11, 5);
        container_set(&mut inv, 4, item("minecraft:blaze_powder", 64));
        inv.slots[slot::MAIN] = Some(item("minecraft:blaze_powder", 4));
        inv.click(5, 0, ContainerInput::QuickMove).unwrap();
        assert_eq!(
            container_get(&inv, 3).unwrap().name(),
            "minecraft:blaze_powder"
        );
        assert!(inv.get(slot::MAIN).is_none());

        // A reagent claims the ingredient slot; a full ingredient slot
        // leaves it in place.
        let mut inv = menu(11, 5);
        inv.slots[slot::MAIN] = Some(item("minecraft:nether_wart", 4));
        inv.click(5, 0, ContainerInput::QuickMove).unwrap();
        assert_eq!(
            container_get(&inv, 3).unwrap().name(),
            "minecraft:nether_wart"
        );

        let mut inv = menu(11, 5);
        container_set(&mut inv, 3, item("minecraft:nether_wart", 64));
        inv.slots[slot::MAIN] = Some(item("minecraft:nether_wart", 4));
        inv.click(5, 0, ContainerInput::QuickMove).unwrap();
        assert_eq!(inv.get(slot::MAIN).unwrap().count, 4);
        assert!(inv.get(slot::HOTBAR).is_none());

        // Bottles and potions go to the bottle slots.
        for name in [
            "minecraft:glass_bottle",
            "minecraft:potion",
            "minecraft:splash_potion",
        ] {
            let mut inv = menu(11, 5);
            inv.slots[slot::MAIN] = Some(item(name, 1));
            inv.click(5, 0, ContainerInput::QuickMove).unwrap();
            assert_eq!(container_get(&inv, 0).unwrap().name(), name);
        }
    }

    /// `CrafterMenu`: the grid quick-moves to the player storage in reverse
    /// order, filling the hotbar's last slot first.
    #[test]
    fn crafter_grid_quick_moves_in_reverse() {
        let mut inv = menu(7, 10);
        container_set(&mut inv, 0, item("minecraft:oak_planks", 3));
        inv.click(0, 0, ContainerInput::QuickMove).unwrap();
        assert!(container_get(&inv, 0).is_none());
        assert_eq!(inv.get(44).unwrap().name(), "minecraft:oak_planks");
        assert!(inv.get(9).is_none());
    }

    /// `GrindstoneMenu` inputs take damageable or enchanted items only.
    #[test]
    fn grindstone_rejects_plain_items() {
        let inv = menu(15, 3);
        // `minecraft:enchantments` is a default (empty) component on every
        // item, so the check must look at the entries, not the component.
        assert!(!inv.slot_accepts(0, &item("minecraft:stone", 1)));
        assert!(!inv.slot_accepts(1, &item("minecraft:stone", 1)));
    }

    #[test]
    fn player_quick_move_auto_equips_armour() {
        let mut inv = Inventory::default();
        inv.slots[slot::MAIN] = Some(item("minecraft:diamond_chestplate", 1));
        inv.click(slot::MAIN as i16, 0, ContainerInput::QuickMove)
            .unwrap();
        assert_eq!(
            inv.get(slot::CHEST).unwrap().name(),
            "minecraft:diamond_chestplate"
        );
        assert!(inv.get(slot::MAIN).is_none());
    }

    fn container_get(inv: &Inventory, index: usize) -> Option<&ItemStack> {
        inv.container.as_ref()?.slots.get(index)?.as_ref()
    }

    fn container_set(inv: &mut Inventory, index: usize, stack: ItemStack) {
        inv.container.as_mut().unwrap().slots[index] = Some(stack);
    }
}
