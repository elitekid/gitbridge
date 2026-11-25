//! 참조·번들 읽기와 결과 객체. 1.0.5 refs.py 를 옮기고 64자 OID 와 v2 매니페스트를 받는다.

use crate::gitrun::{wsl_location, Git, GitRes};
use crate::undo::UndoLog;
use crate::R;
use serde::Serialize;
use std::collections::BTreeMap;

pub const BASE_BRANCH_ORDER: [&str; 3] = ["production", "main", "master"];
pub const IN_PROGRESS_MARKS: [&str; 5] = ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD", "rebase-merge", "rebase-apply"];
/// 번들에서 받을 때 서브모듈 원격에 접속하지 않는다. --atomic 같은 추가 옵션은 뒤에 붙인다.
pub const BUNDLE_FETCH: [&str; 3] = ["fetch", "--no-tags", "--no-recurse-submodules"];
pub const SUMMARY_NAMES: usize = 20;
pub const V2_MANIFEST: &str = "refs/gitbridge/v2/manifest";
pub const V2_DEVNET: &str = "refs/gitbridge/v2/devnet/";
pub const V1_DEVNET: &str = "refs/gitbridge/devnet/";

pub fn is_oid(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn is_zero(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b == b'0')
}

/// 저장소 해시 길이에 맞춘 0 값.
pub fn zero_oid(git: &Git) -> String {
    match git.q(&["rev-parse", "--show-object-format"]) {
        Ok(r) if r.ok() && r.out.trim() == "sha256" => "0".repeat(64),
        _ => "0".repeat(40),
    }
}

fn seg_ok(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && b[0].is_ascii_alphanumeric() && b.iter().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// feature/ 또는 hotfix/ 로 시작하고 영문·숫자·점·밑줄·하이픈만.
pub fn name_ok(name: &str) -> bool {
    let mut it = name.split('/');
    let first = it.next().unwrap_or("").to_ascii_lowercase();
    if first != "feature" && first != "hotfix" {
        return false;
    }
    let rest: Vec<&str> = it.collect();
    !rest.is_empty() && rest.iter().all(|s| seg_ok(s))
}

pub const NAME_RULE_TEXT: &str = "feature/ 또는 hotfix/ 로 시작하고 영문·숫자·점·밑줄·하이픈만";

pub fn ref_commit(git: &Git, r: &str) -> R<Option<String>> {
    let res = git.q(&["rev-parse", "--verify", "-q", &format!("{}^{{commit}}", r)])?;
    Ok(if res.ok() { Some(res.out.trim().to_string()) } else { None })
}

/// 참조의 값(객체 종류와 상관없이).
pub fn ref_value(git: &Git, r: &str) -> R<Option<String>> {
    let res = git.q(&["rev-parse", "--verify", "-q", r])?;
    Ok(if res.ok() { Some(res.out.trim().to_string()) } else { None })
}

pub fn is_ancestor(git: &Git, a: &str, b: &str) -> R<bool> {
    let r = git.q(&["merge-base", "--is-ancestor", a, b])?;
    match r.code {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(format!("커밋 관계를 판정하지 못했습니다 ({}, {}): {}", a, b, r.err)),
    }
}

/// names 중 r 과 경로 한 칸이 대소문자만 다른 첫 이름.
pub fn case_clash<'a>(r: &str, names: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let want: Vec<&str> = r.split('/').collect();
    for name in names {
        for (a, b) in name.split('/').zip(want.iter()) {
            if a != *b {
                if a.eq_ignore_ascii_case(b) {
                    return Some(name.to_string());
                }
                break;
            }
        }
    }
    None
}

pub fn local_branch_clash(git: &Git, r: &str) -> R<Option<String>> {
    let res = git.q(&["for-each-ref", "--format=%(refname)", "refs/heads/"])?;
    if !res.ok() {
        return Err(format!("로컬 브랜치 목록을 읽지 못했습니다.\n{}", res.detail()));
    }
    Ok(case_clash(r, res.lines()).map(|f| f["refs/heads/".len()..].to_string()))
}

/// 접두 아래 참조 → 값. 간접 참조가 있으면 오류.
pub fn ref_map(git: &Git, prefixes: &[&str]) -> R<BTreeMap<String, String>> {
    let mut args = vec!["for-each-ref", "--format=%(refname)\t%(objectname)\t%(symref)"];
    args.extend_from_slice(prefixes);
    let res = git.q(&args)?;
    if !res.ok() {
        return Err(format!("브랜치·태그 목록을 읽지 못했습니다.\n{}", res.detail()));
    }
    let mut m = BTreeMap::new();
    for line in res.lines() {
        let p: Vec<&str> = line.split('\t').collect();
        if p.len() < 2 || (p.len() > 2 && !p[2].is_empty()) {
            return Err("임시 기록에 예상하지 못한 연결 항목이 있습니다.".into());
        }
        m.insert(p[0].to_string(), p[1].to_string());
    }
    Ok(m)
}

/// 접두를 떼어 이름 → 값.
pub fn strip_map(m: &BTreeMap<String, String>, prefix: &str) -> BTreeMap<String, String> {
    m.iter().filter_map(|(k, v)| k.strip_prefix(prefix).map(|n| (n.to_string(), v.clone()))).collect()
}

/// 지정한 임시 참조만 조회 시점 값과 비교해 한 번에 지운다.
pub fn delete_ref_prefixes(git: &Git, prefixes: &[&str]) -> R<()> {
    let refs = ref_map(git, prefixes)?;
    if refs.is_empty() {
        return Ok(());
    }
    let mut lines = vec!["start".to_string()];
    lines.extend(refs.iter().map(|(r, o)| format!("delete {} {}", r, o)));
    lines.push("prepare".into());
    lines.push("commit".into());
    let r = git.input(&["update-ref", "--no-deref", "--stdin"], &(lines.join("\n") + "\n"), true)?;
    if !r.ok() {
        return Err(format!("임시 기록을 정리하지 못했습니다.\n{}", r.detail()));
    }
    Ok(())
}

/// 번들의 list-heads 줄(HEAD 줄 포함).
pub fn bundle_lines(bundle: &str) -> R<Vec<String>> {
    let git = Git::bare_for(&[bundle])?;
    let r = git.q(&["bundle", "list-heads", bundle])?;
    if !r.ok() {
        return Err(format!("번들을 읽지 못했습니다. 파일이 깨졌거나 번들이 아닙니다.\n{}", r.err));
    }
    Ok(r.lines().into_iter().map(|s| s.to_string()).collect())
}

fn split_line(line: &str) -> Option<(&str, &str)> {
    let mut it = line.split_whitespace();
    let oid = it.next()?;
    let r = it.next()?;
    if it.next().is_some() || !is_oid(oid) {
        return None;
    }
    Some((oid, r))
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BundleHead {
    /// base, feature, repository
    pub kind: String,
    pub oid: String,
    #[serde(rename = "ref")]
    pub ref_: String,
    pub branch: String,
    pub name: String,
    pub label: String,
    pub scope: String,
    pub heads: BTreeMap<String, String>,
    pub tags: BTreeMap<String, String>,
    pub native: bool,
    /// 2.0.0 매니페스트가 든 번들.
    pub has_manifest: bool,
}

impl BundleHead {
    pub fn source_head(&self, name: &str) -> String {
        format!("{}{}", if self.native { "refs/heads/" } else { "refs/gitbridge/heads/" }, name)
    }
    pub fn source_tag(&self, name: &str) -> String {
        format!("{}{}", if self.native { "refs/tags/" } else { "refs/gitbridge/tags/" }, name)
    }
}

fn feature_head(lines: &[String]) -> R<BundleHead> {
    let mut heads = Vec::new();
    let mut tags = BTreeMap::new();
    for line in lines {
        let Some((oid, r)) = split_line(line) else { continue };
        if let Some(b) = r.strip_prefix("refs/heads/") {
            heads.push((oid.to_string(), r.to_string(), b.to_string()));
        } else if let Some(t) = r.strip_prefix("refs/tags/") {
            tags.insert(t.to_string(), oid.to_string());
        }
    }
    if heads.len() != 1 {
        return Err(format!("번들 안 브랜치가 정확히 하나여야 합니다. 지금 {}개입니다.", heads.len()));
    }
    let (oid, r, b) = heads.remove(0);
    if !name_ok(&b) {
        return Err(format!(
            "브랜치 이름이 규칙에 맞지 않습니다: {}\n규칙: feature/ 또는 hotfix/ 로 시작하고 영문·숫자·점·밑줄·하이픈만 (예: feature/HAN-117-alert-retry, hotfix/v1.2.2)",
            b
        ));
    }
    Ok(BundleHead { kind: "feature".into(), oid, ref_: r, branch: b.clone(), name: b, tags, ..Default::default() })
}

/// 저장소 묶음, 기준 브랜치 번들, feature 번들 순서로 판정한다.
pub fn get_bundle_head(bundle: &str) -> R<BundleHead> {
    let lines = bundle_lines(bundle)?;
    let heads: Vec<(String, String)> =
        lines.iter().filter_map(|l| split_line(l)).filter(|(_, r)| r.starts_with("refs/")).map(|(o, r)| (o.to_string(), r.to_string())).collect();
    let (mut repo_heads, mut repo_tags) = (BTreeMap::new(), BTreeMap::new());
    let (mut label, mut default, mut scope) = (String::new(), String::new(), String::new());
    let mut has_manifest = false;
    for (oid, r) in &heads {
        let lower = r.to_ascii_lowercase();
        let tail = |p: &str| r[p.len()..].to_string();
        if r == V2_MANIFEST {
            has_manifest = true;
        } else if lower.starts_with("refs/gitbridge/heads/") {
            repo_heads.insert(tail("refs/gitbridge/heads/"), oid.clone());
        } else if lower.starts_with("refs/gitbridge/tags/") {
            repo_tags.insert(tail("refs/gitbridge/tags/"), oid.clone());
        } else if lower.starts_with("refs/gitbridge/meta/repo/") {
            let l = tail("refs/gitbridge/meta/repo/");
            if seg_ok(&l) || l.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-')) {
                label = l;
            }
        } else if lower.starts_with("refs/gitbridge/meta/default/") {
            default = tail("refs/gitbridge/meta/default/");
        } else if lower.starts_with("refs/gitbridge/meta/scope/") {
            let s = tail("refs/gitbridge/meta/scope/").to_ascii_lowercase();
            if s == "standard" || s == "all" {
                scope = s;
            }
        }
    }
    if !repo_heads.is_empty() || (has_manifest && !default.is_empty()) {
        if label.is_empty() || default.is_empty() || scope.is_empty() {
            return Err("저장소 전체 번들의 식별 정보가 빠져 있습니다.".into());
        }
        // 2.0.0 증분 번들은 바뀌지 않은 기본 브랜치를 담지 않는다. 값은 매니페스트로 정한다.
        if !repo_heads.contains_key(&default) && !has_manifest {
            return Err(format!("저장소 전체 번들에 기본 브랜치 {} 가 없습니다.", default));
        }
        return Ok(BundleHead {
            kind: "repository".into(),
            oid: repo_heads.get(&default).cloned().unwrap_or_default(),
            ref_: format!("refs/gitbridge/heads/{}", default),
            branch: default.clone(),
            name: format!("{} ({})", label, default),
            label,
            scope,
            heads: repo_heads,
            tags: repo_tags,
            native: false,
            has_manifest,
        });
    }
    if heads.len() == 1 {
        // 슬래시가 든 기준 브랜치 이름도 받는다(1.0.5 는 한 칸만 받았다).
        if let Some(b) = heads[0].1.strip_prefix("refs/remotes/origin/") {
            if !b.is_empty() && b.split('/').all(seg_ok) {
                return Ok(BundleHead {
                    kind: "base".into(),
                    oid: heads[0].0.clone(),
                    ref_: heads[0].1.clone(),
                    branch: b.to_string(),
                    name: format!("{} (원본 서버 {})", b, b),
                    ..Default::default()
                });
            }
        }
    }
    let native_heads: BTreeMap<String, String> =
        heads.iter().filter_map(|(o, r)| r.strip_prefix("refs/heads/").map(|n| (n.to_string(), o.clone()))).collect();
    if !native_heads.is_empty() && !(native_heads.len() == 1 && name_ok(native_heads.keys().next().unwrap())) {
        let native_tags: BTreeMap<String, String> =
            heads.iter().filter_map(|(o, r)| r.strip_prefix("refs/tags/").map(|n| (n.to_string(), o.clone()))).collect();
        let head_oid = lines.iter().find_map(|l| {
            let p: Vec<&str> = l.split_whitespace().collect();
            (p.len() == 2 && p[1] == "HEAD").then(|| p[0].to_string())
        });
        let matches: Vec<&String> = native_heads.iter().filter(|(_, o)| Some(*o) == head_oid.as_ref()).map(|(n, _)| n).collect();
        let default = if matches.len() == 1 {
            matches[0].clone()
        } else {
            BASE_BRANCH_ORDER
                .iter()
                .chain(["develop"].iter())
                .find(|n| native_heads.contains_key(**n))
                .map(|s| s.to_string())
                .unwrap_or_else(|| native_heads.keys().next().unwrap().clone())
        };
        return Ok(BundleHead {
            kind: "repository".into(),
            oid: native_heads[&default].clone(),
            ref_: format!("refs/heads/{}", default),
            branch: default.clone(),
            name: format!("일반 Git 번들 ({})", default),
            scope: "native".into(),
            heads: native_heads,
            tags: native_tags,
            native: true,
            ..Default::default()
        });
    }
    let mut f = feature_head(&lines)?;
    f.label = label;
    Ok(f)
}

/// 번들 머리말의 "-<커밋>" 줄 = 받는 저장소에 이미 있어야 하는 커밋.
pub fn get_bundle_prereqs(bundle: &str) -> R<Vec<String>> {
    use std::io::Read;
    let path = match wsl_location(bundle) {
        Some(_) => bundle.to_string(),
        None => bundle.to_string(),
    };
    let mut f = std::fs::File::open(&path).map_err(|e| format!("번들 파일을 열지 못했습니다: {} ({})", bundle, e))?;
    let mut buf = vec![0u8; 65536];
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) => return Err(format!("번들 파일을 읽지 못했습니다: {}", e)),
        }
    }
    let text = String::from_utf8_lossy(&buf[..n]);
    let head = match text.find("\n\n") {
        Some(e) if e > 0 => &text[..e],
        _ => &text[..],
    };
    Ok(head
        .split('\n')
        .filter_map(|l| l.strip_prefix('-'))
        .filter_map(|l| {
            let h: String = l.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            is_oid(&h).then_some(h.to_ascii_lowercase())
        })
        .collect())
}

/// 파일 이름에 쓸 저장소 이름. origin 주소 끝을 우선 쓴다.
pub fn get_repo_label(git: &Git) -> R<String> {
    repo_label_read(git).map(|(l, _)| l)
}

/// (라벨, git 으로 읽었는지). 둘째 값이 거짓이면 git 이 저장소로 읽지 못해 폴더 이름으로 대신한 것이다.
pub fn repo_label_read(git: &Git) -> R<(String, bool)> {
    let u = git.q(&["remote", "get-url", "origin"])?;
    let mut name = String::new();
    let mut read = false;
    if u.ok() && !u.out.trim().is_empty() {
        name = url_repo_name(u.out.trim());
        read = true;
    }
    if name.is_empty() {
        let t = git.q(&["rev-parse", "--show-toplevel"])?;
        read = t.ok();
        let src = if t.ok() { t.out.trim().to_string() } else { git.repo.clone().unwrap_or_default() };
        name = src.rsplit(|c| c == '/' || c == '\\').next().unwrap_or("").to_string();
    }
    Ok((label_chars(&name), read))
}

fn url_repo_name(url: &str) -> String {
    let last = url.rsplit(|c| c == '/' || c == '\\' || c == ':').next().unwrap_or("").to_string();
    if last.to_ascii_lowercase().ends_with(".git") {
        last[..last.len() - 4].to_string()
    } else {
        last
    }
}

fn label_chars(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' }).collect()
}

/// git 을 띄우지 않고 저장소 폴더의 .git\config 에서 origin 주소를 읽어 repo_label_read 와 같은 라벨을 만든다.
/// 저장소 맨 위 폴더에 .git 폴더(HEAD·config 포함)가 있고, 설정에 포함(include)·주소 바꾸기(insteadOf)·따옴표·이어 쓰기가 없고,
/// origin 주소가 없거나 한 줄로 있을 때만 값을 돌려준다. 그 밖(WSL 경로 포함)은 None 이니 git 으로 읽는다.
pub fn repo_label_from_files(path: &str) -> Option<String> {
    if wsl_location(path).is_some() {
        return None;
    }
    let dot = std::path::Path::new(path).join(".git");
    if !dot.is_dir() || !dot.join("HEAD").is_file() {
        return None;
    }
    let text = std::fs::read_to_string(dot.join("config")).ok()?;
    let mut in_origin = false;
    let mut url: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let low = line.to_ascii_lowercase();
        if line.contains('"') && !line.starts_with('[') || line.ends_with('\\') || low.starts_with("[include") || low.contains("insteadof") {
            return None;
        }
        if line.starts_with('[') {
            // 헤더 뒤에 같은 줄로 키를 쓴 경우는 git 에 맡긴다.
            if !line.ends_with(']') {
                return None;
            }
            // 섹션 이름은 대소문자를 가리지 않고, 하위 이름(origin)은 가린다.
            let head = line.trim_start_matches('[').trim_end_matches(']').trim();
            let mut it = head.splitn(2, char::is_whitespace);
            let sec = it.next().unwrap_or("").to_ascii_lowercase();
            let sub = it.next().unwrap_or("").trim();
            in_origin = sec == "remote" && sub == "\"origin\"";
            if sec.contains('.') {
                return None;
            }
            continue;
        }
        if !in_origin {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        if k.trim().eq_ignore_ascii_case("url") {
            if url.is_some() {
                return None;
            }
            let v = v.split([';', '#']).next().unwrap_or("").trim();
            // 역슬래시는 git 설정의 이스케이프라 git 과 다르게 읽힐 수 있으니 git 에 맡긴다.
            if v.is_empty() || v.contains('\\') {
                return None;
            }
            url = Some(v.to_string());
        }
    }
    let name = match url {
        Some(u) => url_repo_name(&u),
        // origin 이 없으면 git 은 맨 위 폴더의 디스크 이름(대소문자 그대로)을 쓴다. 실제 경로로 같은 이름을 얻는다.
        None => std::fs::canonicalize(path).ok()?.file_name()?.to_string_lossy().to_string(),
    };
    if name.is_empty() {
        return None;
    }
    Some(label_chars(&name))
}

/// 서버에 묻지 않고 origin 추적 참조로 기준 브랜치를 짐작한다.
pub fn local_base_branch(git: &Git) -> Option<String> {
    let refs: Vec<String> = BASE_BRANCH_ORDER.iter().map(|b| format!("refs/remotes/origin/{}", b)).collect();
    let mut args = vec!["for-each-ref", "--format=%(refname)"];
    args.extend(refs.iter().map(|s| s.as_str()));
    let r = git.q(&args).ok()?;
    if !r.ok() {
        return None;
    }
    let have = r.lines();
    BASE_BRANCH_ORDER.iter().find(|b| have.contains(&format!("refs/remotes/origin/{}", b).as_str())).map(|s| s.to_string())
}

pub fn is_connect_error(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    ["failed to connect", "could not resolve", "couldn't connect", "couldn’t connect", "timed out", "connection refused"].iter().any(|k| t.contains(k))
}

pub fn norm_path(path: &str) -> String {
    if let Some((d, p)) = wsl_location(path) {
        return format!("//wsl.localhost/{}{}", d.to_lowercase(), p.trim_end_matches('/'));
    }
    let full = std::path::absolute(path).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| path.to_string());
    let full = full.replace('/', "\\");
    let mut trimmed = full.trim_end_matches(['\\', '/']).to_string();
    if trimmed.len() == 2 && trimmed.ends_with(':') {
        trimmed.push('\\');
    }
    trimmed.to_lowercase()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    None,
    Here,
    Other(String),
}

pub fn checkout_place(git: &Git, r: &str) -> R<Place> {
    let top = git.q(&["rev-parse", "--show-toplevel"])?;
    if !top.ok() {
        return Err(format!("저장소 위치를 확인하지 못했습니다. {}", top.detail()));
    }
    let here = norm_path(top.out.trim());
    let wt = git.q(&["worktree", "list", "--porcelain"])?;
    if !wt.ok() {
        return Err(format!("작업 폴더 위치를 확인하지 못했습니다. {}", wt.detail()));
    }
    let mut cur: Option<String> = None;
    let want = format!("branch {}", r).to_lowercase();
    for line in wt.out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            cur = Some(norm_path(p));
        } else if line.to_lowercase() == want {
            let c = cur.clone().unwrap_or_default();
            return Ok(if c == here { Place::Here } else { Place::Other(c) });
        }
    }
    Ok(Place::None)
}

/// 작업 트리마다 체크아웃된 브랜치 참조 → 작업 트리 경로.
pub fn worktree_branches(git: &Git) -> R<BTreeMap<String, String>> {
    let wt = git.q(&["worktree", "list", "--porcelain"])?;
    if !wt.ok() {
        return Err(format!("작업 폴더 위치를 확인하지 못했습니다. {}", wt.detail()));
    }
    let mut m = BTreeMap::new();
    let mut cur = String::new();
    for line in wt.out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            cur = p.to_string();
        } else if let Some(b) = line.strip_prefix("branch ") {
            m.insert(b.to_string(), cur.clone());
        }
    }
    Ok(m)
}

/// 머지·리베이스 등 진행 중이면 그 표식 이름.
pub fn in_progress_mark(git: &Git) -> Option<String> {
    let r = match git.q(&["rev-parse", "--absolute-git-dir"]) {
        Ok(r) => r,
        Err(_) => return Some("git 폴더 확인 실패".into()),
    };
    let dir = r.out.trim();
    if !r.ok() || dir.is_empty() {
        return Some("git 폴더 확인 실패".into());
    }
    IN_PROGRESS_MARKS.iter().find(|m| std::path::Path::new(dir).join(m).exists()).map(|s| s.to_string())
}

/// 하루 이상 지난 옛 실행의 임시 받기 참조만 정리한다.
pub fn clear_leftover_import_refs(git: &Git) {
    let cutoff = crate::time::now().seconds() - 86400;
    let Ok(left) = git.q(&["for-each-ref", "--format=%(refname)\t%(objectname)", "refs/oqms-import/"]) else { return };
    for line in left.lines() {
        let Some((r, oid)) = line.split_once('\t') else { continue };
        let Some(rest) = r.strip_prefix("refs/oqms-import/") else { continue };
        if let Some(made) = crate::time::parse_stamp14(rest) {
            if made < cutoff {
                let _ = git.run(&["update-ref", "-d", r, oid]);
            }
        }
    }
}

/// 이번 실행이 쓰는 이름(14자리 시각-임의값). 옛 참조 정리가 앞 14자리를 본다.
pub fn run_stamp() -> String {
    format!("{}-{}", crate::time::now().stamp14(), crate::time::random_hex(4))
}

#[derive(Debug, Default, Clone)]
pub struct TagResult {
    pub created: Vec<String>,
    pub same: Vec<String>,
    pub conflicts: Vec<String>,
    pub skipped: Vec<String>,
    /// 같은 이름인데 다른 커밋이라 원본(번들) 값으로 옮긴 태그.
    pub moved: Vec<String>,
}

/// sources: 태그 이름 → 이 저장소 안에서 값을 읽을 참조나 OID.
/// 태그는 원본(번들)이 기준이다. 없는 태그는 만들고, 같은 커밋이면 두고, 다른 커밋이면 번들 값으로 옮긴다.
/// 옮길 때는 옛 값을 조건으로 갱신하고(그 사이 바뀌면 실패), 옛 값은 되돌리기 기록에 남는다.
pub fn place_missing_tags(git: &Git, sources: &BTreeMap<String, String>, undo: &mut UndoLog) -> R<TagResult> {
    let mut t = TagResult::default();
    if sources.is_empty() {
        return Ok(t);
    }
    let query: String = sources
        .iter()
        .map(|(n, s)| format!("{s}\n{s}^{{commit}}\nrefs/tags/{n}\nrefs/tags/{n}^{{commit}}\n"))
        .collect();
    let r = git.input(&["cat-file", "--batch-check=%(objectname)"], &query, true)?;
    let vals: Vec<Option<String>> = r.lines().iter().map(|x| is_oid(x).then(|| x.to_string())).collect();
    if !r.ok() || vals.len() != 4 * sources.len() {
        return Err(format!("태그 값을 읽지 못했습니다.\n{}", r.detail()));
    }
    let zero = undo.zero.clone();
    let mut todo = Vec::new();
    let mut mv: Vec<(String, String, String, String)> = Vec::new();
    for (i, name) in sources.keys().enumerate() {
        let (raw, want, local, local_c) = (&vals[4 * i], &vals[4 * i + 1], &vals[4 * i + 2], &vals[4 * i + 3]);
        match (raw, want, local) {
            (Some(raw), Some(want), None) => todo.push((name.clone(), raw.clone(), want.clone())),
            (Some(raw), Some(want), Some(old)) => {
                if local_c.as_ref() == Some(want) {
                    t.same.push(name.clone())
                } else {
                    mv.push((name.clone(), old.clone(), raw.clone(), want.clone()));
                }
            }
            _ => t.skipped.push(name.clone()),
        }
    }
    if !mv.is_empty() {
        let changes: Vec<(String, String, String)> = mv.iter().map(|(n, old, raw, _)| (format!("refs/tags/{}", n), old.clone(), raw.clone())).collect();
        if undo.commit(git, &changes, &[])?.ok() {
            t.moved.extend(mv.into_iter().map(|x| x.0));
        } else {
            for (name, old, raw, want) in mv {
                let r = format!("refs/tags/{}", name);
                if undo.commit(git, &[(r.clone(), old, raw)], &[])?.ok() {
                    t.moved.push(name);
                } else if ref_commit(git, &r)?.as_deref() == Some(want.as_str()) {
                    t.same.push(name);
                } else {
                    t.conflicts.push(name);
                }
            }
        }
    }
    if todo.is_empty() {
        return Ok(t);
    }
    let changes: Vec<(String, String, String)> = todo.iter().map(|(n, raw, _)| (format!("refs/tags/{}", n), zero.clone(), raw.clone())).collect();
    if undo.commit(git, &changes, &[])?.ok() {
        t.created.extend(todo.into_iter().map(|x| x.0));
        return Ok(t);
    }
    for (name, raw, want) in todo {
        let r = format!("refs/tags/{}", name);
        if undo.commit(git, &[(r.clone(), zero.clone(), raw)], &[])?.ok() {
            t.created.push(name);
        } else if ref_value(git, &r)?.is_none() {
            t.skipped.push(name);
        } else if ref_commit(git, &r)?.as_deref() == Some(want.as_str()) {
            t.same.push(name);
        } else {
            t.conflicts.push(name);
        }
    }
    Ok(t)
}

pub fn short_names(names: &[String]) -> String {
    let mut s = names.iter().take(SUMMARY_NAMES).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > SUMMARY_NAMES {
        s += &format!(" 외 {}개", names.len() - SUMMARY_NAMES);
    }
    s
}

pub fn tag_summary(t: &TagResult) -> String {
    let mut text = format!("태그 새로 만듦 {}개, 이미 같음 {}개", t.created.len(), t.same.len());
    if !t.created.is_empty() {
        text += &format!(" (새로 만든 태그: {})", short_names(&t.created));
    }
    if !t.moved.is_empty() {
        text += &format!(" · 원본 기준으로 옮긴 태그 {}개: {}", t.moved.len(), short_names(&t.moved));
    }
    if !t.conflicts.is_empty() {
        text += &format!(" · 그 사이 바뀌어 원본 기준으로 옮기지 못한 태그 {}개: {}", t.conflicts.len(), short_names(&t.conflicts));
    }
    if !t.skipped.is_empty() {
        text += &format!(" · 가져올 태그를 읽지 못했거나 커밋이 아니거나 만들지 못해 건너뛴 태그 {}개: {}", t.skipped.len(), short_names(&t.skipped));
    }
    text + "."
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub kind: String,
    pub message: String,
    pub ok: bool,
    pub branch: Option<String>,
    pub commits: Vec<String>,
    pub branch_details: Vec<String>,
    pub main_ahead: Option<i64>,
    pub push_note: Option<String>,
    pub is_base: bool,
    pub file: Option<String>,
    /// 이번 실행의 되돌리기 기록 이름(참조를 바꿨을 때).
    pub undo: Option<String>,
}

impl Outcome {
    pub fn new(kind: &str, message: impl Into<String>, ok: bool) -> Outcome {
        Outcome { kind: kind.into(), message: message.into(), ok, ..Default::default() }
    }
    /// 멈춤. 중단으로 서버 받기를 끊은 오류가 섞여 있으면 앞에 붙은 일반 오류 문구 대신 중단 안내만 남긴다.
    pub fn stop(message: impl Into<String>) -> Outcome {
        let m: String = message.into();
        if m.contains(crate::gitrun::CANCELLED) {
            return Outcome::new("멈춤", crate::gitrun::CANCELLED, false);
        }
        Outcome::new("멈춤", m, false)
    }
}

pub fn format_result(o: &Outcome) -> String {
    let mut lines = vec![format!("[{}] {}", o.kind, o.message)];
    if let Some(p) = &o.push_note {
        if !p.is_empty() {
            lines.push(p.clone());
        }
    }
    if !o.commits.is_empty() {
        lines.push("커밋:".into());
        lines.extend(o.commits.iter().map(|c| format!("  {}", c)));
    }
    if !o.branch_details.is_empty() {
        lines.push("서버 브랜치 마지막 커밋:".into());
        lines.extend(o.branch_details.iter().map(|c| format!("  {}", c)));
    }
    if let Some(a) = o.main_ahead {
        if a > 0 {
            lines.push(format!("서버 기준 브랜치가 이 브랜치보다 {}개 앞서 있습니다 — 머지는 직접 해 주세요.", a));
        }
    }
    if let Some(u) = &o.undo {
        lines.push(format!("되돌리기 기록: {}", u));
    }
    lines.join("\n")
}

/// 천 단위 쉼표.
pub fn commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn file_size(path: &str) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// 출력 폴더 안에 겹치지 않는 번들 파일 이름.
pub fn unique_bundle_path(out_dir: &str, stem: &str) -> String {
    let dir = std::path::Path::new(out_dir);
    let mut p = dir.join(format!("{}.bundle", stem));
    let mut i = 2;
    while p.exists() {
        p = dir.join(format!("{}-{}.bundle", stem, i));
        i += 1;
    }
    p.to_string_lossy().to_string()
}

/// 파일 이름에 넣을 브랜치 이름(슬래시는 밑줄).
pub fn file_part(name: &str) -> String {
    name.replace('/', "_")
}

/// 검증에 실패한 번들 파일만 지운다.
pub fn remove_failed_bundle(path: &Option<String>) {
    if let Some(f) = path {
        match std::fs::remove_file(f) {
            Ok(()) => crate::log::say(&format!("검증을 통과하지 못한 번들 파일을 지웠습니다: {}", f)),
            Err(e) => crate::log::say(&format!("검증을 통과하지 못한 번들 파일을 지우지 못했습니다. 밖으로 옮기지 말아 주세요: {} ({})", f, e)),
        }
    }
}

pub fn tx_ok(r: &GitRes) -> bool {
    r.ok()
}
