//! Мелкие переиспользуемые куски отрисовки: рамка окна, статус, значки, QR, подсказки.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Sparkline};

use omarchy_hotspot_core::backend::{Band, HotspotState};
use omarchy_hotspot_core::devices::{Device, DeviceStatus, bars_str, signal_bars};
use omarchy_hotspot_core::traffic::{format_bytes, format_rate};
use omarchy_hotspot_core::{Lang, Msg, t, tf};

use crate::hotspot_ctx::{band_name, security_name};

use super::mouse::{Hits, Target};
use super::state::{AppState, Busy, Confirm, EditField, Mode};

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub fn spinner_char(tick: usize) -> char {
    SPINNER[tick % SPINNER.len()]
}

/// Слишком маленькое окно: одна строка по центру вместо содержимого.
pub fn draw_too_small(f: &mut Frame, area: Rect, lang: Lang) {
    let p = Paragraph::new(t(lang, Msg::TuiTooSmall))
        .style(Style::new().fg(Color::Red))
        .alignment(Alignment::Center);
    f.render_widget(p, area);
}

/// Внешняя рамка окна с заголовком и вкладками режима. Возвращает область содержимого.
pub fn draw_frame(f: &mut Frame, area: Rect, app: &AppState, hits: &mut Hits) -> Rect {
    let lang = app.lang;
    let left = Line::from(vec![
        Span::styled(" 󰀂 ", Style::new().fg(Color::Blue)),
        Span::styled(
            t(lang, Msg::TuiTitle),
            Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ]);
    let tab = |label: &str, active: bool| {
        if active {
            Span::styled(
                format!(" {label} "),
                Style::new()
                    .fg(Color::Black)
                    .bg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {label} "), Style::new().fg(Color::DarkGray))
        }
    };
    let right = Line::from(vec![
        tab(t(lang, Msg::TuiTabSimple), app.mode == Mode::Simple),
        Span::raw(" "),
        tab(t(lang, Msg::TuiTabAdvanced), app.mode == Mode::Advanced),
        Span::raw(" "),
    ])
    .right_aligned();

    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Blue))
        .padding(Padding::horizontal(1))
        .title(left)
        .title(right);
    let inner = block.inner(area);
    f.render_widget(block, area);
    for (msg, mode) in [
        (Msg::TuiTabSimple, Mode::Simple),
        (Msg::TuiTabAdvanced, Mode::Advanced),
    ] {
        let label = format!(" {} ", t(lang, msg));
        hits.click_label(f.buffer_mut(), area.y, area, &label, Target::Tab(mode));
    }
    inner
}

/// Строки блока «Статус»: индикатор, сводка сети, значки защиты, пароль, ошибка/уведомление.
pub fn status_lines(app: &AppState) -> Vec<Line<'static>> {
    let lang = app.lang;
    let mut lines = Vec::new();

    let (color, text) = if let Some(busy) = app.busy {
        (
            Color::Blue,
            format!("{} {}", spinner_char(app.spinner_tick), busy.label(lang)),
        )
    } else {
        match &app.status.state {
            HotspotState::On { .. } => (Color::Green, t(lang, Msg::TuiStatusOn).to_string()),
            HotspotState::Starting => (Color::Yellow, t(lang, Msg::TuiStatusStarting).to_string()),
            HotspotState::Off | HotspotState::Error(_) => {
                (Color::Red, t(lang, Msg::TuiStatusOff).to_string())
            }
        }
    };
    lines.push(Line::from(vec![
        Span::styled("● ", Style::new().fg(color)),
        Span::styled(text, Style::new().fg(color).add_modifier(Modifier::BOLD)),
    ]));

    if app.is_on() {
        // Диапазон берём из настоящего канала: в конфиге обычно стоит «авто», и показывать
        // пользователю «авто» вместо «5 ГГц» бессмысленно (так же делает `status` в CLI).
        let band = match app.status.channel {
            Some(ch) if ch <= 14 => Band::Ghz2_4,
            Some(_) => Band::Ghz5,
            None => app.cfg.hotspot.band,
        };
        let mut parts = vec![
            app.cfg.hotspot.ssid.clone(),
            band_name(lang, band).to_string(),
        ];
        if let Some(ch) = app.status.channel {
            parts.push(format!("{} {ch}", t(lang, Msg::LabelChannel)));
        }
        if let HotspotState::On {
            since: Some(since), ..
        } = &app.status.state
            && let Ok(elapsed) = since.elapsed()
        {
            parts.push(format_uptime(elapsed.as_secs()));
        }
        lines.push(Line::from(Span::styled(
            parts.join(" · "),
            Style::new().fg(Color::DarkGray),
        )));
        lines.push(badges_line(app));
    } else {
        lines.push(Line::from(Span::styled(
            t(lang, Msg::TuiHintTurnOn),
            Style::new().fg(Color::DarkGray),
        )));
    }
    // Пароль от уже существующего профиля можно посмотреть и при выключенной раздаче
    // (как `omarchy-hotspot password` в CLI), поэтому строка не зависит от `is_on()`.
    lines.push(password_line(app));

    if app.approval_unavailable() {
        lines.push(Line::from(Span::styled(
            format!("⚠ {}", t(lang, Msg::TuiDaemonOffline)),
            Style::new().fg(Color::Yellow),
        )));
    }
    if let Some(err) = &app.error {
        lines.push(Line::from(Span::styled(
            format!("✗ {err}"),
            Style::new().fg(Color::Red),
        )));
    } else if let Some(n) = &app.notice {
        lines.push(Line::from(Span::styled(
            n.clone(),
            Style::new().fg(Color::Yellow),
        )));
    }
    lines
}

/// «ч:мм:сс» — сразу после включения «0 ч 0 мин» выглядело так, будто часы стоят.
pub fn format_uptime(secs: u64) -> String {
    format!("{}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
}

fn badges_line(app: &AppState) -> Line<'static> {
    let lang = app.lang;
    let mut spans = vec![Span::styled(
        format!("[{}]", security_name(lang, app.cfg.hotspot.security)),
        Style::new().fg(Color::Blue),
    )];
    if app.cfg.hotspot.ap_isolation {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("[{}]", t(lang, Msg::TuiBadgeIsolation)),
            Style::new().fg(Color::Magenta),
        ));
    }
    if app.daemon_approval {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("[{}]", t(lang, Msg::TuiBadgeApproval)),
            Style::new().fg(Color::Cyan),
        ));
    }
    spans.push(Span::raw(" "));
    if app.cfg.hotspot.guests_can_reach_pc {
        spans.push(Span::styled(
            format!("[{}]", t(lang, Msg::TuiBadgeGuestsReach)),
            Style::new().fg(Color::Yellow),
        ));
    } else {
        spans.push(Span::styled(
            format!("[{}]", t(lang, Msg::TuiBadgeHidden)),
            Style::new().fg(Color::Green),
        ));
    }
    Line::from(spans)
}

/// Пароль скрыт фиксированным числом точек, чтобы не выдавать его длину.
fn password_line(app: &AppState) -> Line<'static> {
    let lang = app.lang;
    let value = if app.show_password {
        match &app.password {
            Some(p) => p.expose().to_string(),
            None if app.password_absent => "—".to_string(),
            None => "…".to_string(),
        }
    } else if app.password_absent {
        "—".to_string()
    } else {
        "••••••••••".to_string()
    };
    Line::from(vec![
        Span::styled(
            format!("{}: ", t(lang, Msg::LabelPassword)),
            Style::new().fg(Color::DarkGray),
        ),
        Span::raw(value),
    ])
}

/// Блок «Статус»; справа в рамке — переключатель ВКЛ/ВЫКЛ (как в макете `docs/UI.md`).
pub fn status_block(app: &AppState) -> Block<'static> {
    let (label, color) = toggle_label(app);
    let mut spans = Vec::new();
    if let Some(qr) = qr_button_label(app) {
        let style = if app.show_qr {
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(Color::Blue)
        };
        spans.push(Span::styled(qr, style));
    }
    spans.push(Span::styled(
        label,
        Style::new().fg(color).add_modifier(Modifier::BOLD),
    ));
    titled_block(t(app.lang, Msg::TuiSectionStatus)).title(Line::from(spans).right_aligned())
}

/// Кнопка `[ QR ]` в рамке «Статус» — только при включённой раздаче.
pub fn qr_button_label(app: &AppState) -> Option<String> {
    app.is_on().then(|| " [ QR ] ".to_string())
}

/// Надпись переключателя в рамке «Статус» — по ней же ищется место для клика.
pub fn toggle_label(app: &AppState) -> (String, Color) {
    let (label, color) = if app.is_on() {
        (t(app.lang, Msg::TuiToggleOn), Color::Green)
    } else {
        (t(app.lang, Msg::TuiToggleOff), Color::Red)
    };
    (format!(" [ {label} ] "), color)
}

pub fn titled_block(title: &str) -> Block<'static> {
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Blue))
        // Отступ по бокам: иначе текст лип прямо к рамке.
        .padding(Padding::horizontal(1))
        .title(Span::styled(
            format!(" {title} "),
            Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
        ))
}

/// Значок устройства (Nerd Font). Тип устройства по MAC не определить, поэтому он один.
pub const DEVICE_ICON: &str = "󰄜";
/// Колонки списка устройств: имя и IP выравниваем, иначе строки «пляшут».
const NAME_COL: usize = 18;
const IP_COL: usize = 15;
/// Минимальная ширина, при которой график ещё что-то показывает.
const MIN_SPARK_WIDTH: u16 = 8;

/// Блок «Трафик»: график общей скорости слева, цифры справа.
pub fn draw_traffic(f: &mut Frame, area: Rect, app: &AppState) {
    let block = titled_block(t(app.lang, Msg::TuiSectionTraffic));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let text = traffic_line(app);
    let text_w = text.width() as u16;
    let spark_w = inner.width.saturating_sub(text_w + 2);
    if spark_w < MIN_SPARK_WIDTH {
        f.render_widget(Paragraph::new(text), inner);
        return;
    }
    let [spark_area, text_area] =
        Layout::horizontal([Constraint::Length(spark_w), Constraint::Min(text_w)])
            .spacing(2)
            .areas(inner);
    let history = app.traffic.history();
    let data = &history[history.len().saturating_sub(spark_area.width as usize)..];
    f.render_widget(
        Sparkline::default()
            .data(data)
            .style(Style::new().fg(Color::Cyan)),
        spark_area,
    );
    f.render_widget(Paragraph::new(text).alignment(Alignment::Right), text_area);
}

/// «↓ 14.2 МБ/с   ↑ 1.1 МБ/с   3.4 ГБ за сеанс».
pub fn traffic_line(app: &AppState) -> Line<'static> {
    let lang = app.lang;
    let tr = &app.traffic;
    Line::from(vec![
        Span::styled("↓ ", Style::new().fg(Color::Cyan)),
        Span::raw(format_rate(tr.down_bps, lang)),
        Span::raw("   "),
        Span::styled("↑ ", Style::new().fg(Color::Magenta)),
        Span::raw(format_rate(tr.up_bps, lang)),
        Span::raw("   "),
        Span::styled(
            format!(
                "{} {}",
                format_bytes(tr.total_bytes(), lang),
                t(lang, Msg::TuiTrafficSession)
            ),
            Style::new().fg(Color::DarkGray),
        ),
    ])
}

/// Блок «Устройства»: сколько влезает по высоте (с прокруткой), остальное — строкой «и ещё N».
pub fn draw_devices(f: &mut Frame, area: Rect, app: &AppState) {
    let title = devices_title(app.lang, app.devices.len());
    let block = titled_block(&title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    // Без переноса: лишние столбцы убирает `device_line`, строка не разъезжается на две.
    f.render_widget(
        Paragraph::new(device_lines(
            app,
            inner.height as usize,
            inner.width as usize,
        )),
        inner,
    );
}

/// Сколько строк нужно списку устройств без рамки (при `max_rows` строк на устройства).
pub fn device_rows_needed(app: &AppState, max_rows: usize) -> usize {
    if !app.is_on() {
        return 1;
    }
    let n = app.devices.len();
    let hint = usize::from(!app.helper_installed);
    if n == 0 {
        1 + hint
    } else if n > max_rows {
        max_rows + 1
    } else {
        n + hint
    }
}

pub fn devices_title(lang: Lang, count: usize) -> String {
    tf(lang, Msg::TuiSectionDevicesFmt, &[&count.to_string()])
}

pub fn device_lines(app: &AppState, rows: usize, width: usize) -> Vec<Line<'static>> {
    let lang = app.lang;
    if !app.is_on() {
        return vec![hint_line(t(lang, Msg::DevicesNeedOn))];
    }
    if app.devices.is_empty() {
        let mut lines = vec![Line::from(Span::styled(
            t(lang, Msg::TuiDevicesEmpty),
            Style::new().fg(Color::DarkGray),
        ))];
        if !app.helper_installed {
            lines.push(hint_line(t(lang, Msg::TuiDevicesNoLeases)));
        }
        return lines;
    }
    // Одну строку оставляем под «и ещё N», если список не помещается; колесо и ↑↓ листают.
    let n = app.devices.len();
    let (shown, hidden) = if n > rows && rows > 0 {
        (rows.saturating_sub(1), n - rows + 1)
    } else {
        (n, 0)
    };
    let first = app.simple_dev_scroll.min(n - shown);
    let mut lines: Vec<Line<'static>> = app.devices[first..first + shown]
        .iter()
        .map(|d| device_line(app, d, width))
        .collect();
    if hidden > 0 {
        lines.push(hint_line(&tf(
            lang,
            Msg::TuiDevicesMoreFmt,
            &[&hidden.to_string()],
        )));
    } else if !app.helper_installed && lines.len() < rows {
        lines.push(hint_line(t(lang, Msg::TuiDevicesNoLeases)));
    }
    lines
}

fn hint_line(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::new().fg(Color::DarkGray),
    ))
}

/// «󰄜 Pixel-8            ▂▄▆█  10.42.0.34     ↓ 12 МБ/с  ↑ 0.4 МБ/с».
/// В узком окне столбцы убираются справа налево: сначала скорость, потом IP.
fn device_line(app: &AppState, d: &Device, width: usize) -> Line<'static> {
    let lang = app.lang;
    let (down, up) = app.traffic.rate_of(&d.mac);
    let bars = d.signal_dbm.map_or(0, signal_bars);
    let bar_color = match bars {
        4 | 3 => Color::Green,
        2 => Color::Yellow,
        _ => Color::Red,
    };
    let ip = d.ip.map(|ip| ip.to_string()).unwrap_or_default();
    let mut spans = vec![
        Span::styled(format!("{DEVICE_ICON} "), Style::new().fg(Color::Blue)),
        Span::raw(pad(&d.display_name(), NAME_COL)),
        Span::raw(" "),
        Span::styled(bars_str(bars), Style::new().fg(bar_color)),
    ];
    let ip_part = vec![
        Span::raw("  "),
        Span::styled(pad(&ip, IP_COL), Style::new().fg(Color::DarkGray)),
    ];
    let rates = vec![
        Span::styled(" ↓ ", Style::new().fg(Color::Cyan)),
        Span::raw(format_rate(down, lang)),
        Span::styled("  ↑ ", Style::new().fg(Color::Magenta)),
        Span::raw(format_rate(up, lang)),
    ];
    let mark = match d.status {
        DeviceStatus::Pending => Some(Span::styled("  ⧗", Style::new().fg(Color::Yellow))),
        DeviceStatus::Blocked => Some(Span::styled("  ⛔", Style::new().fg(Color::Red))),
        DeviceStatus::Allowed => None,
    };
    let w = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
    let base = w(&spans) + mark.as_ref().map_or(0, Span::width);
    if base + w(&ip_part) <= width {
        let with_rates = base + w(&ip_part) + w(&rates) <= width;
        spans.extend(ip_part);
        if with_rates {
            spans.extend(rates);
        }
    }
    spans.extend(mark);
    Line::from(spans)
}

/// Обрезает по числу символов и дополняет пробелами до ширины колонки.
pub fn pad(text: &str, width: usize) -> String {
    let mut out: String = text.chars().take(width).collect();
    let len = out.chars().count();
    out.extend(std::iter::repeat_n(' ', width.saturating_sub(len)));
    out
}

/// QR чёрным по белому (как в `omarchy-hotspot qr`), с подписью сети и подсказкой.
pub fn qr_paragraph(app: &AppState) -> Paragraph<'static> {
    let lang = app.lang;
    let Some(lines) = &app.qr else {
        let text = if app.busy == Some(Busy::LoadingPassword) || app.password.is_none() {
            format!(
                "{} {}",
                spinner_char(app.spinner_tick),
                t(lang, Msg::TuiBusyPassword)
            )
        } else {
            "—".to_string()
        };
        return Paragraph::new(Line::from(Span::styled(
            text,
            Style::new().fg(Color::DarkGray),
        )))
        .alignment(Alignment::Center);
    };
    let out: Vec<Line<'static>> = lines
        .iter()
        .map(|l| {
            Line::from(Span::styled(
                l.clone(),
                Style::new().fg(Color::Black).bg(Color::White),
            ))
        })
        .collect();
    Paragraph::new(out).alignment(Alignment::Center)
}

/// Минимальная ширина блока QR (когда сам код ещё не готов — под спиннер).
const MIN_QR_BOX_WIDTH: u16 = 22;
const LOADING_QR_BOX_HEIGHT: u16 = 3;

/// Ширина блока QR: сам код + отступы внутри рамки + рамка.
pub fn qr_box_width(app: &AppState) -> u16 {
    let code = app
        .qr
        .as_ref()
        .and_then(|lines| lines.iter().map(|l| l.chars().count() as u16).max())
        .unwrap_or(0);
    (code + 4).max(MIN_QR_BOX_WIDTH)
}

/// Высота блока QR, без которой его показывать нельзя (иначе обрежется).
pub fn qr_box_height(app: &AppState) -> u16 {
    match &app.qr {
        // код + рамка (подсказка «наведи камеру» — в нижней рамке)
        Some(lines) => lines.len() as u16 + 2,
        None => LOADING_QR_BOX_HEIGHT,
    }
}

/// Блок «Подключить» с QR по центру; подсказка — в нижней рамке, чтобы не занимать строки.
pub fn draw_qr_block(f: &mut Frame, area: Rect, app: &AppState) {
    let block = titled_block(t(app.lang, Msg::TuiSectionConnect)).title_bottom(
        Line::from(Span::styled(
            format!(" {} ", t(app.lang, Msg::TuiQrShortHint)),
            Style::new().fg(Color::DarkGray),
        ))
        .centered(),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    // Код ставим по центру, чтобы он не «висел» у верхней рамки.
    let content_h = app.qr.as_ref().map_or(1, |l| l.len() as u16);
    let target = Rect {
        y: inner.y + inner.height.saturating_sub(content_h) / 2,
        height: content_h.min(inner.height),
        ..inner
    };
    f.render_widget(qr_paragraph(app), target);
}

/// QR поверх окна, когда рядом с блоками ему места нет. Не влезает и так — просим увеличить окно.
pub fn draw_qr_overlay(f: &mut Frame, area: Rect, app: &AppState) {
    f.render_widget(Clear, area);
    let (w, h) = (qr_box_width(app), qr_box_height(app));
    if area.width >= w && area.height >= h {
        draw_qr_block(f, centered_rect(area, w, h), app);
    } else {
        let text = vec![
            Line::from(Span::styled(
                t(app.lang, Msg::TuiQrTooSmall),
                Style::new().fg(Color::Yellow),
            )),
            Line::from(Span::styled(
                t(app.lang, Msg::TuiQrZoomClose),
                Style::new().fg(Color::DarkGray),
            )),
        ];
        let rows = wrap_line(text[0].clone(), area.width as usize).len() as u16 + 1;
        let y = area.y + area.height.saturating_sub(rows) / 2;
        f.render_widget(
            Paragraph::new(text)
                .alignment(Alignment::Center)
                .wrap(ratatui::widgets::Wrap { trim: true }),
            Rect {
                y,
                height: rows.min(area.height),
                ..area
            },
        );
    }
}

pub fn hints_line(app: &AppState) -> Line<'static> {
    let lang = app.lang;
    let text = match app.mode {
        Mode::Simple if app.editing.is_some() => t(lang, Msg::TuiEditHint),
        Mode::Simple => t(lang, Msg::TuiHintsSimple),
        Mode::Advanced => t(lang, Msg::AdvKeysSections),
    };
    Line::from(Span::styled(text, Style::new().fg(Color::DarkGray)))
}

/// Оверлей `e`: имя сети и пароль. Рисуется поверх остального окна.
pub fn draw_editor(f: &mut Frame, area: Rect, app: &AppState) {
    let width = area.width.min(OVERLAY_WIDTH);
    // Ширина текста внутри рамки: по столбцу с каждой стороны.
    let Some(lines) = editor_lines(app, width.saturating_sub(2) as usize) else {
        return;
    };
    let lang = app.lang;
    // Высота — под содержимое: иначе подсказка и ошибка обрезались бы рамкой.
    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = centered_rect(area, width, height);
    f.render_widget(Clear, popup);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Blue))
        .title(Span::styled(
            format!(" {} ", t(lang, Msg::TuiEditTitle)),
            Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    f.render_widget(Paragraph::new(lines), inner);
}

/// Ширина всплывающих окон (изменение сети, вопросы).
pub const OVERLAY_WIDTH: u16 = 60;

/// Размер всплывающего окна, если оно открыто: окну программы нужно не меньше.
pub fn overlay_size(app: &AppState) -> Option<(u16, u16)> {
    if let Some(lines) = editor_lines(app, (OVERLAY_WIDTH - 2) as usize) {
        return Some((OVERLAY_WIDTH, lines.len() as u16 + 2));
    }
    confirm_content(app, (OVERLAY_WIDTH - 4) as usize)
        .map(|(_, lines)| (OVERLAY_WIDTH, lines.len() as u16 + 2))
}

fn editor_lines(app: &AppState, wrap_at: usize) -> Option<Vec<Line<'static>>> {
    let editor = app.editing.as_ref()?;
    let lang = app.lang;

    let field_style = |focused: bool| {
        if focused {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new()
        }
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{}: ", t(lang, Msg::LabelNetwork)),
                Style::new().fg(Color::DarkGray),
            ),
            Span::styled(
                editor.ssid.clone(),
                field_style(editor.field == EditField::Ssid),
            ),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                format!("{}: ", t(lang, Msg::LabelPassword)),
                Style::new().fg(Color::DarkGray),
            ),
            if editor.password.is_empty() {
                Span::styled(
                    t(lang, Msg::TuiEditPasswordPlaceholder),
                    field_style(editor.field == EditField::Password).fg(Color::DarkGray),
                )
            } else {
                Span::styled(
                    "•".repeat(editor.password.chars().count()),
                    field_style(editor.field == EditField::Password),
                )
            },
        ]),
    ];
    if app.cfg.access.approval_required {
        lines.push(Line::raw(""));
        for l in wrap_words(t(lang, Msg::TuiEditApprovalHint), wrap_at) {
            lines.push(Line::from(Span::styled(
                l,
                Style::new().fg(Color::DarkGray),
            )));
        }
    }
    if let Some(err) = &editor.error {
        lines.push(Line::raw(""));
        for l in wrap_words(&format!("✗ {err}"), wrap_at) {
            lines.push(Line::from(Span::styled(l, Style::new().fg(Color::Red))));
        }
    }
    Some(lines)
}

/// Вопрос поверх окна (жёлтая рамка): занять ли адаптер, пускать ли новое устройство.
pub fn draw_confirm(f: &mut Frame, area: Rect, app: &AppState) {
    let width = area.width.min(OVERLAY_WIDTH);
    let Some((title, lines)) = confirm_content(app, width.saturating_sub(4) as usize) else {
        return;
    };
    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = centered_rect(area, width, height);
    f.render_widget(Clear, popup);

    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Yellow))
        .padding(Padding::horizontal(1))
        .title(Span::styled(
            format!(" {title} "),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    f.render_widget(Paragraph::new(lines), inner);
}

/// Заголовок и строки вопроса (текст переносим сами, чтобы точно знать высоту).
fn confirm_content(app: &AppState, wrap_at: usize) -> Option<(String, Vec<Line<'static>>)> {
    let confirm = app.confirm.as_ref()?;
    let lang = app.lang;
    let (title, question, hint, note) = match confirm {
        Confirm::StartOverClient(network) => (
            t(lang, Msg::TuiConfirmTitle),
            tf(lang, Msg::TuiConfirmStartFmt, &[network]),
            t(lang, Msg::TuiConfirmHint),
            None,
        ),
        Confirm::NewDevice { name, .. } => (
            t(lang, Msg::NotifyNewDevice),
            tf(lang, Msg::TuiConfirmDeviceFmt, &[name]),
            t(lang, Msg::TuiConfirmDeviceHint),
            Some(t(lang, Msg::TuiConfirmDeviceMacHint)),
        ),
        Confirm::EnableApproval => (
            t(lang, Msg::TuiAskApprovalTitle),
            t(lang, Msg::TuiAskApproval).to_string(),
            t(lang, Msg::TuiAskApprovalHint),
            None,
        ),
        Confirm::SaveRestart => (
            t(lang, Msg::TuiConfirmTitle),
            t(lang, Msg::AdvConfirmRestart).to_string(),
            t(lang, Msg::AdvConfirmRestartHint),
            None,
        ),
        Confirm::UnsavedQuit => (
            t(lang, Msg::AdvConfirmUnsavedTitle),
            t(lang, Msg::AdvConfirmUnsaved).to_string(),
            t(lang, Msg::AdvConfirmUnsavedHint),
            None,
        ),
        Confirm::Reset => (
            t(lang, Msg::AdvConfirmResetTitle),
            t(lang, Msg::AdvConfirmReset).to_string(),
            t(lang, Msg::AdvConfirmResetHint),
            None,
        ),
        Confirm::GeneratePassword => (
            t(lang, Msg::AdvConfirmGenerateTitle),
            t(lang, Msg::AdvConfirmGenerate).to_string(),
            t(lang, Msg::AdvConfirmGenerateHint),
            None,
        ),
    };

    let text = wrap_words(&question, wrap_at);
    let note = note.map(|n| wrap_words(n, wrap_at)).unwrap_or_default();
    let mut lines: Vec<Line<'static>> = text.into_iter().map(Line::from).collect();
    for n in note {
        lines.push(Line::from(Span::styled(
            n,
            Style::new().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::raw(""));
    for l in wrap_words(hint, wrap_at) {
        lines.push(Line::from(Span::styled(
            l,
            Style::new().fg(Color::DarkGray),
        )));
    }
    Some((title.to_string(), lines))
}

/// Перенос строки со стилями по словам: длинная строка не обрезается рамкой.
/// Слово длиннее строки режется по символам.
pub fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    if line.width() <= width {
        return vec![line];
    }
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut cur_w = 0usize;
    for span in line.spans {
        let style = span.style;
        // Кусок, который сам помещается в строку (значок «[ПК скрыт от гостей]»), переносим целиком.
        let span_w = span.content.trim_end_matches(' ').chars().count();
        if span_w <= width {
            if cur_w > 0 && cur_w + span_w > width {
                out.push(Line::from(std::mem::take(&mut cur)));
                cur_w = 0;
            }
            let text = if cur_w == 0 {
                span.content.trim_start_matches(' ').to_string()
            } else {
                span.content.to_string()
            };
            if !text.is_empty() {
                cur_w += text.chars().count();
                match cur.last_mut() {
                    Some(last) if last.style == style => last.content.to_mut().push_str(&text),
                    _ => cur.push(Span::styled(text, style)),
                }
            }
            continue;
        }
        for piece in span.content.split_inclusive(' ') {
            let mut token = piece.to_string();
            loop {
                let word_w = token.trim_end_matches(' ').chars().count();
                if cur_w > 0 && cur_w + word_w > width {
                    out.push(Line::from(std::mem::take(&mut cur)));
                    cur_w = 0;
                }
                if cur_w == 0 {
                    // Пробелы в начале перенесённой строки не нужны.
                    token = token.trim_start_matches(' ').to_string();
                    if token.is_empty() {
                        break;
                    }
                    if token.trim_end_matches(' ').chars().count() > width {
                        let head: String = token.chars().take(width).collect();
                        token = token.chars().skip(width).collect();
                        out.push(Line::from(Span::styled(head, style)));
                        continue;
                    }
                }
                cur_w += token.chars().count();
                match cur.last_mut() {
                    Some(last) if last.style == style => last.content.to_mut().push_str(&token),
                    _ => cur.push(Span::styled(token, style)),
                }
                break;
            }
        }
    }
    if !cur.is_empty() {
        out.push(Line::from(cur));
    }
    out
}

/// Строки статуса, перенесённые под ширину текста внутри рамки.
pub fn status_lines_wrapped(app: &AppState, width: usize) -> Vec<Line<'static>> {
    status_lines(app)
        .into_iter()
        .flat_map(|l| wrap_line(l, width))
        .collect()
}

/// Подсказка по клавишам внизу окна (переносится, а не обрезается).
pub fn hint_lines(app: &AppState, width: usize) -> Vec<Line<'static>> {
    let style = Style::new().fg(Color::DarkGray);
    let text: String = hints_line(app)
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    wrap_segments(&text, width)
        .into_iter()
        .map(|l| Line::from(Span::styled(l, style)))
        .collect()
}

/// Перенос подсказки «Space вкл/выкл · e изменить · …» только между парами «клавиша — действие».
pub fn wrap_segments(text: &str, width: usize) -> Vec<String> {
    const SEP: &str = " · ";
    let mut lines: Vec<String> = Vec::new();
    for seg in text.split(SEP) {
        match lines.last_mut() {
            Some(last)
                if last.chars().count() + SEP.chars().count() + seg.chars().count() <= width =>
            {
                last.push_str(SEP);
                last.push_str(seg);
            }
            _ if seg.chars().count() > width => lines.extend(wrap_words(seg, width)),
            _ => lines.push(seg.to_string()),
        }
    }
    lines
}

/// Простой перенос по словам (по числу символов). Слово длиннее строки не режем.
pub fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(last) if last.chars().count() + 1 + word.chars().count() <= width => {
                last.push(' ');
                last.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    lines
}

pub fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
}
