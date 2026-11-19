//! 탭과 기록 창이 함께 쓰는 그리기 도우미(1.0.5 widgets.py 의 Card·버튼·표지에 해당).

use super::theme::*;
use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke};

/// 칸 간격 단위(8/12/16/24).
pub const GAP_S: f32 = 8.0;
pub const GAP_M: f32 = 12.0;
pub const GAP_L: f32 = 16.0;
#[allow(dead_code)]
pub const GAP_XL: f32 = 24.0;
/// 입력 칸·단추·콤보의 공통 높이.
pub const CTRL_H: f32 = 32.0;
/// 오른쪽 설정 칸 폭.
pub const SIDE_W: f32 = 320.0;

/// 카드: 흰 바탕, 얇은 테두리, 안쪽 여백. 제목이 있으면 맨 위에 굵게 둔다.
pub fn card<R>(ui: &mut egui::Ui, title: Option<&str>, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(GAP_L as i8, GAP_M as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(t) = title {
                ui.label(RichText::new(t).size(15.0).bold().color(TEXT));
                ui.add_space(GAP_S);
            }
            add(ui)
        })
        .inner
}

/// 쪽 제목과 한 줄 설명.
pub fn page_heading(ui: &mut egui::Ui, title: &str, sub: &str) {
    ui.label(RichText::new(title).size(21.0).bold().color(TEXT));
    if !sub.is_empty() {
        ui.label(RichText::new(sub).color(SUB));
    }
    ui.add_space(6.0);
}

/// 작은 보조 글.
pub fn small(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(SUB));
}

/// 목록이 비었을 때의 안내(가운데 정렬, 옅은 글씨).
pub fn empty_hint(ui: &mut egui::Ui, text: &str) {
    ui.add_space(10.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(text).color(IDLE));
    });
    ui.add_space(10.0);
}

/// 주요 단추: 강조색 바탕, 흰 굵은 글씨, 높이 38.
#[allow(dead_code)]
pub fn primary(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    boxed_button(ui, text, enabled, egui::vec2(150.0, 38.0), true, egui::FontId::new(15.0, bold_family()))
}

/// 보조 단추: 흰 바탕, 얇은 테두리.
pub fn secondary(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new(RichText::new(text).color(TEXT)).min_size(egui::vec2(0.0, CTRL_H)))
}

/// 작은 보조 단추(목록 도구 줄용).
pub fn tool(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new(RichText::new(text).size(13.0).color(TEXT)).min_size(egui::vec2(0.0, CTRL_H)))
}

/// 상태 표시 공통 위젯: 16px 아이콘(직접 그림, 상태색) + 간격 7 + 본문색 글자. 배경 없음.
pub fn status_mark(ui: &mut egui::Ui, status: super::common::Status, label: &str) -> egui::Response {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, TEXT);
    let size = egui::vec2(16.0 + 7.0 + galley.size().x, galley.size().y.max(16.0));
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        draw_status_icon(ui.painter(), egui::pos2(rect.left() + 8.0, rect.center().y), status, status.color());
        let pos = egui::pos2(rect.left() + 23.0, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(pos, galley, TEXT);
    }
    resp
}

/// 상태 아이콘 한 개(중심 c, 16x16 안). 글꼴 글리프 없이 도형으로만 그린다.
pub fn draw_status_icon(p: &egui::Painter, c: egui::Pos2, status: super::common::Status, col: Color32) {
    use super::common::Status;
    let w = Stroke::new(1.8, Color32::WHITE);
    let pt = |dx: f32, dy: f32| egui::pos2(c.x + dx, c.y + dy);
    match status {
        Status::Done => {
            p.circle_filled(c, 8.0, col);
            p.line_segment([pt(-3.5, 0.5), pt(-1.0, 3.0)], w);
            p.line_segment([pt(-1.0, 3.0), pt(3.8, -2.6)], w);
        }
        Status::Check => {
            p.add(egui::Shape::convex_polygon(vec![pt(0.0, -7.5), pt(8.0, 7.0), pt(-8.0, 7.0)], col, Stroke::new(1.5, col)));
            p.line_segment([pt(0.0, -2.5), pt(0.0, 2.0)], w);
            p.circle_filled(pt(0.0, 4.6), 1.1, Color32::WHITE);
        }
        Status::Stop => {
            p.rect_filled(egui::Rect::from_center_size(c, egui::vec2(14.0, 14.0)), CornerRadius::same(3), col);
        }
        Status::Hold => {
            p.rect_filled(egui::Rect::from_center_size(pt(-3.5, 0.0), egui::vec2(4.0, 14.0)), CornerRadius::same(1), col);
            p.rect_filled(egui::Rect::from_center_size(pt(3.5, 0.0), egui::vec2(4.0, 14.0)), CornerRadius::same(1), col);
        }
        Status::Wait => {
            p.circle_stroke(c, 7.0, Stroke::new(1.8, col));
            p.line_segment([c, pt(0.0, -4.0)], Stroke::new(1.6, col));
            p.line_segment([c, pt(3.0, 1.5)], Stroke::new(1.6, col));
        }
    }
}

/// 저장소 상태 전용 표시(결과 5종과 따로): 폴더 윤곽선 + 사선 16px, ERR색, 본문색 글자.
pub fn repo_missing_mark(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, TEXT);
    let size = egui::vec2(16.0 + 7.0 + galley.size().x, galley.size().y.max(16.0));
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let c = egui::pos2(rect.left() + 8.0, rect.center().y);
        let s = Stroke::new(1.5, ERR);
        let pt = |dx: f32, dy: f32| egui::pos2(c.x + dx, c.y + dy);
        // 폴더 윤곽: 왼쪽 위 귀가 있는 모양.
        p.add(egui::Shape::closed_line(vec![pt(-7.0, -5.5), pt(-2.5, -5.5), pt(-1.0, -3.5), pt(7.0, -3.5), pt(7.0, 6.0), pt(-7.0, 6.0)], s));
        p.line_segment([pt(-7.5, 7.5), pt(7.5, -7.0)], s);
        p.galley(egui::pos2(rect.left() + 23.0, rect.center().y - galley.size().y / 2.0), galley, TEXT);
    }
    resp
}

/// 필터 팝업의 한 줄: (아이콘) + 글자, 높이 32, 고른 줄은 옅은 바탕.
pub fn choice_row(ui: &mut egui::Ui, status: Option<super::common::Status>, label: &str, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(150.0), CTRL_H), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        if selected {
            ui.painter().rect_filled(rect, CornerRadius::same(4), SEL_BG);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(4), ui.style().visuals.widgets.hovered.weak_bg_fill);
        }
        let mut x = rect.left() + 10.0;
        if let Some(s) = status {
            draw_status_icon(ui.painter(), egui::pos2(x + 8.0, rect.center().y), s, s.color());
            x += 23.0;
        }
        ui.painter().text(egui::pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, label, egui::TextStyle::Body.resolve(ui.style()), TEXT);
    }
    resp
}

/// 낮은 강조 단추(테두리 없음, 호버 때만 옅은 바탕). 높이 28, 글자 가운데.
pub fn ghost_w(ui: &mut egui::Ui, text: &str, enabled: bool, w: f32) -> egui::Response {
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 28.0), sense);
    if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(6), SEL_BG);
    }
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(13.0), if enabled { TEXT } else { IDLE });
    resp
}

/// 머리줄 전체 선택 칸(세 상태). 반환: 눌렸는가.
pub fn tri_check(ui: &mut egui::Ui, rect: egui::Rect, checked: usize, total: usize, enabled: bool) -> bool {
    let resp = ui.interact(rect, ui.scope_id().with("tri_all"), if enabled { egui::Sense::click() } else { egui::Sense::hover() });
    paint_check(ui, rect.center(), checked, total, resp.hovered());
    resp.clicked()
}

/// 18px 체크 상자 그리기: 전부면 체크, 일부면 가로선, 없으면 빈 상자.
pub fn paint_check(ui: &egui::Ui, c: egui::Pos2, checked: usize, total: usize, hover: bool) {
    let r = egui::Rect::from_center_size(c, egui::vec2(18.0, 18.0));
    let on = total > 0 && checked > 0;
    let p = ui.painter();
    p.rect(r, CornerRadius::same(4), if on { ACCENT } else { CARD }, Stroke::new(1.5, if on { ACCENT } else if hover { SUB } else { IDLE }), egui::StrokeKind::Inside);
    if on {
        let w = Stroke::new(2.0, Color32::WHITE);
        if checked >= total {
            p.line_segment([egui::pos2(c.x - 4.5, c.y + 0.5), egui::pos2(c.x - 1.5, c.y + 3.5)], w);
            p.line_segment([egui::pos2(c.x - 1.5, c.y + 3.5), egui::pos2(c.x + 4.5, c.y - 3.5)], w);
        } else {
            p.line_segment([egui::pos2(c.x - 4.5, c.y), egui::pos2(c.x + 4.5, c.y)], w);
        }
    }
}

/// 안내 상자(왼쪽 색 띠 없이 옅은 바탕). 제목은 굵게.
pub fn notice_box<R>(ui: &mut egui::Ui, bg: Color32, edge: Color32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(bg)
        .stroke(Stroke::new(1.0, edge.gamma_multiply(0.45)))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// 경로 입력 칸: 테두리가 보이고 비었을 때 안내 글이 뜬다.
pub fn path_field(ui: &mut egui::Ui, value: &mut String, hint: &str, width: f32) -> egui::Response {
    ui.add(egui::TextEdit::singleline(value).hint_text(RichText::new(hint).color(IDLE)).desired_width(width).min_size(egui::vec2(0.0, CTRL_H)).align(egui::Align2::LEFT_CENTER).margin(Margin::symmetric(8, 6)))
}

/// 줄 하나: 왼쪽에 이름표(고정 폭), 오른쪽에 내용.
pub fn field_row<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(104.0, CTRL_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.label(RichText::new(label).color(SUB));
        });
        add(ui)
    })
    .inner
}

/// 로그 모양의 글 상자(연한 바탕, 고정폭 글꼴).
pub fn log_box<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(LOG_BG)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// 고정폭 로그 글.
pub fn log_text(text: &str) -> RichText {
    RichText::new(text).monospace().color(LOG_FG)
}

/// '결과 크게 보기' 창에 띄울 글(1.0.5 의 결과 크게 보기).
pub struct BigText {
    title: String,
    text: String,
    copied_at: Option<std::time::Instant>,
}

impl BigText {
    pub fn new(title: &str, text: String) -> BigText {
        BigText { title: title.to_string(), text, copied_at: None }
    }
}

/// 결과를 큰 창으로 보여 준다. 크기를 바꿀 수 있고 글을 골라 복사할 수 있다. '닫기'·Esc·창 X 로 닫는다.
pub fn big_text_window(ctx: &egui::Context, slot: &mut Option<BigText>) {
    let Some(b) = slot.as_mut() else {
        return;
    };
    let mut open = true;
    let mut close = false;
    let copied = b.copied_at.is_some_and(|t| t.elapsed().as_secs_f32() < 1.5);
    if copied {
        ctx.request_repaint_after(std::time::Duration::from_millis(300));
    }
    egui::Window::new(b.title.as_str())
        .id(egui::Id::unique("big_text_window"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(egui::vec2(1100.0, 780.0))
        .show(ctx, |ui| {
            // 아래 단추 줄 자리를 남기고 나머지를 글 상자가 채운다.
            let h = (ui.available_height() - 44.0).max(120.0);
            egui::ScrollArea::both().id_salt("big_text_scroll").auto_shrink([false, false]).max_height(h).show(ui, |ui| {
                // 읽기 전용 글 상자: 고칠 수는 없고 마우스로 골라 복사할 수 있다.
                ui.add(egui::TextEdit::multiline(&mut b.text.as_str()).desired_width(f32::INFINITY).code_editor());
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if tool(ui, if copied { "복사됨" } else { "내용 복사" }, !b.text.is_empty()).clicked() {
                    ui.ctx().copy_text(b.text.clone());
                    b.copied_at = Some(std::time::Instant::now());
                }
                if tool(ui, "닫기", true).clicked() {
                    close = true;
                }
            });
        });
    if !open || close || ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        *slot = None;
    }
}


/// 폭을 꽉 채우는 주요 단추(실행 상자용).
pub fn primary_full(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    boxed_button(ui, text, enabled, egui::vec2(ui.available_width(), 38.0), true, egui::FontId::new(15.0, bold_family()))
}

/// 폭을 꽉 채우는 보조 단추.
pub fn secondary_full(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    secondary_w(ui, text, enabled, ui.available_width())
}

/// 정해진 크기 단추를 직접 그린다. 평소·호버·눌림·비활성 모두 글자가 가운데다.
fn boxed_button(ui: &mut egui::Ui, text: &str, enabled: bool, size: egui::Vec2, accent: bool, font: egui::FontId) -> egui::Response {
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let (fill, stroke, color) = if accent {
            let f = if !enabled {
                Color32::from_rgb(0xB5, 0xC2, 0xBA)
            } else if resp.is_pointer_button_down_on() || resp.hovered() {
                ACCENT_DK
            } else {
                ACCENT
            };
            (f, Stroke::NONE, Color32::WHITE)
        } else {
            let v = if !enabled {
                &ui.style().visuals.widgets.noninteractive
            } else {
                ui.style().interact(&resp)
            };
            (v.weak_bg_fill, v.bg_stroke, if enabled { TEXT } else { IDLE })
        };
        ui.painter().rect(rect, CornerRadius::same(6), fill, stroke, egui::StrokeKind::Inside);
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, font, color);
    }
    resp
}

/// 폭을 정한 보조 단추(높이 CTRL_H, 글자 가운데).
pub fn secondary_w(ui: &mut egui::Ui, text: &str, enabled: bool, w: f32) -> egui::Response {
    boxed_button(ui, text, enabled, egui::vec2(w, CTRL_H), false, egui::FontId::proportional(14.0))
}

/// 이름표를 입력 위에 두는 칸(좁은 설정 칸용).
pub fn labeled<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.label(RichText::new(label).size(12.5).color(SUB));
    ui.add_space(4.0);
    let r = add(ui);
    ui.add_space(GAP_M);
    r
}

/// 같은 폭의 구간 단추 줄. 고른 번호가 바뀌면 참.
pub fn segmented(ui: &mut egui::Ui, value: &mut usize, labels: &[&str]) -> bool {
    let n = labels.len().max(1) as f32;
    let gap = 4.0;
    let w = (ui.available_width() - gap * (n - 1.0)) / n;
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (i, l) in labels.iter().enumerate() {
            let on = *value == i;
            let text = if on { RichText::new(*l).bold().color(ACCENT) } else { RichText::new(*l).color(TEXT) };
            let mut b = egui::Button::new(text).min_size(egui::vec2(w, CTRL_H)).corner_radius(CornerRadius::same(6));
            if on {
                b = b.fill(SEL_BG).stroke(Stroke::new(1.0, ACCENT));
            }
            if ui.add(b).clicked() && !on {
                *value = i;
                changed = true;
            }
        }
    });
    changed
}

/// 두 칸 배치의 오른쪽 열: 위쪽 설정은 자기 안에서만 스크롤하고, 실행 상자는 맨 아래에 붙는다.
/// 실행 상자 높이는 지난 그림에서 잰 값을 써서 설정 칸이 그 위 12px 에서 끝나게 한다(겹치지 않는다).
pub fn side_column(ui: &mut egui::Ui, id: &str, settings: impl FnOnce(&mut egui::Ui), exec: impl FnOnce(&mut egui::Ui)) {
    egui::Panel::right(id.to_string())
        .exact_size(SIDE_W + GAP_L)
        .resizable(false)
        .show_separator_line(false)
        .frame(egui::Frame::new().inner_margin(Margin { left: GAP_L as i8, right: 0, top: 0, bottom: 0 }))
        .show(ui, |ui| {
            // 두 영역의 자리를 직접 정한다: 설정은 위, 실행 상자는 맨 아래(상자 아래 변은 이 칸 안에 들어온다).
            let full = ui.max_rect();
            let mem = ui.make_persistent_id("exec_box_h");
            // 실행 상자가 칸보다 커도 칸 밖으로 나가지 않게 묶고, 설정 영역은 남는 만큼만 쓴다(최소 높이를 억지로 주면 두 영역이 겹친다).
            let exec_h: f32 = ui.data(|d| d.get_temp(mem)).unwrap_or(170.0_f32).min(full.height());
            let scroll_h = (full.height() - exec_h - GAP_M).max(0.0);
            let srect = egui::Rect::from_min_size(full.min, egui::vec2(full.width(), scroll_h));
            ui.scope_builder(egui::UiBuilder::new().max_rect(srect), |ui| {
                ui.set_clip_rect(srect.intersect(ui.clip_rect()));
                egui::ScrollArea::vertical().id_salt("side_settings").max_height(scroll_h).auto_shrink([false, false]).show(ui, settings);
            });
            let erect = egui::Rect::from_min_max(egui::pos2(full.left(), full.bottom() - exec_h), full.max);
            let real = ui
                .scope_builder(egui::UiBuilder::new().max_rect(erect), |ui| {
                    egui::Frame::new()
                        .fill(CARD)
                        .stroke(Stroke::new(1.0, LINE))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(GAP_L as i8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing.y = 10.0;
                            exec(ui);
                        })
                        .response
                        .rect
                        .height()
                })
                .inner;
            if (real - exec_h).abs() > 0.5 {
                ui.data_mut(|d| d.insert_temp(mem, real));
                ui.ctx().request_repaint();
            }
        });
}

/// 쪽 제목 줄(두 칸 배치 위쪽): 제목과 설명을 한 줄에 둔다.
pub fn head(ui: &mut egui::Ui, title: &str, sub: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(21.0).bold().color(TEXT));
        if !sub.is_empty() {
            ui.add_space(GAP_S);
            ui.label(RichText::new(sub).color(SUB));
        }
    });
    ui.add_space(GAP_S);
}

/// 표 줄 높이.
pub const ROW_H: f32 = 36.0;

/// 폭을 꽉 채우는 표 줄 하나의 영역을 잡는다. 줄 아래에 옅은 구분선을 긋는다.
pub fn row_rect(ui: &mut egui::Ui, h: f32) -> egui::Rect {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::hover());
    ui.painter().hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, LINE));
    rect
}

/// 표 칸 하나: 줄 안 x 위치와 폭이 고정이고 안쪽 요소는 세로 가운데다. 넘치는 것은 칸 밖으로 그리지 않는다.
pub fn cell<R>(ui: &mut egui::Ui, row: egui::Rect, x: f32, w: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let r = egui::Rect::from_min_size(egui::pos2(row.left() + x, row.top()), egui::vec2(w.max(1.0), row.height()));
    ui.scope_builder(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
        let clip = r.intersect(ui.clip_rect());
        ui.set_clip_rect(clip);
        add(ui)
    })
    .inner
}

