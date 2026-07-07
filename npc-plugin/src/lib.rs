//! NPC plugin for Ember.
//!
//! Spawns native `mannequin` entities as display NPCs.
//!
//! Commands:
//! - `/npc create <name>` — spawn a mannequin wearing the sender's own skin,
//!   named `<name>`, standing still and facing the sender.

use pumpkin_plugin_api::{
    Context, EntityType, Plugin, PluginMetadata, Result, Server,
    command::{Command, CommandError, CommandNode, CommandSender, ConsumedArgs},
    command_wit::{Arg, ArgumentType, StringType},
    commands::CommandHandler,
    register_plugin,
    text::TextComponent,
};

struct NpcPlugin;

/// Extracts a string-typed command argument by key.
fn string_arg(args: &ConsumedArgs, key: &str) -> Option<String> {
    match args.get_value(key) {
        Arg::Simple(value) => Some(value),
        _ => None,
    }
}

struct NpcCreate;

impl CommandHandler for NpcCreate {
    fn handle(
        &self,
        sender: CommandSender,
        _server: Server,
        args: ConsumedArgs,
    ) -> core::result::Result<i32, CommandError> {
        let Some(player) = sender.as_player() else {
            sender.send_message(TextComponent::text("Only players can use /npc create"));
            return Ok(0);
        };

        let name = string_arg(&args, "name").unwrap_or_else(|| "NPC".to_string());
        let pos = player.get_position();
        let yaw = player.get_yaw();
        let world = player.get_world();

        let npc = world.spawn_entity(EntityType::Mannequin, pos);
        npc.set_custom_name(TextComponent::text(&name));
        npc.set_custom_name_visible(true);
        npc.set_immovable(true);
        // Face the sender (opposite of the sender's look direction).
        npc.set_rotation(yaw + 180.0, 0.0);

        // Wear the sender's own skin.
        if let Some(skin) = player.get_skin() {
            npc.set_skin(&skin.value, skin.signature.as_deref());
        }

        sender.send_message(TextComponent::text(&format!("Spawned NPC '{name}'.")));
        Ok(0)
    }
}

impl Plugin for NpcPlugin {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "npc-plugin".into(),
            version: "0.1.0".into(),
            authors: vec!["dong".into()],
            description: "Display NPCs using native mannequin entities".into(),
            dependencies: vec![],
            permissions: vec![],
        }
    }

    fn on_load(&mut self, context: Context) -> Result<()> {
        // /npc create <name>
        let name_node = CommandNode::argument("name", &ArgumentType::String(StringType::Greedy))
            .execute(NpcCreate);
        let create_node = CommandNode::literal("create");
        create_node.then(name_node);

        let cmd = Command::new(&["npc".to_string()], "Manage display NPCs");
        cmd.then(create_node);
        context.register_command(cmd, "npc-plugin:command.npc");
        Ok(())
    }
}

register_plugin!(NpcPlugin);
