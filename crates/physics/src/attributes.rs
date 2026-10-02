//! `AttributeMap` / `AttributeInstance`.

use std::collections::HashMap;

/// `AttributeModifier.Operation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    AddValue,
    AddMultipliedBase,
    AddMultipliedTotal,
}

impl Operation {
    pub fn from_id(id: i32) -> Option<Self> {
        Some(match id {
            0 => Operation::AddValue,
            1 => Operation::AddMultipliedBase,
            2 => Operation::AddMultipliedTotal,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Instance {
    pub base: f64,
    min: f64,
    max: f64,
    /// Per operation, in insertion order (vanilla iterates each operation's
    /// map in order, so float sums come out the same).
    modifiers: [Vec<(String, f64)>; 3],
}

impl Instance {
    pub fn new(base: f64, min: f64, max: f64) -> Self {
        Self { base, min, max, modifiers: Default::default() }
    }

    /// `AttributeInstance.calculateValue` then `RangedAttribute.sanitizeValue`.
    pub fn value(&self) -> f64 {
        let mut base = self.base;
        for (_, amount) in &self.modifiers[0] {
            base += amount;
        }
        let mut result = base;
        for (_, amount) in &self.modifiers[1] {
            result += base * amount;
        }
        for (_, amount) in &self.modifiers[2] {
            result *= 1.0 + amount;
        }
        if result.is_nan() {
            self.min
        } else if result < self.min {
            self.min
        } else {
            rapidbot_world::aabb::java_min(result, self.max)
        }
    }

    pub fn add_modifier(&mut self, id: &str, amount: f64, op: Operation) {
        self.remove_modifier(id);
        self.modifiers[op as usize].push((id.to_owned(), amount));
    }

    pub fn remove_modifier(&mut self, id: &str) {
        for list in &mut self.modifiers {
            list.retain(|(m, _)| m != id);
        }
    }

    pub fn remove_modifiers(&mut self) {
        for list in &mut self.modifiers {
            list.clear();
        }
    }
}

/// The attributes the movement code reads, with vanilla's player defaults.
#[derive(Debug, Clone)]
pub struct Attributes {
    instances: HashMap<&'static str, Instance>,
}

pub const MOVEMENT_SPEED: &str = "minecraft:movement_speed";
pub const JUMP_STRENGTH: &str = "minecraft:jump_strength";
pub const STEP_HEIGHT: &str = "minecraft:step_height";
pub const GRAVITY: &str = "minecraft:gravity";
pub const SNEAKING_SPEED: &str = "minecraft:sneaking_speed";
pub const MOVEMENT_EFFICIENCY: &str = "minecraft:movement_efficiency";
pub const FRICTION_MODIFIER: &str = "minecraft:friction_modifier";
pub const AIR_DRAG_MODIFIER: &str = "minecraft:air_drag_modifier";
pub const WATER_MOVEMENT_EFFICIENCY: &str = "minecraft:water_movement_efficiency";
pub const MINING_EFFICIENCY: &str = "minecraft:mining_efficiency";
pub const BLOCK_BREAK_SPEED: &str = "minecraft:block_break_speed";
pub const SUBMERGED_MINING_SPEED: &str = "minecraft:submerged_mining_speed";
pub const BLOCK_INTERACTION_RANGE: &str = "minecraft:block_interaction_range";
pub const ENTITY_INTERACTION_RANGE: &str = "minecraft:entity_interaction_range";
pub const ATTACK_SPEED: &str = "minecraft:attack_speed";
pub const ATTACK_KNOCKBACK: &str = "minecraft:attack_knockback";

/// `LivingEntity.SPRINTING_MODIFIER_ID` / `SPEED_MODIFIER_SPRINTING` (0.3F).
pub const SPRINTING_MODIFIER_ID: &str = "minecraft:sprinting";
pub const SPRINTING_MODIFIER_AMOUNT: f64 = 0.3f32 as f64;

impl Default for Attributes {
    fn default() -> Self {
        let mut instances = HashMap::new();
        // Registry defaults (`Attributes`), except movement speed which
        // `Player.createAttributes` sets to 0.1F. Float literals are widened
        // exactly as Java widens them.
        instances.insert(MOVEMENT_SPEED, Instance::new(0.1f32 as f64, 0.0, 1024.0));
        instances.insert(JUMP_STRENGTH, Instance::new(0.42f32 as f64, 0.0, 32.0));
        instances.insert(STEP_HEIGHT, Instance::new(0.6, 0.0, 10.0));
        instances.insert(GRAVITY, Instance::new(0.08, -1.0, 1.0));
        instances.insert(SNEAKING_SPEED, Instance::new(0.3, 0.0, 1.0));
        instances.insert(MOVEMENT_EFFICIENCY, Instance::new(0.0, 0.0, 1.0));
        instances.insert(FRICTION_MODIFIER, Instance::new(1.0, 0.0, 2048.0));
        instances.insert(AIR_DRAG_MODIFIER, Instance::new(1.0, 0.0, 2048.0));
        instances.insert(WATER_MOVEMENT_EFFICIENCY, Instance::new(0.0, 0.0, 1.0));
        instances.insert(MINING_EFFICIENCY, Instance::new(0.0, 0.0, 1024.0));
        instances.insert(BLOCK_BREAK_SPEED, Instance::new(1.0, 0.0, 1024.0));
        instances.insert(SUBMERGED_MINING_SPEED, Instance::new(0.2, 0.0, 20.0));
        instances.insert(BLOCK_INTERACTION_RANGE, Instance::new(4.5, 0.0, 64.0));
        instances.insert(ENTITY_INTERACTION_RANGE, Instance::new(3.0, 0.0, 64.0));
        // Synced by the server with the held item's modifier applied.
        instances.insert(ATTACK_SPEED, Instance::new(4.0, 0.0, 1024.0));
        // Not synced: the client only ever sees the default.
        instances.insert(ATTACK_KNOCKBACK, Instance::new(0.0, 0.0, 5.0));
        Self { instances }
    }
}

impl Attributes {
    pub fn value(&self, name: &str) -> f64 {
        self.instances[name].value()
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Instance> {
        self.instances.get_mut(name)
    }

    /// Drops every modifier, keeping base values (`assignBaseValues`).
    pub fn clear_modifiers(&mut self) {
        for instance in self.instances.values_mut() {
            instance.remove_modifiers();
        }
    }

    /// Applies a `ClientboundUpdateAttributesPacket` snapshot: new base,
    /// all modifiers replaced (`handleUpdateAttributes`).
    pub fn apply_snapshot(&mut self, name: &str, base: f64, modifiers: &[(String, f64, Operation)]) {
        if let Some(instance) = self.instances.get_mut(name) {
            instance.base = base;
            instance.remove_modifiers();
            for (id, amount, op) in modifiers {
                instance.add_modifier(id, *amount, *op);
            }
        }
    }
}
