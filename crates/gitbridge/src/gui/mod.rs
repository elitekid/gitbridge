//! GitBridge 2.0.0 화면(egui). 1.0.5 의 4탭(번들 적용·번들 만들기·사용법·업데이트)을 유지하고
//! 기록 창(실행 기록·실행 로그·되돌리기)을 아래에 하나로 묶는다.

pub mod apply_tab;
pub mod common;
pub mod export_preview;
pub mod export_tab;
pub mod guide;
pub mod help_tab;
pub mod history;
mod plan_list;
pub mod theme;
pub mod undo_view;
pub mod update;
pub mod updates_tab;
pub mod widgets;
pub mod worker;

use eframe::egui::{self, RichText};
use theme::Bold;
use gitbridge_core::{log, store, TOOL};
use serde_json::Value;
use std::process::ExitCode;
use std::sync::mpsc::{channel, Receiver};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Export,
    Apply,
    Help,
    Updates,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Export, Tab::Apply, Tab::Help, Tab::Updates];
    pub fn label(self) -> &'static str {
        match self {
            Tab::Apply => "번들 적용",
            Tab::Export => "번들 만들기",
            Tab::Help => "사용법",
            Tab::Updates => "업데이트",
        }
    }
}

/// 실행 로그를 화면에 남기는 최대 줄 수.
const LOG_KEEP: usize = 5000;

/// 화면 전체 상태. 탭마다 자기 상태 구조체를 가지고, 탭 함수는 AppState 전체를 받아 작업을 띄운다.
pub struct AppState {
    pub tab: Tab,
    /// settings.json 전체(1.0.5 키 그대로). 바꾸면 common::set_setting 으로 저장한다.
    pub settings: Value,
    pub jobs: worker::Jobs,
    /// 엔진 로그 줄(실행 로그 보기).
    pub log_lines: Vec<String>,
    log_rx: Receiver<String>,
    /// 상태 줄에 잠깐 보일 안내(설정 저장 실패 등).
    pub notice: String,
    pub apply: apply_tab::ApplyState,
    pub export: export_tab::ExportState,
    #[allow(dead_code)]
    pub help: help_tab::HelpState,
    #[allow(dead_code)]
    pub updates: updates_tab::UpdatesState,
    /// 새 버전 확인·교체 상태.
    pub upd: update::UpdateState,
    pub history: history::HistoryState,
    pub undo: undo_view::UndoState,
    /// 작업이 도는 동안 사용자가 바꾼 설정(키, 값). 엔진이 끝에 자기 스냅샷으로 설정 파일을 덮어쓰므로, 끝난 뒤 다시 얹어 사용자 변경을 지킨다.
    pub edits_during_job: Vec<(String, Value)>,
}

impl AppState {
    fn new(ctx: &egui::Context) -> AppState {
        let (tx, log_rx) = channel();
        log::init(false, store::log_file().ok().as_deref());
        log::set_tap(Some(tx));
        let mut notice = String::new();
        let settings = match store::load_settings() {
            Ok(v) => v,
            Err(e) => {
                notice = format!("설정을 읽지 못했습니다: {}", e);
                Value::Object(Default::default())
            }
        };
        let mut jobs = worker::Jobs::default();
        jobs.set_ctx(ctx.clone());
        let upd = update::UpdateState::new(update::auto_check(&settings));
        let mut st = AppState {
            tab: Tab::Export,
            settings,
            jobs,
            log_lines: Vec::new(),
            log_rx,
            notice,
            apply: Default::default(),
            export: Default::default(),
            help: Default::default(),
            updates: Default::default(),
            upd,
            history: Default::default(),
            undo: Default::default(),
            edits_during_job: Vec::new(),
        };
        if st.upd.enabled() {
            st.upd.start_check(ctx);
        }
        history::reload(&mut st);
        st
    }

    /// 작업을 띄운다. 이미 도는 작업이 있으면 안내만 하고 거짓.
    pub fn spawn<F>(&mut self, label: &str, work: F) -> bool
    where
        F: FnOnce(&worker::Progress) -> worker::Apply + Send + 'static,
    {
        if self.upd.restarting() {
            // 새 버전으로 바꿔 이미 다시 시작했다. 곧 닫히니 새 작업은 받지 않는다.
            return false;
        }
        let ok = self.jobs.spawn(label, work);
        if !ok {
            self.notice = "다른 작업이 아직 진행 중입니다. 끝난 뒤 다시 눌러 주세요.".into();
        }
        ok
    }

    pub fn busy(&self) -> bool {
        self.jobs.busy()
    }

    /// 설정 값 하나를 바꾸고 저장한다. 실패하면 상태 줄에 알린다.
    pub fn save_setting(&mut self, key: &str, value: Value) {
        if self.busy() {
            self.edits_during_job.push((key.to_string(), value.clone()));
        }
        if let Err(e) = common::set_setting(&mut self.settings, key, value) {
            self.notice = e;
        }
    }

    fn pump(&mut self) {
        while let Ok(l) = self.log_rx.try_recv() {
            self.log_lines.push(l);
        }
        if self.log_lines.len() > LOG_KEEP {
            let cut = self.log_lines.len() - LOG_KEEP;
            self.log_lines.drain(..cut);
        }
        let done = self.jobs.poll();
        if !done.is_empty() {
            for a in done {
                a(self);
            }
            // 작업이 끝나면 기록 창을 다시 읽는다.
            history::reload(self);
            self.edits_during_job.clear();
        }
    }
}

struct App {
    st: AppState,
    /// 연속으로 패닉한 프레임 수(성공한 프레임이면 0).
    fails: u32,
    /// 창 아이콘을 이미 정했는지(한 번만 시도한다).
    icon_done: bool,
}

/// 이 횟수만큼 연속으로 패닉하면 같은 곳에서 계속 나는 것이니 안전 화면만 그린다.
const FAIL_LIMIT: u32 = 3;

thread_local! {
    /// 이 스레드(화면 스레드)에서 마지막으로 난 패닉의 메시지와 위치. 훅은 여기에 적기만 한다.
    static LAST_PANIC: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn payload_text(p: &(dyn std::any::Any + Send)) -> Option<String> {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned())
}

/// 패닉 위치를 기록하는 훅을 건다. 로그·잠금은 쓰지 않는다. 원래 훅도 그대로 부른다.
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = payload_text(info.payload()).unwrap_or_else(|| "알 수 없는 오류".to_string());
        let text = match info.location() {
            Some(l) => format!("{} ({}:{})", msg, l.file(), l.line()),
            None => msg,
        };
        let _ = LAST_PANIC.try_with(|c| {
            if let Ok(mut b) = c.try_borrow_mut() {
                *b = Some(text);
            }
        });
        prev(info);
    }));
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.icon_done {
            self.icon_done = true;
            set_window_icon(&ctx, frame);
        }
        if self.fails >= FAIL_LIMIT {
            // 안전 화면: 같은 곳에서 계속 패닉하니 본 화면은 부르지 않는다.
            egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BACK).inner_margin(egui::Margin::same(24))).show(ui, |ui| {
                ui.label(RichText::new(&self.st.notice).color(theme::WARN));
                ui.add_space(12.0);
                if widgets::tool(ui, "다시 그리기", true).clicked() {
                    self.fails = 0;
                    self.st.tab = Tab::Apply;
                    self.st.notice.clear();
                }
            });
            return;
        }
        let st = &mut self.st;
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| draw(st, ui, &ctx))) {
            Ok(()) => self.fails = 0,
            Err(e) => {
                let why = LAST_PANIC.with(|c| c.borrow_mut().take()).or_else(|| payload_text(&*e)).unwrap_or_else(|| "알 수 없는 오류".to_string());
                self.fails += 1;
                self.st.notice = format!("화면을 그리다 오류가 나서 이번 화면을 건너뛰었습니다: {}", why);
                ctx.request_repaint();
            }
        }
    }
}

/// 한 프레임의 화면 본문.
fn draw(st: &mut AppState, ui: &mut egui::Ui, ctx: &egui::Context) {
    {
        st.pump();
        update::drive(st, ctx);
        if st.upd.restarting() {
            egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BACK).inner_margin(egui::Margin::same(24))).show(ui, |ui| {
                ui.label(RichText::new("새 버전으로 다시 시작하는 중…").color(theme::ACCENT));
            });
            return;
        }

        egui::Panel::top("tabs").frame(egui::Frame::new().fill(egui::Color32::WHITE).stroke(egui::Stroke::new(1.0, theme::LINE)).inner_margin(egui::Margin::symmetric(24, 0))).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.set_height(56.0);
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(RichText::new("GitBridge").size(20.0).bold().color(theme::ACCENT));
                ui.add_space(40.0);
                for t in Tab::ALL {
                    let active = st.tab == t;
                    let text = if active { RichText::new(t.label()).size(15.5).bold().color(theme::ACCENT) } else { RichText::new(t.label()).size(15.5).color(theme::SUB) };
                    let galley = ui.painter().layout_no_wrap(t.label().to_string(), egui::FontId::proportional(15.5), theme::TEXT);
                    let size = egui::vec2(galley.size().x + 36.0 + if active { 4.0 } else { 0.0 }, 56.0);
                    let (rect, r) = ui.allocate_exact_size(size, egui::Sense::click());
                    if r.hovered() && !active {
                        ui.painter().rect_filled(rect.shrink2(egui::vec2(0.0, 8.0)), egui::CornerRadius::same(6), theme::BACK);
                    }
                    let tg = ui.painter().layout_no_wrap(text.text().to_string(), if active { egui::FontId::new(15.5, theme::bold_family()) } else { egui::FontId::proportional(15.5) }, if active { theme::ACCENT } else { theme::SUB });
                    ui.painter().galley(rect.center() - tg.size() / 2.0, tg, theme::TEXT);
                    if active {
                        ui.painter().line_segment([egui::pos2(rect.left() + 6.0, rect.bottom() - 1.5), egui::pos2(rect.right() - 6.0, rect.bottom() - 1.5)], egui::Stroke::new(3.0, theme::ACCENT));
                    }
                    if r.clicked() {
                        st.tab = t;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("로컬에 적용 · 서버로 push 안 함   {}", TOOL)).size(12.0).color(theme::HEADER_SUB));
                });
            });
        });

        if st.upd.banner_visible() {
            egui::Panel::top("update_banner")
                .frame(egui::Frame::new().fill(theme::SEL_BG).stroke(egui::Stroke::new(1.0, theme::LINE)).inner_margin(egui::Margin::symmetric(24, 8)))
                .show(ui, |ui| update::banner(st, ui, ctx));
            if st.upd.restarting() {
                return;
            }
        }

        egui::Panel::bottom("status").frame(egui::Frame::new().fill(theme::CARD).inner_margin(egui::Margin::symmetric(24, 6))).show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(r) = &st.jobs.running {
                    ui.spinner();
                    ui.label(RichText::new(format!("{} — {}초", r.progress.stage(), r.started.elapsed().as_secs())).color(theme::ACCENT));
                    if r.progress.cancelled() {
                        ui.label(RichText::new("중단 요청됨").color(theme::WARN));
                        widgets::tool(ui, "중단 대기", false);
                    } else if widgets::tool(ui, "중단", true).on_hover_text("서버 조회·받기는 바로 끊고, 진행 중인 로컬 단계는 끝날 때까지 기다린 뒤 멈춥니다.").clicked() {
                        r.progress.request_cancel();
                    }
                } else if !st.notice.is_empty() {
                    ui.label(RichText::new(&st.notice).color(theme::WARN));
                    if widgets::tool(ui, "닫기", true).clicked() {
                        st.notice.clear();
                    }
                } else {
                    ui.label(RichText::new("준비").color(theme::IDLE));
                }
            });
        });

        // 위 구분선은 패널이 그린다(테두리는 두지 않아 아래 상태 줄 선과 겹치지 않는다).
        let hframe = |m: i8| egui::Frame::new().fill(theme::BACK).inner_margin(egui::Margin::symmetric(24, m));
        if st.history.open {
            egui::Panel::bottom("history").resizable(true).default_size(220.0).min_size(150.0).frame(hframe(0)).show(ui, |ui| {
                history::ui(st, ui);
            });
        } else {
            egui::Panel::bottom("history_bar").resizable(false).exact_size(49.0).frame(hframe(0)).show(ui, |ui| {
                history::bar(st, ui);
            });
        }

        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BACK).inner_margin(egui::Margin { left: 24, right: 24, top: 12, bottom: 16 })).show(ui, |ui| match st.tab {
            Tab::Apply => apply_tab::ui(st, ui),
            Tab::Export => export_tab::ui(st, ui),
            Tab::Help => help_tab::ui(st, ui),
            Tab::Updates => updates_tab::ui(st, ui),
        });

        undo_view::window(st, ctx);

        if st.jobs.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}

fn icon64() -> egui::IconData {
    egui::IconData { rgba: include_bytes!("../../assets/git-bridge-64.rgba").to_vec(), width: 64, height: 64 }
}

/// Windows 는 eframe 이 64px 그림 하나를 줄여 쓰면 작은 크기가 흐려진다. 빈 아이콘을 줘서 eframe 이 건드리지 않게 하고,
/// 창이 생긴 뒤 실행 파일 리소스에서 시스템 크기에 맞는 아이콘을 직접 넣는다.
#[cfg(windows)]
fn startup_icon() -> egui::IconData {
    egui::IconData::default()
}
#[cfg(not(windows))]
fn startup_icon() -> egui::IconData {
    icon64()
}

#[cfg(not(windows))]
fn set_window_icon(_ctx: &egui::Context, _frame: &eframe::Frame) {}

/// 실행 파일 리소스 1번 아이콘을 작은 아이콘·큰 아이콘 크기로 불러 창에 넣는다. 못 하면 64px 그림으로 한 번 대신한다.
#[cfg(windows)]
fn set_window_icon(ctx: &egui::Context, frame: &eframe::Frame) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, LoadImageW, SendMessageW, ICON_BIG, ICON_SMALL, IMAGE_ICON, SM_CXICON, SM_CXSMICON, WM_SETICON,
    };
    let hwnd = match frame.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
        other => {
            log::say(&format!("창 아이콘: 창 핸들을 얻지 못해 64px 그림을 씁니다({}).", if other.is_err() { "오류" } else { "Win32 아님" }));
            ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(std::sync::Arc::new(icon64()))));
            return;
        }
    };
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        let load = |size: i32| LoadImageW(module, 1usize as *const u16, IMAGE_ICON, size, size, 0);
        let small = load(GetSystemMetrics(SM_CXSMICON));
        let big = load(GetSystemMetrics(SM_CXICON));
        if small.is_null() || big.is_null() {
            log::say("창 아이콘: 실행 파일에서 아이콘 리소스를 불러오지 못해 64px 그림을 씁니다.");
            ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(std::sync::Arc::new(icon64()))));
            return;
        }
        SendMessageW(hwnd as _, WM_SETICON, ICON_SMALL as usize, small as isize);
        SendMessageW(hwnd as _, WM_SETICON, ICON_BIG as usize, big as isize);
    }
}

pub fn run() -> ExitCode {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(TOOL)
            .with_inner_size([1280.0, 900.0])
            .with_min_inner_size([1100.0, 720.0])
            .with_icon(startup_icon()),
        ..Default::default()
    };
    install_panic_hook();
    let res = eframe::run_native(
        TOOL,
        options,
        Box::new(|cc| {
            theme::install(&cc.egui_ctx);
            Ok(Box::new(App { st: AppState::new(&cc.egui_ctx), fails: 0, icon_done: false }))
        }),
    );
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = std::fs::write(
                store::log_file().map(|p| p.with_file_name("GitBridge2-gui-error.txt")).unwrap_or_else(|_| "GitBridge2-gui-error.txt".into()),
                format!("화면을 띄우지 못했습니다: {}\n", e),
            );
            ExitCode::from(1)
        }
    }
}
