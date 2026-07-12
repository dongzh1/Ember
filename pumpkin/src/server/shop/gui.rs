// EMBER start - built-in shop/bank/market/lottery system
//! Small helpers shared by every shop-system menu.
//!
//! Each menu (shop/bank/market/lottery) is its own concrete `ScreenHandler`
//! implementation (see `super::shop`) rather than a generic reusable
//! framework - the four menus have different enough slot layouts/actions
//! that a one-size-fits-all "any admin can configure a menu" abstraction
//! (like `PixelShop`'s `GuiItem`/`MenuGui`) isn't worth building for four
//! fixed, code-defined screens.

use pumpkin_protocol::java::server::play::SlotActionType;

/// Simplified click classification for menus that never allow real item
/// movement.
///
/// Only a plain left/right single-click means anything; shift-click,
/// number-key swap, drag, double-click, and throw are all
/// [`ClickKind::Ignored`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickKind {
    Left,
    Right,
    Ignored,
}

/// Classifies a slot click from its raw protocol fields.
///
/// `button == 0` means left-click and `button == 1` means right-click for a
/// `Pickup` action - the same mapping `Player::on_slot_click` itself uses
/// to distinguish them.
#[must_use]
pub const fn classify_click(action_type: &SlotActionType, button: i32) -> ClickKind {
    match (action_type, button) {
        (SlotActionType::Pickup, 0) => ClickKind::Left,
        (SlotActionType::Pickup, 1) => ClickKind::Right,
        _ => ClickKind::Ignored,
    }
}

/// Fixed batch-purchase quantities, scaled to the item's max stack size.
///
/// 64-stack items get x1/x5/x10/x32/x64, 16-stack items (snowballs, ender
/// pearls, ...) get x1/x4/x8/x12/x16. Matches `PixelShop`'s exact tiers: a
/// proven, simple UX that needs no free-text quantity input.
#[must_use]
pub const fn bulk_purchase_amounts(max_stack_size: u8) -> &'static [u32] {
    if max_stack_size <= 16 {
        &[1, 4, 8, 12, 16]
    } else {
        &[1, 5, 10, 32, 64]
    }
}
// EMBER end
