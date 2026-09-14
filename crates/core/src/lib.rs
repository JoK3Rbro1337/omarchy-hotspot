//! Логика omarchy-hotspot без интерфейса и без root. Структура — docs/DECISIONS.md §3.

pub mod access;
pub mod backend;
pub mod config;
pub mod devices;
pub mod doctor;
pub mod error;
pub mod helper;
/// Протокол помощника живёт в отдельном крейте (его подключает помощник root без остального ядра).
pub use omarchy_hotspot_proto as helper_proto;
pub mod hotspot;
pub mod i18n;
pub mod ipc;
pub mod net;
pub mod oplock;
pub mod password;
pub mod qr;
pub mod secret;
pub mod ssid;
pub mod traffic;

pub use error::CoreError;
pub use i18n::{Lang, Msg, t, tf};

/// Имя профиля NetworkManager.
pub const PROFILE_NAME: &str = "omarchy-hotspot";
