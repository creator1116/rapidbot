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
        Self {
            slots: vec![None; slot::COUNT],
            cursor: None,
            state_id: 0,
            selected: 0,
            container: None,
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
        self.container = Some(Container {
            id,
            menu_type,
            ..Default::default()
        });
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
        self.container
            .as_ref()
            .map_or(slot::COUNT, |c| c.slots.len() + 36)
    }

    /// A menu slot as the server numbers it. With a container open its own
    /// slots come first, then the player's 27 main slots and 9 hotbar slots.
    pub fn menu_item(&self, index: usize) -> Option<&ItemStack> {
        if let Some(container) = &self.container {
            if index < container.slots.len() {
                return container.slots.get(index)?.as_ref();
            }
            return self
                .slots
                .get(slot::MAIN + index - container.slots.len())?
                .as_ref();
        }
        self.slots.get(index)?.as_ref()
    }

    fn set_menu_item(&mut self, index: usize, item: Option<ItemStack>) -> bool {
        if let Some(container) = &mut self.container {
            if index < container.slots.len() {
                container.slots[index] = item;
                return true;
            }
            let Some(slot) = self
                .slots
                .get_mut(slot::MAIN + index - container.slots.len())
            else {
                return false;
            };
            *slot = item;
            return true;
        }
        let Some(slot) = self.slots.get_mut(index) else {
            return false;
        };
        *slot = item;
        true
    }

    /// The inventory menu's slot filters (`InventoryMenu` / `Slot.mayPlace`).
    /// Container slots are unconstrained here; a server still rejects what
    /// its menu does not allow.
    fn slot_accepts(&self, index: usize, stack: &ItemStack) -> bool {
        if self.container.is_some() {
            return true;
        }
        match index {
            slot::CRAFT_RESULT => false,
            1..=4 => true,
            slot::HEAD..=slot::FEET => {
                let expected = match index {
                    slot::HEAD => EquipmentSlot::Head,
                    slot::CHEST => EquipmentSlot::Chest,
                    slot::LEGS => EquipmentSlot::Legs,
                    _ => EquipmentSlot::Feet,
                };
                stack.equip_slot() == Some(expected)
            }
            _ => true,
        }
    }

    fn slot_max_stack(&self, index: usize, stack: &ItemStack) -> i32 {
        if self.container.is_none() && (slot::HEAD..=slot::FEET).contains(&index) {
            return 1;
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

    /// `quickMoveStack` for the menus this client opens: a container's own
    /// slots and the player's 27 + 9 storage slots. Stack merging and the
    /// first empty slot are enough for normal chest/furnace work; specialised
    /// menus still settle on the server's slot updates.
    fn quick_move(&mut self, slot: i16) {
        let Ok(index) = usize::try_from(slot) else {
            return;
        };
        if index >= self.menu_len() {
            return;
        }
        let Some(mut stack) = self.menu_item(index).cloned() else {
            return;
        };
        let targets = if let Some(container) = &self.container {
            let own = container.slots.len();
            if index < own {
                (own..own + 36).collect::<Vec<_>>()
            } else {
                (0..own).collect()
            }
        } else if (slot::HOTBAR..slot::HOTBAR + 9).contains(&index) {
            (slot::MAIN..slot::HOTBAR).collect()
        } else if (slot::MAIN..slot::HOTBAR).contains(&index) {
            (slot::HOTBAR..slot::HOTBAR + 9).collect()
        } else {
            (slot::MAIN..slot::HOTBAR + 9).collect()
        };
        for target in targets {
            if stack.count == 0 {
                break;
            }
            let amount = stack.count;
            self.insert_at(target, &mut stack, amount);
        }
        self.set_menu_item(index, (stack.count > 0).then_some(stack));
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
        if index >= self.menu_len() {
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
    /// this client does not implement (creative clone, drag, pickup-all).
    pub fn click(
        &mut self,
        slot: i16,
        button: i8,
        input: ContainerInput,
    ) -> Option<ClickContainer> {
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
        assert_eq!(inv.get(slot::MAIN).unwrap().count, 5);

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
}
