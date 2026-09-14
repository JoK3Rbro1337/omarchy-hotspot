//! Модель продвинутого режима: разделы, параметры и черновик настроек.
//! Здесь нет отрисовки и ввода-вывода — только что показать и как изменить значение.
//! Черновик (`Advanced::draft`) правится стрелками и попадает в конфиг только по `s`.

use omarchy_hotspot_core::backend::{Band, Ipv4Net, Security};
use omarchy_hotspot_core::config::Config;
use omarchy_hotspot_core::doctor::Check;
use omarchy_hotspot_core::helper_proto::CountryCode;
use omarchy_hotspot_core::hotspot::{effective_width, parse_dns_list};
use omarchy_hotspot_core::net::iw::WifiCaps;
use omarchy_hotspot_core::{CoreError, Lang, Msg, t, tf};

use crate::hotspot_ctx::{band_name, security_name};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Radio,
    Security,
    Network,
    Automation,
    Devices,
    Expert,
}

impl Section {
    pub const ALL: [Section; 6] = [
        Section::Radio,
        Section::Security,
        Section::Network,
        Section::Automation,
        Section::Devices,
        Section::Expert,
    ];

    pub fn title(self) -> Msg {
        match self {
            Section::Radio => Msg::AdvSecRadio,
            Section::Security => Msg::AdvSecSecurity,
            Section::Network => Msg::AdvSecNetwork,
            Section::Automation => Msg::AdvSecAutomation,
            Section::Devices => Msg::AdvSecDevices,
            Section::Expert => Msg::AdvSecExpert,
        }
    }

    /// Значок Nerd Font.
    pub fn icon(self) -> &'static str {
        match self {
            Section::Radio => "󰖩",
            Section::Security => "󰒃",
            Section::Network => "󰛳",
            Section::Automation => "󰔟",
            Section::Devices => "󰀂",
            Section::Expert => "󰒓",
        }
    }

    pub fn hint(self) -> Msg {
        match self {
            Section::Radio => Msg::AdvHintSecRadio,
            Section::Security => Msg::AdvHintSecSecurity,
            Section::Network => Msg::AdvHintSecNetwork,
            Section::Automation => Msg::AdvHintSecAutomation,
            Section::Devices => Msg::AdvHintSecDevices,
            Section::Expert => Msg::AdvHintSecExpert,
        }
    }

    /// Параметры раздела. У «Устройств» своего списка параметров нет — там список устройств.
    pub fn items(self) -> &'static [Item] {
        use Item::*;
        match self {
            Section::Radio => &[Band, Channel, Width, Hidden, Country],
            Section::Security => &[
                Security,
                Pmf,
                Isolation,
                GuestsReachPc,
                Approval,
                Notifications,
                Blacklist,
                Password,
            ],
            Section::Network => &[Uplink, Subnet, Dns, Ipv6],
            Section::Automation => &[Autostart, IdleOff, Timer, CloseOnFocusLoss],
            Section::Devices => &[],
            Section::Expert => &[Log, Doctor, Reset],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Band,
    Channel,
    Width,
    Hidden,
    Country,
    Security,
    Pmf,
    Isolation,
    GuestsReachPc,
    Approval,
    Notifications,
    Blacklist,
    Password,
    Uplink,
    Subnet,
    Dns,
    Ipv6,
    Autostart,
    IdleOff,
    Timer,
    CloseOnFocusLoss,
    Log,
    Doctor,
    Reset,
}

/// Как меняется параметр.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// ←/→ перебирают варианты.
    Choice,
    /// ←/→ и Enter переключают вкл/выкл.
    Toggle,
    /// ←/→ перебирают частые варианты, Enter — ввести своё значение.
    ChoiceOrText,
    /// Enter выполняет действие.
    Action,
    /// Только показывается.
    ReadOnly,
}

impl Item {
    pub fn label(self) -> Msg {
        use Item::*;
        match self {
            Band => Msg::AdvItemBand,
            Channel => Msg::AdvItemChannel,
            Width => Msg::AdvItemWidth,
            Hidden => Msg::AdvItemHidden,
            Country => Msg::AdvItemCountry,
            Security => Msg::AdvItemSecurity,
            Pmf => Msg::AdvItemPmf,
            Isolation => Msg::AdvItemIsolation,
            GuestsReachPc => Msg::AdvItemGuestsReachPc,
            Approval => Msg::AdvItemApproval,
            Notifications => Msg::AdvItemNotifications,
            Blacklist => Msg::AdvItemBlacklist,
            Password => Msg::LabelPassword,
            Uplink => Msg::AdvItemUplink,
            Subnet => Msg::AdvItemSubnet,
            Dns => Msg::AdvItemDns,
            Ipv6 => Msg::AdvItemIpv6,
            Autostart => Msg::AdvItemAutostart,
            IdleOff => Msg::AdvItemIdleOff,
            Timer => Msg::AdvItemTimer,
            CloseOnFocusLoss => Msg::AdvItemCloseOnFocusLoss,
            Log => Msg::AdvItemLog,
            Doctor => Msg::AdvItemDoctor,
            Reset => Msg::AdvItemReset,
        }
    }

    pub fn hint(self) -> Msg {
        use Item::*;
        match self {
            Band => Msg::AdvHintBand,
            Channel => Msg::AdvHintChannel,
            Width => Msg::AdvHintWidth,
            Hidden => Msg::AdvHintHidden,
            Country => Msg::AdvHintCountry,
            Security => Msg::AdvHintSecurity,
            Pmf => Msg::AdvHintPmf,
            Isolation => Msg::AdvHintIsolation,
            GuestsReachPc => Msg::AdvHintGuestsReachPc,
            Approval => Msg::AdvHintApproval,
            Notifications => Msg::AdvHintNotifications,
            Blacklist => Msg::AdvHintBlacklist,
            Password => Msg::AdvHintPassword,
            Uplink => Msg::AdvHintUplink,
            Subnet => Msg::AdvHintSubnet,
            Dns => Msg::AdvHintDns,
            Ipv6 => Msg::AdvHintIpv6,
            Autostart => Msg::AdvHintAutostart,
            IdleOff => Msg::AdvHintIdleOff,
            Timer => Msg::AdvHintTimer,
            CloseOnFocusLoss => Msg::AdvHintCloseOnFocusLoss,
            Log => Msg::AdvHintLog,
            Doctor => Msg::AdvHintDoctor,
            Reset => Msg::AdvHintReset,
        }
    }

    pub fn kind(self) -> Kind {
        use Item::*;
        match self {
            Band | Channel | Width | Security | Uplink => Kind::Choice,
            Hidden | Isolation | GuestsReachPc | Approval | Notifications | Autostart
            | CloseOnFocusLoss => Kind::Toggle,
            Country | Subnet | Dns | IdleOff | Timer => Kind::ChoiceOrText,
            Blacklist | Password | Log | Doctor | Reset => Kind::Action,
            Pmf | Ipv6 => Kind::ReadOnly,
        }
    }
}

/// Сведения о системе для вариантов выбора (каналы карты, интерфейсы). Могут быть ещё не загружены.
#[derive(Debug, Default, Clone)]
pub struct Env {
    pub caps: Option<WifiCaps>,
    pub uplinks: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sections,
    Items,
}

/// Ввод своего значения (страна, подсеть, DNS, минуты).
pub struct TextInput {
    pub item: Item,
    pub value: String,
    pub error: Option<String>,
}

/// Что показывается под параметрами «Эксперта».
pub enum ExpertOutput {
    None,
    Loading,
    Log(Vec<String>),
    Doctor(Vec<Check>),
}

/// Состояние продвинутого режима.
pub struct Advanced {
    pub section: Section,
    pub focus: Focus,
    pub item_idx: usize,
    /// Черновик: все поля конфига, но сохраняются только поля продвинутого режима (`merge_into`).
    pub draft: Config,
    pub input: Option<TextInput>,
    pub env: Env,
    pub output: ExpertOutput,
    /// Прокрутка журнала: сколько строк от конца пропустить.
    pub log_scroll: usize,
    pub device_idx: usize,
    /// После сохранения закрыть окно (ответ «да» на «Сохранить изменения?» при выходе).
    pub quit_after_save: bool,
}

impl Advanced {
    pub fn new(cfg: &Config) -> Advanced {
        Advanced {
            section: Section::Radio,
            focus: Focus::Sections,
            item_idx: 0,
            draft: cfg.clone(),
            input: None,
            env: Env::default(),
            output: ExpertOutput::None,
            log_scroll: 0,
            device_idx: 0,
            quit_after_save: false,
        }
    }

    pub fn item(&self) -> Option<Item> {
        if self.focus != Focus::Items {
            return None;
        }
        self.section.items().get(self.item_idx).copied()
    }

    pub fn move_section(&mut self, down: bool) {
        let idx = Section::ALL
            .iter()
            .position(|s| *s == self.section)
            .unwrap_or(0);
        let n = Section::ALL.len();
        let next = if down {
            (idx + 1) % n
        } else {
            (idx + n - 1) % n
        };
        self.section = Section::ALL[next];
        self.item_idx = 0;
    }

    pub fn move_item(&mut self, down: bool) {
        let n = self.section.items().len();
        if n == 0 {
            return;
        }
        self.item_idx = if down {
            (self.item_idx + 1) % n
        } else {
            (self.item_idx + n - 1) % n
        };
    }

    /// Конфиг изменился не через черновик (окно `e`, служба, вопрос первого запуска):
    /// если несохранённых правок нет, черновик просто догоняет конфиг; иначе переносим
    /// только поля, которые пользователь в черновике не трогал.
    pub fn sync_from(&mut self, old: &Config, new: &Config) {
        let mut draft = new.clone();
        merge_changed(&mut draft, &self.draft, old);
        self.draft = draft;
    }
}

/// Поля продвинутого режима по разделам: сравнение и копирование.
fn copy_section(section: Section, to: &mut Config, from: &Config) {
    let (h, n, a, au) = (&from.hotspot, &from.network, &from.access, &from.automation);
    match section {
        Section::Radio => {
            to.hotspot.band = h.band;
            to.hotspot.channel = h.channel;
            to.hotspot.width_mhz = h.width_mhz;
            to.hotspot.hidden = h.hidden;
            to.hotspot.country = h.country.clone();
        }
        Section::Security => {
            to.hotspot.security = h.security;
            to.hotspot.ap_isolation = h.ap_isolation;
            to.hotspot.guests_can_reach_pc = h.guests_can_reach_pc;
            to.access.approval_required = a.approval_required;
            to.access.notifications = a.notifications;
        }
        Section::Network => {
            to.network.uplink_interface = n.uplink_interface.clone();
            to.network.subnet = n.subnet.clone();
            to.network.dns = n.dns.clone();
        }
        Section::Automation => {
            to.automation.autostart_on_login = au.autostart_on_login;
            to.automation.idle_off_minutes = au.idle_off_minutes;
            to.automation.timer_minutes = au.timer_minutes;
            to.ui.close_on_focus_loss = from.ui.close_on_focus_loss;
        }
        Section::Devices | Section::Expert => {}
    }
}

/// В разделе есть несохранённые изменения.
pub fn section_dirty(section: Section, draft: &Config, cfg: &Config) -> bool {
    let mut probe = cfg.clone();
    copy_section(section, &mut probe, draft);
    probe != *cfg
}

pub fn any_dirty(draft: &Config, cfg: &Config) -> bool {
    Section::ALL.iter().any(|s| section_dirty(*s, draft, cfg))
}

/// Перенести в `to` поля продвинутого режима, которые в черновике отличаются от `base`
/// (конфига, с которого начинали правку). Остальное — имя сети, списки устройств и поля,
/// которые пользователь не трогал, — остаётся как в `to`: их могли поменять служба или CLI.
pub fn merge_changed(to: &mut Config, draft: &Config, base: &Config) {
    macro_rules! take {
        ($($a:ident . $b:ident),* $(,)?) => {
            $(if draft.$a.$b != base.$a.$b {
                to.$a.$b = draft.$a.$b.clone();
            })*
        };
    }
    take!(
        hotspot.band,
        hotspot.channel,
        hotspot.width_mhz,
        hotspot.hidden,
        hotspot.country,
        hotspot.security,
        hotspot.ap_isolation,
        hotspot.guests_can_reach_pc,
        access.approval_required,
        access.notifications,
        network.uplink_interface,
        network.subnet,
        network.dns,
        automation.autostart_on_login,
        automation.idle_off_minutes,
        automation.timer_minutes,
        ui.close_on_focus_loss,
    );
}

/// Изменения, которые вступят в силу только после перезапуска раздачи.
pub fn restart_needed(draft: &Config, cfg: &Config) -> bool {
    let (d, c) = (&draft.hotspot, &cfg.hotspot);
    d.band != c.band
        || d.channel != c.channel
        || d.width_mhz != c.width_mhz
        || d.hidden != c.hidden
        || d.country != c.country
        || d.security != c.security
        || d.ap_isolation != c.ap_isolation
        || draft.network.uplink_interface != cfg.network.uplink_interface
        || draft.network.subnet != cfg.network.subnet
        || draft.network.dns != cfg.network.dns
}

/// Параметры, которые меняют профиль NetworkManager (остальное — страна, DNS, источник интернета).
pub fn profile_changed(draft: &Config, cfg: &Config) -> bool {
    let (d, c) = (&draft.hotspot, &cfg.hotspot);
    d.band != c.band
        || d.channel != c.channel
        || d.width_mhz != c.width_mhz
        || d.hidden != c.hidden
        || d.security != c.security
        || d.ap_isolation != c.ap_isolation
        || draft.network.subnet != cfg.network.subnet
}

/// Проверка черновика перед сохранением: то, что можно понять без NetworkManager.
pub fn validate(draft: &Config, env: &Env) -> Result<(), CoreError> {
    if draft.hotspot.security == Security::Wpa3 && env.caps.as_ref().is_some_and(|c| !c.sae) {
        return Err(CoreError::Wpa3Unsupported);
    }
    let subnet = &draft.network.subnet;
    if !Ipv4Net::parse(subnet).is_some_and(|n| n.is_private_host()) {
        return Err(CoreError::SubnetNotPrivate(subnet.clone()));
    }
    parse_dns_list(&draft.network.dns)?;
    let country = draft.hotspot.country.trim();
    if !country.is_empty() {
        CountryCode::parse(country).map_err(|e| CoreError::Config(format!("country: {e}")))?;
    }
    Ok(())
}

const COUNTRIES: [&str; 10] = ["", "UA", "PL", "DE", "CZ", "GB", "US", "FR", "NL", "KZ"];
const SUBNETS: [&str; 3] = ["10.42.0.1/24", "192.168.50.1/24", "172.20.0.1/24"];
const IDLE_MINUTES: [u32; 7] = [0, 5, 10, 15, 30, 60, 120];
const TIMER_MINUTES: [u32; 8] = [0, 15, 30, 60, 120, 180, 240, 480];
/// Частые DNS для гостей: пусто — как у ПК.
const DNS_PRESETS: [&[&str]; 5] = [
    &[],
    &["1.1.1.1", "1.0.0.1"],
    &["9.9.9.9", "149.112.112.112"],
    &["8.8.8.8", "8.8.4.4"],
    &["94.140.14.14", "94.140.15.15"],
];
/// Предел для минут простоя и таймера — сутки.
const MAX_MINUTES: u32 = 24 * 60;

/// Следующий (или предыдущий) вариант по кругу. Текущего нет в списке — начинаем с края.
fn cycle<T: PartialEq + Clone>(opts: &[T], cur: &T, forward: bool) -> T {
    let n = opts.len();
    let next = match opts.iter().position(|o| o == cur) {
        Some(i) if forward => (i + 1) % n,
        Some(i) => (i + n - 1) % n,
        None if forward => 0,
        None => n - 1,
    };
    opts[next].clone()
}

fn channels_for(band: Band, caps: Option<&WifiCaps>) -> Vec<u8> {
    let Some(caps) = caps else {
        return Vec::new();
    };
    match band {
        Band::Ghz2_4 => caps.channels_2ghz.clone(),
        Band::Ghz5 => caps.channels_5ghz.clone(),
        Band::Auto => caps
            .channels_2ghz
            .iter()
            .chain(&caps.channels_5ghz)
            .copied()
            .collect(),
    }
}

/// ←/→ (и Enter у переключателей): изменить параметр в черновике.
pub fn step(cfg: &mut Config, item: Item, forward: bool, env: &Env) {
    let h = &mut cfg.hotspot;
    match item {
        Item::Band => {
            let mut opts = vec![Band::Auto, Band::Ghz2_4];
            // Пока возможности карты не известны, 5 ГГц показываем: проверит сохранение.
            if env
                .caps
                .as_ref()
                .is_none_or(|c| !c.channels_5ghz.is_empty())
            {
                opts.push(Band::Ghz5);
            }
            h.band = cycle(&opts, &h.band, forward);
            // Канал другого диапазона и 80 МГц на 2.4 ГГц после смены диапазона не подходят.
            if h.channel != 0
                && env.caps.is_some()
                && !channels_for(h.band, env.caps.as_ref()).contains(&h.channel)
            {
                h.channel = 0;
            }
            if h.band == Band::Ghz2_4 && h.width_mhz == 80 {
                h.width_mhz = 40;
            }
        }
        Item::Channel => {
            let mut opts = vec![0];
            opts.extend(channels_for(h.band, env.caps.as_ref()));
            h.channel = cycle(&opts, &h.channel, forward);
        }
        Item::Width => {
            let mut opts = vec![20, 40];
            if h.band != Band::Ghz2_4 {
                opts.push(80);
            }
            let cur = if h.width_mhz == 0 { 20 } else { h.width_mhz };
            h.width_mhz = cycle(&opts, &cur, forward);
        }
        Item::Hidden => h.hidden = !h.hidden,
        Item::Country => {
            let mut opts: Vec<String> = COUNTRIES.iter().map(|s| s.to_string()).collect();
            if !opts.contains(&h.country) {
                opts.push(h.country.clone());
            }
            h.country = cycle(&opts, &h.country, forward);
        }
        Item::Security => {
            h.security = match h.security {
                Security::Wpa3 => Security::Wpa2,
                Security::Wpa2 => Security::Wpa3,
            }
        }
        Item::Isolation => h.ap_isolation = !h.ap_isolation,
        Item::GuestsReachPc => h.guests_can_reach_pc = !h.guests_can_reach_pc,
        Item::Approval => cfg.access.approval_required = !cfg.access.approval_required,
        Item::Notifications => cfg.access.notifications = !cfg.access.notifications,
        Item::CloseOnFocusLoss => cfg.ui.close_on_focus_loss = !cfg.ui.close_on_focus_loss,
        Item::Uplink => {
            let n = &mut cfg.network;
            let mut opts = vec![String::new()];
            opts.extend(env.uplinks.iter().cloned());
            if !opts.contains(&n.uplink_interface) {
                opts.push(n.uplink_interface.clone());
            }
            n.uplink_interface = cycle(&opts, &n.uplink_interface, forward);
        }
        Item::Subnet => {
            let n = &mut cfg.network;
            let mut opts: Vec<String> = SUBNETS.iter().map(|s| s.to_string()).collect();
            if !opts.contains(&n.subnet) {
                opts.push(n.subnet.clone());
            }
            n.subnet = cycle(&opts, &n.subnet, forward);
        }
        Item::Dns => {
            let n = &mut cfg.network;
            let mut opts: Vec<Vec<String>> = DNS_PRESETS
                .iter()
                .map(|p| p.iter().map(|s| s.to_string()).collect())
                .collect();
            if !opts.contains(&n.dns) {
                opts.push(n.dns.clone());
            }
            n.dns = cycle(&opts, &n.dns, forward);
        }
        Item::Autostart => {
            let a = &mut cfg.automation;
            a.autostart_on_login = !a.autostart_on_login;
        }
        Item::IdleOff => {
            let a = &mut cfg.automation;
            a.idle_off_minutes = cycle(&IDLE_MINUTES, &a.idle_off_minutes, forward);
        }
        Item::Timer => {
            let a = &mut cfg.automation;
            a.timer_minutes = cycle(&TIMER_MINUTES, &a.timer_minutes, forward);
        }
        Item::Pmf
        | Item::Ipv6
        | Item::Blacklist
        | Item::Password
        | Item::Log
        | Item::Doctor
        | Item::Reset => {}
    }
}

/// Начальное значение поля ввода.
pub fn text_value(cfg: &Config, item: Item) -> String {
    match item {
        Item::Country => cfg.hotspot.country.clone(),
        Item::Subnet => cfg.network.subnet.clone(),
        Item::Dns => cfg.network.dns.join(", "),
        Item::IdleOff => cfg.automation.idle_off_minutes.to_string(),
        Item::Timer => cfg.automation.timer_minutes.to_string(),
        _ => String::new(),
    }
}

/// Принять введённое значение. Ошибка — готовый текст для пользователя.
pub fn set_text(cfg: &mut Config, item: Item, text: &str, lang: Lang) -> Result<(), String> {
    let text = text.trim();
    let user = |e: CoreError| e.user_message(lang);
    match item {
        Item::Country => {
            if text.is_empty() {
                cfg.hotspot.country.clear();
                return Ok(());
            }
            let cc = CountryCode::parse(&text.to_ascii_uppercase())
                .map_err(|_| t(lang, Msg::AdvErrCountry).to_string())?;
            cfg.hotspot.country = cc.as_str().to_string();
        }
        Item::Subnet => {
            // «192.168.50.1» без маски — самая частая запись, считаем её /24.
            let full = if text.contains('/') {
                text.to_string()
            } else {
                format!("{text}/24")
            };
            if !Ipv4Net::parse(&full).is_some_and(|n| n.is_private_host()) {
                return Err(user(CoreError::SubnetNotPrivate(text.to_string())));
            }
            cfg.network.subnet = full;
        }
        Item::Dns => {
            let list: Vec<String> = text
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            parse_dns_list(&list).map_err(|_| t(lang, Msg::AdvErrDns).to_string())?;
            cfg.network.dns = list;
        }
        Item::IdleOff | Item::Timer => {
            let minutes: u32 = text
                .parse()
                .ok()
                .filter(|m| *m <= MAX_MINUTES)
                .ok_or_else(|| t(lang, Msg::AdvErrMinutes).to_string())?;
            if item == Item::IdleOff {
                cfg.automation.idle_off_minutes = minutes;
            } else {
                cfg.automation.timer_minutes = minutes;
            }
        }
        _ => {}
    }
    Ok(())
}

fn on_off(lang: Lang, on: bool) -> String {
    t(lang, if on { Msg::AdvOn } else { Msg::AdvOff }).to_string()
}

fn minutes_text(lang: Lang, minutes: u32, zero: Msg) -> String {
    if minutes == 0 {
        t(lang, zero).to_string()
    } else {
        tf(lang, Msg::AdvMinutesFmt, &[&minutes.to_string()])
    }
}

/// Значение параметра для показа. Пароль и чёрный список рисует окно (им нужно больше данных).
pub fn value_text(cfg: &Config, item: Item, lang: Lang) -> String {
    let h = &cfg.hotspot;
    match item {
        Item::Band => band_name(lang, h.band).to_string(),
        Item::Channel if h.channel == 0 => t(lang, Msg::AdvAuto).to_string(),
        Item::Channel => h.channel.to_string(),
        Item::Width => {
            let width = effective_width(if h.width_mhz == 0 { 20 } else { h.width_mhz }, h.band);
            tf(lang, Msg::AdvWidthFmt, &[&width.to_string()])
        }
        Item::Hidden => on_off(lang, h.hidden),
        Item::Country if h.country.is_empty() => t(lang, Msg::AdvCountryKeep).to_string(),
        Item::Country => h.country.clone(),
        Item::Security => security_name(lang, h.security).to_string(),
        Item::Pmf => t(
            lang,
            match h.security {
                Security::Wpa3 => Msg::AdvPmfRequired,
                Security::Wpa2 => Msg::AdvPmfOptional,
            },
        )
        .to_string(),
        Item::Isolation => on_off(lang, h.ap_isolation),
        Item::GuestsReachPc => t(
            lang,
            if h.guests_can_reach_pc {
                Msg::AdvGuestsAllowed
            } else {
                Msg::AdvGuestsDenied
            },
        )
        .to_string(),
        Item::Approval => on_off(lang, cfg.access.approval_required),
        Item::Notifications => on_off(lang, cfg.access.notifications),
        Item::Blacklist => tf(
            lang,
            Msg::AdvDevicesCountFmt,
            &[&cfg.access.blocked_macs.len().to_string()],
        ),
        Item::Uplink if cfg.network.uplink_interface.is_empty() => {
            t(lang, Msg::AdvAuto).to_string()
        }
        Item::Uplink => cfg.network.uplink_interface.clone(),
        Item::Subnet => cfg.network.subnet.clone(),
        Item::Dns if cfg.network.dns.is_empty() => t(lang, Msg::AdvDnsSystem).to_string(),
        Item::Dns => cfg.network.dns.join(", "),
        Item::Ipv6 => t(lang, Msg::AdvIpv6Off).to_string(),
        Item::Autostart => on_off(lang, cfg.automation.autostart_on_login),
        Item::CloseOnFocusLoss => on_off(lang, cfg.ui.close_on_focus_loss),
        Item::IdleOff => minutes_text(lang, cfg.automation.idle_off_minutes, Msg::AdvNever),
        Item::Timer => minutes_text(lang, cfg.automation.timer_minutes, Msg::AdvNoTimer),
        Item::Password | Item::Log | Item::Doctor | Item::Reset => String::new(),
    }
}

/// Значение переключателя (для зелёной/серой отметки).
pub fn toggle_state(cfg: &Config, item: Item) -> Option<bool> {
    match item {
        Item::Hidden => Some(cfg.hotspot.hidden),
        Item::Isolation => Some(cfg.hotspot.ap_isolation),
        Item::GuestsReachPc => Some(cfg.hotspot.guests_can_reach_pc),
        Item::Approval => Some(cfg.access.approval_required),
        Item::Notifications => Some(cfg.access.notifications),
        Item::Autostart => Some(cfg.automation.autostart_on_login),
        Item::CloseOnFocusLoss => Some(cfg.ui.close_on_focus_loss),
        _ => None,
    }
}

/// Параметр ослабляет защиту — выделяем жёлтым.
pub fn is_risky(cfg: &Config, item: Item) -> bool {
    match item {
        Item::Isolation => !cfg.hotspot.ap_isolation,
        Item::GuestsReachPc => cfg.hotspot.guests_can_reach_pc,
        Item::Approval => !cfg.access.approval_required,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> WifiCaps {
        WifiCaps {
            ap: true,
            sae: true,
            channels_2ghz: vec![1, 6, 11],
            channels_5ghz: vec![36, 40],
        }
    }

    fn env() -> Env {
        Env {
            caps: Some(caps()),
            uplinks: vec!["enp14s0".into(), "wg0".into()],
        }
    }

    #[test]
    fn every_section_item_has_texts_in_all_languages() {
        for s in Section::ALL {
            for l in [Lang::Ru, Lang::Uk, Lang::En] {
                assert!(!t(l, s.title()).is_empty());
                assert!(!t(l, s.hint()).is_empty());
                for i in s.items() {
                    assert!(!t(l, i.label()).is_empty(), "{i:?}");
                    assert!(!t(l, i.hint()).is_empty(), "{i:?}");
                }
            }
        }
    }

    #[test]
    fn arrows_cycle_through_options() {
        let mut c = Config::default();
        let e = env();
        step(&mut c, Item::Channel, true, &e);
        assert_eq!(c.hotspot.channel, 1);
        step(&mut c, Item::Channel, false, &e);
        step(&mut c, Item::Channel, false, &e);
        assert_eq!(c.hotspot.channel, 40, "назад с «авто» — последний канал");
        step(&mut c, Item::Uplink, true, &e);
        assert_eq!(c.network.uplink_interface, "enp14s0");
        step(&mut c, Item::Dns, true, &e);
        assert_eq!(c.network.dns, ["1.1.1.1", "1.0.0.1"]);
        step(&mut c, Item::Timer, false, &e);
        assert_eq!(c.automation.timer_minutes, 480);
    }

    #[test]
    fn band_change_drops_unfit_channel_and_width() {
        let mut c = Config::default();
        c.hotspot.band = Band::Ghz5;
        c.hotspot.channel = 36;
        c.hotspot.width_mhz = 80;
        let e = env();
        step(&mut c, Item::Band, true, &e); // 5 → авто (по кругу)
        assert_eq!(c.hotspot.band, Band::Auto);
        assert_eq!(c.hotspot.channel, 36, "в «авто» 36-й канал допустим");
        step(&mut c, Item::Band, true, &e); // авто → 2.4
        assert_eq!(c.hotspot.band, Band::Ghz2_4);
        assert_eq!(c.hotspot.channel, 0);
        assert_eq!(c.hotspot.width_mhz, 40);
        // На 2.4 ГГц варианта 80 МГц нет.
        step(&mut c, Item::Width, true, &e);
        step(&mut c, Item::Width, true, &e);
        assert_eq!(c.hotspot.width_mhz, 40);
    }

    #[test]
    fn five_ghz_is_hidden_when_card_has_no_channels() {
        let mut c = Config::default();
        let mut e = env();
        e.caps.as_mut().unwrap().channels_5ghz.clear();
        for _ in 0..4 {
            step(&mut c, Item::Band, true, &e);
            assert_ne!(c.hotspot.band, Band::Ghz5);
        }
    }

    #[test]
    fn text_input_is_checked() {
        let mut c = Config::default();
        let l = Lang::Ru;
        assert!(set_text(&mut c, Item::Country, "ua", l).is_ok());
        assert_eq!(c.hotspot.country, "UA");
        assert!(set_text(&mut c, Item::Country, "Ukraine", l).is_err());
        assert!(set_text(&mut c, Item::Subnet, "192.168.7.1", l).is_ok());
        assert_eq!(c.network.subnet, "192.168.7.1/24");
        assert!(set_text(&mut c, Item::Subnet, "8.8.8.8/24", l).is_err());
        assert_eq!(
            c.network.subnet, "192.168.7.1/24",
            "плохое значение записалось"
        );
        assert!(set_text(&mut c, Item::Dns, "1.1.1.1, 9.9.9.9", l).is_ok());
        assert_eq!(c.network.dns, ["1.1.1.1", "9.9.9.9"]);
        assert!(set_text(&mut c, Item::Dns, "1.1.1.1 1.1.1.1", l).is_err());
        assert!(set_text(&mut c, Item::Dns, "a b c d e", l).is_err());
        assert!(set_text(&mut c, Item::Dns, "", l).is_ok());
        assert!(c.network.dns.is_empty());
        assert!(set_text(&mut c, Item::Timer, "90", l).is_ok());
        assert_eq!(c.automation.timer_minutes, 90);
        assert!(set_text(&mut c, Item::IdleOff, "-5", l).is_err());
        assert!(set_text(&mut c, Item::IdleOff, "100000", l).is_err());
    }

    #[test]
    fn dirty_sections_merge_and_restart() {
        let cfg = Config::default();
        let mut draft = cfg.clone();
        assert!(!any_dirty(&draft, &cfg));
        draft.automation.idle_off_minutes = 10;
        assert!(section_dirty(Section::Automation, &draft, &cfg));
        assert!(!section_dirty(Section::Radio, &draft, &cfg));
        assert!(
            !restart_needed(&draft, &cfg),
            "таймер простоя не требует перезапуска"
        );
        draft.network.dns = vec!["1.1.1.1".into()];
        assert!(restart_needed(&draft, &cfg));
        assert!(!profile_changed(&draft, &cfg));
        // Имя сети и списки устройств из черновика не переносятся.
        draft.hotspot.ssid = "Stale".into();
        draft.access.blocked_macs = vec!["aa:bb:cc:dd:ee:ff".into()];
        let mut latest = cfg.clone();
        latest.hotspot.ssid = "Fresh".into();
        // Одобрение выключили из CLI, пока окно было открыто, — сохранение окна его не откатывает.
        latest.access.approval_required = false;
        merge_changed(&mut latest, &draft, &cfg);
        assert_eq!(latest.hotspot.ssid, "Fresh");
        assert!(latest.access.blocked_macs.is_empty());
        assert!(!latest.access.approval_required);
        assert_eq!(latest.automation.idle_off_minutes, 10);
    }

    #[test]
    fn outside_config_changes_keep_unsaved_edits() {
        let cfg = Config::default();
        let mut adv = Advanced::new(&cfg);
        adv.draft.hotspot.hidden = true;
        let mut new = cfg.clone();
        new.hotspot.ssid = "Renamed".into();
        new.access.approval_required = false;
        adv.draft.hotspot.ap_isolation = false; // тот же раздел, что и одобрение
        adv.sync_from(&cfg, &new);
        assert!(adv.draft.hotspot.hidden, "потерялась несохранённая правка");
        assert!(!adv.draft.hotspot.ap_isolation);
        assert!(!adv.draft.access.approval_required, "не догнали конфиг");
        assert_eq!(adv.draft.hotspot.ssid, "Renamed");
    }

    #[test]
    fn wpa3_without_sae_is_refused_on_save() {
        let mut e = env();
        e.caps.as_mut().unwrap().sae = false;
        let c = Config::default();
        assert!(matches!(validate(&c, &e), Err(CoreError::Wpa3Unsupported)));
        let mut c2 = c.clone();
        c2.hotspot.security = Security::Wpa2;
        assert!(validate(&c2, &e).is_ok());
    }
}
