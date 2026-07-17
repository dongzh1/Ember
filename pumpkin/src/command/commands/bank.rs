// EMBER - built-in bank system command
//
//   /bank balance [currency]              - your bank balance (interest settles first)
//   /bank deposit <amount> [currency]     - move money from wallet to bank
//   /bank withdraw <amount> [currency]    - move money from bank to wallet
//   /bank log [currency]                  - your last 10 bank transactions

use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};
use pumpkin_util::translation::{Locale, get_translation_text};

use crate::command::argument_builder::{ArgumentBuilder, argument, command, literal};
use crate::command::argument_types::core::integer::IntegerArgumentType;
use crate::command::argument_types::core::string::StringArgumentType;
use crate::command::context::command_context::CommandContext;
use crate::command::errors::command_syntax_error::CommandSyntaxError;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::command::suggestion::provider::{SuggestionProvider, SuggestionProviderResult};
use crate::command::suggestion::suggestions::SuggestionsBuilder;
use crate::server::Server;
use crate::server::shop::ShopError;

const DESCRIPTION: &str = "Deposit/withdraw money from your bank account.";
const PERMISSION: &str = "ember:command.bank";
const ARG_AMOUNT: &str = "amount";
const ARG_CURRENCY: &str = "currency";

async fn feedback(context: &CommandContext<'_>, msg: TextComponent) {
    context.source.send_feedback(msg, false).await;
}

/// Wraps an already-built (and already localized) message in Ember's plain
/// error color - errors stay clearly red/plain rather than picking up the
/// ember gradient, for legibility.
fn err_text(component: TextComponent) -> TextComponent {
    component.color_named(NamedColor::Red)
}

/// Translates a `ShopError` into player-facing text. `ShopError` is shared
/// across the whole shop system (`market`/`lottery`/`basic_shop` too), so only
/// the variants with structured data get bespoke translated wording here;
/// `Other` already carries a final, fully-formed message from wherever it
/// was constructed (`BankManager`'s own already-localized cap-exceeded
/// message, or a rare fallback derived from an underlying `EconomyError`)
/// and is shown verbatim rather than wrapped in another translation layer.
fn shop_err_text(e: &ShopError, currency: &str, locale: Locale) -> TextComponent {
    match e {
        ShopError::Disabled => err_text(TextComponent::custom(
            "ember",
            "commands.bank.disabled",
            locale,
            vec![],
        )),
        ShopError::Database(msg) => err_text(TextComponent::custom(
            "ember",
            "commands.bank.database_error",
            locale,
            vec![TextComponent::text(msg.clone())],
        )),
        ShopError::InsufficientFunds { have, need } => err_text(TextComponent::custom(
            "ember",
            "commands.bank.insufficient_funds",
            locale,
            vec![
                TextComponent::text(currency.to_string()),
                TextComponent::text(have.to_string()),
                TextComponent::text(need.to_string()),
            ],
        )),
        ShopError::Other(msg) => err_text(TextComponent::text(msg.clone())),
    }
}

fn optional_currency<'a>(
    context: &'a CommandContext,
    has_currency: bool,
) -> Result<Option<&'a str>, CommandSyntaxError> {
    has_currency
        .then(|| StringArgumentType::get(context, ARG_CURRENCY))
        .transpose()
}

/// Suggests every currency id configured in `economy/economy.toml`.
struct CurrencySuggestionProvider;

impl SuggestionProvider for CurrencySuggestionProvider {
    fn suggest<'a>(
        &'a self,
        context: &'a CommandContext,
        builder: SuggestionsBuilder,
    ) -> SuggestionProviderResult<'a> {
        Box::pin(async move {
            let currencies = context.server().economy_manager.currencies();
            builder.filter_and_suggest_iter(currencies).build()
        })
    }
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
            let locale = context.source.output.get_locale();
            let currency = optional_currency(context, self.has_currency)?;
            let currency = resolve_currency(server, currency);
            let tier = resolve_tier(player.gameprofile.id, server).await;

            match server
                .bank_manager
                .settle_and_get(player.gameprofile.id, &currency, &tier)
                .await
            {
                Ok(account) => {
                    // A balance/status display, like `/balance` - plain data
                    // rather than a headline moment, so it keeps the
                    // existing plain green instead of the ember gradient.
                    // The "%" is baked into the pre-formatted rate argument
                    // (not the translation template) since the translation
                    // system treats every literal `%` in a template as the
                    // start of a placeholder.
                    feedback(
                        context,
                        TextComponent::custom(
                            "ember",
                            "commands.bank.balance",
                            locale,
                            vec![
                                TextComponent::text(account.balance.to_string()),
                                TextComponent::text(currency.clone()),
                                TextComponent::text(tier.max_balance.to_string()),
                                TextComponent::text(format!("{}%", tier.daily_rate * 100.0)),
                            ],
                        )
                        .color_named(NamedColor::Green),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, shop_err_text(&e, &currency, locale)).await;
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
            let locale = context.source.output.get_locale();
            let amount = IntegerArgumentType::get(context, ARG_AMOUNT)?;
            let currency = optional_currency(context, self.has_currency)?;
            let currency = resolve_currency(server, currency);
            let tier = resolve_tier(player.gameprofile.id, server).await;

            if amount <= 0 {
                feedback(
                    context,
                    err_text(TextComponent::custom(
                        "ember",
                        "commands.bank.amount_must_be_positive",
                        locale,
                        vec![],
                    )),
                )
                .await;
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
                    locale,
                )
                .await
            {
                Ok(new_balance) => {
                    // Direct success confirmation - gets the branded ember
                    // gradient, applied to the already-resolved string (not
                    // the lazy `Custom` component) since
                    // `text_ember`/`ember_gradient` flattens its input via
                    // `get_text(Locale::EnUs)` internally, which would
                    // silently discard localization.
                    let text = get_translation_text(
                        "ember:commands.bank.deposited",
                        locale,
                        vec![
                            TextComponent::text(amount.to_string()).0,
                            TextComponent::text(currency.clone()).0,
                            TextComponent::text(new_balance.to_string()).0,
                        ],
                    );
                    feedback(context, TextComponent::text_ember(text)).await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, shop_err_text(&e, &currency, locale)).await;
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
            let locale = context.source.output.get_locale();
            let amount = IntegerArgumentType::get(context, ARG_AMOUNT)?;
            let currency = optional_currency(context, self.has_currency)?;
            let currency = resolve_currency(server, currency);
            let tier = resolve_tier(player.gameprofile.id, server).await;

            if amount <= 0 {
                feedback(
                    context,
                    err_text(TextComponent::custom(
                        "ember",
                        "commands.bank.amount_must_be_positive",
                        locale,
                        vec![],
                    )),
                )
                .await;
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
                    locale,
                )
                .await
            {
                Ok(new_balance) => {
                    // Direct success confirmation - gets the branded ember
                    // gradient (see `BankDepositExecutor` for why the
                    // resolved string, not the lazy `Custom` component).
                    let text = get_translation_text(
                        "ember:commands.bank.withdrew",
                        locale,
                        vec![
                            TextComponent::text(amount.to_string()).0,
                            TextComponent::text(currency.clone()).0,
                            TextComponent::text(new_balance.to_string()).0,
                        ],
                    );
                    feedback(context, TextComponent::text_ember(text)).await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, shop_err_text(&e, &currency, locale)).await;
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
            let locale = context.source.output.get_locale();
            let currency = optional_currency(context, self.has_currency)?;
            let currency = resolve_currency(server, currency);

            match server
                .bank_manager
                .recent_transactions(player.gameprofile.id, &currency)
                .await
            {
                Ok(transactions) if transactions.is_empty() => {
                    feedback(
                        context,
                        TextComponent::custom("ember", "commands.bank.log.empty", locale, vec![]),
                    )
                    .await;
                    Ok(0)
                }
                Ok(transactions) => {
                    // A transaction log listing - plain data rows, like
                    // `/balance`'s currency list, so no ember gradient (and
                    // no explicit color at all, matching the previous
                    // behavior).
                    let mut lines = vec![get_translation_text(
                        "ember:commands.bank.log.header",
                        locale,
                        vec![],
                    )];
                    for t in transactions {
                        let label_key = if t.is_interest {
                            "ember:commands.bank.log.interest"
                        } else if t.amount >= 0 {
                            "ember:commands.bank.log.deposit"
                        } else {
                            "ember:commands.bank.log.withdraw"
                        };
                        let label = get_translation_text(label_key, locale, vec![]);
                        let line = get_translation_text(
                            "ember:commands.bank.log.line",
                            locale,
                            vec![
                                TextComponent::text(label).0,
                                TextComponent::text(format!("{:+}", t.amount)).0,
                                TextComponent::text(currency.clone()).0,
                            ],
                        );
                        lines.push(format!("  {line}"));
                    }
                    feedback(context, TextComponent::text(lines.join("\n"))).await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, shop_err_text(&e, &currency, locale)).await;
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
                            .suggests(CurrencySuggestionProvider)
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
                                .suggests(CurrencySuggestionProvider)
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
                                .suggests(CurrencySuggestionProvider)
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
                            .suggests(CurrencySuggestionProvider)
                            .executes(BankLogExecutor { has_currency: true }),
                    ),
            ),
    );
}
// EMBER end
