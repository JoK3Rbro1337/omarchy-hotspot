//! Запись пароля в профиль NetworkManager по D-Bus (SECURITY.md §7): пароль не попадает
//! в аргументы процессов. Права — те же polkit-правила NM, что и у nmcli (без root).

use std::collections::HashMap;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::CoreError;
use crate::secret::Secret;

const NM: &str = "org.freedesktop.NetworkManager";
const SETTINGS_PATH: &str = "/org/freedesktop/NetworkManager/Settings";
const SETTINGS_IFACE: &str = "org.freedesktop.NetworkManager.Settings";
const CONN_IFACE: &str = "org.freedesktop.NetworkManager.Settings.Connection";
const SEC_GROUP: &str = "802-11-wireless-security";
/// Update2: флаг «сохранить на диск».
const UPDATE_TO_DISK: u32 = 0x1;

type Settings = HashMap<String, HashMap<String, OwnedValue>>;

fn dbus_err(e: zbus::Error) -> CoreError {
    // Текст ошибки D-Bus пароль не содержит: NM его не повторяет.
    CoreError::NmDbus(e.to_string())
}

/// Записать PSK в профиль с данным UUID. Остальные настройки не меняются.
pub fn set_psk(uuid: &str, psk: &Secret) -> Result<(), CoreError> {
    let conn = Connection::system().map_err(dbus_err)?;
    let settings = Proxy::new(&conn, NM, SETTINGS_PATH, SETTINGS_IFACE).map_err(dbus_err)?;
    let path: OwnedObjectPath = settings
        .call("GetConnectionByUuid", &(uuid,))
        .map_err(dbus_err)?;
    let profile = Proxy::new(&conn, NM, path.as_str(), CONN_IFACE).map_err(dbus_err)?;
    let mut current: Settings = profile.call("GetSettings", &()).map_err(dbus_err)?;
    let value = OwnedValue::try_from(Value::from(psk.expose()))
        .map_err(|e| CoreError::NmDbus(e.to_string()))?;
    current
        .entry(SEC_GROUP.to_string())
        .or_default()
        .insert("psk".to_string(), value);
    let args: HashMap<String, Value<'_>> = HashMap::new();
    let _reply: HashMap<String, OwnedValue> = profile
        .call("Update2", &(current, UPDATE_TO_DISK, args))
        .map_err(dbus_err)?;
    Ok(())
}
