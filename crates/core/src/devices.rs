//! Список подключённых устройств: сведение данных `iw station dump`, `ip neigh` и аренд dnsmasq.

use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::CoreError;
use crate::helper_proto::Mac;
use crate::net::iw::{self, Station};
use crate::net::leases::Lease;
use crate::net::neigh::{self, Neigh};

/// Состояние устройства в брандмауэре. Проставляет `access::AccessLists::annotate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceStatus {
    #[default]
    Allowed,
    /// Ждёт одобрения: подключено к Wi-Fi, но в интернет не выпущено.
    Pending,
    Blocked,
}

/// Одно устройство для показа в списке. Байты — со стороны точки доступа.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub mac: Mac,
    pub ip: Option<Ipv4Addr>,
    pub hostname: Option<String>,
    pub signal_dbm: Option<i32>,
    /// Принято от устройства (для гостя — «отдача», ↑).
    pub rx_bytes: u64,
    /// Отправлено устройству (для гостя — «скачивание», ↓).
    pub tx_bytes: u64,
    pub connected_secs: u64,
    pub status: DeviceStatus,
}

impl Device {
    /// Сколько устройство скачало (стрелка ↓ в окне).
    pub fn down_bytes(&self) -> u64 {
        self.tx_bytes
    }

    /// Сколько устройство отдало (стрелка ↑ в окне).
    pub fn up_bytes(&self) -> u64 {
        self.rx_bytes
    }

    /// Имя для показа: имя из аренды, иначе IP, иначе MAC.
    pub fn display_name(&self) -> String {
        match (&self.hostname, self.ip) {
            (Some(name), _) => name.clone(),
            (None, Some(ip)) => ip.to_string(),
            (None, None) => self.mac.as_str().to_string(),
        }
    }
}

/// Полоски уровня сигнала: сильнее −55 dBm — 4, −65 — 3, −75 — 2, слабее — 1 (PLAN, этап 4).
pub fn signal_bars(dbm: i32) -> u8 {
    match dbm {
        d if d > -55 => 4,
        d if d > -65 => 3,
        d if d > -75 => 2,
        _ => 1,
    }
}

/// Полоски строкой: заполненные — `▂▄▆█`, недостающие — пробелы, ширина всегда 4 символа.
pub fn bars_str(bars: u8) -> String {
    const FULL: [char; 4] = ['▂', '▄', '▆', '█'];
    let n = usize::from(bars).min(FULL.len());
    let mut s: String = FULL[..n].iter().collect();
    s.extend(std::iter::repeat_n(' ', FULL.len() - n));
    s
}

/// Сводит станции с IP (аренда важнее таблицы соседей) и именами.
/// Порядок: кто дольше подключён — выше; при равенстве по MAC, чтобы список не «прыгал».
pub fn merge(stations: &[Station], neigh: &[Neigh], leases: &[Lease]) -> Vec<Device> {
    let mut out: Vec<Device> = stations
        .iter()
        .map(|st| {
            let lease = leases.iter().find(|l| l.mac == st.mac);
            let ip = lease
                .map(|l| l.ip)
                .or_else(|| neigh.iter().find(|n| n.mac == st.mac).map(|n| n.ip));
            Device {
                mac: st.mac.clone(),
                ip,
                hostname: lease.and_then(|l| l.hostname.clone()),
                signal_dbm: st.signal_dbm,
                rx_bytes: st.rx_bytes,
                tx_bytes: st.tx_bytes,
                connected_secs: st.connected_secs,
                status: DeviceStatus::Allowed,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.connected_secs
            .cmp(&a.connected_secs)
            .then_with(|| a.mac.cmp(&b.mac))
    });
    out
}

/// Опрос системы: станции точки доступа + таблица соседей. Аренды передаются снаружи
/// (их читает помощник, и делаем мы это реже — см. `hotspot::Hotspot::leases`).
pub fn scan(ap_iface: &str, leases: &[Lease]) -> Result<Vec<Device>, CoreError> {
    let stations = iw::station_dump(ap_iface)?;
    // Без таблицы соседей список всё равно полезен (будут имена из аренд), поэтому не падаем.
    let neigh = neigh::neigh_table(ap_iface).unwrap_or_else(|e| {
        tracing::debug!("cannot read neighbour table: {e}");
        Vec::new()
    });
    Ok(merge(&stations, &neigh, leases))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac(last: &str) -> Mac {
        Mac::parse(&format!("3c:2e:f5:11:22:{last}")).unwrap()
    }

    fn station(last: &str, secs: u64) -> Station {
        Station {
            mac: mac(last),
            signal_dbm: Some(-50),
            rx_bytes: 100,
            tx_bytes: 200,
            connected_secs: secs,
            authorized: true,
        }
    }

    #[test]
    fn bars_by_signal() {
        assert_eq!(signal_bars(-40), 4);
        assert_eq!(signal_bars(-55), 3);
        assert_eq!(signal_bars(-56), 3);
        assert_eq!(signal_bars(-70), 2);
        assert_eq!(signal_bars(-90), 1);
        assert_eq!(bars_str(4), "▂▄▆█");
        assert_eq!(bars_str(2), "▂▄  ");
        assert_eq!(bars_str(0).chars().count(), 4);
    }

    #[test]
    fn merges_lease_and_neigh() {
        let st = [station("01", 10), station("02", 99)];
        let ng = [Neigh {
            mac: mac("01"),
            ip: Ipv4Addr::new(10, 42, 0, 34),
        }];
        let ls = [Lease {
            mac: mac("02"),
            ip: Ipv4Addr::new(10, 42, 0, 51),
            hostname: Some("Pixel-8".into()),
        }];
        let d = merge(&st, &ng, &ls);
        // Первым идёт тот, кто подключён дольше.
        assert_eq!(d[0].mac, mac("02"));
        assert_eq!(d[0].hostname.as_deref(), Some("Pixel-8"));
        assert_eq!(d[0].display_name(), "Pixel-8");
        assert_eq!(d[1].ip, Some(Ipv4Addr::new(10, 42, 0, 34)));
        // Без аренды имени нет — показываем IP.
        assert_eq!(d[1].display_name(), "10.42.0.34");
        assert_eq!(d[0].down_bytes(), 200);
        assert_eq!(d[0].up_bytes(), 100);
    }

    #[test]
    fn lease_ip_wins_over_neigh() {
        let st = [station("01", 1)];
        let ng = [Neigh {
            mac: mac("01"),
            ip: Ipv4Addr::new(10, 42, 0, 9),
        }];
        let ls = [Lease {
            mac: mac("01"),
            ip: Ipv4Addr::new(10, 42, 0, 34),
            hostname: None,
        }];
        let d = merge(&st, &ng, &ls);
        assert_eq!(d[0].ip, Some(Ipv4Addr::new(10, 42, 0, 34)));
        // Ни имени, ни аренды с именем — остаётся IP.
        assert_eq!(d[0].display_name(), "10.42.0.34");
    }

    #[test]
    fn no_stations_no_devices() {
        assert!(merge(&[], &[], &[]).is_empty());
    }
}
