// EMBER - built-in economy system commands
//
//   /balance                        - your own balance in every currency
//   /balance <player>                - another player's balance (requires
//                                      ember:command.economy.balance.others)
//   /pay <player> <amount> [currency]         - transfer money to another player
//   /eco give|take|set <player> <amount> [currency] - admin: adjust a balance
//   /eco reset <player> [currency]                  - admin: reset to the
//                                      configured starting balance

use uuid::Uuid;

use pumpkin_util::PermissionLvl;
use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};

use crate::command::argument_builder::{ArgumentBuilder, argument, command, literal};
use crate::command::argument_types::core::integer::IntegerArgumentType;
use crate::command::argument_types::core::string::StringArgumentType;
use crate::command::argument_types::game_profile::GameProfileArgumentType;
use crate::command::context::command_context::CommandContext;
use crate::command::errors::command_syntax_error::CommandSyntaxError;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::server::economy::EconomyError;

const DESCRIPTION_BALANCE: &str = "Check a player's currency balances.";
const DESCRIPTION_PAY: &str = "Pay another player from your own balance.";
const DESCRIPTION_ECO: &str = "Administer player balances.";

const PERMISSION_BALANCE: &str = "ember:command.economy.balance";
const PERMISSION_BALANCE_OTHERS: &str = "ember:command.economy.balance.others";
const PERMISSION_PAY: &str = "ember:command.economy.pay";
const PERMISSION_ECO: &str = "ember:command.economy.eco";

const ARG_TARGET: &str = "target";
const ARG_AMOUNT: &str = "amount";
const ARG_CURRENCY: &str = "currency";

async fn feedback(context: &CommandContext<'_>, msg: TextComponent) {
    context.source.send_feedback(msg, false).await;
}

fn err_text(msg: impl Into<String>) -> TextComponent {
    TextComponent::text(msg.into()).color_named(NamedColor::Red)
}

fn economy_err_text(who: &str, e: EconomyError) -> TextComponent {
    match e {
        EconomyError::Disabled => err_text("The economy system is not enabled on this server."),
        EconomyError::UnknownCurrency(c) => err_text(format!("Unknown currency '{c}'.")),
        EconomyError::InsufficientFunds { have, need } => err_text(format!(
            "{who} doesn't have enough money (has {have}, needs {need})."
        )),
        EconomyError::Database(e) => err_text(format!("Economy database error: {e}")),
    }
}

async fn show_balances(context: &CommandContext<'_>, target_name: &str, target_uuid: Uuid) {
    let economy = &context.server().economy_manager;
    let mut lines = Vec::new();
    for currency in economy.currencies() {
        match economy.get_balance(target_uuid, Some(currency)).await {
            Ok(balance) => lines.push(format!("{currency}: {balance}")),
            Err(e) => {
                feedback(context, economy_err_text(target_name, e)).await;
                return;
            }
        }
    }
    feedback(
        context,
        TextComponent::text(format!("{target_name}'s balance - {}", lines.join(", ")))
            .color_named(NamedColor::Green),
    )
    .await;
}

/// Reads the optional trailing `<currency>` argument, exactly like
/// `WorldConvertExecutor`'s optional `<border>` (`world.rs`): a `has_currency`
/// field on the executor decides whether this node's tree has that argument
/// at all, so we only try to parse it when it's actually there.
fn optional_currency<'a>(
    context: &'a CommandContext,
    has_currency: bool,
) -> Result<Option<&'a str>, CommandSyntaxError> {
    has_currency
        .then(|| StringArgumentType::get(context, ARG_CURRENCY))
        .transpose()
}

struct BalanceSelfExecutor;
impl CommandExecutor for BalanceSelfExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let player = context.source.player_or_err()?;
            let uuid = player.gameprofile.id;
            let name = player.gameprofile.name.clone();
            show_balances(context, &name, uuid).await;
            Ok(1)
        })
    }
}

struct BalanceOtherExecutor;
impl CommandExecutor for BalanceOtherExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let profiles = GameProfileArgumentType::get(context, ARG_TARGET).await?;
            let Some(profile) = profiles.into_iter().next() else {
                feedback(context, err_text("No matching player.")).await;
                return Ok(0);
            };
            show_balances(context, &profile.name.clone(), profile.id).await;
            Ok(1)
        })
    }
}

struct PayExecutor {
    has_currency: bool,
}
impl CommandExecutor for PayExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let payer = context.source.player_or_err()?;
            let payer_uuid = payer.gameprofile.id;
            let payer_name = payer.gameprofile.name.clone();

            let profiles = GameProfileArgumentType::get(context, ARG_TARGET).await?;
            let amount = IntegerArgumentType::get(context, ARG_AMOUNT)?;
            let currency = optional_currency(context, self.has_currency)?;

            let Some(target) = profiles.into_iter().next() else {
                feedback(context, err_text("No matching player.")).await;
                return Ok(0);
            };
            if target.id == payer_uuid {
                feedback(context, err_text("You can't pay yourself.")).await;
                return Ok(0);
            }

            let result = context
                .server()
                .economy_manager
                .transfer(payer_uuid, target.id, currency, i64::from(amount))
                .await;
            match result {
                Ok(()) => {
                    feedback(
                        context,
                        TextComponent::text(format!("Paid {amount} to {}.", target.name))
                            .color_named(NamedColor::Green),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, economy_err_text(&payer_name, e)).await;
                    Ok(0)
                }
            }
        })
    }
}

#[derive(Clone, Copy)]
enum EcoOp {
    Give,
    Take,
    Set,
    Reset,
}

struct EcoExecutor {
    op: EcoOp,
    has_currency: bool,
}
impl CommandExecutor for EcoExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let profiles = GameProfileArgumentType::get(context, ARG_TARGET).await?;
            let Some(target) = profiles.into_iter().next() else {
                feedback(context, err_text("No matching player.")).await;
                return Ok(0);
            };
            let currency = optional_currency(context, self.has_currency)?;
            let economy = &context.server().economy_manager;

            let result = match self.op {
                EcoOp::Give => {
                    let amount = IntegerArgumentType::get(context, ARG_AMOUNT)?;
                    economy
                        .deposit(target.id, currency, i64::from(amount))
                        .await
                        .map(|_| ())
                }
                EcoOp::Take => {
                    let amount = IntegerArgumentType::get(context, ARG_AMOUNT)?;
                    economy
                        .withdraw(target.id, currency, i64::from(amount))
                        .await
                        .map(|_| ())
                }
                EcoOp::Set => {
                    let amount = IntegerArgumentType::get(context, ARG_AMOUNT)?;
                    economy
                        .set_balance(target.id, currency, i64::from(amount))
                        .await
                }
                EcoOp::Reset => economy.reset_balance(target.id, currency).await,
            };

            match result {
                Ok(()) => {
                    feedback(
                        context,
                        TextComponent::text(format!("Updated {}'s balance.", target.name))
                            .color_named(NamedColor::Green),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, economy_err_text(&target.name, e)).await;
                    Ok(0)
                }
            }
        })
    }
}

fn register_balance(dispatcher: &mut CommandDispatcher, registry: &mut PermissionRegistry) {
    registry.register_permission_or_panic(Permission::new(
        PERMISSION_BALANCE,
        DESCRIPTION_BALANCE,
        PermissionDefault::Allow,
    ));
    registry.register_permission_or_panic(Permission::new(
        PERMISSION_BALANCE_OTHERS,
        "Check another player's balance.",
        PermissionDefault::Op(PermissionLvl::Three),
    ));

    dispatcher.register(
        command("balance", DESCRIPTION_BALANCE)
            .requires(PERMISSION_BALANCE)
            .executes(BalanceSelfExecutor)
            .then(
                argument(ARG_TARGET, GameProfileArgumentType)
                    .requires(PERMISSION_BALANCE_OTHERS)
                    .executes(BalanceOtherExecutor),
            ),
    );
}

fn register_pay(dispatcher: &mut CommandDispatcher, registry: &mut PermissionRegistry) {
    registry.register_permission_or_panic(Permission::new(
        PERMISSION_PAY,
        DESCRIPTION_PAY,
        PermissionDefault::Allow,
    ));

    dispatcher.register(
        command("pay", DESCRIPTION_PAY)
            .requires(PERMISSION_PAY)
            .then(
                argument(ARG_TARGET, GameProfileArgumentType).then(
                    argument(ARG_AMOUNT, IntegerArgumentType::with_min(1))
                        .executes(PayExecutor {
                            has_currency: false,
                        })
                        .then(
                            argument(ARG_CURRENCY, StringArgumentType::SingleWord)
                                .executes(PayExecutor { has_currency: true }),
                        ),
                ),
            ),
    );
}

fn register_eco(dispatcher: &mut CommandDispatcher, registry: &mut PermissionRegistry) {
    registry.register_permission_or_panic(Permission::new(
        PERMISSION_ECO,
        DESCRIPTION_ECO,
        PermissionDefault::Op(PermissionLvl::Three),
    ));

    dispatcher.register(
        command("eco", DESCRIPTION_ECO)
            .requires(PERMISSION_ECO)
            .then(
                literal("give").then(
                    argument(ARG_TARGET, GameProfileArgumentType).then(
                        argument(ARG_AMOUNT, IntegerArgumentType::with_min(1))
                            .executes(EcoExecutor {
                                op: EcoOp::Give,
                                has_currency: false,
                            })
                            .then(
                                argument(ARG_CURRENCY, StringArgumentType::SingleWord).executes(
                                    EcoExecutor {
                                        op: EcoOp::Give,
                                        has_currency: true,
                                    },
                                ),
                            ),
                    ),
                ),
            )
            .then(
                literal("take").then(
                    argument(ARG_TARGET, GameProfileArgumentType).then(
                        argument(ARG_AMOUNT, IntegerArgumentType::with_min(1))
                            .executes(EcoExecutor {
                                op: EcoOp::Take,
                                has_currency: false,
                            })
                            .then(
                                argument(ARG_CURRENCY, StringArgumentType::SingleWord).executes(
                                    EcoExecutor {
                                        op: EcoOp::Take,
                                        has_currency: true,
                                    },
                                ),
                            ),
                    ),
                ),
            )
            .then(
                literal("set").then(
                    argument(ARG_TARGET, GameProfileArgumentType).then(
                        argument(ARG_AMOUNT, IntegerArgumentType::with_min(0))
                            .executes(EcoExecutor {
                                op: EcoOp::Set,
                                has_currency: false,
                            })
                            .then(
                                argument(ARG_CURRENCY, StringArgumentType::SingleWord).executes(
                                    EcoExecutor {
                                        op: EcoOp::Set,
                                        has_currency: true,
                                    },
                                ),
                            ),
                    ),
                ),
            )
            .then(
                literal("reset").then(
                    argument(ARG_TARGET, GameProfileArgumentType)
                        .executes(EcoExecutor {
                            op: EcoOp::Reset,
                            has_currency: false,
                        })
                        .then(
                            argument(ARG_CURRENCY, StringArgumentType::SingleWord).executes(
                                EcoExecutor {
                                    op: EcoOp::Reset,
                                    has_currency: true,
                                },
                            ),
                        ),
                ),
            ),
    );
}

pub fn register(dispatcher: &mut CommandDispatcher, registry: &mut PermissionRegistry) {
    register_balance(dispatcher, registry);
    register_pay(dispatcher, registry);
    register_eco(dispatcher, registry);
}
// EMBER end
