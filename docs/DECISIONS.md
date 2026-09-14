# Решения (этап 0)

Документ-проект. По нему пишутся этапы 1–9 без новых архитектурных решений.
Если что-то здесь противоречит плану разработки (`PLAN.md`, не публикуется) — верен этот документ (он написан позже, по фактам).

## 1. Факты о системе (11.09.2026)

### Железо и сеть
- Wi-Fi: MediaTek MT7921K, `mt7921e`, интерфейс `wlp15s0`. Режимы: managed, AP, AP/VLAN, monitor, P2P.
  Комбинации: `AP` ≤ 1 одновременно с `managed` ≤ 2, но **один канал** на всех. Раздача с одновременным
  подключением к чужому Wi-Fi возможна только на том же канале — не поддерживаем, источник интернета
  должен быть не-Wi-Fi (кабель, USB-модем, VPN-туннель).
- SAE (WPA3) поддерживается («Device supports SAE with AUTHENTICATE command» — SAE считает
  wpa_supplicant, это нормально). Шифры: CCMP-128/256, GCMP, CMAC (PMF). TKIP есть в списке, но
  **в режиме AP смешанный WPA/WPA2 с TKIP не работает**: телефон видит сеть, но не проходит
  подключение (тест 1). Чистый RSN/CCMP — работает.
- Регуляторный домен: `UA` (DFS-ETSI). 2.4 ГГц каналы 1–13, 5 ГГц без DFS: 36–48.
- Источник интернета: `enp14s0` (кабель, DHCP). Его никогда не трогаем.
- NetworkManager 1.58.1, iwd и systemd-networkd выключены. Профили: «Проводное подключение 1», `lo`.
- Пользователь в группе `wheel`. Polkit-правило NM (`/usr/share/polkit-1/rules.d/org.freedesktop.NetworkManager.rules`)
  даёт `settings.modify.system` без пароля для `wheel` + локальной сессии. Проверено: профиль
  создаётся, включается и удаляется **без пароля и без root**.
- Пакеты: `dnsmasq 2.93` (установлен на этапе 0, служба не включена — NM запускает свой экземпляр),
  `nftables 1.1.7`, `iw 6.17`, `wireless-regdb`, `libnotify`, `iptables-nft`, `polkit 127`.
- `wifi.powersave = 2` (выключен) уже задан Omarchy в `/etc/NetworkManager/conf.d/omarchy-wifi-powersave.conf`.

### Живые тесты раздачи (телефон Android)
| Тест | Настройки | Итог |
|------|-----------|------|
| 1 | `key-mgmt wpa-psk`, proto/pairwise пустые (WPA+WPA2, TKIP+CCMP), 2.4 ГГц кан. 11 | «Сохранено / ошибка подключения», в журнале нет AP-STA-CONNECTED |
| 2 | `proto rsn pairwise ccmp group ccmp`, кан. 6 | Wi-Fi подключается; «Ошибка конфигурации IP» — ufw режет DHCP/DNS/FORWARD |
| 3 | то же + 3 правила ufw (см. §8) | Интернет есть |
| 4 | `key-mgmt sae pmf 3` (WPA3, PMF обязателен), 2.4 ГГц | Интернет есть, телефон показывает WPA3-Personal |
| 5 | то же, `band a channel 36` (5 ГГц) | Интернет есть |

Что делает NM в режиме `ipv4.method shared` (по факту):
- адрес AP `10.42.0.1/24`, dnsmasq `--listen-address=10.42.0.1`, файл аренд
  `/var/lib/NetworkManager/dnsmasq-<iface>.leases` (каталог `0700 root` — **читается только через помощника**);
  формат строки: `<epoch-истечения> <mac> <ip> <hostname|*> <client-id|*>`;
- своя nft-таблица `ip nm-shared-<iface>`: `masquerade` для `10.42.0.0/24` и chain `forward` (policy accept,
  reject всего чужого). NAT и пересылку делает NM — **мы не дублируем**;
- ставит `net.ipv4.conf.<iface>.forwarding=1` на AP и uplink (не глобальный `ip_forward`).
- Без root доступно: `iw dev <ap> station dump` (MAC, сигнал, байты, время, `authorized`),
  `ip -4 neigh show dev <ap>` (MAC → IP). Имя устройства — только из аренд (root).

### Брандмауэр
- ufw активен: INPUT deny, OUTPUT allow, FORWARD (routed) **disabled → policy drop**. Есть связка
  ufw-docker (`DOCKER-USER → ufw-user-forward`). Правила пользователя: 53317/tcp+udp (LocalSend), docker-dns.
- Таблицы nft: `ip filter`, `ip6 filter` (обе — iptables-nft, «do not touch»). Своей `ip nat` нет.
- Из-за ufw для раздачи нужны три разрешения (§8). NM-таблица `accept` не отменяет `drop` в `ip filter`.

### Omarchy 4.0.0.alpha (важно: сильно отличается от 3.x)
- Установлен в `/usr/share/omarchy` (не `~/.local/share/omarchy`). Правило «не править» — то же.
- **Impala и waybar отсутствуют.** Панель, меню, уведомления, polkit-агент — это `omarchy-shell` (Quickshell/QML)
  с плагинами: `/usr/share/omarchy/shell/plugins/*` (виды: `bar-widget`, `panel`, `service`, `menu`),
  пользовательские — `~/.config/omarchy/plugins/<id>/manifest.json` (`omarchy-plugin-list/add/validate`).
  Сеть — встроенный виджет `omarchy.network` (без функции раздачи), есть панель `omarchy.wifiqr`.
- Меню: официальная точка расширения `~/.config/omarchy/extensions/omarchy-menu.jsonc` (JSONC, ключи —
  точечные id, поля `icon/label/action/when/checked`). Раздел сети: `setup.network.*`.
- Запуск TUI: `omarchy-launch-or-focus-tui <cmd>` → `omarchy-launch-tui` →
  `setsid uwsm-app -- xdg-terminal-exec --app-id=org.omarchy.<cmd> -e <cmd>`. Терминал пользователя — `foot`.
  Фокус существующего окна — по классу/заголовку через `hyprctl clients -j`.
- Hyprland 0.56.2, конфиг на **Lua**: `~/.config/hypr/hyprland.lua` подключает `default/hypr/*.lua`.
  Правила окон: глобальный помощник `o.window(<class-regex | {match}>, {rules})`. Плавающие TUI Omarchy
  получают тег `+floating-window` (float, center, 875×600) по regex классов
  `(org.omarchy.btop|org.omarchy.terminal|…|TUI.float|…)` — наш класс `org.omarchy.omarchy-hotspot` туда не попадает.
- Уведомления: `notify-send` (libnotify) и `omarchy-notification-send`. Polkit-агент: `omarchy.polkit` (в shell) —
  `pkexec` покажет графический запрос пароля.
- `omarchy-network-status` — скрипт, дающий uplink через `ip route get 1.1.1.1` (берём тот же подход).
- Хуки: `~/.config/omarchy/hooks/{post-update.d,theme-set.d,...}` — на этапе 7 можно поставить хук `post-update.d`
  для проверки, что наши вставки уцелели.

## 2. Ключевые решения

| # | Решение | Почему |
|---|---------|--------|
| D1 | Бэкенд раздачи — NetworkManager (`nmcli -t`, `LANG=C`), профиль `omarchy-hotspot`, `ipv4.method shared`. | Проверено вживую; NM сам делает dnsmasq, NAT, forwarding, хранит пароль. Без root. |
| D2 | Защита по умолчанию **WPA3** (`sae`, `pmf 3`, `proto rsn`, `pairwise/group ccmp`). Второй вариант — «WPA2/WPA3 (совместимый)»: `wpa-psk`, `pmf optional`, те же шифры (в NM `pmf 1` = *disable*, поэтому пишем словом). NM при этом **сам добавляет SAE** (§11). Никогда: open/WEP/TKIP. Переключение WPA3↔WPA2 — одна клавиша в простом режиме, с подсказкой «если старое устройство не подключается». | WPA3 работает (тест 4). TKIP ломает AP на mt7921 (тест 1). |
| D3 | Диапазон по умолчанию `auto` → 5 ГГц канал 36, если `iw list` разрешает AP на 5 ГГц; иначе 2.4 ГГц канал 6. Пользователь может выбрать явно. | Тест 5 успешен; 5 ГГц быстрее. 36–48 без DFS в UA. |
| D4 | `802-11-wireless.ap-isolation yes` всегда (клиенты не видят друг друга). | Требование безопасности. Отключаемо только в продвинутом режиме. |
| D5 | Помощник (`crates/helper`) — единственный root-код, запуск через `pkexec`, все подкоманды из `PLAN.md` этап 2 плюс `leases-read --ap <if>` (чтение аренд). (`tc-limit/tc-clear` были написаны под выключенной feature и удалены на этапе 9.) | Всё root-кодо на этапе 2 (Fable). Аренды — каталог 0700 root. |
| D6 | Полкит: `org.omarchy.hotspot.helper` с `allow_active=yes`, `allow_inactive=no`, `allow_any=no`, аннотация `org.freedesktop.policykit.exec.path=/usr/lib/omarchy-hotspot/omarchy-hotspot-helper`. Окончательно утверждается на этапе 2 после обсуждения с пользователем; запасной вариант — `auth_admin_keep`. | Фоновая служба не может спрашивать пароль на каждое одобрение устройства. Помощник делает узкий проверяемый набор операций. |
| D7 | Брандмауэр: наша nft-таблица `inet omarchy_hotspot` (input с AP: только DHCP+DNS; forward: наборы `blocked_macs`, `pending_macs`; drop AP→AP). Если ufw активен — **дополнительно** три `ufw`-разрешения с комментарием `omarchy-hotspot` (§8). NAT — не наш. | Drop в любой таблице режет пакет; у ufw forward drop. Тест 3. |
| D8 | Uplink определяется автоматически: устройство из `ip route get 1.1.1.1` (`ip -j route get` → `dev`). Если это сам Wi-Fi-интерфейс или пусто — ошибка «нет источника интернета». Можно переопределить в конфиге. | Как в `omarchy-network-status`. Один канал на карту. |
| D9 | Окно — TUI (ratatui) в плавающем окне foot с классом `org.omarchy.omarchy-hotspot`, запуск `omarchy-launch-or-focus-tui omarchy-hotspot`. Правило float — вставка в `~/.config/hypr/hyprland.lua` между маркерами. | Omarchy 4 запускает свои TUI так же; регекс по умолчанию наш класс не покрывает. |
| D10 | Интеграция с панелью: этап 7 делает **плагин** `~/.config/omarchy/plugins/omarchy-hotspot/` (kinds `bar-widget`, минимальный QML: иконка + подсказка + клик → окно, ПКМ → вкл/выкл; данные из `omarchy-hotspot status --json`). Пункты меню — через `extensions/omarchy-menu.jsonc` (`setup.network.hotspot`). Waybar-часть из плана **отменяется** (waybar нет). | Официальные точки расширения Omarchy 4. |
| D11 | Служба (этап 5): `omarchy-hotspot daemon` как user-unit systemd `omarchy-hotspot.service` (`Type=simple`, запускается TUI/CLI командой `systemctl --user start`, живёт пока раздача включена). Сокет `$XDG_RUNTIME_DIR/omarchy-hotspot/daemon.sock` (каталог 0700), протокол §6. Помощник для фоновых операций запускается службой как `pkexec … serve` и общается по stdin/stdout (JSON-строки), одна сессия на всё время раздачи. | pkexec сохраняет stdio → не нужен root-сокет и права на него. |
| D12 | Пароль: генерируется `rand::rngs::OsRng`, 16 символов, алфавит без `l1IO0`. Передача в NM — через D-Bus (`zbus`, метод `Update2`/`AddConnection2` с секретами) — на этапе 2; на этапе 1 допускается временно `nmcli … wifi-sec.psk`, но помечается TODO. Нигде не логируется. | Аргументы процесса видны в `ps`. |
| D13 | Конфиг `~/.config/omarchy-hotspot/config.toml` (§5), без пароля; `serde` + `toml`. Отсутствующие поля — значения по умолчанию, неизвестные — игнорируются с предупреждением. | Простота, обратная совместимость по этапам. |
| D14 | Языки: `ru` (по умолчанию), `uk`, `en`; словари — `enum`-ключи + функция `t(key) -> &'static str`, без внешних крейтов i18n. Выбор: конфиг → `$LANG` → `ru`. | Три языка, никаких форматных файлов. |
| D15 | Цвета только ANSI (`Color::Blue` и т.д.), иконки Nerd Font (`󱜠` точка доступа, `󰖩` Wi-Fi, `󰌾` замок, `` телефон, `󰑐` перезапуск). | Тема Omarchy перекрашивает терминал. |
| D16 | `reset`: `off` → удалить профиль NM → помощник `fw-clear`, `dns-clear`, удалить свою nft-таблицу и ufw-правила → удалить конфиг (по подтверждению) → убрать вставки из `hyprland.lua`, `omarchy-menu.jsonc`, плагин, user-unit. Каждый шаг идемпотентен. | Требование CLAUDE.md. |
| D17 | Ошибки: `thiserror` в core (типизированные), `anyhow` в бинарниках. Сообщения пользователю — через словарь, технические детали — в `--verbose`. | Новичку — понятный текст. |
| D18 | Логи: `tracing` + `tracing-subscriber` (stderr в CLI, journald через stderr user-unit). Уровень `info` по умолчанию. Пароль/PSK не попадает в `Debug` (тип `Secret(String)` с ручным `Debug`). | Простота, journald бесплатно. |

## 3. Структура крейтов и модулей

```
Cargo.toml                 workspace, resolver = "3", edition = "2024"
rustfmt.toml               max_width = 100
scripts/check.sh           fmt --check, clippy -D warnings, test
crates/core/src/
  lib.rs                   pub mod …; pub use ключевых типов
  error.rs                 enum CoreError (thiserror)
  secret.rs                struct Secret(String) — Debug = "***", Drop = zeroize
  config.rs                Config + Default + load/save (+ миграция отсутствующих полей)
  i18n.rs                  enum Lang, enum Msg, fn t(lang, Msg) -> &'static str
  password.rs              generate(), check_strength(), validate_user_password()
  hotspot.rs               Hotspot { backend, probe }: start/stop/apply/password/reset, choose_band(); trait SystemProbe (iw, uplink, ufw)
  ssid.rs                  validate_ssid()
  qr.rs                    wifi_qr_string(ssid, psk, security, hidden) -> String (экранирование \ ; , : ")
  backend/mod.rs           trait NetworkBackend, HotspotSettings, HotspotState, Band, Security
  backend/nmcli.rs         NmcliBackend (Command с массивом аргументов, LANG=C, разбор -t/-g)
  backend/nmcli_parse.rs   чистые парсеры вывода nmcli (тестируемые)
  backend/mock.rs          MockBackend (cfg(test) и для TUI-тестов)
  net/uplink.rs            detect_uplink() через `ip -j route get 1.1.1.1`
  net/iw.rs                парсер `iw list` (AP, 5 ГГц каналы без DFS, SAE) и `iw dev station dump`
  net/leases.rs            парсер аренд dnsmasq
  net/neigh.rs             парсер `ip -j -4 neigh`
  devices.rs               Device (MAC, IP, hostname, signal, rx/tx, connected_since), слияние источников, полоски сигнала
  traffic.rs               скорость по разнице байтов, история для Sparkline
  doctor.rs                Check { name, status: Ok|Warn|Fail, advice } + run_all()
  ipc.rs                   типы протокола сокета (§6): Request/Response/Event (serde JSON)
  helper_proto             типы команд помощника (§7) — реэкспорт крейта crates/proto (этап 8)
crates/app/src/
  main.rs                  clap: on|off|status|password|qr|set|reset|doctor|tui(по умолчанию)|daemon
  cli/*.rs                 по подкоманде
  tui/{mod,state,events,ui,simple,advanced,widgets}.rs
  daemon/{mod,server,approval,notify,idle}.rs      (этап 5)
  integrate/{hypr,menu,plugin,systemd}.rs          (этап 7; вставки между маркерами, .bak-<дата>)
crates/helper/src/
  main.rs                  clap enum Cmd (все подкоманды из PLAN этап 2 + leases-read, serve)
  validate.rs              проверка аргументов (iface, mac, cc, ip)
  nft.rs / ufw.rs / station.rs / regdom.rs / dns.rs / leases.rs / serve.rs
```

`crates/helper` зависит не от `core`, а только от `crates/proto` (`omarchy-hotspot-proto`: типы команд и их
проверки, зависимости — `serde` и `thiserror`), чтобы root-бинарник был маленьким. Сделано на этапе 8
(до этого помощник тянул всё ядро: D-Bus, QR, конфиг).

## 4. Основные типы и трейты (сигнатуры)

```rust
// core/backend/mod.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Security { Wpa3, Wpa2 }               // Wpa3: sae + pmf 3; Wpa2: wpa-psk + pmf 1
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Band { Auto, Ghz2_4, Ghz5 }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotspotSettings {
    pub ssid: String, pub password: Option<Secret> /* None = не менять */, pub security: Security,
    pub band: Band, pub channel: Option<u8>, pub hidden: bool,
    pub ap_isolation: bool, pub ap_iface: String, pub subnet: Ipv4Net /* по умолчанию 10.42.0.1/24 */,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HotspotState { Off, Starting, On { since: Option<SystemTime> /* из $XDG_RUNTIME_DIR/omarchy-hotspot/since */, ap_iface: String, uplink: Option<String> }, Error(String) }

pub trait NetworkBackend: Send + Sync {
    fn profile_exists(&self) -> Result<bool, CoreError>;
    fn ensure_profile(&self, s: &HotspotSettings) -> Result<(), CoreError>;   // add или modify
    fn set_password(&self, psk: &Secret) -> Result<(), CoreError>;           // этап 2 → D-Bus
    fn get_password(&self) -> Result<Secret, CoreError>;                     // nmcli -s -g 802-11-wireless-security.psk
    fn up(&self) -> Result<(), CoreError>;
    fn down(&self) -> Result<(), CoreError>;
    fn delete_profile(&self) -> Result<(), CoreError>;
    fn state(&self) -> Result<HotspotState, CoreError>;
    fn wifi_ifaces(&self) -> Result<Vec<String>, CoreError>;                  // nmcli -t -f DEVICE,TYPE device
    fn nm_running(&self) -> Result<bool, CoreError>;
}
pub const PROFILE_NAME: &str = "omarchy-hotspot";

// core/devices.rs
pub struct Device { pub mac: MacAddr, pub ip: Option<Ipv4Addr>, pub hostname: Option<String>,
    pub signal_dbm: Option<i32>, pub rx_bytes: u64, pub tx_bytes: u64, pub connected_secs: u64,
    pub status: DeviceStatus /* Allowed | Pending | Blocked */ }
pub fn signal_bars(dbm: i32) -> u8;   // > -55 → 4, > -65 → 3, > -75 → 2, иначе 1
pub fn merge(stations: &[Station], neigh: &[Neigh], leases: &[Lease]) -> Vec<Device>;

// core/doctor.rs
pub enum Status { Ok, Warn, Fail }
pub struct Check { pub id: CheckId, pub status: Status, pub detail: String, pub advice: Option<Msg> }
pub fn run_all(lang: Lang) -> Vec<Check>;   // NM активен, dnsmasq, Wi-Fi с AP, uplink, ufw, код страны, помощник установлен (этап 2+)

// core/password.rs
pub fn generate() -> Result<Secret, CoreError>;           // 16 симв., rand::rngs::SysRng (бывший OsRng в rand 0.10)
pub enum Strength { TooShort, Weak, Ok, Strong }
pub fn check_strength(p: &str) -> Strength;              // <8 TooShort; <12 или один класс символов Weak
```

Helper (этап 2), `helper_proto.rs`:
```rust
pub enum HelperCmd {
    FwApply { ap: Iface, uplink: Iface, gateway: Ipv4Addr, ufw_active: bool }, FwClear,
    FwBlock { mac: MacAddr }, FwUnblock { mac: MacAddr }, FwPendingAdd { mac: MacAddr }, FwPendingRemove { mac: MacAddr },
    StationKick { ap: Iface, mac: MacAddr }, RegdomSet { cc: CountryCode },
    DnsSet { servers: Vec<IpAddr> /* ≤4 */ }, DnsClear, LeasesRead { ap: Iface },
}
pub enum HelperReply { Ok, Leases(Vec<Lease>), Err { code: HelperErr, msg: String } }
```
Режим `serve`: читает `HelperCmd` JSON по строке из stdin, пишет `HelperReply` по строке в stdout,
завершается при EOF. Абсолютные пути: `/usr/bin/nft`, `/usr/bin/ufw`, `/usr/bin/iw`.
Окружение очищается (`env_clear()`), `PATH` не используется.

## 5. `config.toml` (все поля всех этапов)

```toml
version = 1
language = "ru"            # ru | uk | en; отсутствует → $LANG

[hotspot]
ssid = "Omarchy"           # по умолчанию "Omarchy-<hostname>"
security = "wpa3"          # wpa3 | wpa2
band = "auto"              # auto | 2.4 | 5
channel = 0                # 0 = авто
width_mhz = 20             # 20 | 40 | 80 (этап 6; 5 ГГц)
hidden = false
ap_isolation = true
guests_can_reach_pc = false   # false → input с AP только DHCP/DNS (этап 2)
country = ""               # "" = не менять; иначе 2 буквы (этап 6, regdom-set)

[network]
ap_interface = ""          # "" = авто (первый Wi-Fi с AP)
uplink_interface = ""      # "" = авто по маршруту к 1.1.1.1
subnet = "10.42.0.1/24"    # только частные диапазоны (этап 6)
dns = []                   # [] = как у ПК; иначе ≤4 IP для гостей (этап 6)
ipv6 = false

[access]                   # этап 5
approval_required = true   # новые устройства ждут одобрения
allowed_macs = []          # одобренные
blocked_macs = []          # чёрный список
notifications = true

[automation]               # этап 5–6
autostart_on_login = false
idle_off_minutes = 0       # 0 = выключено
timer_minutes = 0          # 0 = без таймера

[ui]
show_password = false
close_on_focus_loss = true    # окно Omarchy закрывается, если фокус ушёл на другое окно (§8.2)
mode = "simple"            # simple | advanced — последний открытый
```

## 6. Протокол сокета окно ↔ служба (этап 5)

Unix-сокет `$XDG_RUNTIME_DIR/omarchy-hotspot/daemon.sock`, каталог `0700`, сокет `0600`.
Строки JSON (`\n`-разделитель), `serde` с `#[serde(tag = "type")]`. Служба отвечает на каждый запрос
и, кроме того, шлёт события всем подключённым клиентам. Пароль по сокету **не передаётся** (окно берёт его у NM само).

Запросы (`Request`): `Ping`, `GetState`, `GetDevices`, `Start`, `Stop`, `Approve{mac}`, `Deny{mac}`,
`Block{mac}`, `Unblock{mac}`, `Kick{mac}`, `SetLimit{mac,down_kbps,up_kbps}` (этап 9), `ReloadConfig`, `Subscribe`.
Ответы (`Response`): `Pong`, `State{state, uplink, since, devices_count, approval}`, `Devices{list}`, `Ok`,
`Err{code, msg}`. `msg` уже на языке пользователя: его показывает окно.
События (`Event`): `StateChanged{state}`, `DeviceJoined{device}`, `DeviceLeft{mac}`, `DevicePending{device}`,
`DeviceApproved{mac}`, `DeviceBlocked{mac}`, `DeviceUnblocked{mac}`, `IdleWarning{minutes_left}`, `Stopped{reason}`.
Проверки: размер строки ≤ 64 КиБ, неизвестный тип → `Err{code:"bad_request"}`, MAC — валидация как в помощнике
(тип `helper_proto::Mac`), uid подключившегося должен совпадать с нашим (`SO_PEERCRED`).
Без службы окно работает в режиме «только просмотр» (опрашивает NM/iw само).

## 7. Как совмещаемся с ufw и nftables

- Своя таблица `inet omarchy_hotspot` (только её создаём/удаляем `nft delete table inet omarchy_hotspot`).
- Если `ufw` активен (`/usr/bin/ufw status` через помощника; для doctor — `systemctl is-active ufw`):
  ```
  ufw allow in on <ap> proto udp from any port 68 to any port 67 comment 'omarchy-hotspot'
  ufw allow in on <ap> to <gateway> port 53 comment 'omarchy-hotspot'
  ufw route allow in on <ap> out on <uplink> comment 'omarchy-hotspot'
  ```
  Проверено (тест 3). Уточнение этапа 2: правил четыре (DNS отдельно udp и tcp, точный вид — `SECURITY.md` §3.2);
  удаление — по номерам из `ufw status numbered` для строк с `# omarchy-hotspot` (с конца), `fw-apply` сначала
  удаляет старые наши, потом добавляет — так нет дубликатов. Чужие правила и `DEFAULT_*_POLICY` не трогаем.
- ufw-docker (`DOCKER-USER`) нам не мешает: наш forward идёт `<ap> → <uplink>`, не `docker0`.
- Если ufw выключен, а у пользователя свой nft с `input drop` — doctor предупреждает, ничего не чиним.

## 8. Omarchy: терминал, окно, меню, панель

- Запуск: `omarchy-launch-or-focus-tui omarchy-hotspot` (класс `org.omarchy.omarchy-hotspot`, терминал по `xdg-terminal-exec`, у пользователя foot).
- Правило окна — вставка в конец `~/.config/hypr/hyprland.lua` (копия `.bak-<дата>`, маркеры `-- omarchy-hotspot begin/end`):
  ```lua
  -- omarchy-hotspot begin
  o.window("^(org\\.omarchy\\.omarchy-hotspot)$", { tag = "+floating-window" })
  -- omarchy-hotspot end
  ```
  Тег даёт float + center + 875×600, как у остальных TUI Omarchy. Применение: `hyprctl reload`. Заменено в §8.2.
- Меню — вставка в `~/.config/omarchy/extensions/omarchy-menu.jsonc` (JSONC; вставляем внутрь объекта
  перед закрывающей `}` между маркерами `// omarchy-hotspot begin/end`):
  ```jsonc
  "setup.network.hotspot": {"icon":"󱜠","label":"Hotspot","aliases":["hotspot"],"action":"omarchy-launch-or-focus-tui omarchy-hotspot"},
  "setup.network.hotspot-toggle": {"icon":"󰐥","label":"Hotspot On/Off","checked":"omarchy-hotspot status --quiet","action":"omarchy-hotspot toggle"},
  ```
  Файл читается процессом shell при старте — после правки `omarchy-shell shell reload` (уточнить на этапе 7; запасной вариант — перезапуск shell).
- Панель: плагин `~/.config/omarchy/plugins/omarchy-hotspot/{manifest.json,BarWidget.qml}`,
  `kinds:["bar-widget"]`, `barWidget.defaultSection:"right"`, обновление `Process{command:["omarchy-hotspot","status","--json"]}`
  каждые 5 с. Образец API — `shell/plugins/panels/tailscale/Panel.qml` (Quickshell.Io Process, `qs.Ui`). Если API
  плагинов окажется нестабильным (alpha) — оставить только меню, записать в PROGRESS.
- Горячая клавиша: не ставим по умолчанию; на этапе 7 спросить пользователя (вставка в `~/.config/hypr/bindings.lua`).
- Уведомления: `notify-send -a omarchy-hotspot -i network-wireless-hotspot …` (mako/shell покажет).

## 9. Зависимости (crates)

| Крейт | Зачем |
|-------|-------|
| `clap` (derive) | CLI и подкоманды помощника |
| `serde`, `serde_json`, `toml` | конфиг, `--json`, протоколы |
| `thiserror` (core), `anyhow` (bin) | ошибки |
| `tracing`, `tracing-subscriber` | журнал без паролей |
| `rand` | генератор пароля (`OsRng`) |
| `zeroize` | затирание пароля в памяти |
| `rpassword` | скрытый ввод пароля в `set password` (этап 1) |
| `ratatui`, `crossterm` | TUI (этап 3) |
| `qrcode` | QR-код в терминале (этап 1; рисуем сами полублоками) |
| `zbus` | секрет в NM через D-Bus без `ps`-утечки (этап 2; если тяжело — обсудить) |
| `nix` (feature `socket`, `user`) | unix-сокет службы, uid/gid проверки (этап 5) |
| `tokio` (rt, net, io-util, sync, time) | служба и фоновые задачи TUI (этап 3/5) |
| `libc` | только если `nix` не покроет (не добавлять заранее) |

Версии не фиксируются в документе — `cargo add` берёт актуальные; `Cargo.lock` в репозитории.

## 8.1 Уточнения этапа 7 (Omarchy 4.0.3)

- Меню читается с `watchChanges: true` — перезагрузка shell не нужна. Строки вставляем сразу после первой строки
  `{` (у наших строк запятая в конце, JSONC допускает висячие запятые), а не перед `}`.
- Панель: плагин `~/.config/omarchy/plugins/omarchy-hotspot/` (`packaging/shell-plugin/`), включение штатно —
  `omarchy-shell shell rescanPlugins` + `omarchy-plugin-enable omarchy-hotspot --section right --before omarchy.network`.
  Виджет раз в 5 с запускает `omarchy-hotspot status --waybar` (JSON `text/tooltip/class/devices`), клики —
  `Quickshell.execDetached` с массивом аргументов (без оболочки). Командный модуль в `shell.json` не взят:
  его пришлось бы вписывать в JSON без маркеров.
- Правый клик и пункт меню — `omarchy-hotspot toggle`; без терминала ошибка приходит уведомлением.
- Обновление значка (14.09.2026): `omarchy-shell shell rescanPlugins` не выгружает из памяти оболочки старый код
  уже загруженного значка — клик продолжал выполнять прежнюю команду (подтверждено журналом `open.log`: строк не было
  до `omarchy-restart-shell`). Поэтому установщик при изменении файлов уже установленного значка вызывает
  `omarchy-restart-shell` (сам отказывается при заблокированном экране); при первой установке — `rescanPlugins`.
- Горячая клавиша: владелец отказался (13.09.2026).

## 8.2 Компактное окно и мышь (после этапа 8, решение владельца)

- Правило окна в `hyprland.lua` (14.09.2026): `o.window(<класс>, { float = true, size = { 760, 440 } })` — без `move`.
  Место задаёт только правило в памяти Hyprland от `omarchy-hotspot open` — числами: `move = { "(monitor_w-N)", "(Y)" }`,
  где N = ширина окна + полоса панели справа + отступ (запомненный размер, до первой подгонки — 760×440). С `window_w`
  в постоянном правиле окно появлялось левее (Hyprland брал ширину 760 из постоянного правила, а не запомненную) и
  прыгало в угол; одно правило с местом не зависит от того, какое правило Hyprland применит последним.
- (До 14.09.2026) Правило окна вместо тега `floating-window` (875×600): `o.window(<класс>, { float = true, size = { 760, 440 },
  move = { "(monitor_w-window_w-X)", "(Y)" } })` — правый верхний угол под панелью, как панели Omarchy (сеть,
  Bluetooth, звук). Панели Omarchy — всплывающие окна оболочки под своим значком с отступом `Style.gapsOut` =
  половина `general:gaps_out` (`Ui/PopupCard.qml`, `Commons/Style.qml`); у нашего окна ещё рамка Hyprland
  (`general:border_size`), поэтому отступ = gaps_out/2 + border (у Omarchy по умолчанию 5 + 2). Числа установщик
  берёт из `hyprctl` (`getoption`, `monitors[].reserved` фокусного монитора) и проверяет как целые; нет ответа —
  значения Omarchy (панель сверху 26 px). Панель снизу — правый нижний угол над ней; справа — окно левее панели;
  слева — правый верхний угол. Синтаксис проверен `hyprctl eval` на несуществующем классе. Повторный `install.sh`
  заменяет блок между маркерами (с `.bak`), если маркеров ровно по одному; если Hyprland не ответил (установка по SSH,
  из консоли) — существующий блок не трогает. Числа — только `0|[1-9][0-9]{0,4}`.
- Подгонка не трогает окно, если свободного места на мониторе меньше 320×200 px (странные `reserved`); полоса панели
  обрезается размером монитора, отступы — разумными пределами, арифметика с насыщением. Ревью (security-reviewer):
  критичных и высоких нет, 2 низких исправлены.
- Спокойный размер (решение владельца 14.09.2026): окно одного размера в обоих режимах и при вкл/выкл — больший из
  режимов с запасами: статус на 5 строк (сообщение не двигает окно), блоки трафика и устройств видны и при выключенной
  раздаче, под устройства — запас строк (2, растёт шагом 2 до 8 сразу, уменьшается через 30 с), продвинутый режим —
  по самому высокому разделу, самой длинной подсказке и строке клавиш. QR скрыт по умолчанию: `c` или `[ QR ]` в рамке
  «Статус» показывают и прячут; компактный QR (коррекция L, тихая зона 1 модуль, 31×16, подпись в рамке) помещается
  в тот же размер справа, иначе снизу, иначе поверх окна (клик или `c`/`Esc` прячут). При выключении раздачи QR
  прячется. QR строится только из пароля (`rebuild_qr` при `PasswordLoaded`); поэтому `c`/`[ QR ]` (`toggle_qr`) сам
  загружает пароль, если его ещё нет (окно могли открыть при уже работающей раздаче — раньше QR появлялся только после
  `Tab`, который грузил пароль для продвинутого режима), а статус «включено», пришедший после пароля, достраивает QR.
  Окно подрастает только если открытому QR не хватает места или без службы одобрения одновременно видны
  предупреждение и сообщение. Читаемость компактного QR проверена `zbarimg` и `ZXingReader` на картинках «как в
  терминале» (чёрное на белом, тёмный фон вокруг) при ячейке от 8×16 до 2×4 px.
- (До 14.09.2026) Рамка окна облегает содержимое: `ui::desired_size` считает столбцы и строки по тем же функциям, что рисуют
  (статус с переносом, устройства по числу — до 8 строк, дальше прокрутка, QR, подсказки, всплывающие окна).
  Окно больше нужного — рамка по центру; меньше — простой режим ставит QR справа → снизу → прячет его за
  строку «QR не помещается» (`c` или клик — QR на всё окно). QR никогда не рисуется обрезанным (тест по сетке размеров).
- Подгонка окна (`tui/fit.rs`), как у окна About в Omarchy: только под Hyprland, только наше плавающее окно
  (`class == org.omarchy.omarchy-hotspot`, `pid` окна — родитель процесса), после того как нужный размер и размер
  терминала не менялись 350 мс. Сдвиг в пикселях = разница в ячейках × размер ячейки (`TIOCGWINSZ`), затем окно
  прижимается к углу по живым данным Hyprland (`reserved` монитора, gaps_out/2 + рамка) — перенос панели учитывается
  при следующей подгонке. Один проход на каждый новый размер содержимого (даже если размер уже тот — чтобы поставить
  в угол), уточнения — до 3 попыток; ручные размер и место пользователя не перебиваются, пока содержимому не
  понадобится другой размер. Команды: `hl.dsp.window.resize`/`move` (Lua), запасные — `resizewindowpixel`/
  `movewindowpixel exact` по адресу. Подошло больше одного окна (терминал-
  сервер держит окна в одном процессе) — подгонки нет. В Lua-строку попадают только числа и адрес `0x<hex>`.
- Ревью (security-reviewer): критичных и высоких нет; исправлены 6 низких (центрирование в старом синтаксисе,
  несколько окон одного процесса, паника фоновой задачи не восстанавливает терминал живому окну, независимые шаги
  восстановления терминала, ошибка записи конфига в install/uninstall останавливает скрипт, поиск маркеров с пробелами).
- Закрытие при потере фокуса (решение владельца 14.09.2026, `ui.close_on_focus_loss`, по умолчанию вкл, пункт в
  «Автоматизации»): правилом Hyprland такого не сделать, поэтому программа ловит `FocusLost` терминала и закрывается,
  если через 1 с фокус не вернулся. Только в своём плавающем окне Omarchy (его нашла подгонка окна) — в обычном
  терминале пользователя нет. Не закрывается при открытом окне `e`, вопросе (`Confirm`), вводе значения, идущей
  операции (`busy`) и несохранённых правках продвинутого режима. В Omarchy фокус следует за мышью
  (`follow_mouse = 1`, `float_switch_override_focus = 1`), поэтому окно закрывается и когда мышь уведена на другое
  окно, не только по клику. Закрытие мгновенное и только по самому событию `FocusLost` (сначала была пауза 1 с —
  владелец попросил убрать). Не на тике: если фокус ушёл во время операции, окно остаётся и после её конца — результат
  (ошибку, предупреждение о защите) пользователь увидит, вернувшись (находка ревью). Что делает клик по панели
  Omarchy и по пустому рабочему столу, зависит от того, забирают ли они фокус клавиатуры, — проверяется вручную.
- Открытие сразу конечным размером (как `omarchy-launch-about`): размер окна в пикселях зависит от шрифта терминала и
  известен только изнутри, поэтому правило в `hyprland.lua` (760×440) — лишь первое приближение, и окно подгонялось
  через ~0.4 с после открытия (скачок). После подгонки к обычному размеру (без QR и всплывающих окон) программа
  запоминает размер в `$XDG_STATE_HOME/omarchy-hotspot/window-size` и ставит правило размера в памяти Hyprland
  (`hyprctl eval`, Lua-переменная `omarchy_hotspot_size_rule`, прошлое правило выключается). `omarchy-hotspot open`
  (значок на панели, пункт меню) перед `omarchy-launch-or-focus-tui omarchy-hotspot` применяет запомненный размер —
  окно открывается сразу им; каждый запуск `open` пишет строку в `open.log` (время, размер, ответ Hyprland; до 16 КиБ,
  0600, не по ссылке) — по нему видно, открывалось ли окно через `open`. Размер вне 100..=10000 px не применяется; файл читается только обычный и не больше 64 байт,
  пишется через новый временный файл 0600 в каталоге 0700, относительный `XDG_STATE_HOME` не принимается; `reset`
  удаляет каталог и снимает правило из памяти Hyprland, `uninstall.sh` удаляет каталог и сам. Размер содержимого
  не зависит от загруженных данных (проверено тестом `window_size_does_not_jump`), поэтому при открытии пересчитывать нечего;
  меняется он только при открытии/закрытии QR, если тому не хватает места. Установщик заменяет блок меню, если он от прошлой версии.
- Клик, которым активировали окно, ничего не нажимает: терминал сообщает о фокусе (`EnableFocusChange`), клик
  в течение 150 мс после этого пропускается. При «фокус следует за мышью» окно активно уже при наведении, и клик
  срабатывает сразу.
- Мышь (`tui/mouse.rs`): при отрисовке кликабельные места пишут прямоугольники в `Hits`; клик и колесо вызывают
  те же функции, что клавиши. Параметры кликом только выделяются. Вопросы, ввод текста и окно `e` — только
  клавиатура. Движения мыши не передаются в цикл окна. В простом режиме строка устройства убирает столбцы по
  ширине (скорость, потом IP); подробности — в разделе «Устройства» продвинутого режима.
- Без помощника раздача больше не включается (`HelperNotInstalled`), предупреждения `NoHelper/UfwWillBlock` удалены.

## 10. Открытые вопросы (проверить на указанных этапах)

- ~~Этап 1: нужен ли `T:SAE` в QR~~ — снято: `T:WPA` работает для WPA3 (проверено на телефоне, §11). iPhone не проверялся.
- ~~Этап 1: авто-выбор канала NM~~ — снято: канал всегда задаём сами (`choose_band`: 5 ГГц → первый из 36/40/44/48, разрешённых `iw phy`, иначе первый разрешённый; 2.4 ГГц → 6).
- ~~Этап 2: `allow_active=yes` vs `auth_admin_keep`~~ — решено 11.09.2026: `allow_active=yes` (`SECURITY.md` §4).
- ~~Этап 7: команда перезагрузки меню shell и стабильность API `bar-widget`~~ — снято, см. §8.1.
- Всегда: Omarchy alpha обновляется часто — после `omarchy-update` прогонять `doctor`.

## 11. Уточнения этапа 1

- CLI: `on`, `off`, `status [--json|--quiet]` (код выхода 0 — включена), `password`, `qr`,
  `set ssid <имя>`, `set password [--generate|--stdin]` (без флагов — скрытый ввод дважды),
  `set band auto|2.4|5`, `set security wpa3|wpa2`, `reset [--yes]`, `doctor`; глобальные `--lang`, `-v/--verbose`.
- Профиль: `connection.autoconnect no`, `ipv6.method disabled`, `ipv4.addresses` = подсеть из конфига, канал всегда явный.
  Профили ищутся по имени **и** типу `802-11-wireless`, дальше работаем по UUID; `reset` удаляет все дубликаты.
- `nmcli -g` экранирует `:` и `\` даже для одного поля — снимаем экранирование, убираем ровно один `\n` (пробел в конце пароля сохраняется).
- Команды с паролем в аргументах (`cmd::run_secret`) не логируются, stderr nmcli не сохраняется.
- `on` при первом запуске сохраняет конфиг (SSID не меняется вместе с именем ПК). `set` сначала применяет, потом сохраняет конфиг —
  неприменимая настройка не попадает в файл. Если раздача включена, `set` перезапускает её (`nmcli connection up` повторно).
- `reset` (этап 1): выключить → удалить профиль(и) → удалить `~/.config/omarchy-hotspot` и пустой `$XDG_RUNTIME_DIR/omarchy-hotspot`.
- Пока нет помощника, `on` при активном ufw печатает предупреждение (уберётся на этапе 2).
- Режим `security = "wpa2"` на деле смешанный WPA2/WPA3. Проверено по журналу NM (supplicant config):
  `wpa-psk` + `pmf optional` → `key_mgmt 'WPA-PSK WPA-PSK-SHA256 SAE'`, телефон подключается как WPA3-Personal;
  `wpa-psk` + `pmf disable` → `key_mgmt 'WPA-PSK WPA-PSK-SHA256'`, `ieee80211w 0` — чистый WPA2, но без PMF.
  Решение: оставляем смешанный режим (современные устройства сохраняют WPA3 и PMF), в интерфейсе он называется
  «WPA2/WPA3 (совместимый)», в конфиге и CLI значение остаётся `wpa2`. Чистый WPA2 без PMF — не делаем;
  если найдётся устройство, которое не подключается даже в смешанном режиме, обсудить на этапе 6 (продвинутый режим).
- Риск (не проверено): NM добавляет SAE по возможностям wpa_supplicant, а не драйвера. На карте без SAE в режиме AP
  смешанный режим может не включиться — проверить на этапе 8, если будет такая карта; `doctor` уже показывает SAE.
- QR для WPA3: `T:WPA` работает (телефон Android, этап 1) — `T:SAE` не нужен.


## 12. Уточнения этапа 4 (устройства и трафик)

- Источники данных списка: `iw dev <ap> station dump` (MAC, сигнал, байты, время подключения) и
  `ip -j -4 neigh show dev <ap>` (MAC → IP) — оба без root, опрос раз в секунду; аренды dnsmasq
  (`leases-read` через помощника) — только ради **имён**, поэтому запрашиваются редко: раз в 15 с,
  пока ждём имя устройства, подключившегося меньше минуты назад, иначе раз в 2 минуты, и вовсе не
  запрашиваются, если никто не подключён. Каждый вызов помощника — это `pkexec` плюс строка в
  журнале, опрашивать его раз в секунду нельзя.
- IP из аренды важнее IP из таблицы соседей (аренда — то, что реально выдал dnsmasq).
- Имя устройства присылает по DHCP само устройство, то есть это чужие данные: `leases::sanitize_hostname`
  убирает управляющие символы (в том числе escape-коды терминала), невидимые символы и переворот
  текста, обрезает до 24 символов. Иначе гость мог бы «нарисовать» что угодно в окне.
- Байты в `Station`/`Device` — со стороны точки доступа: `rx` принято от устройства, `tx` отправлено ему.
  В окне ↓ — это `tx` (устройство скачивает), ↑ — `rx`. Для читаемости есть `Device::down_bytes/up_bytes`.
- `TrafficMeter` считает по разнице счётчиков между опросами: общий итог за сеанс, скорости ↓/↑ и
  скорость каждого устройства. Счётчик меньше прежнего (устройство переподключилось) — считаем весь
  счётчик новыми байтами; ушедшее устройство свои байты из итога не забирает. Замеры чаще 0.2 с
  пропускаются (иначе на графике выброс). История графика — 120 замеров (≈2 минуты).
- Сортировка списка: кто дольше подключён — выше, при равенстве по MAC (список не «прыгает»).
- Новый метод бэкенда `client_connection(iface)`: имя сети, к которой Wi-Fi-адаптер подключён как
  клиент (`nmcli -t -f DEVICE,STATE,CONNECTION device status`, наш профиль не считается). Нужен,
  чтобы предупредить: одна карта не может быть и точкой доступа, и клиентом. В CLI `on` — строка
  предупреждения перед включением, в окне — вопрос поверх окна.
- Ответ на вопрос в окне — **Enter (да) / Esc (нет)**, а не `y`/`n`: на кириллической раскладке
  клавиша `y` даёт «н», и пользователь, думая «н = нет», нажал бы «да».
- Ошибки опроса устройств в окне не показываются (раз в секунду сообщение бы мигало) — только в журнал.

## 13. Уточнения этапа 5 (служба, одобрение, уведомления)

- **Кто решает про режим одобрения.** Раздачу включают окно, CLI или (позже) панель, а следит за
  устройствами служба. Чтобы решение было одно, `on` включает раздачу с `approval = approval_required &&
  служба доступна`, а служба, увидев включённую раздачу, через 2 секунды сама применяет `fw-apply` с нужным
  режимом, если он отличается от применённого. Отсюда же берётся поле `approval` в ответе `State`:
  окно показывает значок «одобрение» и предупреждение, только когда правила действительно применены.
- **Помощник для службы.** Один процесс `pkexec … serve` на всё время раздачи (`core::helper::ServeHelper`).
  При выключении раздачи канал закрывается и помощник завершается. Проверено 12.09.2026: polkit пускает
  вызов из службы systemd (`allow_active` распространяется на `user@.service`, пока у пользователя есть
  активный сеанс), код возврата 0.
- **Запуск и остановка службы.** `on` (CLI и окно) выполняет `systemctl --user start omarchy-hotspot.service`,
  `off` — `stop`. Файл службы ставит установщик (этап 7), до него служба запускается вручную:
  `omarchy-hotspot daemon`.
- **Списки доступа — в конфиге** (`access.allowed_macs`, `access.blocked_macs`), их пишет служба
  (`core::access`). Окно списки не трогает: оно берёт состояния устройств у службы (`GetDevices`) и шлёт
  `Approve`/`Deny`. При записи имени сети окно перечитывает файл и меняет в нём только `hotspot.ssid`,
  чтобы не затереть одобренные устройства.
- **Отказ устройству** = чёрный список + `station-kick`; пока устройство остаётся в списке, служба
  отключает его снова не чаще раза в 10 секунд (иначе вернуться оно не может, но и «долбить» его незачем).
- **Вопрос про одобрение при первом запуске** задаёт окно (конфига ещё нет), ответ сразу пишется в файл.
  В CLI вопроса нет: там действует значение по умолчанию (`approval_required = true`).

## 14. Уточнения этапа 6 (продвинутый режим)

- **Черновик.** Стрелки меняют копию конфига (`tui::settings::Advanced::draft`), в файл она попадает только
  по `s`, и переносятся лишь поля продвинутого режима (`settings::merge_into`) — имя сети и списки устройств
  берутся из свежего файла под замком. Раздел с правками помечен `*`, при выходе — вопрос
  «Сохранить изменения?» (Enter — сохранить и выйти, Esc — выйти без сохранения). Если конфиг поменялся
  не через черновик (окно `e`, служба), черновик догоняет его в разделах без правок (`sync_from`).
- **Перезапуск.** Нужен при смене диапазона, канала, ширины, скрытости, страны, защиты, изоляции,
  источника интернета, подсети и DNS. При включённой раздаче окно спрашивает и делает `off` + `on`
  (раньше `set` делал повторный `up`, но источник интернета и DNS так не применяются). Одобрение, уведомления,
  доступ гостей к ПК, простой и таймер применяет служба, перечитав конфиг (`ReloadConfig`).
- **Раздел «Устройства»** (просьба владельца): подключённые устройства и чёрный список (в том числе не в сети),
  `b` — отобрать/вернуть интернет (block/unblock), `a`/Enter — разрешить, `k` — отключить (может вернуться),
  `o` — одобрение новых вкл/выкл **сразу**, без `s`. Сведения о выбранном: MAC, сигнал, время, скорость, байты.
- **Страна** задаётся при каждом `on`, если отличается (`iw reg set` не переживает перезагрузку).
  **DNS для гостей** синхронизируется при `on` (файл пишет/удаляет помощник). Частые варианты: Cloudflare,
  Quad9, Google, AdGuard; до 4 своих адресов.
- **Ширина канала** из `hotspot.width_mhz` (20/40/80) → `802-11-wireless.channel-width`; 80 МГц на 2.4 ГГц → 40.
- **IPv6** — только чтение «выключен» (решение владельца 13.09.2026, см. `SECURITY.md` §5.1).
- **Таймер** (`automation.timer_minutes`) считает служба от времени включения; если таймер включили или
  поменяли во время раздачи — от момента изменения. За минуту до конца — событие `TimerWarning` в окно.
- **Автозапуск** (`automation.autostart_on_login`): `systemctl --user enable` службы; служба один раз за вход
  включает раздачу, но не трогает Wi-Fi, подключённый к сети как клиент (уведомление вместо этого).
  Без установленного файла службы включить нельзя — окно говорит, что служба не установлена.
- **Режим окна** (`ui.mode`) запоминается при `Tab`, но только если файл конфига уже есть: иначе пропал бы
  вопрос первого запуска про одобрение.
- Горячие клавиши продвинутого режима с кириллицей: `ы`/`і`→s, `и`→b, `л`→k, `ф`→a, `щ`→o. В поле ввода
  буквы — просто буквы.
- `e` (имя сети и пароль) работает в обоих режимах. `g` на строке «Пароль» — новый пароль после вопроса.

