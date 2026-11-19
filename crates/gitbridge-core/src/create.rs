//! 작업 PC → 원본 feature·hotfix 번들 만들기(1.0.5 create.py).
//! 2.0.0 은 지난번 뽑은 끝(refs/oqms-export/<브랜치>)이 조상이면 증분으로 만들고, 검증 뒤에만 표식을 옮긴다.

use crate::export::{absolute_out_dir, count, ensure_out_dir, log_lines, MARK_ROOT};
use crate::gitrun::{acquire_export_lock, ExportLock, Git};
use crate::refs::*;
use crate::R;

/// 브랜치에서 닿는 태그 가운데 기준 브랜치에 이미 든 것을 뺀다. since 가 있으면 그 커밋에서 닿는 태그도 뺀다.
fn branch_only_tags(git: &Git, tip: &str, since: Option<&str>) -> R<Vec<(String, String)>> {
    let mut args = vec!["for-each-ref".to_string(), "--format=%(refname)\t%(objectname)".into(), format!("--merged={}", tip)];
    if let Some(s) = since {
        args.push(format!("--no-merged={}", s));
    }
    args.push("refs/tags/".into());
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let r = git.q(&a)?;
    if !r.ok() {
        return Err(r.detail().to_string());
    }
    let mut tags: Vec<(String, String)> = r.lines().iter().filter_map(|l| l.split_once('\t')).map(|(n, o)| (n.to_string(), o.to_string())).collect();
    if let Some(base) = local_base_branch(git) {
        let m = git.q(&["for-each-ref", "--format=%(refname)", &format!("--merged=refs/remotes/origin/{}", base), "refs/tags/"])?;
        if !m.ok() {
            return Err(m.detail().to_string());
        }
        let on_base: std::collections::HashSet<&str> = m.lines().into_iter().collect();
        tags.retain(|(n, _)| !on_base.contains(n.as_str()));
    }
    tags.sort();
    Ok(tags)
}

pub fn create_feature(repo: &str, branch: &str, out_dir: &str, full: bool) -> Outcome {
    let out_dir = absolute_out_dir(out_dir);
    let out_dir = out_dir.as_str();
    crate::log::say(&format!("===== 번들 만들기 — 저장소 {} / 브랜치 {} / 출력 {} / 전체 {}", repo, branch, out_dir, full));
    if !name_ok(branch) {
        return Outcome::stop(format!("브랜치 이름이 규칙에 맞지 않습니다: {}  ({})", branch, NAME_RULE_TEXT));
    }
    let git = match Git::open(repo) {
        Ok(g) => g,
        Err(e) => return Outcome::stop(e),
    };
    let mut created: Option<String> = None;
    // (메타 참조, 옛 값, 이번 실행이 넣은 값)
    let mut meta_restore: Option<(String, Option<String>, String)> = None;
    let mut lock: Option<ExportLock> = None;
    let res = create_inner(&git, branch, out_dir, full, &mut created, &mut meta_restore, &mut lock);
    // 아래 정리는 모두 잠금 가드(lock)가 살아 있는 동안 끝낸다.
    remove_failed_bundle(&created);
    if let Some((meta, old, tip)) = meta_restore {
        let r = match &old {
            Some(o) => git.q(&["update-ref", &meta, o, &tip]),
            None => git.q(&["update-ref", "-d", &meta, &tip]),
        };
        if !matches!(r, Ok(ref x) if x.ok()) {
            crate::log::say(&format!("저장소 식별용 임시 기록을 되돌리지 못했습니다: {}", meta));
        }
    }
    drop(lock);
    res.unwrap_or_else(|e| Outcome::stop(format!("예상하지 못한 오류로 멈췄습니다: {}", e)))
}

fn create_inner(
    git: &Git,
    branch: &str,
    out_dir: &str,
    full: bool,
    created: &mut Option<String>,
    meta_restore: &mut Option<(String, Option<String>, String)>,
    lock: &mut Option<ExportLock>,
) -> R<Outcome> {
    if !git.q(&["rev-parse", "--show-toplevel"])?.ok() {
        return Ok(Outcome::stop("저장소가 아닙니다."));
    }
    if let Some(m) = in_progress_mark(git) {
        return Ok(Outcome::stop(format!("저장소가 머지·리베이스 등 진행 중인 상태입니다 ({}).", m)));
    }
    *lock = Some(acquire_export_lock(git)?);
    let st = git.q(&["status", "--porcelain"])?;
    if !st.ok() {
        return Ok(Outcome::stop(format!("작업 파일 상태를 확인하지 못했습니다. {}", st.detail())));
    }
    if !st.out.trim().is_empty() {
        return Ok(Outcome::stop("커밋되지 않은 변경이 있어 번들을 만들지 않았습니다. 의도한 변경을 커밋하고 다시 실행해 주세요."));
    }
    let head_ref = format!("refs/heads/{}", branch);
    let Some(tip) = ref_commit(git, &head_ref)? else {
        return Ok(Outcome::stop(format!("브랜치가 없습니다: {}", branch)));
    };
    let label = get_repo_label(git)?;
    let marker = format!("{}{}", MARK_ROOT, branch);
    // 표식을 옮길 때 예상값으로 쓸 시작 때의 값(커밋으로 벗기지 않은 값).
    let old_marker = ref_value(git, &marker)?;

    // 증분 판정
    let mut since: Option<String> = None;
    if !full {
        if let Some(mk) = ref_commit(git, &marker)? {
            if mk == tip {
                crate::log::say(&format!("{} 가 지난번 만든 뒤로 그대로라 전체를 담은 번들로 다시 만듭니다.", branch));
            } else if is_ancestor(git, &mk, &tip)? {
                since = Some(mk);
            } else {
                crate::log::say(&format!("지난번 보낸 {} 위치가 지금 브랜치에서 이어지지 않습니다 — 전체를 담은 번들로 만듭니다.", branch));
            }
        }
    }

    let meta_ref = format!("refs/gitbridge/meta/repo/{}", label);
    let old_meta = ref_value(git, &meta_ref)?;
    let u = git.q(&["update-ref", &meta_ref, &tip])?;
    if !u.ok() {
        return Ok(Outcome::stop(format!("번들에 저장소 식별 정보를 넣지 못했습니다. {}", u.detail())));
    }
    *meta_restore = Some((meta_ref.clone(), old_meta, tip.clone()));
    let tags = match branch_only_tags(git, &tip, since.as_deref()) {
        Ok(t) => t,
        Err(e) => return Ok(Outcome::stop(format!("브랜치에 달린 태그를 확인하지 못했습니다.\n{}", e))),
    };
    crate::log::say(&format!(
        "작업 브랜치 {}: {} = {} · 함께 담을 태그 {}개",
        if since.is_some() { "바뀐 부분만" } else { "전체 이력" },
        branch,
        tip,
        tags.len()
    ));

    ensure_out_dir(out_dir)?;
    let file = unique_bundle_path(out_dir, &format!("{}-{}-{}", crate::time::now().file_stem(), label, file_part(branch)));
    let excl = since.as_ref().map(|s| format!("^{}", s));
    let tag_refs: Vec<String> = tags.iter().map(|(n, _)| n.clone()).collect();
    let mut args: Vec<&str> = vec!["bundle", "create", &file, &head_ref, &meta_ref];
    args.extend(tag_refs.iter().map(|s| s.as_str()));
    if let Some(e) = &excl {
        args.push(e);
    }
    let cr = git.run(&args)?;
    if !cr.ok() {
        return Ok(Outcome::stop(format!("번들을 만들지 못했습니다. {}", cr.detail())));
    }
    *created = Some(file.clone());

    let head = get_bundle_head(&file)?;
    if head.oid != tip || head.branch != branch {
        return Ok(Outcome::stop(format!("만든 번들의 맨 끝 커밋이 브랜치 끝과 다릅니다: {} {}", head.branch, head.oid)));
    }
    let want_tags: std::collections::BTreeMap<String, String> =
        tags.iter().map(|(n, o)| (n.trim_start_matches("refs/tags/").to_string(), o.clone())).collect();
    if head.tags != want_tags {
        return Ok(Outcome::stop("만든 번들의 태그 목록이 고른 태그와 다릅니다."));
    }
    if head.kind != "feature" || head.label != label {
        return Ok(Outcome::stop("만든 번들의 저장소 식별 정보가 다릅니다."));
    }
    let v = git.run(&["bundle", "verify", &file])?;
    if !v.ok() {
        return Ok(Outcome::stop(format!("만든 번들 검증에 실패했습니다. {}", v.detail())));
    }
    *created = None;

    let expect_old = old_marker.unwrap_or_else(|| zero_oid(git));
    let mk = git.run(&["update-ref", "-m", "gitbridge feature export", &marker, &tip, &expect_old])?;
    let mut rng = vec![tip.as_str()];
    if let Some(e) = &excl {
        rng.push(e);
    }
    let n = count(git, &rng);
    let tag_text = if tags.is_empty() { "없음".to_string() } else { want_tags.keys().cloned().collect::<Vec<_>>().join(", ") };
    let mut o = Outcome::new("번들 생성", file.clone(), true);
    o.branch = Some(branch.into());
    o.commits = log_lines(git, &rng);
    let mut note = format!(
        "크기 {} 바이트 · {} · {} 커밋 {}개 · 태그 {}",
        commas(file_size(&file)),
        if since.is_some() { "바뀐 부분만 담은 번들" } else { "전체를 담은 번들" },
        if since.is_some() { "지난번 뒤" } else { "전체 이력" },
        n,
        tag_text
    );
    if !mk.ok() {
        note += "\n지난번 보낸 위치를 기록하지 못했습니다 — 다음 번엔 --full 로 만들어 주세요. 그사이 다른 작업이 그 기록을 바꿨을 수 있습니다.";
    }
    o.push_note = Some(note);
    o.file = Some(file);
    Ok(o)
}
