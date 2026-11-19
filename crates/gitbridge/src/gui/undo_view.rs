//! 되돌리기 창: undo 기록 목록 -> 미리보기 -> 확인 한 번 -> 복원.

use super::worker::{Apply, Progress};
use super::AppState;
use super::theme::Bold;
use eframe::egui::{self, RichText};
use gitbridge_core::gitrun::Git;
use gitbridge_core::refs::is_zero;
use gitbridge_core::undo;

#[derive(Default)]
pub struct UndoState {
    pub open: bool,
    pub repo: String,
    /// (기록 이름, 항목 수) 최근 먼저.
    pub stamps: Vec<(String, usize)>,
    /// 고른 기록 이름.
    pub selected: Option<String>,
    /// 고른 기록의 미리보기 항목.
    pub entries: Vec<undo::Entry>,
    /// 미리보기를 읽은 저장소와 그때 기록의 journal OID. 실행은 이 둘이 지금과 같을 때만 한다.
    pub bound_repo: String,
    pub bound_oid: String,
    /// "이 기록으로 되돌리기"를 한 번 눌러 확인 단계에 있는지.
    pub confirm: bool,
    /// 마지막 결과·실패 문구.
    pub result: String,
    /// 결과가 실패인지(색 구분용).
    pub result_err: bool,
}

/// 저장소 repo 의 되돌리기 창을 연다(다른 탭·기록 창에서 부른다).
pub fn open(st: &mut AppState, repo: &str) {
    st.undo.open = true;
    st.undo.repo = repo.to_string();
    st.undo.stamps.clear();
    clear_pick(st);
    st.undo.result.clear();
    st.undo.result_err = false;
    read_list(st);
}

/// 고른 기록·미리보기·확인 단계를 푼다.
fn clear_pick(st: &mut AppState) {
    st.undo.selected = None;
    st.undo.entries.clear();
    st.undo.confirm = false;
    st.undo.bound_repo.clear();
    st.undo.bound_oid.clear();
}

/// 기록 stamp 의 journal 참조가 지금 가리키는 OID.
fn journal_oid(git: &Git, stamp: &str) -> Result<String, String> {
    let r = git.q(&["rev-parse", "--verify", "--quiet", &format!("{}{}/journal", undo::UNDO_ROOT, stamp)])?;
    let oid = r.out.trim();
    if !r.ok() || oid.is_empty() {
        return Err(format!("되돌리기 기록 {} 을 찾지 못했습니다.", stamp));
    }
    Ok(oid.to_string())
}

/// 미리보기가 지금 저장소 칸의 경로로 읽은 것인지.
fn bound_here(st: &AppState) -> bool {
    !st.undo.bound_oid.is_empty() && st.undo.bound_repo == st.undo.repo.trim()
}

/// 목록 읽기 작업을 띄운다.
fn read_list(st: &mut AppState) {
    let repo = st.undo.repo.trim().to_string();
    if repo.is_empty() {
        st.undo.result = "저장소 경로를 입력해 주세요.".into();
        st.undo.result_err = true;
        return;
    }
    st.spawn("되돌리기 목록 읽는 중", move |_p: &Progress| -> Apply {
        let res = Git::open(&repo).and_then(|git| undo::list(&git));
        Box::new(move |st: &mut AppState| {
            // 그 사이 저장소 경로를 바꿨으면 버린다.
            if st.undo.repo.trim() != repo {
                return;
            }
            match res {
                Ok(list) => {
                    // 고른 기록이 목록에서 사라졌으면 선택을 풀어 준다.
                    if let Some(sel) = &st.undo.selected {
                        if !list.iter().any(|(s, _)| s == sel) {
                            clear_pick(st);
                        }
                    }
                    st.undo.stamps = list;
                }
                Err(e) => {
                    st.undo.stamps.clear();
                    st.undo.result = format!("목록을 읽지 못했습니다: {}", e);
                    st.undo.result_err = true;
                }
            }
        })
    });
}

/// 기록 하나의 항목 읽기 작업을 띄운다.
fn read_entries(st: &mut AppState, stamp: &str) {
    let repo = st.undo.repo.trim().to_string();
    let stamp = stamp.to_string();
    st.spawn("되돌리기 미리보기 읽는 중", move |_p: &Progress| -> Apply {
        let res = Git::open(&repo).and_then(|git| {
            let oid = journal_oid(&git, &stamp)?;
            Ok((oid, undo::entries(&git, &stamp)?))
        });
        Box::new(move |st: &mut AppState| {
            // 그 사이 다른 기록을 골랐거나 저장소 경로를 바꿨으면 버린다.
            if st.undo.selected.as_deref() != Some(stamp.as_str()) || st.undo.repo.trim() != repo {
                return;
            }
            match res {
                Ok((oid, v)) => {
                    st.undo.entries = v;
                    st.undo.bound_repo = repo;
                    st.undo.bound_oid = oid;
                }
                Err(e) => {
                    st.undo.entries.clear();
                    st.undo.result = format!("미리보기를 읽지 못했습니다: {}", e);
                    st.undo.result_err = true;
                }
            }
        })
    });
}

/// 복원 실행 작업을 띄운다. 미리보기한 저장소·기록(journal OID)과 실행 직전 값이 같을 때만 되돌린다.
/// 끝나면 결과 문구를 보이고 목록을 다시 읽는다.
fn run_undo(st: &mut AppState, stamp: &str) {
    if !bound_here(st) {
        st.undo.confirm = false;
        st.undo.result = "미리보기한 저장소와 지금 저장소가 다릅니다. 기록을 다시 골라 미리보기를 확인해 주세요.".into();
        st.undo.result_err = true;
        return;
    }
    let repo = st.undo.bound_repo.clone();
    let want = st.undo.bound_oid.clone();
    let stamp = stamp.to_string();
    st.spawn("되돌리는 중", move |_p: &Progress| -> Apply {
        let res = Git::open(&repo).and_then(|git| {
            if journal_oid(&git, &stamp)? != want {
                return Err("미리보기 뒤로 이 기록이 바뀌었습니다. 목록을 다시 읽고 미리보기를 다시 확인해 주세요.".to_string());
            }
            undo::run(&git, &stamp)
        });
        Box::new(move |st: &mut AppState| {
            match res {
                Ok(msg) => {
                    st.undo.result = msg;
                    st.undo.result_err = false;
                }
                Err(e) => {
                    st.undo.result = format!("되돌리지 못했습니다: {}", e);
                    st.undo.result_err = true;
                }
            }
            clear_pick(st);
            read_list(st);
        })
    });
}

/// 8자로 줄인 값. zero 는 "없음".
fn short(oid: &str) -> String {
    if is_zero(oid) {
        "없음".to_string()
    } else {
        oid.chars().take(8).collect()
    }
}

/// 열려 있으면 창을 그린다(매 프레임).
pub fn window(st: &mut AppState, ctx: &egui::Context) {
    if !st.undo.open {
        return;
    }
    let mut open = true;
    let mut do_list = false;
    let mut pick: Option<String> = None;
    let mut ask = false;
    let mut do_run = false;
    let mut cancel_confirm = false;
    let mut repo_changed = false;

    egui::Window::new("되돌리기").open(&mut open).default_width(560.0).show(ctx, |ui| {
        ui.label("앞서 바뀐 브랜치·태그를 기록에서 골라 이전 값으로 복원합니다. 그 뒤로 또 바뀌었으면 되돌리지 않습니다.");
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("저장소");
            if ui.add(egui::TextEdit::singleline(&mut st.undo.repo).desired_width(380.0)).changed() {
                repo_changed = true;
            }
            if ui.add_enabled(!st.busy(), egui::Button::new("목록 읽기")).clicked() {
                do_list = true;
            }
        });
        ui.separator();

        if st.undo.stamps.is_empty() {
            ui.label(RichText::new("되돌리기 기록이 없습니다(또는 아직 읽지 않았습니다).").weak());
        } else {
            ui.label(RichText::new("되돌리기 기록(최근 먼저)").bold());
            egui::ScrollArea::vertical().id_salt("undo_list").max_height(130.0).show(ui, |ui| {
                for (stamp, n) in &st.undo.stamps {
                    let on = st.undo.selected.as_deref() == Some(stamp.as_str());
                    if ui.selectable_label(on, format!("{}  (항목 {}개)", stamp, n)).clicked() && !on {
                        pick = Some(stamp.clone());
                    }
                }
            });
        }

        if let Some(sel) = st.undo.selected.clone() {
            ui.separator();
            ui.label(RichText::new(format!("미리보기: {}", sel)).bold());
            if st.undo.entries.is_empty() {
                ui.label(RichText::new("항목을 읽는 중이거나 비어 있습니다.").weak());
            } else {
                egui::ScrollArea::vertical().id_salt("undo_entries").max_height(180.0).show(ui, |ui| {
                    egui::Grid::new("undo_grid").striped(true).num_columns(3).show(ui, |ui| {
                        ui.label(RichText::new("브랜치·태그").bold());
                        ui.label(RichText::new("돌아갈 값(old)").bold());
                        ui.label(RichText::new("바뀐 값(new)").bold());
                        ui.end_row();
                        for e in &st.undo.entries {
                            ui.label(RichText::new(&e.ref_).monospace());
                            ui.label(RichText::new(short(&e.old)).monospace());
                            ui.label(RichText::new(short(&e.new)).monospace());
                            ui.end_row();
                        }
                    });
                });
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if !st.undo.confirm {
                    if ui.add_enabled(!st.busy() && !st.undo.entries.is_empty() && bound_here(st), egui::Button::new("이 기록으로 되돌리기")).clicked() {
                        ask = true;
                    }
                } else {
                    ui.label(RichText::new("정말 되돌릴까요? 브랜치·태그가 이전 값으로 돌아갑니다.").color(super::theme::ERR));
                    if ui.add_enabled(!st.busy() && bound_here(st), egui::Button::new("정말 되돌리기")).clicked() {
                        do_run = true;
                    }
                    if ui.button("취소").clicked() {
                        cancel_confirm = true;
                    }
                }
            });
        }

        if !st.undo.result.is_empty() {
            ui.separator();
            let color = if st.undo.result_err { egui::Color32::from_rgb(0xc6, 0x28, 0x28) } else { super::theme::OK };
            ui.label(RichText::new(&st.undo.result).color(color));
        }
    });

    st.undo.open = open;

    // 저장소 경로가 바뀌면 앞 저장소의 목록·선택·확인을 모두 푼다.
    if repo_changed {
        st.undo.stamps.clear();
        clear_pick(st);
        st.undo.result.clear();
    }

    if do_list {
        st.undo.result.clear();
        read_list(st);
    }
    if let Some(s) = pick.filter(|_| !st.busy()) {
        clear_pick(st);
        st.undo.selected = Some(s.clone());
        st.undo.result.clear();
        read_entries(st, &s);
    }
    if ask {
        st.undo.confirm = true;
    }
    if cancel_confirm {
        st.undo.confirm = false;
    }
    if do_run {
        if let Some(s) = st.undo.selected.clone() {
            st.undo.result.clear();
            run_undo(st, &s);
        }
    }
}
