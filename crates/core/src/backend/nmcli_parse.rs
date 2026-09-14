//! Чистые парсеры вывода `nmcli -t` (режим terse: поля через `:`, `:` и `\` внутри экранированы).

/// Делит строку terse-вывода на поля с учётом экранирования `\:` и `\\`.
pub fn split_terse(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    fields.last_mut().unwrap().push(n);
                }
            }
            ':' => fields.push(String::new()),
            _ => fields.last_mut().unwrap().push(c),
        }
    }
    fields
}

/// Имена устройств заданного типа из `nmcli -t -f DEVICE,TYPE device`.
pub fn devices_of_type(text: &str, ty: &str) -> Vec<String> {
    text.lines()
        .map(split_terse)
        .filter(|f| f.len() >= 2 && f[1] == ty)
        .map(|f| f[0].clone())
        .collect()
}

/// Имя профиля, которым интерфейс подключён сейчас, из
/// `nmcli -t -f DEVICE,STATE,CONNECTION device status`. Профиль раздачи не считается.
pub fn active_connection_of(text: &str, iface: &str, skip: &str) -> Option<String> {
    text.lines()
        .map(split_terse)
        .filter(|f| f.len() >= 3 && f[0] == iface && f[1].starts_with("connected"))
        .map(|f| f[2].clone())
        .find(|name| !name.is_empty() && name != skip)
}

/// Типы подключений, через которые идёт интернет. Мосты (docker0, virbr0) сюда не входят:
/// выбрать их источником значило бы пустить гостей в сеть контейнеров (SECURITY.md §3.1).
const UPLINK_TYPES: [&str; 11] = [
    "ethernet",
    "wifi",
    "wireguard",
    "tun",
    "ppp",
    "gsm",
    "cdma",
    "bond",
    "vlan",
    "team",
    "adsl",
];

/// Подключённые устройства, годные в источники интернета, из
/// `nmcli -t -f DEVICE,TYPE,STATE,CONNECTION device status`, кроме интерфейса с профилем `skip`.
pub fn connected_devices(text: &str, skip: &str) -> Vec<String> {
    text.lines()
        .map(split_terse)
        .filter(|f| f.len() >= 4 && f[2].starts_with("connected") && f[3] != skip)
        .filter(|f| UPLINK_TYPES.contains(&f[1].as_str()) && !f[0].is_empty())
        .map(|f| f[0].clone())
        .collect()
}

/// Значение одного поля из `nmcli -g <поле>`: убираем ровно один завершающий `\n`
/// (пробелы в конце могут быть частью пароля) и снимаем экранирование.
pub fn single_value(out: &str) -> String {
    let line = out.strip_suffix('\n').unwrap_or(out);
    split_terse(line).join(":")
}

/// UUID профилей с именем `name` и типом Wi-Fi из `nmcli -t -f NAME,UUID,TYPE connection show`.
pub fn profile_uuids(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .map(split_terse)
        .filter(|f| f.len() >= 3 && f[0] == name && f[2] == "802-11-wireless")
        .map(|f| f[1].clone())
        .collect()
}

/// Активное подключение по UUID из `nmcli -t -f UUID,DEVICE,STATE connection show --active`:
/// (устройство, состояние: activating | activated | deactivating).
pub fn active_entry(text: &str, uuids: &[String]) -> Option<(String, String)> {
    text.lines()
        .map(split_terse)
        .find(|f| f.len() >= 3 && uuids.contains(&f[0]))
        .map(|f| (f[1].clone(), f[2].clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connected_devices_skip_loopback_and_hotspot() {
        let text = "enp14s0:ethernet:connected:Wired 1\nwlp15s0:wifi:connected:omarchy-hotspot\n\
                    lo:loopback:connected (externally):lo\nwg0:wireguard:connected:wg0\n\
                    docker0:bridge:connected (externally):docker0\n";
        assert_eq!(
            connected_devices(text, "omarchy-hotspot"),
            ["enp14s0", "wg0"]
        );
    }

    #[test]
    fn splits_with_escapes() {
        assert_eq!(split_terse("a:b\\:c:d\\\\"), vec!["a", "b:c", "d\\"]);
        assert_eq!(split_terse(""), vec![""]);
    }

    #[test]
    fn filters_wifi() {
        let out = "enp14s0:ethernet\nlo:loopback\nwlp15s0:wifi\np2p-dev-wlp15s0:wifi-p2p\n";
        assert_eq!(devices_of_type(out, "wifi"), vec!["wlp15s0"]);
    }

    #[test]
    fn finds_client_connection() {
        let out = "enp14s0:connected:Проводное 1\n\
                   wlp15s0:connected:Home\\:Net\n\
                   p2p-dev-wlp15s0:disconnected:\n";
        assert_eq!(
            active_connection_of(out, "wlp15s0", "omarchy-hotspot").as_deref(),
            Some("Home:Net")
        );
        // Раздача — это не «чужое» подключение, предупреждать не о чем.
        let ours = "wlp15s0:connected:omarchy-hotspot\n";
        assert_eq!(
            active_connection_of(ours, "wlp15s0", "omarchy-hotspot"),
            None
        );
        let off = "wlp15s0:disconnected:\n";
        assert_eq!(
            active_connection_of(off, "wlp15s0", "omarchy-hotspot"),
            None
        );
    }

    #[test]
    fn single_value_unescapes_and_keeps_spaces() {
        assert_eq!(single_value("pa\\:ss\\\\wo;rd\"x \n"), "pa:ss\\wo;rd\"x ");
        assert_eq!(single_value(""), "");
    }

    #[test]
    fn finds_profile_and_active_state() {
        let list = "Проводное подключение 1:eae0:802-3-ethernet\n\
                    omarchy-hotspot:7a59:802-11-wireless\n\
                    omarchy-hotspot:dead:802-3-ethernet\n";
        let uuids = profile_uuids(list, "omarchy-hotspot");
        assert_eq!(uuids, vec!["7a59"]);
        let active = "eae0:enp14s0:activated\n7a59:wlp15s0:activating\n";
        assert_eq!(
            active_entry(active, &uuids),
            Some(("wlp15s0".into(), "activating".into()))
        );
        assert_eq!(active_entry("eae0:enp14s0:activated\n", &uuids), None);
    }
}
