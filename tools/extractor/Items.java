import io.netty.buffer.ByteBuf;
import io.netty.buffer.Unpooled;
import java.io.Writer;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HexFormat;
import java.util.List;
import net.minecraft.SharedConstants;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.LayeredRegistryAccess;
import net.minecraft.core.Registry;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.component.DataComponentMap;
import net.minecraft.core.component.TypedDataComponent;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.nbt.NbtOps;
import net.minecraft.nbt.Tag;
import net.minecraft.nbt.TagParser;
import net.minecraft.network.RegistryFriendlyByteBuf;
import net.minecraft.network.VarInt;
import net.minecraft.resources.RegistryDataLoader;
import net.minecraft.resources.RegistryOps;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.RegistryLayer;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.repository.PackRepository;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.packs.resources.MultiPackResourceManager;
import net.minecraft.tags.TagLoader;
import net.minecraft.world.item.Item;
import net.minecraft.world.item.ItemStack;

/**
 * Writes items.json (data component type names and every item's default
 * components, encoded with vanilla's network codecs) and item_vectors.txt
 * (item stacks built from SNBT, encoded with ItemStack.OPTIONAL_STREAM_CODEC)
 * for the Rust decoder's tests.
 */
public class Items {
    /** Stacks exercising the components that items do not have by default. */
    private static final String[] STACKS = {
        "{id:'minecraft:stone',count:64}",
        "{id:'minecraft:diamond_sword',count:1,components:{'minecraft:damage':5,'minecraft:enchantments':{'minecraft:sharpness':5,'minecraft:unbreaking':3},'minecraft:custom_name':{text:'Blade',color:'red',italic:false},'minecraft:lore':['one',{text:'two',bold:true}],'minecraft:repair_cost':3,'minecraft:unbreakable':{}}}",
        "{id:'minecraft:elytra',count:1,components:{'minecraft:damage':431}}",
        "{id:'minecraft:elytra',count:1,components:{'!minecraft:glider':{}}}",
        "{id:'minecraft:leather_chestplate',count:1,components:{'minecraft:glider':{},'minecraft:dyed_color':16711680}}",
        "{id:'minecraft:potion',count:1,components:{'minecraft:potion_contents':{potion:'minecraft:swiftness',custom_color:255,custom_effects:[{id:'minecraft:speed',amplifier:1,duration:100,hidden_effect:{id:'minecraft:speed',amplifier:0,duration:400}}],custom_name:'x'}}}",
        "{id:'minecraft:written_book',count:1,components:{'minecraft:written_book_content':{title:'T',author:'A',generation:1,pages:['p1',{raw:'p2',filtered:'f2'}],resolved:true}}}",
        "{id:'minecraft:writable_book',count:1,components:{'minecraft:writable_book_content':{pages:['a',{raw:'b',filtered:'c'}]}}}",
        "{id:'minecraft:firework_rocket',count:3,components:{'minecraft:fireworks':{flight_duration:2,explosions:[{shape:'star',colors:[I;255,65280],fade_colors:[I;1],has_trail:true,has_twinkle:false}]}}}",
        "{id:'minecraft:firework_star',count:1,components:{'minecraft:firework_explosion':{shape:'burst',colors:[I;7]}}}",
        "{id:'minecraft:player_head',count:1,components:{'minecraft:profile':{name:'Steve'}}}",
        "{id:'minecraft:player_head',count:1,components:{'minecraft:profile':{name:'Alex',id:[I;1,2,3,4],properties:[{name:'textures',value:'abc',signature:'sig'},{name:'other',value:'v'}]}}}",
        "{id:'minecraft:shulker_box',count:1,components:{'minecraft:container':[{slot:0,item:{id:'minecraft:stone',count:3}},{slot:2,item:{id:'minecraft:diamond_pickaxe',count:1,components:{'minecraft:damage':7}}}]}}",
        "{id:'minecraft:bundle',count:1,components:{'minecraft:bundle_contents':[{id:'minecraft:arrow',count:12},{id:'minecraft:stick',count:1}]}}",
        "{id:'minecraft:crossbow',count:1,components:{'minecraft:charged_projectiles':[{id:'minecraft:arrow',count:1}]}}",
        "{id:'minecraft:diamond_chestplate',count:1,components:{'minecraft:trim':{material:'minecraft:gold',pattern:'minecraft:coast'}}}",
        "{id:'minecraft:white_banner',count:1,components:{'minecraft:banner_patterns':[{pattern:'minecraft:creeper',color:'red'}],'minecraft:base_color':'blue'}}",
        "{id:'minecraft:stick',count:1,components:{'minecraft:custom_data':{a:1b,b:'s',c:[1,2,3],d:{e:1.5d}},'minecraft:custom_model_data':{floats:[1.0f,2.0f],flags:[true],strings:['s'],colors:[255]},'minecraft:tooltip_display':{hide_tooltip:false,hidden_components:['minecraft:enchantments']},'minecraft:item_model':'minecraft:stone','minecraft:rarity':'epic','minecraft:enchantment_glint_override':true,'minecraft:max_stack_size':16,'minecraft:tooltip_style':'minecraft:x','minecraft:intangible_projectile':{}}}",
        "{id:'minecraft:iron_sword',count:1,components:{'minecraft:attribute_modifiers':[{type:'minecraft:attack_damage',id:'minecraft:x',amount:5.0d,operation:'add_value',slot:'mainhand'},{type:'minecraft:movement_speed',id:'minecraft:y',amount:0.1d,operation:'add_multiplied_base',slot:'any',display:{type:'override',value:'fast'}},{type:'minecraft:armor',id:'minecraft:z',amount:1.0d,operation:'add_value',display:{type:'hidden'}}]}}",
        "{id:'minecraft:diamond_pickaxe',count:1,components:{'minecraft:can_break':[{blocks:'#minecraft:logs'},{blocks:['minecraft:stone','minecraft:dirt'],state:{facing:'north',age:{min:'1',max:'3'}},nbt:{a:1}}],'minecraft:can_place_on':{blocks:'minecraft:stone'}}}",
        "{id:'minecraft:chest',count:1,components:{'minecraft:block_entity_data':{id:'minecraft:chest',Lock:'k'},'minecraft:lock':{components:{'minecraft:custom_name':'key'}},'minecraft:container_loot':{loot_table:'minecraft:chests/simple_dungeon',seed:5L},'minecraft:block_state':{facing:'north',waterlogged:'true'}}}",
        "{id:'minecraft:armor_stand',count:1,components:{'minecraft:entity_data':{id:'minecraft:armor_stand',Small:1b}}}",
        "{id:'minecraft:cod_bucket',count:1,components:{'minecraft:bucket_entity_data':{Health:3.0f}}}",
        "{id:'minecraft:bee_nest',count:1,components:{'minecraft:bees':[{entity_data:{id:'minecraft:bee'},ticks_in_hive:5,min_ticks_in_hive:10}]}}",
        "{id:'minecraft:compass',count:1,components:{'minecraft:lodestone_tracker':{target:{dimension:'minecraft:overworld',pos:[I;1,64,-3]},tracked:true}}}",
        "{id:'minecraft:compass',count:1,components:{'minecraft:lodestone_tracker':{tracked:false}}}",
        "{id:'minecraft:filled_map',count:1,components:{'minecraft:map_id':7,'minecraft:map_decorations':{a:{type:'minecraft:player',x:1.0d,z:2.0d,rotation:0.0f}}}}",
        "{id:'minecraft:enchanted_book',count:1,components:{'minecraft:stored_enchantments':{'minecraft:mending':1}}}",
        "{id:'minecraft:suspicious_stew',count:1,components:{'minecraft:suspicious_stew_effects':[{id:'minecraft:blindness',duration:100}]}}",
        "{id:'minecraft:goat_horn',count:1,components:{'minecraft:instrument':'minecraft:ponder_goat_horn'}}",
        "{id:'minecraft:decorated_pot',count:1,components:{'minecraft:pot_decorations':{back:{id:'minecraft:brick'},front:{id:'minecraft:angler_pottery_sherd'}}}}",
        "{id:'minecraft:knowledge_book',count:1,components:{'minecraft:recipes':['minecraft:stick']}}",
        "{id:'minecraft:debug_stick',count:1,components:{'minecraft:debug_stick_state':{'minecraft:oak_stairs':'facing'}}}",
        "{id:'minecraft:apple',count:1,components:{'minecraft:food':{nutrition:4,saturation:2.5f,can_always_eat:true},'minecraft:consumable':{consume_seconds:1.0f,animation:'drink',sound:{sound_id:'minecraft:custom',range:8.0f},has_consume_particles:false,on_consume_effects:[{type:'minecraft:apply_effects',effects:[{id:'minecraft:regeneration',duration:20}],probability:0.5f},{type:'minecraft:remove_effects',effects:'minecraft:poison'},{type:'minecraft:clear_all_effects'},{type:'minecraft:teleport_randomly',diameter:8.0f},{type:'minecraft:play_sound',sound:'minecraft:entity.generic.eat'}]},'minecraft:use_remainder':{id:'minecraft:bowl',count:1},'minecraft:use_cooldown':{seconds:1.5f,cooldown_group:'minecraft:g'}}}",
        "{id:'minecraft:totem_of_undying',count:1,components:{'minecraft:death_protection':{death_effects:[{type:'minecraft:clear_all_effects'}]}}}",
        "{id:'minecraft:carved_pumpkin',count:1,components:{'minecraft:equippable':{slot:'head',equip_sound:'minecraft:item.armor.equip_generic',asset_id:'minecraft:x',camera_overlay:'minecraft:misc/pumpkinblur',allowed_entities:['minecraft:player','minecraft:zombie'],dispensable:true,swappable:false,damage_on_hurt:true,equip_on_interact:true,can_be_sheared:true}}}",
        "{id:'minecraft:shield',count:1,components:{'minecraft:blocks_attacks':{block_delay_seconds:0.25f,disable_cooldown_scale:1.0f,damage_reductions:[{horizontal_blocking_angle:90.0f,type:'#minecraft:is_fire',base:0.0f,factor:1.0f}],item_damage:{threshold:3.0f,base:1.0f,factor:1.0f},bypassed_by:'#minecraft:bypasses_shield',block_sound:'minecraft:item.shield.block',disabled_sound:'minecraft:item.shield.break'}}}",
        "{id:'minecraft:iron_pickaxe',count:1,components:{'minecraft:tool':{rules:[{blocks:'#minecraft:mineable/pickaxe',speed:6.0f,correct_for_drops:true},{blocks:'minecraft:cobweb',speed:15.0f},{blocks:['minecraft:stone','minecraft:dirt'],correct_for_drops:false}],default_mining_speed:1.5f,damage_per_block:2,can_destroy_blocks_in_creative:false},'minecraft:weapon':{item_damage_per_attack:2,disable_blocking_for_seconds:5.0f},'minecraft:enchantable':{value:14},'minecraft:repairable':{items:'#minecraft:iron_tool_materials'},'minecraft:damage_resistant':{types:'#minecraft:is_fire'},'minecraft:max_damage':99}}",
        "{id:'minecraft:painting',count:1,components:{'minecraft:painting/variant':'minecraft:kebab'}}",
        "{id:'minecraft:music_disc_cat',count:1,components:{'minecraft:jukebox_playable':'minecraft:cat','minecraft:note_block_sound':'minecraft:x.y','minecraft:ominous_bottle_amplifier':3}}",
        "{id:'minecraft:oak_sign',count:1,components:{'minecraft:custom_name':'plain'}}",
    };

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        PackRepository repo = ServerPacksSource.createVanillaTrustedRepository();
        repo.reload();
        repo.setSelected(List.of("vanilla"));
        MultiPackResourceManager resources = new MultiPackResourceManager(PackType.SERVER_DATA, repo.openAllSelected());
        LayeredRegistryAccess<RegistryLayer> layers = RegistryLayer.createRegistryAccess();
        List<Registry.PendingTags<?>> staticTags = TagLoader.loadTagsForExistingRegistries(resources, layers.getLayer(RegistryLayer.STATIC));
        List<HolderLookup.RegistryLookup<?>> context = TagLoader.buildUpdatedLookups(layers.getAccessForLoading(RegistryLayer.WORLD), staticTags);
        RegistryAccess.Frozen world = RegistryDataLoader.load(resources, context, RegistryDataLoader.WORLD_REGISTRIES, Runnable::run).join();
        RegistryAccess.Frozen access = layers.replaceFrom(RegistryLayer.WORLD, world).compositeAccess();
        staticTags.forEach(Registry.PendingTags::apply);
        BuiltInRegistries.DATA_COMPONENT_INITIALIZERS.build(access).forEach(pending -> pending.apply());

        HexFormat hex = HexFormat.of();
        StringBuilder json = new StringBuilder("{\"component_types\":[");
        Registry<?> types = BuiltInRegistries.DATA_COMPONENT_TYPE;
        for (int i = 0; i < types.size(); i++) {
            if (i > 0) json.append(',');
            json.append('"').append(BuiltInRegistries.DATA_COMPONENT_TYPE.getKey(BuiltInRegistries.DATA_COMPONENT_TYPE.byId(i))).append('"');
        }
        json.append("],\"items\":[");
        for (int i = 0; i < BuiltInRegistries.ITEM.size(); i++) {
            Item item = BuiltInRegistries.ITEM.byId(i);
            DataComponentMap components = item.components();
            RegistryFriendlyByteBuf buf = new RegistryFriendlyByteBuf(Unpooled.buffer(), access);
            VarInt.write(buf, components.size());
            for (TypedDataComponent<?> component : components) {
                TypedDataComponent.STREAM_CODEC.encode(buf, component);
            }
            if (i > 0) json.append(',');
            json.append("{\"name\":\"").append(BuiltInRegistries.ITEM.getKey(item)).append("\",\"components\":\"").append(hex.formatHex(bytes(buf))).append("\"}");
        }
        json.append("]}");
        Files.writeString(Path.of(args[0]), json.toString(), StandardCharsets.UTF_8);

        boolean failed = false;
        RegistryOps<Tag> ops = access.createSerializationContext(NbtOps.INSTANCE);
        try (Writer w = Files.newBufferedWriter(Path.of(args[1]), StandardCharsets.UTF_8)) {
            w.write("# Generated by tools/extract_items.py: <snbt>\\t<ItemStack.OPTIONAL_STREAM_CODEC bytes>\n");
            for (String snbt : STACKS) {
                var parsed = ItemStack.CODEC.parse(ops, TagParser.parseCompoundFully(snbt));
                if (parsed.isError()) {
                    System.out.println("BAD STACK " + snbt.substring(0, 60) + ": " + parsed.error().get().message());
                    failed = true;
                    continue;
                }
                ItemStack stack = parsed.getOrThrow();
                RegistryFriendlyByteBuf buf = new RegistryFriendlyByteBuf(Unpooled.buffer(), access);
                ItemStack.OPTIONAL_STREAM_CODEC.encode(buf, stack);
                w.write(snbt + "\t" + hex.formatHex(bytes(buf)) + "\n");
            }
        }
        if (failed) throw new IllegalStateException("some test stacks did not parse");
        System.out.println("items: " + BuiltInRegistries.ITEM.size() + ", component types: " + types.size());
    }

    private static byte[] bytes(ByteBuf buf) {
        byte[] out = new byte[buf.readableBytes()];
        buf.getBytes(buf.readerIndex(), out);
        return out;
    }
}
