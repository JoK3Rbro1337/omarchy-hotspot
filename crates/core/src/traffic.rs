//! Скорость и общий объём трафика: считаются по разнице счётчиков байтов между опросами.
//! Счётчики берутся у станций (`iw station dump`), поэтому «за сеанс» — это трафик гостей,
//! а не всей машины.

use std::time::Instant;

use crate::Lang;
use crate::devices::Device;
use crate::helper_proto::Mac;

/// Сколько замеров храним для графика (при опросе раз в секунду — около двух минут).
pub const HISTORY_LEN: usize = 120;
/// Слишком частые замеры дают выброс на графике: разница в байтах делится почти на ноль.
const MIN_INTERVAL_SECS: f64 = 0.2;

/// Счётчики одного устройства с прошлого замера и его текущая скорость.
struct Seen {
    mac: Mac,
    down: u64,
    up: u64,
    down_bps: f64,
    up_bps: f64,
}

/// Считалка трафика за сеанс раздачи. Один экземпляр живёт, пока раздача включена.
#[derive(Default)]
pub struct TrafficMeter {
    seen: Vec<Seen>,
    last: Option<Instant>,
    /// Всего скачано гостями за сеанс.
    pub total_down: u64,
    /// Всего отдано гостями за сеанс.
    pub total_up: u64,
    pub down_bps: f64,
    pub up_bps: f64,
    history: Vec<u64>,
}

impl TrafficMeter {
    pub fn new() -> TrafficMeter {
        TrafficMeter::default()
    }

    /// Забыть всё: вызывается при выключении раздачи, иначе следующий сеанс начнётся не с нуля.
    pub fn reset(&mut self) {
        *self = TrafficMeter::default();
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_down.saturating_add(self.total_up)
    }

    /// Скорость одного устройства (байт/с): скачивание и отдача.
    pub fn rate_of(&self, mac: &Mac) -> (f64, f64) {
        self.seen
            .iter()
            .find(|s| &s.mac == mac)
            .map_or((0.0, 0.0), |s| (s.down_bps, s.up_bps))
    }

    /// Данные для графика: сумма скоростей ↓ и ↑ (байт/с) по замерам, слева — старые.
    pub fn history(&self) -> &[u64] {
        &self.history
    }

    /// Новый замер. `now` — время опроса (в тестах задаётся вручную).
    pub fn update(&mut self, devices: &[Device], now: Instant) {
        let elapsed = self.last.map(|last| (now - last).as_secs_f64());
        if elapsed.is_some_and(|s| s < MIN_INTERVAL_SECS) {
            return;
        }

        let (mut down_delta, mut up_delta) = (0u64, 0u64);
        for d in devices {
            let (down, up) = (d.down_bytes(), d.up_bytes());
            match self.seen.iter_mut().find(|s| s.mac == d.mac) {
                Some(prev) => {
                    // Счётчик меньше прежнего — устройство переподключилось и начало
                    // считать с нуля: тогда весь текущий счётчик и есть новые байты.
                    down_delta += if down < prev.down {
                        down
                    } else {
                        down - prev.down
                    };
                    up_delta += if up < prev.up { up } else { up - prev.up };
                    if let Some(secs) = elapsed {
                        prev.down_bps = (down.saturating_sub(prev.down)) as f64 / secs;
                        prev.up_bps = (up.saturating_sub(prev.up)) as f64 / secs;
                    }
                    prev.down = down;
                    prev.up = up;
                }
                None => {
                    down_delta += down;
                    up_delta += up;
                    self.seen.push(Seen {
                        mac: d.mac.clone(),
                        down,
                        up,
                        down_bps: 0.0,
                        up_bps: 0.0,
                    });
                }
            }
        }
        // Ушедшие устройства из списка убираем, но их байты уже в общем итоге.
        self.seen.retain(|s| devices.iter().any(|d| d.mac == s.mac));

        self.total_down = self.total_down.saturating_add(down_delta);
        self.total_up = self.total_up.saturating_add(up_delta);
        if let Some(secs) = elapsed {
            self.down_bps = down_delta as f64 / secs;
            self.up_bps = up_delta as f64 / secs;
            self.push_history((down_delta + up_delta) as f64 / secs);
        }
        self.last = Some(now);
    }

    fn push_history(&mut self, bps: f64) {
        if self.history.len() == HISTORY_LEN {
            self.history.remove(0);
        }
        self.history.push(bps.max(0.0) as u64);
    }
}

const UNITS_RU: [&str; 5] = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
const UNITS_EN: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

/// «1.4 МБ», «512 Б». Шаг 1024, как в файловых менеджерах.
pub fn format_bytes(bytes: u64, lang: Lang) -> String {
    let units = match lang {
        Lang::En => UNITS_EN,
        _ => UNITS_RU,
    };
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < units.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", units[0])
    } else if value < 10.0 {
        format!("{value:.1} {}", units[unit])
    } else {
        format!("{} {}", value.round() as u64, units[unit])
    }
}

/// «1.4 МБ/с». Скорость всегда с единицей, даже нулевая: так строка не «дёргается».
pub fn format_rate(bytes_per_sec: f64, lang: Lang) -> String {
    let suffix = match lang {
        Lang::En => "/s",
        _ => "/с",
    };
    let bytes = if bytes_per_sec > 0.0 {
        bytes_per_sec.round() as u64
    } else {
        0
    };
    format!("{}{suffix}", format_bytes(bytes, lang))
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::time::Duration;

    use super::*;
    use crate::devices::DeviceStatus;

    fn device(last: &str, down: u64, up: u64) -> Device {
        Device {
            mac: Mac::parse(&format!("3c:2e:f5:11:22:{last}")).unwrap(),
            ip: Some(Ipv4Addr::new(10, 42, 0, 34)),
            hostname: None,
            signal_dbm: Some(-50),
            rx_bytes: up,
            tx_bytes: down,
            connected_secs: 5,
            status: DeviceStatus::Allowed,
        }
    }

    #[test]
    fn counts_rate_and_total() {
        let t0 = Instant::now();
        let mut m = TrafficMeter::new();
        m.update(&[device("01", 1000, 500)], t0);
        // Первый замер: скорости ещё нет, но байты уже учтены.
        assert_eq!(m.total_down, 1000);
        assert_eq!(m.total_up, 500);
        assert_eq!(m.down_bps, 0.0);

        m.update(&[device("01", 3000, 1500)], t0 + Duration::from_secs(2));
        assert_eq!(m.total_down, 3000);
        assert_eq!(m.down_bps, 1000.0);
        assert_eq!(m.up_bps, 500.0);
        assert_eq!(m.history(), [1500]);
        assert_eq!(m.total_bytes(), 4500);
    }

    #[test]
    fn rate_per_device() {
        let t0 = Instant::now();
        let mut m = TrafficMeter::new();
        let mac = device("01", 0, 0).mac;
        m.update(&[device("01", 0, 0)], t0);
        m.update(&[device("01", 4000, 1000)], t0 + Duration::from_secs(2));
        assert_eq!(m.rate_of(&mac), (2000.0, 500.0));
        assert_eq!(m.rate_of(&device("09", 0, 0).mac), (0.0, 0.0));
    }

    #[test]
    fn device_left_keeps_its_bytes() {
        let t0 = Instant::now();
        let mut m = TrafficMeter::new();
        m.update(&[device("01", 1000, 0)], t0);
        m.update(&[], t0 + Duration::from_secs(1));
        assert_eq!(m.total_down, 1000);
        // Устройство вернулось со счётчиками с нуля — старый итог не теряется.
        m.update(&[device("01", 40, 0)], t0 + Duration::from_secs(2));
        assert_eq!(m.total_down, 1040);
    }

    #[test]
    fn counter_reset_is_not_negative() {
        let t0 = Instant::now();
        let mut m = TrafficMeter::new();
        m.update(&[device("01", 5000, 0)], t0);
        m.update(&[device("01", 100, 0)], t0 + Duration::from_secs(1));
        assert_eq!(m.total_down, 5100);
        assert_eq!(m.down_bps, 100.0);
    }

    #[test]
    fn too_frequent_updates_are_skipped() {
        let t0 = Instant::now();
        let mut m = TrafficMeter::new();
        m.update(&[device("01", 1000, 0)], t0);
        m.update(&[device("01", 9000, 0)], t0 + Duration::from_millis(10));
        assert_eq!(
            m.total_down, 1000,
            "слишком частый замер должен пропускаться"
        );
    }

    #[test]
    fn history_is_capped() {
        let t0 = Instant::now();
        let mut m = TrafficMeter::new();
        for i in 0..(HISTORY_LEN as u64 + 10) {
            m.update(&[device("01", i * 100, 0)], t0 + Duration::from_secs(i + 1));
        }
        assert_eq!(m.history().len(), HISTORY_LEN);
    }

    #[test]
    fn reset_clears_session() {
        let t0 = Instant::now();
        let mut m = TrafficMeter::new();
        m.update(&[device("01", 1000, 100)], t0);
        m.reset();
        assert_eq!(m.total_bytes(), 0);
        assert!(m.history().is_empty());
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format_bytes(512, Lang::Ru), "512 Б");
        assert_eq!(format_bytes(1536, Lang::Ru), "1.5 КБ");
        assert_eq!(format_bytes(15 * 1024 * 1024, Lang::Ru), "15 МБ");
        assert_eq!(format_bytes(1536, Lang::En), "1.5 KB");
        assert_eq!(format_rate(0.0, Lang::Ru), "0 Б/с");
        assert_eq!(format_rate(2_097_152.0, Lang::En), "2.0 MB/s");
    }
}
