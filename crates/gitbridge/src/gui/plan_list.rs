//! 번들 적용 탭의 '번들 → 저장소' 줄 목록(1.0.5 처럼 번들마다 저장소를 바로 보여 준다).
//! 줄 자료는 엔진의 QueuePlan 이 갖고, 이 파일은 화면 쪽 상태(체크·선택·실행 결과)와 그리기만 맡는다.

use super::common::{kind_label, outcome_status, Status};
use super::theme::Bold;
use super::worker::Live;
use super::{theme, widgets};
use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke};
use gitbridge_core::queue::{self, PlanRow, QueuePlan};
use gitbridge_core::refs::Outcome;
use std::collections::HashMap;

/// 체크 칸 폭.
const CHK_W: f32 = 28.0;

/// 적용 실행 중 이 줄이 어디까지 왔는가.
#[derive(Clone, Copy, Default, PartialEq)]
enum RunState {
    /// 실행 대상이 아니거나 이미 끝났다.
    #[default]
    Idle,
    /// 이번 실행 대상이고 차례를 기다린다.
    Queued,
    /// 지금 적용하는 중이다.
    Running,
}

/// 칼럼 폭 기본 비율(번들, 저장소). 나머지는 상태 칸이다.
const COL_DEFAULT: [f32; 2] = [0.38, 0.24];
/// 칼럼 최소 폭.
const COL_MIN_W: f32 = 70.0;
/// 지금 적용 중인 줄의 배경.
const RUN_BG: Color32 = Color32::from_rgb(0xF4, 0xF1, 0xDD);

/// 줄 하나의 화면 쪽 상태.
#[derive(Clone, Default)]
struct RowUi {
    /// 적용 실행 중 진행 상태.
    run: RunState,
    /// 적용 체크.
    checked: bool,
    /// 줄 선택(저장소 바꾸기 등의 대상).
    selected: bool,
    /// 마지막으로 본 저장소 값. 바뀌면 체크·결과를 다시 정한다.
    repo: String,
    /// 실행 결과.
    result: Option<RowResult>,
    /// 이 화면 상태가 속한 줄의 번들 경로. 엔진이 줄 순서를 바꿔도(연속 번들 정렬·위로/아래로) 상태가 줄을 따라가게 한다.
    path: String,
}

#[derive(Clone)]
struct RowResult {
    /// 상태 칸에 보일 짧은 문구(완료는 종류, 그 밖은 '종류 — 첫 줄').
    text: String,
    /// 마우스를 올렸을 때 보일 전체 문구.
    full: String,
    status: Status,
}

/// 줄 목록 전체. plan 은 오래 걸리는 일 동안 작업 스레드로 옮겨 가 있어서(loading) 그동안은 None 이다.
#[derive(Default)]
pub struct PlanList {
    pub plan: Option<QueuePlan>,
    rows: Vec<RowUi>,
    loading: bool,
    /// 번들·저장소 칼럼 폭 비율(끌어서 바꾼 값). 없으면 기본값.
    col: Option<[f32; 2]>,
    /// 적용 실행 중 이번 실행 줄 수, 끝난 줄 수, 멈춘 줄 수.
    run_total: usize,
    run_done: usize,
    run_stop: usize,
    /// 방금 적용을 시작한 줄(표가 그 줄로 스크롤한다).
    follow: Option<usize>,
    /// 이번 실행 중 사용자가 목록을 직접 스크롤했다(그러면 더는 적용 중인 줄로 끌고 가지 않는다).
    user_scrolled: bool,
    /// 실행이 끝나 결과 칸이 커지면 따라가기로 밀려난 스크롤을 맨 위로 되돌린다(사용자가 직접 스크롤했으면 두지 않는다).
    reset_scroll: bool,
}

/// 실행 결과 하나를 줄 상태 칸에 보일 모양으로 만든다.
/// 칸에는 짧게(완료는 종류만, 그 밖은 종류와 첫 줄) 보이고 전체 문구는 마우스를 올리면 나온다.
fn result_of(o: &Outcome) -> RowResult {
    let status = outcome_status(o);
    let kind = kind_label(&o.kind);
    let first = o.message.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("판정한 저장소 폴더:")).unwrap_or("");
    let text = if status == Status::Done || first.is_empty() { kind.clone() } else { format!("{} — {}", kind, first) };
    RowResult { text, full: format!("[{}] {}", kind, o.message), status }
}

/// 처음부터 체크해 둘 줄인가: 저장소가 있고, 엔진이 체크로 정했고, 전에 적용한 적이 없을 때.
fn default_checked(r: &PlanRow) -> bool {
    !r.repo.is_empty() && r.checked && r.seen.is_empty()
}

/// 경로의 마지막 폴더 이름(목록 저장소 칸과 적용 중 진행 줄에 쓴다).
pub(super) fn repo_name(p: &str) -> String {
    let t = p.trim_end_matches(['\\', '/']);
    t.rsplit(['\\', '/']).next().filter(|s| !s.is_empty()).unwrap_or(p).to_string()
}

fn kind_text(kind: &str) -> &str {
    match kind {
        "base" => "기준 브랜치 번들",
        "repository" => "저장소 전체 번들",
        "feature" => "Feature 번들",
        other => other,
    }
}

impl PlanList {
    /// 매 프레임 처음에 부른다. plan 이 없고 작업 스레드에도 없으면(작업이 비정상으로 끝남) 새로 만든다.
    /// 참이면 목록이 비정상으로 사라져 비웠다는 뜻.
    pub fn ensure(&mut self, busy: bool) -> bool {
        if !busy {
            self.end_run();
        }
        if self.plan.is_none() && !(self.loading && busy) {
            let lost = self.loading;
            self.plan = Some(QueuePlan::new());
            self.rows.clear();
            self.loading = false;
            return lost;
        }
        self.sync();
        false
    }

    /// 적용을 시작한다: 이번에 돌 줄은 이전 결과를 지우고 '순서 대기'로 둔다.
    pub fn begin_run(&mut self, idx: &[usize]) {
        self.sync();
        for u in self.rows.iter_mut() {
            u.run = RunState::Idle;
        }
        for &i in idx {
            if let Some(u) = self.rows.get_mut(i) {
                u.run = RunState::Queued;
                u.result = None;
            }
        }
        self.run_total = idx.len();
        self.run_done = 0;
        self.run_stop = 0;
        self.follow = None;
        self.user_scrolled = false;
    }

    /// 실행이 끝났거나 멈췄을 때 진행 표시를 걷는다(작업이 비정상으로 끝나도 '적용 중'이 남지 않게 매 프레임 확인).
    pub fn end_run(&mut self) {
        if self.run_total == 0 && self.follow.is_none() && self.rows.iter().all(|u| u.run == RunState::Idle) {
            return;
        }
        for u in self.rows.iter_mut() {
            u.run = RunState::Idle;
        }
        self.run_total = 0;
        self.run_done = 0;
        self.run_stop = 0;
        self.follow = None;
        self.reset_scroll = !self.user_scrolled;
    }

    /// 작업 스레드가 올린 줄 단위 소식을 줄 상태에 바로 반영한다(매 프레임).
    pub fn pump(&mut self, events: Vec<Live>) {
        for ev in events {
            match ev {
                Live::Start { file } => {
                    if let Some(i) = self.rows.iter().position(|u| u.path == file) {
                        self.rows[i].run = RunState::Running;
                        if !self.user_scrolled {
                            self.follow = Some(i);
                        }
                    }
                }
                Live::Done { outcome, retry } => {
                    let Some(file) = outcome.file.as_deref() else { continue };
                    let Some(i) = self.rows.iter().position(|u| u.path == file) else { continue };
                    let u = &mut self.rows[i];
                    if retry {
                        // 앞 번들이 들어간 뒤 한 번 더 시도한다. 지금은 결과를 보이지 않고 차례를 기다린다.
                        u.run = RunState::Queued;
                        u.result = None;
                        continue;
                    }
                    let res = result_of(&outcome);
                    if res.status == Status::Done {
                        u.checked = false;
                    } else if res.status == Status::Stop {
                        self.run_stop += 1;
                    }
                    u.result = Some(res);
                    u.run = RunState::Idle;
                    self.run_done += 1;
                }
            }
        }
    }

    /// 적용 중이면 목록 위에 보일 한 줄(완료한 줄 수 / 이번 실행 줄 수). 아래 진행 줄에는 숫자 없이 지금 줄만 보인다.
    pub fn run_text(&self) -> Option<String> {
        if self.run_total == 0 {
            return None;
        }
        let mut t = format!("적용 중 · 완료 {}/{}", self.run_done, self.run_total);
        if self.run_stop > 0 {
            t += &format!(" · 멈춤 {}개", self.run_stop);
        }
        Some(t)
    }

    /// 작업 스레드로 plan 을 옮기려고 꺼낸다. 돌려놓을 때까지 loading.
    pub fn take(&mut self) -> Option<QueuePlan> {
        // 작업 스레드가 줄 순서를 바꿔도(연속 번들 정렬) 체크 상태가 경로로 따라가므로, 여기서는 현재 체크를 PlanRow 에도 남겨 둔다.
        if let Some(plan) = self.plan.as_mut() {
            for (r, u) in plan.rows.iter_mut().zip(self.rows.iter()) {
                r.checked = u.checked;
            }
        }
        let p = self.plan.take();
        if p.is_some() {
            self.loading = true;
        }
        p
    }

    /// 작업이 끝나 plan 을 돌려놓는다.
    pub fn put(&mut self, plan: QueuePlan) {
        self.plan = Some(plan);
        self.loading = false;
        self.sync();
    }

    pub fn is_loading(&self) -> bool {
        self.plan.is_none()
    }

    /// 목록을 비운다(옛 plan 이 버려지며 임시 폴더도 치워진다).
    pub fn clear(&mut self) {
        self.plan = Some(QueuePlan::new());
        self.rows.clear();
        self.loading = false;
    }

    pub fn row_count(&self) -> usize {
        self.plan.as_ref().map_or(0, |p| p.rows.len())
    }

    /// 그 경로(또는 그 경로의 ZIP)에서 온 줄이 이미 있는가.
    pub fn has_source(&self, p: &str) -> bool {
        self.plan.as_ref().is_some_and(|pl| pl.rows.iter().any(|r| r.path == p || r.archive == p))
    }

    /// 화면 쪽 줄 상태를 plan 의 줄에 맞춘다(새 줄 추가, 저장소가 바뀐 줄 갱신).
    fn sync(&mut self) {
        let Some(plan) = self.plan.as_ref() else { return };
        // 엔진이 줄 순서를 바꿨으면(추가·저장소 바꾸기 때 연속 번들 정렬) 경로로 줄을 다시 찾아 체크·선택·결과가 밀리지 않게 한다.
        let aligned = self.rows.iter().zip(plan.rows.iter()).all(|(u, r)| u.path == r.path);
        if !aligned {
            let mut old: HashMap<String, RowUi> = self.rows.drain(..).map(|u| (u.path.clone(), u)).collect();
            for r in plan.rows.iter() {
                if let Some(u) = old.remove(&r.path) {
                    self.rows.push(u);
                } else {
                    // 옛 상태가 없는 새 줄.
                    self.rows.push(RowUi { checked: default_checked(r), selected: false, repo: r.repo.clone(), result: None, path: r.path.clone(), run: RunState::Idle });
                }
            }
        }
        self.rows.truncate(plan.rows.len());
        for (i, r) in plan.rows.iter().enumerate() {
            if i >= self.rows.len() {
                self.rows.push(RowUi { checked: default_checked(r), selected: false, repo: r.repo.clone(), result: None, path: r.path.clone(), run: RunState::Idle });
            } else {
                let u = &mut self.rows[i];
                if u.repo != r.repo {
                    // 저장소가 없던 줄에 저장소가 생기면 바로 적용할 수 있게 체크한다(전에 적용한 줄은 그대로 꺼 둔다).
                    if u.repo.is_empty() && !r.repo.is_empty() {
                        u.checked = r.seen.is_empty();
                    }
                    if r.repo.is_empty() {
                        u.checked = false;
                    }
                    u.repo = r.repo.clone();
                    u.result = None;
                }
            }
        }
    }

    /// (번들 수, 적용할 것 = 체크됨 && 저장소 있음, 저장소 없는 것).
    pub fn counts(&self) -> (usize, usize, usize) {
        let Some(plan) = self.plan.as_ref() else { return (0, 0, 0) };
        let total = plan.rows.len();
        let k = plan.rows.iter().zip(self.rows.iter()).filter(|(r, u)| u.checked && !r.repo.is_empty()).count();
        let m = plan.rows.iter().filter(|r| r.repo.is_empty()).count();
        (total, k, m)
    }

    /// 목록 위 한 줄 요약.
    pub fn summary_text(&self) -> String {
        let (total, k, m) = self.counts();
        format!("번들 {}개 · 적용할 것 {}개 · 저장소 없는 것 {}개(적용 안 함)", total, k, m)
    }

    /// 선택한 줄 번호(오름차순).
    pub fn selected(&self) -> Vec<usize> {
        self.rows.iter().enumerate().filter(|(_, u)| u.selected).map(|(i, _)| i).collect()
    }

    /// 실행할 줄 번호: 체크됨 && 저장소 있음.
    pub fn run_indices(&self) -> Vec<usize> {
        let Some(plan) = self.plan.as_ref() else { return Vec::new() };
        plan.rows.iter().zip(self.rows.iter()).enumerate().filter(|(_, (r, u))| u.checked && !r.repo.is_empty()).map(|(i, _)| i).collect()
    }

    /// 줄을 정확히 하나만 골랐을 때 그 줄 번호.
    pub fn selected_one(&self) -> Option<usize> {
        let s = self.selected();
        if s.len() == 1 {
            Some(s[0])
        } else {
            None
        }
    }

    /// i 번 줄에 전체 동기화를 켤 수 있는가(plan_can_sync).
    pub fn can_sync(&self, i: usize) -> bool {
        self.plan.as_ref().and_then(|p| p.rows.get(i)).is_some_and(queue::plan_can_sync)
    }

    /// i 번 줄이 전체 동기화로 정해져 있는가.
    pub fn is_sync(&self, i: usize) -> bool {
        self.plan.as_ref().and_then(|p| p.rows.get(i)).is_some_and(|r| r.sync_all)
    }

    /// i 번 줄의 전체 동기화를 켜거나 끈다(조건이 안 되면 꺼진다).
    pub fn set_sync(&mut self, i: usize, on: bool) {
        if let Some(plan) = self.plan.as_mut() {
            queue::plan_set_sync(plan, i, on);
        }
    }

    /// 실행할 줄(idx) 가운데 (전체 동기화로 정해진 줄 수, 그중 조건이 안 맞는 줄 수).
    pub fn sync_counts(&self, idx: &[usize]) -> (usize, usize) {
        let Some(plan) = self.plan.as_ref() else { return (0, 0) };
        let mut n = 0;
        let mut bad = 0;
        for &i in idx {
            if let Some(r) = plan.rows.get(i).filter(|r| r.sync_all) {
                n += 1;
                if !queue::plan_can_sync(r) {
                    bad += 1;
                }
            }
        }
        (n, bad)
    }

    /// 고른 한 줄을 delta 칸 옮긴다. 화면 상태(체크·선택·결과)도 같이 옮겨 선택이 따라간다.
    pub fn move_selected(&mut self, delta: i32) {
        let Some(i) = self.selected_one() else { return };
        let Some(plan) = self.plan.as_mut() else { return };
        if let Some(j) = queue::plan_move(plan, i, delta) {
            if i < self.rows.len() && j < self.rows.len() {
                let u = self.rows.remove(i);
                self.rows.insert(j, u);
            }
            self.sync();
        }
    }

    /// 위로/아래로를 눌러도 되는가: (위로, 아래로). 한 줄만 골랐을 때만 가능.
    pub fn can_move(&self) -> (bool, bool) {
        match self.selected_one() {
            Some(i) => (i > 0, i + 1 < self.row_count()),
            None => (false, false),
        }
    }

    /// 고른 줄이 하나이고 그 줄에 결과가 있으면 그 줄의 브랜치 이름(없을 수 있음).
    pub fn selected_result_branch(&self) -> Option<String> {
        let i = self.selected_one()?;
        let u = self.rows.get(i)?;
        u.result.as_ref()?;
        Some(self.plan.as_ref()?.rows.get(i)?.branch.clone())
    }

    /// 고른 줄에 저장소를 지정한다.
    pub fn set_repo(&mut self, idx: &[usize], repo: &str) -> Result<(), String> {
        let Some(plan) = self.plan.as_mut() else { return Err("번들 목록을 읽는 중입니다.".into()) };
        let r = queue::plan_set_repo(plan, idx, repo);
        self.sync();
        r
    }

    /// 고른 줄을 <상위 폴더>/<이름> 새 저장소로 지정한다. 못 한 줄의 이유 목록을 돌려준다.
    pub fn set_new_repo_parent(&mut self, idx: &[usize], parent: &str) -> Result<Vec<String>, String> {
        let Some(plan) = self.plan.as_mut() else { return Err("번들 목록을 읽는 중입니다.".into()) };
        let r = queue::plan_set_new_repo_parent(plan, idx, parent);
        self.sync();
        r
    }

    /// 고른 줄을 목록에서 뺀다.
    pub fn remove(&mut self, idx: &[usize]) {
        let Some(plan) = self.plan.as_mut() else { return };
        let mut sorted = idx.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        queue::plan_remove(plan, &sorted);
        for &i in sorted.iter().rev() {
            if i < self.rows.len() {
                self.rows.remove(i);
            }
        }
        self.sync();
    }

    /// 실행 결과를 줄 상태로 옮긴다. 줄의 결과는 Outcome.file == 줄의 path 로 찾는다. 성공한 줄은 체크를 뺀다.
    pub fn apply_results(&mut self, outcomes: &[Outcome]) {
        self.sync();
        let Some(plan) = self.plan.as_ref() else { return };
        for (i, r) in plan.rows.iter().enumerate() {
            let Some(u) = self.rows.get_mut(i) else { break };
            let mine: Vec<&Outcome> = outcomes.iter().filter(|o| o.file.as_deref() == Some(r.path.as_str())).collect();
            // 한 줄에 결과가 여럿이면 실패를 먼저, 없으면 마지막 것을 보인다.
            let pick = mine.iter().copied().find(|o| !o.ok).or_else(|| mine.last().copied());
            if let Some(o) = pick {
                let res = result_of(o);
                let status = res.status;
                u.result = Some(res);
                if status == Status::Done {
                    u.checked = false;
                }
            }
        }
    }

    /// 결과의 '파일'(임시 폴더 경로)을 줄의 표시 이름("ZIP 전체 경로 > 번들 이름")으로 바꾼다.
    /// 줄 결과 매핑(apply_results)을 한 뒤에 부른다.
    pub fn show_names(&self, outcomes: &mut [Outcome]) {
        let Some(plan) = self.plan.as_ref() else { return };
        for o in outcomes.iter_mut() {
            if let Some(r) = plan.rows.iter().find(|r| !r.archive.is_empty() && o.file.as_deref() == Some(r.path.as_str())) {
                o.file = Some(queue::shown_bundle_name(&r.archive, &r.path));
            }
        }
    }

    /// 엔진이 남긴 멈춤 결과(읽기 실패 등)를 목록 위에 짧게 보인다.
    fn notes_box(&mut self, ui: &mut egui::Ui) {
        let Some(plan) = self.plan.as_mut() else { return };
        if plan.notes.is_empty() {
            return;
        }
        let mut clear = false;
        widgets::notice_box(ui, theme::ERR_BG, theme::ERR, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("확인이 필요한 항목 {}개", plan.notes.len())).bold().color(theme::ERR));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::ghost_w(ui, "지우기", true, 56.0).clicked() {
                        clear = true;
                    }
                });
            });
            for o in plan.notes.iter().take(3) {
                let first = o.message.lines().next().unwrap_or("");
                ui.add(egui::Label::new(RichText::new(first).size(12.0)).truncate()).on_hover_text(o.message.as_str());
            }
            if plan.notes.len() > 3 {
                widgets::small(ui, &format!("외 {}개", plan.notes.len() - 3));
            }
        });
        if clear {
            plan.notes.clear();
        }
        ui.add_space(widgets::GAP_S);
    }

    /// 표(체크 | 번들 | 저장소 | 상태). 남은 높이를 모두 쓴다. 체크·줄 선택은 여기서 바로 바꾼다.
    /// locked 면(적용·확인 중) 체크는 바꾸지 못하고 줄 선택만 된다.
    pub fn table(&mut self, ui: &mut egui::Ui, empty_msg: &str, locked: bool) {
        if self.plan.is_none() {
            widgets::empty_hint(ui, "번들을 읽고 저장소를 찾는 중입니다…");
            return;
        }
        self.notes_box(ui);
        if self.row_count() == 0 {
            widgets::empty_hint(ui, empty_msg);
            return;
        }
        let (total, chk_n, missing) = self.counts();
        let with_repo = total - missing;
        let rem = (ui.available_height() - 10.0).max(60.0);
        let Some(plan) = self.plan.as_ref() else { return };
        let rows_ui = &mut self.rows;
        let follow = self.follow.take();
        let reset_scroll = std::mem::take(&mut self.reset_scroll);
        let running = self.run_total > 0;
        let mut scrolled_now = false;
        let mut col = self.col.unwrap_or(COL_DEFAULT);
        let mut col_changed = false;
        let mut clicked: Option<(usize, bool)> = None;
        let mut toggle_all = false;
        egui::Frame::new().fill(theme::LOG_BG).stroke(Stroke::new(1.0, theme::LINE)).corner_radius(CornerRadius::same(6)).inner_margin(Margin::symmetric(10, 4)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_height(rem);
            ui.spacing_mut().item_spacing.y = 0.0;
            let ctrl = ui.input(|i| i.modifiers.command);
            // 열 위치는 폭에서 정해 머리줄과 줄이 같게 한다.
            let cw = ui.available_width() - ui.spacing().scroll.allocated_width();
            let name_x = 2.0 + CHK_W;
            let rest = (cw - name_x - 2.0 - 16.0).max(120.0);
            // 창이 좁아져 저장된 비율이 상태 칸 최소 폭을 침범하면 폭을 계산하기 전에 보정한다(저장값은 끌 때만 바뀐다).
            let min_f = (COL_MIN_W / rest).min(0.3);
            if col[0] + col[1] > 1.0 - min_f {
                let hi = (1.0 - min_f - min_f).max(min_f);
                col[0] = col[0].clamp(min_f, hi);
                col[1] = col[1].min((1.0 - min_f - col[0]).max(min_f));
            }
            let name_w = rest * col[0];
            let repo_w = rest * col[1];
            let st_w = rest - name_w - repo_w;
            let repo_x = name_x + name_w + 8.0;
            let st_x = repo_x + repo_w + 8.0;
            {
                let head = widgets::row_rect(ui, 26.0);
                let all_box = egui::Rect::from_min_size(egui::pos2(head.left() + 2.0, head.top()), egui::vec2(CHK_W, head.height()));
                if widgets::tri_check(ui, all_box, chk_n, with_repo, with_repo > 0 && !locked) {
                    toggle_all = true;
                }
                for (x, w, name) in [(name_x, name_w, "번들"), (repo_x, repo_w, "저장소"), (st_x, st_w, "상태")] {
                    widgets::cell(ui, head, x, w, |ui| {
                        ui.label(RichText::new(name).size(12.0).color(theme::SUB));
                    });
                }
                // 칼럼 경계를 끌어 폭을 바꾼다(두 번 누르면 기본 폭). 번들 경계는 번들 칸, 저장소 경계는 저장소 칸 폭을 바꾸고 상태 칸이 나머지를 쓴다.
                for k in 0..2 {
                    let bx = head.left() + if k == 0 { repo_x } else { st_x } - 4.0;
                    let hr = egui::Rect::from_center_size(egui::pos2(bx, head.center().y), egui::vec2(10.0, head.height()));
                    let resp = ui.interact(hr, ui.scope_id().with(("plan_col_resize", k)), egui::Sense::click_and_drag());
                    let active = resp.hovered() || resp.dragged();
                    if active {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }
                    ui.painter().vline(bx, head.y_range().shrink(6.0), Stroke::new(if active { 2.0 } else { 1.0 }, if active { theme::ACCENT } else { theme::LINE }));
                    if resp.double_clicked() {
                        col[k] = COL_DEFAULT[k];
                        col_changed = true;
                    } else if resp.dragged() {
                        let other = col[1 - k];
                        let hi = (1.0 - other - min_f).max(min_f);
                        col[k] = (col[k] + resp.drag_delta().x / rest).clamp(min_f, hi);
                        col_changed = true;
                    }
                    // 두 칸 비율의 합이 상태 칸 최소 폭을 침범하면 건드리지 않은 쪽을 줄여 상태 칸이 밀려나지 않게 한다.
                    if col[0] + col[1] > 1.0 - min_f {
                        col[1 - k] = (1.0 - min_f - col[k]).max(min_f);
                    }
                }
            }
            let mut area = egui::ScrollArea::vertical().id_salt("apply_plan_list").auto_shrink([false, false]);
            if reset_scroll {
                area = area.vertical_scroll_offset(0.0);
            }
            let list_out = area.show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (i, (r, u)) in plan.rows.iter().zip(rows_ui.iter_mut()).enumerate() {
                    let row = widgets::row_rect(ui, widgets::ROW_H);
                    // 새로 적용을 시작한 줄이 화면 밖일 때만 그 줄로 스크롤한다(보이면 그대로 둔다).
                    if follow == Some(i) && !ui.clip_rect().contains_rect(row) {
                        ui.scroll_to_rect(row, Some(egui::Align::Center));
                    }
                    // 줄 전체 클릭(선택). 뒤에 만드는 체크 칸이 자기 영역의 클릭을 가져간다.
                    let rr = ui.interact(row, ui.scope_id().with(("plan_row", i)), egui::Sense::click());
                    let body = egui::Rect::from_min_max(row.min, egui::pos2(row.max.x, row.max.y - 1.0));
                    let marked = u.selected || rr.hovered();
                    if marked {
                        let fill = if u.selected { theme::SEL_BG } else { ui.visuals().faint_bg_color };
                        ui.painter().rect_filled(body, CornerRadius::ZERO, fill);
                    }
                    if u.run == RunState::Running {
                        // 선택·호버 배경이 있어도 적용 중임이 보이게 왼쪽에 띠를 항상 긋는다.
                        if !marked {
                            ui.painter().rect_filled(body, CornerRadius::ZERO, RUN_BG);
                        }
                        ui.painter().rect_filled(egui::Rect::from_min_size(body.min, egui::vec2(3.0, body.height())), CornerRadius::ZERO, theme::ACCENT);
                    }
                    if rr.clicked() {
                        clicked = Some((i, ctrl));
                    }
                    let has_repo = !r.repo.is_empty();
                    {
                        let can_check = has_repo && !locked;
                        let box_r = egui::Rect::from_min_size(egui::pos2(row.left() + 2.0, row.top()), egui::vec2(CHK_W, row.height()));
                        let cr = ui.interact(box_r, ui.scope_id().with(("plan_chk", i)), if can_check { egui::Sense::click() } else { egui::Sense::hover() });
                        widgets::paint_check(ui, box_r.center(), usize::from(u.checked && has_repo), 1, can_check && cr.hovered());
                        if can_check && cr.clicked() {
                            u.checked = !u.checked;
                        }
                        if !has_repo {
                            cr.on_hover_text("저장소가 없는 번들은 적용하지 않습니다.");
                        }
                    }
                    // 번들 이름
                    widgets::cell(ui, row, name_x, name_w, |ui| {
                        let mut tip = if r.archive.is_empty() { r.path.clone() } else { r.archive.clone() };
                        let kind = kind_text(&r.kind);
                        if !kind.is_empty() {
                            tip.push_str(&format!("\n종류: {}", kind));
                        }
                        ui.add(egui::Label::new(RichText::new(r.name.as_str()).bold()).truncate()).on_hover_text(tip);
                    });
                    // 저장소
                    widgets::cell(ui, row, repo_x, repo_w, |ui| {
                        if !has_repo {
                            let tip = if r.note.trim().is_empty() { "이 번들을 적용할 저장소를 찾지 못했습니다.".to_string() } else { r.note.clone() };
                            ui.add(egui::Label::new(RichText::new("저장소 없음 — 적용 안 함").color(theme::WARN)).truncate()).on_hover_text(tip);
                        } else if r.create_new {
                            ui.add(egui::Label::new(RichText::new(format!("새 저장소: {}", r.repo)).color(theme::ACCENT)).truncate()).on_hover_text(r.repo.as_str());
                        } else if r.sync_all {
                            // 범위 표시: 이 줄만 전체 동기화로 적용한다.
                            ui.add(egui::Label::new(RichText::new(format!("{} · 전체 동기화", repo_name(&r.repo))).color(theme::ACCENT)).truncate())
                                .on_hover_text(format!("{}\n범위: 전체 동기화", r.repo));
                        } else {
                            ui.add(egui::Label::new(RichText::new(repo_name(&r.repo))).truncate()).on_hover_text(r.repo.as_str());
                        }
                    });
                    // 상태: 적용 중·순서 대기 > 실행 결과 > 전에 적용함 > 대기 > 저장소가 없는 이유
                    widgets::cell(ui, row, st_x, st_w, |ui| {
                        if u.run == RunState::Running {
                            ui.add(egui::Spinner::new().size(13.0).color(theme::ACCENT));
                        }
                        let (text, color, tip): (String, Color32, String) = if u.run == RunState::Running {
                            ("적용 중…".to_string(), theme::ACCENT, "지금 이 번들을 저장소에 적용하는 중입니다.".to_string())
                        } else if u.run == RunState::Queued {
                            ("순서 대기".to_string(), theme::IDLE, "이번 적용 대상입니다. 앞 번들이 끝나면 적용합니다.".to_string())
                        } else if let Some(res) = &u.result {
                            (res.text.clone(), res.status.color(), res.full.clone())
                        } else if !r.seen.is_empty() {
                            let t = format!("전에 적용함 ({})", r.seen);
                            (t.clone(), theme::SUB, t)
                        } else if has_repo && r.sync_all {
                            ("대기 · 전체 동기화".to_string(), theme::IDLE, "현재 체크아웃한 feature/*와 작업 파일만 유지하고, 나머지 로컬 브랜치·태그를 번들과 맞춥니다.".to_string())
                        } else if has_repo {
                            ("대기".to_string(), theme::IDLE, String::new())
                        } else {
                            (r.note.lines().next().unwrap_or("").to_string(), theme::SUB, r.note.clone())
                        };
                        let resp = ui.add(egui::Label::new(RichText::new(text).size(13.0).color(color)).truncate());
                        if !tip.is_empty() {
                            resp.on_hover_text(tip);
                        }
                    });
                }
            });
            // 적용 중 사용자가 목록을 직접 스크롤하면(휠·스크롤바 끌기) 이번 실행에서는 더 끌고 가지 않는다.
            if running {
                let inner = list_out.inner_rect;
                let bar = egui::Rect::from_min_max(egui::pos2(inner.right() - 14.0, inner.top()), inner.max);
                scrolled_now = ui.input(|i| {
                    let over = i.pointer.hover_pos().map(|p| inner.contains(p)).unwrap_or(false);
                    let wheel = over && i.smooth_scroll_delta.y != 0.0;
                    // 스크롤바 위에서 누른 채 실제로 움직였을 때만(단순 클릭은 제외).
                    let drag = i.pointer.primary_down() && i.pointer.delta().y != 0.0 && i.pointer.hover_pos().map(|p| bar.contains(p)).unwrap_or(false);
                    wheel || drag
                });
            }
        });
        if scrolled_now {
            self.user_scrolled = true;
        }
        if col_changed {
            self.col = Some(col);
        }
        // 줄 선택: 클릭 = 그 줄만, Ctrl+클릭 = 토글.
        if let Some((i, ctrl)) = clicked {
            if ctrl {
                if let Some(u) = rows_ui.get_mut(i) {
                    u.selected = !u.selected;
                }
            } else {
                for (j, u) in rows_ui.iter_mut().enumerate() {
                    u.selected = j == i;
                }
            }
        }
        // 머리줄 전체 체크: 전부 체크가 아니면 모두 켜고, 전부 체크면 모두 끈다(저장소 없는 줄은 건드리지 않는다).
        if toggle_all {
            let v = chk_n < with_repo;
            for (r, u) in plan.rows.iter().zip(rows_ui.iter_mut()) {
                if !r.repo.is_empty() {
                    u.checked = v;
                }
            }
        }
    }
}
