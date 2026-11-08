//! Isolated native HubEngine regression target; exercises the actual system SQLite engine.
#[path = "../../crates/cito-native/src/core/auth.rs"]
mod auth;
#[path = "../../crates/cito-native/src/core/database.rs"]
mod database;
#[path = "../../crates/cito-native/src/core/hub.rs"]
mod hub;
#[path = "../../crates/cito-native/src/core/hub_plugins.rs"]
mod hub_plugins;
#[path = "../../crates/cito-native/src/core/identity.rs"]
mod identity;
