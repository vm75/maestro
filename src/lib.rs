pub mod api;
#[path = "platforms/asus_ui.rs"]
pub mod asus_ui;
pub mod auth;
pub mod ban_attack;
pub mod certificate_http;
pub mod command;
pub mod config;
pub mod core;
pub mod models;
pub mod platforms;
pub mod modules;
pub mod nginx_ingress;
mod nginx_mutation;
pub mod nginx_render;
pub mod state;
pub mod storage;
pub mod system;
pub mod util;

// Stable crate-root aliases while domain sources move beneath modules/.
pub use modules::{adguard, firewall, nginx, nginx_sites};
