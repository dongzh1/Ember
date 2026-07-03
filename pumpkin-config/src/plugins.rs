use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Default)]
#[serde(default)]
pub struct PluginsConfig {
    /// List of permissions that are globally blocked for all plugins.
    pub blocked_permissions: Vec<String>,
    // EMBER start - auto_approve_permissions
    /// If true, automatically approve all plugin permission requests
    /// without prompting in the console. Default: false.
    #[serde(default)]
    pub auto_approve_permissions: bool,
    // EMBER end
}
