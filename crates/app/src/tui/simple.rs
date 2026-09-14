//! Простой режим: слева статус, трафик и устройства, справа QR — только по запросу (`c` или `[ QR ]`).
//! Высота блоков с запасом (сообщение в статусе, строки устройств): окно не меняет размер от
//! сообщений, включения раздачи и каждого подключившегося устройства.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::mouse::{Hits, Scroll, Target};
use super::state::{AppState, MAX_DEVICE_ROWS};
use super::widgets;

/// Ширина левой колонки без QR (рамки блоков включительно) — не уже этого.
pub const LEFT_WIDTH: u16 = 50;
/// Уже этого левая колонка не становится: QR тогда уходит вниз или поверх окна.
const LEFT_MIN_WIDTH: u16 = 40;
/// Высота блока «Трафик».
const TRAFFIC_HEIGHT: u16 = 3;
/// Сколько строк подсказки по клавишам показываем.
const HINT_MAX_LINES: usize = 2;
/// Строк статуса с запасом: состояние, сеть, значки, пароль и одно сообщение.
const STATUS_RESERVED_LINES: usize = 5;
/// Рамка блока сверху и снизу; рамка и отступы по бокам.
const BLOCK_V: u16 = 2;
const BLOCK_H: u16 = 4;

/// Где стоит QR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrPlace {
    /// QR не просили (или раздача выключена).
    None,
    Right,
    Below,
    /// Рядом с блоками места нет — поверх окна.
    Overlay,
}

/// QR показан по запросу и нужен (раздача включена).
pub fn qr_wanted(app: &AppState) -> bool {
    app.show_qr && app.is_on()
}

/// Высота простого режима без QR при ширине содержимого `width` (с запасами).
pub fn base_height(app: &AppState, width: u16) -> u16 {
    left_height(app, width) + hints(app, width).len() as u16
}

/// Размер с QR справа, если QR показан: не уже `width`.
pub fn qr_size(app: &AppState, width: u16) -> Option<(u16, u16)> {
    if !qr_wanted(app) {
        return None;
    }
    let (qw, qh) = (widgets::qr_box_width(app), widgets::qr_box_height(app));
    let w = width.max(LEFT_MIN_WIDTH + 1 + qw);
    let left = left_height(app, w - 1 - qw).max(qh);
    Some((w, left + hints(app, w).len() as u16))
}

/// Где QR помещается в этой области.
pub fn qr_place(app: &AppState, area: Rect) -> QrPlace {
    if !qr_wanted(app) {
        return QrPlace::None;
    }
    let (qw, qh) = (widgets::qr_box_width(app), widgets::qr_box_height(app));
    let content_h = area
        .height
        .saturating_sub(hints(app, area.width).len() as u16);
    if area.width > qw + LEFT_MIN_WIDTH && content_h >= qh {
        return QrPlace::Right;
    }
    // Снизу — если над QR остаётся место на статус, трафик и строку устройств.
    let above = status_height(app, area.width, false) + TRAFFIC_HEIGHT + BLOCK_V + 1;
    if area.width >= qw && content_h >= qh + above {
        return QrPlace::Below;
    }
    QrPlace::Overlay
}

/// Высота левой колонки с запасами при ширине `width`.
fn left_height(app: &AppState, width: u16) -> u16 {
    status_height(app, width, true) + TRAFFIC_HEIGHT + devices_height(app)
}

/// Высота блока «Статус»; `reserve` — с запасом под сообщение.
fn status_height(app: &AppState, width: u16, reserve: bool) -> u16 {
    let lines = status_lines(app, width).len();
    let lines = if reserve {
        lines.max(STATUS_RESERVED_LINES)
    } else {
        lines
    };
    lines as u16 + BLOCK_V
}

/// Блок «Устройства»: строки по числу устройств, но не меньше запаса окна.
fn devices_height(app: &AppState) -> u16 {
    let rows = widgets::device_rows_needed(app, MAX_DEVICE_ROWS).max(app.device_rows_reserved);
    rows as u16 + BLOCK_V
}

fn status_lines(app: &AppState, width: u16) -> Vec<Line<'static>> {
    widgets::status_lines_wrapped(app, width.saturating_sub(BLOCK_H) as usize)
}

fn hints(app: &AppState, width: u16) -> Vec<Line<'static>> {
    widgets::hint_lines(app, width as usize)
        .into_iter()
        .take(HINT_MAX_LINES)
        .collect()
}

pub fn draw(f: &mut Frame, area: Rect, app: &AppState, hits: &mut Hits) {
    let hint = hints(app, area.width);
    let [content, hint_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(hint.len() as u16)]).areas(area);

    let (qw, qh) = (widgets::qr_box_width(app), widgets::qr_box_height(app));
    let (left, qr_area) = match qr_place(app, area) {
        QrPlace::Right => {
            let [left, right] =
                Layout::horizontal([Constraint::Min(LEFT_MIN_WIDTH), Constraint::Length(qw)])
                    .spacing(1)
                    .areas(content);
            (
                left,
                Some(Rect {
                    height: qh,
                    ..right
                }),
            )
        }
        QrPlace::Below => {
            let [top, bottom] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(qh)]).areas(content);
            let w = qw.min(bottom.width);
            let x = bottom.x + (bottom.width - w) / 2;
            (
                top,
                Some(Rect {
                    x,
                    width: w,
                    ..bottom
                }),
            )
        }
        // Поверх окна QR рисует `ui::draw` — после рамки.
        QrPlace::None | QrPlace::Overlay => (content, None),
    };
    draw_left(f, left, app, hits);
    if let Some(r) = qr_area {
        widgets::draw_qr_block(f, r, app);
        hits.click(r, Target::Qr);
    }
    f.render_widget(Paragraph::new(hint), hint_area);
}

fn draw_left(f: &mut Frame, area: Rect, app: &AppState, hits: &mut Hits) {
    let status = status_lines(app, area.width);
    // Места мало — первым сжимается список устройств, потом пропадает трафик, статус — последним.
    let mut free = area.height;
    let status_h = status_height(app, area.width, true).min(free);
    free -= status_h;
    let traffic_h = if free > TRAFFIC_HEIGHT + BLOCK_V {
        TRAFFIC_HEIGHT
    } else {
        0
    };
    let mut devices_h = devices_height(app).min(free - traffic_h);
    if devices_h <= BLOCK_V {
        devices_h = 0;
    }
    let [status_area, traffic_area, devices_area, _] = Layout::vertical([
        Constraint::Length(status_h),
        Constraint::Length(traffic_h),
        Constraint::Length(devices_h),
        Constraint::Min(0),
    ])
    .areas(area);

    f.render_widget(
        Paragraph::new(status).block(widgets::status_block(app)),
        status_area,
    );
    let buf = f.buffer_mut();
    let (label, _) = widgets::toggle_label(app);
    hits.click_label(buf, status_area.y, status_area, &label, Target::Toggle);
    if let Some(qr) = widgets::qr_button_label(app) {
        hits.click_label(buf, status_area.y, status_area, &qr, Target::Qr);
    }
    if traffic_h > 0 {
        widgets::draw_traffic(f, traffic_area, app);
    }
    if devices_h > 0 {
        widgets::draw_devices(f, devices_area, app);
        hits.wheel(devices_area, Scroll::SimpleDevices);
    }
}
