//! 받은 번들 폴더를 차례로 처리한다(1.0.5 queue.py).
//!
//! 화면 없는 CLI 용이다. 1.0.5 의 BundleQueue 에서 add_inputs(폴더 불러오기, 이미 적용한 번들은 건너뜀)와
//! apply(위에서부터 적용, "기대는 커밋이 없다"는 뒤 번들이 들어간 뒤 다시 시도)만 옮겼다.

use crate::archive;
use crate::gitrun::{wsl_location, Git};
use crate::refs::{get_bundle_head, get_bundle_prereqs, norm_path, repo_label_read, BundleHead, Outcome};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const NEED_PREREQ_TEXT: &str = "기대는 커밋이 저장소에 없다";
const SKIP_DIRS: [&str; 15] = [
    "node_modules", ".git", "build", "target", "out", "bin", "obj", ".idea", ".gradle", "dist", ".next", "vendor", ".venv", "venv", ".tox",
];
const CREATE_KINDS: [&str; 7] = [
    "번들 생성",
    "기준 번들 생성",
    "보낼 커밋 없음",
    "main 번들 생성",
    "주요 참조 번들 생성",
    "주요 참조 증분 번들 생성",
    "전체 저장소 번들 생성",
];

#[derive(Debug, Clone, Default)]
struct Row {
    path: String,
    head: Option<BundleHead>,
    prereqs: Vec<String>,
    repo: String,
    note: String,
    sha: String,
    seen: String,
    status: String,
    create_new: bool,
    auto_repo: bool,
    /// ZIP 에서 푼 번들이면 원래 ZIP 경로.
    archive: String,
    /// ZIP 을 푼 임시 폴더(낱개 .bundle 이면 빈 문자열).
    temp_dir: String,
    /// 이 줄만 '전체 동기화'로 적용한다(파일 목록 모드에서 plan 줄이 정한다).
    sync_all: bool,
    result: Option<Outcome>,
}

#[derive(Debug, Clone)]
struct Candidate {
    path: String,
    label: String,
}

fn first_line(s: &str) -> String {
    s.split('\n').next().unwrap_or("").to_string()
}

fn path_key(p: &str) -> String {
    if p.is_empty() {
        String::new()
    } else {
        norm_path(p)
    }
}

fn base_name(p: &str) -> String {
    Path::new(p).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

fn file_sha256(path: &str) -> R<String> {
    let mut f = fs::File::open(path).map_err(|e| format!("번들 파일을 열지 못했습니다: {} ({})", path, e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("번들 파일을 읽지 못했습니다: {}", e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{:02x}", b)).collect())
}

use crate::R;

// ---------------------------------------------------------------- 이름 규칙

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// "yyyyMMdd-HHmm-" 으로 시작하면 그 뒤. 아니면 None.
fn strip_stamp(name: &str) -> Option<&str> {
    let b = name.as_bytes();
    if b.len() < 14 || !b[..8].iter().all(|c| c.is_ascii_digit()) || b[8] != b'-' || !b[9..13].iter().all(|c| c.is_ascii_digit()) || b[13] != b'-' {
        return None;
    }
    Some(&name[14..])
}

/// ".bundle" 또는 "-<숫자>.bundle" 로 끝나는 꼬리인지.
fn seq_tail_ok(rest: &str) -> bool {
    if rest.eq_ignore_ascii_case(".bundle") {
        return true;
    }
    match rest.strip_prefix('-') {
        Some(r) if r.len() > 7 && r.is_char_boundary(r.len() - 7) => {
            let (digits, ext) = r.split_at(r.len() - 7);
            all_digits(digits) && ext.eq_ignore_ascii_case(".bundle")
        }
        _ => false,
    }
}

/// 만들기 쪽 파일 이름 <yyyyMMdd-HHmm>-<라벨>-<브랜치>[-순번].bundle 과 통째로 맞는지.
fn name_matches(file_name: &str, label: &str, branch: &str, case_sensitive: bool) -> bool {
    let Some(rest) = strip_stamp(file_name) else { return false };
    let want = format!("{}-{}", label, branch);
    if case_sensitive {
        match rest.strip_prefix(want.as_str()) {
            Some(tail) => seq_tail_ok(tail),
            None => false,
        }
    } else {
        let (r, w) = (rest.to_lowercase(), want.to_lowercase());
        match r.strip_prefix(w.as_str()) {
            Some(tail) => seq_tail_ok(tail),
            None => false,
        }
    }
}

fn name_suffixes(head: &BundleHead) -> Vec<String> {
    if head.kind == "repository" {
        return vec!["all-refs".into(), "base-develop-tags".into()];
    }
    if head.kind == "base" {
        return vec![head.branch.clone()];
    }
    vec![head.branch.replace('/', "_")]
}

fn bundle_label(bundle_path: &str, head: &BundleHead) -> String {
    if !head.label.is_empty() {
        return head.label.clone();
    }
    if head.kind != "base" {
        return String::new();
    }
    let name = base_name(bundle_path);
    let Some(rest) = strip_stamp(&name) else { return String::new() };
    let chars: Vec<char> = rest.chars().collect();
    let lower = |c: &[char]| c.iter().collect::<String>().to_lowercase();
    // 끝의 ".bundle"
    if chars.len() < 7 || lower(&chars[chars.len() - 7..]) != ".bundle" {
        return String::new();
    }
    let stem: Vec<char> = chars[..chars.len() - 7].to_vec();
    let want = format!("-{}", head.branch.to_lowercase());
    let want_n = format!("-{}", head.branch).chars().count();
    let try_end = |s: &[char]| -> Option<String> {
        if s.len() > want_n && lower(&s[s.len() - want_n..]) == want {
            Some(s[..s.len() - want_n].iter().collect())
        } else {
            None
        }
    };
    if let Some(l) = try_end(&stem) {
        return l;
    }
    // 끝의 "-<숫자>"
    let mut i = stem.len();
    while i > 0 && stem[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i < stem.len() && i > 0 && stem[i - 1] == '-' {
        if let Some(l) = try_end(&stem[..i - 1]) {
            return l;
        }
    }
    String::new()
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 일반 번들의 파일명은 새 폴더 제안에만 쓴다. 기존 저장소의 신원으로 쓰지 않는다.
fn new_repo_label(bundle_path: &str, head: &BundleHead) -> String {
    let label = bundle_label(bundle_path, head);
    if !label.is_empty() || !head.native {
        return label;
    }
    let file = base_name(bundle_path);
    let mut name = match file.rfind('.') {
        Some(i) if i > 0 => file[..i].to_string(),
        _ => file.clone(),
    };
    // 끝의 -yyyyMMdd 또는 -yyyyMMdd-HHmm
    let strip_date = |s: &str| -> Option<String> {
        let b = s.as_bytes();
        if b.len() >= 9 && b[b.len() - 9] == b'-' && all_digits(&s[s.len() - 8..]) && s.is_char_boundary(s.len() - 9) {
            return Some(s[..s.len() - 9].to_string());
        }
        None
    };
    if name.len() >= 14 && name.is_char_boundary(name.len() - 5) {
        let (head_part, tail) = name.split_at(name.len() - 5);
        if tail.starts_with('-') && all_digits(&tail[1..]) {
            if let Some(s) = strip_date(head_part) {
                name = s;
            }
        } else if let Some(s) = strip_date(&name) {
            name = s;
        }
    } else if let Some(s) = strip_date(&name) {
        name = s;
    }
    let name: String = name.chars().map(|c| if is_word(c) || c == '.' || c == '-' { c } else { '_' }).collect();
    let name = name.trim_matches(|c| c == ' ' || c == '.').to_string();
    if name.is_empty() || name == "." || name == ".." {
        return String::new();
    }
    let up = name.to_uppercase();
    let stem = up.split('.').next().unwrap_or("");
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem)
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        return format!("_{}", name);
    }
    name
}

/// 라벨 한 칸이 폴더 이름으로 안전한지(상위로 빠지거나 뿌리 자체가 되지 않는지).
fn single_component(label: &str) -> bool {
    let mut it = Path::new(label).components();
    matches!((it.next(), it.next()), (Some(std::path::Component::Normal(_)), None))
}

// ---------------------------------------------------------------- 설정·후보

fn settings_str(settings: &Value, key: &str) -> String {
    settings.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn repo_map(settings: &Value) -> Vec<(String, String)> {
    settings
        .get("repoMap")
        .and_then(|v| v.as_object())
        .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
        .unwrap_or_default()
}

fn is_dir(p: &str) -> bool {
    !p.is_empty() && Path::new(p).is_dir()
}

fn trim_sep(p: &str) -> String {
    let t = p.trim_end_matches(['\\', '/']);
    if t.is_empty() || (t.len() == 2 && t.ends_with(':')) {
        p.to_string()
    } else {
        t.to_string()
    }
}

/// .git 이 있는 폴더를 찾는다. 찾은 폴더 안으로는 더 내려가지 않는다. 빌드·의존성 폴더는 건너뛴다.
fn find_git_repos(root: &str) -> Vec<String> {
    let depth = 5;
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
        let Ok(rd) = fs::read_dir(&d) else { continue };
        let mut subs: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir() && !t.is_symlink()).unwrap_or(false))
            .map(|e| e.path())
            .collect();
        subs.sort_by_key(|p| p.to_string_lossy().to_uppercase());
        for s in subs {
            let n = base_name(&s.to_string_lossy()).to_lowercase();
            if SKIP_DIRS.contains(&n.as_str()) || n == "__pycache__" {
                continue;
            }
            queue.push_back((s, lvl + 1));
        }
    }
    found
}

fn repo_candidate_paths(settings: &Value, extra: &[String]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    if let Some(arr) = settings.get("repos").and_then(|v| v.as_array()) {
        for e in arr {
            if let Some(s) = e.as_str() {
                paths.push(s.to_string());
            } else if let Some(s) = e.get("path").and_then(|v| v.as_str()) {
                paths.push(s.to_string());
            }
        }
    }
    paths.push(settings_str(settings, "repo"));
    paths.extend(repo_map(settings).into_iter().map(|x| x.1));
    if let Some(arr) = settings.get("repoCachePaths").and_then(|v| v.as_array()) {
        paths.extend(arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())));
    }
    paths.extend(extra.iter().cloned());
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in paths {
        if p.is_empty() || !is_dir(&p) || !seen.insert(path_key(&p)) {
            continue;
        }
        out.push(trim_sep(&p));
    }
    out
}

/// 저장소 경로(path_key) → 읽은 라벨. 프로세스 동안 기억해 번들을 넣을 때마다 후보마다 git 을 다시 띄우지 않는다.
/// 후보 짝짓기만 이 기억을 쓰고, 적용 직전 검사와 적용 뒤 짝 기억은 repo_label_fresh 로 새로 읽는다.
static LABELS: std::sync::Mutex<Option<HashMap<String, String>>> = std::sync::Mutex::new(None);

fn repo_label(path: &str) -> String {
    if let Some(l) = LABELS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&path_key(path)).cloned()) {
        return l;
    }
    // 처음 보는 저장소는 .git\config 에서 바로 읽는다. 후보마다 git 을 띄우면 저장소 20여 개에 3초쯤 걸려서다.
    // 파일로 확신할 수 없는 모양(WSL 경로·포함 설정·주소 바꾸기·따옴표 값 등)이면 git 으로 읽는다.
    if let Some(l) = crate::refs::repo_label_from_files(path) {
        LABELS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(path_key(path), l.clone());
        return l;
    }
    repo_label_fresh(path)
}

/// git 으로 새로 읽어 저장소로 읽었을 때만 기억한다. 폴더 이름으로 대신했거나 읽기에 실패하면 옛 기억을 지운다.
fn repo_label_fresh(path: &str) -> String {
    let key = path_key(path);
    let read = Git::open(path).and_then(|g| repo_label_read(&g));
    let mut labels = LABELS.lock().unwrap_or_else(|e| e.into_inner());
    match read {
        Ok((l, true)) if !l.is_empty() => {
            labels.get_or_insert_with(HashMap::new).insert(key, l.clone());
            l
        }
        other => {
            if let Some(m) = labels.as_mut() {
                m.remove(&key);
            }
            match other {
                Ok((l, _)) if !l.is_empty() => l,
                _ => base_name(path).chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' }).collect(),
            }
        }
    }
}

fn applied_sha_map(history: &[Value]) -> HashMap<String, String> {
    // 번들 내용(SHA-256) → 마지막으로 성공한 적용 시각. 만들기 기록은 적용이 아니다. history 는 최신이 앞.
    let mut m = HashMap::new();
    for rec in history {
        let sha = rec.get("bundleSha").and_then(|v| v.as_str()).unwrap_or("");
        let kind = rec.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        if rec.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) && !sha.is_empty() && !CREATE_KINDS.contains(&kind) && !m.contains_key(sha) {
            m.insert(sha.to_string(), rec.get("time").and_then(|v| v.as_str()).unwrap_or("").to_string());
        }
    }
    m
}

fn has_commit_raw(repo: &str, oid: &str) -> bool {
    Git::open(repo).and_then(|g| g.q(&["cat-file", "-e", &format!("{}^{{commit}}", oid)])).map(|r| r.ok()).unwrap_or(false)
}

struct Matcher<'a> {
    cands: &'a [Candidate],
    history: &'a [Value],
    settings: &'a Value,
    cache: HashMap<(String, String), bool>,
}

impl Matcher<'_> {
    fn has_commit(&mut self, repo: &str, oid: &str) -> bool {
        let key = (path_key(repo), oid.to_string());
        if let Some(v) = self.cache.get(&key) {
            return *v;
        }
        let v = has_commit_raw(repo, oid);
        self.cache.insert(key, v);
        v
    }
}

fn label_matches(c: &Candidate, label: &str) -> bool {
    if wsl_location(&c.path).is_some() {
        c.label == label
    } else {
        c.label.to_lowercase() == label.to_lowercase()
    }
}

fn candidate_name_matches(file_name: &str, c: &Candidate, branch: &str) -> bool {
    name_matches(file_name, &c.label, branch, wsl_location(&c.path).is_some())
}

/// 기억한 짝에서 라벨이 정확히 같은 것. WSL 경로만 대소문자를 구별한다.
fn remembered_repo(rmap: &[(String, String)], label: &str) -> String {
    if let Some((_, v)) = rmap.iter().find(|(k, _)| k == label) {
        if is_dir(v) {
            return v.clone();
        }
    }
    for (k, v) in rmap {
        if wsl_location(v).is_none() && k.to_lowercase() == label.to_lowercase() && is_dir(v) {
            return v.clone();
        }
    }
    String::new()
}

/// 이력 기록의 번들 이름이 같은 라벨의 번들이었는지.
fn record_label_matches(rec: &Value, label: &str, case_sensitive: bool) -> bool {
    let name = rec.get("bundleName").and_then(|v| v.as_str()).unwrap_or("");
    let branch = rec.get("branch").and_then(|v| v.as_str()).unwrap_or("");
    let mut alts = vec!["all-refs".to_string(), "base-develop-tags".to_string()];
    if !branch.is_empty() {
        alts.push(branch.to_string());
        alts.push(branch.replace('/', "_"));
    }
    alts.iter().any(|a| name_matches(name, label, a, case_sensitive))
}

/// (repo, note). 먼저 기억한 짝(repoMap: origin 이름 → 경로). 그다음 라벨이 정확히 같은 저장소,
/// 라벨이 없으면 고정된 파일 이름 모양, 그래도 없으면 기대는 커밋을 가진 저장소.
/// 후보가 여럿이면 기억한 경로, 같은 라벨 번들을 적용한 이력, 최근 저장소 순으로 고르고, 그래도 여럿이면 비워 둔다.
fn resolve_bundle_repo(bundle_path: &str, head: &BundleHead, prereqs: &[String], m: &mut Matcher) -> (String, String) {
    let name = base_name(bundle_path);
    let rmap = repo_map(m.settings);
    let mut hits: Vec<Candidate> = Vec::new();
    let label = bundle_label(bundle_path, head);
    if !label.is_empty() {
        let remembered = remembered_repo(&rmap, &label);
        if !remembered.is_empty() {
            return (remembered, String::new());
        }
        hits = m.cands.iter().filter(|c| label_matches(c, &label)).cloned().collect();
    } else if (head.kind == "base" || head.kind == "repository") && !head.native {
        let suffixes = name_suffixes(head);
        let mut keys: Vec<&(String, String)> = rmap.iter().collect();
        keys.sort_by_key(|(k, _)| std::cmp::Reverse(k.chars().count()));
        for (k, v) in keys {
            let c = Candidate { path: v.clone(), label: k.clone() };
            if suffixes.iter().any(|s| candidate_name_matches(&name, &c, s)) && is_dir(v) {
                return (v.clone(), String::new());
            }
        }
        hits = m.cands.iter().filter(|c| suffixes.iter().any(|s| candidate_name_matches(&name, c, s))).cloned().collect();
    }
    if hits.is_empty() && !prereqs.is_empty() {
        let cands = m.cands.to_vec();
        hits = cands.into_iter().filter(|c| m.has_commit(&c.path, &prereqs[0])).collect();
    }
    if hits.len() > 1 {
        let values: Vec<String> = rmap.iter().map(|(_, v)| path_key(v)).collect();
        let remembered: Vec<Candidate> = hits.iter().filter(|c| values.contains(&path_key(&c.path))).cloned().collect();
        if !remembered.is_empty() {
            hits = remembered;
        }
    }
    if hits.len() == 1 {
        return (hits[0].path.clone(), String::new());
    }
    if hits.is_empty() {
        if (head.kind == "base" || head.kind == "repository") && prereqs.is_empty() {
            return (String::new(), "맞는 저장소가 없습니다 — 기존 저장소를 고르거나 [새 저장소 만들기…]를 써 주세요".into());
        }
        return (String::new(), "맞는 저장소를 못 찾았습니다 — [저장소 바꾸기…]로 골라 주세요".into());
    }
    // 기준 브랜치 이름은 저장소마다 같으므로, 이력은 같은 라벨의 번들을 적용한 기록만 본다.
    // 라벨 없는 옛 feature 번들만 브랜치 이름으로 본다.
    for rec in m.history {
        let ok = rec.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        let rrepo = rec.get("repo").and_then(|v| v.as_str()).unwrap_or("");
        if !(ok && !rrepo.is_empty()) {
            continue;
        }
        for c in &hits {
            if path_key(&c.path) != path_key(rrepo) {
                continue;
            }
            if !label.is_empty() {
                if record_label_matches(rec, &label, wsl_location(&c.path).is_some()) {
                    return (c.path.clone(), String::new());
                }
            } else if head.kind == "feature" && rec.get("branch").and_then(|v| v.as_str()) == Some(head.branch.as_str()) {
                return (c.path.clone(), String::new());
            }
        }
    }
    let cur = path_key(&settings_str(m.settings, "repo"));
    if let Some(c) = hits.iter().find(|c| path_key(&c.path) == cur) {
        return (c.path.clone(), String::new());
    }
    (String::new(), format!("맞는 저장소가 {}곳입니다 — [저장소 바꾸기…]로 골라 주세요", hits.len()))
}

/// 같은 이름이 겹쳐 붙은 "-2" 순번이 원래 파일 뒤로 가게 한다.
fn bundle_sort_key(bundle_path: &str) -> String {
    let file = base_name(bundle_path);
    let mut n = match file.rfind('.') {
        Some(i) if i > 0 => file[..i].to_string(),
        _ => file.clone(),
    };
    let mut seq = 1u32;
    if let Some(i) = n.rfind('-') {
        let tail = &n[i + 1..];
        if (1..=2).contains(&tail.len()) && all_digits(tail) {
            let stem = n[..i].to_string();
            let dir = Path::new(bundle_path).parent().map(|p| p.to_path_buf()).unwrap_or_default();
            if dir.join(format!("{}.bundle", stem)).exists() {
                seq = tail.parse().unwrap_or(1);
                n = stem;
            }
        }
    }
    format!("{}|{:03}", n, seq)
}

fn set_row_idle(row: &mut Row) {
    // 아직 실행하지 않은 줄의 상태 글
    if row.head.is_none() {
        return;
    }
    row.result = None;
    row.status = if !row.seen.is_empty() {
        format!("전에 적용함 ({})", row.seen)
    } else if !row.repo.is_empty() {
        "대기".to_string()
    } else {
        row.note.clone()
    };
}

fn head_oid_in(rows: &[Row], i: usize, prereqs: &[String]) -> Option<usize> {
    (0..rows.len()).find(|&j| j != i && rows[j].head.as_ref().map(|h| prereqs.contains(&h.oid)).unwrap_or(false))
}

/// 저장소가 빈 줄을 짝짓는다. 앞 번들 끝 커밋에 기대는 번들(같은 브랜치의 연속 번들)은 앞 번들과 같은 저장소로.
/// 독립 복원 가능한 완전본인데 맞는 저장소가 없으면 저장소 모음 폴더 아래에 원래 이름으로 자동 생성한다.
fn resolve_queue_repos(rows: &mut [Row], cands: &[Candidate], history: &[Value], settings: &Value) {
    let mut m = Matcher { cands, history, settings, cache: HashMap::new() };
    let resolve = |rows: &mut [Row], i: usize, m: &mut Matcher| {
        let Some(head) = rows[i].head.clone() else { return };
        let (repo, note) = resolve_bundle_repo(&rows[i].path, &head, &rows[i].prereqs.clone(), m);
        rows[i].auto_repo = !repo.is_empty();
        rows[i].repo = repo;
        rows[i].note = note;
    };
    // 앞 번들 끝 커밋에 기대는 줄은 아직 어느 저장소에도 그 커밋이 없으니, 앞 번들 연결을 먼저 해 본다.
    let mut deferred = Vec::new();
    for i in 0..rows.len() {
        if rows[i].head.is_none() || !rows[i].repo.is_empty() {
            continue;
        }
        if !rows[i].prereqs.is_empty() && head_oid_in(rows, i, &rows[i].prereqs).is_some() {
            deferred.push(i);
            continue;
        }
        resolve(rows, i, &mut m);
    }
    fn link_chained(rows: &mut [Row]) {
        let mut changed = true;
        while changed {
            changed = false;
            for i in 0..rows.len() {
                if rows[i].head.is_none() || !rows[i].repo.is_empty() || rows[i].prereqs.is_empty() {
                    continue;
                }
                let prereqs = rows[i].prereqs.clone();
                let parent = (0..rows.len()).find(|&j| j != i && rows[j].head.as_ref().map(|h| prereqs.contains(&h.oid)).unwrap_or(false) && !rows[j].repo.is_empty());
                if let Some(j) = parent {
                    rows[i].repo = rows[j].repo.clone();
                    rows[i].auto_repo = true;
                    rows[i].note.clear();
                    changed = true;
                }
            }
        }
    }
    link_chained(rows);
    let left: Vec<usize> = deferred.into_iter().filter(|&i| rows[i].repo.is_empty()).collect();
    if !left.is_empty() {
        for i in left {
            resolve(rows, i, &mut m);
        }
        link_chained(rows);
    }
    let root = settings_str(settings, "srcRoot");
    if root.is_empty() || !is_dir(&root) {
        return;
    }
    let root = trim_sep(&root);
    for row in rows.iter_mut() {
        let Some(head) = row.head.clone() else { continue };
        if !row.repo.is_empty() || !(head.kind == "base" || head.kind == "repository") || !row.prereqs.is_empty() {
            continue;
        }
        let label = new_repo_label(&row.path, &head);
        if label.is_empty() {
            continue;
        }
        if !single_component(&label) {
            row.note = "번들의 저장소 이름으로 안전한 생성 경로를 만들 수 없습니다.".into();
            continue;
        }
        let dest = Path::new(&root).join(&label);
        let dest_s = dest.to_string_lossy().to_string();
        if dest.exists() {
            row.note = format!(
                "{}{}",
                if head.native {
                    "같은 이름의 폴더가 있어 새로 만들지 않았습니다. 기존 저장소라면 [저장소 바꾸기…]로 선택해 주세요: "
                } else {
                    "같은 이름의 폴더가 있지만 git 저장소가 아닙니다 — 폴더를 확인해 주세요: "
                },
                dest_s
            );
            continue;
        }
        row.repo = dest_s;
        row.create_new = true;
        row.auto_repo = true;
        row.note.clear();
    }
}

/// 같은 브랜치의 연속 번들은 앞 번들 뒤로 순서를 맞춘다.
fn order_chained(rows: &mut Vec<Row>) {
    let mut guard = 0;
    let mut moved = true;
    while moved && guard < 200 {
        moved = false;
        guard += 1;
        for r_idx in 0..rows.len() {
            if rows[r_idx].head.is_none() || rows[r_idx].prereqs.is_empty() {
                continue;
            }
            let prereqs = rows[r_idx].prereqs.clone();
            let Some(p_idx) = head_oid_in(rows, r_idx, &prereqs) else { continue };
            if r_idx < p_idx {
                let row = rows.remove(r_idx);
                rows.insert(p_idx, row);
                moved = true;
                break;
            }
        }
    }
}

fn record(row: &Row, o: &Outcome) {
    // ZIP 에서 푼 번들은 임시 경로 대신 "ZIP 경로 > 안의 이름"을 남긴다(1.0.5 와 같다).
    let shown = if row.archive.is_empty() { row.path.clone() } else { format!("{} > {}", row.archive, base_name(&row.path)) };
    if let Err(e) = crate::store::append_history_shown(o, &row.repo, Some(&row.path), &shown) {
        crate::log::say(&format!("기록을 남기지 못했습니다: {}", e));
    }
}

/// dir 의 번들(·ZIP)을 src_root 아래 저장소와 짝짓는다. apply 가 거짓이면 계획만 보고한다.
pub fn run_queue(dir: &str, src_root: &str, apply: bool) -> Vec<Outcome> {
    let c = AtomicBool::new(false);
    run_queue_cancel(dir, src_root, apply, &c)
}

/// run_queue 와 같되, cancel 이 참이 되면 아직 적용하지 않은 번들은 적용하지 않고 '취소로 보류'로 남긴다.
pub fn run_queue_cancel(dir: &str, src_root: &str, apply: bool, cancel: &std::sync::atomic::AtomicBool) -> Vec<Outcome> {
    if !Path::new(dir).is_dir() {
        return vec![Outcome::stop(format!("번들 폴더를 찾을 수 없습니다: {}", dir))];
    }
    run_paths_cancel(&[dir.to_string()], src_root, apply, cancel)
}

/// 폴더 하나 대신 경로 목록(폴더·.bundle·.zip 섞어서)을 받는 같은 처리. ZIP 은 풀고 번들마다 저장소를 자동으로 짝짓는다.
pub fn run_paths_cancel(paths: &[String], src_root: &str, apply: bool, cancel: &std::sync::atomic::AtomicBool) -> Vec<Outcome> {
    let mut settings = match crate::store::load_settings() {
        Ok(s) => s,
        Err(e) => return vec![Outcome::stop(e)],
    };
    let history = match crate::store::read_history(500) {
        Ok(h) => h,
        Err(e) => return vec![Outcome::stop(e)],
    };
    let temp_root = match archive::import_temp_dir() {
        Ok(t) => t,
        Err(e) => return vec![Outcome::stop(e)],
    };
    let mut out: Vec<Outcome> = Vec::new();
    let expanded = archive::expand_inputs(paths, &temp_root);
    let result = run_rows(&expanded, src_root, apply, &mut settings, &history, &mut out, cancel);
    // 한 번 실행으로 끝나므로 풀어 둔 임시 폴더는 모두 치운다.
    for d in &expanded.temp_dirs {
        archive::cleanup_import_dir(d, &temp_root);
    }
    if let Err(e) = result {
        out.push(Outcome::stop(e));
    }
    out
}

/// 명령줄 --src-root 는 이번 실행에서만 쓰고 설정에 넣지 않는다. 자동 생성 위치에도 같은 값을 쓴다.
/// (짝짓기에 쓸 설정 사본, 저장소 후보)
fn effective_cands(settings: &Value, src_root: &str) -> (Value, Vec<Candidate>) {
    let use_root = !src_root.is_empty();
    let mut eff = settings.clone();
    if use_root {
        eff["srcRoot"] = json!(src_root);
    }
    let extra = if use_root && is_dir(src_root) { find_git_repos(src_root) } else { Vec::new() };
    let cands: Vec<Candidate> = repo_candidate_paths(&eff, &extra).into_iter().map(|p| Candidate { label: repo_label(&p), path: p }).collect();
    (eff, cands)
}

fn zip_error_outcome(path: &str, message: &str) -> Outcome {
    Outcome::stop(format!("ZIP 읽기 실패 — {}: {}", base_name(path), message))
}

/// 번들을 줄로 만든다(짝짓기 전). 이미 있는 경로(known)·같은 내용(known_sha)은 한 번만 넣는다.
/// 전에 적용한 번들은 include_seen 이 거짓이면 넣지 않고, 참이면 seen 을 채워 넣는다.
/// 읽기 실패는 out 에 '멈춤'으로 남긴다. 돌려주는 값: (줄, 건너뛴 수, 같은 번들 수).
fn read_rows(
    bundles: &[archive::BundleInput],
    include_seen: bool,
    applied: &HashMap<String, String>,
    known: &mut HashSet<String>,
    known_sha: &mut HashSet<String>,
    out: &mut Vec<Outcome>,
) -> (Vec<Row>, usize, usize) {
    let mut items: Vec<(&archive::BundleInput, std::time::SystemTime, String)> = Vec::new();
    let mut dup_n = 0;
    for b in bundles {
        if !known.insert(path_key(&b.path)) {
            dup_n += 1;
            continue;
        }
        let mtime = fs::metadata(&b.path).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        items.push((b, mtime, bundle_sort_key(&b.path).to_lowercase()));
    }
    items.sort_by(|a, b| (a.1, &a.2).cmp(&(b.1, &b.2)));
    let mut rows: Vec<Row> = Vec::new();
    let mut skip_n = 0;
    for (b, _, _) in items {
        let mut row = Row { path: b.path.clone(), archive: b.archive.clone(), temp_dir: b.temp_dir.clone(), ..Default::default() };
        let built = (|| -> R<bool> {
            let sha = file_sha256(&b.path)?;
            row.sha = sha.clone();
            if !known_sha.insert(sha.clone()) {
                dup_n += 1;
                return Ok(false);
            }
            if let Some(t) = applied.get(&sha) {
                if !include_seen {
                    skip_n += 1;
                    return Ok(false);
                }
                row.seen = t.clone();
            }
            row.head = Some(get_bundle_head(&b.path)?);
            row.prereqs = get_bundle_prereqs(&b.path)?;
            Ok(true)
        })();
        match built {
            Ok(true) => rows.push(row),
            Ok(false) => {}
            Err(e) => {
                row.head = None;
                row.status = first_line(&e);
                out.push(Outcome::stop(format!("읽기 실패 — {}: {}", base_name(&row.path), row.status)));
            }
        }
    }
    (rows, skip_n, dup_n)
}

/// 줄 만들기 + 짝짓기 + 연속 번들 순서 + 상태 글. 전에 적용한 번들은 include_seen 이 거짓이면 넣지 않고 건너뜀 결과를 낸다.
fn build_rows(expanded: &archive::ExpandedInputs, src_root: &str, include_seen: bool, settings: &Value, history: &[Value], out: &mut Vec<Outcome>) -> Vec<Row> {
    let (eff, cands) = effective_cands(settings, src_root);

    for (path, message) in &expanded.errors {
        out.push(zip_error_outcome(path, message));
    }

    let applied = applied_sha_map(history);
    let mut known: HashSet<String> = HashSet::new();
    let mut known_sha: HashSet<String> = HashSet::new();
    let (mut rows, skip_n, dup_n) = read_rows(&expanded.bundles, include_seen, &applied, &mut known, &mut known_sha, out);
    if skip_n > 0 || dup_n > 0 {
        let mut msg = format!("이미 적용한 번들 {}개는 건너뛰었습니다.", skip_n);
        if dup_n > 0 {
            msg += &format!(" 같은 번들 {}개는 한 번만 봤습니다.", dup_n);
        }
        out.push(Outcome::new("건너뜀", msg, true));
    }

    resolve_queue_repos(&mut rows, &cands, history, &eff);
    order_chained(&mut rows);
    for row in rows.iter_mut() {
        set_row_idle(row);
    }
    rows
}

fn run_rows(expanded: &archive::ExpandedInputs, src_root: &str, apply: bool, settings: &mut Value, history: &[Value], out: &mut Vec<Outcome>, cancel: &AtomicBool) -> R<()> {
    let mut rows = build_rows(expanded, src_root, false, settings, history, out);
    let pending: Vec<usize> = (0..rows.len()).filter(|&i| !rows[i].repo.is_empty()).collect();
    let first_new = out.len();
    run_apply_rows(&mut rows, pending, apply, settings, out, cancel, &mut |_| {});
    // 결과의 '파일' 은 임시 폴더 경로 대신 "ZIP > 번들 이름" 으로 보인다(폴더 큐·CLI 경로).
    for o in out[first_new..].iter_mut() {
        if let Some(r) = rows.iter().find(|r| !r.archive.is_empty() && o.file.as_deref() == Some(r.path.as_str())) {
            o.file = Some(shown_bundle_name(&r.archive, &r.path));
        }
    }
    Ok(())
}

/// 결과 첫 줄에 붙이는 머리. 어느 로컬 폴더에 대고 판정했는지 보인다.
const REPO_LINE: &str = "판정한 저장소 폴더: ";

/// 짝지은 저장소가 이 번들의 저장소가 아닌 것 같으면 그 이유를 돌려준다(적용 전에 먼저 막는다).
/// 조건: 번들이 저장소 이름(라벨)을 알고, 폴더의 저장소 이름이 다르고, 번들이 기대는 커밋이 이 폴더에 하나도 없으며
/// 같은 목록의 앞 번들이 그 커밋을 만들어 주지도 않을 때. 이 경우를 '앞선 번들이 먼저 필요'로 안내하면 틀린 길로 이끈다.
fn repo_fit_problem(rows: &[Row], i: usize) -> Option<String> {
    let row = &rows[i];
    let head = row.head.as_ref()?;
    if row.repo.is_empty() || row.create_new || row.prereqs.is_empty() || !is_dir(&row.repo) {
        return None;
    }
    let label = bundle_label(&row.path, head);
    if label.is_empty() {
        return None;
    }
    let repo_name = repo_label_fresh(&row.repo);
    if repo_name.to_lowercase() == label.to_lowercase() {
        return None;
    }
    if head_oid_in(rows, i, &row.prereqs).is_some() {
        return None;
    }
    if row.prereqs.iter().any(|p| has_commit_raw(&row.repo, p)) {
        return None;
    }
    let short: Vec<String> = row.prereqs.iter().take(3).map(|p| p.chars().take(8).collect()).collect();
    Some(format!(
        "다른 저장소 같습니다 — 이 번들은 '{}' 저장소(기본 브랜치 {})용인데, 고른 폴더 {} 는 '{}' 저장소이고 번들이 기대는 커밋({})이 하나도 없습니다. 적용하지 않았습니다. [저장소 바꾸기…]로 맞는 저장소를 골라 주세요.",
        label,
        if head.branch.is_empty() { "?" } else { head.branch.as_str() },
        row.repo,
        repo_name,
        short.join(", ")
    ))
}

/// pending(rows 의 색인) 줄을 위에서부터 적용하고(apply 가 참일 때), 모든 줄의 결과를 rows 순서로 out 에 붙인다.
/// 앞 번들이 아직 안 들어가 "기대는 커밋이 없다"로 멈춘 줄은 재시도하고, 취소되면 남은 줄은 '취소로 보류'로 남긴다.
/// 적용 중 줄 하나마다 보내는 진행 알림.
pub enum RunEvent<'a> {
    /// pos(0부터, 이번 실행 줄 순서)번째 줄의 적용을 시작한다. file 은 줄의 번들 경로.
    Start { file: &'a str, name: &'a str, repo: &'a str, pos: usize, total: usize },
    /// 한 줄의 결과. retry 가 참이면 앞 번들을 기다리느라 뒤에서 한 번 더 시도한다.
    Done { outcome: &'a Outcome, retry: bool },
}

fn run_apply_rows(rows: &mut [Row], mut pending: Vec<usize>, apply: bool, settings: &mut Value, out: &mut Vec<Outcome>, cancel: &AtomicBool, on_event: &mut dyn FnMut(RunEvent)) {
    if apply {
        let mut recorded: HashSet<usize> = HashSet::new();
        let mut attempted: Vec<usize> = Vec::new();
        let mut cancelled: Vec<usize> = Vec::new();
        // 앞 번들이 아직 안 들어가 "기대는 커밋이 없다"로 멈춘 것은, 그 뒤에 다른 번들이 들어갔으면 한 번 더 시도한다
        let mut rnd = 0;
        loop {
            rnd += 1;
            let mut progress = false;
            let mut retry = Vec::new();
            for (pos, &i) in pending.iter().enumerate() {
                // 적용(재시도 포함) 직전마다 취소를 본다. 취소면 이 항목과 남은 항목은 적용하지 않는다.
                if cancel.load(Ordering::SeqCst) {
                    for &j in pending[pos..].iter().chain(retry.iter()) {
                        if !cancelled.contains(&j) {
                            cancelled.push(j);
                        }
                    }
                    break;
                }
                if !attempted.contains(&i) {
                    attempted.push(i);
                }
                crate::log::say(&format!("===== [{}] {} → {}", i + 1, base_name(&rows[i].path), rows[i].repo));
                on_event(RunEvent::Start { file: &rows[i].path, name: &base_name(&rows[i].path), repo: &rows[i].repo, pos: i, total: rows.len() });
                if let Some(msg) = repo_fit_problem(rows, i) {
                    let mut o = Outcome::stop(msg);
                    o.file = Some(rows[i].path.clone());
                    rows[i].status = format!("[{}] {}", o.kind, first_line(&o.message));
                    rows[i].result = Some(o);
                    record(&rows[i], rows[i].result.as_ref().unwrap());
                    recorded.insert(i);
                    on_event(RunEvent::Done { outcome: rows[i].result.as_ref().unwrap(), retry: false });
                    continue;
                }
                let mut o = crate::apply::apply(&rows[i].path, &rows[i].repo, rows[i].sync_all);
                if o.file.is_none() {
                    o.file = Some(rows[i].path.clone());
                }
                rows[i].status = format!("[{}] {}", o.kind, first_line(&o.message));
                if let Some(n) = o.push_note.as_ref().filter(|n| !n.is_empty()) {
                    rows[i].status += &format!(" · {}", first_line(n));
                }
                let again = !o.ok && o.message.contains(NEED_PREREQ_TEXT);
                if o.ok {
                    progress = true;
                }
                rows[i].result = Some(o);
                on_event(RunEvent::Done { outcome: rows[i].result.as_ref().unwrap(), retry: again });
                if again {
                    retry.push(i);
                } else {
                    record(&rows[i], rows[i].result.as_ref().unwrap());
                    recorded.insert(i);
                }
            }
            if !cancelled.is_empty() {
                break;
            }
            pending = retry;
            if !(progress && !pending.is_empty() && rnd < 20) {
                break;
            }
        }
        // 재시도 대기에서 멈춘 항목도 마지막 실제 시도의 결과를 한 번만 기록한다.
        for &i in &attempted {
            if !recorded.contains(&i) {
                record(&rows[i], rows[i].result.as_ref().unwrap());
            }
        }
        // 적용에 성공한 짝은 기억해 둔다
        let mut labels: HashMap<String, String> = HashMap::new();
        let mut changed = false;
        for &i in &attempted {
            if !rows[i].result.as_ref().map(|r| r.ok).unwrap_or(false) {
                continue;
            }
            let repo = rows[i].repo.clone();
            let label = labels.entry(repo.clone()).or_insert_with(|| repo_label_fresh(&repo)).clone();
            if let Some(obj) = settings.as_object_mut() {
                let map = obj.entry("repoMap").or_insert_with(|| json!({}));
                if !map.is_object() {
                    *map = json!({});
                }
                map[label.as_str()] = json!(repo);
                obj.insert("repo".into(), json!(repo));
                changed = true;
            }
        }
        if changed {
            if let Err(e) = crate::store::save_settings(settings) {
                out.push(Outcome::stop(e));
            }
        }
        let ok_n = rows.iter().filter(|r| r.result.as_ref().map(|o| o.ok).unwrap_or(false)).count();
        let err_n = attempted.len() - ok_n;
        crate::log::say(&format!("===== 결과: 번들 {}개 — 완료 {}, 멈춤 {}", attempted.len(), ok_n, err_n));
        // 취소로 적용하지 못한 항목은 기록과 요약이 끝난 뒤 보류 결과로 바꾼다(기록에는 실제 시도만 남는다).
        for &i in &cancelled {
            let mut o = Outcome::new("취소로 보류", format!("{} — 취소해서 적용하지 않았습니다.", base_name(&rows[i].path)), false);
            o.file = Some(rows[i].path.clone());
            rows[i].status = format!("[{}] {}", o.kind, first_line(&o.message));
            rows[i].result = Some(o);
        }
    }

    for (i, row) in rows.iter().enumerate() {
        // 결과 첫 줄에 판정한 로컬 폴더 전체 경로를 적는다.
        let with_repo = |mut o: Outcome| -> Outcome {
            if !row.repo.is_empty() && !o.message.starts_with(REPO_LINE) {
                o.message = format!("{}{}
{}", REPO_LINE, row.repo, o.message);
            }
            o
        };
        match &row.result {
            Some(o) => out.push(with_repo(o.clone())),
            None if row.repo.is_empty() => out.push(Outcome::stop(format!("{} — {}", base_name(&row.path), row.note))),
            None => {
                let mut o = if let Some(msg) = repo_fit_problem(rows, i) {
                    Outcome::stop(msg)
                } else {
                    let target = if row.create_new { format!("{} (새로 만듦)", row.repo) } else { row.repo.clone() };
                    Outcome::new("대기", format!("{}{}", describe_plan(row, &target), tag_preview(row)), true)
                };
                o.file = Some(row.path.clone());
                out.push(with_repo(o));
            }
        }
    }
}

fn describe_plan(row: &Row, target: &str) -> String {
    // apply::apply 는 미리보기 구분 없이 실제로 적용하므로, 미리보기(대기) 문구는 여기서 만든다.
    if row.sync_all {
        format!(
            "{} → {} (전체 동기화: 현재 체크아웃한 feature/*와 작업 파일만 유지하고, 나머지 로컬 브랜치·태그를 번들과 맞춥니다)",
            base_name(&row.path),
            target
        )
    } else {
        format!("{} → {}", base_name(&row.path), target)
    }
}

/// 미리보기 줄에 붙일 태그 안내. 태그는 원본(번들)이 기준이라, 저장소에 이미 있는 같은 이름 태그가
/// 번들과 다른 값이면 번들 값으로 옮겨집니다. 읽기만 하며, 번들 태그 값이 주석 태그 객체라 커밋과 못 맞추면 '옮길 수 있는 것'으로 센다.
fn tag_preview(row: &Row) -> String {
    let head = match &row.head {
        Some(h) if !h.tags.is_empty() => h,
        _ => return String::new(),
    };
    if row.create_new {
        return format!("
태그: 번들에 {}개, 모두 새로 만듭니다.", head.tags.len());
    }
    let git = match Git::open(&row.repo) {
        Ok(g) => g,
        Err(_) => return String::new(),
    };
    // 로컬 태그를 한 번에 읽는다(태그마다 git 을 부르면 태그 158개에 12초 걸렸다).
    // 목록을 못 읽으면 빠른 판정을 건너뛰고 태그마다 예전처럼 git 에 묻는다.
    let (listed, fast) = match git.q(&["for-each-ref", "--format=%(refname)%09%(objectname)%09%(objecttype)%09%(*objectname)%09%(*objecttype)", "refs/tags/"]) {
        Ok(r) if r.ok() => (r.out, true),
        _ => (String::new(), false),
    };
    // refname → (값, 이 값이 가리키는 커밋. 바로 알 수 없으면 None)
    let mut local: HashMap<String, (String, Option<String>)> = HashMap::new();
    let mut lower_n: HashMap<String, usize> = HashMap::new();
    for line in listed.lines() {
        // 출력 끝 공백이 잘려 마지막 줄의 빈 칸(가벼운 태그의 *objectname 등)이 없을 수 있어 칸 수로 거르지 않는다.
        let f: Vec<&str> = line.trim_end_matches('\r').split('\t').collect();
        let at = |i: usize| f.get(i).copied().unwrap_or("");
        if at(0).is_empty() || at(1).is_empty() {
            continue;
        }
        let commit = match (at(2), at(4)) {
            ("commit", _) => Some(at(1).to_string()),
            ("tag", "commit") if !at(3).is_empty() => Some(at(3).to_string()),
            _ => None,
        };
        *lower_n.entry(f[0].to_lowercase()).or_insert(0) += 1;
        local.insert(f[0].to_string(), (f[1].to_string(), commit));
    }
    let (mut new_n, mut move_n) = (0usize, 0usize);
    for (name, val) in &head.tags {
        let r = format!("refs/tags/{}", name);
        let same_case = lower_n.get(&r.to_lowercase()).copied().unwrap_or(0);
        // 정확히 하나만 있고 커밋까지 알면 바로 판정한다. 대소문자만 다른 이름(NTFS 에서 rev-parse 가
        // 다른 파일을 집을 수 있음)이나 중첩·비커밋 태그는 예전처럼 git 에 묻는다.
        match local.get(&r) {
            _ if !fast => {}
            None if same_case == 0 => {
                new_n += 1;
                continue;
            }
            Some((v, c)) if same_case == 1 && (v == val || c.is_some()) => {
                if v != val && c.as_deref() != Some(val.as_str()) {
                    move_n += 1;
                }
                continue;
            }
            _ => {}
        }
        match crate::refs::ref_value(&git, &r) {
            Ok(None) => new_n += 1,
            Ok(Some(local)) => {
                if &local != val && crate::refs::ref_commit(&git, &r).ok().flatten().as_deref() != Some(val.as_str()) {
                    move_n += 1;
                }
            }
            Err(_) => return String::new(),
        }
    }
    let mut s = format!("
태그: 번들에 {}개, 새로 생길 것 {}개", head.tags.len(), new_n);
    if move_n > 0 {
        s.push_str(&format!(", 원본 기준으로 옮길 수 있는 것 {}개(옛 값은 되돌리기 기록에 남깁니다)", move_n));
    }
    s.push('.');
    s
}

// ---------------------------------------------------------------- 계획(번들 → 저장소 줄 목록)

/// 화면이 들고 있는 한 줄. 공개 필드는 화면용이고, 비공개 필드는 적용에 쓴다.
#[derive(Debug, Clone)]
pub struct PlanRow {
    /// 번들 실제 경로(ZIP 에서 푼 것이면 임시 폴더 안 경로)
    pub path: String,
    /// 원래 ZIP 경로, 낱개 .bundle 이면 ""
    pub archive: String,
    /// 표시 이름: ZIP 안이면 "ZIP파일이름 > 번들이름", 아니면 번들 파일 이름
    pub name: String,
    /// 저장소 경로(새로 만들 경로 포함). 없으면 ""
    pub repo: String,
    /// repo 가 아직 없는 새로 만들 경로
    pub create_new: bool,
    /// repo 가 비었을 때 이유·안내, 아니면 ""
    pub note: String,
    /// 전에 적용했다면 그 시각, 아니면 ""
    pub seen: String,
    /// 기본 체크: seen 이 비었으면 true
    pub checked: bool,
    /// head.kind ("base"/"repository"/"incremental" 등 BundleHead 값 그대로)
    pub kind: String,
    /// head.branch (없으면 "")
    pub branch: String,
    /// 이 줄만 '전체 동기화'로 적용한다. plan_can_sync 가 참인 줄만 켤 수 있다.
    pub sync_all: bool,
    head: Option<BundleHead>,
    prereqs: Vec<String>,
    sha: String,
    auto_repo: bool,
    temp_dir: String,
}

/// 번들→저장소 줄 목록. 풀어 둔 ZIP 임시 폴더는 줄이 없어지거나 계획이 사라질 때 치운다.
#[derive(Debug, Default)]
pub struct QueuePlan {
    pub rows: Vec<PlanRow>,
    /// ZIP 읽기 실패·번들 읽기 실패 '멈춤' 결과
    pub notes: Vec<Outcome>,
    temp_root: PathBuf,
    temp_dirs: Vec<String>,
}

impl QueuePlan {
    pub fn new() -> Self {
        QueuePlan::default()
    }
}

impl Drop for QueuePlan {
    fn drop(&mut self) {
        let root = self.temp_root.clone();
        for d in self.temp_dirs.drain(..) {
            archive::cleanup_import_dir(&d, &root);
        }
    }
}

/// 화면·결과에 보일 번들 이름. ZIP 에서 나온 번들은 1.0.5 record() 처럼 "ZIP 전체 경로 > 번들 이름",
/// 낱개 번들은 파일 이름만(임시 폴더 경로는 적용 뒤 지워지므로 보이지 않는다).
pub fn shown_bundle_name(archive_path: &str, path: &str) -> String {
    if archive_path.is_empty() {
        base_name(path)
    } else {
        format!("{} > {}", archive_path, base_name(path))
    }
}

fn plan_display_name(archive_path: &str, path: &str) -> String {
    shown_bundle_name(archive_path, path)
}

fn plan_row_of(row: &Row) -> PlanRow {
    PlanRow {
        path: row.path.clone(),
        archive: row.archive.clone(),
        name: plan_display_name(&row.archive, &row.path),
        repo: row.repo.clone(),
        create_new: row.create_new,
        note: row.note.clone(),
        seen: row.seen.clone(),
        checked: row.seen.is_empty(),
        kind: row.head.as_ref().map(|h| h.kind.clone()).unwrap_or_default(),
        branch: row.head.as_ref().map(|h| h.branch.clone()).unwrap_or_default(),
        sync_all: row.sync_all,
        head: row.head.clone(),
        prereqs: row.prereqs.clone(),
        sha: row.sha.clone(),
        auto_repo: row.auto_repo,
        temp_dir: row.temp_dir.clone(),
    }
}

fn row_of(p: &PlanRow) -> Row {
    Row {
        path: p.path.clone(),
        head: p.head.clone(),
        prereqs: p.prereqs.clone(),
        repo: p.repo.clone(),
        note: p.note.clone(),
        sha: p.sha.clone(),
        seen: p.seen.clone(),
        create_new: p.create_new,
        auto_repo: p.auto_repo,
        archive: p.archive.clone(),
        temp_dir: p.temp_dir.clone(),
        sync_all: p.sync_all,
        ..Default::default()
    }
}

/// 전체 동기화를 켤 수 있는 줄인가: 저장소 전체 번들(repository) · 범위 all · 이미 있는 저장소 폴더 · 새로 만들 줄이 아님.
pub fn plan_can_sync(row: &PlanRow) -> bool {
    match &row.head {
        Some(h) => h.kind == "repository" && h.scope == "all" && !row.repo.is_empty() && !row.create_new && is_dir(&row.repo),
        None => false,
    }
}

/// idx 줄의 전체 동기화를 켜거나 끈다. 조건(plan_can_sync)이 안 되면 켜지지 않고 꺼진다.
pub fn plan_set_sync(plan: &mut QueuePlan, idx: usize, on: bool) {
    if idx >= plan.rows.len() {
        return;
    }
    let can = plan_can_sync(&plan.rows[idx]);
    plan.rows[idx].sync_all = on && can;
}

/// 조건이 깨진 줄의 전체 동기화 표시를 끈다.
fn fix_sync(rows: &mut [PlanRow]) {
    for r in rows.iter_mut() {
        if r.sync_all {
            let ok = plan_can_sync(r);
            r.sync_all = ok;
        }
    }
}

/// 같은 브랜치의 연속 번들을 앞 번들 뒤로 맞춘다(order_chained 의 PlanRow 판).
/// 추가·저장소 바꾸기 때만 부른다. 화면은 줄 순서가 바뀔 수 있다고 보고 경로로 줄을 다시 찾는다.
fn order_plan(rows: &mut Vec<PlanRow>) {
    let mut guard = 0;
    let mut moved = true;
    while moved && guard < 200 {
        moved = false;
        guard += 1;
        for r_idx in 0..rows.len() {
            if rows[r_idx].head.is_none() || rows[r_idx].prereqs.is_empty() {
                continue;
            }
            let prereqs = rows[r_idx].prereqs.clone();
            let parent = (0..rows.len()).find(|&j| j != r_idx && rows[j].head.as_ref().map(|h| prereqs.contains(&h.oid)).unwrap_or(false));
            let Some(p_idx) = parent else { continue };
            if r_idx < p_idx {
                let row = rows.remove(r_idx);
                rows.insert(p_idx, row);
                moved = true;
                break;
            }
        }
    }
}

/// idx 줄을 delta 칸 옮긴다(손으로 순서 바꾸기). 새 위치를 돌려주고, 범위를 벗어나거나 안 움직이면 None.
pub fn plan_move(plan: &mut QueuePlan, idx: usize, delta: i32) -> Option<usize> {
    if idx >= plan.rows.len() || delta == 0 {
        return None;
    }
    let to = idx as i64 + delta as i64;
    if to < 0 || to >= plan.rows.len() as i64 {
        return None;
    }
    let to = to as usize;
    let row = plan.rows.remove(idx);
    plan.rows.insert(to, row);
    Some(to)
}

/// 어떤 줄도 쓰지 않는 임시 폴더를 치운다.
fn prune_plan_temp(plan: &mut QueuePlan) {
    let used: HashSet<String> = plan.rows.iter().filter(|r| !r.temp_dir.is_empty()).map(|r| r.temp_dir.clone()).collect();
    let root = plan.temp_root.clone();
    let (keep, gone): (Vec<String>, Vec<String>) = std::mem::take(&mut plan.temp_dirs).into_iter().partition(|d| used.contains(d));
    for d in &gone {
        archive::cleanup_import_dir(d, &root);
    }
    plan.temp_dirs = keep;
}

/// paths(폴더·.bundle·.zip 섞음)의 번들을 계획 줄로 더한다. 이미 있는 경로·같은 내용은 넣지 않는다.
/// 전에 적용한 번들도 넣되 seen 을 채우고 checked=false 로 둔다. 저장소가 빈 줄은 계획 전체를 기준으로 짝짓는다.
pub fn plan_add(plan: &mut QueuePlan, paths: &[String], src_root: &str) -> Result<(), String> {
    let settings = crate::store::load_settings()?;
    let history = crate::store::read_history(500)?;
    let temp_root = archive::import_temp_dir()?;
    let (eff, cands) = effective_cands(&settings, src_root);

    let expanded = archive::expand_inputs(paths, &temp_root);
    plan.temp_root = temp_root;
    plan.temp_dirs.extend(expanded.temp_dirs.iter().cloned());
    for (path, message) in &expanded.errors {
        plan.notes.push(zip_error_outcome(path, message));
    }

    let applied = applied_sha_map(&history);
    let mut known: HashSet<String> = plan.rows.iter().map(|r| path_key(&r.path)).collect();
    let mut known_sha: HashSet<String> = plan.rows.iter().filter(|r| !r.sha.is_empty()).map(|r| r.sha.clone()).collect();
    let (new_rows, _skip_n, _dup_n) = read_rows(&expanded.bundles, true, &applied, &mut known, &mut known_sha, &mut plan.notes);

    let mut rows: Vec<Row> = plan.rows.iter().map(row_of).collect();
    rows.extend(new_rows);
    resolve_queue_repos(&mut rows, &cands, &history, &eff);
    // 연속 번들 순서는 여기서(추가 때) 맞춘다. 줄 순서가 바뀔 수 있으니 화면은 경로로 줄을 다시 찾는다. 실행(plan_run)은 목록 순서 그대로다.
    plan.rows = rows.iter().map(plan_row_of).collect();
    order_plan(&mut plan.rows);
    fix_sync(&mut plan.rows);
    prune_plan_temp(plan);
    Ok(())
}

/// repo 가 빈 줄만(repo 있는 줄·새로 만들 줄은 그대로) 다시 짝짓는다. note 도 갱신한다.
pub fn plan_rematch(plan: &mut QueuePlan, src_root: &str) -> Result<(), String> {
    let settings = crate::store::load_settings()?;
    let history = crate::store::read_history(500)?;
    let (eff, cands) = effective_cands(&settings, src_root);
    let mut rows: Vec<Row> = plan.rows.iter().map(row_of).collect();
    resolve_queue_repos(&mut rows, &cands, &history, &eff);
    plan.rows = rows.iter().map(plan_row_of).collect();
    order_plan(&mut plan.rows);
    fix_sync(&mut plan.rows);
    Ok(())
}

/// idx 줄의 저장소를 repo 로 정한다(git 저장소여야 함). 성공하면 번들 라벨 → 저장소 짝을 repoMap 에 기억한다.
pub fn plan_set_repo(plan: &mut QueuePlan, idx: &[usize], repo: &str) -> Result<(), String> {
    let repo = trim_sep(repo);
    let is_repo = is_dir(&repo) && Git::open(&repo).and_then(|g| g.q(&["rev-parse", "--show-toplevel"])).map(|r| r.ok()).unwrap_or(false);
    if !is_repo {
        return Err(format!("선택한 폴더는 git 저장소가 아닙니다: {}", repo));
    }
    let mut done: HashSet<usize> = HashSet::new();
    let mut labels: Vec<String> = Vec::new();
    for &i in idx {
        if i >= plan.rows.len() || !done.insert(i) {
            continue;
        }
        let row = &mut plan.rows[i];
        row.repo = repo.clone();
        row.create_new = false;
        row.auto_repo = false;
        row.note.clear();
        // 저장소가 바뀌면 전체 동기화 선택은 당시 저장소에 대한 뜻이었으니 다시 검사한다(1.0.5 는 다시 고르게 했다).
        let keep = row.sync_all && plan_can_sync(row);
        row.sync_all = keep;
        if let Some(h) = &row.head {
            let label = bundle_label(&row.path, h);
            if !label.is_empty() {
                labels.push(label);
            }
        }
    }
    // 저장소를 바꾼 때도 연속 번들 순서를 맞춘다(화면은 경로로 줄을 다시 찾는다).
    order_plan(&mut plan.rows);
    if labels.is_empty() {
        return Ok(());
    }
    let mut settings = crate::store::load_settings()?;
    if let Some(obj) = settings.as_object_mut() {
        let map = obj.entry("repoMap").or_insert_with(|| json!({}));
        if !map.is_object() {
            *map = json!({});
        }
        for label in &labels {
            map[label.as_str()] = json!(repo);
        }
        crate::store::save_settings(&settings)?;
    }
    Ok(())
}

/// idx 줄 가운데 완전본(base/repository)이고 기대는 커밋이 없는 줄만 <parent>/<새 저장소 이름> 으로 새로 만들게 한다.
/// 건너뛴 줄의 이유를 돌려준다.
pub fn plan_set_new_repo_parent(plan: &mut QueuePlan, idx: &[usize], parent: &str) -> Result<Vec<String>, String> {
    let parent = trim_sep(parent);
    if !is_dir(&parent) {
        return Err(format!("저장소를 만들 폴더를 찾을 수 없습니다: {}", parent));
    }
    let mut reasons: Vec<String> = Vec::new();
    let mut done: HashSet<usize> = HashSet::new();
    for &i in idx {
        if i >= plan.rows.len() || !done.insert(i) {
            continue;
        }
        let row = &mut plan.rows[i];
        let Some(head) = row.head.clone() else {
            reasons.push(format!("{} — 읽을 수 없는 번들입니다.", row.name));
            continue;
        };
        if !(head.kind == "base" || head.kind == "repository") || !row.prereqs.is_empty() {
            reasons.push(format!("{} — 처음부터 복원할 수 있는 번들이 아니라 새 저장소로 만들 수 없습니다.", row.name));
            continue;
        }
        let label = new_repo_label(&row.path, &head);
        if label.is_empty() {
            reasons.push(format!("{} — 번들에서 저장소 이름을 알 수 없습니다.", row.name));
            continue;
        }
        if !single_component(&label) {
            reasons.push(format!("{} — 번들의 저장소 이름으로 안전한 생성 경로를 만들 수 없습니다.", row.name));
            continue;
        }
        let dest = Path::new(&parent).join(&label);
        let dest_s = dest.to_string_lossy().to_string();
        if dest.exists() {
            reasons.push(format!(
                "{} — {}{}",
                row.name,
                if head.native {
                    "같은 이름의 폴더가 있어 새로 만들지 않았습니다. 기존 저장소라면 [저장소 바꾸기…]로 선택해 주세요: "
                } else {
                    "같은 이름의 폴더가 있지만 git 저장소가 아닙니다 — 폴더를 확인해 주세요: "
                },
                dest_s
            ));
            continue;
        }
        row.repo = dest_s;
        row.create_new = true;
        row.auto_repo = false;
        row.note.clear();
        row.sync_all = false;
    }
    Ok(reasons)
}

/// idx 줄을 뺀다(중복·범위 밖은 무시). 쓰는 줄이 없어진 임시 폴더는 치운다(줄이 0개면 모두).
pub fn plan_remove(plan: &mut QueuePlan, idx: &[usize]) {
    let drop_set: HashSet<usize> = idx.iter().copied().filter(|&i| i < plan.rows.len()).collect();
    let mut k = 0usize;
    plan.rows.retain(|_| {
        let keep = !drop_set.contains(&k);
        k += 1;
        keep
    });
    prune_plan_temp(plan);
}

/// idx 줄 중 repo 가 있는 줄만 plan_rows 순서대로 처리한다. apply 가 거짓이면 '대기' 계획 결과만 낸다.
/// 각 Outcome.file 은 그 줄의 번들 경로다. repo 가 빈 줄은 결과에 넣지 않는다.
/// 줄 사본만 받으므로 화면은 도는 동안에도 목록을 그대로 보여 준다(저장소를 다시 찾지 않는다).
pub fn plan_run(plan_rows: &[PlanRow], idx: &[usize], apply: bool, cancel: &AtomicBool) -> Vec<Outcome> {
    plan_run_live(plan_rows, idx, apply, cancel, &mut |_| {})
}

/// plan_run 과 같고, 적용 중 줄마다 시작·끝을 on_event 로 알린다(화면이 줄 상태를 바로 바꾸게).
/// 알림은 진행 표시용이다. 최종 결과는 돌려주는 Outcome 목록이 기준이다.
pub fn plan_run_live(plan_rows: &[PlanRow], idx: &[usize], apply: bool, cancel: &AtomicBool, on_event: &mut dyn FnMut(RunEvent)) -> Vec<Outcome> {
    let want: HashSet<usize> = idx.iter().copied().collect();
    let mut rows: Vec<Row> = Vec::new();
    for (i, p) in plan_rows.iter().enumerate() {
        if !want.contains(&i) || p.repo.is_empty() || p.head.is_none() {
            continue;
        }
        let mut row = row_of(p);
        // 전체 동기화는 실행 시점에도 조건이 맞는 줄에만 준다.
        row.sync_all = p.sync_all && plan_can_sync(p);
        set_row_idle(&mut row);
        rows.push(row);
    }
    // 연속 번들 순서는 추가·저장소 바꾸기 때 맞춘다. 실행은 목록 순서 그대로(손으로 바꾼 순서를 존중한다).
    let mut out: Vec<Outcome> = Vec::new();
    if rows.is_empty() {
        return out;
    }
    let mut settings = if apply {
        match crate::store::load_settings() {
            Ok(s) => s,
            Err(e) => return vec![Outcome::stop(e)],
        }
    } else {
        json!({})
    };
    let pending: Vec<usize> = (0..rows.len()).collect();
    run_apply_rows(&mut rows, pending, apply, &mut settings, &mut out, cancel, on_event);
    out
}
