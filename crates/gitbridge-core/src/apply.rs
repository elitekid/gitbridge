//! 번들 적용(1.0.5 feature.py apply_bundle, base.py apply_base_bundle·apply_repository_bundle).
//! 2.0.0: 참조 변경은 모두 UndoLog 를 거치고, 저장소 번들은 매니페스트의 전체 상태를 기준으로 맞춘다.
//! 증분 번들도 매니페스트 전체 목록에 없는 원본 보관 참조를 정리(prune)하되, 그 번들의 범위(all 은 전체, standard 는 태그·기본 브랜치·develop)까지만 지운다. 로컬 브랜치는 지우지 않는다.

use crate::gitrun::{Git, GitRes};
use crate::manifest::Manifest;
use crate::refs::*;
use crate::undo::UndoLog;
use crate::R;
use std::collections::BTreeMap;
use std::path::Path;

fn say(text: &str) {
    crate::log::say(text);
}

fn abs_path(p: &str) -> String {
    std::path::absolute(p).map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|_| p.to_string())
}

fn needs_prereq(v: &GitRes) -> bool {
    let t = format!("{}{}", v.out, v.err).to_lowercase();
    ["prerequisite", "requires", "lacks"].iter().any(|k| t.contains(k))
}

fn committed(r: R<GitRes>) -> bool {
    matches!(r, Ok(x) if x.ok())
}

/// 원본 보관 참조 r 이 번들 범위(scope)의 정리 대상인지 판정한다. default 는 기본 브랜치 이름이다.
fn in_prune_scope(r: &str, scope: &str, default: &str) -> bool {
    let rest = match r.strip_prefix(V2_DEVNET).or_else(|| r.strip_prefix(V1_DEVNET)) {
        Some(x) => x,
        None => return false,
    };
    match scope {
        "all" => rest.starts_with("heads/") || rest.starts_with("tags/"),
        "standard" => match rest.strip_prefix("heads/") {
            Some(nm) => nm == default || nm == "develop",
            None => rest.starts_with("tags/"),
        },
        _ => false,
    }
}

fn log_commits(git: &Git, extra: &[&str]) -> Vec<String> {
    let mut a = vec!["log", "--format=%h %s"];
    a.extend_from_slice(extra);
    match git.q(&a) {
        Ok(r) => {
            let v: Vec<String> = r.lines().into_iter().map(|s| s.to_string()).collect();
            v
        }
        Err(_) => Vec::new(),
    }
}

fn join_notes(a: &Option<String>, b: &Option<String>) -> Option<String> {
    let v: Vec<&str> = [a, b].iter().filter_map(|x| x.as_deref()).filter(|x| !x.is_empty()).collect();
    if v.is_empty() {
        None
    } else {
        Some(v.join("\n"))
    }
}

/// 임시 참조를 지운다. 못 지우면 로그만 남긴다.
pub(crate) fn cleanup_temp(git: &Git, temp: &[String]) {
    if temp.is_empty() {
        return;
    }
    let p: Vec<&str> = temp.iter().map(|s| s.as_str()).collect();
    if let Err(e) = delete_ref_prefixes(git, &p) {
        say(&format!("임시 브랜치·태그 정리 보류: {}", e));
    }
}

/// 안쪽 실행의 결과를 받아 오류를 멈춤으로 바꾸고, 임시 참조를 지우고, 되돌리기 기록 이름을 붙인다.
pub(crate) fn finish(git: &Git, temp: &[String], undo: &UndoLog, r: R<Outcome>) -> Outcome {
    let mut o = r.unwrap_or_else(|e| Outcome::stop(format!("예상하지 못한 오류로 멈췄습니다: {}", e)));
    cleanup_temp(git, temp);
    o.undo = undo.name();
    o
}

/// 번들 종류를 보고 feature·기준·저장소 적용, sync-all, 새 저장소 복원으로 나눈다.
pub fn apply(bundle: &str, repo: &str, sync_all: bool) -> Outcome {
    if !bundle.is_empty() && !repo.is_empty() && Path::new(bundle).is_file() {
        if let Ok(head) = get_bundle_head(bundle) {
            let b = abs_path(bundle);
            let is_dir = Path::new(repo).is_dir();
            match head.kind.as_str() {
                "base" => {
                    return if is_dir { apply_base(&b, repo) } else { crate::restore::restore_new_repository(&b, repo) };
                }
                "repository" => {
                    return if sync_all && is_dir {
                        crate::sync::sync_repository(&b, repo)
                    } else if is_dir {
                        apply_repository(&b, repo)
                    } else {
                        crate::restore::restore_new_repository(&b, repo)
                    };
                }
                _ => {}
            }
        }
    }
    apply_feature(bundle, repo)
}

// ───────────────────────── feature 번들 ─────────────────────────

pub fn apply_feature(bundle: &str, repo: &str) -> Outcome {
    say(&format!("===== 적용 시작 — 번들 {} / 저장소 {} / 로컬 적용 전용", bundle, repo));
    if bundle.is_empty() || !Path::new(bundle).is_file() {
        return Outcome::stop("번들 파일이 없습니다.");
    }
    if repo.is_empty() || !Path::new(repo).is_dir() {
        return Outcome::stop("저장소 폴더가 없습니다.");
    }
    let bundle = abs_path(bundle);
    let git = match Git::open(repo) {
        Ok(g) => g,
        Err(e) => return Outcome::stop(e),
    };
    let mut undo = UndoLog::new(&git);
    let mut temp: Vec<String> = Vec::new();
    let r = feature_inner(&git, &bundle, &mut undo, &mut temp);
    finish(&git, &temp, &undo, r)
}

fn feature_inner(git: &Git, bundle: &str, undo: &mut UndoLog, temp: &mut Vec<String>) -> R<Outcome> {
    // 1. 저장소 점검
    if !git.run(&["rev-parse", "--show-toplevel"])?.ok() {
        return Ok(Outcome::stop("고른 폴더가 git 저장소가 아닙니다."));
    }
    if let Some(m) = in_progress_mark(git) {
        return Ok(Outcome::stop(format!("저장소가 머지·리베이스 등 진행 중인 상태입니다 ({}). Git 도구에서 먼저 끝내거나 취소해 주세요.", m)));
    }
    clear_leftover_import_refs(git);

    let head = match get_bundle_head(bundle) {
        Ok(h) => h,
        Err(e) => return Ok(Outcome::stop(e)),
    };
    if head.kind != "feature" {
        return Ok(Outcome::stop("feature 브랜치 번들이 아닙니다. 기준 번들이나 저장소 번들은 자동 판별로 적용해 주세요."));
    }
    let name = head.name.clone();
    let bref = head.ref_.clone();
    let n = head.oid.clone();
    say(&format!("번들 브랜치: {}  끝 {}", name, n));

    // 번들 검증. Feature 적용은 로컬 전용이므로 origin 네트워크에 접속하지 않는다.
    let v = git.run(&["bundle", "verify", bundle])?;
    if !v.ok() {
        if needs_prereq(&v) {
            return Ok(Outcome::stop("이 번들이 기대는 커밋이 저장소에 없다는 이유로 적용하지 못했습니다. 같은 브랜치의 이전 번들을 먼저 넣었는지, 서버 기준 브랜치가 받아졌는지 확인해 주세요."));
        }
        return Ok(Outcome::stop(format!("번들 검증에 실패했습니다. 파일이 깨졌을 수 있습니다.\n{}", v.err)));
    }

    // 번들 브랜치와 함께 온 태그를 한 번에 임시 참조로 받는다. 실패하면 아무것도 바뀌지 않는다.
    let stamp = run_stamp();
    let ntmp = format!("refs/oqms-import/{}-bundle", stamp);
    let tag_base = format!("refs/oqms-import/{}-tags/", stamp);
    temp.push(ntmp.clone());
    temp.push(tag_base.clone());
    let mut specs = vec![format!("{}:{}", bref, ntmp)];
    for t in head.tags.keys() {
        specs.push(format!("refs/tags/{}:{}{}", t, tag_base, t));
    }
    let mut args: Vec<&str> = BUNDLE_FETCH.to_vec();
    args.push("--atomic");
    args.push(bundle);
    args.extend(specs.iter().map(|s| s.as_str()));
    let bf = git.run(&args)?;
    if !bf.ok() {
        return Ok(Outcome::stop(format!("번들을 받지 못했습니다. 아무것도 바꾸지 않았습니다.\n{}", bf.err)));
    }

    // 받은 값과 기존 끝 후보를 한 번에 읽는다. 정확한 이름으로 비교한다.
    let origin_ref = format!("refs/remotes/origin/{}", name);
    let devnet_v2 = format!("{}heads/{}", V2_DEVNET, name);
    let devnet_v1 = format!("{}heads/{}", V1_DEVNET, name);
    let rd = git.q(&["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads/", &origin_ref, &devnet_v2, &devnet_v1, &ntmp, &tag_base])?;
    if !rd.ok() {
        return Ok(Outcome::stop(format!("저장소 브랜치·태그를 읽지 못했습니다.\n{}", rd.detail())));
    }
    let values: BTreeMap<String, String> = rd.lines().into_iter().filter_map(|x| x.split_once(' ')).map(|(a, b)| (a.to_string(), b.to_string())).collect();
    if values.get(&ntmp) != Some(&n) {
        return Ok(Outcome::stop("받은 커밋이 번들 머리 값과 다릅니다."));
    }
    if head.tags.iter().any(|(t, oid)| values.get(&format!("{}{}", tag_base, t)) != Some(oid)) {
        return Ok(Outcome::stop("받은 태그 값이 번들 머리 값과 다릅니다. 아무것도 바꾸지 않았습니다."));
    }

    // Windows 참조 파일은 대소문자를 가리지 않는다. 이름이 대소문자만 다른 브랜치는 같은 파일이 된다.
    if let Some(clash) = case_clash(&bref, values.keys().filter(|x| x.starts_with("refs/heads/")).map(|s| s.as_str())) {
        return Ok(Outcome::stop(format!(
            "대소문자만 다른 로컬 브랜치 {} 가 있어 {} 를 만들거나 옮기지 않고 멈춥니다. 아무것도 바꾸지 않았습니다. 로컬 브랜치 이름을 번들 이름과 맞춘 뒤 다시 실행해 주세요.",
            &clash["refs/heads/".len()..],
            name
        )));
    }

    // 기존 끝 E. 로컬 Feature, 캐시된 origin 추적 참조, 보관한 원본 Feature 중 가장 앞선 값을 고른다.
    // 서로 갈라졌으면 어느 쪽도 덮지 않는다.
    let l = values.get(&bref).cloned();
    let mut known: Vec<(&str, String)> = Vec::new();
    for (label, kr) in [("로컬", &bref), ("캐시된 origin", &origin_ref), ("보관된 원본", &devnet_v2), ("보관된 원본", &devnet_v1)] {
        if let Some(oid) = values.get(kr) {
            if !known.iter().any(|x| x.1 == *oid) {
                known.push((label, oid.clone()));
            }
        }
    }
    let mut e: Option<String> = None;
    let mut e_label = "";
    for (label, oid) in &known {
        let Some(cur) = e.clone() else {
            e = Some(oid.clone());
            e_label = *label;
            continue;
        };
        if is_ancestor(git, &cur, oid)? {
            e = Some(oid.clone());
            e_label = *label;
        } else if !is_ancestor(git, oid, &cur)? {
            return Ok(Outcome::stop(format!("이 저장소에 보존된 {} Feature 이력이 서로 갈라져 있습니다. 어느 쪽도 덮지 않고 아무것도 바꾸지 않았습니다.", name)));
        }
    }
    if let Some(ev) = &e {
        say(&format!("기존 Feature 기준: {} {}", e_label, ev));
    }

    // 판정. 멈춤은 가장 앞선 끝 E 로, 보고는 로컬 브랜치가 실제로 움직인 만큼(L → N)으로 정한다.
    if let Some(ev) = &e {
        if *ev != n && !is_ancestor(git, ev, &n)? {
            if is_ancestor(git, &n, ev)? {
                let mut r = Outcome::stop(format!("원본의 {} 브랜치에 이 번들보다 앞선 커밋이 있습니다. 원본 커밋을 덮어쓰지 않고 아무것도 바꾸지 않았습니다.", name));
                r.branch = Some(name.clone());
                return Ok(r);
            }
            return Ok(Outcome::stop(format!(
                "번들과 기존 {} 이 서로 갈라져 있습니다. 아무것도 바꾸지 않았습니다.\n- 원본에서 이 브랜치에 직접 커밋했다면: 원본 저장소를 작업 PC 로 가져가 기준 저장소를 교체한 뒤 이어서 작업해 주세요.\n- 작업 PC 에서 리베이스했다면: 새 브랜치 이름으로 다시 내보내 주세요.",
                name
            )));
        }
    }
    let kind = match &l {
        None => "새로 만들기",
        Some(x) if *x == n => "이미 들어가 있음",
        _ => "이어 붙이기",
    };
    say(&format!(
        "판정: {}  (기존 끝 {}, 로컬 끝 {}, 번들 끝 {})",
        kind,
        e.as_deref().unwrap_or("없음"),
        l.as_deref().unwrap_or("없음"),
        n
    ));

    // 8. 로컬 브랜치 반영
    if l.as_deref() != Some(n.as_str()) {
        let place = match checkout_place(git, &bref) {
            Ok(p) => p,
            Err(e) => return Ok(Outcome::stop(e)),
        };
        let old = l.clone().unwrap_or_else(|| undo.zero.clone());
        match place {
            Place::Other(p) => {
                return Ok(Outcome::stop(format!("{} 이 다른 작업 폴더({})에서 작업 중인 브랜치라, 그쪽 파일이 옛 상태로 남아 꼬이므로 멈춥니다.", name, p)));
            }
            Place::Here => {
                let st = git.run(&["status", "--porcelain", "--untracked-files=no"])?;
                if !st.ok() {
                    return Ok(Outcome::stop(format!("{} 이 지금 작업 중인 브랜치인데 작업 파일 상태를 확인하지 못했습니다. 아무것도 바꾸지 않았습니다.\n{}", name, st.detail())));
                }
                if !st.out.is_empty() {
                    return Ok(Outcome::stop(format!("{} 이 지금 작업 중인 브랜치이고 수정 중인 파일이 있습니다. 커밋하거나 치운 뒤 다시 실행해 주세요.", name)));
                }
                // 옮기기 전에 덮기 전 값을 기록으로 먼저 남긴다. 기록이 안 되면 옮기지 않는다.
                let rec_fail: Option<String> = match undo.record_before(git, &bref, &old, &n) {
                    Ok(x) if x.ok() => None,
                    Ok(x) => Some(x.detail().to_string()),
                    Err(e) => Some(e),
                };
                if let Some(d) = rec_fail {
                    let mut msg = format!("되돌리기 기록을 남기지 못해 작업 중인 브랜치 {} 를 옮기지 않았습니다. 아무것도 바꾸지 않았습니다.", name);
                    if !d.is_empty() {
                        msg += &format!("\n{}", d);
                    }
                    return Ok(Outcome::stop(msg));
                }
                let mg = git.run(&["merge", "--ff-only", &n])?;
                if !mg.ok() {
                    let mut msg = format!("작업 중인 브랜치를 앞으로 옮기지 못했습니다.\n{}", mg.err);
                    if !committed(undo.forget_last(git, &bref)) {
                        msg += &format!("\n되돌리기 기록 {} 에 끝나지 않은 이동 항목이 남았습니다. 브랜치는 옮기지 않았습니다. 그 기록으로 되돌리기를 실행하면 멈추고 기록을 지우는 방법을 알려 드립니다.", undo.stamp);
                    }
                    return Ok(Outcome::stop(msg));
                }
                // 이동이 끝났으니 기록 항목을 확정한다. 못 하면 이동은 성공으로 두고 안내만 남긴다.
                if !committed(undo.confirm_last(git, &bref)) {
                    say(&format!("되돌리기 기록 {} 를 확정하지 못해 그 기록으로 되돌리기는 멈춥니다. 브랜치는 옮겨졌습니다.", undo.stamp));
                }
            }
            Place::None => {
                let u = undo.commit(git, &[(bref.clone(), old, n.clone())], &[])?;
                if !u.ok() {
                    return Ok(Outcome::stop(format!("로컬 브랜치를 반영하지 못했습니다. 그사이 누가 바꿨을 수 있습니다.\n{}", u.detail())));
                }
            }
        }
        if ref_commit(git, &bref)?.as_deref() != Some(n.as_str()) {
            return Ok(Outcome::stop("로컬 브랜치 값이 예상과 다릅니다."));
        }
    }

    // 8-1. 함께 온 태그. 로컬에 없으면 만들고, 같은 이름의 다른 커밋은 원본 값으로 옮긴다(옛 값은 되돌리기 기록에 남는다).
    let mut tag_note = String::new();
    let mut tags_left = 0usize;
    if !head.tags.is_empty() {
        let sources: BTreeMap<String, String> = head.tags.keys().map(|t| (t.clone(), format!("{}{}", tag_base, t))).collect();
        match place_missing_tags(git, &sources, undo) {
            Err(e) => {
                let mut r = Outcome::new("일부 적용", format!("{} 브랜치는 반영했지만 함께 온 태그를 만들지 못했습니다.\n{}", name, e), false);
                r.branch = Some(name.clone());
                return Ok(r);
            }
            Ok(t) => {
                tags_left = t.conflicts.len() + t.skipped.len();
                tag_note = format!(" {}", tag_summary(&t));
            }
        }
    }

    let mut res = Outcome::new(if tags_left == 0 { kind } else { "일부 적용" }, "", tags_left == 0);
    res.branch = Some(name.clone());

    // 9. 결과 정리. 원격 게시와 develop 병합은 원본에서 사람이 별도로 수행한다.
    let base_b = local_base_branch(git);
    let base_ref = base_b.as_ref().map(|b| format!("refs/remotes/origin/{}", b));
    let rng = match (&l, &base_ref) {
        (Some(lv), _) => format!("{}..{}", lv, n),
        (None, Some(br)) => format!("{}..{}", br, n),
        _ => n.clone(),
    };
    if kind != "이미 들어가 있음" {
        res.commits = log_commits(git, &["-n", "50", &rng]);
    }
    if let Some(br) = &base_ref {
        let cnt = git.q(&["rev-list", "--count", &format!("{}..{}", n, br)])?;
        if cnt.ok() {
            res.main_ahead = cnt.out.trim().parse::<i64>().ok();
        }
    }
    res.message = match kind {
        "새로 만들기" => format!("{} 브랜치를 새로 만들었습니다.", name),
        "이어 붙이기" => {
            let count = git.q(&["rev-list", "--count", &rng])?;
            let total = if count.ok() && count.out.trim().chars().all(|c| c.is_ascii_digit()) && !count.out.trim().is_empty() {
                count.out.trim().to_string()
            } else {
                "확인 불가".to_string()
            };
            format!("{} 뒤에 커밋 {}개를 이어 붙였습니다.", name, total)
        }
        _ => format!("{} 에 이미 들어가 있는 번들입니다.", name),
    };
    if let Some(ev) = &e {
        if l.as_deref() != Some(ev.as_str()) {
            if *ev == n {
                res.message += &format!(" 원본 브랜치({})에는 이미 있던 커밋이라 로컬 브랜치만 맞췄습니다.", e_label);
            } else {
                res.message += &format!(" 그중 {} 까지는 원본 브랜치({})에 이미 있던 커밋입니다.", &ev[..8.min(ev.len())], e_label);
            }
        }
    }
    res.message += &tag_note;
    Ok(res)
}

// ───────────────────────── 기준 번들 ─────────────────────────

pub fn apply_base(bundle: &str, repo: &str) -> Outcome {
    say(&format!("===== 기준 번들 적용 — 번들 {} / 저장소 {}", bundle, repo));
    let git = match Git::open(repo) {
        Ok(g) => g,
        Err(e) => return Outcome::stop(e),
    };
    let mut undo = UndoLog::new(&git);
    let mut temp: Vec<String> = Vec::new();
    let r = base_inner(&git, bundle, &mut undo, &mut temp);
    finish(&git, &temp, &undo, r)
}

fn base_inner(git: &Git, bundle: &str, undo: &mut UndoLog, temp: &mut Vec<String>) -> R<Outcome> {
    if !git.run(&["rev-parse", "--show-toplevel"])?.ok() {
        return Ok(Outcome::stop("고른 폴더가 git 저장소가 아닙니다."));
    }
    if let Some(m) = in_progress_mark(git) {
        return Ok(Outcome::stop(format!("저장소가 머지·리베이스 등 진행 중인 상태입니다 ({}). 먼저 끝내거나 취소해 주세요.", m)));
    }
    clear_leftover_import_refs(git);

    let head = get_bundle_head(bundle)?;
    if head.kind != "base" {
        return Ok(Outcome::stop("기준 브랜치 번들이 아닙니다."));
    }
    let b = head.branch.clone();
    let track_ref = format!("refs/remotes/origin/{}", b);

    let v = git.run(&["bundle", "verify", bundle])?;
    if !v.ok() {
        if needs_prereq(&v) {
            return Ok(Outcome::stop(format!("이 번들이 기대는 커밋이 저장소에 없다는 이유로 적용하지 못했습니다. 중간 {} 번들을 빠뜨렸거나 다른 저장소용 번들입니다. 원본에서 [전체로 만들기]로 다시 뽑아 주세요.", b)));
        }
        return Ok(Outcome::stop(format!("번들 검증에 실패했습니다.\n{}", v.err)));
    }

    let tmp = format!("refs/oqms-import/{}-base", run_stamp());
    temp.push(tmp.clone());
    let spec = format!("{}:{}", track_ref, tmp);
    let mut args: Vec<&str> = BUNDLE_FETCH.to_vec();
    args.extend([bundle, spec.as_str()]);
    let bf = git.run(&args)?;
    if !bf.ok() {
        return Ok(Outcome::stop(format!("번들을 받지 못했습니다.\n{}", bf.err)));
    }
    let Some(n) = ref_commit(git, &tmp)? else {
        return Ok(Outcome::stop("번들을 받지 못했습니다.\n임시로 받은 기준 커밋을 읽지 못했습니다."));
    };
    apply_base_value(git, &b, &n, "기준", undo)
}

/// 1.0.5 _apply_base_value. 바뀐 참조는 undo 에 남긴다.
pub(crate) fn apply_base_value(git: &Git, branch: &str, new_oid: &str, kind_prefix: &str, undo: &mut UndoLog) -> R<Outcome> {
    let b = branch;
    let n = new_oid;
    let track_ref = format!("refs/remotes/origin/{}", b);
    let local_ref = format!("refs/heads/{}", b);
    let orig = ref_commit(git, &track_ref)?;
    let l = ref_commit(git, &local_ref)?;
    // 대소문자만 다른 로컬 브랜치가 있으면 Windows 에서 같은 참조 파일이라 로컬 브랜치는 건드리지 않는다.
    let clash = local_branch_clash(git, &local_ref)?;
    let probe = match (&orig, &l) {
        (Some(x), _) | (None, Some(x)) => Some(x.clone()),
        _ => ref_commit(git, "HEAD")?,
    };
    if let Some(p) = &probe {
        if !git.q(&["merge-base", p, n])?.ok() {
            return Ok(Outcome::stop("이 번들은 이 저장소와 공통 조상이 없습니다. 다른 저장소용 번들 같습니다."));
        }
    }

    let kind: &str = match &orig {
        Some(o) if o == n => "이미 최신",
        None => "기준 갱신",
        Some(o) => {
            if is_ancestor(git, o, n)? {
                "기준 갱신"
            } else if is_ancestor(git, n, o)? {
                let mut res = Outcome::new("예전 번들", format!("이 {} 번들은 이미 반영된 것보다 오래됐습니다. 아무것도 바꾸지 않았습니다.", b), true);
                res.branch = Some(b.to_string());
                res.is_base = true;
                return Ok(res);
            } else {
                return Ok(Outcome::stop(format!("원본 {} 기준 값과 번들이 서로 갈라져 있습니다. 자동으로 덮지 않았습니다.", b)));
            }
        }
    };

    if kind == "기준 갱신" {
        let old = orig.clone().unwrap_or_else(|| undo.zero.clone());
        let u = undo.commit(git, &[(track_ref.clone(), old, n.to_string())], &[])?;
        if !u.ok() {
            return Ok(Outcome::stop(format!("원본 {} 기준 값을 갱신하지 못했습니다.\n{}", b, u.detail())));
        }
    }

    let note: String = if let Some(c) = &clash {
        format!("대소문자만 다른 로컬 브랜치 {} 가 있어 로컬 {} 는 만들거나 옮기지 않았습니다. 이름을 맞춘 뒤 다시 적용해 주세요.", c, b)
    } else if let Some(lv) = &l {
        if lv == n {
            format!("로컬 {} 도 이미 같은 위치입니다.", b)
        } else if is_ancestor(git, lv, n)? {
            match checkout_place(git, &local_ref)? {
                Place::None => {
                    let ok = committed(undo.commit(git, &[(local_ref.clone(), lv.clone(), n.to_string())], &[]));
                    if ok {
                        format!("로컬 {} 를 앞으로 옮겼습니다.", b)
                    } else {
                        format!("로컬 {} 를 옮기지 못했습니다.", b)
                    }
                }
                Place::Here => {
                    let st = git.run(&["status", "--porcelain", "--untracked-files=no"])?;
                    if !st.ok() {
                        format!("로컬 {} 가 지금 작업 중인 브랜치인데 작업 파일 상태를 확인하지 못해 옮기지 않았습니다.", b)
                    } else if !st.out.is_empty() {
                        format!("로컬 {} 가 지금 작업 중인 브랜치이고 수정 중인 파일이 있어 옮기지 않았습니다. 정리한 뒤 다시 적용하면 옮깁니다.", b)
                    } else {
                        // 옮기기 전에 덮기 전 값을 기록으로 먼저 남긴다. 기록이 안 되면 옮기지 않는다.
                        let rec = matches!(undo.record_before(git, &local_ref, lv, n), Ok(x) if x.ok());
                        if !rec {
                            format!("작업 중인 로컬 {} 는 되돌리기 기록을 남기지 못해 옮기지 않았습니다.", b)
                        } else {
                            let mg = git.run(&["merge", "--ff-only", n])?;
                            if mg.ok() {
                                let moved = format!("작업 중인 로컬 {} 를 작업 폴더까지 앞으로 옮겼습니다.", b);
                                if committed(undo.confirm_last(git, &local_ref)) {
                                    moved
                                } else {
                                    format!("{} 되돌리기 기록 {} 를 확정하지 못해 그 기록으로 되돌리기는 멈춥니다. 브랜치는 옮겨졌습니다.", moved, undo.stamp)
                                }
                            } else {
                                let base = format!("작업 중인 로컬 {} 를 옮기지 못했습니다.", b);
                                if committed(undo.forget_last(git, &local_ref)) {
                                    base
                                } else {
                                    format!("{} 되돌리기 기록 {} 에 끝나지 않은 이동 항목이 남았습니다. 브랜치는 옮기지 않았습니다. 그 기록으로 되돌리기를 실행하면 멈추고 기록을 지우는 방법을 알려 드립니다.", base, undo.stamp)
                                }
                            }
                        }
                    }
                }
                Place::Other(p) => format!("로컬 {} 가 다른 작업 폴더({})에서 작업 중인 브랜치라 옮기지 않았습니다.", b, p),
            }
        } else if is_ancestor(git, n, lv)? {
            let ahead = git.q(&["rev-list", "--count", lv, &format!("^{}", n)])?.out.trim().to_string();
            format!("로컬 {} 에 원본에 없는 커밋이 {}개 있어 그대로 뒀습니다.", b, ahead)
        } else {
            format!("로컬 {} 가 원본 {} 와 갈라져 있어 그대로 뒀습니다. 확인이 필요합니다.", b, b)
        }
    } else {
        let ok = committed(undo.commit(git, &[(local_ref.clone(), undo.zero.clone(), n.to_string())], &[]));
        if ok {
            format!("로컬 {} 가 없어서 새로 만들었습니다.", b)
        } else {
            format!("로컬 {} 를 만들지 못했습니다.", b)
        }
    };

    let msg = if kind == "기준 갱신" { format!("원본 {} 기준 값을 새 번들로 갱신했습니다.", b) } else { format!("원본 {} 기준 값이 이미 이 번들과 같습니다.", b) };
    let res_kind = if kind_prefix == "기준" { kind.to_string() } else { format!("{} · {}", kind_prefix, kind) };
    let mut res = Outcome::new(&res_kind, msg, true);
    res.branch = Some(b.to_string());
    res.push_note = Some(note);
    res.is_base = true;
    let local_after = if clash.is_some() { None } else { ref_commit(git, &local_ref)? };
    let reached = match &local_after {
        Some(la) => is_ancestor(git, n, la)?,
        None => false,
    };
    if !reached {
        res.kind = "일부 적용".into();
        res.ok = false;
        res.message += &format!(" 로컬 {} 브랜치는 아직 번들 커밋을 포함하지 못했습니다.", b);
    }
    if kind == "기준 갱신" {
        let excl = orig.as_ref().map(|o| format!("^{}", o));
        let mut commits = match &excl {
            Some(x) => log_commits(git, &[n, x.as_str()]),
            None => log_commits(git, &["-n", "50", n]),
        };
        commits.truncate(50);
        res.commits = commits;
    }
    Ok(res)
}

/// 로컬 브랜치가 번들 커밋을 포함하는지 본다. 앞서 있는 경우도 포함으로 친다.
fn local_reaches(git: &Git, branch: &str, oid: &str) -> R<bool> {
    let r = format!("refs/heads/{}", branch);
    if local_branch_clash(git, &r)?.is_some() {
        return Ok(false);
    }
    match ref_commit(git, &r)? {
        Some(local) => Ok(local == oid || is_ancestor(git, oid, &local)?),
        None => Ok(false),
    }
}

/// 보류 사유를 구체적으로 만든다. 기준 값 갱신 문구만 사유로 읽히지 않게 한다.
fn hold_reason(sub: &Outcome, branch: &str) -> String {
    let mut note = sub.push_note.clone().unwrap_or_default().trim().replace('\n', " ");
    if sub.kind == "예전 번들" {
        note = format!("origin/{} 추적 값이 번들보다 최신이라 예전 번들로 처리했고 기준 값은 바꾸지 않았습니다. {}", branch, note);
    }
    if note.is_empty() {
        note = sub.message.clone();
    }
    if sub.kind == "멈춤" {
        return note;
    }
    format!("{} 로컬 {} 는 번들 커밋을 포함하지 못했습니다.", note, branch)
}

// ───────────────────────── 저장소 참조 번들 ─────────────────────────

/// 원본 참조를 보관하고 기준 브랜치와 포함된 develop·test를 안전하게 전진시킨다.
pub fn apply_repository(bundle: &str, repo: &str) -> Outcome {
    say(&format!("===== 저장소 번들 적용 — 번들 {} / 저장소 {}", bundle, repo));
    let git = match Git::open(repo) {
        Ok(g) => g,
        Err(e) => return Outcome::stop(e),
    };
    let mut undo = UndoLog::new(&git);
    let mut temp: Vec<String> = Vec::new();
    let r = repository_inner(&git, bundle, &mut undo, &mut temp);
    finish(&git, &temp, &undo, r)
}

fn repository_inner(git: &Git, bundle: &str, undo: &mut UndoLog, temp: &mut Vec<String>) -> R<Outcome> {
    if !git.run(&["rev-parse", "--show-toplevel"])?.ok() {
        return Ok(Outcome::stop("고른 폴더가 git 저장소가 아닙니다."));
    }
    if let Some(m) = in_progress_mark(git) {
        return Ok(Outcome::stop(format!("저장소가 머지·리베이스 등 진행 중인 상태입니다 ({}).", m)));
    }
    let head = get_bundle_head(bundle)?;
    if head.kind != "repository" {
        return Ok(Outcome::stop("저장소 번들이 아닙니다."));
    }
    let v = git.run(&["bundle", "verify", bundle])?;
    if !v.ok() {
        return Ok(Outcome::stop(format!("번들 검증에 실패했습니다.\n{}", v.detail())));
    }
    let branch = head.branch.clone();
    let stamp = run_stamp();
    let stage = format!("refs/oqms-import/{}-repository", stamp);
    temp.push(format!("{}/", stage));

    // 매니페스트에는 증분이어도 그 범위의 전체 상태가 적혀 있다. 없으면(1.0.5 번들) 번들에 든 참조가 전부다.
    let mut manifest: Option<Manifest> = None;
    if head.has_manifest {
        let mref = format!("refs/oqms-import/{}-manifest", stamp);
        temp.push(mref.clone());
        let spec = format!("+{}:{}", V2_MANIFEST, mref);
        let mut args: Vec<&str> = BUNDLE_FETCH.to_vec();
        args.extend([bundle, spec.as_str()]);
        let f = git.run(&args)?;
        if !f.ok() {
            return Ok(Outcome::stop(format!("번들의 목록 파일을 받지 못했습니다.\n{}", f.detail())));
        }
        match Manifest::read(git, &mref) {
            Ok(m) => manifest = Some(m),
            Err(e) => return Ok(Outcome::stop(e)),
        }
    }
    let (heads, tags, incremental) = match &manifest {
        Some(m) => (m.heads.clone(), m.tags.clone(), m.incremental),
        None => (head.heads.clone(), head.tags.clone(), false),
    };
    if let Some(m) = &manifest {
        if !m.default.is_empty() && m.default != branch {
            return Ok(Outcome::stop("목록 파일의 기본 브랜치가 번들 식별 정보와 다릅니다. 아무것도 바꾸지 않았습니다."));
        }
    }
    let Some(n) = heads.get(&branch).cloned() else {
        return Ok(Outcome::stop(format!("번들 목록 파일에 기본 브랜치 {} 의 값이 없습니다. 아무것도 바꾸지 않았습니다.", branch)));
    };
    for (nm, o) in heads.iter().chain(tags.iter()) {
        if !is_oid(o) {
            return Ok(Outcome::stop(format!("번들 안 브랜치·태그 값이 올바르지 않습니다: {} = {}", nm, o)));
        }
    }
    if manifest.is_some() {
        let diff = head.heads.iter().find(|(k, o)| heads.get(*k) != Some(*o)).map(|x| x.0.clone()).or_else(|| head.tags.iter().find(|(k, o)| tags.get(*k) != Some(*o)).map(|x| x.0.clone()));
        if let Some(d) = diff {
            return Ok(Outcome::stop(format!("번들에 든 브랜치·태그 값이 목록 파일과 다릅니다: {}", d)));
        }
    }

    // 번들에 든 참조를 임시 영역으로 한꺼번에 받는다. 실패하면 아무것도 바뀌지 않는다.
    let mut specs: Vec<String> = Vec::new();
    for nm in head.heads.keys() {
        specs.push(format!("+{}:{}/heads/{}", head.source_head(nm), stage, nm));
    }
    for nm in head.tags.keys() {
        specs.push(format!("+{}:{}/tags/{}", head.source_tag(nm), stage, nm));
    }
    if !specs.is_empty() {
        let mut args: Vec<&str> = BUNDLE_FETCH.to_vec();
        args.push("--atomic");
        args.push(bundle);
        args.extend(specs.iter().map(|s| s.as_str()));
        let f = git.run(&args)?;
        if !f.ok() {
            return Ok(Outcome::stop(format!("저장소 브랜치·태그를 임시 보관 공간으로 받지 못했습니다.\n{}", f.err)));
        }
    }
    let got = ref_map(git, &[&format!("{}/", stage)])?;
    let got_h = strip_map(&got, &format!("{}/heads/", stage));
    let got_t = strip_map(&got, &format!("{}/tags/", stage));
    for (nm, oid) in &head.heads {
        if got_h.get(nm) != Some(oid) {
            return Ok(Outcome::stop(if *nm == branch {
                "임시로 받은 저장소 기준 커밋이 번들 식별 정보와 다릅니다.".to_string()
            } else {
                format!("임시로 받은 브랜치 값이 번들 식별 정보와 다릅니다: {}", nm)
            }));
        }
    }
    for (nm, oid) in &head.tags {
        if got_t.get(nm) != Some(oid) {
            return Ok(Outcome::stop(format!("임시로 받은 태그 값이 번들 식별 정보와 다릅니다: {}", nm)));
        }
    }

    // bundle verify 는 연결성만 검사하고 객체를 저장소에 넣지는 않는다. 위에서 받은 뒤라야 계보를 판정할 수 있다.
    if ref_commit(git, &n)?.is_none() {
        return Ok(Outcome::stop(format!("기본 브랜치 {} 의 커밋 {} 가 이 저장소에 없습니다. 앞선 번들을 먼저 적용했는지 확인해 주세요.", branch, n)));
    }
    let probe = match ref_commit(git, &format!("refs/remotes/origin/{}", branch))? {
        Some(x) => Some(x),
        None => match ref_commit(git, &format!("refs/heads/{}", branch))? {
            Some(x) => Some(x),
            None => ref_commit(git, "HEAD")?,
        },
    };
    if let Some(p) = &probe {
        let anc = git.q(&["merge-base", p, &n])?;
        if anc.code == 1 {
            return Ok(Outcome::stop("이 저장소 번들은 고른 저장소와 공통 조상이 없습니다."));
        }
        if !anc.ok() {
            return Ok(Outcome::stop(format!("저장소 이력을 확인하지 못했습니다.\n{}", anc.detail())));
        }
    }

    // 원본 보관 참조. 번들에 없는 브랜치·태그는 매니페스트 값이 이 저장소에 있어야 한다.
    let mut devnet: BTreeMap<String, String> = BTreeMap::new();
    let mut missing_tags: Vec<String> = Vec::new();
    for (nm, o) in &heads {
        if !head.heads.contains_key(nm) && !git.q(&["cat-file", "-e", o])?.ok() {
            return Ok(Outcome::stop(format!("바뀐 부분만 담은 번들이 기대는 브랜치 {} 의 커밋이 저장소에 없습니다. 앞선 번들을 먼저 적용했는지 확인해 주세요.", nm)));
        }
        devnet.insert(format!("{}heads/{}", V2_DEVNET, nm), o.clone());
    }
    for (nm, o) in &tags {
        if !head.tags.contains_key(nm) && !git.q(&["cat-file", "-e", o])?.ok() {
            missing_tags.push(nm.clone());
            continue;
        }
        devnet.insert(format!("{}tags/{}", V2_DEVNET, nm), o.clone());
    }
    let current = ref_map(git, &[V2_DEVNET, V1_DEVNET])?;
    let zero = undo.zero.clone();
    let mut changes: Vec<(String, String, String)> = Vec::new();
    for (r, o) in &devnet {
        match current.get(r) {
            Some(c) if c == o => {}
            Some(c) => changes.push((r.clone(), c.clone(), o.clone())),
            None => changes.push((r.clone(), zero.clone(), o.clone())),
        }
    }
    if !changes.is_empty() {
        let u = undo.commit(git, &changes, &[])?;
        if !u.ok() {
            return Ok(Outcome::stop(format!("저장소 브랜치·태그를 임시 보관 공간으로 받지 못했습니다.\n{}", u.detail())));
        }
    }
    // 이번 번들에 없는 옛 보관 참조는 읽은 값과 비교해 한 번에 지운다(undo 기록에 남는다).
    // 지우는 대상은 그 번들의 범위로 제한한다. all 은 보관 참조 전체, standard 는 태그와 기본 브랜치·develop 만이다.
    // standard 번들은 all 로 받아 둔 다른 브랜치(feature/x 등)의 보관 참조를 지우지 않는다. 그 밖의 범위는 정리하지 않는다.
    let mut prune_note = String::new();
    let mut pruned = 0usize;
    if !head.native {
        let scope: String = match &manifest {
            Some(m) => m.scope.clone(),
            None => head.scope.clone(),
        };
        let stale: Vec<(String, String, String)> = current
            .iter()
            .filter(|(r, _)| !devnet.contains_key(*r) && in_prune_scope(r.as_str(), &scope, &branch))
            .map(|(r, o)| (r.clone(), o.clone(), zero.clone()))
            .collect();
        if !stale.is_empty() {
            match undo.commit(git, &stale, &[]) {
                Ok(r) if r.ok() => pruned = stale.len(),
                Ok(r) => prune_note = format!("이번 번들에 없는 옛 보관 브랜치·태그 {}개를 지우지 못해 그대로 뒀습니다.\n{}", stale.len(), r.detail()),
                Err(e) => prune_note = format!("옛 보관 브랜치·태그 목록을 읽지 못해 정리하지 않았습니다. {}", e),
            }
        }
    }

    // 로컬에 없는 태그는 보관한 원본 값으로 만든다. 같은 이름의 다른 커밋은 덮지 않는다.
    let sources: BTreeMap<String, String> = tags
        .keys()
        .filter(|nm| devnet.contains_key(&format!("{}tags/{}", V2_DEVNET, nm)))
        .map(|nm| (nm.clone(), format!("{}tags/{}", V2_DEVNET, nm)))
        .collect();
    let mut tr = place_missing_tags(git, &sources, undo)?;
    tr.skipped.extend(missing_tags);
    let tags_left = tr.conflicts.len() + tr.skipped.len();
    let local_heads = ref_map(git, &["refs/heads/"])?;
    let branch_differences = heads.iter().filter(|(nm, oid)| matches!(local_heads.get(&format!("refs/heads/{}", nm)), Some(l) if l != *oid)).count();

    let mut res = apply_base_value(git, &branch, &n, "저장소 보관", undo)?;
    let original_hold_reason = hold_reason(&res, &branch);
    let mut extra = format!(
        "원본 브랜치 {}개·태그 {}개를 {} 아래에 보관했습니다.",
        heads.len(),
        tags.len(),
        V2_DEVNET.trim_end_matches('/')
    );
    if incremental {
        extra += &format!(" 바뀐 부분만 담은 번들이라 바뀐 브랜치 {}개·태그 {}개를 받고 나머지는 목록 파일 값으로 맞췄습니다.", head.heads.len(), head.tags.len());
    }
    if pruned > 0 {
        extra += &format!(" 원본에서 없어진 보관 브랜치·태그 {}개를 지웠습니다.", pruned);
    }
    if branch_differences > 0 {
        extra += &format!(" 적용 전 로컬과 값이 다른 브랜치 {}개를 확인했습니다. 각 반영 결과는 아래와 같습니다.", branch_differences);
    }
    if !tags.is_empty() {
        extra += &format!(" {}", tag_summary(&tr));
    }
    res.message = format!("{} {}", extra, res.message);
    if !prune_note.is_empty() {
        res.push_note = join_notes(&res.push_note, &Some(prune_note));
    }
    // 기본 브랜치가 계보 문제 등으로 멈췄으면 바로 끝낸다. 로컬만 보류된 경우는 계속한다.
    let extra_names: Vec<String> = ["develop", "test"].iter().filter(|x| **x != branch && heads.contains_key(**x)).map(|x| x.to_string()).collect();
    if !res.ok && res.kind != "일부 적용" {
        res.message += &format!(" {} 반영 보류: {}", branch, original_hold_reason);
        if !extra_names.is_empty() {
            res.message += &format!(" 번들에 든 {} 은(는) 시도하지 않았습니다.", extra_names.join(", "));
        }
        return Ok(res);
    }
    // 예전 번들 조기 반환 등으로 기준 값만 처리됐어도 로컬이 번들 커밋에 도달했는지 따로 확인한다.
    let mut held: Vec<String> = Vec::new();
    let mut base_reason: Option<String> = None;
    if res.ok && !local_reaches(git, &branch, &n)? {
        base_reason = Some(original_hold_reason.clone());
        res.kind = "일부 적용".into();
        res.ok = false;
    }
    if !res.ok {
        held.push(branch.clone());
        res.message += &format!(" {} 반영 보류: {}", branch, base_reason.unwrap_or(original_hold_reason));
    }
    // 기본 브랜치 외에 번들에 든 develop·test 도 같은 안전 규칙으로 갱신한다(중복 제외).
    for nm in &extra_names {
        let oid = heads[nm].clone();
        if ref_commit(git, &oid)?.is_none() {
            held.push(nm.clone());
            res.message += &format!(" {} 반영 보류: 커밋 {} 가 이 저장소에 없습니다.", nm, oid);
            continue;
        }
        let mut sub = apply_base_value(git, nm, &oid, "저장소 보관", undo)?;
        res.push_note = join_notes(&res.push_note, &sub.push_note);
        res.commits.extend(sub.commits.iter().cloned());
        res.commits.truncate(50);
        if sub.ok && !local_reaches(git, nm, &oid)? {
            sub.ok = false;
        }
        if !sub.ok {
            held.push(nm.clone());
            res.message += &format!(" {} 반영 보류: {}", nm, hold_reason(&sub, nm));
            continue;
        }
        res.message += &format!(" {}: {}", nm, sub.message);
        if sub.kind.contains("기준 갱신") && res.ok {
            res.kind = "저장소 보관 · 기준 갱신".into();
        }
    }
    if !held.is_empty() {
        res.kind = "일부 적용".into();
        res.ok = false;
        res.message += &format!(" 로컬 반영이 보류된 브랜치: {}.", held.join(", "));
    }
    // 태그가 없거나 다른 커밋과 충돌해 만들지 못한 것이 있으면 일부만 적용한 것이다.
    if tags_left > 0 && res.ok {
        res.kind = "일부 적용".into();
        res.ok = false;
        res.message += &format!(" 태그 {}개는 만들지 못해 일부만 적용했습니다.", tags_left);
    }
    Ok(res)
}
