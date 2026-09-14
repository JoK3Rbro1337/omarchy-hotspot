//! Продвинутый режим: слева разделы, справа параметры, внизу подсказка к выбранному.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use omarchy_hotspot_core::devices::{DeviceStatus, bars_str, signal_bars};
use omarchy_hotspot_core::doctor::Status;
use omarchy_hotspot_core::password::{self, Strength};
use omarchy_hotspot_core::traffic::{format_bytes, format_rate};
use omarchy_hotspot_core::{Msg, t, tf};

use super::mouse::{Hits, Scroll, Target};
use super::settings::{self, ExpertOutput, Focus, Item, Kind, Section};
use super::state::AppState;
use super::widgets::{self, DEVICE_ICON, format_uptime, pad, titled_block, wrap_words};

const SECTIONS_WIDTH: u16 = 22;
/// Ширина блока параметров в компактном окне (рамка включительно).
const ITEMS_WIDTH: u16 = 58;
/// Ширина содержимого продвинутого режима без внешней рамки окна.
pub const CONTENT_WIDTH: u16 = SECTIONS_WIDTH + 1 + ITEMS_WIDTH;
/// Строк списка устройств без прокрутки.
const DEVICE_ROWS: usize = 8;
/// Строка с клавишами — не больше стольких строк.
const KEYS_MAX_LINES: usize = 2;
/// Подсказка к параметру — не больше стольких строк.
const HINT_MAX_LINES: usize = 3;
/// Сообщение об ошибке — не больше стольких строк.
const MESSAGE_MAX_LINES: usize = 3;
const NAME_COL: usize = 18;
const STATUS_COL: usize = 16;
/// Строк под сведения о выбранном устройстве (с пустой строкой-отступом).
const DETAIL_LINES: u16 = 7;
const LABEL_MIN: usize = 16;
const LABEL_MAX: usize = 36;
/// Места под значение справа от подписи («[x] вкл», «30 мин *»).
const VALUE_MIN: usize = 16;

/// Высота продвинутого режима при ширине содержимого `width` — одна на все разделы: самый высокий
/// раздел (с запасом строк устройств и сведениями), самая длинная подсказка и строка клавиш.
/// Журнал и проверка системы помещаются в эту высоту и листаются.
pub fn envelope_height(app: &AppState, width: u16) -> u16 {
    let w = width as usize;
    let message = message_lines(app, w).len().max(1);
    (1 + message + hint_reserve(app, w) + keys_reserve(app, w)) as u16
        + content_envelope(app, width)
}

/// Поле ввода своего значения: окну нужно не меньше.
pub fn input_size(app: &AppState) -> Option<(u16, u16)> {
    app.adv.input.as_ref().map(|_| {
        (
            INPUT_WIDTH,
            input_lines(app, INPUT_WIDTH as usize - 4).len() as u16 + 2,
        )
    })
}

/// Высота рамок разделов и параметров: самый высокий раздел.
fn content_envelope(app: &AppState, width: u16) -> u16 {
    let sections = Section::ALL.len() as u16 + 2;
    let items = Section::ALL
        .iter()
        .map(|s| s.items().len() as u16 + 2)
        .max()
        .unwrap_or(0);
    let text_w = width.saturating_sub(SECTIONS_WIDTH + 1 + 4) as usize;
    let rows = app
        .device_rows()
        .len()
        .min(DEVICE_ROWS)
        .max(app.device_rows_reserved) as u16;
    // Сводка «одобрение: вкл / выкл / вкл, но не действует» — запас под самый длинный вариант,
    // иначе окно подрастало бы при включении раздачи без службы.
    let blocked = app.cfg.access.blocked_macs.len().to_string();
    let summary = [Msg::AdvOn, Msg::AdvOff, Msg::AdvApprovalNotActive]
        .into_iter()
        .map(|m| {
            let text = tf(app.lang, Msg::AdvDevSummaryFmt, &[t(app.lang, m), &blocked]);
            wrap_words(&text, text_w).len()
        })
        .max()
        .unwrap_or(1) as u16;
    let devices = summary + 1 + rows + DETAIL_LINES + 2;
    sections.max(items).max(devices)
}

/// Строк под подсказку: самая длинная подсказка раздела или параметра.
fn hint_reserve(app: &AppState, width: usize) -> usize {
    let lang = app.lang;
    let count = |m: Msg| wrap_words(t(lang, m), width).len().min(HINT_MAX_LINES);
    Section::ALL
        .iter()
        .flat_map(|s| std::iter::once(s.hint()).chain(s.items().iter().map(|i| i.hint())))
        .map(count)
        .max()
        .unwrap_or(1)
}

/// Строк под клавиши: самая длинная строка клавиш.
fn keys_reserve(app: &AppState, width: usize) -> usize {
    [
        Msg::AdvKeysSections,
        Msg::AdvKeysItems,
        Msg::AdvKeysDevices,
        Msg::AdvInputHint,
    ]
    .into_iter()
    .map(|m| {
        widgets::wrap_segments(t(app.lang, m), width)
            .len()
            .min(KEYS_MAX_LINES)
    })
    .max()
    .unwrap_or(1)
}

fn hint_lines(app: &AppState, width: usize) -> Vec<String> {
    wrap_words(t(app.lang, hint_msg(app)), width)
        .into_iter()
        .take(HINT_MAX_LINES)
        .collect()
}

fn keys_lines(app: &AppState, width: usize) -> Vec<String> {
    widgets::wrap_segments(t(app.lang, keys_msg(app)), width)
        .into_iter()
        .take(KEYS_MAX_LINES)
        .collect()
}

pub fn draw(f: &mut Frame, area: Rect, app: &AppState, hits: &mut Hits) {
    let width = area.width.max(1) as usize;

    // «● Раздача включена · Имя · 5 ГГц · канал 36» — первые две строки блока статуса простого режима.
    let mut status_src = widgets::status_lines(app).into_iter();
    let mut status = status_src.next().unwrap_or_default();
    if app.is_on()
        && let Some(summary) = status_src.next()
    {
        status
            .spans
            .push(Span::styled(" · ", Style::new().fg(Color::DarkGray)));
        status.spans.extend(summary.spans);
    }
    let message = message_lines(app, width);
    let hint = hint_lines(app, width);
    let keys = keys_lines(app, width);

    // Под сообщение, подсказку и клавиши — место с запасом: рамки не прыгают, когда текст короче.
    let msg_h = message.len().max(1) as u16;
    let hint_h = hint_reserve(app, width).max(hint.len()) as u16;
    let keys_h = keys_reserve(app, width).max(keys.len()) as u16;
    let content_h = area
        .height
        .saturating_sub(1 + msg_h + hint_h + keys_h)
        .max(4);

    let [status_area, msg_area, content, hint_area, keys_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(msg_h),
        Constraint::Length(content_h),
        Constraint::Length(hint_h),
        Constraint::Length(keys_h),
    ])
    .areas(area);

    f.render_widget(Paragraph::new(status), status_area);
    f.render_widget(Paragraph::new(message), msg_area);
    let [left, right] =
        Layout::horizontal([Constraint::Length(SECTIONS_WIDTH), Constraint::Min(20)])
            .spacing(1)
            .areas(content);
    draw_sections(f, left, app, hits);
    match app.adv.section {
        Section::Devices => draw_devices(f, right, app, hits),
        _ => draw_items(f, right, app, hits),
    }
    f.render_widget(
        Paragraph::new(
            hint.into_iter()
                .map(|l| Line::from(Span::styled(l, Style::new().fg(Color::DarkGray))))
                .collect::<Vec<_>>(),
        ),
        hint_area,
    );
    // Клавиши — у нижней рамки: запасная пустая строка остаётся над ними.
    let keys_top = Rect {
        y: keys_area.y + keys_area.height.saturating_sub(keys.len() as u16),
        height: keys.len() as u16,
        ..keys_area
    };
    f.render_widget(
        Paragraph::new(
            keys.into_iter()
                .map(|l| Line::from(Span::styled(l, Style::new().fg(Color::DarkGray))))
                .collect::<Vec<_>>(),
        ),
        keys_top,
    );
    if app.adv.input.is_some() {
        draw_input(f, area, app);
    }
}

/// Верхняя строка: занятость, ошибка, уведомление или напоминание о несохранённом.
fn message_lines(app: &AppState, width: usize) -> Vec<Line<'static>> {
    let lang = app.lang;
    let (text, color) = if let Some(busy) = app.busy {
        (
            format!(
                "{} {}",
                widgets::spinner_char(app.spinner_tick),
                busy.label(lang)
            ),
            Color::Blue,
        )
    } else if let Some(err) = &app.error {
        (format!("✗ {err}"), Color::Red)
    } else if let Some(n) = &app.notice {
        (n.clone(), Color::Yellow)
    } else if settings::any_dirty(&app.adv.draft, &app.cfg) {
        (t(lang, Msg::AdvUnsaved).to_string(), Color::Yellow)
    } else {
        (String::new(), Color::Reset)
    };
    let mut lines: Vec<Line<'static>> = text
        .lines()
        .flat_map(|l| wrap_words(l, width))
        .take(MESSAGE_MAX_LINES)
        .map(|l| Line::from(Span::styled(l, Style::new().fg(color))))
        .collect();
    // Одобрение включено, а белый список не действует — это видно в любом режиме.
    if app.approval_unavailable() {
        lines.push(Line::from(Span::styled(
            format!("⚠ {}", t(lang, Msg::TuiDaemonOffline)),
            Style::new().fg(Color::Yellow),
        )));
    }
    lines
}

fn hint_msg(app: &AppState) -> Msg {
    let adv = &app.adv;
    match (adv.focus, adv.item()) {
        (Focus::Items, Some(item)) => item.hint(),
        _ => adv.section.hint(),
    }
}

fn keys_msg(app: &AppState) -> Msg {
    let adv = &app.adv;
    if adv.input.is_some() {
        Msg::AdvInputHint
    } else if adv.focus == Focus::Sections {
        Msg::AdvKeysSections
    } else if adv.section == Section::Devices {
        Msg::AdvKeysDevices
    } else {
        Msg::AdvKeysItems
    }
}

fn selected_style(focused: bool) -> Style {
    if focused {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD)
    }
}

fn draw_sections(f: &mut Frame, area: Rect, app: &AppState, hits: &mut Hits) {
    let lang = app.lang;
    let adv = &app.adv;
    let block = titled_block(t(lang, Msg::AdvTitleSections));
    let inner = block.inner(area);
    let inner_width = inner.width as usize;
    hits.wheel(area, Scroll::Sections);
    for i in 0..Section::ALL.len() {
        hits.click(row(inner, i), Target::Section(i));
    }
    let lines: Vec<Line> = Section::ALL
        .iter()
        .map(|s| {
            let dirty = settings::section_dirty(*s, &adv.draft, &app.cfg);
            let text = format!(
                "{} {}{}",
                s.icon(),
                t(lang, s.title()),
                if dirty { " *" } else { "" }
            );
            if *s == adv.section {
                Line::from(Span::styled(
                    pad(&text, inner_width),
                    selected_style(adv.focus == Focus::Sections),
                ))
            } else {
                Line::from(Span::raw(text))
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn section_block(app: &AppState) -> Block<'static> {
    let section = app.adv.section;
    let dirty = settings::section_dirty(section, &app.adv.draft, &app.cfg);
    let title = format!(
        "{}{}",
        t(app.lang, section.title()),
        if dirty { " *" } else { "" }
    );
    titled_block(&title)
}

fn draw_items(f: &mut Frame, area: Rect, app: &AppState, hits: &mut Hits) {
    let lang = app.lang;
    let adv = &app.adv;
    let block = section_block(app);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = inner.width as usize;

    let items = adv.section.items();
    let label_col = items
        .iter()
        .map(|i| t(lang, i.label()).chars().count() + 2)
        .max()
        .unwrap_or(LABEL_MIN)
        .clamp(LABEL_MIN, LABEL_MAX)
        .min(width.saturating_sub(VALUE_MIN));

    let mut lines: Vec<Line> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        let selected = adv.focus == Focus::Items && idx == adv.item_idx;
        lines.push(item_line(app, *item, selected, label_col, width));
    }
    hits.wheel(area, Scroll::Items);
    for i in 0..items.len() {
        hits.click(row(inner, i), Target::Item(i));
    }
    let out_rows = (inner.height as usize).saturating_sub(lines.len() + 1);
    let output = output_lines(app, out_rows, width);
    if !output.is_empty() {
        // Колесо над журналом листает журнал, а не параметры.
        let top = inner.y + lines.len() as u16 + 1;
        let out_area =
            Rect::new(inner.x, top, inner.width, output.len() as u16).intersection(inner);
        hits.wheel(out_area, Scroll::Output);
        lines.push(Line::raw(""));
        lines.extend(output);
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// Строка `i` внутри области (пустой прямоугольник, если за её пределами).
fn row(inner: Rect, i: usize) -> Rect {
    let y = inner.y.saturating_add(i as u16);
    Rect::new(inner.x, y, inner.width, 1).intersection(inner)
}

/// «Канал                 ◀ 36 ▶ *»: подпись, значение, отметка изменённого.
fn item_line(
    app: &AppState,
    item: Item,
    selected: bool,
    label_col: usize,
    width: usize,
) -> Line<'static> {
    let lang = app.lang;
    let draft = &app.adv.draft;
    let label = pad(t(lang, item.label()), label_col);
    let changed = settings::value_text(draft, item, lang)
        != settings::value_text(&app.cfg, item, lang)
        || settings::toggle_state(draft, item) != settings::toggle_state(&app.cfg, item);

    let mut spans: Vec<Span> = vec![Span::raw(label)];
    match item {
        Item::Password => spans.extend(password_spans(app)),
        Item::Log | Item::Doctor | Item::Reset => spans.push(Span::styled(
            "Enter ▸",
            Style::new().fg(if item == Item::Reset {
                Color::Red
            } else {
                Color::DarkGray
            }),
        )),
        _ => {
            let value = settings::value_text(draft, item, lang);
            let mut style = match settings::toggle_state(draft, item) {
                Some(true) => Style::new().fg(Color::Green),
                Some(false) => Style::new().fg(Color::DarkGray),
                None if item.kind() == Kind::ReadOnly => Style::new().fg(Color::DarkGray),
                None => Style::new(),
            };
            if settings::is_risky(draft, item) {
                style = Style::new().fg(Color::Yellow);
            }
            let text = match settings::toggle_state(draft, item) {
                Some(on) => format!("[{}] {value}", if on { "x" } else { " " }),
                None => value,
            };
            let arrows = selected && matches!(item.kind(), Kind::Choice | Kind::ChoiceOrText);
            if arrows {
                spans.push(Span::raw("◀ "));
                spans.push(Span::styled(text, style));
                spans.push(Span::raw(" ▶"));
            } else {
                spans.push(Span::styled(text, style));
            }
        }
    }
    if changed {
        spans.push(Span::styled(" *", Style::new().fg(Color::Yellow)));
    }
    if selected {
        // Выделяем всю строку: так видно, какой параметр меняют стрелки.
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
        for s in &mut spans {
            s.style = s.style.add_modifier(Modifier::REVERSED);
        }
    }
    Line::from(spans)
}

/// Сила пароля полоской «████████░░ надёжный».
fn password_spans(app: &AppState) -> Vec<Span<'static>> {
    let lang = app.lang;
    let Some(psk) = &app.password else {
        let msg = if app.password_absent {
            Msg::AdvPasswordNone
        } else {
            Msg::AdvPasswordUnknown
        };
        return vec![Span::styled(t(lang, msg), Style::new().fg(Color::DarkGray))];
    };
    let (filled, color, label) = match password::check_strength(psk.expose()) {
        Strength::TooShort => (1, Color::Red, Msg::AdvStrengthWeak),
        Strength::Weak => (3, Color::Red, Msg::AdvStrengthWeak),
        Strength::Ok => (6, Color::Yellow, Msg::AdvStrengthOk),
        Strength::Strong => (10, Color::Green, Msg::AdvStrengthStrong),
    };
    vec![
        Span::styled("█".repeat(filled), Style::new().fg(color)),
        Span::styled("░".repeat(10 - filled), Style::new().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(t(lang, label), Style::new().fg(color)),
    ]
}

/// Результат «Журнала» или «Проверки системы» под параметрами «Эксперта».
fn output_lines(app: &AppState, rows: usize, width: usize) -> Vec<Line<'static>> {
    let lang = app.lang;
    if app.adv.section != Section::Expert || rows == 0 {
        return Vec::new();
    }
    match &app.adv.output {
        ExpertOutput::None => Vec::new(),
        ExpertOutput::Loading => vec![Line::from(Span::styled(
            format!(
                "{} {}",
                widgets::spinner_char(app.spinner_tick),
                t(lang, Msg::AdvLoading)
            ),
            Style::new().fg(Color::Blue),
        ))],
        ExpertOutput::Log(lines) => {
            // Показываем конец журнала; PgUp листает назад.
            let max_scroll = lines.len().saturating_sub(rows);
            let scroll = app.adv.log_scroll.min(max_scroll);
            let end = lines.len() - scroll;
            let start = end.saturating_sub(rows);
            lines[start..end]
                .iter()
                .map(|l| {
                    let style = if l.starts_with("──") {
                        Style::new().fg(Color::Blue)
                    } else {
                        Style::new().fg(Color::DarkGray)
                    };
                    Line::from(Span::styled(
                        l.chars().take(width).collect::<String>(),
                        style,
                    ))
                })
                .collect()
        }
        ExpertOutput::Doctor(checks) => {
            let mut out = Vec::new();
            for c in checks {
                let (mark, color) = match c.status {
                    Status::Ok => ("✓", Color::Green),
                    Status::Warn => ("!", Color::Yellow),
                    Status::Fail => ("✗", Color::Red),
                };
                out.push(Line::from(vec![
                    Span::styled(format!("{mark} "), Style::new().fg(color)),
                    Span::raw(t(lang, c.id.title()).to_string()),
                    Span::styled(format!(" — {}", c.detail), Style::new().fg(Color::DarkGray)),
                ]));
                if let Some(advice) = c.advice {
                    for l in wrap_words(t(lang, advice), width.saturating_sub(2)) {
                        out.push(Line::from(Span::styled(
                            format!("  {l}"),
                            Style::new().fg(Color::DarkGray),
                        )));
                    }
                }
            }
            let all_ok = checks.iter().all(|c| c.status == Status::Ok);
            out.push(Line::from(Span::styled(
                t(
                    lang,
                    if all_ok {
                        Msg::DoctorAllOk
                    } else {
                        Msg::DoctorHasProblems
                    },
                ),
                Style::new().fg(if all_ok { Color::Green } else { Color::Yellow }),
            )));
            out.truncate(rows);
            out
        }
    }
}

/// «Одобрение: вкл · в чёрном списке: 1».
fn devices_summary(app: &AppState) -> String {
    let lang = app.lang;

    // Состояние одобрения — по службе, а не по настройке: важно, действует ли оно на деле.
    let approval = t(
        lang,
        if !app.cfg.access.approval_required {
            Msg::AdvOff
        } else if app.approval_unavailable() {
            Msg::AdvApprovalNotActive
        } else {
            Msg::AdvOn
        },
    );
    tf(
        lang,
        Msg::AdvDevSummaryFmt,
        &[approval, &app.cfg.access.blocked_macs.len().to_string()],
    )
}

fn draw_devices(f: &mut Frame, area: Rect, app: &AppState, hits: &mut Hits) {
    let lang = app.lang;
    let block = section_block(app);
    let inner = block.inner(area);
    f.render_widget(block, area);
    hits.wheel(area, Scroll::Devices);

    let rows = app.device_rows();
    let selected = app.selected_device_idx();
    let focused = app.adv.focus == Focus::Items;

    // Длинное «вкл, но не действует…» переносим, а не обрезаем рамкой.
    let mut lines: Vec<Line> = wrap_words(&devices_summary(app), inner.width as usize)
        .into_iter()
        .map(|l| {
            let color = if app.approval_unavailable() {
                Color::Yellow
            } else {
                Color::DarkGray
            };
            Line::from(Span::styled(l, Style::new().fg(color)))
        })
        .collect();
    lines.push(Line::raw(""));
    if rows.is_empty() {
        let msg = if app.is_on() || !app.cfg.access.blocked_macs.is_empty() {
            Msg::AdvDevNone
        } else {
            Msg::DevicesNeedOn
        };
        lines.push(Line::from(Span::styled(
            t(lang, msg),
            Style::new().fg(Color::DarkGray),
        )));
        f.render_widget(Paragraph::new(lines), inner);
        return;
    }

    let detail = selected
        .and_then(|i| rows[i].device)
        .map(|_| DETAIL_LINES)
        .unwrap_or(0);
    let list_rows = (inner.height.saturating_sub(lines.len() as u16 + detail) as usize).max(1);
    // Окно списка сдвигается так, чтобы выбранная строка была видна.
    let sel = selected.unwrap_or(0);
    let first = sel.saturating_sub(list_rows - 1);
    let width = inner.width as usize;
    let list_top = lines.len();
    for (i, row) in rows.iter().enumerate().skip(first).take(list_rows) {
        hits.click(self::row(inner, list_top + (i - first)), Target::Device(i));
        let (status_text, status_color) = match row.status {
            DeviceStatus::Allowed => (Msg::StatusAllowed, Color::Green),
            DeviceStatus::Pending => (Msg::StatusPending, Color::Yellow),
            DeviceStatus::Blocked => (Msg::StatusBlocked, Color::Red),
        };
        let name = row
            .device
            .map(|d| d.display_name())
            .unwrap_or_else(|| row.mac.as_str().to_string());
        let place = match row.device {
            Some(d) => d.ip.map(|ip| ip.to_string()).unwrap_or_default(),
            None => t(lang, Msg::AdvDevOffline).to_string(),
        };
        let mut spans = vec![
            Span::styled(format!("{DEVICE_ICON} "), Style::new().fg(Color::Blue)),
            Span::raw(pad(&name, NAME_COL)),
            Span::raw(" "),
            Span::styled(
                pad(t(lang, status_text), STATUS_COL),
                Style::new().fg(status_color),
            ),
            Span::styled(place, Style::new().fg(Color::DarkGray)),
        ];
        if focused && Some(i) == selected {
            let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
            spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
            for s in &mut spans {
                s.style = s.style.add_modifier(Modifier::REVERSED);
            }
        }
        lines.push(Line::from(spans));
    }

    if let Some(d) = selected.and_then(|i| rows[i].device) {
        let label = |m: Msg| Span::styled(pad(t(lang, m), 14), Style::new().fg(Color::DarkGray));
        let (down, up) = app.traffic.rate_of(&d.mac);
        let bars = d.signal_dbm.map_or(0, signal_bars);
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(pad("MAC", 14), Style::new().fg(Color::DarkGray)),
            Span::raw(d.mac.as_str().to_string()),
        ]));
        lines.push(Line::from(vec![
            label(Msg::AdvDevSignal),
            Span::raw(bars_str(bars)),
            Span::styled(
                d.signal_dbm
                    .map(|v| format!("  {v} dBm"))
                    .unwrap_or_default(),
                Style::new().fg(Color::DarkGray),
            ),
        ]));
        lines.push(Line::from(vec![
            label(Msg::AdvDevConnected),
            Span::raw(format_uptime(d.connected_secs)),
        ]));
        lines.push(Line::from(vec![
            label(Msg::AdvDevSpeed),
            Span::styled("↓ ", Style::new().fg(Color::Cyan)),
            Span::raw(format_rate(down, lang)),
            Span::styled("  ↑ ", Style::new().fg(Color::Magenta)),
            Span::raw(format_rate(up, lang)),
        ]));
        lines.push(Line::from(vec![
            label(Msg::AdvDevTotal),
            Span::styled("↓ ", Style::new().fg(Color::Cyan)),
            Span::raw(format_bytes(d.down_bytes(), lang)),
            Span::styled("  ↑ ", Style::new().fg(Color::Magenta)),
            Span::raw(format_bytes(d.up_bytes(), lang)),
        ]));
        lines.push(Line::from(vec![
            label(Msg::AdvDevAccess),
            Span::raw(
                t(
                    lang,
                    match rows[selected.unwrap_or(0)].status {
                        DeviceStatus::Allowed => Msg::StatusAllowed,
                        DeviceStatus::Pending => Msg::StatusPending,
                        DeviceStatus::Blocked => Msg::StatusBlocked,
                    },
                )
                .to_string(),
            ),
        ]));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// Поле ввода своего значения поверх окна.
/// Ширина поля ввода своего значения.
const INPUT_WIDTH: u16 = 64;

fn draw_input(f: &mut Frame, area: Rect, app: &AppState) {
    let Some(input) = &app.adv.input else { return };
    let lang = app.lang;
    let width = area.width.min(INPUT_WIDTH);
    let lines = input_lines(app, width.saturating_sub(4) as usize);
    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = widgets::centered_rect(area, width, height);
    f.render_widget(Clear, popup);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Blue))
        .padding(ratatui::widgets::Padding::horizontal(1))
        .title(Span::styled(
            format!(" {} ", t(lang, input.item.label())),
            Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    f.render_widget(Paragraph::new(lines), inner);
}

fn input_lines(app: &AppState, wrap_at: usize) -> Vec<Line<'static>> {
    let Some(input) = &app.adv.input else {
        return Vec::new();
    };
    let lang = app.lang;

    let mut lines = vec![Line::from(vec![
        Span::styled(
            input.value.clone(),
            Style::new().add_modifier(Modifier::REVERSED),
        ),
        Span::styled("▏", Style::new().fg(Color::Blue)),
    ])];
    if let Some(err) = &input.error {
        lines.push(Line::raw(""));
        for l in wrap_words(&format!("✗ {err}"), wrap_at) {
            lines.push(Line::from(Span::styled(l, Style::new().fg(Color::Red))));
        }
    }
    lines.push(Line::raw(""));
    for l in wrap_words(t(lang, Msg::AdvInputHint), wrap_at) {
        lines.push(Line::from(Span::styled(
            l,
            Style::new().fg(Color::DarkGray),
        )));
    }
    lines
}
