//! Маршруты IPv4 (`ip -j -4 route show table all`): чтобы подсеть раздачи не перекрыла LAN или VPN.
//! Иначе трафик ПК к адресам той сети ушёл бы гостям (любой гость может занять такой адрес).

use std::net::Ipv4Addr;

use crate::CoreError;
use crate::backend::Ipv4Net;
use crate::net::cmd;

/// Маршрут к сети через интерфейс.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub addr: Ipv4Addr,
    pub prefix: u8,
    pub dev: String,
}

impl std::fmt::Display for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{} ({})", self.addr, self.prefix, self.dev)
    }
}

/// Маршруты шире /8 — это «маршрут по умолчанию» по частям (VPN ставит 0.0.0.0/1 и 128.0.0.0/1),
/// более узкая подсеть раздачи их законно перекрывает.
const MIN_PREFIX: u8 = 8;

pub fn read_routes() -> Result<Vec<Route>, CoreError> {
    let json = cmd::run("ip", &["-j", "-4", "route", "show", "table", "all"])?;
    parse_routes(&json)
}

/// Обычные (unicast) маршруты и собственные адреса интерфейсов (`local`). У маршрута с несколькими
/// путями берутся интерфейсы всех путей (`nexthops`). Служебные `broadcast`/`anycast`/`multicast`,
/// маршруты без интерфейса (`blackhole`, `unreachable`) и `lo` не нужны.
pub fn parse_routes(json: &str) -> Result<Vec<Route>, CoreError> {
    let v: Vec<serde_json::Value> = serde_json::from_str(json).map_err(|e| CoreError::Parse {
        tool: "ip",
        reason: e.to_string(),
    })?;
    let str_of =
        |v: &serde_json::Value, key: &str| v.get(key).and_then(|x| x.as_str()).map(str::to_string);
    let mut out = Vec::new();
    for r in &v {
        let wanted = str_of(r, "type").is_none_or(|t| t == "unicast" || t == "local");
        let Some(dst) = str_of(r, "dst").filter(|_| wanted) else {
            continue;
        };
        let (a, p) = dst.split_once('/').unwrap_or((&dst, "32"));
        let (Ok(addr), Ok(prefix)) = (a.parse::<Ipv4Addr>(), p.parse::<u8>()) else {
            continue; // "default" и прочее не-адресное
        };
        if prefix > 32 {
            continue;
        }
        let devs: Vec<String> = match r.get("nexthops").and_then(|n| n.as_array()) {
            Some(hops) => hops.iter().filter_map(|h| str_of(h, "dev")).collect(),
            None => str_of(r, "dev").into_iter().collect(),
        };
        for dev in devs.into_iter().filter(|d| d != "lo") {
            out.push(Route { addr, prefix, dev });
        }
    }
    Ok(out)
}

/// Две сети пересекаются, если совпадают по общей (более короткой) маске.
fn overlaps(a: Ipv4Addr, a_prefix: u8, b: Ipv4Addr, b_prefix: u8) -> bool {
    let p = a_prefix.min(b_prefix);
    let mask = if p == 0 { 0 } else { u32::MAX << (32 - p) };
    u32::from(a) & mask == u32::from(b) & mask
}

/// Первый маршрут другого интерфейса, который пересекается с подсетью раздачи.
/// Маршруты самого адаптера точки доступа не считаются: это сама раздача (или подключение,
/// которое она заменит).
pub fn find_conflict<'a>(
    subnet: &Ipv4Net,
    routes: &'a [Route],
    ap_iface: &str,
) -> Option<&'a Route> {
    routes.iter().find(|r| {
        r.dev != ap_iface
            && r.prefix >= MIN_PREFIX
            && overlaps(subnet.addr, subnet.prefix, r.addr, r.prefix)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(s: &str) -> Ipv4Net {
        Ipv4Net::parse(s).unwrap()
    }

    #[test]
    fn parses_unicast_only() {
        let j = r#"[
            {"dst":"default","gateway":"192.168.1.1","dev":"enp14s0","protocol":"dhcp"},
            {"dst":"192.168.1.0/24","dev":"enp14s0","protocol":"kernel","scope":"link"},
            {"dst":"10.8.0.1","dev":"wg0"},
            {"type":"local","dst":"10.2.0.2","dev":"wg1","table":"local"},
            {"type":"local","dst":"127.0.0.1","dev":"lo","table":"local"},
            {"type":"broadcast","dst":"192.168.1.255","dev":"enp14s0","table":"local"},
            {"type":"blackhole","dst":"10.99.0.0/16"},
            {"dst":"172.20.0.0/16","nexthops":[{"gateway":"10.0.0.1","dev":"tun1"},{"gateway":"10.0.1.1","dev":"tun2"}]},
            {"dst":"10.0.0.0/33","dev":"x"}
        ]"#;
        let r: Vec<String> = parse_routes(j)
            .unwrap()
            .iter()
            .map(Route::to_string)
            .collect();
        assert_eq!(
            r,
            [
                "192.168.1.0/24 (enp14s0)",
                "10.8.0.1/32 (wg0)",
                "10.2.0.2/32 (wg1)",
                "172.20.0.0/16 (tun1)",
                "172.20.0.0/16 (tun2)",
            ]
        );
        assert!(parse_routes("oops").is_err());
    }

    #[test]
    fn conflicts() {
        let routes = parse_routes(
            r#"[
            {"dst":"192.168.1.0/24","dev":"enp14s0"},
            {"dst":"10.8.0.0/24","dev":"wg0"},
            {"dst":"0.0.0.0/1","dev":"tun0"},
            {"dst":"128.0.0.0/1","dev":"tun0"},
            {"dst":"10.42.0.0/24","dev":"wlp15s0"}
        ]"#,
        )
        .unwrap();
        let hit = |s: &str| find_conflict(&net(s), &routes, "wlp15s0").map(|r| r.dev.clone());
        // По умолчанию и «маршрут по умолчанию» VPN по частям — не мешают.
        assert_eq!(hit("10.42.0.1/24"), None);
        assert_eq!(hit("192.168.2.1/24"), None);
        // LAN и VPN — мешают, в обе стороны вложенности.
        assert_eq!(hit("192.168.1.77/24"), Some("enp14s0".into()));
        assert_eq!(hit("192.168.1.1/30"), Some("enp14s0".into()));
        assert_eq!(hit("192.168.0.1/16"), Some("enp14s0".into()));
        assert_eq!(hit("10.0.0.1/8"), Some("wg0".into()));
        // VPN «весь трафик»: к своей сети маршрута нет, есть только собственный адрес.
        let vpn = parse_routes(r#"[{"type":"local","dst":"10.2.0.2","dev":"wg1"}]"#).unwrap();
        assert!(find_conflict(&net("10.2.0.1/24"), &vpn, "wlp15s0").is_some());
        assert!(find_conflict(&net("10.3.0.1/24"), &vpn, "wlp15s0").is_none());
        // Маршрут самого адаптера точки доступа не считается.
        assert_eq!(
            find_conflict(&net("10.42.0.1/24"), &routes, "other"),
            routes.get(4)
        );
    }
}
