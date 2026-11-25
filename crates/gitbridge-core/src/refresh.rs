//! 원본 서버 참조를 읽어 현재 브랜치와 체크아웃하지 않은 로컬 브랜치를 안전하게 전진시킨다(1.0.5 refresh.py).
//! 2.0.0 은 로컬 브랜치를 옮기기 전 값을 되돌리기 기록에 함께 남긴다.

use crate::gitrun::Git;
use crate::refs::*;
use crate::snapshot::Snapshot;
use crate::undo::UndoLog;
use crate::R;
use std::collections::BTreeMap;

const STANDARD_BRANCHES: [&str; 4] = ["main", "production", "develop", "master"];

fn branch_details(git: &Git, remote: &BTreeMap<String, String>, local: &BTreeMap<String, String>, statuses: &BTreeMap<String, String>) -> Vec<String> {
    let local_only: BTreeMap<&str, &String> =
        local.iter().filter_map(|(r, o)| r.strip_prefix("refs/heads/").filter(|n| !remote.contains_key(*n)).map(|n| (n, o))).collect();
    let mut oids: Vec<&str> = Vec::new();
    for o in remote.values().chain(local_only.values().copied()) {
        if !oids.contains(&o.as_str()) {
            oids.push(o);
        }
    }
    let mut summaries: BTreeMap<String, (String, String)> = BTreeMap::new();
    for chunk in oids.chunks(64) {
        let mut args = vec!["log", "--no-walk=unsorted", "--date=format-local:%Y-%m-%d %H:%M", "--format=%H%x09%cd%x09%s"];
        args.extend_from_slice(chunk);
        match git.q(&args) {
            Ok(r) if r.ok() => {
                for line in r.lines() {
                    let p: Vec<&str> = line.splitn(3, '\t').collect();
                    if p.len() == 3 {
                        summaries.insert(p[0].into(), (p[1].into(), p[2].into()));
                    }
                }
            }
            Ok(r) => {
                crate::log::say(&format!("서버 브랜치 마지막 커밋 정보 조회 실패: {}", r.detail()));
                break;
            }
            Err(e) => {
                crate::log::say(&format!("서버 브랜치 마지막 커밋 정보 조회 실패: {}", e));
                break;
            }
        }
    }
    let unknown = ("시각 확인 불가".to_string(), "메시지 확인 불가".to_string());
    let prio = |s: Option<&String>| match s.map(|x| x.as_str()) {
        Some("로컬·서버 이력 갈라짐" | "로컬에 추가 커밋 있음" | "다른 작업 폴더에서 사용 중" | "현재 브랜치 갱신 보류") => 0,
        Some("갱신" | "생성") => 1,
        Some("이미 최신") => 3,
        _ => 2,
    };
    let mut rows = Vec::new();
    for (name, oid) in &local_only {
        let (d, s) = summaries.get(*oid).unwrap_or(&unknown);
        rows.push(format!("{} [로컬에만 있는 브랜치] · 로컬 {} {} · {}", name, d, &oid[..8], s));
    }
    let mut names: Vec<&String> = remote.keys().collect();
    names.sort_by_key(|n| (prio(statuses.get(*n)), n.to_lowercase()));
    for name in names {
        let oid = &remote[name];
        let status = statuses.get(name).map(|s| s.as_str()).unwrap_or("서버에만 있음");
        let (d, s) = summaries.get(oid).unwrap_or(&unknown);
        let mut line = format!("{} [{}] · 서버 {} {} · {}", name, status, d, &oid[..8], s);
        if let Some(lo) = local.get(&format!("refs/heads/{}", name)) {
            if lo != oid && status != "갱신" && status != "생성" {
                line += &format!(" · 로컬 {}", &lo[..8]);
            }
        }
        rows.push(line);
    }
    rows
}

pub fn refresh(repo: &str) -> Outcome {
    crate::log::say(&format!("===== 서버 최신화 — 저장소 {}", repo));
    let git = match Git::open(repo) {
        Ok(g) => g,
        Err(e) => return Outcome::stop(e),
    };
    refresh_inner(&git, None).unwrap_or_else(|e| Outcome::stop(format!("예상하지 못한 오류로 멈췄습니다: {}", e)))
}

/// 번들 만들기 앞의 최신화. 이미 받아 둔 서버 스냅샷(origin 추적 참조 포함)의 값으로 로컬 브랜치를 옮겨, 최신화와 번들이 같은 서버 값을 쓴다.
pub fn refresh_with_snapshot(git: &Git, snap: &Snapshot) -> Outcome {
    crate::log::say("===== 서버 최신화 — 번들 만들기 앞 단계");
    refresh_inner(git, Some(snap)).unwrap_or_else(|e| Outcome::stop(format!("예상하지 못한 오류로 멈췄습니다: {}", e)))
}

fn refresh_inner(git: &Git, snap: Option<&Snapshot>) -> R<Outcome> {
    if !git.q(&["rev-parse", "--show-toplevel"])?.ok() {
        return Ok(Outcome::stop("고른 폴더가 git 저장소가 아닙니다."));
    }
    if let Some(m) = in_progress_mark(git) {
        return Ok(Outcome::stop(format!("저장소가 머지·리베이스 등 진행 중인 상태입니다 ({}).", m)));
    }
    if !git.q(&["remote", "get-url", "origin"])?.ok() {
        return Ok(Outcome::stop("서버 주소(origin)가 없어 서버 최신화를 할 수 없습니다."));
    }
    if snap.is_none() {
        let f = git.run(&["fetch", "--atomic", "--prune", "--no-tags", "origin", "+refs/heads/*:refs/remotes/origin/*"])?;
        if !f.ok() {
            return Ok(Outcome::stop(format!("서버 브랜치를 읽지 못했습니다. 네트워크와 GitLab 인증을 확인해 주세요.\n{}", f.detail())));
        }
    }
    let local = ref_map(git, &["refs/heads/"])?;
    let remote: BTreeMap<String, String> = if let Some(s) = snap {
        s.heads.clone()
    } else {
        strip_map(&ref_map(git, &["refs/remotes/origin/"]).or_else(|_| -> R<_> {
            // origin/HEAD 는 간접 참조라 ref_map 이 거절한다. 그때는 직접 읽는다.
            let r = git.q(&["for-each-ref", "--format=%(refname)\t%(objectname)", "refs/remotes/origin/"])?;
            Ok(r.lines().iter().filter_map(|l| l.split_once('\t')).map(|(a, b)| (a.to_string(), b.to_string())).collect())
        })?, "refs/remotes/origin/")
        .into_iter()
        .filter(|(n, _)| n != "HEAD")
        .collect()
    };
    let checked = worktree_branches(git)?;
    let cur = git.q(&["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let current_branch = if cur.ok() { cur.out.trim().to_string() } else { String::new() };

    let (mut updated, mut current_n, mut created_n, mut latest, mut checked_n, mut dirty, mut ahead, mut diverged_n, mut no_remote) =
        (0, 0, 0, 0, 0, 0, 0, 0, 0);
    let mut updates: Vec<(String, String, String)> = Vec::new();
    let mut creates: Vec<(String, String, String)> = Vec::new();
    let mut current_update: Option<(String, String, String)> = None;
    let mut changed = Vec::new();
    let mut statuses: BTreeMap<String, String> = BTreeMap::new();
    let mut diverged = Vec::new();
    let mut current_blocked = Vec::new();
    for (r, old) in &local {
        let name = &r["refs/heads/".len()..];
        let Some(new) = remote.get(name) else {
            no_remote += 1;
            continue;
        };
        if old == new {
            latest += 1;
            statuses.insert(name.into(), "이미 최신".into());
            continue;
        }
        let rel = (|| -> R<u8> {
            if is_ancestor(git, old, new)? {
                Ok(0)
            } else if is_ancestor(git, new, old)? {
                Ok(1)
            } else {
                Ok(2)
            }
        })();
        let rel = match rel {
            Ok(x) => x,
            Err(e) => return Ok(Outcome::stop(format!("브랜치 관계를 확인하지 못했습니다: {}\n{}", name, e))),
        };
        match rel {
            0 => {
                if checked.contains_key(r) {
                    if name == current_branch {
                        current_update = Some((name.into(), old.clone(), new.clone()));
                        statuses.insert(name.into(), "현재 브랜치 갱신 보류".into());
                    } else {
                        checked_n += 1;
                        statuses.insert(name.into(), "다른 작업 폴더에서 사용 중".into());
                    }
                } else {
                    updates.push((r.clone(), old.clone(), new.clone()));
                    changed.push(format!("{}: {} → {}", name, &old[..8], &new[..8]));
                    updated += 1;
                    statuses.insert(name.into(), "갱신".into());
                }
            }
            1 => {
                ahead += 1;
                statuses.insert(name.into(), "로컬에 추가 커밋 있음".into());
            }
            _ => {
                diverged_n += 1;
                diverged.push(name.to_string());
                statuses.insert(name.into(), "로컬·서버 이력 갈라짐".into());
            }
        }
    }
    let mut case_blocked = Vec::new();
    for name in STANDARD_BRANCHES {
        let r = format!("refs/heads/{}", name);
        if !local.contains_key(&r) {
            if let Some(oid) = remote.get(name) {
                if let Some(c) = case_clash(&r, local.keys().map(|s| s.as_str())) {
                    case_blocked.push(format!("{} ({})", name, &c["refs/heads/".len()..]));
                    continue;
                }
                creates.push((r, oid.clone(), name.to_string()));
            }
        }
    }

    let mut undo = UndoLog::new(git);
    let mut current_moved = false;
    if let Some((name, old, new)) = &current_update {
        let mv = git.run(&["merge", "--ff-only", "--no-autostash", new])?;
        let after = git.q(&["rev-parse", "HEAD"])?;
        let after = if after.ok() { after.out.trim().to_string() } else { String::new() };
        if !mv.ok() {
            if &after != old {
                return Ok(Outcome::stop(format!("현재 브랜치 {} 갱신 실패 뒤 HEAD가 예상과 달라졌습니다. 저장소를 확인해 주세요.\n{}", name, mv.detail())));
            }
            dirty += 1;
            current_blocked.push(format!("{}: {}", name, mv.detail().trim().lines().next().unwrap_or("")));
        } else {
            if &after != new {
                return Ok(Outcome::stop(format!("현재 브랜치 {}의 빠른 전진 결과가 서버 값과 다릅니다.", name)));
            }
            let rec = undo.record(git, &format!("refs/heads/{}", name), old, new)?;
            if !rec.ok() {
                crate::log::say(&format!("현재 브랜치 {} 의 되돌리기 기록을 남기지 못했습니다.", name));
            }
            current_moved = true;
            current_n += 1;
            statuses.insert(name.clone(), "갱신".into());
            changed.push(format!("{}(현재): {} → {}", name, &old[..8], &new[..8]));
        }
    }
    if !updates.is_empty() || !creates.is_empty() {
        let mut ch = updates.clone();
        ch.extend(creates.iter().map(|(r, o, _)| (r.clone(), undo.zero.clone(), o.clone())));
        let res = undo.commit(git, &ch, &[])?;
        if !res.ok() {
            let suffix = if current_moved { " 현재 브랜치의 빠른 전진은 이미 완료됐습니다." } else { " 변경은 확정되지 않았습니다." };
            let mut o = Outcome::stop(format!("나머지 로컬 브랜치를 한 번에 갱신하지 못했습니다.{}\n{}", suffix, res.detail()));
            o.undo = undo.name();
            return Ok(o);
        }
    }
    let mut tracking_failed = Vec::new();
    for (_, oid, name) in &creates {
        let a = git.run(&["config", "--local", &format!("branch.{}.remote", name), "origin"])?;
        let b = git.run(&["config", "--local", &format!("branch.{}.merge", name), &format!("refs/heads/{}", name)])?;
        if !a.ok() || !b.ok() {
            tracking_failed.push(name.clone());
        }
        changed.push(format!("{}(생성): {}", name, &oid[..8]));
        created_n += 1;
        statuses.insert(name.clone(), "생성".into());
    }
    let message = format!(
        "서버 브랜치를 확인했습니다. 작업 중이 아닌 로컬 브랜치 갱신 {}개, 현재 브랜치 갱신 {}개, 기준 브랜치 생성 {}개. 이미 최신 {}개, 다른 작업 폴더에서 사용 중 {}개, 현재 브랜치 갱신 보류 {}개, 로컬에 추가 커밋 있음 {}개, 로컬·서버 이력 갈라짐 {}개, 로컬에만 있는 브랜치 {}개.",
        updated, current_n, created_n, latest, checked_n, dirty, ahead, diverged_n, no_remote
    );
    let mut o = Outcome::new("서버 최신화", message.clone(), true);
    o.branch = (!current_branch.is_empty()).then(|| current_branch.clone());
    o.commits = changed;
    o.branch_details = branch_details(git, &remote, &local, &statuses);
    let mut notes = Vec::new();
    if !diverged.is_empty() {
        notes.push(format!("이력이 갈라져 건드리지 않은 브랜치: {}", diverged.join(", ")));
    }
    if !current_blocked.is_empty() {
        notes.push(format!("로컬 작업과 겹치거나 Git이 갱신을 거부해 보존한 현재 브랜치: {}", current_blocked.join(", ")));
    }
    if !case_blocked.is_empty() {
        notes.push(format!("대소문자만 다른 로컬 브랜치가 있어 만들지 않은 기준 브랜치: {}", case_blocked.join(", ")));
    }
    if !tracking_failed.is_empty() {
        notes.push(format!("브랜치는 만들었지만 origin 추적 설정을 쓰지 못함: {}", tracking_failed.join(", ")));
    }
    if !notes.is_empty() {
        o.push_note = Some(notes.join("\n"));
    }
    o.undo = undo.name();
    crate::log::say(&message);
    Ok(o)
}
