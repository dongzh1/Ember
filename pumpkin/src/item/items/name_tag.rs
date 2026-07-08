use std::pin::Pin;
use std::sync::Arc;

use crate::entity::EntityBase;
use crate::entity::player::Player;
use crate::item::{ItemBehaviour, ItemMetadata};
use pumpkin_data::data_component_impl::CustomNameImpl;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;

pub struct NameTagItem;

impl ItemMetadata for NameTagItem {
    fn ids() -> Box<[u16]> {
        [Item::NAME_TAG.id].into()
    }
}

impl ItemBehaviour for NameTagItem {
    fn use_on_entity<'a>(
        &'a self,
        item: &'a mut ItemStack,
        player: &'a Player,
        entity: Arc<dyn EntityBase>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if entity.get_entity().entity_type.saveable
                && let Some(name) = item.get_data_component::<CustomNameImpl>()
            {
                // TODO
                entity.get_entity().set_custom_name(name.name.clone());
                // EMBER start - name-tagged mobs are exempt from distance despawn
                if let Some(mob) = entity.as_mob_entity() {
                    mob.set_persistence_required(true);
                }
                // EMBER end
                item.decrement_unless_creative(player.gamemode.load(), 1);
            }
        })
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
