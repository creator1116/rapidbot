// Dumps block collision data from the vanilla server jar as JSON.
//
// Run through tools/extract_blocks.py, which compiles this against the jar.
// Shapes are exported in VoxelShape form (per-axis coordinate lists plus the
// filled cells of the discrete shape) so collision can be reproduced exactly,
// including step-up candidates, which read the coordinate lists directly.

import java.io.FileWriter;
import java.io.Writer;
import java.lang.reflect.Field;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

import it.unimi.dsi.fastutil.doubles.DoubleList;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;
import net.minecraft.world.level.material.FluidState;
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
                .append(interactionIndex)
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
     * Bit 6: requiresCorrectToolForDrops.
     */
    private static int faceFlags(BlockState state) {
        int flags = 0;
        Direction[] horizontal = {Direction.NORTH, Direction.EAST, Direction.SOUTH, Direction.WEST};
        for (int i = 0; i < 4; i++) {
            if (state.isFaceSturdy(EmptyBlockGetter.INSTANCE, BlockPos.ZERO, horizontal[i])) flags |= 1 << i;
        }
        if (state.getBlock() instanceof net.minecraft.world.level.block.IceBlock) flags |= 16;
        if (state.isCollisionShapeFullBlock(EmptyBlockGetter.INSTANCE, BlockPos.ZERO)) flags |= 32;
        if (state.requiresCorrectToolForDrops()) flags |= 64;
        return flags;
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
