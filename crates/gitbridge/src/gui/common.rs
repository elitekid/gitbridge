//! 탭들이 함께 쓰는 타입과 도우미. 이 파일의 공개 항목 이름·모양은 고정이다.

use super::{theme, widgets};
use super::theme::Bold;
use eframe::egui::{self, Color32, RichText};
use gitbridge_core::refs::Outcome;
use gitbridge_core::store;
use serde_json::Value;

/// 9.2 상태 이름 다섯 개.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// 완료
    Done,
    /// 확인 필요
    Check,
    /// 멈춤
    Stop,
    /// 보류
    Hold,
    /// 대기
    Wait,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Done => "완료",
            Status::Check => "확인 필요",
            Status::Stop => "멈춤",
            Status::Hold => "보류",
            Status::Wait => "대기",
        }
    }
    pub fn color(self) -> Color32 {
        match self {
            Status::Done => theme::OK,
            Status::Check => theme::WARN,
            Status::Stop => theme::ERR,
            Status::Hold | Status::Wait => theme::IDLE,
        }
    }
    /// 색 있는 상태 표지.
    pub fn badge(self, ui: &mut egui::Ui) -> egui::Response {
        widgets::status_mark(ui, self, self.label())
    }
}

/// 엔진 결과 종류·성공 여부를 상태로 바꾼다.
/// 멈춤·실패 → 멈춤, 대기 → 대기, 건너뜀·예전 번들 → 보류,
/// 일부 적용 → 확인 필요, 나머지 성공 → 완료(기준 브랜치가 앞섬·덧붙임 안내는 상태에 넣지 않는다).
pub fn status_of(kind: &str, ok: bool) -> Status {
    match kind {
        "대기" => return Status::Wait,
        "건너뜀" | "예전 번들" => return Status::Hold,
        _ => {}
    }
    if !ok || kind == "멈춤" {
        return Status::Stop;
    }
    if kind == "일부 적용" {
        return Status::Check;
    }
    Status::Done
}

/// 화면에 보일 결과 종류 이름. 저장·비교에는 원래 값을 쓰고, 보이는 곳에서만 이 함수를 거친다.
pub fn kind_label(kind: &str) -> String {
    match kind {
        "주요 참조 번들 생성" => "주요 브랜치 번들 생성".to_string(),
        "주요 참조 증분 번들 생성" => "주요 브랜치 바뀐 부분만 번들 생성".to_string(),
        other => other.to_string(),
    }
}

pub fn outcome_status(o: &Outcome) -> Status {
    status_of(&o.kind, o.ok)
}

/// history.jsonl 한 줄(엔진 store 형식)의 상태.
pub fn record_status(v: &Value) -> Status {
    status_of(
        v.get("kind").and_then(|x| x.as_str()).unwrap_or(""),
        v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false),
    )
}

/// 설정 문자열 값(1.0.5 settings.json 키 그대로: repo, bundleDir, exportDir, srcRoot, scanRoot ...).
pub fn setting_str(settings: &Value, key: &str) -> String {
    settings.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

/// 설정 값 하나를 바꾸고 바로 저장한다. 저장 실패는 문구로 돌려준다.
pub fn set_setting(settings: &mut Value, key: &str, value: Value) -> Result<(), String> {
    if !settings.is_object() {
        *settings = Value::Object(Default::default());
    }
    if let Some(m) = settings.as_object_mut() {
        m.insert(key.to_string(), value);
    }
    store::save_settings(settings)
}

/// 폴더 고르기 대화상자. start 가 있으면 그 폴더에서 연다.
pub fn pick_folder(title: &str, start: &str) -> Option<String> {
    let mut d = rfd::FileDialog::new().set_title(title);
    if !start.is_empty() && std::path::Path::new(start).is_dir() {
        d = d.set_directory(start);
    }
    d.pick_folder().map(|p| p.display().to_string())
}

/// 번들·ZIP 파일 여러 개 고르기.
pub fn pick_bundles(start: &str) -> Vec<String> {
    let mut d = rfd::FileDialog::new()
        .set_title("번들 파일 추가")
        .add_filter("번들·ZIP (*.bundle, *.zip)", &["bundle", "zip"])
        .add_filter("모든 파일", &["*"]);
    if !start.is_empty() && std::path::Path::new(start).is_dir() {
        d = d.set_directory(start);
    }
    d.pick_files().unwrap_or_default().into_iter().map(|p| p.display().to_string()).collect()
}

/// 결과 하나를 상태 표지 + 본문으로 그린다. 멈춤이면 멈춤 안내(이유·다음 할 일)를 함께 보인다.
pub fn show_outcome(ui: &mut egui::Ui, o: &Outcome) {
    let st = outcome_status(o);
    ui.horizontal_wrapped(|ui| {
        st.badge(ui);
        ui.label(RichText::new(kind_label(&o.kind)).bold());
        if let Some(b) = &o.branch {
            if !b.is_empty() {
                ui.label(RichText::new(b).monospace());
            }
        }
    });
    ui.label(&o.message);
    if let Some(p) = &o.push_note {
        if !p.is_empty() {
            ui.label(RichText::new(p).color(Status::Check.color()));
        }
    }
    if !o.commits.is_empty() {
        egui::CollapsingHeader::new("이번에 들어간 커밋").id_salt(("commits", &o.message)).show(ui, |ui| {
            for c in &o.commits {
                ui.label(RichText::new(c).monospace());
            }
        });
    }
    if !o.branch_details.is_empty() {
        egui::CollapsingHeader::new("서버 브랜치 마지막 커밋").id_salt(("details", &o.message)).show(ui, |ui| {
            for c in &o.branch_details {
                ui.label(RichText::new(c).monospace());
            }
        });
    }
    if let Some(a) = o.main_ahead {
        if a > 0 {
            ui.label(RichText::new(format!("서버 기준 브랜치가 이 브랜치보다 {}개 앞서 있습니다. 머지는 인텔리제이에서 해 주세요.", a)).color(Status::Check.color()));
        }
    }
    if let Some(f) = &o.file {
        // 긴 경로가 줄바꿈 없이 한 줄로 늘어나 본문 폭을 밀어내지 않도록 한 라벨로 줄바꿈한다.
        ui.add(egui::Label::new(RichText::new(format!("파일: {}", f)).monospace()).wrap());
    }
    if let Some(u) = &o.undo {
        ui.label(RichText::new(format!("되돌리기 기록: {} (기록 창의 되돌리기에서 복원할 수 있습니다)", u)).weak());
    }
    if st == Status::Stop {
        let g = super::guide::stop_guide(&o.message);
        stop_guide_box(ui, &g.reason, &g.next);
    }
}

/// 결과 여러 개를 차례로 그린다.
pub fn show_outcomes(ui: &mut egui::Ui, list: &[Outcome]) {
    for (i, o) in list.iter().enumerate() {
        if i > 0 {
            ui.separator();
        }
        ui.push_id(i, |ui| show_outcome(ui, o));
    }
}

/// 결과를 기록 파일에 남긴다. 실패하면 로그에 남긴다(작업 스레드에서 부른다).
pub fn record(o: &Outcome, repo: &str, bundle: Option<&str>) {
    if let Err(e) = store::append_history(o, repo, bundle) {
        gitbridge_core::log::say(&format!("기록을 남기지 못했습니다: {}", e));
    }
}

/// 멈춤 안내 상자(멈춘 이유·다음 할 일).
pub fn stop_guide_box(ui: &mut egui::Ui, reason: &str, next: &str) {
    widgets::notice_box(ui, theme::ERR_BG, theme::ERR, |ui| {
        ui.label(RichText::new("멈춤 안내").bold().color(theme::ERR));
        ui.label(format!("멈춘 이유: {}", reason));
        ui.label(format!("다음 할 일: {}", next));
    });
}
