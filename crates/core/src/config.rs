//! `~/.config/omarchy-hotspot/config.toml` — без пароля (его хранит NetworkManager).
//! Отсутствующие поля берутся по умолчанию, неизвестные — игнорируются с предупреждением.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::CoreError;
use crate::backend::{Band, Security};
use crate::ssid::{SSID_MAX_BYTES, truncate_bytes};

pub const CONFIG_VERSION: u32 = 1;
pub const DEFAULT_SUBNET: &str = "10.42.0.1/24";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    /// `None` → язык из `$LANG`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub hotspot: HotspotConfig,
    pub network: NetworkConfig,
    pub access: AccessConfig,
    pub automation: AutomationConfig,
    pub ui: UiConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotspotConfig {
    pub ssid: String,
    pub security: Security,
    pub band: Band,
    /// 0 = авто.
    pub channel: u8,
    pub width_mhz: u16,
    pub hidden: bool,
    pub ap_isolation: bool,
    pub guests_can_reach_pc: bool,
    /// "" = не менять.
    pub country: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    /// "" = авто.
    pub ap_interface: String,
    /// "" = авто по маршруту к 1.1.1.1.
    pub uplink_interface: String,
    pub subnet: String,
    pub dns: Vec<String>,
    pub ipv6: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccessConfig {
    pub approval_required: bool,
    pub allowed_macs: Vec<String>,
    pub blocked_macs: Vec<String>,
    pub notifications: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutomationConfig {
    pub autostart_on_login: bool,
    pub idle_off_minutes: u32,
    pub timer_minutes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiMode {
    Simple,
    Advanced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub show_password: bool,
    pub mode: UiMode,
    /// Закрывать окно, когда оно теряет фокус (как системные панели Omarchy).
    pub close_on_focus_loss: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: CONFIG_VERSION,
            language: None,
            hotspot: HotspotConfig::default(),
            network: NetworkConfig::default(),
            access: AccessConfig::default(),
            automation: AutomationConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

impl Default for HotspotConfig {
    fn default() -> Self {
        HotspotConfig {
            ssid: default_ssid(&hostname()),
            security: Security::Wpa3,
            band: Band::Auto,
            channel: 0,
            width_mhz: 20,
            hidden: false,
            ap_isolation: true,
            guests_can_reach_pc: false,
            country: String::new(),
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        NetworkConfig {
            ap_interface: String::new(),
            uplink_interface: String::new(),
            subnet: DEFAULT_SUBNET.into(),
            dns: Vec::new(),
            ipv6: false,
        }
    }
}

impl Default for AccessConfig {
    fn default() -> Self {
        AccessConfig {
            approval_required: true,
            allowed_macs: Vec::new(),
            blocked_macs: Vec::new(),
            notifications: true,
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            show_password: false,
            mode: UiMode::Simple,
            close_on_focus_loss: true,
        }
    }
}

/// "Omarchy-<hostname>", обрезанное до 32 байт; без имени — просто "Omarchy".
pub fn default_ssid(hostname: &str) -> String {
    let h: String = hostname
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    if h.is_empty() {
        return "Omarchy".into();
    }
    truncate_bytes(&format!("Omarchy-{h}"), SSID_MAX_BYTES).to_string()
}

fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default()
}

/// Каталог конфига: `$XDG_CONFIG_HOME/omarchy-hotspot` или `~/.config/omarchy-hotspot`.
pub fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("omarchy-hotspot"))
}

pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

/// Результат чтения: конфиг и список неизвестных ключей (для предупреждения).
#[derive(Debug)]
pub struct Loaded {
    pub config: Config,
    pub unknown_keys: Vec<String>,
}

/// Читает конфиг; файла нет → значения по умолчанию.
pub fn load(path: &Path) -> Result<Loaded, CoreError> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Loaded {
                config: Config::default(),
                unknown_keys: Vec::new(),
            });
        }
        Err(e) => return Err(e.into()),
    };
    parse(&text)
}

pub fn parse(text: &str) -> Result<Loaded, CoreError> {
    let bad = |e: &dyn std::fmt::Display| CoreError::Config(e.to_string());
    let config: Config = toml::from_str(text).map_err(|e| bad(&e))?;
    let raw: toml::Table = toml::from_str(text).map_err(|e| bad(&e))?;
    let known = toml::Table::try_from(Config::default()).map_err(|e| bad(&e))?;
    let mut unknown_keys = Vec::new();
    collect_unknown(&raw, &known, "", &mut unknown_keys);
    Ok(Loaded {
        config,
        unknown_keys,
    })
}

/// Ключи из `raw`, которых нет в эталоне. Раздел `[limits]` — от лимита скорости, от которого
/// отказались: в старых конфигах он есть, молча пропускаем (исчезнет при следующем сохранении).
fn collect_unknown(raw: &toml::Table, known: &toml::Table, prefix: &str, out: &mut Vec<String>) {
    for (k, v) in raw {
        let full = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        // `language` не сериализуется, когда не задан.
        if full == "language" || full == "limits" {
            continue;
        }
        match (v, known.get(k)) {
            (_, None) => out.push(full),
            (toml::Value::Table(r), Some(toml::Value::Table(kn))) => {
                collect_unknown(r, kn, &full, out)
            }
            _ => {}
        }
    }
}

/// Прочитать, изменить и записать конфиг под замком. Конфиг пишут трое — окно, CLI и служба,
/// — и без замка правка одного затирала бы правку другого (например, служба записала одобренное
/// устройство, а окно тут же сохранило имя сети из своей старой копии).
/// Возвращает результат изменения и новый конфиг.
pub fn update<T>(
    path: &Path,
    change: impl FnOnce(&mut Config) -> Result<T, CoreError>,
) -> Result<(T, Config), CoreError> {
    let _guard = lock(path)?;
    let mut config = load(path)?.config;
    let value = change(&mut config)?;
    save(path, &config)?;
    Ok((value, config))
}

/// Файл-замок рядом с конфигом. Держится только на время «прочитал → изменил → записал».
fn lock(path: &Path) -> Result<nix::fcntl::Flock<fs::File>, CoreError> {
    // Каталог нужен уже сейчас — под файл замка. Права на него ставит `save`.
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path.with_extension("toml.lock"))?;
    nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusive)
        .map_err(|(_, e)| CoreError::Io(std::io::Error::from(e)))
}

/// Атомарная запись: временный файл рядом → rename. Каталог 0700, файл 0600.
pub fn save(path: &Path, config: &Config) -> Result<(), CoreError> {
    let text = toml::to_string_pretty(config).map_err(|e| CoreError::Config(e.to_string()))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    let tmp = path.with_extension("toml.tmp");
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ssid_from_hostname() {
        assert_eq!(default_ssid("archpc\n"), "Omarchy-archpc");
        assert_eq!(default_ssid(""), "Omarchy");
        assert_eq!(default_ssid(&"x".repeat(40)).len(), 32);
    }

    #[test]
    fn empty_file_gives_defaults() {
        let l = parse("").unwrap();
        assert_eq!(l.config.hotspot.security, Security::Wpa3);
        assert_eq!(l.config.hotspot.band, Band::Auto);
        assert!(l.config.hotspot.ap_isolation);
        assert_eq!(l.config.network.subnet, DEFAULT_SUBNET);
        assert!(l.unknown_keys.is_empty());
    }

    #[test]
    fn partial_file_and_unknown_keys() {
        let text = r#"
            language = "en"
            colour = "red"
            [hotspot]
            ssid = "Test"
            band = "2.4"
            security = "wpa2"
            magic = 1
            [limits]
            default_down_kbps = 0
            [limits.per_device]
            "aa:bb:cc:dd:ee:ff" = { down_kbps = 100 }
        "#;
        let l = parse(text).unwrap();
        assert_eq!(l.config.language.as_deref(), Some("en"));
        assert_eq!(l.config.hotspot.ssid, "Test");
        assert_eq!(l.config.hotspot.band, Band::Ghz2_4);
        assert_eq!(l.config.hotspot.security, Security::Wpa2);
        assert!(l.config.hotspot.ap_isolation);
        assert_eq!(l.unknown_keys, vec!["colour", "hotspot.magic"]);
    }

    #[test]
    fn bad_value_is_error() {
        assert!(parse("[hotspot]\nband = \"6\"").is_err());
    }

    #[test]
    fn update_reads_the_file_and_keeps_other_fields() {
        let dir = std::env::temp_dir().join(format!("oh-upd-test-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut c = Config::default();
        c.access.allowed_macs = vec!["aa:bb:cc:dd:ee:01".into()];
        save(&path, &c).unwrap();
        // Меняем только имя сети — список одобренных устройств остаётся.
        let (out, config) = update(&path, |cfg| {
            cfg.hotspot.ssid = "Changed".into();
            Ok(7)
        })
        .unwrap();
        assert_eq!(out, 7);
        assert_eq!(config.hotspot.ssid, "Changed");
        assert_eq!(config.access.allowed_macs, ["aa:bb:cc:dd:ee:01"]);
        assert_eq!(load(&path).unwrap().config, config);
        // Ошибка изменения ничего не записывает.
        let err = update(&path, |cfg| {
            cfg.hotspot.ssid = "Nope".into();
            Err::<(), _>(CoreError::Config("no".into()))
        });
        assert!(err.is_err());
        assert_eq!(load(&path).unwrap().config.hotspot.ssid, "Changed");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn roundtrip_save_load() {
        let dir = std::env::temp_dir().join(format!("oh-cfg-test-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut c = Config::default();
        c.hotspot.ssid = "Сеть; \"x\"".into();
        c.hotspot.band = Band::Ghz5;
        save(&path, &c).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let l = load(&path).unwrap();
        assert_eq!(l.config, c);
        assert!(l.unknown_keys.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }
}
