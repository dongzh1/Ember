// EMBER - built-in bank system command
//
//   /bank balance [currency]              - your bank balance (interest settles first)
//   /bank deposit <amount> [currency]     - move money from wallet to bank
//   /bank withdraw <amount> [currency]    - move money from bank to wallet
//   /bank log [currency]                  - your last 10 bank transactions

use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};

use crate::command::argument_builder::{ArgumentBuilder, argument, command, literal};
use crate::command::argument_types::core::integer::IntegerArgumentType;
use crate::command::argument_types::core::string::StringArgumentType;
use crate::command::context::command_context::CommandContext;
use crate::command::errors::command_syntax_error::CommandSyntaxError;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::server::Server;
use crate::server::shop::ShopError;

const DESCRIPTION: &str = "Deposit/withdraw money from your bank account.";
const PERMISSION: &str = "ember:command.bank";
const ARG_AMOUNT: &str = "amount";
const ARG_CURRENCY: &str = "currency";

async fn feedback(context: &CommandContext<'_>, msg: TextComponent) {
    context.source.send_feedback(msg, false).await;
}

fn err_text(msg: impl Into<String>) -> TextComponent {
    TextComponent::text(msg.into()).color_named(NamedColor::Red)
}

fn shop_err_text(e: &ShopError) -> TextComponent {
    err_text(e.to_string())
}

fn optional_currency<'a>(
    context: &'a CommandContext,
    has_currency: bool,
) -> Result<Option<&'a str>, CommandSyntaxError> {
    has_currency
        .then(|| StringArgumentType::get(context, ARG_CURRENCY))
        .transpose()
}

fn resolve_currency<'a>(server: &'a Server, currency: Option<&'a str>) -> String {
    currency
        .unwrap_or_else(|| server.economy_manager.default_currency())
        .to_string()
}

/// Checks every configured bank tier's `permission` against `player`, then
/// resolves to the tier with the highest `max_balance` among the ones they
/// qualify for. Falls back to whatever the no-permission-required tiers
/// allow if `player` isn't currently online to check permissions against
/// (shouldn't happen for the command's own caller, but this is also reused
/// wherever only a `Uuid` is on hand).
async fn resolve_tier(player_uuid: uuid::Uuid, server: &Server) -> pumpkin_config::BankTier {
    let mut held = std::collections::HashSet::new();
    if let Some(player) = server.get_player_by_uuid(player_uuid) {
        for tier in server.bank_manager.tiers() {
            if let Some(node) = &tier.permission
                && player.has_permission(server, node).await
            {
                held.insert(node.clone());
            }
        }
    }
    server
        .bank_manager
        .resolve_tier(&|node: &str| held.contains(node))
}

struct BankBalanceExecutor {
    has_currency: bool,
}
impl CommandExecutor for BankBalanceExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let player = context.source.player_or_err()?;
            let server = context.server();
            let currency = optional_currency(context, self.has_currency)?;
            let currency = resolve_currency(server, currency);
            let tier = resolve_tier(player.gameprofile.id, server).await;

            match server
                .bank_manager
                .settle_and_get(player.gameprofile.id, &currency, &tier)
                .await
            {
                Ok(account) => {
                    feedback(
                        context,
                        TextComponent::text(format!(
                            "Bank balance: {} {currency} (cap {}, {}%/day interest)",
                            account.balance,
                            tier.max_balance,
                            tier.daily_rate * 100.0
                        ))
                        .color_named(NamedColor::Green),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, shop_err_text(&e)).await;
                    Ok(0)
                }
            }
        })
    }
}

struct BankDepositExecutor {
    has_currency: bool,
}
impl CommandExecutor for BankDepositExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let player = context.source.player_or_err()?;
            let server = context.server();
            let amount = IntegerArgumentType::get(context, ARG_AMOUNT)?;
            let currency = optional_currency(context, self.has_currency)?;
            let currency = resolve_currency(server, currency);
            let tier = resolve_tier(player.gameprofile.id, server).await;

            if amount <= 0 {
                feedback(context, err_text("Amount must be positive.")).await;
                return Ok(0);
            }

            match server
                .bank_manager
                .deposit(
                    player.gameprofile.id,
                    &currency,
                    i64::from(amount),
                    &tier,
                    &server.economy_manager,
                )
                .await
            {
                Ok(new_balance) => {
                    feedback(
                        context,
                        TextComponent::text(format!(
                            "Deposited {amount} {currency}. New bank balance: {new_balance}."
                        ))
                        .color_named(NamedColor::Green),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, shop_err_text(&e)).await;
                    Ok(0)
                }
            }
        })
    }
}

struct BankWithdrawExecutor {
    has_currency: bool,
}
impl CommandExecutor for BankWithdrawExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let player = context.source.player_or_err()?;
            let server = context.server();
            let amount = IntegerArgumentType::get(context, ARG_AMOUNT)?;
            let currency = optional_currency(context, self.has_currency)?;
            let currency = resolve_currency(server, currency);
            let tier = resolve_tier(player.gameprofile.id, server).await;

            if amount <= 0 {
                feedback(context, err_text("Amount must be positive.")).await;
                return Ok(0);
            }

            match server
                .bank_manager
                .withdraw(
                    player.gameprofile.id,
                    &currency,
                    i64::from(amount),
                    &tier,
                    &server.economy_manager,
                )
                .await
            {
                Ok(new_balance) => {
                    feedback(
                        context,
                        TextComponent::text(format!(
                            "Withdrew {amount} {currency}. New bank balance: {new_balance}."
                        ))
                        .color_named(NamedColor::Green),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, shop_err_text(&e)).await;
                    Ok(0)
                }
            }
        })
    }
}

struct BankLogExecutor {
    has_currency: bool,
}
impl CommandExecutor for BankLogExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let player = context.source.player_or_err()?;
            let server = context.server();
            let currency = optional_currency(context, self.has_currency)?;
            let currency = resolve_currency(server, currency);

            match server
                .bank_manager
                .recent_transactions(player.gameprofile.id, &currency)
                .await
            {
                Ok(transactions) if transactions.is_empty() => {
                    feedback(context, TextComponent::text("No transactions yet.")).await;
                    Ok(0)
                }
                Ok(transactions) => {
                    let mut lines = vec!["Recent bank transactions:".to_string()];
                    for t in transactions {
                        let label = if t.is_interest {
                            "interest".to_string()
                        } else if t.amount >= 0 {
                            "deposit".to_string()
                        } else {
                            "withdraw".to_string()
                        };
                        lines.push(format!("  {label}: {:+} {currency}", t.amount));
                    }
                    feedback(context, TextComponent::text(lines.join("\n"))).await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, shop_err_text(&e)).await;
                    Ok(0)
                }
            }
        })
    }
}

pub fn register(dispatcher: &mut CommandDispatcher, registry: &mut PermissionRegistry) {
    registry.register_permission_or_panic(Permission::new(
        PERMISSION,
        DESCRIPTION,
        PermissionDefault::Allow,
    ));

    dispatcher.register(
        command("bank", DESCRIPTION)
            .requires(PERMISSION)
            .then(
                literal("balance")
                    .executes(BankBalanceExecutor {
                        has_currency: false,
                    })
                    .then(
                        argument(ARG_CURRENCY, StringArgumentType::SingleWord)
                            .executes(BankBalanceExecutor { has_currency: true }),
                    ),
            )
            .then(
                literal("deposit").then(
                    argument(ARG_AMOUNT, IntegerArgumentType::with_min(1))
                        .executes(BankDepositExecutor {
                            has_currency: false,
                        })
                        .then(
                            argument(ARG_CURRENCY, StringArgumentType::SingleWord)
                                .executes(BankDepositExecutor { has_currency: true }),
                        ),
                ),
            )
            .then(
                literal("withdraw").then(
                    argument(ARG_AMOUNT, IntegerArgumentType::with_min(1))
                        .executes(BankWithdrawExecutor {
                            has_currency: false,
                        })
                        .then(
                            argument(ARG_CURRENCY, StringArgumentType::SingleWord)
                                .executes(BankWithdrawExecutor { has_currency: true }),
                        ),
                ),
            )
            .then(
                literal("log")
                    .executes(BankLogExecutor {
                        has_currency: false,
                    })
                    .then(
                        argument(ARG_CURRENCY, StringArgumentType::SingleWord)
                            .executes(BankLogExecutor { has_currency: true }),
                    ),
            ),
    );
}
// EMBER end
