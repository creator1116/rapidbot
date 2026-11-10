// Dumps block collision data from the vanilla server jar as JSON.
//
// Run through tools/extract_blocks.py, which compiles this against the jar.
// Shapes are exported in VoxelShape form (per-axis coordinate lists plus the
// filled cells of the discrete shape) so collision can be reproduced exactly,
// including step-up candidates, which read the coordinate lists directly.

import java.io.FileWriter;
import java.io.Writer;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

import it.unimi.dsi.fastutil.doubles.DoubleList;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.resources.Identifier;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.flag.FeatureFlagSet;
import net.minecraft.world.flag.FeatureFlags;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockBehaviour;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;
import net.minecraft.world.level.material.FluidState;
import net.minecraft.world.phys.AABB;
import net.minecraft.world.phys.Vec3;
import net.minecraft.world.phys.shapes.DiscreteVoxelShape;
import net.minecraft.world.phys.shapes.VoxelShape;

public class Extract {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        Field discreteField = VoxelShape.class.getDeclaredField("shape");
        discreteField.setAccessible(true);

        Map<String, Integer> shapeIndex = new HashMap<>();
        List<String> shapes = new ArrayList<>();
        shapes.add("null"); // index 0: empty
        shapeIndex.put("empty", 0);

        StringBuilder states = new StringBuilder();
        int stateCount = 0;
        for (BlockState state : Block.BLOCK_STATE_REGISTRY) {
            int id = Block.getId(state);
            if (id != stateCount) throw new IllegalStateException("state ids not dense at " + id);
            stateCount++;

            VoxelShape shape = state.getCollisionShape(EmptyBlockGetter.INSTANCE, BlockPos.ZERO);
            // BlockCollisions treats the Shapes.block() singleton specially
            // (strict AABB intersection instead of joinIsNotEmpty), so
            // identity matters, not just geometry.
            String key = shape.isEmpty() ? "empty"
                : shapeJson(shape, (DiscreteVoxelShape) discreteField.get(shape), shape == net.minecraft.world.phys.shapes.Shapes.block());
            Integer index = shapeIndex.get(key);
            if (index == null) {
                index = shapes.size();
                shapes.add(key);
                shapeIndex.put(key, index);
            }

            // The crosshair raycast (ClipContext.Block.OUTLINE) uses getShape.
            VoxelShape outline = state.getShape(EmptyBlockGetter.INSTANCE, BlockPos.ZERO);
            String outlineKey = outline.isEmpty() ? "empty"
                : shapeJson(outline, (DiscreteVoxelShape) discreteField.get(outline), outline == net.minecraft.world.phys.shapes.Shapes.block());
            Integer outlineIndex = shapeIndex.get(outlineKey);
            if (outlineIndex == null) {
                outlineIndex = shapes.size();
                shapes.add(outlineKey);
                shapeIndex.put(outlineKey, outlineIndex);
            }

            // Cauldrons, hoppers and composters: the face reported for a hit
            // can come from this shape (clipWithInteractionOverride).
            VoxelShape interaction = state.getInteractionShape(EmptyBlockGetter.INSTANCE, BlockPos.ZERO);
            String interactionKey = interaction.isEmpty() ? "empty"
                : shapeJson(interaction, (DiscreteVoxelShape) discreteField.get(interaction), interaction == net.minecraft.world.phys.shapes.Shapes.block());
            Integer interactionIndex = shapeIndex.get(interactionKey);
            if (interactionIndex == null) {
                interactionIndex = shapes.size();
                shapes.add(interactionKey);
                shapeIndex.put(interactionKey, interactionIndex);
            }

            FluidState fluid = state.getFluidState();
            StringBuilder props = new StringBuilder();
            for (Property<?> p : state.getProperties()) {
                if (props.length() > 0) props.append(',');
                props.append(p.getName()).append('=').append(valueName(state, p));
            }

            if (states.length() > 0) states.append(",\n");
            states.append("[")
                .append(BuiltInRegistries.BLOCK.getId(state.getBlock())).append(',')
                .append(index).append(',')
                .append(state.isAir() ? 1 : 0).append(',')
                .append(state.hasLargeCollisionShape() ? 1 : 0).append(',')
                .append(quote(fluid.isEmpty() ? "" : BuiltInRegistries.FLUID.getKey(fluid.getType()).toString())).append(',')
                .append(fluid.getAmount()).append(',')
                .append(fluid.isEmpty() ? 0 : (fluid.getValue(net.minecraft.world.level.material.FlowingFluid.FALLING) ? 1 : 0)).append(',')
                .append(quote(props.toString())).append(',')
                .append(faceFlags(state)).append(',')
                .append(outlineIndex).append(',')
                .append(Float.toString(state.getDestroySpeed(EmptyBlockGetter.INSTANCE, BlockPos.ZERO))).append(',')
                .append(interactionIndex).append(',')
                .append(state.canBeReplaced() ? 1 : 0)
                .append("]");
        }

        StringBuilder blocks = new StringBuilder();
        for (Block block : BuiltInRegistries.BLOCK) {
            if (blocks.length() > 0) blocks.append(",\n");
            blocks.append("{\"id\":").append(BuiltInRegistries.BLOCK.getId(block))
                .append(",\"name\":").append(quote(BuiltInRegistries.BLOCK.getKey(block).toString()))
                .append(",\"friction\":").append(Float.toString(block.getFriction()))
                .append(",\"speed_factor\":").append(Float.toString(block.getSpeedFactor()))
                .append(",\"jump_factor\":").append(Float.toString(block.getJumpFactor()))
                .append(",\"bounce\":").append(Float.toString(block.getBounceRestitution()))
                .append(",\"dynamic_shape\":").append(block.hasDynamicShape())
                // Feature flags beyond `minecraft:vanilla`, which every server
                // enables: `MultiPlayerGameMode.useItemOn` refuses a click on
                // a block whose flags the connection has not enabled.
                .append(",\"features\":").append(featureNames(block.requiredFeatures()))
                .append(",\"default_state\":").append(Block.getId(block.defaultBlockState()))
                .append(",\"interactive\":").append(hasCustomUse(block))
                // Grass, flowers, saplings and bamboo are drawn, hit and
                // collided with where their offset type puts them: a hash of
                // the block's position (BlockBehaviour.Properties.offsetType).
                .append(",\"offset_type\":").append(offsetType(block))
                .append(",\"offset_shape\":").append(shapeFollowsOffset(block))
                .append(",\"max_h_offset\":").append(Float.toString(maxHorizontalOffset(block)))
                .append(",\"max_v_offset\":").append(Float.toString(maxVerticalOffset(block)))
                .append("}");
        }

        // Attributes are referenced by registry ID in update_attributes.
        StringBuilder attributes = new StringBuilder();
        for (net.minecraft.world.entity.ai.attributes.Attribute attribute : BuiltInRegistries.ATTRIBUTE) {
            if (attributes.length() > 0) attributes.append(",\n");
            // Unranged attributes do not clamp; +-MAX_VALUE stands in for
            // infinity, which JSON cannot express.
            double min = -Double.MAX_VALUE, max = Double.MAX_VALUE;
            if (attribute instanceof net.minecraft.world.entity.ai.attributes.RangedAttribute ranged) {
                min = ranged.getMinValue();
                max = ranged.getMaxValue();
            }
            attributes.append("{\"id\":").append(BuiltInRegistries.ATTRIBUTE.getId(attribute))
                .append(",\"name\":").append(quote(BuiltInRegistries.ATTRIBUTE.getKey(attribute).toString()))
                .append(",\"default\":").append(Double.toString(attribute.getDefaultValue()))
                .append(",\"min\":").append(Double.toString(min))
                .append(",\"max\":").append(Double.toString(max))
                .append("}");
        }

        try (Writer w = new FileWriter(args[0])) {
            w.write("{\"version\":" + quote(SharedConstants.getCurrentVersion().id()) + ",\n");
            w.write("\"attributes\":[\n" + attributes + "],\n");
            w.write("\"entity_types\":" + names(BuiltInRegistries.ENTITY_TYPE) + ",\n");
            // Width, height and eye height of each entity type's default pose.
            StringBuilder dims = new StringBuilder("[");
            for (int i = 0; i < BuiltInRegistries.ENTITY_TYPE.size(); i++) {
                net.minecraft.world.entity.EntityDimensions d = BuiltInRegistries.ENTITY_TYPE.byId(i).getDimensions();
                if (i > 0) dims.append(',');
                dims.append('[').append(Float.toString(d.width())).append(',').append(Float.toString(d.height())).append(',')
                    .append(Float.toString(d.eyeHeight())).append(']');
            }
            w.write("\"entity_dimensions\":" + dims + "],\n");
            // Per entity type: the tracking update interval (the length of a
            // client interpolation step) and the entity class with its
            // superclasses, which decide how the client interpolates it and
            // whether the crosshair can pick it. The class is the type
            // argument of the EntityType constant.
            Map<Object, String> classes = new HashMap<>();
            for (Field f : net.minecraft.world.entity.EntityTypes.class.getDeclaredFields()) {
                if (!java.lang.reflect.Modifier.isStatic(f.getModifiers()) || f.getType() != net.minecraft.world.entity.EntityType.class) continue;
                if (!(f.getGenericType() instanceof java.lang.reflect.ParameterizedType p)) continue;
                if (!(p.getActualTypeArguments()[0] instanceof Class<?> c)) continue;
                StringBuilder chain = new StringBuilder();
                for (Class<?> k = c; k != null && k != Object.class; k = k.getSuperclass()) {
                    if (chain.length() > 0) chain.append(' ');
                    chain.append(k.getSimpleName());
                }
                f.setAccessible(true);
                classes.put(f.get(null), chain.toString());
            }
            StringBuilder info = new StringBuilder("[");
            for (int i = 0; i < BuiltInRegistries.ENTITY_TYPE.size(); i++) {
                net.minecraft.world.entity.EntityType<?> t = BuiltInRegistries.ENTITY_TYPE.byId(i);
                if (i > 0) info.append(',');
                info.append('[').append(t.updateInterval()).append(',').append(quote(classes.getOrDefault(t, ""))).append(']');
            }
            w.write("\"entity_info\":" + info + "],\n");
            w.write("\"mob_effects\":" + names(BuiltInRegistries.MOB_EFFECT) + ",\n");
            // Argument parsers are referenced by registry ID in the commands
            // packet; items by ID in item stacks.
            w.write("\"command_argument_types\":" + names(BuiltInRegistries.COMMAND_ARGUMENT_TYPE) + ",\n");
            w.write("\"items\":" + names(BuiltInRegistries.ITEM) + ",\n");
            // A block item places exactly one block: -1 means not a BlockItem.
            // Item and block names usually match, but not always (redstone,
            // seeds, ...), so this mapping cannot be derived from names.
            StringBuilder itemBlocks = new StringBuilder("[");
            StringBuilder itemWallBlocks = new StringBuilder("[");
            StringBuilder itemWallDirections = new StringBuilder("[");
            Field wallBlockField = net.minecraft.world.item.StandingAndWallBlockItem.class.getDeclaredField("wallBlock");
            wallBlockField.setAccessible(true);
            Field attachmentDirectionField = net.minecraft.world.item.StandingAndWallBlockItem.class.getDeclaredField("attachmentDirection");
            attachmentDirectionField.setAccessible(true);
            for (int i = 0; i < BuiltInRegistries.ITEM.size(); i++) {
                net.minecraft.world.item.Item item = BuiltInRegistries.ITEM.byId(i);
                if (i > 0) {
                    itemBlocks.append(',');
                    itemWallBlocks.append(',');
                    itemWallDirections.append(',');
                }
                itemBlocks.append(item instanceof net.minecraft.world.item.BlockItem blockItem
                    ? BuiltInRegistries.BLOCK.getId(blockItem.getBlock())
                    : -1);
                if (item instanceof net.minecraft.world.item.StandingAndWallBlockItem standingAndWall) {
                    Block wallBlock = (Block) wallBlockField.get(standingAndWall);
                    Direction attachmentDirection = (Direction) attachmentDirectionField.get(standingAndWall);
                    itemWallBlocks.append(BuiltInRegistries.BLOCK.getId(wallBlock));
                    itemWallDirections.append(attachmentDirection.get3DDataValue());
                } else {
                    itemWallBlocks.append(-1);
                    itemWallDirections.append(-1);
                }
            }
            w.write("\"item_blocks\":" + itemBlocks + "],\n");
            w.write("\"item_wall_blocks\":" + itemWallBlocks + "],\n");
            w.write("\"item_wall_directions\":" + itemWallDirections + "],\n");
            w.write("\"shapes\":[\n" + String.join(",\n", shapes) + "],\n");
            w.write("\"blocks\":[\n" + blocks + "],\n");
            w.write("\"states\":[\n" + states + "]}\n");
        }
        System.out.println(stateCount + " states, " + (shapes.size() - 1) + " distinct shapes");

        // Mth's lookup tables are built with the JVM's Math.asin/cos/sin,
        // which other maths libraries do not reproduce bit for bit.
        Field asinField = net.minecraft.util.Mth.class.getDeclaredField("ASIN_TAB");
        Field cosField = net.minecraft.util.Mth.class.getDeclaredField("COS_TAB");
        Field sinField = net.minecraft.util.Mth.class.getDeclaredField("SIN");
        asinField.setAccessible(true);
        cosField.setAccessible(true);
        sinField.setAccessible(true);
        double[] asinTab = (double[]) asinField.get(null);
        double[] cosTab = (double[]) cosField.get(null);
        float[] sinTab = (float[]) sinField.get(null);
        try (Writer w = new FileWriter(args[1])) {
            w.write("// Generated by tools/extractor from net.minecraft.util.Mth. Do not edit.\n");
            w.write("pub const ASIN_TAB: [u64; 257] = [\n");
            for (double d : asinTab) w.write("    0x" + Long.toHexString(Double.doubleToRawLongBits(d)) + ",\n");
            w.write("];\npub const COS_TAB: [u64; 257] = [\n");
            for (double d : cosTab) w.write("    0x" + Long.toHexString(Double.doubleToRawLongBits(d)) + ",\n");
            long sum = 0;
            for (int i = 0; i < sinTab.length; i++) sum = sum * 31 + Float.floatToRawIntBits(sinTab[i]);
            w.write("];\n/// Rolling hash (h * 31 + bits) over Mth.SIN, to check our table.\n");
            w.write("pub const SIN_TABLE_HASH: i64 = " + sum + ";\n");
        }
    }

    private static <T extends Comparable<T>> String valueName(BlockState state, Property<T> p) {
        return p.getName(state.getValue(p));
    }

    private static String shapeJson(VoxelShape shape, DiscreteVoxelShape discrete, boolean isBlock) {
        StringBuilder sb = new StringBuilder("{\"block\":" + isBlock + ",");
        for (Direction.Axis axis : Direction.Axis.values()) {
            DoubleList coords = shape.getCoords(axis);
            sb.append('"').append(axis.getName()).append("\":[");
            for (int i = 0; i < coords.size(); i++) {
                if (i > 0) sb.append(',');
                // Double.toString round-trips exactly.
                sb.append(Double.toString(coords.getDouble(i)));
            }
            sb.append("],");
        }
        // Filled cells, x-major then y then z, as a 0/1 string.
        sb.append("\"fill\":\"");
        for (int x = 0; x < discrete.getXSize(); x++)
            for (int y = 0; y < discrete.getYSize(); y++)
                for (int z = 0; z < discrete.getZSize(); z++)
                    sb.append(discrete.isFull(x, y, z) ? '1' : '0');
        sb.append("\"}");
        return sb.toString();
    }

    /**
     * Bits 0-3: isFaceSturdy for north, east, south, west (fluid flow uses
     * it). Bit 4: the block is an IceBlock. Bit 5: full-cube collision.
     * Bit 6: requiresCorrectToolForDrops. Bit 7: sturdy up. Bit 8: sturdy
     * down (placement support checks). Bit 9: isSolid. Bits 10-13:
     * SupportType.CENTER for north, east, south, west. Bit 14-15: center
     * support up/down.
     */
    private static int faceFlags(BlockState state) {
        int flags = 0;
        Direction[] horizontal = {Direction.NORTH, Direction.EAST, Direction.SOUTH, Direction.WEST};
        for (int i = 0; i < 4; i++) {
            if (state.isFaceSturdy(EmptyBlockGetter.INSTANCE, BlockPos.ZERO, horizontal[i])) flags |= 1 << i;
            if (state.isFaceSturdy(EmptyBlockGetter.INSTANCE, BlockPos.ZERO, horizontal[i], net.minecraft.world.level.block.SupportType.CENTER)) flags |= 1 << (10 + i);
        }
        if (state.getBlock() instanceof net.minecraft.world.level.block.IceBlock) flags |= 16;
        if (state.isCollisionShapeFullBlock(EmptyBlockGetter.INSTANCE, BlockPos.ZERO)) flags |= 32;
        if (state.requiresCorrectToolForDrops()) flags |= 64;
        if (state.isFaceSturdy(EmptyBlockGetter.INSTANCE, BlockPos.ZERO, Direction.UP)) flags |= 128;
        if (state.isFaceSturdy(EmptyBlockGetter.INSTANCE, BlockPos.ZERO, Direction.DOWN)) flags |= 256;
        if (state.isSolid()) flags |= 512;
        if (state.isFaceSturdy(EmptyBlockGetter.INSTANCE, BlockPos.ZERO, Direction.UP, net.minecraft.world.level.block.SupportType.CENTER)) flags |= 1 << 14;
        if (state.isFaceSturdy(EmptyBlockGetter.INSTANCE, BlockPos.ZERO, Direction.DOWN, net.minecraft.world.level.block.SupportType.CENTER)) flags |= 1 << 15;
        return flags;
    }

    /**
     * Whether the block class declares its own right-click behavior
     * (`useItemOn` or `useWithoutItem`). Blocks without an override return
     * `TRY_WITH_EMPTY_HAND`/`PASS`, so a click falls through to the held
     * item's `use` action; blocks with an override can consume the click.
     */
    private static boolean hasCustomUse(Block block) {
        for (Class<?> c = block.getClass(); c != null && c != BlockBehaviour.class; c = c.getSuperclass()) {
            for (Method m : c.getDeclaredMethods()) {
                String name = m.getName();
                if (name.equals("useItemOn") || name.equals("useWithoutItem")) return true;
            }
        }
        return false;
    }

    /**
     * The feature flags a set requires, in a stable order. `minecraft:vanilla`
     * is kept: nearly every block requires it, and a server that enables
     * nothing at all makes vanilla refuse those interactions.
     */
    private static String featureNames(FeatureFlagSet set) {
        java.util.TreeSet<String> names = new java.util.TreeSet<>();
        for (Identifier id : FeatureFlags.REGISTRY.toNames(set)) {
            names.add(id.toString());
        }
        StringBuilder sb = new StringBuilder("[");
        for (String name : names) {
            if (sb.length() > 1) sb.append(',');
            sb.append(quote(name));
        }
        return sb.append(']').toString();
    }

    /**
     * `BlockBehaviour.OffsetType`, as 0 for none, 1 for XZ and 2 for XYZ.
     *
     * The type is not readable (the properties turn it into a private
     * lambda), so it is probed: only XYZ ever moves a block vertically.
     */
    private static int offsetType(Block block) {
        BlockState state = block.defaultBlockState();
        if (!state.hasOffsetFunction()) return 0;
        for (int i = 0; i < 64; i++) {
            if (state.getOffset(new BlockPos(i * 7, 0, i * 13)).y != 0.0) return 2;
        }
        return 1;
    }

    /**
     * Whether this block's shapes are exactly `base.move(state.getOffset(pos))`,
     * which is how flowers, bamboo and dripstone are written: shifting the
     * shape taken at the origin by the change in the offset then gives the
     * shape anywhere else.
     *
     * The comparison allows a nanometre: shifting a shape that already has the
     * origin's offset baked in re-associates the addition, which moves the
     * last bits of a coordinate.
     */
    private static boolean shapeFollowsOffset(Block block) {
        if (!block.defaultBlockState().hasOffsetFunction()) return true;
        // Every state, not just the default. `PointedDripstoneBlock` builds
        // its shape from the tip state's offset rather than this state's, and
        // asking about each state is what decides whether that still reduces
        // to a per-position shift.
        for (BlockState state : block.getStateDefinition().getPossibleStates()) {
            BlockPos origin = BlockPos.ZERO;
            BlockPos other = new BlockPos(7, 0, 3);
            Vec3 atOrigin = state.getOffset(origin);
            Vec3 elsewhere = state.getOffset(other);
            if (!sameWhenShifted(
                    state.getCollisionShape(EmptyBlockGetter.INSTANCE, origin),
                    state.getCollisionShape(EmptyBlockGetter.INSTANCE, other),
                    atOrigin,
                    elsewhere)
                || !sameWhenShifted(
                    state.getShape(EmptyBlockGetter.INSTANCE, origin),
                    state.getShape(EmptyBlockGetter.INSTANCE, other),
                    atOrigin,
                    elsewhere)) {
                return false;
            }
        }
        return true;
    }

    private static boolean sameWhenShifted(VoxelShape at, VoxelShape other, Vec3 atOffset, Vec3 otherOffset) {
        if (at.isEmpty() || other.isEmpty()) return at.isEmpty() && other.isEmpty();
        VoxelShape shifted = at.move(
            otherOffset.x - atOffset.x, otherOffset.y - atOffset.y, otherOffset.z - atOffset.z);
        List<AABB> shiftedBoxes = shifted.toAabbs();
        List<AABB> otherBoxes = other.toAabbs();
        if (shiftedBoxes.size() != otherBoxes.size()) return false;
        for (int i = 0; i < shiftedBoxes.size(); i++) {
            AABB one = shiftedBoxes.get(i);
            AABB two = otherBoxes.get(i);
            if (Math.abs(one.minX - two.minX) > 1e-9 || Math.abs(one.minY - two.minY) > 1e-9
                || Math.abs(one.minZ - two.minZ) > 1e-9 || Math.abs(one.maxX - two.maxX) > 1e-9
                || Math.abs(one.maxY - two.maxY) > 1e-9 || Math.abs(one.maxZ - two.maxZ) > 1e-9) {
                return false;
            }
        }
        return true;
    }

    /** `getMaxHorizontalOffset`: protected, 0.25 unless the block says otherwise. */
    private static float maxHorizontalOffset(Block block) throws Exception {
        Method method = BlockBehaviour.class.getDeclaredMethod("getMaxHorizontalOffset");
        method.setAccessible(true);
        return (float) method.invoke(block);
    }

    /** `getMaxVerticalOffset`: protected, 0.2 unless the block says otherwise. */
    private static float maxVerticalOffset(Block block) throws Exception {
        Method method = BlockBehaviour.class.getDeclaredMethod("getMaxVerticalOffset");
        method.setAccessible(true);
        return (float) method.invoke(block);
    }

    /** Registry entry names in ID order. */
    private static <T> String names(net.minecraft.core.Registry<T> registry) {
        StringBuilder sb = new StringBuilder("[");
        for (int i = 0; i < registry.size(); i++) {
            if (i > 0) sb.append(',');
            sb.append(quote(registry.getKey(registry.byId(i)).toString()));
        }
        return sb.append("]").toString();
    }

    private static String quote(String s) {
        return "\"" + s.replace("\\", "\\\\").replace("\"", "\\\"") + "\"";
    }
}
