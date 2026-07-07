//! `EasyWorld` — a native Pumpkin plugin and prerequisite library (`前置插件`)
//! for world management.
//!
//! It provides three things:
//!
//! 1. **A reusable service** ([`service::WorldService`]) that other plugins
//!    depend on: fetch it with
//!    `context.get_service::<WorldService>("easyworld").await`.
//! 2. **An `/ew` command tree** for operators to list, create, load, unload,
//!    clone, and delete worlds at runtime.
//! 3. **An in-game dialog UI** (`/ew menu`) whose buttons drive the same
//!    service, with responses handled via [`CustomClickActionEvent`].

// The `#[plugin_impl]` macro expands `on_load` into
// `GLOBAL_RUNTIME.block_on(async { Box::pin(async { .. }) })`, whose outer
// async block yields an (unawaited) future. That is intentional in the macro,
// but trips `clippy::async_yields_async`; we can't edit the macro, so allow it
// crate-wide.
#![allow(clippy::async_yields_async)]

use std::sync::Arc;

use pumpkin::plugin::api::events::player::custom_click_action::CustomClickActionEvent;
use pumpkin::plugin::{Context, EventPriority};
use pumpkin_api_macros::{plugin_impl, plugin_method};
use pumpkin_util::PermissionLvl;
use pumpkin_util::permission::{Permission, PermissionDefault};

use crate::dialog::WorldClickHandler;
use crate::service::WorldService;

pub mod commands;
pub mod dialog;
pub mod service;

/// Name the [`WorldService`] is registered under; also the plugin's permission
/// namespace.
const SERVICE_NAME: &str = "easyworld";

/// Permission guarding the `/ew` command tree.
const COMMAND_PERMISSION: &str = "easyworld:command.manage";

#[plugin_method]
async fn on_load(&mut self, context: Arc<Context>) -> Result<(), String> {
    context.init_log();

    // The single shared service that everything else (commands, dialog, and
    // other plugins) drives world operations through.
    let service = Arc::new(WorldService::new(context.server.clone()));

    // (a) Expose the service for dependent plugins.
    context
        .register_service(SERVICE_NAME, service.clone())
        .await;

    // Permission node must live in this plugin's namespace.
    let permission = Permission::new(
        COMMAND_PERMISSION,
        "Manage worlds through EasyWorld.",
        PermissionDefault::Op(PermissionLvl::Three),
    );
    if let Err(e) = context.register_permission(permission).await {
        context.log(format!("EasyWorld: permission not registered: {e}"));
    }

    // (b) Register the `/ew` command tree.
    let tree = commands::build_command_tree(&service);
    context.register_command(tree, COMMAND_PERMISSION).await;

    // (c) React to dialog button clicks. The handler owns the last remaining
    // handle to `service`, so no redundant clone is needed here.
    context
        .register_event::<CustomClickActionEvent, WorldClickHandler>(
            Arc::new(WorldClickHandler::new(service)),
            EventPriority::Normal,
            false,
        )
        .await;

    context.log("EasyWorld loaded: /ew command and WorldService are ready.");
    Ok(())
}

#[plugin_impl]
pub struct EasyWorld;

impl EasyWorld {
    /// Creates a new plugin instance. Called by the generated `plugin()`
    /// factory.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for EasyWorld {
    fn default() -> Self {
        Self::new()
    }
}
