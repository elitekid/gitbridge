//! 서버에서 한 번 받은 브랜치·태그를 최신화와 번들 생성 동안 고정한다.

use crate::gitrun::Git;
use crate::refs::{delete_ref_prefixes, in_progress_mark, ref_map, strip_map};
use crate::R;
use std::collections::BTreeMap;

pub struct Snapshot {
    pub prefix: String,
    pub heads: BTreeMap<String, String>,
    pub tags: BTreeMap<String, String>,
    git: Git,
    pub cleanup_error: String,
}

impl Snapshot {
    /// origin 추적 브랜치와 격리한 브랜치·태그를 한 번의 atomic fetch 로 받는다. 로컬 태그는 바꾸지 않는다.
    pub fn fetch(git: &Git, update_tracking: bool) -> R<Snapshot> {
        if !git.q(&["rev-parse", "--show-toplevel"])?.ok() {
            return Err("고른 폴더가 git 저장소가 아닙니다.".into());
        }
        if let Some(m) = in_progress_mark(git) {
            return Err(format!("저장소가 머지·리베이스 등 진행 중인 상태입니다 ({}).", m));
        }
        if !git.q(&["remote", "get-url", "origin"])?.ok() {
            return Err("서버 주소(origin)가 없어 서버 정보를 받을 수 없습니다.".into());
        }
        let prefix = format!("refs/gitbridge/snapshot/{}/", crate::time::random_hex(16));
        crate::log::say("===== 서버 브랜치·태그 받기 — 한 번 받은 값으로 최신화와 번들을 만듭니다.");
        let mut specs = vec![format!("+refs/heads/*:{}heads/*", prefix), format!("+refs/tags/*:{}tags/*", prefix)];
        if update_tracking {
            specs.insert(0, "+refs/heads/*:refs/remotes/origin/*".into());
        }
        let mut args = vec!["-c", "fetch.pruneTags=false", "-c", "remote.origin.pruneTags=false", "fetch", "--atomic", "--prune", "--no-tags", "--refmap=", "origin"];
        args.extend(specs.iter().map(|s| s.as_str()));
        let mut snap = Snapshot { prefix: prefix.clone(), heads: BTreeMap::new(), tags: BTreeMap::new(), git: git.clone(), cleanup_error: String::new() };
        let f = git.run(&args)?;
        if !f.ok() {
            return Err(format!("서버 브랜치·태그를 받지 못했습니다. 네트워크와 GitLab 인증을 확인해 주세요.\n{}", f.detail()));
        }
        let values = ref_map(git, &[&prefix])?;
        snap.heads = strip_map(&values, &format!("{}heads/", prefix));
        snap.tags = strip_map(&values, &format!("{}tags/", prefix));
        Ok(snap)
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        if let Err(e) = delete_ref_prefixes(&self.git, &[&self.prefix]) {
            self.cleanup_error = format!("서버 임시 브랜치·태그 정리 보류: {}", e);
            crate::log::say(&self.cleanup_error);
        }
    }
}
