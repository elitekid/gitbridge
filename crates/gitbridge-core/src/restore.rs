//! 독립 전체 번들로 새 저장소를 만든다(1.0.5 base.py restore_new_repository).
//! 2.0.0: 증분 매니페스트는 거절하고, 매니페스트가 있으면 그 값과 대조한 뒤 v2 원본 보관 참조도 만든다.

use crate::gitrun::{wsl_location, Git};
use crate::manifest::Manifest;
use crate::refs::*;
use crate::R;
use std::collections::BTreeMap;
use std::path::Path;

const TMP_REFS: &str = "refs/gitbridge/restore/";
const INCREMENTAL_MSG: &str = "바뀐 부분만 담은 번들로는 새 저장소를 만들 수 없습니다. 전체를 담은 번들(새 저장소를 만들 수 있음)로 다시 만들어 주세요.";

pub fn restore_new_repository(bundle: &str, dest: &str) -> Outcome {
    crate::log::say(&format!("===== 새 저장소 복원 — 번들 {} / 대상 {}", bundle, dest));
    if Path::new(dest).exists() {
        return Outcome::stop(format!("새 저장소 대상 폴더가 이미 있습니다: {}", dest));
    }
    let abs = std::path::absolute(dest).unwrap_or_else(|_| Path::new(dest).to_path_buf());
    let (Some(parent), Some(name)) = (abs.parent(), abs.file_name()) else {
        return Outcome::stop(format!("새 저장소의 상위 폴더가 없습니다: {}", dest));
    };
    if !parent.is_dir() {
        return Outcome::stop(format!("새 저장소의 상위 폴더가 없습니다: {}", parent.display()));
    }
    let tmp = parent.join(format!(".gitbridge-{}-{}", name.to_string_lossy(), crate::time::random_hex(4)));
    let tmp_s = tmp.to_string_lossy().to_string();
    let res = restore_inner(bundle, dest, &tmp_s);
    if tmp.is_dir() {
        if let Err(e) = std::fs::remove_dir_all(&tmp) {
            crate::log::say(&format!("임시 폴더를 지우지 못했습니다. 직접 지워 주세요: {} ({})", tmp_s, e));
        }
    }
    res.unwrap_or_else(|e| Outcome::stop(format!("새 저장소 복원 중 오류로 멈췄습니다: {}", e)))
}

fn restore_inner(bundle: &str, dest: &str, tmp: &str) -> R<Outcome> {
    let head = get_bundle_head(bundle)?;
    if head.kind != "base" && head.kind != "repository" {
        return Ok(Outcome::stop("feature 번들로는 새 저장소를 만들 수 없습니다."));
    }
    if !get_bundle_prereqs(bundle)?.is_empty() {
        return Ok(Outcome::stop(INCREMENTAL_MSG));
    }
    let (b, heads, tags, mut specs) = if head.kind == "base" {
        let b = head.branch.clone();
        let specs = vec![format!("{}:{}heads/{}", head.ref_, TMP_REFS, b)];
        (b.clone(), BTreeMap::from([(b, head.oid.clone())]), BTreeMap::new(), specs)
    } else {
        let mut specs: Vec<String> = head.heads.keys().map(|x| format!("{}:{}heads/{}", head.source_head(x), TMP_REFS, x)).collect();
        specs.extend(head.tags.keys().map(|x| format!("{}:{}tags/{}", head.source_tag(x), TMP_REFS, x)));
        (head.branch.clone(), head.heads.clone(), head.tags.clone(), specs)
    };

    if cfg!(windows) && wsl_location(dest).is_none() {
        let names: Vec<String> =
            heads.keys().map(|x| format!("refs/heads/{}", x)).chain(tags.keys().map(|x| format!("refs/tags/{}", x))).collect();
        for (i, n) in names.iter().enumerate() {
            if let Some(c) = case_clash(n, names[..i].iter().map(|s| s.as_str())) {
                return Ok(Outcome::stop(format!("Windows에서 함께 복원할 수 없는, 대소문자만 다른 브랜치·태그가 있습니다: {}, {}", c, n)));
            }
        }
    }

    // 해시 길이는 번들 OID 길이를 따른다.
    let mut init = vec!["init", "-q"];
    if head.oid.len() == 64 {
        init.push("--object-format=sha256");
    }
    init.push(tmp);
    if !Git::bare_for(&[tmp, bundle])?.run(&init)?.ok() {
        return Ok(Outcome::stop("새 git 저장소를 초기화하지 못했습니다."));
    }
    let git = Git::open(tmp)?;

    let manifest_tmp = format!("{}manifest", TMP_REFS);
    if head.has_manifest {
        specs.push(format!("{}:{}", V2_MANIFEST, manifest_tmp));
    }
    let mut args: Vec<&str> = BUNDLE_FETCH.to_vec();
    args.extend(["--atomic", bundle]);
    args.extend(specs.iter().map(|s| s.as_str()));
    let f = git.run(&args)?;
    if !f.ok() {
        return Ok(Outcome::stop(format!("새 저장소에 번들을 받지 못했습니다.\n{}", f.err)));
    }
    let manifest = if head.has_manifest { Some(Manifest::read(&git, &manifest_tmp)?) } else { None };
    if let Some(m) = &manifest {
        if m.incremental {
            return Ok(Outcome::stop(INCREMENTAL_MSG));
        }
        if m.heads != heads || m.tags != tags {
            return Ok(Outcome::stop("번들의 목록 파일과 실제 브랜치·태그가 다릅니다."));
        }
    }
    if !heads.contains_key(&b) {
        return Ok(Outcome::stop(format!("기본 브랜치를 지정하지 못했습니다: {}", b)));
    }

    let mut lines = vec!["start".to_string()];
    for (n, o) in &heads {
        lines.push(format!("create refs/heads/{} {}", n, o));
        lines.push(format!("create refs/remotes/origin/{} {}", n, o));
        if manifest.is_some() {
            lines.push(format!("create {}heads/{} {}", V2_DEVNET, n, o));
        }
    }
    for (n, o) in &tags {
        lines.push(format!("create refs/tags/{} {}", n, o));
        if manifest.is_some() {
            lines.push(format!("create {}tags/{} {}", V2_DEVNET, n, o));
        }
    }
    lines.push("prepare".into());
    lines.push("commit".into());
    let r = git.update_refs(&lines, true)?;
    if !r.ok() {
        return Ok(Outcome::stop(format!("로컬 브랜치를 만들지 못했습니다: {}", r.detail())));
    }
    if !git.q(&["symbolic-ref", "HEAD", &format!("refs/heads/{}", b)])?.ok() {
        return Ok(Outcome::stop(format!("기본 브랜치를 지정하지 못했습니다: {}", b)));
    }
    let co = git.run(&["reset", "-q", "--hard", &heads[&b]])?;
    if !co.ok() {
        return Ok(Outcome::stop(format!("기본 브랜치 작업 파일을 만들지 못했습니다.\n{}", co.err)));
    }
    delete_ref_prefixes(&git, &[TMP_REFS])?;
    drop(git);
    std::fs::rename(tmp, dest).map_err(|e| format!("완성한 저장소를 제자리로 옮기지 못했습니다: {} ({})", dest, e))?;

    let base = Path::new(dest).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| dest.to_string());
    let mut o = Outcome::new(
        "새 저장소 복원",
        format!("{} 를 새 저장소로 만들었습니다. 기본 브랜치 {}, 브랜치 {}개, 태그 {}개.", base, b, heads.len(), tags.len()),
        true,
    );
    o.branch = Some(b);
    o.push_note = Some(format!("새 저장소: {}", dest));
    o.is_base = true;
    Ok(o)
}
