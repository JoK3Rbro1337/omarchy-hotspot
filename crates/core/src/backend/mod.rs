//! Абстракция над NetworkManager. Реальная реализация — `nmcli`, для тестов — `mock`.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::CoreError;
use crate::secret::Secret;

#[cfg(test)]
pub mod mock;
pub mod nm_dbus;
pub mod nmcli;
pub mod nmcli_parse;

pub use crate::PROFILE_NAME;

/// Wpa3: `sae` + PMF обязателен. Wpa2: `wpa-psk` + PMF по возможности — NM сам добавляет SAE,
/// получается смешанный WPA2/WPA3 (новые устройства — WPA3, старые — WPA2). Всегда RSN/CCMP.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    Wpa3,
    Wpa2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Band {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "2.4")]
    Ghz2_4,
    #[serde(rename = "5")]
    Ghz5,
}

/// Подсеть точки доступа: адрес ПК и длина префикса (10.42.0.1/24).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ipv4Net {
    pub addr: std::net::Ipv4Addr,
    pub prefix: u8,
}

impl Ipv4Net {
    pub fn parse(s: &str) -> Option<Ipv4Net> {
        let (a, p) = s.split_once('/')?;
        let prefix: u8 = p.parse().ok()?;
        if !(8..=30).contains(&prefix) {
            return None;
        }
        Some(Ipv4Net {
            addr: a.parse().ok()?,
            prefix,
        })
    }

    /// Адрес ПК лежит в частном диапазоне (10/8, 172.16/12, 192.168/16) и не совпадает
    /// с адресом самой сети или широковещательным: иначе гости не получат адреса, а
    /// публичный диапазон перекрыл бы настоящие сайты.
    pub fn is_private_host(&self) -> bool {
        let o = self.addr.octets();
        let (private, min_prefix) = match o {
            [10, ..] => (true, 8),
            [172, b, ..] if (16..=31).contains(&b) => (true, 12),
            [192, 168, ..] => (true, 16),
            _ => (false, 0),
        };
        if !private || self.prefix < min_prefix {
            return false;
        }
        let ip = u32::from(self.addr);
        let host_mask = u32::MAX >> self.prefix;
        let host = ip & host_mask;
        host != 0 && host != host_mask
    }
}

impl std::fmt::Display for Ipv4Net {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

/// Настройки профиля. Диапазон и канал здесь уже конкретные (авто разрешено заранее).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotspotSettings {
    pub ssid: String,
    /// `None` — не менять пароль, сохранённый в NM.
    pub password: Option<Secret>,
    pub security: Security,
    pub band: Band,
    pub channel: Option<u8>,
    /// Ширина канала в МГц; 0 — пусть решает NetworkManager (самая узкая).
    pub width_mhz: u16,
    pub hidden: bool,
    pub ap_isolation: bool,
    pub ap_iface: String,
    pub subnet: Ipv4Net,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HotspotState {
    Off,
    Starting,
    On {
        /// `None` — раздачу включили не мы (время неизвестно).
        since: Option<SystemTime>,
        ap_iface: String,
        uplink: Option<String>,
    },
    Error(String),
}

pub trait NetworkBackend: Send + Sync {
    fn profile_exists(&self) -> Result<bool, CoreError>;
    /// Создать профиль или обновить существующий.
    fn ensure_profile(&self, s: &HotspotSettings) -> Result<(), CoreError>;
    fn set_password(&self, psk: &Secret) -> Result<(), CoreError>;
    fn get_password(&self) -> Result<Secret, CoreError>;
    fn up(&self) -> Result<(), CoreError>;
    fn down(&self) -> Result<(), CoreError>;
    fn delete_profile(&self) -> Result<(), CoreError>;
    fn state(&self) -> Result<HotspotState, CoreError>;
    fn wifi_ifaces(&self) -> Result<Vec<String>, CoreError>;
    /// Имя сети, к которой Wi-Fi-адаптер сейчас подключён как клиент (не наш профиль).
    /// Нужно, чтобы предупредить: при включении раздачи это подключение оборвётся.
    fn client_connection(&self, iface: &str) -> Result<Option<String>, CoreError>;
    /// Подключённые сетевые интерфейсы (кроме loopback и нашей точки доступа):
    /// из них пользователь выбирает источник интернета.
    fn connected_ifaces(&self) -> Result<Vec<String>, CoreError>;
    fn nm_running(&self) -> Result<bool, CoreError>;
}
