//! 기록 창: 실행 기록(상태 다섯·필터·멈춤 안내)과 실행 로그를 한 곳에 묶고, 되돌리기 창을 연다.

use super::common::{record_status, setting_str, Status};
use super::{theme, widgets, AppState};
use super::theme::Bold;
use eframe::egui::{self, RichText};
use gitbridge_core::store;
use serde_json::Value;

#[derive(Default)]
pub struct HistoryState {
    pub records: Vec<Value>,
    /// 참이면 실행 로그 보기, 거짓이면 실행 기록 보기.
    pub show_log: bool,
    /// 상태 필터(없으면 전체).
    pub filter: Option<Status>,
    /// 필터에 맞는 records 번호(새 것부터). 필터가 바뀌거나 기록을 다시 읽을 때만 다시 만든다.
    view: Vec<usize>,
    /// view 를 만든 때의 필터. None 이면 다시 만들어야 한다.
    view_key: Option<Option<Status>>,
    /// 고른 줄의 records 안 번호. 같은 초에 기록이 여러 개여도 줄마다 따로 고른다.
    /// 다시 읽을 때 같은 기록(내용이 같은 줄)의 새 번호로 옮기고, 없으면 푼다.
    pub selected: Option<usize>,
    /// 로그 파일 경로 문구(다시 읽을 때 갱신).
    pub log_path: String,
    /// 참이면 아래 기록 창을 펼친다(저장하지 않는다. 기본은 접힘).
    pub open: bool,
    /// '전체 복사'를 누른 때. 잠깐 '복사됨'으로 보인다.
    log_copied_at: Option<std::time::Instant>,
}

/// history.jsonl 을 다시 읽는다(작업이 끝날 때마다 불린다).
pub fn reload(st: &mut AppState) {
    match store::read_history(500) {
        Ok(v) => {
            let old = st.history.selected.and_then(|i| st.history.records.get(i).cloned());
            st.history.selected = old.and_then(|o| v.iter().position(|r| *r == o));
            st.history.records = v;
            st.history.view_key = None;
        }
        Err(e) => st.notice = format!("실행 기록을 읽지 못했습니다: {}", e),
    }
    st.history.log_path = match store::log_file() {
        Ok(p) => p.display().to_string(),
        Err(e) => format!("(로그 파일 경로를 알 수 없습니다: {})", e),
    };
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// 문자열 배열(없거나 문자열 하나여도 받는다).
fn lines_of(v: &Value, key: &str) -> Vec<String> {
    match v.get(key) {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect(),
        Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
        _ => Vec::new(),
    }
}

/// 경로의 마지막 이름.
fn base_name(p: &str) -> String {
    let t = p.trim_end_matches(['/', '\\']);
    t.rsplit(['/', '\\']).next().unwrap_or("").to_string()
}

const STATUSES: [Status; 5] = [Status::Done, Status::Check, Status::Stop, Status::Hold, Status::Wait];

pub fn ui(st: &mut AppState, ui: &mut egui::Ui) {
    let mut do_reload = false;
    let mut do_undo = false;

    let hw = ui.available_width();
    ui.allocate_ui_with_layout(egui::vec2(hw, 48.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_height(48.0);
        ui.spacing_mut().item_spacing.x = widgets::GAP_S;
        ui.label(RichText::new("기록").size(15.0).bold());
        ui.add_space(6.0);
        if ui.selectable_label(!st.history.show_log, "지난 이력").clicked() {
            st.history.show_log = false;
        }
        if ui.selectable_label(st.history.show_log, "이번 실행 기록").clicked() {
            st.history.show_log = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::tool(ui, "접기", true).clicked() {
                st.history.open = false;
            }
            if widgets::tool(ui, "되돌리기…", true).clicked() {
                do_undo = true;
            }
            if widgets::tool(ui, "새로 읽기", true).clicked() {
                do_reload = true;
            }
            if st.history.show_log {
                // 1.0.5 의 '기록 전체 복사'. 문의할 때 실행 기록을 통째로 붙여 넣을 수 있게 한다.
                let copied = st.history.log_copied_at.is_some_and(|t| t.elapsed().as_secs_f32() < 1.5);
                if copied {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
                }
                if widgets::tool(ui, if copied { "복사됨" } else { "전체 복사" }, !st.log_lines.is_empty()).clicked() {
                    ui.ctx().copy_text(st.log_lines.join("\n"));
                    st.history.log_copied_at = Some(std::time::Instant::now());
                }
            }
        });
    });

    if st.history.show_log {
        log_view(st, ui);
    } else {
        record_view(st, ui);
    }

    if do_reload {
        reload(st);
    }
    if do_undo {
        open_undo(st);
    }
}

/// 되돌리기 창을 연다. 기본 저장소: 고른 기록의 repo, 없으면 설정의 repo.
fn open_undo(st: &mut AppState) {
    let mut repo = String::new();
    if let Some(r) = st.history.selected.and_then(|i| st.history.records.get(i)) {
        repo = text(r, "repo");
    }
    if repo.is_empty() {
        repo = setting_str(&st.settings, "repo");
    }
    super::undo_view::open(st, &repo);
}

/// 접힌 기록 줄: 기록 건수와 마지막 기록 한 줄, [기록 펼치기]·[되돌리기…].
pub fn bar(st: &mut AppState, ui: &mut egui::Ui) {
    let mut do_undo = false;
    let w = ui.available_width();
    ui.allocate_ui_with_layout(egui::vec2(w, 48.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.set_height(48.0);
        ui.spacing_mut().item_spacing.x = widgets::GAP_S;
        if widgets::tool(ui, "기록 펼치기", true).clicked() {
            st.history.open = true;
        }
        if widgets::tool(ui, "되돌리기…", true).clicked() {
            do_undo = true;
        }
        // 남은 폭 안에서만 그리고 넘치면 잘린다(버튼을 침범하지 않는다).
        let rest = ui.available_rect_before_wrap();
        let clip = ui.clip_rect().intersect(rest);
        ui.scope_builder(egui::UiBuilder::new().max_rect(rest).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
            ui.set_clip_rect(clip);
            ui.spacing_mut().item_spacing.x = widgets::GAP_S;
            if let Some(v) = st.history.records.first() {
                ui.label(RichText::new("· 마지막:").color(theme::SUB));
                record_status(v).badge(ui);
                ui.label(RichText::new(text(v, "time")).color(theme::SUB));
                ui.label(super::common::kind_label(&text(v, "kind")));
                ui.add(egui::Label::new(RichText::new(base_name(&text(v, "repo"))).bold()).truncate());
            }
        });
    });
    if do_undo {
        open_undo(st);
    }
}

fn log_view(st: &mut AppState, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("로그 파일 경로").size(12.0).color(theme::SUB));
        ui.label(RichText::new(&st.history.log_path).size(12.0).monospace().color(theme::SUB));
    });
    let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
    let total = st.log_lines.len();
    egui::ScrollArea::vertical().id_salt("log_scroll").auto_shrink([false, false]).stick_to_bottom(true).show_rows(ui, row_h, total, |ui, range| {
        for i in range {
            if let Some(l) = st.log_lines.get(i) {
                ui.label(widgets::log_text(l));
            }
        }
    });
}

/// 필터 없이 목록에 그리는 최근 기록 수.
const RECENT_SHOWN: usize = 20;

fn record_view(st: &mut AppState, ui: &mut egui::Ui) {
    // 상태 필터 줄.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = widgets::GAP_S;
        let cur = match st.history.filter {
            None => "전체".to_string(),
            Some(s) => s.label().to_string(),
        };
        ui.scope(|ui| {
            ui.spacing_mut().interact_size.y = widgets::CTRL_H;
            egui::ComboBox::from_id_salt("hist_filter").selected_text(format!("상태: {}", cur)).width(150.0).show_ui(ui, |ui| {
                if widgets::choice_row(ui, None, "전체", st.history.filter.is_none()).clicked() {
                    st.history.filter = None;
                    ui.close();
                }
                for s in STATUSES {
                    if widgets::choice_row(ui, Some(s), s.label(), st.history.filter == Some(s)).clicked() {
                        st.history.filter = Some(s);
                        ui.close();
                    }
                }
            });
        });
        if st.history.view_key != Some(st.history.filter) {
            let f = st.history.filter;
            st.history.view = st.history.records.iter().enumerate().filter(|(_, v)| f.is_none() || f == Some(record_status(v))).map(|(i, _)| i).collect();
            st.history.view_key = Some(f);
        }
        let shown = match st.history.filter {
            None => format!("최근 {}건", st.history.view.len().min(RECENT_SHOWN)),
            Some(_) => format!("{}건", st.history.view.len()),
        };
        ui.label(RichText::new(shown).color(theme::SUB));
    });

    if st.history.records.is_empty() {
        widgets::empty_hint(ui, "아직 저장된 실행 이력이 없습니다. 적용하거나 만들면 이곳에 쌓입니다.");
        return;
    }

    let filter = st.history.filter;
    let mut picked: Option<usize> = None;
    let selected = st.history.selected;

    // 고른 줄의 자세한 내용은 오른쪽 칸에 둔다. 목록 줄이 밀려 가려지지 않는다.
    if let Some(v) = selected.and_then(|i| st.history.records.get(i)) {
        egui::Panel::right("history_detail")
            .exact_size(380.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().inner_margin(egui::Margin { left: 12, right: 0, top: 0, bottom: 0 }))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("history_detail_scroll").auto_shrink([false, false]).show(ui, |ui| {
                    detail(ui, v);
                });
            });
    }

    egui::ScrollArea::both().id_salt("history_scroll").auto_shrink([false, false]).show(ui, |ui| {
        egui::Grid::new("history_grid").striped(true).num_columns(6).min_row_height(28.0).spacing([22.0, 4.0]).show(ui, |ui| {
            for h in ["시각", "상태", "결과", "브랜치", "저장소", "번들 파일"] {
                ui.label(RichText::new(h).size(12.0).color(theme::SUB));
            }
            ui.end_row();
            // 필터가 없으면 가장 새 RECENT_SHOWN 건만 그린다. 필터를 고르면 읽어 둔 전체에서 맞는 것을 모두 그린다.
            let limit = if filter.is_none() { RECENT_SHOWN } else { usize::MAX };
            for &i in st.history.view.iter().take(limit) {
                let v = &st.history.records[i];
                let status = record_status(v);
                let time = text(v, "time");
                let on = selected == Some(i);
                if ui.selectable_label(on, &time).clicked() {
                    picked = Some(i);
                }
                status.badge(ui);
                ui.label(super::common::kind_label(&text(v, "kind")));
                ui.label(RichText::new(text(v, "branch")).monospace().color(theme::SUB));
                ui.label(RichText::new(base_name(&text(v, "repo"))).bold());
                let mut b = text(v, "bundleName");
                if b.is_empty() {
                    b = base_name(&text(v, "bundle"));
                }
                ui.label(b);
                ui.end_row();
            }
        });
    });

    if let Some(i) = picked {
        st.history.selected = Some(i);
    }
}

/// 고른 줄의 메시지·pushNote·commits. 멈춤이면 멈춤 안내를 함께 보인다.
fn detail(ui: &mut egui::Ui, v: &Value) {
    let status = record_status(v);
    let message = text(v, "message");
    ui.horizontal_wrapped(|ui| {
        status.badge(ui);
        ui.label(RichText::new(super::common::kind_label(&text(v, "kind"))).bold());
        ui.label(RichText::new(text(v, "branch")).monospace());
        ui.label(RichText::new(text(v, "time")).color(theme::SUB));
    });
    let repo = text(v, "repo");
    if !repo.is_empty() {
        widgets::small(ui, &format!("저장소: {}", repo));
    }
    let bundle = text(v, "bundle");
    if !bundle.is_empty() {
        widgets::small(ui, &format!("번들 파일: {}", bundle));
    }
    if !message.is_empty() {
        ui.label(&message);
    }
    let note = text(v, "pushNote");
    if !note.is_empty() {
        ui.label(RichText::new(note).color(Status::Check.color()));
    }
    let commits = lines_of(v, "commits");
    if !commits.is_empty() {
        egui::CollapsingHeader::new("이번에 들어간 커밋").id_salt("hist_commits").show(ui, |ui| {
            for c in &commits {
                ui.label(RichText::new(c).monospace());
            }
        });
    }
    if status == Status::Stop {
        let g = super::guide::stop_guide(&message);
        super::common::stop_guide_box(ui, &g.reason, &g.next);
    }
}
