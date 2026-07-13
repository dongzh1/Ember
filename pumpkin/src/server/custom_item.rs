// EMBER start: custom items (item_model component on vanilla base items)
//! Custom items: a real vanilla item wearing a `minecraft:item_model` component.
//!
//! Points at a model somewhere in the resource pack builder's `source_dir`.
//! There's no "new item id" concept in the protocol - every custom item is,
//! underneath, still some existing vanilla `base_item` (the same approach
//! `CraftEngine` itself uses on Paper).
//!
//! Scoped to the new (1.21.2+) `item_model` component only - the old
//! numeric `custom_model_data` component is an unimplemented stub upstream
//! (no NBT/wire codec registered at all), so pre-1.21.2 clients just see
//! the base item's own vanilla appearance rather than the custom one. See
//! `EMBER.md` for the full writeup of that gap.
use pumpkin_config::{CustomItemConfig, CustomItemListConfig, LoadConfiguration};
use pumpkin_data::data_component::DataComponent;
use pumpkin_data::data_component_impl::{CustomNameImpl, ItemModelImpl};
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_util::text::TextComponent;
use tokio::sync::RwLock;

pub struct CustomItemManager {
    config: RwLock<CustomItemListConfig>,
}

impl Default for CustomItemManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CustomItemManager {
    #[must_use]
    pub fn new() -> Self {
        let exec_dir = std::env::current_dir().expect("Failed to get current directory");
        Self {
            config: RwLock::new(CustomItemListConfig::load(&exec_dir)),
        }
    }

    pub async fn list_ids(&self) -> Vec<String> {
        self.config
            .read()
            .await
            .items
            .iter()
            .map(|i| i.id.clone())
            .collect()
    }

    /// Identifies which configured custom item a held stack is, by matching
    /// its `ItemModel` component's path against each entry's `model` - the
    /// only marker a placed stack carries pointing back to its config
    /// entry, since custom items aren't a real, separate item id.
    pub async fn find_id_by_model(&self, model: &str) -> Option<String> {
        let config = self.config.read().await;
        config
            .items
            .iter()
            .find(|i| i.model == model)
            .map(|i| i.id.clone())
    }

    /// Builds one stack of `count` (already clamped to the base item's max
    /// stack size by the caller - same split-across-stacks convention as
    /// vanilla `/give`). `None` if `id` isn't configured, or `base_item`
    /// isn't a real item.
    pub async fn build_stack(&self, id: &str, count: u8) -> Option<ItemStack> {
        let config = self.config.read().await;
        let entry = config
            .items
            .iter()
            .find(|i| i.id.eq_ignore_ascii_case(id))?;
        Self::stack_from_entry(entry, count)
    }

    /// Looks up a configured custom item, without building a stack - used
    /// by `server::furniture::FurnitureManager` to resolve which base item
    /// and model a placed furniture instance should render as.
    pub async fn resolve_visual(&self, id: &str) -> Option<(&'static Item, String)> {
        let config = self.config.read().await;
        let entry = config
            .items
            .iter()
            .find(|i| i.id.eq_ignore_ascii_case(id))?;
        let item = Item::from_registry_key(&entry.base_item.to_lowercase())?;
        Some((item, entry.model.clone()))
    }

    fn stack_from_entry(entry: &CustomItemConfig, count: u8) -> Option<ItemStack> {
        let item = Item::from_registry_key(&entry.base_item.to_lowercase())?;
        let mut stack = ItemStack::new(count, item);
        stack.patch.push((
            DataComponent::ItemModel,
            Some(Box::new(ItemModelImpl {
                id: entry.model.clone().into(),
            })),
        ));
        if let Some(name) = &entry.display_name {
            stack.patch.push((
                DataComponent::CustomName,
                Some(Box::new(CustomNameImpl {
                    name: TextComponent::text(name.clone()),
                })),
            ));
        }
        Some(stack)
    }
}

#[cfg(test)]
mod tests {
    use pumpkin_data::data_component_impl::get;

    use super::*;

    fn config(display_name: Option<&str>) -> CustomItemConfig {
        CustomItemConfig {
            id: "test_sword".to_string(),
            base_item: "diamond_sword".to_string(),
            model: "ember:items/test_sword".to_string(),
            display_name: display_name.map(str::to_string),
        }
    }

    #[test]
    fn builds_a_stack_with_the_item_model_patch() {
        let stack = CustomItemManager::stack_from_entry(&config(None), 3).unwrap();
        assert_eq!(stack.item.registry_key, "diamond_sword");
        assert_eq!(stack.item_count, 3);
        let (id, component) = &stack.patch[0];
        assert_eq!(*id, DataComponent::ItemModel);
        let model = get::<ItemModelImpl>(component.as_ref().unwrap().as_ref());
        assert_eq!(model.id.as_ref(), "ember:items/test_sword");
    }

    #[test]
    fn omits_custom_name_when_not_configured() {
        let stack = CustomItemManager::stack_from_entry(&config(None), 1).unwrap();
        assert_eq!(stack.patch.len(), 1);
    }

    #[test]
    fn adds_custom_name_when_configured() {
        let stack =
            CustomItemManager::stack_from_entry(&config(Some("Legendary Sword")), 1).unwrap();
        assert_eq!(stack.patch.len(), 2);
        let (id, component) = &stack.patch[1];
        assert_eq!(*id, DataComponent::CustomName);
        let name = get::<CustomNameImpl>(component.as_ref().unwrap().as_ref());
        assert_eq!(name.name.clone().get_text(), "Legendary Sword");
    }

    #[test]
    fn unknown_base_item_returns_none() {
        let mut cfg = config(None);
        cfg.base_item = "not_a_real_item".to_string();
        assert!(CustomItemManager::stack_from_entry(&cfg, 1).is_none());
    }
}
// EMBER end
