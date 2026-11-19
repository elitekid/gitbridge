//! 되돌리기 기록. 참조를 바꾸는 같은 update-ref 트랜잭션 안에서
//! 덮기 전 값(refs/gitbridge/undo/<stamp>/...)과 journal blob 을 함께 남긴다.

use crate::gitrun::{Git, GitRes};
use crate::refs::{is_zero, ref_map, ref_value, zero_oid, Place};
use crate::R;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

pub const UNDO_ROOT: &str = "refs/gitbridge/undo/";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    #[serde(rename = "ref")]
    pub ref_: String,
    pub old: String,
    pub new: String,
    /// 옮기기 전에 남겨 아직 실제로 일어났는지 확정 못 한 항목. 옛 기록은 false 로 읽힌다.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
}

pub struct UndoLog {
    pub stamp: String,
    pub entries: Vec<Entry>,
    pub zero: String,
    journal_oid: Option<String>,
    backed: HashSet<String>,
    last_added_backup: Option<String>,
}

fn backup_name(stamp: &str, r: &str) -> String {
    let tail = if let Some(x) = r.strip_prefix("refs/heads/") {
        format!("heads/{}", x)
    } else if let Some(x) = r.strip_prefix("refs/remotes/origin/") {
        format!("remotes/origin/{}", x)
    } else if let Some(x) = r.strip_prefix("refs/tags/") {
        format!("tags/{}", x)
    } else {
        format!("other/{}", r.trim_start_matches("refs/"))
    };
    format!("{}{}/{}", UNDO_ROOT, stamp, tail)
}

impl UndoLog {
    pub fn new(git: &Git) -> UndoLog {
        UndoLog { stamp: crate::refs::run_stamp(), entries: Vec::new(), zero: zero_oid(git), journal_oid: None, backed: HashSet::new(), last_added_backup: None }
    }

    pub fn journal_ref(&self) -> String {
        format!("{}{}/journal", UNDO_ROOT, self.stamp)
    }

    /// 바뀐 것이 있으면 기록 이름.
    pub fn name(&self) -> Option<String> {
        (!self.entries.is_empty()).then(|| self.stamp.clone())
    }

    fn journal_lines(&self, git: &Git, all: &[Entry]) -> R<String> {
        let text = serde_json::to_string(all).map_err(|e| e.to_string())?;
        let r = git.input(&["hash-object", "-w", "--stdin"], &text, true)?;
        if !r.ok() {
            return Err(format!("되돌리기 기록을 만들지 못했습니다.\n{}", r.detail()));
        }
        let oid = r.out.trim().to_string();
        Ok(match &self.journal_oid {
            Some(old) => format!("update {} {} {}", self.journal_ref(), oid, old),
            None => format!("create {} {}", self.journal_ref(), oid),
        })
    }

    fn finish(&mut self, all: Vec<Entry>, journal_line: &str) {
        self.journal_oid = journal_line.split_whitespace().nth(2).map(|s| s.to_string());
        for e in &all[self.entries.len()..] {
            if !is_zero(&e.old) {
                self.backed.insert(e.ref_.clone());
            }
        }
        self.entries = all;
    }

    /// changes 를 한 트랜잭션으로 바꾸며 기록한다. extra 는 기록 없이 같은 트랜잭션에 넣을 줄.
    /// 실패하면 기록을 늘리지 않고 git 결과를 돌려준다.
    pub fn commit(&mut self, git: &Git, changes: &[(String, String, String)], extra: &[String]) -> R<GitRes> {
        let mut lines = vec!["start".to_string()];
        let mut all = self.entries.clone();
        let mut backed = HashSet::new();
        for (r, old, new) in changes {
            lines.push(if is_zero(old) {
                format!("create {} {}", r, new)
            } else if is_zero(new) {
                format!("delete {} {}", r, old)
            } else {
                format!("update {} {} {}", r, new, old)
            });
            if !is_zero(old) && !self.backed.contains(r) && backed.insert(r.clone()) {
                lines.push(format!("create {} {}", backup_name(&self.stamp, r), old));
            }
            all.push(Entry { ref_: r.clone(), old: old.clone(), new: new.clone(), pending: false });
        }
        lines.extend(extra.iter().cloned());
        let journal = if changes.is_empty() { None } else { Some(self.journal_lines(git, &all)?) };
        if let Some(j) = &journal {
            lines.push(j.clone());
        }
        lines.push("prepare".into());
        lines.push("commit".into());
        let r = git.input(&["update-ref", "--no-deref", "--stdin"], &(lines.join("\n") + "\n"), false)?;
        if r.ok() {
            if let Some(j) = journal {
                self.finish(all, &j);
            }
        }
        Ok(r)
    }

    /// 밖에서 이미 옮긴 참조(merge --ff-only 등)의 덮기 전 값과 기록만 남긴다.
    pub fn record(&mut self, git: &Git, r: &str, old: &str, new: &str) -> R<GitRes> {
        self.record_entry(git, r, old, new, false)
    }

    fn record_entry(&mut self, git: &Git, r: &str, old: &str, new: &str, pending: bool) -> R<GitRes> {
        let mut lines = vec!["start".to_string()];
        if !is_zero(old) && !self.backed.contains(r) {
            lines.push(format!("create {} {}", backup_name(&self.stamp, r), old));
        }
        let mut all = self.entries.clone();
        all.push(Entry { ref_: r.into(), old: old.into(), new: new.into(), pending });
        let j = self.journal_lines(git, &all)?;
        lines.push(j.clone());
        lines.push("prepare".into());
        lines.push("commit".into());
        let res = git.input(&["update-ref", "--no-deref", "--stdin"], &(lines.join("\n") + "\n"), true)?;
        if res.ok() {
            self.finish(all, &j);
        }
        Ok(res)
    }

    /// 밖에서 옮길 참조(merge --ff-only 등)를 옮기기 '전에' 덮기 전 값과 기록을 먼저 남긴다. 실패하면 기록은 그대로다.
    pub fn record_before(&mut self, git: &Git, r: &str, old: &str, new: &str) -> R<GitRes> {
        let added = !is_zero(old) && !self.backed.contains(r);
        let res = self.record_entry(git, r, old, new, true)?;
        self.last_added_backup = if res.ok() && added { Some(r.to_string()) } else { None };
        Ok(res)
    }

    /// record_before 로 남긴 마지막 항목의 이동이 실제로 끝났을 때 pending 을 풀어 확정한다.
    pub fn confirm_last(&mut self, git: &Git, r: &str) -> R<GitRes> {
        match self.entries.last() {
            Some(e) if e.ref_ == r && e.pending => {}
            _ => return Err(format!("되돌리기 기록의 마지막 항목이 끝나지 않은 {} 이동이 아니라 확정할 수 없습니다.", r)),
        }
        if self.journal_oid.is_none() {
            return Err("되돌리기 기록 항목을 찾지 못했습니다.".to_string());
        }
        let mut all = self.entries.clone();
        if let Some(last) = all.last_mut() {
            last.pending = false;
        }
        let line = self.journal_lines(git, &all)?;
        let new_oid = line.split_whitespace().nth(2).map(|s| s.to_string());
        let lines = vec!["start".to_string(), line, "prepare".to_string(), "commit".to_string()];
        let res = git.input(&["update-ref", "--no-deref", "--stdin"], &(lines.join("\n") + "\n"), true)?;
        if res.ok() {
            self.entries = all;
            self.journal_oid = new_oid;
        }
        Ok(res)
    }

    /// record_before 뒤 실제 이동이 실패했을 때 그 마지막 항목을 기록에서 뺀다.
    pub fn forget_last(&mut self, git: &Git, r: &str) -> R<GitRes> {
        let last = match self.entries.last() {
            Some(e) if e.ref_ == r => e.clone(),
            _ => return Err(format!("되돌리기 기록의 마지막 항목이 {} 가 아니라 뺄 수 없습니다.", r)),
        };
        let rest: Vec<Entry> = self.entries[..self.entries.len() - 1].to_vec();
        let mut lines = vec!["start".to_string()];
        let new_oid: Option<String>;
        if rest.is_empty() {
            let cur = self.journal_oid.clone().ok_or_else(|| "되돌리기 기록 항목을 찾지 못했습니다.".to_string())?;
            lines.push(format!("delete {} {}", self.journal_ref(), cur));
            new_oid = None;
        } else {
            let line = self.journal_lines(git, &rest)?;
            new_oid = line.split_whitespace().nth(2).map(|s| s.to_string());
            lines.push(line);
        }
        let drop_backup = self.last_added_backup.as_deref() == Some(r) && !is_zero(&last.old);
        if drop_backup {
            lines.push(format!("delete {} {}", backup_name(&self.stamp, r), last.old));
        }
        lines.push("prepare".into());
        lines.push("commit".into());
        let res = git.input(&["update-ref", "--no-deref", "--stdin"], &(lines.join("\n") + "\n"), true)?;
        if res.ok() {
            self.entries = rest;
            self.journal_oid = new_oid;
            if drop_backup {
                self.backed.remove(r);
            }
            self.last_added_backup = None;
        }
        Ok(res)
    }
}

fn read_journal(git: &Git, stamp: &str) -> R<Vec<Entry>> {
    let jr = format!("{}{}/journal", UNDO_ROOT, stamp);
    let r = git.q(&["cat-file", "-p", &jr])?;
    if !r.ok() {
        return Err(format!("되돌리기 기록 {} 을 읽지 못했습니다.", stamp));
    }
    serde_json::from_str(&r.out).map_err(|e| format!("되돌리기 기록 {} 이 깨졌습니다: {}", stamp, e))
}

/// (기록 이름, 항목 수) 최근 것부터.
pub fn list(git: &Git) -> R<Vec<(String, usize)>> {
    let m = ref_map(git, &[UNDO_ROOT])?;
    let mut stamps: Vec<String> = m.keys().filter_map(|k| k.strip_prefix(UNDO_ROOT)?.strip_suffix("/journal").map(|s| s.to_string())).collect();
    stamps.sort();
    stamps.reverse();
    let mut out = Vec::new();
    for s in stamps {
        let n = read_journal(git, &s).map(|v| v.len()).unwrap_or(0);
        out.push((s, n));
    }
    Ok(out)
}

/// 기록 하나를 되돌린다. 지금 값이 기록의 마지막 값과 모두 같을 때만 한다.
pub fn run(git: &Git, stamp: &str) -> R<String> {
    let entries = read_journal(git, stamp)?;
    // 끝나지 않은 이동이 있으면 어떤 참조도 바꾸기 전에 멈춘다.
    let pending: Vec<&Entry> = entries.iter().filter(|e| e.pending).collect();
    if !pending.is_empty() {
        let mut items: Vec<String> = Vec::new();
        for e in pending {
            let cur = ref_value(git, &e.ref_)?;
            let cur_s = match &cur {
                Some(v) if !is_zero(v) => v.chars().take(7).collect::<String>(),
                _ => "없음".to_string(),
            };
            items.push(format!(
                "{} {} → {} (지금 {})",
                crate::refs::short_names(&[e.ref_.clone()]),
                e.old.chars().take(7).collect::<String>(),
                e.new.chars().take(7).collect::<String>(),
                cur_s
            ));
        }
        return Err(format!(
            "되돌리기 기록 {stamp} 에 끝나지 않은 이동이 있어 되돌리지 않았습니다: {}. 그 이동은 적용 중 멈춰 실제로 일어났는지 이 기록만으로는 알 수 없습니다. 지금 브랜치 값이 맞다면 이 기록은 필요 없으니 다음 명령으로 지우면 됩니다: git for-each-ref --format=\"delete %(refname) %(objectname)\" refs/gitbridge/undo/{stamp}/ | git update-ref --stdin",
            items.join(", "),
            stamp = stamp
        ));
    }
    // 참조마다 처음 old 와 마지막 new.
    let mut order: Vec<String> = Vec::new();
    let mut span: BTreeMap<String, (String, String)> = BTreeMap::new();
    for e in &entries {
        match span.get_mut(&e.ref_) {
            Some(v) => v.1 = e.new.clone(),
            None => {
                order.push(e.ref_.clone());
                span.insert(e.ref_.clone(), (e.old.clone(), e.new.clone()));
            }
        }
    }
    let zero = zero_oid(git);
    let norm = |v: &str| -> String { if is_zero(v) { zero.clone() } else { v.to_string() } };
    // 1) 사전 검사: 되돌릴 것 / 이미 되돌림 / 그 뒤로 바뀜.
    let mut todo: Vec<String> = Vec::new();
    let mut done = 0usize;
    let mut changed: Vec<String> = Vec::new();
    for r in &order {
        let (old, new) = &span[r];
        if norm(old) == norm(new) {
            continue;
        }
        let cur = ref_value(git, r)?.unwrap_or_else(|| zero.clone());
        if cur == norm(new) {
            todo.push(r.clone());
        } else if cur == norm(old) {
            done += 1;
        } else {
            changed.push(r.clone());
        }
    }
    if !changed.is_empty() {
        return Err(format!("그 뒤로 바뀐 브랜치·태그가 있어 되돌리지 않았습니다: {}", crate::refs::short_names(&changed)));
    }
    // 2) 체크아웃 위치 검사. 무엇이든 바꾸기 전에 끝낸다.
    let mut keep_place: Option<String> = None;
    for r in &todo {
        if r.starts_with("refs/heads/") {
            match crate::refs::checkout_place(git, r)? {
                Place::Here => {
                    if is_zero(&span[r].0) {
                        return Err(format!("작업 중인 {} 는 이번 기록이 새로 만든 브랜치라 지울 수 없습니다. 다른 브랜치로 옮긴 뒤 다시 실행해 주세요.", r));
                    }
                    keep_place = Some(r.clone());
                }
                Place::Other(p) => {
                    return Err(format!("{} 가 다른 작업 폴더({})에서 작업 중인 브랜치라 되돌리지 않았습니다.", r, p));
                }
                Place::None => {}
            }
        }
    }
    // 3) 체크아웃 브랜치를 뺀 참조를 한 트랜잭션으로 되돌린다. 기록 참조는 아직 지우지 않는다.
    let mut lines = vec!["start".to_string()];
    for r in todo.iter().rev() {
        if Some(r) == keep_place.as_ref() {
            continue;
        }
        let (old, new) = &span[r];
        lines.push(if is_zero(old) {
            format!("delete {} {}", r, new)
        } else if is_zero(new) {
            format!("create {} {}", r, old)
        } else {
            format!("update {} {} {}", r, old, new)
        });
    }
    if lines.len() > 1 {
        lines.push("prepare".into());
        lines.push("commit".into());
        let res = git.input(&["update-ref", "--no-deref", "--stdin"], &(lines.join("\n") + "\n"), false)?;
        if !res.ok() {
            return Err(format!("브랜치·태그를 되돌리지 못했습니다. 변경은 확정되지 않았습니다.\n{}", res.detail()));
        }
    }
    // 4) 체크아웃 브랜치는 마지막에, HEAD 가 기대한 상태일 때만 되돌린다.
    if let Some(r) = &keep_place {
        let (old, new) = &span[r];
        let head_ref = git.q(&["symbolic-ref", "-q", "HEAD"])?;
        let head_val = git.q(&["rev-parse", "--verify", "HEAD"])?;
        if !(head_ref.ok() && head_ref.out.trim() == r.as_str() && head_val.ok() && head_val.out.trim() == new.as_str()) {
            return Err(format!("작업 중인 {} 가 그사이 바뀌어 되돌리지 않았습니다. 다른 브랜치·태그는 이미 되돌렸고 같은 기록으로 다시 실행하면 이어서 합니다.", r));
        }
        let res = git.run(&["reset", "--keep", old])?;
        if !res.ok() {
            return Err(format!("작업 중인 {} 를 되돌리지 못했습니다. 다른 브랜치·태그는 이미 되돌렸습니다. 작업 파일 변경을 정리한 뒤 같은 기록으로 다시 실행하면 이어서 합니다.\n{}", r, res.detail()));
        }
    }
    // 5) 모든 참조가 old 에 닿은 뒤에만 기록 참조를 지운다.
    let prefix = format!("{}{}/", UNDO_ROOT, stamp);
    let mut del = vec!["start".to_string()];
    // 지우기 직전까지 되돌린 참조가 다시 바뀌지 않았는지 같은 트랜잭션에서 검사한다.
    for r in &order {
        let (old, new) = &span[r];
        if norm(old) != norm(new) {
            del.push(format!("verify {} {}", r, norm(old)));
        }
    }
    for (r, o) in ref_map(git, &[prefix.as_str()])? {
        del.push(format!("delete {} {}", r, o));
    }
    del.push("prepare".into());
    del.push("commit".into());
    let mut msg = format!("되돌리기 기록 {} 에서 브랜치·태그 {}개를 이전 값으로 되돌렸습니다.", stamp, todo.len());
    if done > 0 {
        msg.push_str(&format!(" 이미 되돌려져 있던 것은 {}개입니다.", done));
    }
    let res = git.input(&["update-ref", "--no-deref", "--stdin"], &(del.join("\n") + "\n"), false)?;
    if !res.ok() {
        let mut moved_again: Vec<String> = Vec::new();
        for r in &order {
            let (old, new) = &span[r];
            if norm(old) == norm(new) {
                continue;
            }
            let cur = ref_value(git, r)?.unwrap_or_else(|| zero.clone());
            if cur != norm(old) {
                moved_again.push(r.clone());
            }
        }
        if !moved_again.is_empty() {
            return Err(format!(
                "되돌린 뒤 {} 가 다시 바뀌어 되돌리기 기록 {} 를 지우지 않고 남겼습니다. 이번 실행으로 되돌린 브랜치·태그는 {}개입니다. 지금 값을 확인한 뒤 같은 기록으로 다시 실행해 주세요.",
                crate::refs::short_names(&moved_again),
                stamp,
                todo.len()
            ));
        }
        msg.push_str(" 되돌리기 기록 정리는 보류했습니다.");
    }
    Ok(msg)
}
