//! "번들 만들기" 탭. 저장소를 여러 개 두고 범위(기준 브랜치·주요 참조·전체·작업 브랜치)를 골라 번들을 만든다.
//! 작업 브랜치(feature)는 서버 최신화 없이 지금 로컬 브랜치로 만든다(1.0.5 와 같음, 서버에 닿지 않는 작업 PC 용). 서버 상태를 반영하려면 서버 최신화 단추를 먼저 누른다.
//! 만들기 전 미리보기는 export_preview 가 git 읽기 명령만으로 보여 준다.
//! 저장소 목록은 1.0.5 settings.json 을 그대로 이어 쓴다(9.1): repos 항목, 저장소 모음 폴더(srcRoot) 자동 찾기,
//! 뺀 저장소(exportIgnore), 찾은 저장소 캐시(repoCache*). repos 가 비었으면 옛 exportRepos 를 옮겨 쓴다.

use super::common;
use super::{theme, widgets};
use super::export_preview;
use super::worker::{Apply, Progress};
use super::AppState;
use super::theme::Bold;
use eframe::egui::{self, RichText};
use gitbridge_core::refs::{name_ok, Outcome, NAME_RULE_TEXT};
use gitbridge_core::gitrun::Git;
use gitbridge_core::{archive, create, export, refresh};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Scope {
    #[default]
    Base,
    Standard,
    All,
    Feature,
}

impl Scope {
    /// 엔진·미리보기에 넘기는 이름.
    fn key(self) -> &'static str {
        match self {
            Scope::Base => "base",
            Scope::Standard => "standard",
            Scope::All => "all",
            Scope::Feature => "feature",
        }
    }
}

#[derive(Default)]
pub struct ExportState {
    /// 설정에서 처음 한 번 읽었는지.
    loaded: bool,
    /// 저장소 목록(1.0.5 repos 항목: path·branch·featureBranch·checked).
    repos: Vec<Value>,
    new_repo: String,
    scope: Scope,
    /// 기준 브랜치 지정(비우면 자동).
    base_branch: String,
    out_dir: String,
    full: bool,
    /// 마지막 만들기 결과.
    outcomes: Vec<Outcome>,
    /// 마지막 미리보기 줄.
    preview_lines: Vec<String>,
    /// 저장소 목록 확인 결과 한 줄.
    scan_note: String,
    /// 저장소 카드의 [목록 관리…] 영역이 열렸는지(저장하지 않는다).
    manage_open: bool,
    /// 저장소별 로컬 작업 브랜치 목록(path_key 기준). 그리는 중에는 git 을 부르지 않고 뒤에서 읽어 둔 것만 쓴다.
    branches: HashMap<String, BranchInfo>,
    /// 뒤에서 목록을 읽는 중이면 그 결과를 받는 통로.
    branch_rx: Option<std::sync::mpsc::Receiver<Vec<(String, BranchInfo)>>>,
    /// 목록 전체를 다시 읽어야 한다는 표시(처음 열 때·사용자가 누른 다시 찾기).
    /// 목록에 새로 들어온(아직 읽지 않은) 저장소는 이 표시 없이도 읽는다.
    branch_dirty: bool,
    /// 이미 읽었어도 다시 읽을 저장소 경로(만들기·서버 최신화를 한 저장소).
    branch_need: Vec<String>,
    /// 폴더 추가를 뒤에서 훑는 중이면 그 결과를 받는 통로.
    add_rx: Option<std::sync::mpsc::Receiver<Vec<AddScan>>>,
    /// 아직 훑기 시작하지 않은 추가 경로(훑는 중에 또 넣은 것).
    add_pending: Vec<String>,
    /// 목록에서 빼기 확인 대화(묻는 중이면 값이 있다).
    drop_ask: Option<DropAsk>,
    /// 목록에서 뺀(또는 다시 넣은) 결과 안내 제목과 저장소 이름들(결과 카드에 보인다).
    drop_title: String,
    drop_names: Vec<String>,
    /// '결과 크게 보기' 창(열려 있으면 값이 있다).
    big: Option<widgets::BigText>,
    /// 이 탭을 마지막으로 그린 프레임 번호(바로 앞 프레임이 아니면 다른 탭에서 돌아온 것이다).
    seen_frame: u64,
    /// 지난 프레임에 창이 포커스를 갖고 있었는지(다시 얻는 순간을 알아본다).
    was_focused: bool,
}

/// 목록에서 빼기 확인 대화에 보여 줄 내용.
#[derive(Clone, Default)]
struct DropAsk {
    /// 목록에서 빼고 뺀 저장소(exportIgnore)로 기억할 경로.
    ignored: Vec<String>,
    /// 폴더가 없어 목록에서만 지우고 뺀 저장소로 남기지 않을 경로.
    missing: Vec<String>,
    /// 이 중 지금 표에 안 보이는(검색·필터로 숨겨진) 수.
    hidden: usize,
    /// 체크 항목 빼기에서 온 것이면 참, 폴더 없는 저장소 빼기에서 온 것이면 거짓.
    from_checked: bool,
}

/// 폴더 추가 훑기 결과 하나: (넣은 경로, 그 폴더가 저장소 자체인지, 후보 목록 또는 오류).
type AddScan = (String, bool, Result<Vec<String>, String>);

/// 저장소 하나의 작업 브랜치(feature/·hotfix/) 목록과 지금 체크아웃된 브랜치.
#[derive(Default, Clone)]
pub struct BranchInfo {
    list: Vec<String>,
    current: Option<String>,
    /// 저장소 폴더가 있는지(뒤에서 읽을 때 한 번만 본다. 그리는 중에는 stat 을 부르지 않는다).
    exists: bool,
}

/// 1.0.5 _get_local_feature_info: 로컬 브랜치 가운데 feature/·hotfix/ 만(대소문자 무시), 대소문자 무시 정렬. HEAD 표시가 현재 브랜치.
fn read_branch_info(repo: &str) -> BranchInfo {
    let mut info = BranchInfo::default();
    if !Path::new(repo).is_dir() {
        return info;
    }
    info.exists = true;
    let git = match Git::open(repo) {
        Ok(g) => g,
        Err(_) => return info,
    };
    let r = match git.q(&["for-each-ref", "--format=%(refname:lstrip=2)%09%(HEAD)", "refs/heads/"]) {
        Ok(r) if r.ok() => r,
        _ => return info,
    };
    for line in r.out.lines() {
        let mut it = line.trim_end_matches('\r').splitn(2, '\t');
        let name = it.next().unwrap_or("").trim();
        let head = it.next().unwrap_or("").trim() == "*";
        let low = name.to_lowercase();
        if !(low.starts_with("feature/") || low.starts_with("hotfix/")) {
            continue;
        }
        if head {
            info.current = Some(name.to_string());
        }
        info.list.push(name.to_string());
    }
    info.list.sort_by_key(|n| n.to_lowercase());
    info
}

/// 뒤에서 읽은 결과를 받고, 다시 읽을 일이 있으면 시작한다. 1.0.5 처럼 목록에 없는 저장된 브랜치는 현재 브랜치로 바꿔 저장한다.
fn pump_branches(st: &mut AppState, ctx: &egui::Context) {
    let mut done: Option<Vec<(String, BranchInfo)>> = None;
    if let Some(rx) = &st.export.branch_rx {
        match rx.try_recv() {
            Ok(v) => done = Some(v),
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => st.export.branch_rx = None,
        }
    }
    if let Some(v) = done {
        st.export.branch_rx = None;
        // 읽는 사이 목록에서 뺀 저장소의 결과는 버린다(다시 넣을 때 옛 값으로 남지 않게).
        let live: HashSet<String> = st.export.repos.iter().map(|e| path_key(entry_path(e))).collect();
        for (k, info) in v {
            if live.contains(&k) {
                st.export.branches.insert(k, info);
            }
        }
        let mut changed = false;
        for e in st.export.repos.iter_mut() {
            let k = path_key(entry_path(e));
            let fb = e.get("featureBranch").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if let Some(info) = st.export.branches.get(&k) {
                if let Some(cur) = &info.current {
                    if !info.list.iter().any(|n| *n == fb) {
                        e["featureBranch"] = json!(cur);
                        changed = true;
                    }
                }
            }
        }
        if changed {
            save_entries(st);
        }
    }
    if st.export.branch_rx.is_none() {
        // 저장소마다 git 을 한 번 부른다(이 PC 에서 한 번에 약 77ms). 전체 표시가 없으면 필요한 저장소만 읽는다.
        let all = std::mem::take(&mut st.export.branch_dirty);
        let need: HashSet<String> = std::mem::take(&mut st.export.branch_need).iter().map(|p| path_key(p)).collect();
        let mut seen: HashSet<String> = HashSet::new();
        let paths: Vec<String> = st
            .export
            .repos
            .iter()
            .map(|e| entry_path(e).to_string())
            .filter(|p| !p.is_empty())
            .filter(|p| {
                let k = path_key(p);
                seen.insert(k.clone()) && (all || need.contains(&k) || !st.export.branches.contains_key(&k))
            })
            .collect();
        if paths.is_empty() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let c2 = ctx.clone();
        st.export.branch_rx = Some(rx);
        std::thread::spawn(move || {
            let v: Vec<(String, BranchInfo)> = paths.iter().map(|p| (path_key(p), read_branch_info(p))).collect();
            let _ = tx.send(v);
            c2.request_repaint();
        });
    }
}

/// 저장소 표의 체크박스 열 폭.
const CHK_W: f32 = 40.0;

/// 1.0.5 repos.SKIP_DIRS: 저장소를 찾을 때 내려가지 않는 폴더.
const SKIP_DIRS: &[&str] = &[
    "node_modules", ".git", "build", "target", "out", "bin", "obj", ".idea", ".gradle", "dist", ".next", "vendor",
    ".venv", "venv", ".tox", "__pycache__",
];

/// 경로 비교 키(1.0.5 path_key: 구분자 통일, 끝 구분자 제거, 소문자).
fn path_key(p: &str) -> String {
    p.trim().trim_matches('"').replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

fn entry_path(e: &Value) -> &str {
    e.get("path").and_then(|v| v.as_str()).unwrap_or("")
}

fn entry_checked(e: &Value) -> bool {
    e.get("checked").and_then(|v| v.as_bool()).unwrap_or(true)
}

fn new_entry(path: &str) -> Value {
    json!({"path": path, "branch": "auto", "featureBranch": "", "checked": true})
}

/// 설정 배열을 목록으로(파워셸이 원소 하나를 값 하나로 쓴 경우도 받는다).
fn as_list(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::Null) | None => Vec::new(),
        Some(x) => vec![x.clone()],
    }
}

/// 1.0.5 get_repo_entries: 문자열·객체 항목을 path·branch·featureBranch·checked 객체로 맞춘다.
/// repos 가 비었으면 옛 exportRepos 를 옮겨 쓴다(1.0.5 read_settings).
fn read_entries(settings: &Value) -> Vec<Value> {
    let mut raw: Vec<Value> = as_list(settings.get("repos")).into_iter().filter(|x| !x.is_null() && x.as_str() != Some("")).collect();
    if raw.is_empty() {
        raw = as_list(settings.get("exportRepos")).into_iter().filter(|x| x.as_str().is_some_and(|s| !s.is_empty())).collect();
    }
    raw.into_iter()
        .filter_map(|e| match e {
            Value::String(s) => Some(new_entry(&s)),
            Value::Object(m) => {
                let s = |k: &str| m.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
                let branch = if s("branch").is_empty() { "auto".to_string() } else { s("branch") };
                let checked = m.get("checked").and_then(|v| v.as_bool()).unwrap_or(true);
                Some(json!({"path": s("path"), "branch": branch, "featureBranch": s("featureBranch"), "checked": checked}))
            }
            _ => None,
        })
        .collect()
}

fn ignore_keys(settings: &Value) -> HashSet<String> {
    as_list(settings.get("exportIgnore")).iter().filter_map(|x| x.as_str()).map(path_key).collect()
}

/// 1.0.5 find_git_repos: .git 이 있는 폴더를 너비 우선으로 찾는다. 찾은 폴더 안으로는 내려가지 않는다.
fn find_git_repos(root: &str, depth: usize) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((PathBuf::from(root), 0));
    while let Some((d, lvl)) = queue.pop_front() {
        if d.join(".git").exists() {
            found.push(d.to_string_lossy().to_string());
            continue;
        }
        if lvl >= depth {
            continue;
        }
        let rd = match std::fs::read_dir(&d) {
            Ok(rd) => rd,
            Err(e) if lvl == 0 => return Err(format!("{}: {}", d.display(), e)),
            Err(_) => continue,
        };
        let mut subs: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir() && !t.is_symlink()).unwrap_or(false))
            .map(|e| e.path())
            .filter(|p| {
                let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                !SKIP_DIRS.contains(&n.as_str())
            })
            .collect();
        subs.sort_by_key(|p| p.to_string_lossy().to_uppercase());
        for s in subs {
            queue.push_back((s, lvl + 1));
        }
    }
    Ok(found)
}

fn save_entries(st: &mut AppState) {
    let v = st.export.repos.clone();
    st.save_setting("repos", Value::Array(v));
}

/// 1.0.5 sync_src_root_repos: 저장소 모음 폴더에서 찾은 저장소 중 목록에 없고 뺀 적 없는 것을 체크해 넣는다.
/// 찾은 경로는 repoCache* 에 남긴다(번들 적용의 저장소 후보로 쓰인다). 넣은 개수를 돌려준다.
fn merge_found(st: &mut AppState, root: &str, found: &[String]) -> usize {
    let ignore = ignore_keys(&st.settings);
    let mut known: HashSet<String> = st.export.repos.iter().map(|e| path_key(entry_path(e))).collect();
    let mut added = 0;
    for c in found {
        let k = path_key(c);
        if known.contains(&k) || ignore.contains(&k) {
            continue;
        }
        st.export.repos.push(new_entry(c));
        known.insert(k);
        added += 1;
    }
    st.save_setting("repoCacheRoot", json!(root));
    st.save_setting("repoCachePaths", json!(found));
    st.save_setting("repoCacheUpdatedAt", json!(gitbridge_core::time::now().display()));
    if added > 0 {
        save_entries(st);
    }
    added
}

/// 저장소 모음 폴더를 뒤에서 훑어 목록에 넣는다. quiet 면 폴더가 없을 때 알리지 않는다.
fn start_scan(st: &mut AppState, quiet: bool) {
    let root = common::setting_str(&st.settings, "srcRoot");
    if root.is_empty() || !Path::new(&root).is_dir() {
        if !quiet {
            st.export.scan_note = "저장소 모음 폴더를 먼저 지정해 주세요.".into();
        }
        return;
    }
    if st.busy() {
        return;
    }
    st.export.scan_note = "저장소 모음 폴더에서 저장소를 찾는 중…".into();
    st.spawn("저장소 찾기", move |_p: &Progress| -> Apply {
        let res = find_git_repos(&root, 5);
        Box::new(move |st: &mut AppState| {
            if common::setting_str(&st.settings, "srcRoot") != root {
                return;
            }
            // 사용자가 누른 다시 찾기면 전체를 다시 읽고, 뒤에서 한 찾기면 새로 들어온 저장소만 읽는다.
            if !quiet {
                st.export.branch_dirty = true;
            }
            st.export.scan_note = match res {
                Err(e) => format!("새 저장소를 확인하지 못했습니다. 저장된 목록은 그대로 사용할 수 있습니다.\n새 저장소 다시 찾기로 다시 시도해 주세요.\n{}", e),
                Ok(found) => match merge_found(st, &root, &found) {
                    0 => "저장소 목록 확인 완료 · 새로 추가된 저장소 없음".into(),
                    n if quiet => format!("뒤에서 새 저장소 {}개를 찾아 목록에 넣었습니다.", n),
                    n => format!("저장소 모음 폴더에서 새 저장소 {}개를 목록에 넣었습니다.", n),
                },
            };
        })
    });
}

/// 1.0.5 add_repo_paths: 저장소 폴더면 그대로, 아니면 안에서 찾아 넣는다.
/// 폴더 훑기는 뒤 스레드에서 한다. 이미 훑는 중이거나 기다리는 같은 경로는 다시 넣지 않는다.
fn queue_add(st: &mut AppState, p: &str) {
    let p = p.trim().trim_matches('"').to_string();
    if p.is_empty() {
        st.export.scan_note = "폴더를 찾지 못했습니다. 경로를 확인해 주세요.".into();
        return;
    }
    let k = path_key(&p);
    if st.export.add_pending.iter().any(|x| path_key(x) == k) {
        return;
    }
    st.export.add_pending.push(p);
    st.export.scan_note = "저장소를 찾는 중…".into();
}

/// 훑기 결과를 받고, 기다리는 경로가 있으면 다음 훑기를 시작한다(화면 스레드, 매 프레임).
fn pump_adds(st: &mut AppState, ctx: &egui::Context) {
    let mut done: Option<Vec<AddScan>> = None;
    if let Some(rx) = &st.export.add_rx {
        match rx.try_recv() {
            Ok(v) => done = Some(v),
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                st.export.add_rx = None;
                st.export.scan_note = "저장소 찾기가 중간에 멈췄습니다. 다시 넣어 주세요.".into();
            }
        }
    }
    if let Some(v) = done {
        st.export.add_rx = None;
        let mut added = 0;
        let mut err: Option<String> = None;
        for (p, direct, res) in v {
            match res {
                Ok(cands) => added += merge_added(st, &p, direct, &cands),
                Err(e) => err = Some(e),
            }
        }
        if added > 0 {
            save_entries(st);
            st.export.new_repo.clear();
        }
        st.export.scan_note = match err {
            Some(e) if added == 0 => e,
            _ => format!("저장소 {}개를 목록에 넣었습니다.", added),
        };
    }
    if st.export.add_rx.is_none() && !st.export.add_pending.is_empty() {
        let paths = std::mem::take(&mut st.export.add_pending);
        let (tx, rx) = std::sync::mpsc::channel();
        let c2 = ctx.clone();
        st.export.add_rx = Some(rx);
        std::thread::spawn(move || {
            let v: Vec<AddScan> = paths
                .into_iter()
                .map(|p| {
                    if !Path::new(&p).is_dir() {
                        return (p, false, Err("폴더를 찾지 못했습니다. 경로를 확인해 주세요.".to_string()));
                    }
                    let direct = Path::new(&p).join(".git").exists();
                    let cands = if direct { Ok(vec![p.clone()]) } else { find_git_repos(&p, 5) };
                    (p, direct, cands)
                })
                .collect();
            let _ = tx.send(v);
            c2.request_repaint();
        });
    }
}

/// 훑은 후보를 목록에 넣는다(화면 스레드). 뺀 저장소는 그 폴더를 직접 넣을 때만 다시 넣고, 상위 폴더 검색으로는 되살리지 않는다.
fn merge_added(st: &mut AppState, _root: &str, direct: bool, cands: &[String]) -> usize {
    let ignore = ignore_keys(&st.settings);
    let mut known: HashSet<String> = st.export.repos.iter().map(|e| path_key(entry_path(e))).collect();
    let mut added = 0;
    for c in cands {
        let k = path_key(c);
        if known.contains(&k) || (!direct && ignore.contains(&k)) {
            continue;
        }
        if direct {
            let rest: Vec<Value> =
                as_list(st.settings.get("exportIgnore")).into_iter().filter(|x| x.as_str().is_some_and(|s| path_key(s) != k)).collect();
            st.save_setting("exportIgnore", Value::Array(rest));
        }
        st.export.repos.push(new_entry(c));
        known.insert(k);
        added += 1;
    }
    added
}

/// 목록에서 빼고 exportIgnore 에 남겨 다시 찾기에서 되살아나지 않게 한다(한 줄 빼기).
fn remove_at(st: &mut AppState, i: usize) {
    if i >= st.export.repos.len() {
        return;
    }
    let p = entry_path(&st.export.repos[i]).to_string();
    drop_entries(st, &[p], &[]);
}

/// 지금 표에 보이는 줄인지. 2.0.0 번들 만들기 목록에는 검색·필터가 없어 모든 줄이 보인다(나중에 필터를 넣으면 여기만 고친다).
fn row_visible(_e: &Value) -> bool {
    true
}

/// 폴더가 없는 저장소 경로(뒤에서 읽어 둔 결과 기준).
fn missing_paths(ex: &ExportState) -> Vec<String> {
    ex.repos
        .iter()
        .map(entry_path)
        .filter(|p| !p.is_empty() && ex.branches.get(&path_key(p)).is_some_and(|i| !i.exists))
        .map(|p| p.to_string())
        .collect()
}

/// 뺀 저장소(exportIgnore) 경로들(빈 값 제외).
fn ignored_list(settings: &Value) -> Vec<String> {
    as_list(settings.get("exportIgnore")).iter().filter_map(|x| x.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string()).collect()
}

/// 1.0.5 _drop_entries: ignored 는 설정 목록에서 빼고 뺀 저장소로 기억(다시 넣을 때 쓸 기준·작업 브랜치·체크도 exportRemovedEntries 에 보관),
/// missing 은 목록에서만 지운다(뺀 저장소 목록에 남기지 않는다). 저장소 폴더는 건드리지 않는다.
fn drop_entries(st: &mut AppState, ignored: &[String], missing: &[String]) {
    let drop_keys: HashSet<String> = ignored.iter().chain(missing.iter()).map(|p| path_key(p)).collect();
    let ignore_set: HashSet<String> = ignored.iter().map(|p| path_key(p)).collect();
    let mut saved: serde_json::Map<String, Value> = match st.settings.get("exportRemovedEntries") {
        Some(Value::Object(m)) => m.clone(),
        _ => serde_json::Map::new(),
    };
    for e in st.export.repos.iter() {
        let p = entry_path(e);
        let k = path_key(p);
        if !ignore_set.contains(&k) {
            continue;
        }
        let olds: Vec<String> = saved.keys().filter(|x| path_key(x) == k).cloned().collect();
        for o in olds {
            saved.remove(&o);
        }
        let s = |key: &str| e.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let branch = if s("branch").is_empty() { "auto".to_string() } else { s("branch") };
        saved.insert(p.to_string(), json!({"branch": branch, "featureBranch": s("featureBranch"), "checked": entry_checked(e)}));
    }
    st.save_setting("exportRemovedEntries", Value::Object(saved));
    // 뺀 저장소는 [다시 찾기]에서 다시 들어오지 않게 기억한다.
    let mut ignore: Vec<Value> = as_list(st.settings.get("exportIgnore")).into_iter().filter(|x| x.as_str().is_some_and(|s| !s.is_empty())).collect();
    let mut known: HashSet<String> = ignore.iter().filter_map(|x| x.as_str()).map(path_key).collect();
    for p in ignored {
        if known.insert(path_key(p)) {
            ignore.push(json!(p));
        }
    }
    st.save_setting("exportIgnore", Value::Array(ignore));
    st.export.repos.retain(|e| !drop_keys.contains(&path_key(entry_path(e))));
    save_entries(st);
    // 뺀 저장소의 읽은 값을 지워 두면 다시 넣을 때 새로 읽는다. 남은 저장소는 다시 읽지 않는다.
    st.export.branches.retain(|k, _| !drop_keys.contains(k));
}

/// 결과 카드에 보일 안내를 정한다.
fn set_drop_note(st: &mut AppState, title: String, paths: &[String], extra: Option<String>) {
    st.export.drop_title = title;
    let mut names: Vec<String> =
        paths.iter().map(|p| Path::new(p.trim_end_matches(['\\', '/'])).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.clone())).collect();
    if let Some(x) = extra {
        names.push(x);
    }
    st.export.drop_names = names;
    st.export.outcomes.clear();
    st.export.preview_lines.clear();
}

/// 체크 항목 빼기: 먼저 확인 대화를 띄운다(체크한 수와 검색·필터로 숨겨진 수를 보여 준다).
fn begin_drop_checked(st: &mut AppState) {
    if st.busy() {
        return;
    }
    let missing_keys: HashSet<String> = missing_paths(&st.export).iter().map(|p| path_key(p)).collect();
    let mut ignored: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut hidden = 0usize;
    for e in st.export.repos.iter().filter(|e| entry_checked(e)) {
        let p = entry_path(e);
        if p.is_empty() {
            continue;
        }
        if missing_keys.contains(&path_key(p)) {
            missing.push(p.to_string());
        } else {
            ignored.push(p.to_string());
        }
        if !row_visible(e) {
            hidden += 1;
        }
    }
    if ignored.is_empty() && missing.is_empty() {
        return;
    }
    st.export.drop_ask = Some(DropAsk { ignored, missing, hidden, from_checked: true });
}

/// 폴더 없는 저장소 빼기: 확인 대화를 띄운다.
fn begin_drop_missing(st: &mut AppState) {
    if st.busy() {
        return;
    }
    let missing = missing_paths(&st.export);
    if missing.is_empty() {
        return;
    }
    st.export.drop_ask = Some(DropAsk { ignored: Vec::new(), missing, hidden: 0, from_checked: false });
}

/// 확인 대화를 그리고, 답에 따라 뺀다.
fn show_drop_confirm(st: &mut AppState, ctx: &egui::Context) {
    let ask = match st.export.drop_ask.clone() {
        Some(a) => a,
        None => return,
    };
    let mut yes = false;
    let mut no = false;
    egui::Window::new("목록에서 빼기").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0)).show(ctx, |ui| {
        ui.set_max_width(420.0);
        if ask.from_checked {
            ui.label(format!("체크한 저장소 {}개를 목록에서 뺍니까?", ask.ignored.len() + ask.missing.len()));
            ui.label(format!("이 중 {}개는 검색·필터로 지금 표에 보이지 않는 저장소입니다.", ask.hidden));
            if !ask.missing.is_empty() {
                ui.label(format!("이 중 폴더가 없는 {}개는 뺀 저장소 목록에 남기지 않습니다.", ask.missing.len()));
            }
            ui.add_space(6.0);
            ui.label("저장소 폴더는 그대로입니다. 뺀 저장소는 [목록 관리 → 뺀 저장소 모두 다시 넣기]로 기준·작업 브랜치 설정까지 되돌릴 수 있습니다.");
        } else {
            ui.label(format!("폴더가 없는 저장소 {}개를 목록에서 뺍니까?", ask.missing.len()));
            ui.add_space(6.0);
            ui.label("폴더를 다시 만들면 [새 저장소 다시 찾기]나 끌어다 놓기로 다시 넣을 수 있습니다.");
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if widgets::secondary(ui, "빼기", true).clicked() {
                yes = true;
            }
            if widgets::secondary(ui, "취소", true).clicked() {
                no = true;
            }
        });
    });
    if no {
        st.export.drop_ask = None;
    }
    if yes {
        st.export.drop_ask = None;
        drop_entries(st, &ask.ignored, &ask.missing);
        let all: Vec<String> = ask.ignored.iter().chain(ask.missing.iter()).cloned().collect();
        let (title, extra) = if ask.from_checked {
            (format!("저장소 {}개를 목록에서 뺐습니다.", all.len()), Some("저장소 폴더는 그대로입니다. [목록 관리 → 뺀 저장소 모두 다시 넣기]로 되돌릴 수 있습니다.".to_string()))
        } else {
            (format!("폴더 없는 저장소 {}개를 목록에서 뺐습니다.", all.len()), None)
        };
        set_drop_note(st, title, &all, extra);
    }
}

/// 1.0.5 restore_ignored: 뺀 저장소를 exportIgnore 에서 지우고 목록에 되돌린다. 뺄 때 보관한 기준·작업 브랜치·체크를 그대로 되살리고,
/// 보관한 것이 없는 저장소는 폴더 추가와 같은 방식(뒤에서 훑기)으로 넣는다. 폴더가 없어진 것은 목록에 넣지 않고 기억만 지운다.
fn restore_ignored(st: &mut AppState, paths: Vec<String>) {
    if st.busy() || paths.is_empty() {
        return;
    }
    let keys: HashSet<String> = paths.iter().map(|p| path_key(p)).collect();
    let rest_ignore: Vec<Value> =
        as_list(st.settings.get("exportIgnore")).into_iter().filter(|x| x.as_str().is_some_and(|s| !s.is_empty() && !keys.contains(&path_key(s)))).collect();
    st.save_setting("exportIgnore", Value::Array(rest_ignore));
    let mut saved: serde_json::Map<String, Value> = match st.settings.get("exportRemovedEntries") {
        Some(Value::Object(m)) => m.clone(),
        _ => serde_json::Map::new(),
    };
    let by_key: HashMap<String, String> = saved.keys().map(|k| (path_key(k), k.clone())).collect();
    let mut known: HashSet<String> = st.export.repos.iter().map(|e| path_key(entry_path(e))).collect();
    let mut restored: Vec<String> = Vec::new();
    let mut rest: Vec<String> = Vec::new();
    let mut gone = 0usize;
    for p in &paths {
        let key = path_key(p);
        let old: Option<Value> = by_key.get(&key).and_then(|k| saved.remove(k));
        if !Path::new(p).is_dir() {
            gone += 1;
            continue;
        }
        if known.contains(&key) {
            continue;
        }
        match old {
            Some(Value::Object(o)) => {
                let s = |k: &str| o.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
                let branch = if s("branch").is_empty() { "auto".to_string() } else { s("branch") };
                let checked = o.get("checked").and_then(|v| v.as_bool()).unwrap_or(true);
                st.export.repos.push(json!({"path": p, "branch": branch, "featureBranch": s("featureBranch"), "checked": checked}));
                known.insert(key);
                restored.push(p.clone());
            }
            _ => rest.push(p.clone()),
        }
    }
    st.save_setting("exportRemovedEntries", Value::Object(saved));
    save_entries(st);
    for p in &rest {
        queue_add(st, p);
    }
    // 다시 넣은 저장소는 예전에 읽은 값이 남아 있을 수 있어 새로 읽는다.
    st.export.branch_need.extend(restored.iter().cloned());
    let all: Vec<String> = restored.iter().chain(rest.iter()).cloned().collect();
    let extra = if gone > 0 { Some(format!("폴더가 없어 목록에서만 지운 항목 {}개.", gone)) } else { None };
    set_drop_note(st, format!("저장소 {}개를 목록에 다시 넣었습니다.", all.len()), &all, extra);
}

fn load(st: &mut AppState) {
    st.export.repos = read_entries(&st.settings);
    st.export.out_dir = common::setting_str(&st.settings, "exportDir");
    st.export.loaded = true;
    st.export.branch_dirty = true;
    start_scan(st, true);
}

/// 체크한 저장소 경로.
fn checked_paths(st: &AppState) -> Vec<String> {
    st.export.repos.iter().filter(|e| entry_checked(e)).map(|e| entry_path(e).to_string()).filter(|p| !p.is_empty()).collect()
}

pub fn ui(st: &mut AppState, ui: &mut egui::Ui) {
    if !st.export.loaded {
        load(st);
    }
    let ctx = ui.ctx().clone();
    pump_adds(st, &ctx);
    // 다른 탭에서 돌아왔거나 창이 다시 포커스를 얻으면, 그사이 밖에서 만든 작업 브랜치가 보이게 체크한 저장소를 다시 읽는다(1.0.5 와 같다).
    let frame = ctx.cumulative_frame_nr();
    let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
    // 한 프레임을 두 번 그리는 경우(egui 의 다시 그리기 패스)는 같은 번호라 돌아온 것으로 치지 않는다.
    let back = frame > st.export.seen_frame + 1 || (focused && !st.export.was_focused);
    st.export.seen_frame = frame;
    st.export.was_focused = focused;
    if back && st.export.scope == Scope::Feature {
        let checked = checked_paths(st);
        st.export.branch_need.extend(checked);
    }
    pump_branches(st, &ctx);
    let busy = st.busy();
    let src_root = common::setting_str(&st.settings, "srcRoot");
    let ignored_paths = ignored_list(&st.settings);
    let ignored = ignored_paths.len();
    let mut want_drop_checked = false;
    let mut want_drop_missing = false;
    let mut want_restore = false;
    let mut list_rect: Option<egui::Rect> = None;
    let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
    let mut new_root: Option<String> = None;
    let mut want_rescan = false;
    let mut remove: Option<usize> = None;
    let mut adds: Vec<String> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| p.is_dir()).map(|p| p.display().to_string()).collect());
    let mut save_checks = false;
    let mut save_dir = false;
    let mut want_make = false;
    let mut want_preview = false;
    let mut want_refresh = false;
    {
        let ex = &mut st.export;
        let total = ex.repos.len();
        let checked = ex.repos.iter().filter(|e| entry_checked(e)).count();
        widgets::head(ui, "번들 만들기", "체크한 저장소마다 번들 파일을 만듭니다. 지난번 이후 달라진 것만 담는 방식이 기본입니다.");

        // 오른쪽 열: 내보내기 설정 + 맨 아래 실행 상자
        widgets::side_column(
            ui,
            "export_side",
            |ui| {
                widgets::card(ui, Some("내보내기 설정"), |ui| {
                    widgets::labeled(ui, "출력 폴더", |ui| {
                        let r = widgets::path_field(ui, &mut ex.out_dir, "번들 파일을 둘 폴더 (저장소 바깥)", ui.available_width());
                        if r.lost_focus() {
                            save_dir = true;
                        }
                        ui.add_space(widgets::GAP_S);
                        let w = (ui.available_width() - widgets::GAP_S) / 2.0;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = widgets::GAP_S;
                            if widgets::secondary_w(ui, "찾아보기", true, w).clicked() {
                                if let Some(p) = common::pick_folder("번들을 둘 폴더 (저장소 바깥)", &ex.out_dir) {
                                    ex.out_dir = p;
                                    save_dir = true;
                                }
                            }
                            let can_open = Path::new(ex.out_dir.trim()).is_dir();
                            if widgets::secondary_w(ui, "폴더 열기", can_open, w).clicked() {
                                let _ = std::process::Command::new("explorer").arg(ex.out_dir.trim()).spawn();
                            }
                        });
                    });
                    widgets::labeled(ui, "보낼 범위", |ui| {
                        let names = [(Scope::Base, "기준 브랜치만"), (Scope::Standard, "기준 + develop + 태그"), (Scope::All, "모든 서버 브랜치 + 태그"), (Scope::Feature, "작업 브랜치 (feature·hotfix)")];
                        let cur = names.iter().find(|n| n.0 == ex.scope).map(|n| n.1).unwrap_or("");
                        egui::ComboBox::from_id_salt("export_scope").selected_text(cur).width(ui.available_width()).show_ui(ui, |ui| {
                            for (sc, label) in names {
                                ui.selectable_value(&mut ex.scope, sc, label);
                            }
                        });
                    });
                    if ex.scope == Scope::Feature {
                        widgets::small(ui, "작업 브랜치는 저장소 목록에서 저장소마다 고릅니다. 서버에 접속하지 않고 지금 로컬 브랜치로 만듭니다. 서버 상태를 반영하려면 먼저 서버 최신화를 눌러 주세요.");
                    } else {
                        widgets::labeled(ui, "기준 브랜치", |ui| {
                            widgets::path_field(ui, &mut ex.base_branch, "비워 두면 자동", ui.available_width());
                            widgets::small(ui, "비우면 서버의 production · main · master 순으로 자동으로 고릅니다.");
                        });
                    }
                    if ex.scope == Scope::Feature {
                        // 1.0.5 와 같게 작업 브랜치 번들은 늘 전체 이력을 담는다(받는 쪽에 기준 이력이 없어도 적용된다).
                        let mut on = true;
                        ui.add_enabled(false, egui::Checkbox::new(&mut on, "전체를 담은 번들 (작업 브랜치는 늘 전체 이력)"));
                    } else {
                        ui.checkbox(&mut ex.full, "전체를 담은 번들 (새 저장소를 만들 수 있음)");
                    }
                });
            },
            |ui| {
                ui.spacing_mut().item_spacing.y = widgets::GAP_S;
                if busy {
                    widgets::small(ui, "다른 작업이 진행 중입니다.");
                } else if checked == 0 {
                    widgets::small(ui, "저장소를 체크해 주세요.");
                } else {
                    widgets::small(ui, "파일을 만들지 않고 무엇이 담길지만 미리 볼 수 있습니다.");
                }
                ui.add_space(4.0);
                let label = if checked > 0 { format!("체크한 {}개 번들 만들기", checked) } else { "번들 만들기".to_string() };
                if widgets::primary_full(ui, &label, !busy && checked > 0).clicked() {
                    want_make = true;
                }
                if widgets::secondary_full(ui, "미리보기", !busy && checked > 0).on_hover_text("파일을 만들지 않고 무엇이 담길지만 보여 줍니다.").clicked() {
                    want_preview = true;
                }
                let rlabel = if checked > 0 { format!("체크한 {}개 서버 최신화", checked) } else { "서버 최신화".to_string() };
                if widgets::secondary_full(ui, &rlabel, !busy && checked > 0)
                    .on_hover_text("체크한 저장소의 서버 브랜치를 받아, 뒤처진 로컬 브랜치를 앞으로 당겨 맞춥니다(빠른 전진). 로컬 작업과 겹치거나 갈라진 브랜치는 그대로 둡니다. 전체를 하려면 [전체 체크] 후 누르세요.")
                    .clicked()
                {
                    want_refresh = true;
                }
            },
        );

        // 왼쪽 본문: 저장소 목록(위, 남는 높이 전부) + 결과(아래, 비었으면 한 줄 높이).
        // 목록과 결과는 각자 안에서만 스크롤한다.
        // 위아래 카드 사이 간격은 gap 하나만 쓴다(기본 항목 간격이 더해져 아래로 넘치던 원인).
        ui.spacing_mut().item_spacing.y = 0.0;
        let col_rect = ui.available_rect_before_wrap();
        ui.set_clip_rect(col_rect.intersect(ui.clip_rect()));
        let gap = widgets::GAP_L;
        let avail = ui.available_height();
        let has_res = !ex.outcomes.is_empty() || !ex.preview_lines.is_empty() || !ex.drop_title.is_empty();
        let missing_n = missing_paths(&*ex).len();
        let res_h = if has_res { ((avail - gap) * 0.44).max(100.0).min((avail - gap - 90.0).max(60.0)) } else { 80.0 };
        let top_h = (avail - gap - res_h).max(90.0);
        let w = ui.available_width();
        let stroke = if hovering { egui::Stroke::new(2.0, theme::ACCENT) } else { egui::Stroke::new(1.0, theme::LINE) };
        // 평상시 문구는 숨기고 문제가 있을 때만 머리줄 아래에 보인다.
        let note_problem = !ex.scan_note.is_empty() && !ex.scan_note.starts_with("저장소 목록 확인 완료") && !ex.scan_note.contains("찾는 중");
        ui.allocate_ui_with_layout(egui::vec2(w, top_h), egui::Layout::top_down(egui::Align::Min), |ui| {
            egui::Frame::new().fill(theme::CARD).stroke(stroke).corner_radius(egui::CornerRadius::same(8)).inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_height(top_h - 26.0);
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = widgets::GAP_S;
                    ui.label(RichText::new("저장소").size(15.0).bold());
                    ui.label(RichText::new(format!("전체 {} · 체크 {}", total, checked)).bold().color(theme::ACCENT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let lab = if ex.manage_open { "목록 관리 닫기" } else { "목록 관리…" };
                        if widgets::tool(ui, lab, true).clicked() {
                            ex.manage_open = !ex.manage_open;
                        }
                        // 1.0.5 check_visible: 지금 표에 보이는 줄만 체크한다(이미 체크한 줄은 그대로).
                        if widgets::tool(ui, "표시 항목 체크", total > 0).on_hover_text("지금 표에 보이는 저장소를 체크합니다. 이미 체크한 저장소는 그대로 둡니다.").clicked() {
                            for e in ex.repos.iter_mut() {
                                if row_visible(e) {
                                    e["checked"] = json!(true);
                                }
                            }
                            save_checks = true;
                        }
                        if widgets::tool(ui, "체크 해제", total > 0).clicked() {
                            for e in ex.repos.iter_mut() {
                                e["checked"] = json!(false);
                            }
                            save_checks = true;
                        }
                        if widgets::tool(ui, "전체 체크", total > 0).clicked() {
                            for e in ex.repos.iter_mut() {
                                e["checked"] = json!(true);
                            }
                            save_checks = true;
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = widgets::GAP_S;
                    if widgets::tool(ui, "체크 항목 목록에서 빼기", !busy && checked > 0).on_hover_text("체크한 저장소를 설정 목록에서만 뺍니다(Delete 키도 같습니다). 저장소 폴더는 그대로입니다.").clicked() {
                        want_drop_checked = true;
                    }
                    ui.add(egui::Label::new(RichText::new("뺀 저장소는 다시 찾기에서 돌아오지 않습니다 · [목록 관리 → 뺀 저장소 다시 넣기]로 되돌립니다.").size(12.0).color(theme::SUB)).truncate());
                });
                if note_problem && !ex.manage_open {
                    widgets::small(ui, &ex.scan_note);
                }
                if ex.manage_open {
                    egui::Frame::new().fill(theme::BACK).stroke(egui::Stroke::new(1.0, theme::LINE)).corner_radius(egui::CornerRadius::same(6)).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = widgets::GAP_S;
                        widgets::field_row(ui, "저장소 모음 폴더", |ui| {
                            ui.spacing_mut().item_spacing.x = widgets::GAP_S;
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if widgets::tool(ui, "새 저장소 다시 찾기", !busy).clicked() {
                                    want_rescan = true;
                                }
                                if widgets::tool(ui, "바꾸기…", !busy).clicked() {
                                    if let Some(p) = common::pick_folder("저장소 모음 폴더 선택", &src_root) {
                                        new_root = Some(p);
                                    }
                                }
                                let shown = if src_root.is_empty() { "(지정 안 함)".to_string() } else { src_root.clone() };
                                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                    ui.add(egui::Label::new(RichText::new(shown).monospace().color(theme::SUB)).truncate());
                                });
                            });
                        });
                        widgets::field_row(ui, "저장소 추가", |ui| {
                            ui.spacing_mut().item_spacing.x = widgets::GAP_S;
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if widgets::secondary(ui, "추가", !busy).clicked() {
                                    adds.push(ex.new_repo.clone());
                                }
                                if widgets::secondary(ui, "폴더 고르기", true).clicked() {
                                    if let Some(p) = common::pick_folder("저장소 폴더 고르기", &ex.new_repo) {
                                        ex.new_repo = p;
                                    }
                                }
                                let w = ui.available_width();
                                widgets::path_field(ui, &mut ex.new_repo, "저장소 폴더 경로", w);
                            });
                        });
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = widgets::GAP_S;
                            if widgets::secondary(ui, &format!("뺀 저장소 모두 다시 넣기 ({}개)", ignored), !busy && ignored > 0).clicked() {
                                want_restore = true;
                            }
                            if widgets::secondary(ui, &format!("폴더 없는 저장소 {}개 빼기", missing_n), !busy && missing_n > 0).clicked() {
                                want_drop_missing = true;
                            }
                        });
                        if !ex.scan_note.is_empty() {
                            widgets::small(ui, &ex.scan_note);
                        }
                        if ignored > 0 {
                            widgets::small(ui, &format!("뺀 저장소는 다시 찾기에서 돌아오지 않습니다 ({}개). 그 폴더를 직접 추가하면 다시 들어옵니다.", ignored));
                        }
                    });
                }
                ui.add_space(4.0);
                if ex.repos.is_empty() {
                    widgets::empty_hint(ui, "등록된 저장소가 없습니다. 저장소 폴더를 이 카드에 끌어다 놓거나 [목록 관리…]에서 폴더를 골라 추가해 주세요.");
                } else {
                    let rem = (ui.available_height() - 10.0).max(60.0);
                    let list_frame = egui::Frame::new().fill(theme::LOG_BG).stroke(egui::Stroke::new(1.0, theme::LINE)).corner_radius(egui::CornerRadius::same(6)).inner_margin(egui::Margin::symmetric(10, 4)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.set_height(rem);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        // 열 위치는 폭에서 정해 머리줄과 줄이 같게 한다.
                        let cw = ui.available_width() - ui.spacing().scroll.allocated_width();
                        let (rm_w, st_w, br_w) = (56.0, 90.0, 230.0);
                        let show_fb = ex.scope == Scope::Feature;
                        let rm_x = cw - rm_w - 2.0;
                        let st_x = rm_x - 8.0 - st_w;
                        // 작업 브랜치 열은 작업 브랜치 범위일 때만 둔다. 아니면 그 폭을 저장소·위치 열이 쓴다.
                        let br_x = if show_fb { st_x - 8.0 - br_w } else { st_x };
                        let rest = br_x - 8.0 - 2.0;
                        let name_x = 2.0 + CHK_W;
                        let name_w = ((rest - CHK_W) * 0.55).max(40.0);
                        let dir_x = name_x + name_w + 8.0;
                        let dir_w = (br_x - 8.0 - dir_x).max(0.0);
                        {
                            let head = widgets::row_rect(ui, 26.0);
                            let all_box = egui::Rect::from_min_size(egui::pos2(head.left() + 2.0, head.top()), egui::vec2(CHK_W, head.height()));
                            let all_n = ex.repos.len();
                            let chk_n = ex.repos.iter().filter(|e| entry_checked(e)).count();
                            if widgets::tri_check(ui, all_box, chk_n, all_n, all_n > 0) {
                                let v = chk_n < all_n;
                                for e in ex.repos.iter_mut() {
                                    e["checked"] = json!(v);
                                }
                                save_checks = true;
                            }
                            for (x, w, name) in [(name_x, name_w, "저장소"), (dir_x, dir_w, "위치"), (br_x, br_w, "작업 브랜치"), (st_x, st_w, "상태")] {
                                if name == "작업 브랜치" && !show_fb {
                                    continue;
                                }
                                widgets::cell(ui, head, x, w, |ui| {
                                    ui.label(RichText::new(name).size(12.0).color(theme::SUB));
                                });
                            }
                        }
                        egui::ScrollArea::vertical().id_salt("export_repo_list").auto_shrink([false, false]).show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for (i, e) in ex.repos.iter_mut().enumerate() {
                                let row = widgets::row_rect(ui, widgets::ROW_H);
                                let c = entry_checked(e);
                                let p = entry_path(e).to_string();
                                let missing = ex.branches.get(&path_key(&p)).is_some_and(|i| !i.exists);
                                let name = Path::new(&p).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.clone());
                                let dir = Path::new(&p).parent().map(|d| d.display().to_string()).unwrap_or_default();
                                let fb = e.get("featureBranch").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let info = ex.branches.get(&path_key(&p));
                                // 아직 읽지 않은 저장소는 다음 읽기에 들어가므로 읽는 중으로 보인다.
                                let reading = info.is_none() && !p.is_empty();
                                let mut pick: Option<String> = None;
                                {
                                    let box_r = egui::Rect::from_min_size(egui::pos2(row.left() + 2.0, row.top()), egui::vec2(CHK_W, row.height()));
                                    let resp = ui.interact(box_r, ui.scope_id().with(("chk", i)), egui::Sense::click());
                                    widgets::paint_check(ui, box_r.center(), usize::from(c), 1, resp.hovered());
                                    if resp.clicked() {
                                        e["checked"] = json!(!c);
                                        save_checks = true;
                                    }
                                }
                                widgets::cell(ui, row, name_x, name_w, |ui| {
                                    ui.add(egui::Label::new(RichText::new(name).bold()).truncate());
                                });
                                widgets::cell(ui, row, dir_x, dir_w, |ui| {
                                    ui.add(egui::Label::new(RichText::new(dir).size(12.0).color(if missing { theme::ERR } else { theme::SUB })).truncate());
                                });
                                if show_fb {
                                widgets::cell(ui, row, br_x, br_w, |ui| {
                                    if missing {
                                        ui.label(RichText::new("-").size(12.0).color(theme::SUB));
                                    } else if reading {
                                        ui.label(RichText::new("읽는 중…").size(12.0).color(theme::SUB));
                                    } else if info.map_or(true, |i| i.list.is_empty()) {
                                        ui.label(RichText::new("feature 없음").size(12.0).color(theme::IDLE));
                                    } else if let Some(info) = info {
                                        let shown = if fb.is_empty() { "고르기".to_string() } else { fb.clone() };
                                        let cb = egui::ComboBox::from_id_salt(("export_fb", i)).selected_text(RichText::new(shown).size(12.0)).width(br_w - 8.0).truncate().show_ui(ui, |ui| {
                                            for n in &info.list {
                                                if ui.selectable_label(*n == fb, n.as_str()).clicked() {
                                                    pick = Some(n.clone());
                                                }
                                            }
                                        });
                                        if !fb.is_empty() {
                                            cb.response.on_hover_text(fb.as_str());
                                        }
                                    }
                                });
                                }
                                if let Some(n) = pick {
                                    e["featureBranch"] = json!(n);
                                    save_checks = true;
                                }
                                widgets::cell(ui, row, st_x, st_w, |ui| {
                                    if missing {
                                        widgets::repo_missing_mark(ui, "폴더 없음");
                                    }
                                });
                                widgets::cell(ui, row, rm_x, rm_w, |ui| {
                                    if widgets::ghost_w(ui, "빼기", !busy, rm_w).clicked() {
                                        remove = Some(i);
                                    }
                                });
                            }
                        });
                    });
                    list_rect = Some(list_frame.response.rect);
                }
            });
        });
        ui.add_space(gap);

        // 결과 카드: 비었으면 머리줄과 안내 한 줄 높이, 있으면 남은 높이에서 안쪽만 스크롤한다.
        let h = ui.available_height();
        let w = ui.available_width();
        ui.allocate_ui_with_layout(egui::vec2(w, h), egui::Layout::top_down(egui::Align::Min), |ui| {
            egui::Frame::new().fill(theme::CARD).stroke(egui::Stroke::new(1.0, theme::LINE)).corner_radius(egui::CornerRadius::same(8)).inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_height((h - 26.0).max(40.0));
                ui.horizontal(|ui| {
                    ui.label(RichText::new("저장소별 결과").size(15.0).bold());
                    if !ex.outcomes.is_empty() {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let result_text = || ex.outcomes.iter().map(|o| format!("[{}] {}", common::kind_label(&o.kind), o.message)).collect::<Vec<_>>().join("\n");
                            if widgets::tool(ui, "결과 복사", true).clicked() {
                                ui.ctx().copy_text(result_text());
                            }
                            if widgets::tool(ui, "결과 크게 보기", true).clicked() {
                                ex.big = Some(widgets::BigText::new("번들 만들기 결과", result_text()));
                            }
                        });
                    }
                });
                ui.add_space(4.0);
                if ex.outcomes.is_empty() && ex.preview_lines.is_empty() && ex.drop_title.is_empty() {
                    widgets::small(ui, "아직 만들지 않았습니다. 만들면 저장소마다 결과가 이곳에 나옵니다.");
                    return;
                }
                egui::ScrollArea::vertical().id_salt("export_results").auto_shrink([false, false]).show(ui, |ui| {
                    if !ex.drop_title.is_empty() {
                        ui.label(RichText::new(&ex.drop_title).bold());
                        for n in &ex.drop_names {
                            widgets::small(ui, n);
                        }
                        ui.add_space(widgets::GAP_S);
                    }
                    if !ex.preview_lines.is_empty() {
                        widgets::small(ui, "미리보기");
                        widgets::log_box(ui, |ui| {
                            for l in &ex.preview_lines {
                                ui.label(widgets::log_text(l));
                            }
                        });
                        ui.add_space(widgets::GAP_S);
                    }
                    if ex.outcomes.is_empty() {
                        widgets::small(ui, "아직 만들지 않았습니다. 만들면 저장소마다 결과가 이곳에 나옵니다.");
                    } else {
                        common::show_outcomes(ui, &ex.outcomes);
                    }
                    ui.add_space(widgets::GAP_M);
                });
            });
        });
    }

    // Delete 키: 목록 영역 위에 마우스가 있고 글자 입력 중이 아닐 때 [체크 항목 목록에서 빼기]와 같다.
    if st.export.drop_ask.is_none() && !busy && !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.key_pressed(egui::Key::Delete)) {
        let over = match (list_rect, ctx.input(|i| i.pointer.hover_pos())) {
            (Some(r), Some(p)) => r.contains(p),
            _ => false,
        };
        if over {
            want_drop_checked = true;
        }
    }
    if save_checks {
        save_entries(st);
    }
    if let Some(i) = remove {
        remove_at(st, i);
    }
    if want_drop_checked {
        begin_drop_checked(st);
    }
    if want_drop_missing {
        begin_drop_missing(st);
    }
    if want_restore {
        restore_ignored(st, ignored_paths);
    }
    show_drop_confirm(st, &ctx);
    widgets::big_text_window(&ctx, &mut st.export.big);
    for p in adds {
        queue_add(st, &p);
    }
    if let Some(root) = new_root {
        st.save_setting("srcRoot", json!(root));
        want_rescan = true;
    }
    if want_rescan {
        start_scan(st, false);
    }
    if save_dir {
        let v = st.export.out_dir.trim().to_string();
        st.save_setting("exportDir", json!(v));
    }
    if want_refresh {
        start_refresh(st);
    } else if want_make {
        start_make(st);
    } else if want_preview {
        start_preview(st);
    }
}

/// 1.0.5 run_server_refresh: 체크한 저장소(폴더 있는 것만)를 차례로 서버 최신화하고 기록에 남긴다. 중단 요청이면 받는 중인 서버 fetch 는 그 자리에서 끊기고 다음 건부터 멈춘다.
fn start_refresh(st: &mut AppState) {
    let checked = checked_paths(st);
    let repos: Vec<String> = checked.iter().filter(|p| Path::new(p.as_str()).is_dir()).cloned().collect();
    if repos.is_empty() {
        st.notice = "체크한 저장소가 없습니다. 목록에 저장소를 넣고 체크해 주세요.".into();
        return;
    }
    st.spawn("서버 최신화", move |p: &Progress| -> Apply {
        let mut outs: Vec<Outcome> = Vec::new();
        let total = repos.len();
        for (i, repo) in repos.iter().enumerate() {
            if p.cancelled() {
                break;
            }
            p.set(format!("({}/{}) {} 서버 최신화", i + 1, total, repo));
            let r = refresh::refresh(repo);
            common::record(&r, repo, None);
            outs.push(r);
        }
        Box::new(move |st: &mut AppState| {
            st.export.outcomes = outs;
            st.export.drop_title.clear();
            st.export.preview_lines.clear();
            // 그사이 폴더가 없어진 저장소도 표시가 바뀌도록 체크한 저장소를 모두 다시 읽는다.
            st.export.branch_need.extend(checked);
        })
    });
}

/// 저장소마다 고른 작업 브랜치(path_key → 이름).
fn feature_map(st: &AppState) -> HashMap<String, String> {
    st.export
        .repos
        .iter()
        .map(|e| (path_key(entry_path(e)), e.get("featureBranch").and_then(|v| v.as_str()).unwrap_or("").trim().to_string()))
        .collect()
}

/// 체크한 저장소마다 작업 브랜치를 골랐고 이름이 규칙에 맞는지 본다. 아니면 알림을 띄우고 false.
fn check_features(st: &mut AppState, repos: &[String], feats: &HashMap<String, String>) -> bool {
    let name_of = |r: &String| Path::new(r).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| r.clone());
    let none: Vec<String> = repos.iter().filter(|r| feats.get(&path_key(r)).map_or(true, |f| f.is_empty())).map(name_of).collect();
    if !none.is_empty() {
        st.notice = format!("작업 브랜치를 고르지 않은 저장소가 있습니다: {}", none.join(", "));
        return false;
    }
    let bad: Vec<String> = repos.iter().filter(|r| !name_ok(&feats[&path_key(r)])).map(|r| format!("{} ({})", name_of(r), feats[&path_key(r)])).collect();
    if !bad.is_empty() {
        st.notice = format!("브랜치 이름이 규칙에 맞지 않습니다: {}  ({})", bad.join(", "), NAME_RULE_TEXT);
        return false;
    }
    true
}

fn start_make(st: &mut AppState) {
    let repos = checked_paths(st);
    let scope = st.export.scope.key().to_string();
    let feats = feature_map(st);
    let base_text = st.export.base_branch.trim().to_string();
    let out_dir = st.export.out_dir.trim().to_string();
    let full = st.export.full || scope == "feature";
    if repos.is_empty() {
        st.notice = "체크한 저장소가 없습니다. 목록에 저장소를 넣고 체크해 주세요.".into();
        return;
    }
    if out_dir.is_empty() {
        st.notice = "출력 폴더를 골라 주세요.".into();
        return;
    }
    if scope == "feature" && !check_features(st, &repos, &feats) {
        return;
    }
    st.save_setting("exportDir", json!(out_dir.clone()));
    let base: Option<String> = if base_text.is_empty() { None } else { Some(base_text) };
    st.spawn("번들 만들기", move |p: &Progress| -> Apply {
        let mut outs: Vec<Outcome> = Vec::new();
        let total = repos.len();
        for (i, repo) in repos.iter().enumerate() {
            if p.cancelled() {
                break;
            }
            let tag = format!("({}/{}) {}", i + 1, total, repo);
            if scope == "feature" {
                // 1.0.5 와 같게 서버 최신화 없이 지금 로컬 브랜치로 만든다(서버에 못 붙는 PC 에서도 만들 수 있다).
                p.set(format!("{} 번들 만드는 중", tag));
                let feature = feats.get(&path_key(repo)).cloned().unwrap_or_default();
                let o = create::create_feature(repo, &feature, &out_dir, full);
                common::record(&o, repo, o.file.as_deref());
                outs.push(o);
            } else {
                // 1.0.5 와 같게 번들을 만들기 앞에 서버 최신화(origin 추적 참조·기준 브랜치 포함 로컬 브랜치 갱신)를 먼저 한다. 최신화가 멈추면 그 저장소의 번들은 만들지 않는다.
                p.set(format!("{} 서버 최신화 후 번들 만드는 중", tag));
                for o in export::export_refreshed(repo, &out_dir, &scope, base.as_deref(), full) {
                    common::record(&o, repo, o.file.as_deref());
                    outs.push(o);
                }
            }
        }
        // 1.0.5 와 같게, 이번 실행에서 실제로 만든 번들만 모아 2개 이상이면 ZIP 하나로 묶는다(출력 폴더에 예전부터 있던 번들은 담지 않는다).
        // 묶음 줄은 기록(history)에 남기지 않는다.
        let made_files: Vec<String> = outs.iter().filter(|o| o.ok).filter_map(|o| o.file.clone()).filter(|f| !f.is_empty()).collect();
        if made_files.len() >= 2 {
            if p.cancelled() {
                outs.push(Outcome::new(
                    "건너뜀",
                    format!("중단 요청으로 끝나서 묶음 ZIP 은 만들지 않았습니다. 이번에 만든 번들 {}개는 출력 폴더에 낱개로 남아 있습니다.", made_files.len()),
                    true,
                ));
            } else {
                p.set(format!("묶음 ZIP 만드는 중 (번들 {}개)", made_files.len()));
                let zip_name = if scope == "feature" { "feature-bundles" } else { "base-bundles" };
                match archive::create_base_bundle_zip(&made_files, &out_dir, zip_name, true) {
                    Ok(made) if !made.zip.is_empty() => {
                        let zip = made.zip;
                        gitbridge_core::log::say(&format!("===== 묶음 ZIP 만들기 — {}", zip));
                        let mut msg = if made.left.is_empty() {
                            format!("이번에 만든 번들 {}개를 ZIP 하나로 묶고, 낱개 번들은 정리했습니다. 가져갈 파일: {}", made_files.len(), zip)
                        } else {
                            format!("이번에 만든 번들 {}개를 ZIP 하나로 묶었습니다. 가져갈 파일: {}", made_files.len(), zip)
                        };
                        if !made.left.is_empty() {
                            let list: Vec<String> = made.left.iter().map(|(p, e)| format!("{} ({})", p, e)).collect();
                            msg += &format!("
낱개 번들 {}개를 지우지 못했습니다: {}", made.left.len(), list.join(", "));
                        }
                        let mut o = Outcome::new("묶음 ZIP 생성", msg, true);
                        o.file = Some(zip);
                        outs.push(o);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        gitbridge_core::log::say(&format!("===== 묶음 ZIP 실패 — {}", e));
                        outs.push(Outcome::new(
                            "묶음 ZIP 실패",
                            format!("묶음 ZIP 을 만들지 못했습니다. 낱개 번들은 그대로 남아 있습니다. {}", e),
                            false,
                        ));
                    }
                }
            }
        }
        Box::new(move |st: &mut AppState| {
            st.export.outcomes = outs;
            st.export.drop_title.clear();
            st.export.branch_need.extend(repos);
        })
    });
}

fn start_preview(st: &mut AppState) {
    let repos = checked_paths(st);
    let scope = st.export.scope.key().to_string();
    let feats = feature_map(st);
    let base_text = st.export.base_branch.trim().to_string();
    let full = st.export.full || scope == "feature";
    if repos.is_empty() {
        st.notice = "체크한 저장소가 없습니다. 목록에 저장소를 넣고 체크해 주세요.".into();
        return;
    }
    if scope == "feature" && !check_features(st, &repos, &feats) {
        return;
    }
    let base: Option<String> = if base_text.is_empty() { None } else { Some(base_text) };
    st.spawn("미리보기", move |p: &Progress| -> Apply {
        let mut lines: Vec<String> = Vec::new();
        let total = repos.len();
        for (i, repo) in repos.iter().enumerate() {
            if p.cancelled() {
                break;
            }
            p.set(format!("미리보기 ({}/{}) {}", i + 1, total, repo));
            lines.push(format!("[{}]", repo));
            let feature = feats.get(&path_key(repo)).cloned().unwrap_or_default();
            let br: Option<&str> = if scope == "feature" { Some(feature.as_str()) } else { base.as_deref() };
            match export_preview::preview(repo, &scope, br, full) {
                Ok(v) => lines.extend(v.into_iter().map(|l| format!("  {}", l))),
                Err(e) => lines.push(format!("  미리보기 실패: {}", e)),
            }
            lines.push(String::new());
        }
        Box::new(move |st: &mut AppState| {
            st.export.preview_lines = lines;
            st.export.drop_title.clear();
        })
    });
}
