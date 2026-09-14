//! Сценарии «включить / выключить / применить настройки / сбросить» поверх `NetworkBackend`.
//! Без ввода-вывода для пользователя: CLI и TUI показывают результат сами.

use crate::CoreError;
use crate::backend::{Band, HotspotSettings, HotspotState, Ipv4Net, NetworkBackend, Security};
use crate::config::Config;
use crate::helper::HelperRunner;
use crate::helper_proto::{CountryCode, DnsServer, HelperRequest, Iface, Mac};
use crate::net::iw::{self, WifiCaps};
use crate::net::leases::{self, Lease};
use crate::net::routes::{self, Route};
use crate::net::uplink;
use crate::oplock::{self, OpGuard};
use crate::password;
use crate::secret::Secret;
use crate::ssid::validate_ssid;

/// Сведения о системе, которые не относятся к NetworkManager (подменяются в тестах).
pub trait SystemProbe {
    fn wifi_caps(&self, iface: &str) -> Result<WifiCaps, CoreError>;
    fn uplink(&self) -> Result<Option<String>, CoreError>;
    /// Код страны Wi-Fi сейчас (`iw reg get`); `None` — не задан или не прочитался.
    fn reg_country(&self) -> Option<String>;
    /// Наш файл DNS для гостей существует (его пишет помощник, читать его root не нужен).
    fn dns_file_exists(&self) -> bool;
    /// Маршруты IPv4 системы (для проверки пересечения подсети с LAN/VPN).
    fn routes(&self) -> Result<Vec<Route>, CoreError>;
    /// Замок на вкл/выкл/применить/сброс между программами (`oplock`); в тестах — без замка.
    fn lock_ops(&self) -> Result<Option<OpGuard>, CoreError>;
}

/// Файл DNS для гостей (SECURITY.md §2.1, `dns-set`). Путь тот же, что у помощника.
pub const DNS_FILE: &str = "/etc/NetworkManager/dnsmasq-shared.d/omarchy-hotspot.conf";

#[derive(Debug, Default)]
pub struct RealProbe;

impl SystemProbe for RealProbe {
    fn wifi_caps(&self, iface: &str) -> Result<WifiCaps, CoreError> {
        iw::caps_for_iface(iface)
    }

    fn uplink(&self) -> Result<Option<String>, CoreError> {
        uplink::detect_uplink()
    }

    fn reg_country(&self) -> Option<String> {
        iw::reg_country().ok().flatten()
    }

    fn dns_file_exists(&self) -> bool {
        std::path::Path::new(DNS_FILE).exists()
    }

    fn routes(&self) -> Result<Vec<Route>, CoreError> {
        routes::read_routes()
    }

    fn lock_ops(&self) -> Result<Option<OpGuard>, CoreError> {
        oplock::acquire()
    }
}

/// Допустимая ширина канала для диапазона: 80 МГц только на 5 ГГц, неизвестное — авто (0).
pub fn effective_width(width_mhz: u16, band: Band) -> u16 {
    match (width_mhz, band) {
        (80, Band::Ghz2_4) => 40,
        (20 | 40 | 80, _) => width_mhz,
        _ => 0,
    }
}

/// Предпочтительные каналы 5 ГГц: без DFS во многих странах.
const PREFERRED_5GHZ: [u8; 4] = [36, 40, 44, 48];
const DEFAULT_2GHZ: u8 = 6;

/// Конкретные диапазон и канал. `channel = 0` — выбрать самим.
pub fn choose_band(band: Band, channel: u8, caps: &WifiCaps) -> Result<(Band, u8), CoreError> {
    if channel != 0 {
        let (b, list) = if channel <= 14 {
            (Band::Ghz2_4, &caps.channels_2ghz)
        } else {
            (Band::Ghz5, &caps.channels_5ghz)
        };
        if (band != Band::Auto && band != b) || !list.contains(&channel) {
            return Err(CoreError::ChannelUnavailable(channel));
        }
        return Ok((b, channel));
    }
    let pick5 = || {
        PREFERRED_5GHZ
            .into_iter()
            .find(|c| caps.channels_5ghz.contains(c))
            .or_else(|| caps.channels_5ghz.first().copied())
    };
    let pick2 = || {
        if caps.channels_2ghz.is_empty() || caps.channels_2ghz.contains(&DEFAULT_2GHZ) {
            DEFAULT_2GHZ
        } else {
            caps.channels_2ghz[0]
        }
    };
    match band {
        Band::Auto => Ok(pick5().map_or((Band::Ghz2_4, pick2()), |c| (Band::Ghz5, c))),
        Band::Ghz5 => pick5()
            .map(|c| (Band::Ghz5, c))
            .ok_or(CoreError::Band5Unavailable),
        Band::Ghz2_4 => Ok((Band::Ghz2_4, pick2())),
    }
}

#[derive(Debug)]
pub enum StartOutcome {
    AlreadyOn,
    Started {
        /// Настройки без пароля.
        settings: HotspotSettings,
        uplink: String,
        /// Профиль создан впервые (сгенерирован новый пароль).
        created: bool,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Профиля ещё нет: настройки применятся при первом включении.
    Saved,
    /// Профиль обновлён, раздача выключена.
    Updated,
    /// Раздача была включена и перезапущена с новыми настройками.
    Restarted,
}

pub struct Hotspot<'a> {
    pub backend: &'a dyn NetworkBackend,
    pub probe: &'a dyn SystemProbe,
    pub helper: &'a dyn HelperRunner,
}

impl<'a> Hotspot<'a> {
    pub fn new(
        backend: &'a dyn NetworkBackend,
        probe: &'a dyn SystemProbe,
        helper: &'a dyn HelperRunner,
    ) -> Self {
        Hotspot {
            backend,
            probe,
            helper,
        }
    }

    fn require_nm(&self) -> Result<(), CoreError> {
        if self.backend.nm_running()? {
            Ok(())
        } else {
            Err(CoreError::NmNotRunning)
        }
    }

    /// Wi-Fi-интерфейс для точки доступа и возможности его карты.
    fn pick_iface(&self, cfg: &Config) -> Result<(String, WifiCaps), CoreError> {
        let ifaces = self.backend.wifi_ifaces()?;
        if ifaces.is_empty() {
            return Err(CoreError::NoWifi);
        }
        let wanted = cfg.network.ap_interface.trim();
        if !wanted.is_empty() {
            if !ifaces.iter().any(|i| i == wanted) {
                return Err(CoreError::IfaceNotFound(wanted.to_string()));
            }
            let caps = self.probe.wifi_caps(wanted)?;
            return if caps.ap {
                Ok((wanted.to_string(), caps))
            } else {
                Err(CoreError::NoApSupport)
            };
        }
        for iface in ifaces {
            match self.probe.wifi_caps(&iface) {
                Ok(caps) if caps.ap => return Ok((iface, caps)),
                Ok(_) => {}
                Err(e) => tracing::warn!(%iface, "cannot read Wi-Fi capabilities: {e}"),
            }
        }
        Err(CoreError::NoApSupport)
    }

    /// Настройки профиля из конфига: проверки, выбор интерфейса, диапазона и канала.
    pub fn resolve(
        &self,
        cfg: &Config,
        password: Option<Secret>,
    ) -> Result<HotspotSettings, CoreError> {
        let h = &cfg.hotspot;
        validate_ssid(&h.ssid)?;
        if let Some(p) = &password {
            password::validate_user_password(p.expose())?;
        }
        let subnet = Ipv4Net::parse(&cfg.network.subnet)
            .filter(Ipv4Net::is_private_host)
            .ok_or_else(|| CoreError::SubnetNotPrivate(cfg.network.subnet.clone()))?;
        let (ap_iface, caps) = self.pick_iface(cfg)?;
        // Подсеть не должна перекрывать LAN/VPN: маршруты не прочитались — не включаем (SECURITY.md §5.1).
        if let Some(r) = routes::find_conflict(&subnet, &self.probe.routes()?, &ap_iface) {
            return Err(CoreError::SubnetConflict(subnet.to_string(), r.to_string()));
        }
        if h.security == Security::Wpa3 && !caps.sae {
            return Err(CoreError::Wpa3Unsupported);
        }
        let (band, channel) = choose_band(h.band, h.channel, &caps)?;
        Ok(HotspotSettings {
            ssid: h.ssid.clone(),
            password,
            security: h.security,
            band,
            channel: Some(channel),
            width_mhz: effective_width(h.width_mhz, band),
            hidden: h.hidden,
            ap_isolation: h.ap_isolation,
            ap_iface,
            subnet,
        })
    }

    pub fn state(&self) -> Result<HotspotState, CoreError> {
        self.require_nm()?;
        self.backend.state()
    }

    /// Включить. `approval` — режим одобрения устройств (белый список в брандмауэре, SECURITY.md §2.1);
    /// без фоновой службы (этап 5) его включать нельзя — некому одобрять.
    pub fn start(&self, cfg: &Config, approval: bool) -> Result<StartOutcome, CoreError> {
        // Проверка «уже включена?» и само включение — под одним замком (SECURITY.md §10).
        let _ops = self.probe.lock_ops()?;
        self.require_nm()?;
        if matches!(
            self.backend.state()?,
            HotspotState::On { .. } | HotspotState::Starting
        ) {
            return Ok(StartOutcome::AlreadyOn);
        }
        // Раздача без брандмауэра не запускается (SECURITY.md §4, §10).
        if !self.helper.installed() {
            return Err(CoreError::HelperNotInstalled);
        }
        // Сначала все проверки без изменений в системе (имя, пароль, подсеть, каналы, источник
        // интернета, правила): плохие настройки не должны успеть поменять страну или DNS.
        let checked = self.resolve(cfg, None)?;
        let uplink = self.uplink_for(cfg)?;
        if uplink == checked.ap_iface {
            return Err(CoreError::UplinkIsAp(uplink));
        }
        firewall_request(cfg, &checked.ap_iface, &uplink, approval)?;
        // Страна влияет на список разрешённых каналов — задаём её до окончательного выбора канала.
        // DNS гостей dnsmasq читает при запуске раздачи — файл должен быть готов до `up`.
        self.sync_country(cfg)?;
        self.sync_dns(cfg)?;
        let created = !self.backend.profile_exists()?;
        let password = if created {
            Some(password::generate()?)
        } else {
            None
        };
        let mut settings = self.resolve(cfg, password)?;

        let firewall = firewall_request(cfg, &settings.ap_iface, &uplink, approval)?;

        self.backend.ensure_profile(&settings)?;
        settings.password = None;
        if let Err(e) = self.backend.up() {
            // Тайм-аут `nmcli --wait` не отменяет включение: NM мог бы поднять раздачу позже,
            // уже без правил брандмауэра (SECURITY.md §10).
            let _ = self.backend.down();
            return Err(e);
        }

        // Защиту не включили — раздача без неё не остаётся (SECURITY.md §10). Часть правил
        // могла встать до ошибки (правила ufw переживают перезагрузку) — снимаем.
        if let Err(e) = self.helper.call(&firewall) {
            let _ = self.backend.down();
            let _ = self.helper.call(&HelperRequest::FwClear);
            return Err(e);
        }
        Ok(StartOutcome::Started {
            settings,
            uplink,
            created,
        })
    }

    fn uplink_for(&self, cfg: &Config) -> Result<String, CoreError> {
        match cfg.network.uplink_interface.trim() {
            "" => self.probe.uplink()?.ok_or(CoreError::NoUplink),
            // Имя из конфига — только из списка, который предлагает окно (подключённые, без мостов):
            // иначе опечатка или `docker0` пустили бы гостей в контейнеры вместо интернета.
            forced if self.uplink_candidates()?.iter().any(|c| c == forced) => {
                Ok(forced.to_string())
            }
            forced => Err(CoreError::Config(format!(
                "uplink_interface: {forced} is not a connected non-bridge interface"
            ))),
        }
    }

    /// Выключить. `Ok(true)` — раздача была включена.
    pub fn stop(&self) -> Result<bool, CoreError> {
        let _ops = self.probe.lock_ops()?;
        self.require_nm()?;
        let was_on = !matches!(self.backend.state()?, HotspotState::Off);
        let down = self.backend.down();
        // Выключить не удалось и раздача всё ещё работает — правила оставляем: раздача
        // без защиты не остаётся (SECURITY.md §10). Иначе снимаем их даже после ошибки
        // (ufw-правила переживают сбои).
        if down.is_err()
            && matches!(
                self.backend.state(),
                Ok(HotspotState::On { .. } | HotspotState::Starting)
            )
        {
            return down.map(|_| was_on);
        }
        let clear = self.helper_if_installed(&HelperRequest::FwClear);
        down?;
        clear?;
        Ok(was_on)
    }

    /// Вызвать помощника, если он установлен; иначе ничего не делать.
    fn helper_if_installed(&self, req: &HelperRequest) -> Result<(), CoreError> {
        if self.helper.installed() {
            self.helper.call(req)?;
        }
        Ok(())
    }

    /// Применить изменённый конфиг (и, если задан, новый пароль) к профилю NM.
    pub fn apply(&self, cfg: &Config, password: Option<Secret>) -> Result<ApplyOutcome, CoreError> {
        let _ops = self.probe.lock_ops()?;
        self.require_nm()?;
        let exists = self.backend.profile_exists()?;
        if !exists && password.is_none() {
            // Проверяем, что настройки вообще применимы, но профиль не создаём.
            self.resolve(cfg, None)?;
            return Ok(ApplyOutcome::Saved);
        }
        let settings = self.resolve(cfg, password)?;
        let state = self.backend.state()?;
        // Правила брандмауэра привязаны к адаптеру: на другой адаптер — только через выкл/вкл.
        if let HotspotState::On { ap_iface, .. } = &state
            && *ap_iface != settings.ap_iface
        {
            return Err(CoreError::AdapterChanged {
                running: ap_iface.clone(),
                wanted: settings.ap_iface,
            });
        }
        let was_on = !matches!(state, HotspotState::Off);
        self.backend.ensure_profile(&settings)?;
        if was_on {
            // Повторная активация применяет новые настройки; устройства переподключатся.
            self.backend.up()?;
            Ok(ApplyOutcome::Restarted)
        } else {
            Ok(ApplyOutcome::Updated)
        }
    }

    /// Задать код страны Wi-Fi из конфига, если он указан и отличается от текущего.
    /// Без помощника ничего не делаем: страна — улучшение, а не условие работы.
    pub fn sync_country(&self, cfg: &Config) -> Result<(), CoreError> {
        let wanted = cfg.hotspot.country.trim();
        if wanted.is_empty() || !self.helper.installed() {
            return Ok(());
        }
        let cc =
            CountryCode::parse(wanted).map_err(|e| CoreError::Config(format!("country: {e}")))?;
        if self.probe.reg_country().as_deref() == Some(cc.as_str()) {
            return Ok(());
        }
        self.helper
            .call(&HelperRequest::RegdomSet { country: cc })?;
        Ok(())
    }

    /// Привести файл DNS для гостей к конфигу: список задан — записать, пуст — удалить.
    /// Вступает в силу при следующем запуске раздачи.
    pub fn sync_dns(&self, cfg: &Config) -> Result<(), CoreError> {
        if !self.helper.installed() {
            return Ok(());
        }
        let servers = dns_servers(cfg)?;
        if servers.is_empty() {
            if self.probe.dns_file_exists() {
                self.helper.call(&HelperRequest::DnsClear)?;
            }
            return Ok(());
        }
        self.helper.call(&HelperRequest::DnsSet { servers })?;
        Ok(())
    }

    /// Wi-Fi-адаптер для раздачи и возможности его карты (каналы, WPA3) — для настроек.
    pub fn wifi_caps(&self, cfg: &Config) -> Result<(String, WifiCaps), CoreError> {
        self.pick_iface(cfg)
    }

    /// Интерфейсы, через которые сейчас может идти интернет (выбор источника в настройках).
    pub fn uplink_candidates(&self) -> Result<Vec<String>, CoreError> {
        self.require_nm()?;
        self.backend.connected_ifaces()
    }

    /// Пароль из NM; профиля нет → `None`.
    pub fn password(&self) -> Result<Option<Secret>, CoreError> {
        self.require_nm()?;
        if !self.backend.profile_exists()? {
            return Ok(None);
        }
        self.backend.get_password().map(Some)
    }

    /// Сеть, от которой отключится Wi-Fi-адаптер при включении раздачи (одна карта не может
    /// быть и точкой доступа, и клиентом). `None` — адаптер свободен.
    pub fn client_connection(&self, cfg: &Config) -> Result<Option<String>, CoreError> {
        let (iface, _) = self.pick_iface(cfg)?;
        self.backend.client_connection(&iface)
    }

    /// Аренды dnsmasq (имя и IP гостей) — их читает помощник: каталог доступен только root.
    /// Помощник не установлен — пустой список, а не ошибка: список устройств и без имён полезен.
    pub fn leases(&self, ap_iface: &str) -> Result<Vec<Lease>, CoreError> {
        if !self.helper.installed() {
            return Ok(Vec::new());
        }
        let ap = Iface::parse(ap_iface).map_err(|e| CoreError::Config(e.to_string()))?;
        let out = self.helper.call(&HelperRequest::LeasesRead { ap })?;
        Ok(leases::parse_leases(&out))
    }

    /// Выключить, удалить профиль и всё, что ставил помощник (DECISIONS D16). Повторный вызов безопасен.
    pub fn reset(&self) -> Result<(), CoreError> {
        // Сброс — последнее средство: если замок так и не освободился, сбрасываем без него.
        let _ops = self
            .probe
            .lock_ops()
            .inspect_err(|e| tracing::warn!("reset without the action lock: {e}"))
            .ok();
        // Все шаги выполняются независимо; возвращается первая ошибка. Шаги помощника
        // не зависят от NetworkManager: правила и файл DNS снимаем, даже если он не запущен.
        let nm = self.require_nm();
        let (down, delete) = match &nm {
            Ok(()) => (self.backend.down(), self.backend.delete_profile()),
            Err(_) => (Ok(()), Ok(())),
        };
        let steps = [
            nm,
            down,
            delete,
            self.helper_if_installed(&HelperRequest::FwClear),
            self.helper_if_installed(&HelperRequest::DnsClear),
        ];
        steps.into_iter().collect()
    }
}

/// DNS-серверы для гостей из конфига (с проверкой, SECURITY.md §2.2).
pub fn dns_servers(cfg: &Config) -> Result<Vec<DnsServer>, CoreError> {
    parse_dns_list(&cfg.network.dns)
}

/// Разобрать и проверить список DNS: адреса, без повторов, не больше четырёх.
pub fn parse_dns_list<S: AsRef<str>>(list: &[S]) -> Result<Vec<DnsServer>, CoreError> {
    let bad = |e: crate::helper_proto::ArgError| CoreError::Config(format!("dns: {e}"));
    let servers = list
        .iter()
        .map(|s| DnsServer::parse(s.as_ref().trim()).map_err(bad))
        .collect::<Result<Vec<_>, _>>()?;
    // IPv4 внутри IPv6 (`::ffff:127.0.0.1`) обходил бы проверку loopback — такие адреса не принимаем.
    if servers
        .iter()
        .any(|s| matches!(s.ip(), std::net::IpAddr::V6(v6) if v6.to_ipv4_mapped().is_some()))
    {
        return Err(CoreError::Config("dns: IPv4-mapped IPv6 address".into()));
    }
    // Пустой список — «как у ПК»; проверка помощника требует хотя бы один адрес.
    if !servers.is_empty() {
        crate::helper_proto::check_dns_list(&servers).map_err(bad)?;
    }
    Ok(servers)
}

/// Запрос `fw-apply` из конфига. Плохой MAC в конфиге — ошибка настроек.
pub fn firewall_request(
    cfg: &Config,
    ap: &str,
    uplink: &str,
    approval: bool,
) -> Result<HelperRequest, CoreError> {
    let iface = |s: &str| Iface::parse(s).map_err(|e| CoreError::Config(format!("{s}: {e}")));
    let macs = |list: &[String], key: &str| -> Result<Vec<Mac>, CoreError> {
        let mut out = Vec::with_capacity(list.len());
        for m in list {
            let mac = Mac::parse(m).map_err(|e| CoreError::Config(format!("{key}: {m}: {e}")))?;
            if !out.contains(&mac) {
                out.push(mac);
            }
        }
        Ok(out)
    };
    Ok(HelperRequest::FwApply {
        ap: iface(ap)?,
        uplink: iface(uplink)?,
        approval,
        guests_reach_pc: cfg.hotspot.guests_can_reach_pc,
        allow: if approval {
            macs(&cfg.access.allowed_macs, "access.allowed_macs")?
        } else {
            Vec::new()
        },
        block: macs(&cfg.access.blocked_macs, "access.blocked_macs")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::mock::MockBackend;
    use crate::helper::mock::MockHelper;

    struct FakeProbe {
        country: Option<String>,
        dns_file: bool,
        caps: WifiCaps,
        uplink: Option<String>,
        routes: Vec<Route>,
    }

    impl FakeProbe {
        fn good() -> Self {
            FakeProbe {
                caps: WifiCaps {
                    ap: true,
                    sae: true,
                    channels_2ghz: (1..=13).collect(),
                    channels_5ghz: vec![36, 40, 44, 48],
                },
                uplink: Some("enp14s0".into()),
                country: None,
                dns_file: false,
                routes: Vec::new(),
            }
        }
    }

    impl SystemProbe for FakeProbe {
        fn wifi_caps(&self, _iface: &str) -> Result<WifiCaps, CoreError> {
            Ok(self.caps.clone())
        }
        fn uplink(&self) -> Result<Option<String>, CoreError> {
            Ok(self.uplink.clone())
        }
        fn reg_country(&self) -> Option<String> {
            self.country.clone()
        }
        fn dns_file_exists(&self) -> bool {
            self.dns_file
        }
        fn routes(&self) -> Result<Vec<Route>, CoreError> {
            Ok(self.routes.clone())
        }
        fn lock_ops(&self) -> Result<Option<OpGuard>, CoreError> {
            Ok(None)
        }
    }

    fn cfg() -> Config {
        let mut c = Config::default();
        c.hotspot.ssid = "Test".into();
        c
    }

    #[test]
    fn band_choice() {
        let caps = FakeProbe::good().caps;
        assert_eq!(choose_band(Band::Auto, 0, &caps).unwrap(), (Band::Ghz5, 36));
        assert_eq!(
            choose_band(Band::Ghz2_4, 0, &caps).unwrap(),
            (Band::Ghz2_4, 6)
        );
        assert_eq!(
            choose_band(Band::Auto, 11, &caps).unwrap(),
            (Band::Ghz2_4, 11)
        );
        assert!(matches!(
            choose_band(Band::Ghz2_4, 36, &caps),
            Err(CoreError::ChannelUnavailable(36))
        ));
        assert!(matches!(
            choose_band(Band::Ghz5, 52, &caps),
            Err(CoreError::ChannelUnavailable(52))
        ));
        let no5 = WifiCaps {
            channels_5ghz: vec![],
            ..caps.clone()
        };
        assert_eq!(choose_band(Band::Auto, 0, &no5).unwrap(), (Band::Ghz2_4, 6));
        assert!(matches!(
            choose_band(Band::Ghz5, 0, &no5),
            Err(CoreError::Band5Unavailable)
        ));
        let only149 = WifiCaps {
            channels_5ghz: vec![149, 153],
            ..caps
        };
        assert_eq!(
            choose_band(Band::Auto, 0, &only149).unwrap(),
            (Band::Ghz5, 149)
        );
    }

    #[test]
    fn first_start_creates_profile_with_generated_password() {
        let b = MockBackend::new(&["wlp15s0"]);
        let p = FakeProbe::good();
        let hp = MockHelper::installed();
        let h = Hotspot::new(&b, &p, &hp);
        let out = h.start(&cfg(), false).unwrap();
        let StartOutcome::Started {
            settings,
            uplink,
            created,
        } = out
        else {
            panic!("expected Started");
        };
        assert!(created);
        assert_eq!(uplink, "enp14s0");
        assert!(settings.password.is_none());
        assert_eq!((settings.band, settings.channel), (Band::Ghz5, Some(36)));
        let psk = h.password().unwrap().unwrap();
        assert_eq!(psk.expose().len(), 16);
        assert!(matches!(h.state().unwrap(), HotspotState::On { .. }));
        assert!(matches!(
            h.start(&cfg(), false).unwrap(),
            StartOutcome::AlreadyOn
        ));
        assert_eq!(hp.names(), ["fw-apply"]);
        assert!(h.stop().unwrap());
        assert_eq!(hp.names(), ["fw-apply", "fw-clear"]);
        h.reset().unwrap();
        assert_eq!(
            hp.names(),
            ["fw-apply", "fw-clear", "fw-clear", "dns-clear"]
        );
    }

    #[test]
    fn firewall_request_from_config() {
        let mut c = cfg();
        c.access.blocked_macs = vec!["AA:BB:CC:DD:EE:01".into(), "aa:bb:cc:dd:ee:01".into()];
        c.access.allowed_macs = vec!["aa:bb:cc:dd:ee:02".into()];
        c.hotspot.guests_can_reach_pc = true;
        let HelperRequest::FwApply {
            approval,
            guests_reach_pc,
            allow,
            block,
            ..
        } = firewall_request(&c, "wlp15s0", "enp14s0", true).unwrap()
        else {
            panic!()
        };
        assert!(approval && guests_reach_pc);
        assert_eq!(allow.len(), 1);
        assert_eq!(block.len(), 1);
        // Без режима одобрения белый список не передаётся.
        let HelperRequest::FwApply { allow, .. } =
            firewall_request(&c, "wlp15s0", "enp14s0", false).unwrap()
        else {
            panic!()
        };
        assert!(allow.is_empty());
        c.access.blocked_macs = vec!["not-a-mac".into()];
        assert!(matches!(
            firewall_request(&c, "wlp15s0", "enp14s0", false),
            Err(CoreError::Config(_))
        ));
    }

    #[test]
    fn helper_failure_turns_hotspot_off() {
        let b = MockBackend::new(&["wlp15s0"]);
        let p = FakeProbe::good();
        let mut hp = MockHelper::installed();
        hp.fail = true;
        let h = Hotspot::new(&b, &p, &hp);
        assert!(matches!(
            h.start(&cfg(), false),
            Err(CoreError::HelperFailed(_))
        ));
        assert!(matches!(h.state().unwrap(), HotspotState::Off));
        // Часть правил могла встать до ошибки — снимаем всё.
        assert_eq!(hp.names(), ["fw-apply", "fw-clear"]);
    }

    #[test]
    fn forced_uplink_must_be_a_known_candidate() {
        let hp = MockHelper::installed();
        let b = MockBackend::new(&["wlp15s0"]);
        let p = FakeProbe::good();
        let h = Hotspot::new(&b, &p, &hp);
        let mut c = cfg();
        c.network.uplink_interface = "docker0".into();
        let err = h.start(&c, false).unwrap_err();
        assert!(matches!(err, CoreError::Config(_)), "{err:?}");
        assert!(hp.names().is_empty());
        c.network.uplink_interface = "enp14s0".into();
        h.start(&c, false).unwrap();
    }

    #[test]
    fn without_helper_refuses_to_start() {
        let b = MockBackend::new(&["wlp15s0"]);
        let p = FakeProbe::good();
        let hp = MockHelper::default();
        let h = Hotspot::new(&b, &p, &hp);
        assert!(matches!(
            h.start(&cfg(), false),
            Err(CoreError::HelperNotInstalled)
        ));
        assert!(matches!(h.state().unwrap(), HotspotState::Off));
        h.reset().unwrap();
    }

    #[test]
    fn second_start_keeps_password() {
        let b = MockBackend::new(&["wlp15s0"]);
        let p = FakeProbe::good();
        let hp = MockHelper::installed();
        let h = Hotspot::new(&b, &p, &hp);
        h.start(&cfg(), false).unwrap();
        let first = h.password().unwrap().unwrap();
        assert!(h.stop().unwrap());
        assert!(!h.stop().unwrap());
        let StartOutcome::Started { created, .. } = h.start(&cfg(), false).unwrap() else {
            panic!()
        };
        assert!(!created);
        assert_eq!(h.password().unwrap().unwrap(), first);
    }

    #[test]
    fn start_errors() {
        let hp = MockHelper::installed();
        let b = MockBackend::new(&["wlp15s0"]);
        let mut p = FakeProbe::good();
        p.uplink = None;
        assert!(matches!(
            Hotspot::new(&b, &p, &hp).start(&cfg(), false),
            Err(CoreError::NoUplink)
        ));
        p.uplink = Some("wlp15s0".into());
        assert!(matches!(
            Hotspot::new(&b, &p, &hp).start(&cfg(), false),
            Err(CoreError::UplinkIsAp(_))
        ));
        let mut p = FakeProbe::good();
        p.caps.sae = false;
        assert!(matches!(
            Hotspot::new(&b, &p, &hp).start(&cfg(), false),
            Err(CoreError::Wpa3Unsupported)
        ));
        let mut c = cfg();
        c.hotspot.security = Security::Wpa2;
        assert!(Hotspot::new(&b, &p, &hp).start(&c, false).is_ok());

        let mut p = FakeProbe::good();
        p.caps.ap = false;
        let b = MockBackend::new(&["wlp15s0"]);
        assert!(matches!(
            Hotspot::new(&b, &p, &hp).start(&cfg(), false),
            Err(CoreError::NoApSupport)
        ));
        let b = MockBackend::new(&[]);
        assert!(matches!(
            Hotspot::new(&b, &FakeProbe::good(), &hp).start(&cfg(), false),
            Err(CoreError::NoWifi)
        ));
        let b = MockBackend::new(&["wlp15s0"]);
        let mut c = cfg();
        c.network.ap_interface = "wlan9".into();
        assert!(matches!(
            Hotspot::new(&b, &FakeProbe::good(), &hp).start(&c, false),
            Err(CoreError::IfaceNotFound(_))
        ));
        b.with(|s| s.nm_running = false);
        assert!(matches!(
            Hotspot::new(&b, &FakeProbe::good(), &hp).start(&cfg(), false),
            Err(CoreError::NmNotRunning)
        ));
    }

    #[test]
    fn failed_activation_is_reported() {
        let hp = MockHelper::installed();
        let b = MockBackend::new(&["wlp15s0"]);
        b.with(|s| s.fail_up = true);
        let p = FakeProbe::good();
        assert!(matches!(
            Hotspot::new(&b, &p, &hp).start(&cfg(), false),
            Err(CoreError::ActivationFailed(_))
        ));
        // Включение могло продолжиться в NM — выключаем, правила не ставим.
        b.with(|s| assert_eq!(s.down_calls, 1));
        assert!(hp.names().is_empty());
    }

    #[test]
    fn apply_refuses_to_move_a_running_hotspot_to_another_adapter() {
        let hp = MockHelper::installed();
        let b = MockBackend::new(&["wlp15s0", "wlan1"]);
        let p = FakeProbe::good();
        let h = Hotspot::new(&b, &p, &hp);
        let mut c = cfg();
        h.start(&c, false).unwrap();
        c.network.ap_interface = "wlan1".into();
        let err = h.apply(&c, None).unwrap_err();
        assert!(matches!(err, CoreError::AdapterChanged { .. }), "{err:?}");
        b.with(|s| assert_eq!(s.profile.as_ref().unwrap().ap_iface, "wlp15s0"));
        // Выключенную — можно.
        h.stop().unwrap();
        assert_eq!(h.apply(&c, None).unwrap(), ApplyOutcome::Updated);
    }

    #[test]
    fn apply_settings() {
        let hp = MockHelper::installed();
        let b = MockBackend::new(&["wlp15s0"]);
        let p = FakeProbe::good();
        let h = Hotspot::new(&b, &p, &hp);
        let mut c = cfg();
        // Профиля нет — только сохраняем.
        assert_eq!(h.apply(&c, None).unwrap(), ApplyOutcome::Saved);
        assert!(!b.profile_exists().unwrap());
        // Свой пароль до первого включения — создаёт профиль.
        let psk = Secret::new("MyOwnPass12345");
        assert_eq!(
            h.apply(&c, Some(psk.clone())).unwrap(),
            ApplyOutcome::Updated
        );
        assert_eq!(h.password().unwrap().unwrap(), psk);
        // Включаем и меняем диапазон — перезапуск, пароль прежний.
        h.start(&c, false).unwrap();
        c.hotspot.band = Band::Ghz2_4;
        assert_eq!(h.apply(&c, None).unwrap(), ApplyOutcome::Restarted);
        b.with(|s| {
            assert_eq!(s.profile.as_ref().unwrap().band, Band::Ghz2_4);
            assert_eq!(s.up_calls, 2);
        });
        assert_eq!(h.password().unwrap().unwrap(), psk);
        // Плохой пароль и SSID не проходят.
        assert!(matches!(
            h.apply(&c, Some(Secret::new("short"))),
            Err(CoreError::Password(_))
        ));
        c.hotspot.ssid = String::new();
        assert!(matches!(h.apply(&c, None), Err(CoreError::Ssid(_))));
    }

    #[test]
    fn country_and_dns_are_set_before_start() {
        let hp = MockHelper::installed();
        let b = MockBackend::new(&["wlp15s0"]);
        let mut p = FakeProbe::good();
        p.country = Some("DE".into());
        let h = Hotspot::new(&b, &p, &hp);
        let mut c = cfg();
        c.hotspot.country = "UA".into();
        c.network.dns = vec!["1.1.1.1".into(), "9.9.9.9".into()];
        h.start(&c, false).unwrap();
        assert_eq!(hp.names(), ["regdom-set", "dns-set", "fw-apply"]);

        // Страна уже та, DNS не задан и файла нет — помощника лишний раз не зовём.
        let hp = MockHelper::installed();
        let b = MockBackend::new(&["wlp15s0"]);
        p.country = Some("UA".into());
        let h = Hotspot::new(&b, &p, &hp);
        c.network.dns.clear();
        h.start(&c, false).unwrap();
        assert_eq!(hp.names(), ["fw-apply"]);

        // DNS убрали из настроек, а файл остался — удаляем.
        p.dns_file = true;
        let hp = MockHelper::installed();
        let h = Hotspot::new(&b, &p, &hp);
        h.sync_dns(&c).unwrap();
        assert_eq!(hp.names(), ["dns-clear"]);
    }

    #[test]
    fn bad_country_dns_and_subnet_are_rejected() {
        let hp = MockHelper::installed();
        let b = MockBackend::new(&["wlp15s0"]);
        let p = FakeProbe::good();
        let h = Hotspot::new(&b, &p, &hp);
        let mut c = cfg();
        c.hotspot.country = "Ukraine".into();
        assert!(matches!(h.start(&c, false), Err(CoreError::Config(_))));
        c.hotspot.country.clear();
        c.network.dns = vec!["1.1.1.1".into(), "1.1.1.1".into()];
        assert!(matches!(h.start(&c, false), Err(CoreError::Config(_))));
        c.network.dns = vec!["not-an-ip".into()];
        assert!(matches!(h.start(&c, false), Err(CoreError::Config(_))));
        // DNS задан, но подсеть плохая — файл DNS не должен успеть записаться.
        c.network.dns = vec!["1.1.1.1".into()];
        for bad in [
            "8.8.8.1/24",
            "10.42.0.0/24",
            "10.42.0.255/24",
            "192.168.1.1/8",
        ] {
            c.network.subnet = bad.into();
            assert!(
                matches!(h.start(&c, false), Err(CoreError::SubnetNotPrivate(_))),
                "{bad} принят"
            );
        }
        assert!(
            hp.names().is_empty(),
            "помощника позвали при плохих настройках"
        );
        c.network.dns = vec!["::ffff:127.0.0.1".into()];
        c.network.subnet = "192.168.77.1/24".into();
        assert!(matches!(h.start(&c, false), Err(CoreError::Config(_))));
        c.network.dns.clear();
        h.start(&c, false).unwrap();
    }

    #[test]
    fn subnet_overlapping_lan_or_vpn_is_rejected() {
        let hp = MockHelper::installed();
        let b = MockBackend::new(&["wlp15s0"]);
        let mut p = FakeProbe::good();
        p.routes = routes::parse_routes(
            r#"[{"dst":"192.168.1.0/24","dev":"enp14s0"},{"dst":"10.42.0.0/24","dev":"wlp15s0"}]"#,
        )
        .unwrap();
        let h = Hotspot::new(&b, &p, &hp);
        let mut c = cfg();
        c.network.subnet = "192.168.1.200/24".into();
        let err = h.start(&c, false).unwrap_err();
        assert!(matches!(err, CoreError::SubnetConflict(..)), "{err:?}");
        assert!(
            err.user_message(crate::i18n::Lang::En)
                .contains("192.168.1.0/24 (enp14s0)")
        );
        assert!(hp.names().is_empty(), "помощника позвали при пересечении");
        // Маршрут самого адаптера точки доступа (раздача уже работала) — не пересечение.
        c.network.subnet = "10.42.0.1/24".into();
        h.start(&c, false).unwrap();
    }

    #[test]
    fn failed_stop_keeps_the_firewall_while_hotspot_runs() {
        let hp = MockHelper::installed();
        let b = MockBackend::new(&["wlp15s0"]);
        let p = FakeProbe::good();
        let h = Hotspot::new(&b, &p, &hp);
        h.start(&cfg(), false).unwrap();
        b.with(|s| s.fail_down = true);
        assert!(h.stop().is_err());
        assert_eq!(
            hp.names(),
            ["fw-apply"],
            "правила сняты с работающей раздачи"
        );
        h.stop().unwrap();
        assert_eq!(hp.names(), ["fw-apply", "fw-clear"]);
    }

    #[test]
    fn channel_width_fits_the_band() {
        assert_eq!(effective_width(80, Band::Ghz5), 80);
        assert_eq!(effective_width(80, Band::Ghz2_4), 40);
        assert_eq!(effective_width(40, Band::Ghz2_4), 40);
        assert_eq!(effective_width(0, Band::Ghz5), 0);
        assert_eq!(effective_width(160, Band::Ghz5), 0);
        let hp = MockHelper::installed();
        let b = MockBackend::new(&["wlp15s0"]);
        let p = FakeProbe::good();
        let h = Hotspot::new(&b, &p, &hp);
        let mut c = cfg();
        c.hotspot.band = Band::Ghz2_4;
        c.hotspot.width_mhz = 80;
        assert_eq!(h.resolve(&c, None).unwrap().width_mhz, 40);
    }

    #[test]
    fn reset_is_idempotent() {
        let hp = MockHelper::installed();
        let b = MockBackend::new(&["wlp15s0"]);
        let p = FakeProbe::good();
        let h = Hotspot::new(&b, &p, &hp);
        h.start(&cfg(), false).unwrap();
        h.reset().unwrap();
        assert!(!b.profile_exists().unwrap());
        assert_eq!(h.state().unwrap(), HotspotState::Off);
        assert!(h.password().unwrap().is_none());
        h.reset().unwrap();
    }
}
