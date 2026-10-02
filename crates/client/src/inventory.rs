//! The player's inventory as the server describes it: `Inventory`,
//! `InventoryMenu` and whichever container menu is open.

use rapidbot_buf::{Decode, DecodeError, VarInt};
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

/// A container menu other than the player's own (a chest, a furnace).
#[derive(Debug, Clone, Default)]
pub struct Container {
    pub id: i32,
    /// `MenuType` registry ID from `open_screen`.
    pub menu_type: i32,
    /// The container's own slots. The player's 36 inventory slots that
    /// follow them in the menu live in [`Inventory`].
    pub slots: Vec<Option<ItemStack>>,
    pub state_id: i32,
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
}

impl Default for Inventory {
    fn default() -> Self {
        Self { slots: vec![None; slot::COUNT], cursor: None, state_id: 0, selected: 0, container: None }
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
        [MainHand, OffHand, Feet, Legs, Chest, Head].into_iter().any(|s| self.equipped(s).is_some_and(|i| i.can_glide_in(s)))
    }

    /// The hotbar slot holding an item (`minecraft:diamond_pickaxe`).
    pub fn hotbar_slot_of(&self, name: &str) -> Option<u8> {
        (0..9).find(|i| self.hotbar(*i).is_some_and(|s| s.name() == name))
    }

    /// Items in the inventory screen's slots, with their menu slot.
    pub fn items(&self) -> impl Iterator<Item = (usize, &ItemStack)> {
        self.slots.iter().enumerate().filter_map(|(i, s)| s.as_ref().map(|s| (i, s)))
    }

    /// How many of an item the 36 storage slots and the offhand hold.
    pub fn count(&self, name: &str) -> i32 {
        self.items().filter(|(i, s)| *i >= slot::MAIN && s.name() == name).map(|(_, s)| s.count).sum()
    }

    /// With a container open, its menu lists the container's slots and then
    /// the player's 27 + 9: map a slot past the container's to ours.
    fn player_slot(container_slots: usize, index: usize) -> Option<usize> {
        let offset = index.checked_sub(container_slots)?;
        (offset < 36).then_some(slot::MAIN + offset)
    }

    /// `handleContainerContent`. Returns the slots that changed.
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
        } else if let Some(container) = self.container.as_mut().filter(|c| c.id == id) {
            let own = items.len().saturating_sub(36);
            let mut items = items.into_iter();
            container.slots = items.by_ref().take(own).collect();
            container.state_id = state_id;
            for (offset, item) in items.enumerate() {
                self.slots[slot::MAIN + offset] = item;
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
        } else if let Some(container) = self.container.as_mut().filter(|c| c.id == id) {
            container.state_id = state_id;
            let own = container.slots.len();
            match at {
                Some(i) if i < own => container.slots[i] = item,
                Some(i) => {
                    if let Some(slot) = Self::player_slot(own, i) {
                        self.slots[slot] = item;
                    }
                }
                None => {}
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
        self.container = Some(Container { id, menu_type, ..Default::default() });
        Ok(())
    }

    /// `closeContainer`: back to the inventory menu, cursor emptied.
    pub(crate) fn close_container(&mut self) {
        self.container = None;
        self.cursor = None;
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
        inv.set_slot(&set_slot(0, 1, 36, &stack_bytes("minecraft:elytra", 1))).unwrap();
        assert!(!inv.can_glide());
        assert_eq!(inv.held().unwrap().name(), "minecraft:elytra");
        inv.set_slot(&set_slot(0, 2, slot::CHEST as i16, &stack_bytes("minecraft:elytra", 1))).unwrap();
        assert!(inv.can_glide());
        assert_eq!(inv.state_id, 2);
        // Taken off again.
        inv.set_slot(&set_slot(0, 3, slot::CHEST as i16, &[0])).unwrap();
        assert!(!inv.can_glide());
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

        inv.set_slot(&set_slot(3, 2, 27, &stack_bytes("minecraft:apple", 1))).unwrap();
        assert_eq!(inv.get(slot::MAIN).unwrap().name(), "minecraft:apple");
        inv.close_container();
        assert!(inv.container.is_none());
    }
}
