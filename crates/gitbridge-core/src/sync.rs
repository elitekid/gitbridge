//! 전체 저장소 번들로 로컬 브랜치·origin 추적·태그를 맞춘다(1.0.5 base.py sync_repository_bundle).
//! 2.0.0: 교체 전 값은 pre-sync 참조 대신 UndoLog 에 남기고, 모든 참조를 한 트랜잭션으로 바꾼다.

use crate::apply::finish;
use crate::gitrun::Git;
use crate::manifest::Manifest;
use crate::refs::*;
use crate::undo::UndoLog;
use crate::R;
use std::collections::BTreeMap;

/// origin 추적 참조 → 값. origin/HEAD 같은 간접 참조는 건너뛴다.
fn origin_map(git: &Git) -> R<BTreeMap<String, String>> {
    let res = git.q(&["for-each-ref", "--format=%(refname)\t%(objectname)\t%(symref)", "refs/remotes/origin/"])?;
    if !res.ok() {
        return Err(format!("브랜치·태그 목록을 읽지 못했습니다.\n{}", res.detail()));
    }
    let mut m = BTreeMap::new();
    for line in res.lines() {
        let p: Vec<&str> = line.split('\t').collect();
        if p.len() < 2 || (p.len() > 2 && !p[2].is_empty()) {
            continue;
        }
        m.insert(p[0].to_string(), p[1].to_string());
    }
    Ok(m)
}

/// 이전 값 → 새 값(없으면 None) 차이를 changes 에 더한다.
fn push_change(changes: &mut Vec<(String, String, String)>, zero: &str, r: &str, old: Option<&String>, new: Option<&String>) {
    match (old, new) {
        (Some(o), Some(n)) if o == n => {}
        (Some(o), Some(n)) => changes.push((r.to_string(), o.clone(), n.clone())),
        (Some(o), None) => changes.push((r.to_string(), o.clone(), zero.to_string())),
        (None, Some(n)) => changes.push((r.to_string(), zero.to_string(), n.clone())),
        (None, None) => {}
    }
}

pub fn sync_repository(bundle: &str, repo: &str) -> Outcome {
    crate::log::say(&format!("===== 원본 전체 동기화 — 번들 {} / 저장소 {} / 서버 접속 없음", bundle, repo));
    let git = match Git::open(repo) {
        Ok(g) => g,
        Err(e) => return Outcome::stop(e),
    };
    let mut undo = UndoLog::new(&git);
    let stage = format!("refs/oqms-import/{}-sync", run_stamp());
    let temp = vec![format!("{}/", stage)];
    let r = sync_inner(&git, bundle, &stage, &mut undo);
    finish(&git, &temp, &undo, r)
}

fn sync_inner(git: &Git, bundle: &str, stage: &str, undo: &mut UndoLog) -> R<Outcome> {
    if !git.run(&["rev-parse", "--show-toplevel"])?.ok() {
        return Ok(Outcome::stop("고른 폴더가 git 저장소가 아닙니다."));
    }
    if let Some(m) = in_progress_mark(git) {
        return Ok(Outcome::stop(format!("저장소가 머지·리베이스 등 진행 중인 상태입니다 ({}).", m)));
    }
    let cur = git.q(&["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let current_branch = if cur.ok() { cur.out.trim().to_string() } else { String::new() };
    if !name_ok(&current_branch) {
        return Ok(Outcome::stop("전체 동기화는 보존할 현재 feature·hotfix 브랜치에서 실행해야 합니다."));
    }

    let head = get_bundle_head(bundle)?;
    if head.kind != "repository" || head.scope != "all" {
        return Ok(Outcome::stop("모든 서버 브랜치와 태그가 든 전체 저장소 번들만 전체 동기화할 수 있습니다."));
    }
    let verified = git.run(&["bundle", "verify", bundle])?;
    if !verified.ok() {
        return Ok(Outcome::stop(format!("번들 검증에 실패했습니다.\n{}", verified.detail())));
    }

    // 2.0.0 매니페스트가 있으면 증분이 아닌지, 기본 브랜치 값이 있는지 확인한다.
    if head.has_manifest {
        let mref = format!("{}/manifest", stage);
        let spec = format!("+{}:{}", V2_MANIFEST, mref);
        let mut args: Vec<&str> = BUNDLE_FETCH.to_vec();
        args.extend([bundle, spec.as_str()]);
        let f = git.run(&args)?;
        if !f.ok() {
            return Ok(Outcome::stop(format!("번들의 목록 파일을 받지 못했습니다.\n{}", f.detail())));
        }
        let m = match Manifest::read(git, &mref) {
            Ok(m) => m,
            Err(e) => return Ok(Outcome::stop(e)),
        };
        if m.incremental {
            return Ok(Outcome::stop("바뀐 부분만 담은 번들로는 전체 동기화를 할 수 없습니다. 모든 서버 브랜치와 태그가 든 전체 저장소 번들을 골라 주세요."));
        }
        if !head.heads.contains_key(&head.branch) {
            return Ok(Outcome::stop(format!("번들 목록 파일에 기본 브랜치 {} 의 값이 없습니다. 아무것도 바꾸지 않았습니다.", head.branch)));
        }
    }

    let mut specs: Vec<String> = Vec::new();
    for name in head.heads.keys() {
        specs.push(format!("+{}:{}/heads/{}", head.source_head(name), stage, name));
    }
    for name in head.tags.keys() {
        specs.push(format!("+{}:{}/tags/{}", head.source_tag(name), stage, name));
    }
    let mut args: Vec<&str> = BUNDLE_FETCH.to_vec();
    args.push("--atomic");
    args.push(bundle);
    args.extend(specs.iter().map(|s| s.as_str()));
    let fetched = git.run(&args)?;
    if !fetched.ok() {
        return Ok(Outcome::stop(format!("전체 브랜치·태그를 임시 보관 공간으로 받지 못했습니다.\n{}", fetched.err)));
    }

    let staged_heads = ref_map(git, &[&format!("{}/heads/", stage)])?;
    let staged_tags = ref_map(git, &[&format!("{}/tags/", stage)])?;
    for (name, oid) in &head.heads {
        if staged_heads.get(&format!("{}/heads/{}", stage, name)) != Some(oid) {
            return Ok(Outcome::stop(format!("임시로 받은 브랜치 값이 번들 식별 정보와 다릅니다: {}", name)));
        }
    }
    for (name, oid) in &head.tags {
        if staged_tags.get(&format!("{}/tags/{}", stage, name)) != Some(oid) {
            return Ok(Outcome::stop(format!("임시로 받은 태그 값이 번들 식별 정보와 다릅니다: {}", name)));
        }
    }

    let probe = match ref_commit(git, &format!("refs/remotes/origin/{}", head.branch))? {
        Some(x) => Some(x),
        None => match ref_commit(git, &format!("refs/heads/{}", head.branch))? {
            Some(x) => Some(x),
            None => ref_commit(git, "HEAD")?,
        },
    };
    if let Some(p) = &probe {
        let anc = git.q(&["merge-base", p, &head.oid])?;
        if anc.code == 1 {
            return Ok(Outcome::stop("이 전체 저장소 번들은 고른 저장소와 공통 조상이 없습니다."));
        }
        if !anc.ok() {
            return Ok(Outcome::stop(format!("저장소 이력을 확인하지 못했습니다.\n{}", anc.detail())));
        }
    }

    let local_heads = ref_map(git, &["refs/heads/"])?;
    let local_tags = ref_map(git, &["refs/tags/"])?;
    let origin_heads = origin_map(git)?;
    let current_ref = format!("refs/heads/{}", current_branch);
    let worktrees = worktree_branches(git)?;
    let wanted_head_refs: BTreeMap<String, String> = head.heads.iter().map(|(n, o)| (format!("refs/heads/{}", n), o.clone())).collect();
    let mut changed_branch_refs: Vec<String> = Vec::new();
    for (r, old) in &local_heads {
        if *r == current_ref {
            continue;
        }
        if wanted_head_refs.get(r) != Some(old) {
            changed_branch_refs.push(r.clone());
        }
    }
    for (r, new) in &wanted_head_refs {
        if *r != current_ref && local_heads.get(r) != Some(new) {
            changed_branch_refs.push(r.clone());
        }
    }
    let mut blocked: Vec<String> = changed_branch_refs.into_iter().filter(|r| worktrees.contains_key(r)).collect();
    blocked.sort();
    blocked.dedup();
    if !blocked.is_empty() {
        let detail = blocked.iter().map(|x| format!("{} ({})", &x["refs/heads/".len()..], worktrees[x])).collect::<Vec<_>>().join(", ");
        return Ok(Outcome::stop(format!("다른 작업 폴더에서 작업 중인 브랜치는 교체할 수 없습니다: {}", detail)));
    }

    // 새로 만들 브랜치가 기존 로컬 브랜치와 대소문자만 다르면 Windows 에서 같은 참조 파일이라 멈춘다.
    for name in head.heads.keys() {
        let r = format!("refs/heads/{}", name);
        if !local_heads.contains_key(&r) {
            if let Some(clash) = case_clash(&r, local_heads.keys().map(|s| s.as_str())) {
                return Ok(Outcome::stop(format!("대소문자만 다른 로컬 브랜치 {} 가 있어 {} 를 만들 수 없습니다. 아무것도 바꾸지 않았습니다.", &clash["refs/heads/".len()..], name)));
            }
        }
    }

    // 모든 변경을 한 묶음으로 만든다. 현재 브랜치와 작업 파일은 바꾸지 않는다.
    let zero = undo.zero.clone();
    let mut changes: Vec<(String, String, String)> = Vec::new();
    for (r, old) in &local_heads {
        if *r == current_ref {
            continue;
        }
        push_change(&mut changes, &zero, r, Some(old), wanted_head_refs.get(r));
    }
    for (r, new) in &wanted_head_refs {
        if *r != current_ref && !local_heads.contains_key(r) {
            push_change(&mut changes, &zero, r, None, Some(new));
        }
    }

    let wanted_origin: BTreeMap<String, String> = head.heads.iter().map(|(n, o)| (format!("refs/remotes/origin/{}", n), o.clone())).collect();
    for (r, old) in &origin_heads {
        push_change(&mut changes, &zero, r, Some(old), wanted_origin.get(r));
    }
    for (r, new) in &wanted_origin {
        if !origin_heads.contains_key(r) {
            push_change(&mut changes, &zero, r, None, Some(new));
        }
    }

    let wanted_tags: BTreeMap<String, String> = head.tags.iter().map(|(n, o)| (format!("refs/tags/{}", n), o.clone())).collect();
    for (r, old) in &local_tags {
        push_change(&mut changes, &zero, r, Some(old), wanted_tags.get(r));
    }
    for (r, new) in &wanted_tags {
        if !local_tags.contains_key(r) {
            push_change(&mut changes, &zero, r, None, Some(new));
        }
    }

    // 원본 보관 참조: 전체 번들이므로 이번 번들에 없는 것(1.0.5 위치 포함)은 지운다.
    let old_devnet = ref_map(git, &[V2_DEVNET, V1_DEVNET])?;
    let mut wanted_devnet: BTreeMap<String, String> = BTreeMap::new();
    wanted_devnet.extend(head.heads.iter().map(|(n, o)| (format!("{}heads/{}", V2_DEVNET, n), o.clone())));
    wanted_devnet.extend(head.tags.iter().map(|(n, o)| (format!("{}tags/{}", V2_DEVNET, n), o.clone())));
    for (r, old) in &old_devnet {
        push_change(&mut changes, &zero, r, Some(old), wanted_devnet.get(r));
    }
    for (r, new) in &wanted_devnet {
        if !old_devnet.contains_key(r) {
            push_change(&mut changes, &zero, r, None, Some(new));
        }
    }

    let archived = changes.iter().filter(|(_, old, _)| *old != zero).count();
    if !changes.is_empty() {
        let updated = undo.commit(git, &changes, &[])?;
        if !updated.ok() {
            return Ok(Outcome::stop(format!("브랜치·태그를 한 번에 교체하지 못했습니다. 변경은 확정되지 않았습니다.\n{}", updated.detail())));
        }
    }

    let mut res = Outcome::new(
        "원본 전체 동기화",
        format!("현재 {}와 작업 파일은 유지하고, 나머지 로컬 브랜치·origin 추적 브랜치·태그를 원본 전체 번들과 맞췄습니다.", current_branch),
        true,
    );
    res.branch = Some(current_branch);
    res.is_base = true;
    res.push_note = Some(match undo.name() {
        Some(u) => format!("교체 전 브랜치·태그 {}개를 되돌리기 기록 {} 에 보관했습니다. 서버에는 접속하거나 쓰지 않았습니다.", archived, u),
        None => "바뀐 브랜치·태그가 없었습니다. 서버에는 접속하거나 쓰지 않았습니다.".to_string(),
    });
    Ok(res)
}
