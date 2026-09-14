//! Подгонка плавающего окна Omarchy под содержимое (как окно About в Omarchy): окно само
//! меняет размер при включении раздачи, смене вкладки, появлении устройств — и остаётся в правом
//! верхнем углу под панелью, как системные панели Omarchy (сеть, Bluetooth, звук).
//! Работает только в нашем окне `org.omarchy.omarchy-hotspot` под Hyprland: если программу
//! открыли в обычном терминале, его размер не трогаем.
//! `hyprctl` запускается массивом аргументов; в Lua-команду попадают только числа и адрес окна,
//! проверенный по формату `0x<hex>`.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Класс окна, который Omarchy даёт программе (`omarchy-launch-or-focus-tui omarchy-hotspot`).
pub const APP_CLASS: &str = "org.omarchy.omarchy-hotspot";
/// Желаемый размер должен продержаться столько, прежде чем двигать окно (иначе окно дёргается
/// на каждом промежуточном кадре: статус, потом QR, потом устройства).
const SETTLE: Duration = Duration::from_millis(350);
/// Сколько раз уточнять размер, если первый раз не попали в сетку ячеек терминала.
const MAX_ATTEMPTS: u8 = 3;
/// Стартовый размер окна из правила в `hyprland.lua` (install.sh): им открываем, пока свой не запомнен.
pub const DEFAULT_SIZE: (i64, i64) = (760, 440);
/// Имя Lua-переменной правила размера в памяти Hyprland (как `omarchy_about_size_rule` у окна About).
const SIZE_RULE_VAR: &str = "omarchy_hotspot_size_rule";
/// Запомненный размер окна в пикселях вне этих пределов — испорченный файл, не применяем.
const SIZE_LIMITS: std::ops::RangeInclusive<i64> = 100..=10_000;
/// Меньше этого свободного места на мониторе (в пикселях) — данные Hyprland странные, окно не трогаем.
const MIN_FREE: (i64, i64) = (320, 200);

#[derive(Debug, Default)]
pub struct Fitter {
    /// Размер, который нужен содержимому, и с какого момента он такой.
    wanted: Option<(u16, u16)>,
    /// Это обычный размер окна (без открытого QR и всплывающих окон): его запоминаем для открытия.
    wanted_is_base: bool,
    wanted_since: Option<Instant>,
    /// Для этого размера подгонка закончена (удачно или попытки кончились).
    done_for: Option<(u16, u16)>,
    attempts: u8,
    /// Размер терминала на прошлом тике и когда он последний раз менялся (анимация Hyprland).
    last_term: Option<(u16, u16)>,
    term_changed: Option<Instant>,
    /// Идёт вызов `hyprctl` в фоне.
    busy: Arc<AtomicBool>,
    /// Под Hyprland ли мы вообще (без него подгонка не нужна).
    disabled: bool,
    /// Программа открыта в своём плавающем окне Omarchy (а не в обычном терминале пользователя).
    own_window: Arc<AtomicBool>,
}

impl Fitter {
    pub fn new() -> Fitter {
        Fitter {
            disabled: std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none(),
            ..Fitter::default()
        }
    }

    /// Окно найдено как наше плавающее окно Omarchy: только в нём уместно закрываться при потере фокуса.
    pub fn own_window(&self) -> bool {
        self.own_window.load(Ordering::Relaxed)
    }

    /// Запомнить, какой размер нужен содержимому сейчас (после каждой отрисовки).
    pub fn want(&mut self, size: (u16, u16), is_base: bool) {
        self.wanted_is_base = is_base;
        if self.wanted != Some(size) {
            self.wanted = Some(size);
            self.wanted_since = Some(Instant::now());
            self.attempts = 0;
        }
    }

    /// Раз в тик: если нужно и можно — подогнать окно в фоне.
    pub fn tick(&mut self, term: (u16, u16)) {
        let Some(pass) = self.decide(term, Instant::now()) else {
            return;
        };
        self.busy.store(true, Ordering::Relaxed);
        let busy = Arc::clone(&self.busy);
        let own = Arc::clone(&self.own_window);
        std::thread::spawn(move || {
            let result = match pass {
                Pass::Fit(wanted) => fit_window(term, wanted, &own),
                Pass::Remember => remember_window(),
            };
            if let Err(e) = result {
                tracing::debug!("window fit skipped: {e}");
            }
            busy.store(false, Ordering::Relaxed);
        });
    }

    /// Нужно ли сейчас подгонять окно (и под какой размер). Без побочных эффектов, кроме счётчиков.
    fn decide(&mut self, term: (u16, u16), now: Instant) -> Option<Pass> {
        if self.last_term != Some(term) {
            self.last_term = Some(term);
            self.term_changed = Some(now);
        }
        let wanted = self.wanted?;
        if self.disabled || self.done_for == Some(wanted) || self.busy.load(Ordering::Relaxed) {
            return None;
        }
        let settled = |t: Option<Instant>| t.is_none_or(|t| now.duration_since(t) >= SETTLE);
        if !settled(self.wanted_since) || !settled(self.term_changed) {
            return None;
        }
        // Первый проход — всегда (даже если размер уже тот: окно надо поставить в угол), дальше —
        // только пока не попали в размер. Попали или не получается (монитор меньше, окно не
        // плавающее) — больше не трогаем, пока содержимому не понадобится другой размер.
        // Ручной размер и положение пользователя до тех пор не перебиваются.
        if self.attempts > 0 && (term == wanted || self.attempts >= MAX_ATTEMPTS) {
            self.done_for = Some(wanted);
            // Попали в обычный размер — запомнить его в пикселях: следующее окно откроется сразу им.
            return (term == wanted && self.wanted_is_base).then_some(Pass::Remember);
        }
        self.attempts += 1;
        Some(Pass::Fit(wanted))
    }
}

/// Что сделать в фоне.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// Подогнать размер и поставить окно в угол.
    Fit((u16, u16)),
    /// Размер подошёл: запомнить его в пикселях и сообщить Hyprland.
    Remember,
}

/// Файл с размером окна в пикселях (`$XDG_STATE_HOME/omarchy-hotspot/window-size`).
/// Относительный `XDG_STATE_HOME` не принимаем (так велит спецификация XDG): иначе `reset`
/// удалил бы каталог относительно текущей папки.
pub fn state_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|h| h.join(".local/state"))
        })?;
    Some(base.join("omarchy-hotspot"))
}

fn size_file() -> Option<PathBuf> {
    state_dir().map(|d| d.join("window-size"))
}

/// «ширина высота» в пикселях; мусор и нелепые числа — `None`.
fn parse_size(text: &str) -> Option<(i64, i64)> {
    let mut it = text.split_whitespace().map(|n| n.parse::<i64>().ok());
    let (w, h) = (it.next()??, it.next()??);
    (it.next().is_none() && SIZE_LIMITS.contains(&w) && SIZE_LIMITS.contains(&h)).then_some((w, h))
}

/// Только обычный файл и не больше нескольких байт (FIFO или ссылка на /dev/zero не подвесят `open`).
pub fn remembered_size() -> Option<(i64, i64)> {
    use std::io::Read;
    let file = size_file()?;
    if !std::fs::symlink_metadata(&file).ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    std::fs::File::open(&file)
        .ok()?
        .take(64)
        .read_to_string(&mut text)
        .ok()?;
    parse_size(&text)
}

/// Короткий журнал последнего `open` (`open.log` рядом с размером): когда запускался и что ответил
/// Hyprland — чтобы было видно, открывали ли окно через `open`. Только время, размер и итог.
pub fn log_open(line: &str) {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let Some(dir) = state_dir() else { return };
    if std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .is_err()
    {
        return;
    }
    let path = dir.join("open.log");
    // Не по ссылке и не бесконечно: старый журнал больше 16 КиБ начинаем заново.
    if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file() || m.len() > 16 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        // Не по символической ссылке и без ожидания (если на месте файла окажется канал).
        .custom_flags((nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK).bits())
        .open(&path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Снять наше правило размера из памяти Hyprland.
pub fn clear_size_rule() {
    let lua = format!(
        "if {v} then {v}:set_enabled(false) end; {v} = nil",
        v = SIZE_RULE_VAR
    );
    let _ = hyprctl(&["eval", &lua]);
}

/// Правило для нашего окна в памяти Hyprland (не в конфиге): прошлое выключается, новое ставится.
/// Размер и место в углу — числами: `window_w` в выражении `move` Hyprland берёт из постоянного
/// правила (760), а не из этого размера, и окно появлялось левее, а потом прыгало в угол.
/// Всё в строке — только числа из `i64`, класс — константа.
fn size_rule_lua(w: i64, h: i64, place: Option<&[String; 2]>) -> String {
    let place = place
        .map(|[x, y]| format!(", move = {{ \"{x}\", \"{y}\" }}"))
        .unwrap_or_default();
    format!(
        "if {v} then {v}:set_enabled(false) end; {v} = hl.window_rule({{ match = {{ class = \"^(org\\\\.omarchy\\\\.omarchy-hotspot)$\" }}, size = {{ {w}, {h} }}{place} }})",
        v = SIZE_RULE_VAR
    )
}

/// Размер не больше свободного места монитора, но и не меньше нижнего предела.
fn fit_on_monitor((w, h): (i64, i64), (free_w, free_h): (i64, i64)) -> (i64, i64) {
    let min = *SIZE_LIMITS.start();
    (w.min(free_w.max(min)), h.min(free_h.max(min)))
}

/// Место окна размером `w`×`h` в углу монитора — выражения правила Hyprland без `window_w`:
/// «монитор минус число» справа (и снизу, если панель внизу), число сверху.
fn corner_exprs(mon: &Monitor, w: i64, h: i64, gap: i64) -> [String; 2] {
    let [_, top, right, bottom] = mon.reserved;
    let x = format!(
        "(monitor_w-{})",
        w.saturating_add(right).saturating_add(gap)
    );
    let y = if bottom > top {
        format!(
            "(monitor_h-{})",
            h.saturating_add(bottom).saturating_add(gap)
        )
    } else {
        format!("({})", top.saturating_add(gap))
    };
    [x, y]
}

/// Сообщить Hyprland размер и место, которыми открывать окно (перед открытием и после подгонки).
/// Место считается для монитора с фокусом — на нём Hyprland и откроет окно. Данные монитора
/// не прочитались — правило только с размером (место тогда поправит подгонка).
pub fn apply_size_rule(w: i64, h: i64) -> Result<(), String> {
    if !SIZE_LIMITS.contains(&w) || !SIZE_LIMITS.contains(&h) {
        return Err(format!("window size out of range: {w}x{h}"));
    }
    let (mut w, mut h) = (w, h);
    let place = hyprctl(&["-j", "monitors"])
        .ok()
        .and_then(|json| focused_monitor(&json))
        .map(|mon| {
            let gap = edge_gap(
                &hyprctl(&["-j", "getoption", "general:gaps_out"]).unwrap_or_default(),
                &hyprctl(&["-j", "getoption", "general:border_size"]).unwrap_or_default(),
            );
            // Размер запомнен на большом мониторе, открываем на маленьком — не шире свободного места,
            // иначе окно встало бы за левым краем.
            (w, h) = fit_on_monitor((w, h), mon.free_size(gap));
            corner_exprs(&mon, w, h, gap)
        });
    let out = hyprctl(&["eval", &size_rule_lua(w, h, place.as_ref())])?;
    if out.trim_start().starts_with("error") {
        return Err(format!("hyprctl eval: {}", out.trim()));
    }
    Ok(())
}

/// Окно подогнано: запомнить его размер в пикселях (если изменился) и сразу применить правило.
fn remember_window() -> Result<(), String> {
    let parent = i64::from(std::os::unix::process::parent_id());
    let win = pick_window(&hyprctl(&["-j", "clients"])?, parent)
        .ok_or("our floating window is not found")?;
    let size = (win.width, win.height);
    // Правило ставим заново при каждом запуске: окно могли открыть в обход `open`, а перезагрузка
    // конфига Hyprland стирает правила в памяти. Файл переписываем, только если размер другой.
    if remembered_size() == Some(size) {
        return apply_size_rule(size.0, size.1);
    }
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let file = size_file().ok_or("no state directory")?;
    if let Some(dir) = file.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| e.to_string())?;
    }
    // Временный файл создаётся заново (не по оставшейся ссылке), права 0600, потом rename.
    let tmp = file.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    writeln!(f, "{} {}", size.0, size.1).map_err(|e| e.to_string())?;
    drop(f);
    std::fs::rename(&tmp, &file).map_err(|e| e.to_string())?;
    apply_size_rule(size.0, size.1)
}

/// Наше окно в `hyprctl -j clients`.
#[derive(Debug, PartialEq, Eq)]
struct Window {
    address: String,
    x: i64,
    y: i64,
    width: i64,
    height: i64,
    monitor: i64,
}

/// Монитор в логических пикселях и занятые панелями полосы по краям (`reserved`: слева, сверху,
/// справа, снизу) — так Hyprland сообщает место, которое заняла панель, где бы она ни стояла.
#[derive(Debug, PartialEq, Eq)]
struct Monitor {
    x: i64,
    y: i64,
    width: i64,
    height: i64,
    reserved: [i64; 4],
}

fn fit_window(term: (u16, u16), wanted: (u16, u16), own: &AtomicBool) -> Result<(), String> {
    let parent = i64::from(std::os::unix::process::parent_id());
    let win = pick_window(&hyprctl(&["-j", "clients"])?, parent)
        .ok_or("our floating window is not found")?;
    own.store(true, Ordering::Relaxed);
    let ws = crossterm::terminal::window_size().map_err(|e| e.to_string())?;
    // Размер ячейки в пикселях: терминал сообщает размер текстовой области (без полей).
    // Если не сообщает — по размеру окна (с полями ячейка выйдет чуть больше, догоним попыткой).
    let cell = |px: u16, cells: u16, win_px: i64| {
        if px > 0 && cells > 0 {
            f64::from(px) / f64::from(cells)
        } else {
            win_px as f64 / f64::from(cells.max(1))
        }
    };
    let cell_w = cell(ws.width, term.0, win.width);
    let cell_h = cell(ws.height, term.1, win.height);
    let mon =
        pick_monitor(&hyprctl(&["-j", "monitors"])?, win.monitor).ok_or("monitor is not found")?;
    let gap = edge_gap(
        &hyprctl(&["-j", "getoption", "general:gaps_out"])?,
        &hyprctl(&["-j", "getoption", "general:border_size"])?,
    );
    let free = mon.free_size(gap);
    if free.0 < MIN_FREE.0 || free.1 < MIN_FREE.1 {
        return Err(format!("not enough room on the monitor: {free:?}"));
    }
    let (w, h) = target_size(&win, term, wanted, (cell_w, cell_h), free);
    if (w, h) != (win.width, win.height) {
        dispatch(
            &format!(
                "hl.dsp.window.resize({{ window = \"address:{}\", x = {w}, y = {h} }})",
                win.address
            ),
            &[
                "resizewindowpixel",
                &format!("exact {w} {h},address:{}", win.address),
            ],
        )?;
    }
    let (x, y) = mon.corner(w, h, gap);
    if (x, y) != (win.x, win.y) {
        dispatch(
            &format!(
                "hl.dsp.window.move({{ window = \"address:{}\", x = {x}, y = {y} }})",
                win.address
            ),
            &[
                "movewindowpixel",
                &format!("exact {x} {y},address:{}", win.address),
            ],
        )?;
    }
    Ok(())
}

impl Monitor {
    /// Сколько места окну: монитор без панели и отступов с обеих сторон.
    fn free_size(&self, gap: i64) -> (i64, i64) {
        let [l, t, r, b] = self.reserved;
        let side = |len: i64, a: i64, z: i64| {
            len.saturating_sub(a)
                .saturating_sub(z)
                .saturating_sub(gap.saturating_mul(2))
        };
        (side(self.width, l, r), side(self.height, t, b))
    }

    /// Правый верхний угол под панелью (как у панелей Omarchy). Панель внизу — правый нижний угол
    /// над ней; панель справа — окно левее её. `gap` — отступ панелей Omarchy плюс рамка окна.
    fn corner(&self, w: i64, h: i64, gap: i64) -> (i64, i64) {
        let [_, top, right, bottom] = self.reserved;
        let far = |origin: i64, len: i64, bar: i64, size: i64| {
            origin
                .saturating_add(len)
                .saturating_sub(bar)
                .saturating_sub(gap)
                .saturating_sub(size)
        };
        let x = far(self.x, self.width, right, w);
        let y = if bottom > top {
            far(self.y, self.height, bottom, h)
        } else {
            self.y.saturating_add(top).saturating_add(gap)
        };
        (x, y)
    }
}

/// Отступ окна от края и от панели: как у панелей Omarchy (половина `general:gaps_out`)
/// плюс рамка окна Hyprland (`general:border_size`) — у панелей её нет, а у окна она снаружи.
fn edge_gap(gaps_out_json: &str, border_json: &str) -> i64 {
    let value = |json: &str| serde_json::from_str::<Value>(json).ok();
    let gaps_out = value(gaps_out_json)
        .and_then(|v| {
            let css = v.get("css").and_then(Value::as_str).map(str::to_string);
            css.and_then(|c| {
                c.split_whitespace()
                    .next()
                    .and_then(|n| n.parse::<f64>().ok())
            })
            .or_else(|| v.get("int").and_then(Value::as_f64))
        })
        .unwrap_or(10.0);
    let border = value(border_json)
        .and_then(|v| v.get("int").and_then(Value::as_i64))
        .unwrap_or(2);
    // Отступы в сотни пикселей не бывают: ограничиваем, чтобы странное значение не увело окно.
    ((gaps_out / 2.0).round() as i64).clamp(0, 200) + border.clamp(0, 50)
}

/// Окно нашего класса, чей процесс — наш родитель (терминал), и только плавающее.
/// Подошло больше одного (терминал-сервер держит все окна в одном процессе) — не угадываем.
fn pick_window(clients_json: &str, parent_pid: i64) -> Option<Window> {
    let clients: Vec<Value> = serde_json::from_str(clients_json).ok()?;
    let mut found = clients.iter().filter_map(|c| {
        let ours = c.get("class")?.as_str()? == APP_CLASS
            && c.get("pid")?.as_i64()? == parent_pid
            && c.get("floating")?.as_bool()?;
        let address = c.get("address")?.as_str()?;
        if !ours || !valid_address(address) {
            return None;
        }
        let size = c.get("size")?.as_array()?;
        let at = c.get("at")?.as_array()?;
        Some(Window {
            address: address.to_string(),
            x: at.first()?.as_i64()?,
            y: at.get(1)?.as_i64()?,
            width: size.first()?.as_i64()?,
            height: size.get(1)?.as_i64()?,
            monitor: c.get("monitor").and_then(Value::as_i64).unwrap_or(0),
        })
    });
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// Адрес окна попадает в Lua-команду — только `0x` и шестнадцатеричные цифры.
fn valid_address(s: &str) -> bool {
    s.strip_prefix("0x").is_some_and(|hex| {
        (1..=16).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

/// Монитор окна в логических пикселях; `None` — не найден.
fn pick_monitor(monitors_json: &str, id: i64) -> Option<Monitor> {
    let monitors: Vec<Value> = serde_json::from_str(monitors_json).ok()?;
    monitors
        .iter()
        .find(|m| m.get("id").and_then(Value::as_i64) == Some(id))
        .and_then(parse_monitor)
}

/// Монитор с фокусом — на нём откроется новое окно.
fn focused_monitor(monitors_json: &str) -> Option<Monitor> {
    let monitors: Vec<Value> = serde_json::from_str(monitors_json).ok()?;
    monitors
        .iter()
        .find(|m| m.get("focused").and_then(Value::as_bool) == Some(true))
        .and_then(parse_monitor)
}

fn parse_monitor(m: &Value) -> Option<Monitor> {
    let num = |key: &str| m.get(key).and_then(Value::as_f64);
    let scale = num("scale").filter(|s| *s > 0.0)?;
    let (mut w, mut h) = (num("width")? / scale, num("height")? / scale);
    // Повёрнутый на 90° или 270° монитор: ширина и высота меняются местами.
    if m.get("transform")
        .and_then(Value::as_i64)
        .is_some_and(|t| t % 2 == 1)
    {
        std::mem::swap(&mut w, &mut h);
    }
    // Нелепые размеры (отрицательные, не числа) — монитор не используем.
    if !(w >= 1.0 && h >= 1.0 && w.is_finite() && h.is_finite()) {
        return None;
    }
    let reserved = m.get("reserved").and_then(Value::as_array);
    // Полоса панели не бывает больше самого монитора.
    let side = |i: usize, limit: f64| {
        reserved
            .and_then(|r| r.get(i))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .clamp(0, limit as i64)
    };
    Some(Monitor {
        x: m.get("x")?.as_i64()?,
        y: m.get("y")?.as_i64()?,
        width: w as i64,
        height: h as i64,
        reserved: [side(0, w), side(1, h), side(2, w), side(3, h)],
    })
}

/// Новый размер окна в пикселях: сдвиг на разницу в ячейках (поля терминала не масштабируются),
/// с округлением в большую сторону — чтобы не потерять последнюю строку или столбец.
fn target_size(
    win: &Window,
    term: (u16, u16),
    wanted: (u16, u16),
    cell: (f64, f64),
    max: (i64, i64),
) -> (i64, i64) {
    let shift = |want: u16, have: u16, px: f64| {
        let delta = (f64::from(want) - f64::from(have)) * px;
        delta.ceil() as i64
    };
    let mut w = win.width.saturating_add(shift(wanted.0, term.0, cell.0));
    let mut h = win.height.saturating_add(shift(wanted.1, term.1, cell.1));
    w = w.min(max.0);
    h = h.min(max.1);
    (w.max(1), h.max(1))
}

fn hyprctl(args: &[&str]) -> Result<String, String> {
    let out = Command::new("hyprctl")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("hyprctl: {e}"))?;
    String::from_utf8(out.stdout).map_err(|_| "hyprctl: not UTF-8".to_string())
}

/// Команда Hyprland: сначала Lua-синтаксис (Hyprland с конфигом на Lua), при ошибке — старый.
/// `hyprctl` всегда выходит с кодом 0, ошибку видно только по тексту ответа.
fn dispatch(lua: &str, legacy: &[&str]) -> Result<(), String> {
    let failed = |out: &str| {
        let out = out.trim_start();
        out.starts_with("error") || out.starts_with("info")
    };
    let out = hyprctl(&["dispatch", lua])?;
    if !failed(&out) {
        return Ok(());
    }
    let mut args = vec!["dispatch"];
    args.extend_from_slice(legacy);
    let old = hyprctl(&args)?;
    if failed(&old) {
        return Err(format!("hyprctl dispatch: {}", out.trim()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_only_our_floating_window_of_our_terminal() {
        let json = r#"[
            {"address":"0x1","class":"foot","pid":100,"floating":true,"at":[0,0],"size":[800,600],"monitor":0},
            {"address":"0x2","class":"org.omarchy.omarchy-hotspot","pid":999,"floating":true,"at":[0,0],"size":[800,600],"monitor":0},
            {"address":"0x3","class":"org.omarchy.omarchy-hotspot","pid":100,"floating":false,"at":[0,0],"size":[800,600],"monitor":0},
            {"address":"0x4f","class":"org.omarchy.omarchy-hotspot","pid":100,"floating":true,"at":[842,420],"size":[875,600],"monitor":1}
        ]"#;
        let w = pick_window(json, 100).unwrap();
        // Два наших окна у одного процесса — не угадываем, какое из них это.
        let two = json.replace("\"floating\":false", "\"floating\":true");
        assert!(pick_window(&two, 100).is_none());
        assert_eq!(
            (w.address.as_str(), w.x, w.y, w.width, w.height, w.monitor),
            ("0x4f", 842, 420, 875, 600, 1)
        );
        assert!(pick_window(json, 5).is_none());
        assert!(pick_window("not json", 100).is_none());
        let evil = r#"[{"address":"0x1\" }) os.execute(\"id","class":"org.omarchy.omarchy-hotspot","pid":1,"floating":true,"size":[1,1]}]"#;
        assert!(
            pick_window(evil, 1).is_none(),
            "адрес с кавычками пропущен в Lua"
        );
    }

    #[test]
    fn address_format() {
        assert!(valid_address("0x55ee4743ed80"));
        assert!(!valid_address("55ee"));
        assert!(!valid_address("0x"));
        assert!(!valid_address("0x12 34"));
        assert!(!valid_address("0x12345678901234567"));
    }

    #[test]
    fn target_moves_by_cells_and_respects_the_monitor() {
        let win = Window {
            address: "0x1".into(),
            x: 0,
            y: 0,
            width: 875,
            height: 600,
            monitor: 0,
        };
        // 120×37 ячеек по 7×15.8 px → нужно 92×24.
        let (w, h) = target_size(&win, (120, 37), (92, 24), (7.0, 15.8), (4000, 4000));
        assert_eq!((w, h), (875 - 28 * 7, 600 - 205));
        // Больше свободного места не растягиваем.
        let (w, h) = target_size(&win, (120, 37), (400, 100), (7.0, 15.8), (1000, 700));
        assert_eq!((w, h), (1000, 700));
    }

    fn monitors() -> &'static str {
        r#"[
            {"id":0,"x":0,"y":0,"width":2560,"height":1440,"scale":1,"transform":0,"reserved":[0,26,0,0],"focused":true},
            {"id":1,"x":2560,"y":0,"width":3840,"height":2160,"scale":2.0,"transform":0,"reserved":[0,0,0,30]},
            {"id":2,"x":0,"y":1440,"width":1920,"height":1080,"scale":1,"transform":1,"reserved":[0,0,40,0]}
        ]"#
    }

    #[test]
    fn corner_is_under_the_bar_wherever_it_is() {
        let gap = 7; // 10/2 + рамка 2
        // Панель сверху (как у Omarchy по умолчанию): правый верхний угол под ней.
        let top = pick_monitor(monitors(), 0).unwrap();
        assert_eq!(top.corner(700, 400, gap), (2560 - 7 - 700, 26 + 7));
        assert_eq!(top.free_size(gap), (2560 - 14, 1440 - 26 - 14));
        // Панель снизу, масштаб 2: правый нижний угол над панелью, в логических пикселях.
        let bottom = pick_monitor(monitors(), 1).unwrap();
        assert_eq!((bottom.width, bottom.height), (1920, 1080));
        assert_eq!(
            bottom.corner(700, 400, gap),
            (2560 + 1920 - 7 - 700, 1080 - 30 - 7 - 400)
        );
        // Панель справа, монитор повёрнут: окно левее панели, сверху.
        let right = pick_monitor(monitors(), 2).unwrap();
        assert_eq!((right.width, right.height), (1080, 1920));
        assert_eq!(right.corner(700, 400, gap), (1080 - 40 - 7 - 700, 1440 + 7));
        assert!(pick_monitor(monitors(), 9).is_none());
        // Отрицательный размер монитора — монитор не используем (и не падаем).
        let negative = r#"[{"id":0,"x":0,"y":0,"width":-800,"height":600,"scale":1,"reserved":[0,26,0,0],"focused":true}]"#;
        assert!(pick_monitor(negative, 0).is_none());
        assert!(focused_monitor(negative).is_none());
        // Запомненный размер шире маленького монитора — ужимаем до свободного места.
        assert_eq!(fit_on_monitor((1800, 900), (1266, 700)), (1266, 700));
        assert_eq!(fit_on_monitor((657, 396), (2546, 1400)), (657, 396));
        assert_eq!(fit_on_monitor((657, 396), (-50, 10)), (100, 100));
        // Нелепые данные: полоса панели больше монитора — обрезается, места для окна нет.
        let odd =
            r#"[{"id":0,"x":0,"y":0,"width":800,"height":600,"scale":1,"reserved":[0,99999,0,0]}]"#;
        let m = pick_monitor(odd, 0).unwrap();
        assert_eq!(m.reserved[1], 600);
        assert!(m.free_size(gap).1 < MIN_FREE.1);
    }

    #[test]
    fn gap_is_half_of_gaps_out_plus_border() {
        let gaps = r#"{"option":"general:gaps_out","css":"10 10 10 10","set":true}"#;
        let border = r#"{"option":"general:border_size","int":2,"set":true}"#;
        assert_eq!(edge_gap(gaps, border), 7);
        assert_eq!(edge_gap(r#"{"int":20}"#, r#"{"int":0}"#), 10);
        // hyprctl не ответил — значения Omarchy по умолчанию.
        assert_eq!(edge_gap("", ""), 7);
    }

    #[test]
    fn places_once_refits_until_the_size_matches_then_remembers_it() {
        let past = Instant::now() - SETTLE * 2;
        let mut f = Fitter {
            disabled: true,
            ..Fitter::default()
        };
        f.want((92, 24), true);
        f.wanted_since = Some(past);
        assert_eq!(
            f.decide((120, 37), Instant::now()),
            None,
            "без Hyprland ничего не делаем"
        );

        let mut f = Fitter::default();
        f.want((92, 24), true);
        f.wanted_since = Some(past);
        f.last_term = Some((92, 24));
        f.term_changed = Some(past);
        // Размер уже тот, но окно всё равно ставим в угол — один раз, потом запоминаем размер.
        assert_eq!(
            f.decide((92, 24), Instant::now()),
            Some(Pass::Fit((92, 24)))
        );
        assert_eq!(f.decide((92, 24), Instant::now()), Some(Pass::Remember));
        assert_eq!(f.decide((92, 24), Instant::now()), None);
        assert_eq!(f.done_for, Some((92, 24)));
        // Пользователь растянул окно руками — содержимое то же, окно не трогаем.
        assert_eq!(f.decide((130, 40), Instant::now() + SETTLE * 2), None);

        // Открыли QR (не обычный размер): подгоняем, но не запоминаем; попыток не больше MAX_ATTEMPTS.
        f.want((60, 12), false);
        f.wanted_since = Some(past);
        let later = Instant::now() + SETTLE * 4;
        let passes: Vec<Pass> = (0..10).filter_map(|_| f.decide((130, 40), later)).collect();
        assert_eq!(passes.len(), usize::from(MAX_ATTEMPTS));
        assert!(!passes.contains(&Pass::Remember));
    }

    #[test]
    fn remembered_size_file_and_rule() {
        assert_eq!(parse_size("657 396\n"), Some((657, 396)));
        for bad in [
            "",
            "657",
            "657 x",
            "657 396 1",
            "5 396",
            "657 99999",
            "-657 396",
        ] {
            assert_eq!(parse_size(bad), None, "{bad:?}");
        }
        assert_eq!(
            size_rule_lua(657, 396, None),
            r#"if omarchy_hotspot_size_rule then omarchy_hotspot_size_rule:set_enabled(false) end; omarchy_hotspot_size_rule = hl.window_rule({ match = { class = "^(org\\.omarchy\\.omarchy-hotspot)$" }, size = { 657, 396 } })"#
        );
        // Место — числами от края монитора, без `window_w`.
        let top = focused_monitor(monitors()).unwrap();
        let place = corner_exprs(&top, 657, 396, 7);
        assert_eq!(place, ["(monitor_w-664)".to_string(), "(33)".to_string()]);
        assert!(
            size_rule_lua(657, 396, Some(&place))
                .ends_with(r#"size = { 657, 396 }, move = { "(monitor_w-664)", "(33)" } })"#)
        );
        // Панель снизу — от нижнего края.
        let bottom = pick_monitor(monitors(), 1).unwrap();
        assert_eq!(
            corner_exprs(&bottom, 657, 396, 7),
            ["(monitor_w-664)".to_string(), "(monitor_h-433)".to_string()]
        );
        // То же место, что ставит подгонка: x = ширина монитора − (ширина окна + отступ).
        assert_eq!(top.corner(657, 396, 7), (2560 - 664, 33));
        assert!(
            apply_size_rule(5, 396).is_err(),
            "нелепый размер ушёл в Hyprland"
        );
    }
}
