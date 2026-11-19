//! "번들 적용" 탭. 번들 파일 목록('번들 → 저장소' 줄 목록)이나 번들 폴더(큐)를 저장소에 적용하고, 적용 전에 미리보기를 보인다.
//! 2.0.0 은 원격에 쓰지 않는다(로컬 적용 전용).

use super::common::{pick_bundles, pick_folder, setting_str, show_outcomes};
use super::plan_list::PlanList;
use super::theme::Bold;
use super::worker::{Apply, Progress};
use super::{theme, widgets, AppState};
use eframe::egui::{self, CornerRadius, Margin, RichText, Stroke};
use gitbridge_core::queue::{self, QueuePlan};
use gitbridge_core::refs::Outcome;
use serde_json::json;

#[derive(Default)]
pub struct ApplyState {
    /// 설정에서 처음 한 번 읽었는지.
    loaded: bool,
    repo: String,
    bundle_dir: String,
    src_root: String,
    /// 거짓이면 번들 파일 목록, 참이면 번들 폴더(큐).
    mode_dir: bool,
    /// 파일 목록 모드의 줄 목록(엔진 QueuePlan + 화면 쪽 체크·선택·결과).
    list: PlanList,
    /// 마지막으로 줄을 짝지을 때 쓴 저장소 모음 폴더(같은 값이면 다시 찾지 않는다).
    matched_root: String,
    /// 저장소 모음 폴더가 바뀌었는데 다른 작업 때문에 아직 다시 찾지 못했다.
    rematch_pending: bool,
    /// 마지막 실행 결과.
    outcomes: Vec<Outcome>,
    /// 미리보기 결과.
    preview_outcomes: Vec<Outcome>,
    preview_title: String,
    /// 전체 동기화 확인 대화가 열려 있으면 전체 동기화로 정해진 번들 수, 실행할 줄, 그 줄들의 (번들, 저장소, 전체 동기화).
    sync_confirm: Option<(usize, Vec<usize>, Vec<(String, String, bool)>)>,
    /// '브랜치 이름 복사'를 누른 시각(초). 잠시 '복사됨'으로 보인다.
    branch_copied: Option<f64>,
    /// '결과 크게 보기' 창(열려 있으면 값이 있다).
    big: Option<widgets::BigText>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    None,
    Apply,
    Preview,
    Undo,
}

/// 줄 목록 도구 줄의 누른 단추.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PlanAct {
    None,
    Add,
    Clear,
    SetRepo,
    NewRepo,
    Remove,
    Up,
    Down,
}

/// 끌어다 놓은 항목을 받는다. 폴더는 번들 폴더(큐)로, .bundle/.zip 파일은 줄 목록에 더할 경로로 돌려준다.
fn take_drops(st: &mut AppState, ctx: &egui::Context) -> (Vec<(&'static str, String)>, Vec<String>) {
    let dropped: Vec<std::path::PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
    let mut saves = Vec::new();
    let mut add: Vec<String> = Vec::new();
    if dropped.is_empty() {
        return (saves, add);
    }
    let mut skipped = 0;
    for p in dropped {
        let text = p.display().to_string();
        if p.is_dir() {
            st.apply.bundle_dir = text.clone();
            st.apply.mode_dir = true;
            saves.push(("bundleDir", text));
        } else {
            let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if ext == "bundle" || ext == "zip" {
                st.apply.mode_dir = false;
                if !add.contains(&text) {
                    add.push(text);
                }
            } else {
                skipped += 1;
            }
        }
    }
    if skipped > 0 {
        st.notice = format!("번들(.bundle)·ZIP 이 아닌 파일 {}개는 받지 않았습니다.", skipped);
    }
    (saves, add)
}

/// 이름표를 위에 둔 경로 설정(좁은 설정 칸용): 입력 칸 + 단추 하나(같은 높이).
#[allow(clippy::too_many_arguments)]
fn side_path(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str, btn: &str, title: &str, key: &'static str, saves: &mut Vec<(&'static str, String)>) {
    widgets::labeled(ui, label, |ui| {
        let r = widgets::path_field(ui, value, hint, ui.available_width());
        if r.lost_focus() {
            saves.push((key, value.trim().to_string()));
        }
        ui.add_space(widgets::GAP_S);
        if widgets::secondary_full(ui, btn, true).clicked() {
            let start = value.clone();
            if let Some(p) = pick_folder(title, &start) {
                *value = p.clone();
                saves.push((key, p));
            }
        }
    });
}

pub fn ui(st: &mut AppState, ui: &mut egui::Ui) {
    if !st.apply.loaded {
        st.apply.repo = setting_str(&st.settings, "repo");
        st.apply.bundle_dir = setting_str(&st.settings, "bundleDir");
        st.apply.src_root = setting_str(&st.settings, "srcRoot");
        st.apply.matched_root = st.apply.src_root.trim().to_string();
        st.apply.loaded = true;
    }
    let ctx = ui.ctx().clone();
    // 줄 목록 plan 이 작업 중 비정상으로 사라졌으면 비워서 다시 시작한다.
    let running = st.busy();
    if st.apply.list.ensure(running) {
        st.notice = "번들 목록을 읽던 작업이 끝나지 못해 목록을 비웠습니다. 다시 올려 주세요.".into();
    }
    // 전체 동기화 확인 창이 떠 있는 동안에는 끌어다 놓은 파일을 받지 않는다(확인한 대상이 바뀌지 않게).
    let (mut saves, dropped) = if st.apply.sync_confirm.is_none() { take_drops(st, &ctx) } else { (Vec::new(), Vec::new()) };
    if !dropped.is_empty() {
        start_add(st, dropped);
    }
    // 저장소 모음 폴더가 바뀌었는데 작업 때문에 미뤄 둔 다시 찾기.
    if st.apply.rematch_pending && !st.apply.mode_dir && !st.busy() {
        start_rematch(st);
    }
    let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
    let busy = st.busy();
    let mut act = Act::None;
    let mut exec_act = Act::None;
    let mut plan_act = PlanAct::None;
    let mut sync_toggle: Option<bool> = None;
    let mut copied_now = false;

    {
        let s = &mut st.apply;
        widgets::head(ui, "번들 적용", "받은 번들을 이 컴퓨터의 저장소에 적용합니다. 서버에는 아무것도 쓰지 않습니다.");
        let (total, k, missing) = s.list.counts();
        let n_sel = s.list.selected().len();
        let loading = s.list.is_loading();
        // 줄 하나만 골랐을 때 쓰는 단추: 적용 순서 위로/아래로, 이 줄만 전체 동기화.
        let one = s.list.selected_one();
        let (can_up, can_down) = s.list.can_move();
        let sync_can = one.is_some_and(|i| s.list.can_sync(i));
        let sync_on = one.is_some_and(|i| s.list.is_sync(i));
        let list_editable = !busy && !loading && s.sync_confirm.is_none();
        // 브랜치 이름 복사: 결과가 있는 줄 하나를 골랐으면 그 줄의 브랜치, 아니면 마지막 적용 결과 중 브랜치가 있는 것.
        let branch_val: String = match s.list.selected_result_branch() {
            Some(b) => b,
            None => s.outcomes.iter().rev().filter_map(|o| o.branch.clone()).find(|b| !b.is_empty()).unwrap_or_default(),
        };
        let now = ctx.input(|i| i.time);
        let branch_copied = s.branch_copied.is_some_and(|t| now - t < 1.5);
        if branch_copied {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
        let apply_label = if s.mode_dir {
            "폴더의 번들 적용".to_string()
        } else if k > 0 {
            format!("번들 {}개 적용", k)
        } else {
            "번들 적용".to_string()
        };
        // 파일 목록 모드는 적용할 줄이 하나도 없으면 실행·미리보기를 막는다.
        let can_run = !busy && s.sync_confirm.is_none() && (s.mode_dir || k > 0);
        let exec_hint = if busy {
            "다른 작업이 진행 중입니다."
        } else if !s.mode_dir && k == 0 {
            if total == 0 {
                "번들을 먼저 올려 주세요."
            } else {
                "체크하고 저장소가 있는 번들이 없습니다."
            }
        } else {
            "미리보기는 저장소를 바꾸지 않고 결과만 보여 줍니다."
        };

        // 오른쪽 열: 적용 설정 + 맨 아래 실행 상자
        widgets::side_column(
            ui,
            "apply_side",
            |ui| {
                widgets::card(ui, Some("적용 설정"), |ui| {
                    widgets::labeled(ui, "입력 방식", |ui| {
                        let mut idx = if s.mode_dir { 1 } else { 0 };
                        if widgets::segmented(ui, &mut idx, &["번들 파일 목록", "번들 폴더(큐)"]) {
                            s.mode_dir = idx == 1;
                        }
                    });
                    if s.mode_dir {
                        side_path(ui, "번들 폴더", &mut s.bundle_dir, "번들 파일이 들어오는 폴더", "바꾸기…", "번들 파일이 들어오는 폴더", "bundleDir", &mut saves);
                        side_path(ui, "저장소 모음 폴더", &mut s.src_root, "저장소들이 모여 있는 폴더", "바꾸기…", "저장소 모음 폴더 선택", "srcRoot", &mut saves);
                    } else {
                        side_path(ui, "저장소 모음 폴더", &mut s.src_root, "저장소들이 모여 있는 폴더", "바꾸기…", "저장소 모음 폴더 선택", "srcRoot", &mut saves);
                        widgets::small(ui, "번들마다 이 폴더 아래에서 저장소를 자동으로 찾습니다. 폴더를 바꾸면 저장소 없는 줄을 다시 찾습니다.");
                    }
                });
            },
            |ui| {
                ui.spacing_mut().item_spacing.y = widgets::GAP_S;
                widgets::small(ui, exec_hint);
                ui.add_space(4.0);
                if widgets::primary_full(ui, &apply_label, can_run).clicked() {
                    exec_act = Act::Apply;
                }
                if widgets::secondary_full(ui, "미리보기", can_run).on_hover_text("저장소를 바꾸지 않고 적용하면 어떻게 될지만 보여 줍니다.").clicked() {
                    exec_act = Act::Preview;
                }
            },
        );

        // 왼쪽 본문: 적용 대기 목록(위, 남는 높이 전부) + 판정·결과(아래, 비었으면 한 줄 높이).
        // 위아래 카드 사이 간격은 gap 하나만 쓴다(기본 항목 간격이 더해져 아래로 넘치던 원인).
        ui.spacing_mut().item_spacing.y = 0.0;
        // 왼쪽 본문은 자기 열 밖으로 그려지지 않게 자른다(긴 경로 줄이 카드 폭을 밀어 옆 카드를 덮던 문제의 방어선).
        let col_rect = ui.available_rect_before_wrap();
        ui.set_clip_rect(col_rect.intersect(ui.clip_rect()));
        let gap = widgets::GAP_L;
        let avail = ui.available_height();
        let has_res = !s.outcomes.is_empty() || !s.preview_outcomes.is_empty();
        // 위·아래 카드 높이의 합이 남은 높이(avail)를 넘지 않게 한다: 최소 높이를 억지로 주면(위 200 + 아래 140)
        // 기록 창이 열려 본문이 낮을 때 아래 결과 카드가 창 밖으로 밀려 마지막 항목이 잘렸다.
        let res_h = if has_res { ((avail - gap) * 0.40).max(100.0).min((avail - gap - 90.0).max(60.0)) } else { 80.0 };
        let top_h = if s.mode_dir { 150.0_f32.min(avail - gap - res_h).max(90.0) } else { (avail - gap - res_h).max(90.0) };
        let w = ui.available_width();
        let stroke = if hovering { Stroke::new(2.0, theme::ACCENT) } else { Stroke::new(1.0, theme::LINE) };
        ui.allocate_ui_with_layout(egui::vec2(w, top_h), egui::Layout::top_down(egui::Align::Min), |ui| {
            egui::Frame::new().fill(theme::CARD).stroke(stroke).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(16, 12)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_height(top_h - 26.0);
                if s.mode_dir {
                    ui.label(RichText::new("번들 폴더(큐)").size(15.0).bold());
                    ui.add_space(widgets::GAP_S);
                    widgets::small(
                        ui,
                        "폴더 안의 번들·ZIP 을 위에서부터 차례로 적용합니다. 저장소는 저장소 모음 폴더 아래에서 찾고, 이미 적용한 번들은 건너뜁니다. 맞는 저장소가 없을 때 전체를 담은 번들은 저장소 모음 폴더 아래에 새 저장소로 만듭니다.",
                    );
                    if hovering {
                        ui.add_space(widgets::GAP_S);
                        widgets::notice_box(ui, theme::SEL_BG, theme::ACCENT, |ui| {
                            ui.label("놓으면 번들 폴더로 씁니다. 번들 파일을 놓으려면 먼저 [번들 파일 목록]으로 바꿔 주세요.");
                        });
                    }
                } else {
                    // 제목 + 한 줄 요약
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = widgets::GAP_S;
                        ui.label(RichText::new("적용 대기 목록").size(15.0).bold());
                        if loading {
                            ui.label(RichText::new("불러오는 중…").bold().color(theme::ACCENT));
                        } else {
                            ui.label(RichText::new(s.list.summary_text()).bold().color(theme::ACCENT));
                        }
                    });
                    ui.add_space(2.0);
                    widgets::small(ui, "번들을 올리면 번들마다 저장소를 바로 찾아 보여 줍니다. 저장소가 없는 번들은 적용하지 않으니 [저장소 바꾸기…]나 [새 저장소 만들기…]로 정해 주세요.");
                    if !loading && missing > 0 && s.src_root.trim().is_empty() {
                        widgets::small(ui, "저장소 모음 폴더를 정하면 저장소를 자동으로 찾습니다.");
                    }
                    ui.add_space(widgets::GAP_S);
                    // 도구 줄
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = widgets::GAP_S;
                        let editable = list_editable;
                        if widgets::tool(ui, "파일 추가…", editable).clicked() {
                            plan_act = PlanAct::Add;
                        }
                        if widgets::tool(ui, "비우기", editable && total > 0).clicked() {
                            plan_act = PlanAct::Clear;
                        }
                        ui.add_space(widgets::GAP_M);
                        let sel_ok = editable && n_sel > 0;
                        if widgets::tool(ui, "저장소 바꾸기…", sel_ok).on_hover_text("고른 줄에 적용할 git 저장소 폴더를 정합니다.").clicked() {
                            plan_act = PlanAct::SetRepo;
                        }
                        if widgets::tool(ui, "새 저장소 만들기…", sel_ok).on_hover_text("고른 기준·저장소 번들을 상위 폴더 아래 새 저장소로 만듭니다.").clicked() {
                            plan_act = PlanAct::NewRepo;
                        }
                        if widgets::tool(ui, "목록에서 빼기", sel_ok).clicked() {
                            plan_act = PlanAct::Remove;
                        }
                    });
                    ui.add_space(4.0);
                    // 한 줄만 골랐을 때 쓰는 단추줄: 적용 순서 바꾸기 + 이 줄만 전체 동기화
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = widgets::GAP_S;
                        if widgets::tool(ui, "위로", list_editable && can_up).on_hover_text("고른 번들 한 개를 적용 순서에서 한 칸 앞으로 옮깁니다.").clicked() {
                            plan_act = PlanAct::Up;
                        }
                        if widgets::tool(ui, "아래로", list_editable && can_down).on_hover_text("고른 번들 한 개를 적용 순서에서 한 칸 뒤로 옮깁니다.").clicked() {
                            plan_act = PlanAct::Down;
                        }
                        ui.add_space(widgets::GAP_M);
                        let mut on = sync_on;
                        let r = ui
                            .add_enabled(list_editable && sync_can, egui::Checkbox::new(&mut on, "전체 동기화"))
                            .on_hover_text("고른 한 줄만 이 번들로 전체 동기화합니다. 저장소 전체 번들(모든 서버 브랜치 + 태그)을 기존 저장소 폴더에 적용할 때만 고를 수 있습니다.");
                        if r.changed() {
                            sync_toggle = Some(on);
                        }
                    });
                    ui.add_space(4.0);
                    let msg = if hovering { "여기에 놓으면 목록에 더합니다." } else { "번들 또는 ZIP 을 이 칸에 끌어다 놓거나, 위의 [파일 추가…]를 눌러 주세요." };
                    s.list.table(ui, msg, !list_editable);
                }
            });
        });
        ui.add_space(gap);

        // 판정 · 실행 결과 카드(미리보기 포함)
        let h = ui.available_height();
        let w = ui.available_width();
        ui.allocate_ui_with_layout(egui::vec2(w, h), egui::Layout::top_down(egui::Align::Min), |ui| {
            egui::Frame::new().fill(theme::CARD).stroke(Stroke::new(1.0, theme::LINE)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(16, 12)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_height((h - 26.0).max(40.0));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = widgets::GAP_S;
                    ui.label(RichText::new("판정 · 실행 결과").size(15.0).bold());
                    if !s.outcomes.is_empty() {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let has_undo = s.outcomes.iter().any(|o| o.undo.is_some());
                            if has_undo && widgets::tool(ui, "되돌리기 열기", true).clicked() {
                                act = Act::Undo;
                            }
                            let result_text = || s.outcomes.iter().map(|o| format!("[{}] {}", o.kind, o.message)).collect::<Vec<_>>().join("\n");
                            if widgets::tool(ui, "결과 복사", true).clicked() {
                                ui.ctx().copy_text(result_text());
                            }
                            if widgets::tool(ui, "결과 크게 보기", true).clicked() {
                                s.big = Some(widgets::BigText::new("번들 적용 결과", result_text()));
                            }
                            let branch_label = if branch_copied { "복사됨" } else { "브랜치 이름 복사" };
                            if widgets::tool(ui, branch_label, !branch_val.is_empty()).on_hover_text("고른 줄(없으면 마지막 적용 결과)의 브랜치 이름을 복사합니다.").clicked() {
                                ui.ctx().copy_text(branch_val.clone());
                                copied_now = true;
                            }
                        });
                    }
                });
                ui.add_space(widgets::GAP_S);
                if s.outcomes.is_empty() && s.preview_outcomes.is_empty() {
                    widgets::small(ui, "아직 실행하지 않았습니다. 적용하면 이곳에 결과가 나옵니다.");
                    return;
                }
                egui::ScrollArea::vertical().id_salt("apply_results").auto_shrink([false, false]).show(ui, |ui| {
                    if !s.preview_outcomes.is_empty() {
                        widgets::small(ui, if s.preview_title.is_empty() { "미리보기" } else { &s.preview_title });
                        widgets::log_box(ui, |ui| {
                            show_outcomes(ui, &s.preview_outcomes);
                        });
                        ui.add_space(widgets::GAP_S);
                    }
                    if s.outcomes.is_empty() {
                        widgets::small(ui, "아직 실행하지 않았습니다. 적용하면 이곳에 결과가 나옵니다.");
                    } else {
                        show_outcomes(ui, &s.outcomes);
                    }
                    ui.add_space(widgets::GAP_M);
                });
            });
        });
    }

    // 전체 동기화 실행 전 확인(1.0.5 의 예/아니오 대화와 같은 뜻).
    let mut confirm_yes = false;
    let mut confirm_no = false;
    if let Some(n) = st.apply.sync_confirm.as_ref().map(|c| c.0) {
        // 모달이라 답하기 전에는 뒤 화면(탭·목록·기록 창)을 누를 수 없다. 바깥을 누르거나 Esc 는 '아니오'다.
        let modal = egui::Modal::new(egui::Id::unique("sync_confirm")).show(&ctx, |ui| {
            ui.set_width(460.0);
            ui.label(RichText::new("원본 전체 상태로 동기화").size(15.0).bold());
            ui.add_space(6.0);
            ui.label(format!("전체 동기화를 고른 {}개 번들에만 적용합니다.", n));
            ui.label("현재 체크아웃한 feature/*와 작업 파일만 유지하고, 나머지 로컬 브랜치·태그를 번들과 맞춥니다.");
            ui.add_space(6.0);
            ui.label("교체 전 참조는 복구 영역에 남기고 원격에는 접속하지 않습니다. 계속할까요?");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if widgets::tool(ui, "예", true).clicked() {
                    confirm_yes = true;
                }
                if widgets::tool(ui, "아니오", true).clicked() {
                    confirm_no = true;
                }
            });
        });
        if !confirm_yes && modal.should_close() {
            confirm_no = true;
        }
    }
    // 확인 창 다음에 그려 Esc 는 확인 창이 먼저 받는다.
    widgets::big_text_window(&ctx, &mut st.apply.big);
    if copied_now {
        st.apply.branch_copied = Some(ctx.input(|i| i.time));
        ctx.request_repaint();
    }
    if let Some(on) = sync_toggle {
        if let Some(i) = st.apply.list.selected_one() {
            st.apply.list.set_sync(i, on);
        }
    }
    if confirm_no {
        st.apply.sync_confirm = None;
    }
    if confirm_yes {
        // 확인하는 동안 목록이 바뀌었을 수 있어, 확인한 개수와 지금 개수가 같을 때만 바로 실행한다.
        let asked = st.apply.sync_confirm.take();
        let now_idx = st.apply.list.run_indices();
        let now = st.apply.list.sync_counts(&now_idx).0;
        let now_targets = run_targets(st, &now_idx);
        // 개수·줄 번호뿐 아니라 줄마다 번들·저장소·전체 동기화 여부가 같아야 하고, 그사이 폴더 모드로 바뀌지 않았어야 한다.
        let same = asked.as_ref().is_some_and(|(n, v, t)| *n == now && *v == now_idx && *t == now_targets);
        if same && !st.apply.mode_dir {
            run_plan_checked(st, true, true);
        } else {
            st.notice = "확인하는 사이 전체 동기화 대상이 바뀌어 실행하지 않았습니다. 다시 적용해 주세요.".into();
        }
    }

    if exec_act != Act::None {
        act = exec_act;
    }
    let mut want_rematch = false;
    for (k, v) in saves {
        if k == "srcRoot" {
            want_rematch = true;
        }
        st.save_setting(k, json!(v));
    }
    // 저장소 모음 폴더가 바뀌면 저장소 없는 줄을 다시 짝짓는다(파일 목록 모드만).
    if want_rematch && !st.apply.mode_dir {
        request_rematch(st);
    }
    match plan_act {
        PlanAct::None => {}
        PlanAct::Add => {
            let start = st.apply.bundle_dir.clone();
            let picked = pick_bundles(&start);
            start_add(st, picked);
        }
        PlanAct::Clear => st.apply.list.clear(),
        PlanAct::SetRepo => {
            let idx = st.apply.list.selected();
            let start = st.apply.src_root.trim().to_string();
            if let Some(dir) = pick_folder("이 번들을 적용할 git 저장소 폴더", &start) {
                if let Err(e) = st.apply.list.set_repo(&idx, &dir) {
                    st.notice = e;
                }
                // 엔진이 repoMap 을 디스크에 저장했으니 화면 설정도 맞춘다(안 맞추면 다음 설정 저장이 덮어쓴다).
                if let Ok(v) = gitbridge_core::store::load_settings() {
                    st.settings = v;
                }
            }
        }
        PlanAct::NewRepo => {
            let idx = st.apply.list.selected();
            let start = st.apply.src_root.trim().to_string();
            if let Some(parent) = pick_folder("새 저장소를 둘 상위 폴더", &start) {
                match st.apply.list.set_new_repo_parent(&idx, &parent) {
                    Ok(why) => {
                        if !why.is_empty() {
                            st.notice = format!("새 저장소로 만들지 못한 줄이 있습니다: {}", why.join(" / "));
                        }
                    }
                    Err(e) => st.notice = e,
                }
            }
        }
        PlanAct::Remove => {
            let idx = st.apply.list.selected();
            st.apply.list.remove(&idx);
        }
        PlanAct::Up => st.apply.list.move_selected(-1),
        PlanAct::Down => st.apply.list.move_selected(1),
    }
    match act {
        Act::None => {}
        Act::Apply => start_apply(st),
        Act::Preview => start_preview(st),
        Act::Undo => {
            let repo = st.apply.repo.trim().to_string();
            super::undo_view::open(st, &repo);
        }
    }
}

/// 확인 창과 실행 시점의 대상을 비교하려고 실행할 줄마다 (번들, 저장소, 전체 동기화)를 뽑는다.
fn run_targets(st: &AppState, idx: &[usize]) -> Vec<(String, String, bool)> {
    let Some(plan) = st.apply.list.plan.as_ref() else {
        return Vec::new();
    };
    idx.iter().filter_map(|&i| plan.rows.get(i)).map(|r| (r.path.clone(), r.repo.clone(), r.sync_all)).collect()
}

/// plan 을 &mut 로 만지는 오래 걸리는 일을 작업 스레드에서 돌린다.
/// 도는 동안 plan 은 작업 스레드가 들고 있고(목록은 '불러오는 중'), 끝나면 화면 스레드에서 plan 을 되돌려 놓은 다음 after 를 부른다.
fn spawn_plan<W, A>(st: &mut AppState, label: &str, work: W)
where
    W: FnOnce(&mut QueuePlan, &Progress) -> A + Send + 'static,
    A: FnOnce(&mut AppState) + Send + 'static,
{
    if st.busy() {
        st.notice = "다른 작업이 아직 진행 중입니다. 끝난 뒤 다시 눌러 주세요.".into();
        return;
    }
    let Some(mut plan) = st.apply.list.take() else {
        return;
    };
    st.spawn(label, move |p: &Progress| -> Apply {
        let after = work(&mut plan, p);
        Box::new(move |st: &mut AppState| {
            st.apply.list.put(plan);
            after(st);
        })
    });
}

/// 고른 번들·ZIP·폴더를 줄 목록에 더한다(번들을 읽고 저장소를 짝짓는 건 작업 스레드에서).
fn start_add(st: &mut AppState, paths: Vec<String>) {
    if paths.is_empty() {
        return;
    }
    let paths: Vec<String> = paths.into_iter().filter(|p| !st.apply.list.has_source(p)).collect();
    if paths.is_empty() {
        st.notice = "이미 목록에 있는 파일입니다.".into();
        return;
    }
    let root = st.apply.src_root.trim().to_string();
    let root_after = root.clone();
    spawn_plan(st, "번들 읽기", move |plan, p| {
        p.set("번들·ZIP 을 읽고 저장소를 찾는 중");
        let r = queue::plan_add(plan, &paths, &root);
        move |st: &mut AppState| {
            st.apply.matched_root = root_after;
            if let Err(e) = r {
                st.notice = e;
            }
        }
    });
}

/// 저장소 모음 폴더가 바뀌었을 때: 다른 작업이 돌면 끝난 뒤로 미루고, 아니면 바로 다시 찾는다.
fn request_rematch(st: &mut AppState) {
    let root = st.apply.src_root.trim().to_string();
    if root == st.apply.matched_root {
        return;
    }
    if st.busy() {
        st.apply.rematch_pending = true;
        return;
    }
    start_rematch(st);
}

/// 저장소가 비어 있는 줄만 다시 짝짓는다.
fn start_rematch(st: &mut AppState) {
    let root = st.apply.src_root.trim().to_string();
    st.apply.matched_root = root.clone();
    st.apply.rematch_pending = false;
    if st.apply.list.row_count() == 0 {
        return;
    }
    spawn_plan(st, "저장소 다시 찾기", move |plan, p| {
        p.set("저장소 모음 폴더에서 저장소를 다시 찾는 중");
        let r = queue::plan_rematch(plan, &root);
        move |st: &mut AppState| {
            if let Err(e) = r {
                st.notice = e;
            }
        }
    });
}

/// 적용 뒤 설정을 다시 읽는다(큐가 repoMap·repo 를 설정에 저장했을 수 있다).
fn reload_after_run(st: &mut AppState) {
    if let Ok(v) = gitbridge_core::store::load_settings() {
        st.settings = v;
    }
    // 도는 동안 사용자가 바꾼 설정은 엔진의 덮어쓰기 뒤에도 남긴다(1.0.5 는 같은 설정 객체를 같이 써서 사용자 변경이 남았다).
    for (k, v) in std::mem::take(&mut st.edits_during_job) {
        st.save_setting(&k, v);
    }
    let r = setting_str(&st.settings, "repo");
    if !r.is_empty() {
        st.apply.repo = r;
    }
}

/// 줄 목록에서 체크하고 저장소가 있는 줄만 plan_run 으로 적용(apply=true)하거나 계획만 낸다(apply=false).
/// 기록은 엔진이 남기므로 여기서 다시 남기지 않는다. 중단을 누르면 엔진이 남은 줄을 보류로 돌려준다.
fn start_plan_run(st: &mut AppState, apply: bool) {
    run_plan_checked(st, apply, false);
}

/// 실행 전 검사. 전체 동기화 줄 가운데 조건이 안 맞는 것이 섞이면 막고 안내한다.
/// 실제 적용(apply)에 전체 동기화 줄이 있으면 확인 대화를 먼저 띄운다(confirmed 가 참이면 이미 확인한 것).
fn run_plan_checked(st: &mut AppState, apply: bool, confirmed: bool) {
    let idx = st.apply.list.run_indices();
    if idx.is_empty() {
        st.notice = "적용할 번들이 없습니다. 체크하고 저장소가 있는 번들이 하나 이상 있어야 합니다.".into();
        return;
    }
    let (n_sync, bad) = st.apply.list.sync_counts(&idx);
    if bad > 0 {
        st.notice = "전체 동기화 대상의 저장소와 번들 범위를 다시 확인해 주세요. 전체 동기화는 기존 저장소에 적용할 '모든 서버 브랜치 + 태그' 번들만 가능합니다.".into();
        return;
    }
    if apply && n_sync > 0 && !confirmed {
        let targets = run_targets(st, &idx);
        st.apply.sync_confirm = Some((n_sync, idx, targets));
        return;
    }
    launch_plan_run(st, apply, idx);
}

/// 적용·미리보기는 줄 사본으로 돌린다. 목록(plan)은 화면에 그대로 두어 '저장소를 찾는 중'으로 바뀌지 않게 하고,
/// 도는 동안 목록 편집은 busy 로 잠근다(풀어 둔 ZIP 임시 폴더도 plan 이 계속 쥐고 있다).
fn launch_plan_run(st: &mut AppState, apply: bool, idx: Vec<usize>) {
    if st.busy() {
        st.notice = "다른 작업이 아직 진행 중입니다. 끝난 뒤 다시 눌러 주세요.".into();
        return;
    }
    let Some(rows) = st.apply.list.plan.as_ref().map(|p| p.rows.clone()) else {
        return;
    };
    st.spawn(if apply { "번들 목록 적용" } else { "번들 목록 미리보기" }, move |p: &Progress| -> Apply {
        p.set(if apply { "번들마다 저장소에 적용하는 중" } else { "적용 계획을 만드는 중 (저장소는 바꾸지 않습니다)" });
        let outcomes = queue::plan_run(&rows, &idx, apply, p.cancel_flag());
        Box::new(move |st: &mut AppState| {
            let mut outcomes = outcomes;
            if apply {
                reload_after_run(st);
                st.apply.list.apply_results(&outcomes);
                st.apply.list.show_names(&mut outcomes);
                st.apply.outcomes = outcomes;
            } else {
                st.apply.list.show_names(&mut outcomes);
                st.apply.preview_outcomes = outcomes;
                st.apply.preview_title = "번들 목록 계획 — 아직 아무것도 적용하지 않았습니다.".into();
            }
        })
    });
}

/// 적용. 폴더 큐도 파일 목록(plan)도 엔진이 기록까지 한다.
fn start_apply(st: &mut AppState) {
    if !st.apply.mode_dir {
        start_plan_run(st, true);
        return;
    }
    let dir = st.apply.bundle_dir.trim().to_string();
    let root = st.apply.src_root.trim().to_string();
    if dir.is_empty() {
        st.notice = "번들 폴더를 먼저 골라 주세요.".into();
        return;
    }
    st.save_setting("bundleDir", json!(dir.clone()));
    st.save_setting("srcRoot", json!(root.clone()));
    st.spawn("번들 폴더 적용", move |p: &Progress| -> Apply {
        p.set("번들 폴더를 읽고 적용하는 중");
        // 큐는 엔진이 기록 파일에 남기므로 여기서 다시 남기지 않는다.
        // 중단을 누르면 엔진이 아직 적용하지 않은 번들을 '취소로 보류'로 남긴다.
        // 폴더 모드는 폴더가 없을 때 멈춤 안내를 내는 기존 경로를 그대로 쓴다.
        let outcomes = gitbridge_core::queue::run_queue_cancel(&dir, &root, true, p.cancel_flag());
        Box::new(move |st: &mut AppState| {
            reload_after_run(st);
            st.apply.outcomes = outcomes;
        })
    });
}

/// 미리보기. 읽기만 한다.
fn start_preview(st: &mut AppState) {
    if !st.apply.mode_dir {
        start_plan_run(st, false);
        return;
    }
    let dir = st.apply.bundle_dir.trim().to_string();
    let root = st.apply.src_root.trim().to_string();
    if dir.is_empty() {
        st.notice = "번들 폴더를 먼저 골라 주세요.".into();
        return;
    }
    st.spawn("번들 폴더 미리보기", move |p: &Progress| -> Apply {
        p.set("번들 폴더를 읽는 중 (저장소는 바꾸지 않습니다)");
        let outcomes = gitbridge_core::queue::run_queue_cancel(&dir, &root, false, p.cancel_flag());
        Box::new(move |st: &mut AppState| {
            st.apply.preview_outcomes = outcomes;
            st.apply.preview_title = "폴더 큐 계획 — 아직 아무것도 적용하지 않았습니다.".into();
        })
    });
}
