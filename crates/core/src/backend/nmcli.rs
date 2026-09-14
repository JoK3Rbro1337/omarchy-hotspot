//! `NetworkBackend` через `nmcli` (LANG=C, разбор только `-t` / `-g`).

use std::fs;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::nmcli_parse::{
    active_connection_of, active_entry, connected_devices, devices_of_type, profile_uuids,
    single_value,
};
use super::{Band, HotspotSettings, HotspotState, NetworkBackend, PROFILE_NAME, Security};
use crate::CoreError;
use crate::net::{cmd, uplink};
use crate::secret::Secret;

const NMCLI: &str = "nmcli";
const PSK: &str = "802-11-wireless-security.psk";
/// Сколько ждать активации (секунды).
const UP_WAIT: &str = "45";

// Коды выхода nmcli (man nmcli, «EXIT STATUS»).
const EXIT_NM_NOT_RUNNING: i32 = 8;
const EXIT_NOT_FOUND: i32 = 10;

#[derive(Debug, Default)]
pub struct NmcliBackend {
    /// Имя пользователя для `connection.permissions` (пусто — профиль общий).
    owner: String,
}

impl NmcliBackend {
    pub fn new() -> Self {
        // Имя — из системной базы пользователей по uid, не из окружения (его может не быть).
        let owner = nix::unistd::User::from_uid(nix::unistd::getuid())
            .ok()
            .flatten()
            .map(|u| u.name)
            .filter(|u| {
                !u.is_empty()
                    && u.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
            })
            .unwrap_or_default();
        NmcliBackend { owner }
    }

    fn nm(&self, args: &[&str]) -> Result<String, CoreError> {
        cmd::run(NMCLI, args).map_err(map_exit)
    }

    fn nm_secret(&self, args: &[&str]) -> Result<String, CoreError> {
        cmd::run_secret(NMCLI, args).map_err(map_exit)
    }

    fn uuids(&self) -> Result<Vec<String>, CoreError> {
        let out = self.nm(&["-t", "-f", "NAME,UUID,TYPE", "connection", "show"])?;
        Ok(profile_uuids(&out, PROFILE_NAME))
    }

    fn uuid(&self) -> Result<String, CoreError> {
        self.uuids()?.into_iter().next().ok_or(CoreError::NoProfile)
    }

    fn active(&self, uuids: &[String]) -> Result<Option<(String, String)>, CoreError> {
        let out = self.nm(&[
            "-t",
            "-f",
            "UUID,DEVICE,STATE",
            "connection",
            "show",
            "--active",
        ])?;
        Ok(active_entry(&out, uuids))
    }
}

fn map_exit(e: CoreError) -> CoreError {
    match e {
        CoreError::ToolFailed {
            code: EXIT_NM_NOT_RUNNING,
            ..
        } => CoreError::NmNotRunning,
        other => other,
    }
}

/// Пары «свойство значение» профиля (без пароля). Порядок важен только для читаемости.
/// `owner` — пользователь, которому принадлежит профиль (SECURITY.md §7); пусто — общий профиль.
pub fn profile_args(s: &HotspotSettings, owner: &str) -> Vec<String> {
    let yn = |b: bool| if b { "yes" } else { "no" };
    let (key_mgmt, pmf) = match s.security {
        Security::Wpa3 => ("sae", "required"),
        Security::Wpa2 => ("wpa-psk", "optional"),
    };
    let mut a: Vec<(&str, String)> = vec![
        ("connection.interface-name", s.ap_iface.clone()),
        ("connection.autoconnect", "no".into()),
        (
            "connection.permissions",
            if owner.is_empty() {
                String::new()
            } else {
                format!("user:{owner}")
            },
        ),
        ("802-11-wireless.mode", "ap".into()),
        ("802-11-wireless.ssid", s.ssid.clone()),
    ];
    let band = match s.band {
        Band::Ghz5 => "a",
        // Auto должен быть разрешён до вызова; на всякий случай — 2.4 ГГц.
        Band::Ghz2_4 | Band::Auto => "bg",
    };
    a.push(("802-11-wireless.band", band.into()));
    a.push((
        "802-11-wireless.channel",
        s.channel.map_or_else(|| "0".into(), |c| c.to_string()),
    ));
    // Ширина канала: 0 → auto (NM берёт самую узкую, так было до этапа 6).
    let width = match s.width_mhz {
        20 | 40 | 80 => format!("{}mhz", s.width_mhz),
        _ => "auto".into(),
    };
    a.push(("802-11-wireless.channel-width", width));
    a.extend([
        ("802-11-wireless.hidden", yn(s.hidden).into()),
        ("802-11-wireless.ap-isolation", yn(s.ap_isolation).into()),
        ("802-11-wireless-security.key-mgmt", key_mgmt.into()),
        ("802-11-wireless-security.proto", "rsn".into()),
        ("802-11-wireless-security.pairwise", "ccmp".into()),
        ("802-11-wireless-security.group", "ccmp".into()),
        ("802-11-wireless-security.pmf", pmf.into()),
        ("ipv4.method", "shared".into()),
        ("ipv4.addresses", s.subnet.to_string()),
        ("ipv6.method", "disabled".into()),
    ]);
    a.into_iter()
        .flat_map(|(k, v)| [k.to_string(), v])
        .collect()
}

/// Файл с временем включения: `$XDG_RUNTIME_DIR/omarchy-hotspot/since`.
fn since_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())?;
    Some(PathBuf::from(dir).join("omarchy-hotspot").join("since"))
}

fn write_since() {
    let Some(path) = since_path() else { return };
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let res = (|| -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        writeln!(f, "{secs}")
    })();
    if let Err(e) = res {
        tracing::warn!("cannot write start time: {e}");
    }
}

fn read_since() -> Option<SystemTime> {
    let secs: u64 = fs::read_to_string(since_path()?)
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(secs))
}

fn remove_since() {
    if let Some(p) = since_path() {
        let _ = fs::remove_file(&p);
        // Каталог удаляется, только если он пуст.
        if let Some(dir) = p.parent() {
            let _ = fs::remove_dir(dir);
        }
    }
}

impl NetworkBackend for NmcliBackend {
    fn profile_exists(&self) -> Result<bool, CoreError> {
        Ok(!self.uuids()?.is_empty())
    }

    fn ensure_profile(&self, s: &HotspotSettings) -> Result<(), CoreError> {
        if self.owner.is_empty() {
            // Общий профиль означал бы, что пароль читают все пользователи ПК (SECURITY.md §7).
            return Err(CoreError::Config("cannot determine the user name".into()));
        }
        let props = profile_args(s, &self.owner);
        let existing = self.uuids()?.into_iter().next();
        let mut args: Vec<&str> = match &existing {
            Some(u) => vec!["connection", "modify", "uuid", u],
            None => vec![
                "connection",
                "add",
                "type",
                "wifi",
                "con-name",
                PROFILE_NAME,
            ],
        };
        args.extend(props.iter().map(String::as_str));
        self.nm(&args)?;
        // Пароль — отдельно, по D-Bus: в аргументах nmcli он был бы виден в `ps`.
        if let Some(psk) = &s.password {
            self.set_password(psk)?;
        }
        Ok(())
    }

    fn set_password(&self, psk: &Secret) -> Result<(), CoreError> {
        let uuid = self.uuid()?;
        super::nm_dbus::set_psk(&uuid, psk)
    }

    fn get_password(&self) -> Result<Secret, CoreError> {
        let uuid = self.uuid()?;
        let out = self.nm_secret(&["-s", "-g", PSK, "connection", "show", "uuid", &uuid])?;
        let psk = Secret::new(single_value(&out));
        if psk.expose().is_empty() {
            return Err(CoreError::Parse {
                tool: NMCLI,
                reason: "password is empty or not readable".into(),
            });
        }
        Ok(psk)
    }

    fn up(&self) -> Result<(), CoreError> {
        let uuid = self.uuid()?;
        match self.nm(&["--wait", UP_WAIT, "connection", "up", "uuid", &uuid]) {
            Ok(_) => {
                write_since();
                Ok(())
            }
            Err(CoreError::ToolFailed { stderr, .. }) => Err(CoreError::ActivationFailed(stderr)),
            Err(e) => Err(e),
        }
    }

    fn down(&self) -> Result<(), CoreError> {
        let uuids = self.uuids()?;
        if let Some(uuid) = uuids.first()
            && self.active(&uuids)?.is_some()
        {
            match self.nm(&["connection", "down", "uuid", uuid]) {
                // Успело выключиться само — это не ошибка.
                Ok(_)
                | Err(CoreError::ToolFailed {
                    code: EXIT_NOT_FOUND,
                    ..
                }) => {}
                Err(e) => return Err(e),
            }
        }
        remove_since();
        Ok(())
    }

    fn delete_profile(&self) -> Result<(), CoreError> {
        for uuid in self.uuids()? {
            match self.nm(&["connection", "delete", "uuid", &uuid]) {
                Ok(_)
                | Err(CoreError::ToolFailed {
                    code: EXIT_NOT_FOUND,
                    ..
                }) => {}
                Err(e) => return Err(e),
            }
        }
        remove_since();
        Ok(())
    }

    fn state(&self) -> Result<HotspotState, CoreError> {
        let uuids = self.uuids()?;
        if uuids.is_empty() {
            return Ok(HotspotState::Off);
        }
        Ok(match self.active(&uuids)? {
            Some((dev, st)) if st == "activated" => HotspotState::On {
                since: read_since(),
                ap_iface: dev,
                uplink: uplink::detect_uplink().ok().flatten(),
            },
            Some((_, st)) if st == "activating" => HotspotState::Starting,
            _ => HotspotState::Off,
        })
    }

    fn wifi_ifaces(&self) -> Result<Vec<String>, CoreError> {
        let out = self.nm(&["-t", "-f", "DEVICE,TYPE", "device"])?;
        Ok(devices_of_type(&out, "wifi"))
    }

    fn client_connection(&self, iface: &str) -> Result<Option<String>, CoreError> {
        let out = self.nm(&["-t", "-f", "DEVICE,STATE,CONNECTION", "device", "status"])?;
        Ok(active_connection_of(&out, iface, PROFILE_NAME))
    }

    fn connected_ifaces(&self) -> Result<Vec<String>, CoreError> {
        let out = self.nm(&[
            "-t",
            "-f",
            "DEVICE,TYPE,STATE,CONNECTION",
            "device",
            "status",
        ])?;
        Ok(connected_devices(&out, PROFILE_NAME))
    }

    fn nm_running(&self) -> Result<bool, CoreError> {
        match cmd::run(NMCLI, &["-t", "-f", "RUNNING", "general"]) {
            Ok(o) => Ok(o.trim() == "running"),
            Err(CoreError::ToolFailed { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Ipv4Net;

    fn settings(security: Security, band: Band) -> HotspotSettings {
        HotspotSettings {
            ssid: "Test Net".into(),
            password: None,
            security,
            band,
            channel: Some(36),
            width_mhz: 0,
            hidden: false,
            ap_isolation: true,
            ap_iface: "wlp15s0".into(),
            subnet: Ipv4Net::parse("10.42.0.1/24").unwrap(),
        }
    }

    fn get<'a>(args: &'a [String], key: &str) -> &'a str {
        let i = args.iter().position(|a| a == key).expect(key);
        &args[i + 1]
    }

    #[test]
    fn wpa3_profile() {
        let a = profile_args(&settings(Security::Wpa3, Band::Ghz5), "alice");
        assert_eq!(a.len() % 2, 0);
        assert_eq!(get(&a, "802-11-wireless.mode"), "ap");
        assert_eq!(get(&a, "802-11-wireless.band"), "a");
        assert_eq!(get(&a, "802-11-wireless.channel"), "36");
        assert_eq!(get(&a, "802-11-wireless.channel-width"), "auto");
        assert_eq!(get(&a, "802-11-wireless.ap-isolation"), "yes");
        assert_eq!(get(&a, "802-11-wireless-security.key-mgmt"), "sae");
        assert_eq!(get(&a, "802-11-wireless-security.pmf"), "required");
        assert_eq!(get(&a, "802-11-wireless-security.proto"), "rsn");
        assert_eq!(get(&a, "802-11-wireless-security.pairwise"), "ccmp");
        assert_eq!(get(&a, "ipv4.method"), "shared");
        assert_eq!(get(&a, "ipv4.addresses"), "10.42.0.1/24");
        assert_eq!(get(&a, "connection.autoconnect"), "no");
        assert_eq!(get(&a, "connection.permissions"), "user:alice");
        assert!(!a.iter().any(|x| x.contains("psk")));
    }

    #[test]
    fn wpa2_profile() {
        let a = profile_args(&settings(Security::Wpa2, Band::Ghz2_4), "");
        assert_eq!(get(&a, "802-11-wireless.band"), "bg");
        assert_eq!(get(&a, "802-11-wireless-security.key-mgmt"), "wpa-psk");
        assert_eq!(get(&a, "802-11-wireless-security.pmf"), "optional");
        assert_eq!(get(&a, "connection.permissions"), "");
    }
}
