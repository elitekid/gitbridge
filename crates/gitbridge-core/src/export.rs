//! 원본 → 작업 PC 번들 만들기. 기준 브랜치 번들(1.0.5 와 같음)과 저장소 번들(standard 증분, all 전체).
//! 증분의 표식(refs/oqms-export/<브랜치>)은 만든 번들의 검증이 끝난 뒤에만 옮긴다.

use crate::gitrun::{acquire_export_lock, ExportLock, Git};
use crate::manifest::{Manifest, FORMAT};
use crate::refs::*;
use crate::snapshot::Snapshot;
use crate::R;
use std::collections::BTreeMap;

const EXPORT_PREFIXES: [&str; 4] = ["refs/gitbridge/heads/", "refs/gitbridge/tags/", "refs/gitbridge/meta/", "refs/gitbridge/v2/manifest"];
/// 지난번 standard 번들의 매니페스트. 다음 증분의 태그 비교 기준이다.
pub const EXPORTED_STANDARD: &str = "refs/gitbridge/v2/exported/standard";
pub const MARK_ROOT: &str = "refs/oqms-export/";

pub fn server_heads(git: &Git) -> R<BTreeMap<String, String>> {
    let ls = git.run(&["ls-remote", "--heads", "origin"])?;
    if !ls.ok() {
        if is_connect_error(&ls.err) {
            return Err(format!("이 PC 에서 원본 서버에 연결되지 않습니다. 원본 PC(서버에 연결되는 PC)에서 만들어 주세요.\n{}", ls.err));
        }
        return Err(format!("서버 브랜치 목록을 읽지 못했습니다. 네트워크나 로그인을 확인해 주세요.\n{}", ls.err));
    }
    let mut heads = BTreeMap::new();
    for line in ls.lines() {
        let mut it = line.split_whitespace();
        if let (Some(oid), Some(r), None) = (it.next(), it.next(), it.next()) {
            if let Some(b) = r.strip_prefix("refs/heads/") {
                if is_oid(oid) {
                    heads.insert(b.to_string(), oid.to_ascii_lowercase());
                }
            }
        }
    }
    Ok(heads)
}

pub fn pick_base_branch(heads: &BTreeMap<String, String>, branch: Option<&str>, label: &str) -> R<String> {
    if let Some(b) = branch.filter(|b| !b.is_empty() && *b != "auto") {
        if !heads.contains_key(b) {
            return Err(format!("{} 서버에 지정한 기준 브랜치 {} 가 없습니다.", label, b));
        }
        return Ok(b.to_string());
    }
    BASE_BRANCH_ORDER
        .iter()
        .find(|c| heads.contains_key(**c))
        .map(|s| s.to_string())
        .ok_or_else(|| format!("{} 서버에 production·main·master 가 없습니다. 목록에서 기준 브랜치를 직접 지정해 주세요.", label))
}

pub(crate) fn log_lines(git: &Git, range: &[&str]) -> Vec<String> {
    let mut args = vec!["log", "--format=%h %s", "-n", "50"];
    args.extend_from_slice(range);
    git.q(&args).map(|r| r.lines().iter().map(|s| s.to_string()).collect()).unwrap_or_default()
}

pub(crate) fn count(git: &Git, range: &[&str]) -> String {
    let mut args = vec!["rev-list", "--count"];
    args.extend_from_slice(range);
    match git.q(&args) {
        Ok(r) if r.ok() && r.out.trim().chars().all(|c| c.is_ascii_digit()) && !r.out.trim().is_empty() => r.out.trim().to_string(),
        _ => "확인 불가".into(),
    }
}

pub(crate) fn ensure_out_dir(out_dir: &str) -> R<()> {
    std::fs::create_dir_all(out_dir).map_err(|e| format!("출력 폴더를 만들지 못했습니다: {} ({})", out_dir, e))
}

/// 출력 폴더를 절대 경로로 확정한다. WSL 경로나 이미 절대인 경로는 그대로 둔다.
pub(crate) fn absolute_out_dir(p: &str) -> String {
    if crate::gitrun::wsl_location(p).is_some() || std::path::Path::new(p).is_absolute() {
        return p.to_string();
    }
    match std::path::absolute(p) {
        Ok(a) => a.to_string_lossy().to_string(),
        Err(_) => p.to_string(),
    }
}

/// 기준 브랜치 하나를 증분 또는 전체로 만든다(1.0.5 기준 번들과 같은 형식).
pub fn export_base(repo: &str, out_dir: &str, full: bool, branch: Option<&str>) -> Outcome {
    let out_dir = absolute_out_dir(out_dir);
    let out_dir = out_dir.as_str();
    crate::log::say(&format!("===== 기준 번들 만들기 — 저장소 {} / 기준 브랜치 {} / 출력 {} / 전체 {}", repo, branch.unwrap_or("auto"), out_dir, full));
    let mut created: Option<String> = None;
    // 잠금 가드는 클로저 밖에서 쥐어, 실패 번들 삭제까지 잠금 안에서 끝나게 한다.
    let mut lock: Option<ExportLock> = None;
    let res = (|| -> R<Outcome> {
        let git = Git::open(repo)?;
        if !git.q(&["rev-parse", "--show-toplevel"])?.ok() {
            return Ok(Outcome::stop("git 저장소가 아닙니다."));
        }
        if !git.q(&["remote", "get-url", "origin"])?.ok() {
            return Ok(Outcome::stop("서버 주소(origin)가 없습니다."));
        }
        lock = Some(acquire_export_lock(&git)?);
        let label = get_repo_label(&git)?;
        let heads = match server_heads(&git) {
            Ok(h) => h,
            Err(e) if e.starts_with("이 PC 에서") => {
                return Ok(Outcome::stop(e.replacen(
                    "원본 PC(서버에 연결되는 PC)에서 만들어 주세요.",
                    "기준 번들 만들기는 원본 PC 에서 해 주세요. (작업 PC 에서는 [번들 적용] 탭만 씁니다)",
                    1,
                )))
            }
            Err(e) => return Ok(Outcome::stop(e)),
        };
        let b = match pick_base_branch(&heads, branch, &label) {
            Ok(b) => b,
            Err(e) => return Ok(Outcome::stop(e)),
        };
        let m = heads[&b].clone();
        let track = format!("refs/remotes/origin/{}", b);
        let marker = format!("{}{}", MARK_ROOT, b);
        crate::log::say(&format!("기준 브랜치: {} = {}", b, m));
        let f = git.run(&["fetch", "--no-tags", "origin", &format!("+refs/heads/{}:{}", b, track)])?;
        if !f.ok() {
            return Ok(Outcome::stop(format!("서버에서 {} 를 받지 못했습니다.\n{}", b, f.err)));
        }
        if ref_commit(&git, &track)?.as_deref() != Some(m.as_str()) {
            return Ok(Outcome::stop(format!("받는 사이 서버 {} 가 바뀌었습니다. 다시 실행해 주세요.", b)));
        }
        let mark = ref_commit(&git, &marker)?;
        // 표식을 옮길 때 예상값으로 쓸 시작 때의 값(커밋으로 벗기지 않은 값).
        let old_marker = ref_value(&git, &marker)?;
        let mut base = None;
        let mut mode = "전체를 담은";
        if let (false, Some(mark)) = (full, mark) {
            if mark == m {
                crate::log::say(&format!("서버 {} 가 지난번 만든 뒤로 그대로라 전체를 담은 번들로 다시 만듭니다.", b));
            } else if is_ancestor(&git, &mark, &m)? {
                base = Some(mark);
                mode = "바뀐 부분만 담은";
            } else {
                crate::log::say(&format!("지난번 보낸 위치가 지금 서버 {} 에서 이어지지 않습니다 — 전체를 담은 번들로 만듭니다.", b));
            }
        }
        ensure_out_dir(out_dir)?;
        let file = unique_bundle_path(out_dir, &format!("{}-{}-{}", crate::time::now().file_stem(), label, file_part(&b)));
        let excl = base.as_ref().map(|x| format!("^{}", x));
        let mut args = vec!["bundle", "create", file.as_str(), track.as_str()];
        if let Some(e) = &excl {
            args.push(e);
        }
        let cr = git.run(&args)?;
        if !cr.ok() {
            return Ok(Outcome::stop(format!("번들을 만들지 못했습니다.\n{}", cr.err)));
        }
        created = Some(file.clone());
        let head = get_bundle_head(&file)?;
        if head.kind != "base" || head.branch != b || head.oid != m {
            return Ok(Outcome::stop(format!("만든 번들의 맨 끝 커밋이 서버 {} 와 다릅니다.", b)));
        }
        let v = git.run(&["bundle", "verify", &file])?;
        if !v.ok() {
            return Ok(Outcome::stop(format!("만든 번들 검증에 실패했습니다.\n{}", v.err)));
        }
        // 검증을 통과한 번들은 지우지 않는다. 표식이 이 번들을 기준으로 옮겨지기 때문이다.
        created = None;
        let expect_old = old_marker.unwrap_or_else(|| zero_oid(&git));
        let mk = git.run(&["update-ref", "-m", "oqms-bundle base export", &marker, &m, &expect_old])?;
        let mut rng = vec![m.as_str()];
        if let Some(e) = &excl {
            rng.push(e);
        }
        let mut o = Outcome::new(
            "기준 번들 생성",
            format!("{} {} 를 {} 번들로 만들었습니다. 커밋 {}개, {} 바이트.", label, b, mode, count(&git, &rng), commas(file_size(&file))),
            true,
        );
        if let Ok(t) = git.q(&["show", "-s", "--date=format-local:%Y-%m-%d %H:%M", "--format=%cd %h %s", &m]) {
            if let Some(first) = t.lines().first().filter(|_| t.ok()) {
                o.message += &format!(" 실제 번들 끝: {}", first);
            }
        }
        o.branch = Some(b);
        o.commits = log_lines(&git, &rng);
        let mut note = format!("파일: {}", file);
        if !mk.ok() {
            note += "\n지난번 보낸 위치를 기록하지 못했습니다 — 다음 번엔 [전체로 만들기]를 켜 주세요. 그사이 다른 작업이 그 기록을 바꿨을 수 있습니다.";
        }
        o.push_note = Some(note);
        o.file = Some(file);
        o.is_base = true;
        Ok(o)
    })();
    remove_failed_bundle(&created);
    drop(lock);
    res.unwrap_or_else(|e| Outcome::stop(format!("예상하지 못한 오류로 멈췄습니다: {}", e)))
}

/// 저장소 번들. standard 는 기준 브랜치와 develop·모든 태그, all 은 모든 브랜치·태그.
/// standard 는 표식이 모두 조상일 때 증분으로, all 은 늘 전체로 만든다.
pub fn export_repository(repo: &str, out_dir: &str, scope: &str, branch: Option<&str>, full: bool) -> Outcome {
    export_repository_run(repo, out_dir, scope, branch, full, false).pop().unwrap_or_else(|| Outcome::stop("결과가 없습니다."))
}

/// 번들 만들기 앞에 서버 최신화(origin 추적 참조·기준 브랜치 등 로컬 브랜치 갱신)를 먼저 한다(1.0.5 와 같음). base 는 최신화를 하고 기준 번들을, standard·all 은 한 번 받은 서버 값으로 최신화와 번들을 만든다.
/// 결과는 [최신화, 번들] 순서이며, 최신화가 멈추면 번들은 만들지 않고 최신화 결과만 돌려준다. 작업 브랜치(feature) 번들은 서버에 닿지 않는 PC 를 위해 이 함수가 아니라 create_feature 를 쓴다.
pub fn export_refreshed(repo: &str, out_dir: &str, scope: &str, branch: Option<&str>, full: bool) -> Vec<Outcome> {
    match scope {
        "base" => {
            let r = crate::refresh::refresh(repo);
            if !r.ok {
                return vec![r];
            }
            vec![r, export_base(repo, out_dir, full, branch)]
        }
        _ => export_repository_run(repo, out_dir, scope, branch, full, true),
    }
}

fn export_repository_run(repo: &str, out_dir: &str, scope: &str, branch: Option<&str>, full: bool, refresh_first: bool) -> Vec<Outcome> {
    if scope != "standard" && scope != "all" {
        return vec![Outcome::stop(format!("알 수 없는 저장소 범위입니다: {}", scope))];
    }
    let out_dir = absolute_out_dir(out_dir);
    let out_dir = out_dir.as_str();
    crate::log::say(&format!("===== 저장소 브랜치·태그 번들 만들기 — 저장소 {} / 범위 {} / 기준 브랜치 {}", repo, scope, branch.unwrap_or("auto")));
    let git = match Git::open(repo) {
        Ok(g) => g,
        Err(e) => return vec![Outcome::stop(e)],
    };
    // 최신화를 함께 할 때는 origin 추적 참조도 같은 fetch 로 갱신한다.
    let snap = match Snapshot::fetch(&git, refresh_first) {
        Ok(s) => s,
        Err(e) => return vec![Outcome::stop(format!("서버 브랜치·태그 번들을 만들지 못했습니다: {}", e))],
    };
    let mut pre: Option<Outcome> = None;
    if refresh_first {
        let r = crate::refresh::refresh_with_snapshot(&git, &snap);
        if !r.ok {
            return vec![r];
        }
        pre = Some(r);
    }
    let mut created: Option<String> = None;
    let mut made: BTreeMap<String, String> = BTreeMap::new();
    let mut lock: Option<ExportLock> = None;
    let mut res = repository_inner(&git, &snap, out_dir, scope, branch, full, &mut created, &mut made, &mut lock)
        .unwrap_or_else(|e| Outcome::stop(format!("예상하지 못한 오류로 멈췄습니다: {}", e)));
    // 아래 정리는 모두 잠금 가드(lock)가 살아 있는 동안 끝낸다.
    remove_failed_bundle(&created);
    let mut notes = Vec::new();
    if !made.is_empty() {
        // 이번 실행이 만든 참조만, 만든 값을 예상 OID 로 하여 한 트랜잭션으로 지운다.
        let mut tx = vec!["start".to_string()];
        tx.extend(made.iter().map(|(r, o)| format!("delete {} {}", r, o)));
        tx.push("prepare".into());
        tx.push("commit".into());
        let fail = match git.input(&["update-ref", "--no-deref", "--stdin"], &(tx.join("\n") + "\n"), false) {
            Ok(r) if r.ok() => None,
            Ok(r) => Some(r.detail().to_string()),
            Err(e) => Some(e),
        };
        if let Some(e) = fail {
            let n = format!("임시 번들 기록 정리 보류: {}", e);
            crate::log::say(&n);
            notes.push(n);
        }
    }
    drop(snap);
    drop(lock);
    // Snapshot 의 정리 실패는 Drop 안에서 로그에 남는다.
    if !notes.is_empty() {
        let p = res.push_note.take().unwrap_or_default();
        res.push_note = Some([p].into_iter().chain(notes).filter(|x| !x.is_empty()).collect::<Vec<_>>().join("\n"));
    }
    pre.into_iter().chain(std::iter::once(res)).collect()
}

#[allow(clippy::too_many_arguments)]
fn repository_inner(
    git: &Git,
    snap: &Snapshot,
    out_dir: &str,
    scope: &str,
    branch: Option<&str>,
    full: bool,
    created: &mut Option<String>,
    made: &mut BTreeMap<String, String>,
    lock: &mut Option<ExportLock>,
) -> R<Outcome> {
    *lock = Some(acquire_export_lock(git)?);
    let label = get_repo_label(git)?;
    let (heads, tags) = (snap.heads.clone(), snap.tags.clone());
    let b = pick_base_branch(&heads, branch, &label)?;
    let selected: Vec<String> = if scope == "all" {
        heads.keys().cloned().collect()
    } else {
        let mut v = vec![b.clone()];
        if b != "develop" && heads.contains_key("develop") {
            v.push("develop".into());
        }
        v
    };
    let sel_heads: BTreeMap<String, String> = selected.iter().map(|n| (n.clone(), heads[n].clone())).collect();

    // 표식·지난 매니페스트의 시작 때 값(잠금 안). 나중에 이 값을 예상값으로 옮긴다(standard 만, full 이어도 읽는다).
    let mut old_marks: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut old_manifest: Option<String> = None;
    if scope == "standard" {
        for n in sel_heads.keys() {
            old_marks.insert(n.clone(), ref_value(git, &format!("{}{}", MARK_ROOT, n))?);
        }
        old_manifest = ref_value(git, EXPORTED_STANDARD)?;
    }

    // 증분 판정: standard 이고, 지난 매니페스트가 같은 기본 브랜치이고, 고른 브랜치마다 표식이 조상이다.
    let mut marks: BTreeMap<String, String> = BTreeMap::new();
    let mut prev: Option<Manifest> = None;
    let mut why_full = String::new();
    let mut tag_full = false;
    if scope == "standard" && !full {
        match ref_value(git, EXPORTED_STANDARD)? {
            Some(_) => match Manifest::read(git, EXPORTED_STANDARD) {
                Ok(m) if m.default == b && m.label == label => prev = Some(m),
                Ok(_) => why_full = "지난번 번들과 기본 브랜치나 저장소 이름이 다릅니다".into(),
                Err(e) => why_full = e,
            },
            None => why_full = "지난번 만든 저장소 번들 기록이 없습니다".into(),
        }
        if prev.is_some() {
            for (n, oid) in &sel_heads {
                match ref_commit(git, &format!("{}{}", MARK_ROOT, n))? {
                    Some(mk) if mk == *oid || is_ancestor(git, &mk, oid)? => {
                        marks.insert(n.clone(), mk);
                    }
                    Some(_) => {
                        why_full = format!("지난번 보낸 {} 위치가 지금 서버 {} 에서 이어지지 않습니다", n, n);
                        break;
                    }
                    None => {
                        why_full = format!("{} 의 지난번 보낸 위치 기록이 없습니다", n);
                        break;
                    }
                }
            }
            // 마커에서 이미 닿는 커밋을 가리키는 새·바뀐 태그는 증분 번들에서 빠지므로 완전본으로 만든다(1.0.5 도 받을 수 있다).
            if why_full.is_empty() {
                if let Some(p) = &prev {
                    'tags: for (n, o) in tags.iter().filter(|(n, o)| p.tags.get(*n) != Some(*o)) {
                        for mk in marks.values() {
                            if is_ancestor(git, o, mk)? {
                                why_full = format!("새·바뀐 태그 {} 가 지난번 번들에 이미 든 커밋을 가리켜 바뀐 부분만 담은 번들에 담을 수 없습니다", n);
                                tag_full = true;
                                break 'tags;
                            }
                        }
                    }
                }
            }
            if !why_full.is_empty() {
                prev = None;
                marks.clear();
            }
        }
        if prev.is_none() {
            crate::log::say(&format!("{} — 전체를 담은 번들로 만듭니다.", why_full));
        }
    }
    // 지난번과 달라진 것이 없으면 빈 번들은 못 만드니 전체를 담은 번들로 만든다.
    if let Some(p) = &prev {
        if p.heads == sel_heads && p.tags == tags && sel_heads.iter().all(|(n, o)| marks.get(n) == Some(o)) {
            crate::log::say(&format!("{} 서버 브랜치·태그가 지난번 만든 뒤로 그대로라 전체를 담은 번들로 다시 만듭니다.", label));
            prev = None;
            marks.clear();
        }
    }
    let incremental = prev.is_some();
    let (send_heads, send_tags): (BTreeMap<String, String>, BTreeMap<String, String>) = match &prev {
        Some(p) => (
            sel_heads.iter().filter(|(n, o)| marks.get(*n) != Some(*o)).map(|(n, o)| (n.clone(), o.clone())).collect(),
            tags.iter().filter(|(n, o)| p.tags.get(*n) != Some(*o)).map(|(n, o)| (n.clone(), o.clone())).collect(),
        ),
        None => (sel_heads.clone(), tags.clone()),
    };
    let manifest = Manifest {
        format: FORMAT.into(),
        label: label.clone(),
        default: b.clone(),
        scope: scope.into(),
        incremental,
        heads: sel_heads.clone(),
        tags: tags.clone(),
        created: crate::time::now().display(),
        tool: crate::TOOL.into(),
    };
    let moid = manifest.write(git)?;
    let meta = [format!("refs/gitbridge/meta/repo/{}", label), format!("refs/gitbridge/meta/default/{}", b), format!("refs/gitbridge/meta/scope/{}", scope)];
    let mut expected: BTreeMap<String, String> = BTreeMap::new();
    for (n, o) in &send_heads {
        expected.insert(format!("refs/gitbridge/heads/{}", n), o.clone());
    }
    for (n, o) in &send_tags {
        expected.insert(format!("refs/gitbridge/tags/{}", n), o.clone());
    }
    for r in &meta {
        expected.insert(r.clone(), moid.clone());
    }
    expected.insert(V2_MANIFEST.into(), moid.clone());

    delete_ref_prefixes(git, &EXPORT_PREFIXES)?;
    let mut tx = vec!["start".to_string()];
    tx.extend(expected.iter().map(|(r, o)| format!("create {} {}", r, o)));
    tx.push("prepare".into());
    tx.push("commit".into());
    let mres = git.input(&["update-ref", "--no-deref", "--stdin"], &(tx.join("\n") + "\n"), false)?;
    if !mres.ok() {
        return Err(format!("번들용 임시 기록을 한 번에 만들지 못했습니다.\n{}", mres.detail()));
    }
    *made = expected.clone();
    if ref_map(git, &EXPORT_PREFIXES)? != expected {
        return Err("고정한 서버 값과 번들용 임시 기록이 다릅니다. 다시 실행해 주세요.".into());
    }

    ensure_out_dir(out_dir)?;
    let suffix = match (scope, incremental) {
        ("all", _) => "all-refs",
        (_, true) => "base-develop-tags-incr",
        _ => "base-develop-tags",
    };
    let file = unique_bundle_path(out_dir, &format!("{}-{}-{}", crate::time::now().file_stem(), label, suffix));
    let refs: Vec<String> = expected.keys().cloned().collect();
    let excl: Vec<String> = marks.values().map(|m| format!("^{}", m)).collect();
    let mut args: Vec<&str> = vec!["bundle", "create", &file];
    args.extend(refs.iter().map(|s| s.as_str()));
    args.extend(excl.iter().map(|s| s.as_str()));
    let cr = git.run(&args)?;
    if !cr.ok() {
        return Err(format!("저장소 번들을 만들지 못했습니다.\n{}", cr.err));
    }
    *created = Some(file.clone());

    // 검증: 번들 참조는 모두 예상 값이어야 하고, 증분에서 끝이 제외된 참조만 빠질 수 있다.
    let mut listed: BTreeMap<String, String> = BTreeMap::new();
    for line in bundle_lines(&file)? {
        let mut it = line.split_whitespace();
        if let (Some(o), Some(r)) = (it.next(), it.next()) {
            if r == "HEAD" {
                continue;
            }
            if listed.insert(r.to_string(), o.to_string()).is_some() {
                return Err(format!("만든 번들에 같은 기록이 두 번 들어 있습니다: {}", r));
            }
        }
    }
    let wrong = listed.iter().any(|(r, o)| expected.get(r) != Some(o));
    let missing: Vec<&String> = expected.keys().filter(|r| !listed.contains_key(*r)).collect();
    let must = meta.iter().chain([V2_MANIFEST.to_string()].iter()).any(|r| !listed.contains_key(r));
    if wrong || must || (!incremental && !missing.is_empty()) {
        return Err("만든 번들의 기록 값이 고정한 서버 값과 다릅니다.".into());
    }
    let info = get_bundle_head(&file)?;
    if info.kind != "repository" || info.label != label || info.branch != b || info.scope != scope {
        return Err("만든 저장소 번들의 내용 검증에 실패했습니다.".into());
    }
    let v = git.run(&["bundle", "verify", &file])?;
    if !v.ok() {
        return Err(format!("만든 번들 검증에 실패했습니다.\n{}", v.err));
    }
    *created = None;

    // 검증을 통과한 뒤에만 표식과 지난 매니페스트를 옮긴다(standard 만).
    let mut mark_note = String::new();
    if scope == "standard" {
        let mut tx = vec!["start".to_string()];
        for (n, o) in &sel_heads {
            let r = format!("{}{}", MARK_ROOT, n);
            match old_marks.get(n).and_then(|x| x.as_ref()) {
                Some(old) => tx.push(format!("update {} {} {}", r, o, old)),
                None => tx.push(format!("create {} {}", r, o)),
            }
        }
        match &old_manifest {
            Some(old) => tx.push(format!("update {} {} {}", EXPORTED_STANDARD, moid, old)),
            None => tx.push(format!("create {} {}", EXPORTED_STANDARD, moid)),
        }
        tx.push("prepare".into());
        tx.push("commit".into());
        let mk = git.input(&["update-ref", "--no-deref", "--stdin"], &(tx.join("\n") + "\n"), false)?;
        if !mk.ok() {
            mark_note = "\n지난번 보낸 위치를 기록하지 못했습니다 — 다음 번엔 --full 로 만들어 주세요. 그사이 다른 작업이 그 기록을 바꿨을 수 있습니다.".into();
        }
    }

    let tip_refs: Vec<String> = send_heads.values().cloned().chain(send_tags.values().cloned()).collect();
    let mut rng: Vec<&str> = tip_refs.iter().map(|s| s.as_str()).collect();
    rng.extend(excl.iter().map(|s| s.as_str()));
    let commits = if tip_refs.is_empty() { "0".to_string() } else { count(git, &rng) };
    let kind = match (scope, incremental) {
        ("all", _) => "전체 저장소 번들 생성",
        (_, true) => "주요 참조 증분 번들 생성",
        _ => "주요 참조 번들 생성",
    };
    let size = commas(file_size(&file));
    let msg = if incremental {
        format!(
            "{}의 바뀐 브랜치 {}개와 태그 {}개를 지난번 번들에 이어지는, 바뀐 부분만 담은 번들로 만들었습니다. 전체 상태(브랜치 {}개·태그 {}개)는 목록 파일에 적었습니다. 커밋 {}개, {} 바이트.",
            label,
            send_heads.len(),
            send_tags.len(),
            sel_heads.len(),
            tags.len(),
            commits,
            size
        )
    } else {
        let mut s = format!("{}의 브랜치 {}개와 태그 {}개를 전체를 담은 번들(새 저장소를 만들 수 있음)로 만들었습니다. 커밋 {}개, {} 바이트.", label, sel_heads.len(), tags.len(), commits, size);
        if tag_full {
            s += &format!(" 바뀐 부분만 담은 번들 대신 전체를 담은 번들로 만든 이유: {}.", why_full);
        }
        s
    };
    let mut o = Outcome::new(kind, msg, true);
    let base_range: Vec<String> = match marks.get(&b) {
        Some(mk) if send_heads.contains_key(&b) => vec![heads[&b].clone(), format!("^{}", mk)],
        Some(_) => vec![],
        None => vec![heads[&b].clone()],
    };
    if !base_range.is_empty() {
        let r: Vec<&str> = base_range.iter().map(|s| s.as_str()).collect();
        o.commits = log_lines(git, &r);
    }
    o.branch = Some(b);
    o.push_note = Some(format!("파일: {}{}", file, mark_note));
    o.file = Some(file);
    o.is_base = true;
    Ok(o)
}
