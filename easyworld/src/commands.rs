//! The `/ew` command tree.
//!
//! Every executor mirrors the built-in `/world` command style: it drives the
//! shared [`WorldService`] and reports back with colored feedback. Executors
//! hold an `Arc<WorldService>` captured at registration time, which is how they
//! obtain an owned `Arc<Server>` (the world primitives require `&Arc<Server>`).

use std::collections::HashSet;
use std::sync::Arc;

use pumpkin::command::args::simple::SimpleArgConsumer;
use pumpkin::command::args::{ConsumedArgs, FindArg};
use pumpkin::command::tree::CommandTree;
use pumpkin::command::tree::builder::{argument, literal};
use pumpkin::command::{CommandExecutor, CommandResult, CommandSender};
use pumpkin::server::Server;
use pumpkin_util::text::{TextComponent, color::NamedColor};

use crate::dialog::build_menu_dialog;
use crate::service::WorldService;

const DESCRIPTION: &str = "EasyWorld: manage worlds at runtime.";
const ARG_NAME: &str = "name";
const ARG_SRC: &str = "source";
const ARG_DST: &str = "destination";

fn err_text(msg: impl Into<String>) -> TextComponent {
    TextComponent::text(msg.into()).color_named(NamedColor::Red)
}

fn ok_text(msg: impl Into<String>) -> TextComponent {
    TextComponent::text(msg.into()).color_named(NamedColor::Green)
}

/// `/ew list` — loaded worlds (name + players) and on-disk worlds.
struct ListExecutor {
    service: Arc<WorldService>,
}

impl CommandExecutor for ListExecutor {
    fn execute<'a>(
        &'a self,
        sender: &'a CommandSender,
        _server: &'a Server,
        _args: &'a ConsumedArgs<'a>,
    ) -> CommandResult<'a> {
        Box::pin(async move {
            let loaded = self.service.list_loaded();
            let on_disk = self.service.list_on_disk();
            let loaded_names: HashSet<&str> =
                loaded.iter().map(|(name, _)| name.as_str()).collect();

            let mut lines = vec![format!("Loaded worlds ({}):", loaded.len())];
            for (name, players) in &loaded {
                lines.push(format!("  {name} - {players} player(s)"));
            }

            let unloaded: Vec<&String> = on_disk
                .iter()
                .filter(|name| !loaded_names.contains(name.as_str()))
                .collect();
            lines.push(format!("On disk, not loaded ({}):", unloaded.len()));
            for name in unloaded {
                lines.push(format!("  {name}"));
            }

            sender
                .send_message(TextComponent::text(lines.join("\n")))
                .await;
            Ok(1)
        })
    }
}

/// `/ew create <name>` and `/ew load <name>` — both load-or-create.
struct LoadExecutor {
    service: Arc<WorldService>,
}

impl CommandExecutor for LoadExecutor {
    fn execute<'a>(
        &'a self,
        sender: &'a CommandSender,
        _server: &'a Server,
        args: &'a ConsumedArgs<'a>,
    ) -> CommandResult<'a> {
        Box::pin(async move {
            let name = SimpleArgConsumer::find_arg(args, ARG_NAME)?.to_string();

            if self.service.find(&name).is_some() {
                sender
                    .send_message(err_text(format!("World '{name}' is already loaded.")))
                    .await;
                return Ok(0);
            }
            if self.service.is_unloading(&name) {
                sender
                    .send_message(err_text(format!(
                        "World '{name}' is still unloading, retry shortly."
                    )))
                    .await;
                return Ok(0);
            }

            let world = self.service.create_or_load(name).await;
            sender
                .send_message(ok_text(format!(
                    "World '{}' loaded.",
                    world.get_world_name()
                )))
                .await;
            Ok(1)
        })
    }
}

/// `/ew unload <name>`.
struct UnloadExecutor {
    service: Arc<WorldService>,
}

impl CommandExecutor for UnloadExecutor {
    fn execute<'a>(
        &'a self,
        sender: &'a CommandSender,
        _server: &'a Server,
        args: &'a ConsumedArgs<'a>,
    ) -> CommandResult<'a> {
        Box::pin(async move {
            let name = SimpleArgConsumer::find_arg(args, ARG_NAME)?.to_string();
            match self.service.unload(&name).await {
                Ok(()) => {
                    sender
                        .send_message(ok_text(format!("World '{name}' saved and unloaded.")))
                        .await;
                    Ok(1)
                }
                Err(e) => {
                    sender
                        .send_message(err_text(format!("Cannot unload '{name}': {e}")))
                        .await;
                    Ok(0)
                }
            }
        })
    }
}

/// `/ew clone <src> <dst>` and `/ew clone-ro <src> <dst>`.
struct CloneExecutor {
    service: Arc<WorldService>,
    readonly: bool,
}

impl CommandExecutor for CloneExecutor {
    fn execute<'a>(
        &'a self,
        sender: &'a CommandSender,
        _server: &'a Server,
        args: &'a ConsumedArgs<'a>,
    ) -> CommandResult<'a> {
        Box::pin(async move {
            let src = SimpleArgConsumer::find_arg(args, ARG_SRC)?.to_string();
            let dst = SimpleArgConsumer::find_arg(args, ARG_DST)?.to_string();

            let result = if self.readonly {
                self.service.clone_world_readonly(&src, &dst).await
            } else {
                self.service.clone_world(&src, &dst).await
            };

            match result {
                Ok(world) => {
                    let kind = if self.readonly {
                        "read-only clone"
                    } else {
                        "clone"
                    };
                    sender
                        .send_message(ok_text(format!(
                            "World '{src}' {kind}d to '{}' and loaded.",
                            world.get_world_name()
                        )))
                        .await;
                    Ok(1)
                }
                Err(e) => {
                    sender
                        .send_message(err_text(format!("Cannot clone '{src}': {e}")))
                        .await;
                    Ok(0)
                }
            }
        })
    }
}

/// `/ew delete <name>`.
struct DeleteExecutor {
    service: Arc<WorldService>,
}

impl CommandExecutor for DeleteExecutor {
    fn execute<'a>(
        &'a self,
        sender: &'a CommandSender,
        _server: &'a Server,
        args: &'a ConsumedArgs<'a>,
    ) -> CommandResult<'a> {
        Box::pin(async move {
            let name = SimpleArgConsumer::find_arg(args, ARG_NAME)?.to_string();
            match self.service.delete(&name).await {
                Ok(()) => {
                    sender
                        .send_message(ok_text(format!("World '{name}' deleted.")))
                        .await;
                    Ok(1)
                }
                Err(e) => {
                    sender
                        .send_message(err_text(format!("Cannot delete '{name}': {e}")))
                        .await;
                    Ok(0)
                }
            }
        })
    }
}

/// `/ew menu` — opens the in-game dialog UI (players only).
struct MenuExecutor {
    service: Arc<WorldService>,
}

impl CommandExecutor for MenuExecutor {
    fn execute<'a>(
        &'a self,
        sender: &'a CommandSender,
        _server: &'a Server,
        _args: &'a ConsumedArgs<'a>,
    ) -> CommandResult<'a> {
        Box::pin(async move {
            let Some(player) = sender.as_player() else {
                sender
                    .send_message(err_text("Only players can open the EasyWorld menu."))
                    .await;
                return Ok(0);
            };
            let dialog = build_menu_dialog(&self.service);
            player.show_dialog(&dialog).await;
            Ok(1)
        })
    }
}

/// Builds the full `/ew` command tree. Each executor captures its own handle to
/// the shared [`WorldService`].
#[must_use]
pub fn build_command_tree(service: &Arc<WorldService>) -> CommandTree {
    CommandTree::new(["ew"], DESCRIPTION)
        .then(literal("list").execute(ListExecutor {
            service: service.clone(),
        }))
        .then(
            literal("create").then(argument(ARG_NAME, SimpleArgConsumer).execute(LoadExecutor {
                service: service.clone(),
            })),
        )
        .then(
            literal("load").then(argument(ARG_NAME, SimpleArgConsumer).execute(LoadExecutor {
                service: service.clone(),
            })),
        )
        .then(
            literal("unload").then(
                argument(ARG_NAME, SimpleArgConsumer).execute(UnloadExecutor {
                    service: service.clone(),
                }),
            ),
        )
        .then(
            literal("clone").then(argument(ARG_SRC, SimpleArgConsumer).then(
                argument(ARG_DST, SimpleArgConsumer).execute(CloneExecutor {
                    service: service.clone(),
                    readonly: false,
                }),
            )),
        )
        .then(
            literal("clone-ro").then(argument(ARG_SRC, SimpleArgConsumer).then(
                argument(ARG_DST, SimpleArgConsumer).execute(CloneExecutor {
                    service: service.clone(),
                    readonly: true,
                }),
            )),
        )
        .then(
            literal("delete").then(
                argument(ARG_NAME, SimpleArgConsumer).execute(DeleteExecutor {
                    service: service.clone(),
                }),
            ),
        )
        .then(literal("menu").execute(MenuExecutor {
            service: service.clone(),
        }))
}
