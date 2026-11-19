//! 번들 만들기 미리보기(읽기만). 만들기 전에 "이번에 무엇이 담길지"를 줄로 보여 준다.
//! git 읽기 명령(Git::q)과 ls-remote 만 쓰고, 참조·파일·표식을 바꾸지 않는다.
//! 서버 최신화(fetch)도 하지 않으므로 서버 끝 커밋이 이 PC 에 아직 없으면 커밋 수는 "확인 불가"로 낸다.

use gitbridge_core::export::{pick_base_branch, server_heads, EXPORTED_STANDARD, MARK_ROOT};
use gitbridge_core::gitrun::Git;
use gitbridge_core::manifest::Manifest;
use gitbridge_core::refs::{get_repo_label, is_ancestor, name_ok, ref_commit, ref_value, NAME_RULE_TEXT};

fn short(o: &str) -> &str {
    &o[..o.len().min(8)]
}

/// rev-list --count 결과. 못 세면 "확인 불가".
fn count(git: &Git, range: &[&str]) -> String {
    let mut args = vec!["rev-list", "--count"];
    args.extend_from_slice(range);
    match git.q(&args) {
        Ok(r) if r.ok() && !r.out.trim().is_empty() && r.out.trim().chars().all(|c| c.is_ascii_digit()) => r.out.trim().to_string(),
        _ => "확인 불가".to_string(),
    }
}

fn n_text(c: &str) -> String {
    if c == "확인 불가" {
        c.to_string()
    } else {
        format!("{}개", c)
    }
}

/// 한 브랜치의 지난 표식과 지금 끝을 비교한다. 돌려주는 첫 값: "same"(그대로)·"incr"(증분)·"full"(전체)·"unknown"(확인 불가).
fn judge(git: &Git, name: &str, tip: &str, full: bool) -> Result<(&'static str, String), String> {
    let have = git.q(&["cat-file", "-e", &format!("{}^{{commit}}", tip)])?.ok();
    if full {
        let n = if have { count(git, &[tip]) } else { "확인 불가".to_string() };
        return Ok(("full", format!("{}: 전체를 담아 만듭니다(전체 옵션). 커밋 {}", name, n_text(&n))));
    }
    let mark = ref_commit(git, &format!("{}{}", MARK_ROOT, name))?;
    let Some(mk) = mark else {
        let n = if have { count(git, &[tip]) } else { "확인 불가".to_string() };
        return Ok(("full", format!("{}: 지난번 보낸 위치가 없어 전체를 담아 만듭니다. 커밋 {}", name, n_text(&n))));
    };
    if mk == tip {
        return Ok(("same", format!("{}: 지난번 보낸 뒤로 그대로입니다 (끝 {}) — 전체를 담은 번들로 다시 만들 예상", name, short(tip))));
    }
    if !have {
        return Ok((
            "unknown",
            format!("{}: 서버 끝 {} 가 이 PC 에 아직 없어 커밋 수를 셀 수 없습니다(만들 때 받아서 정합니다). 지난번 보낸 위치 {}", name, short(tip), short(&mk)),
        ));
    }
    let carets = format!("^{}", mk);
    if is_ancestor(git, &mk, tip)? {
        let n = count(git, &[tip, carets.as_str()]);
        Ok(("incr", format!("{}: 바뀐 부분만 — 지난번 보낸 위치 {} 이후 커밋 {}", name, short(&mk), n_text(&n))))
    } else {
        let n = count(git, &[tip]);
        Ok(("full", format!("{}: 지난번 보낸 위치 {} 가 지금 끝의 조상이 아니어서 전체를 담아 만듭니다. 커밋 {}", name, short(&mk), n_text(&n))))
    }
}

/// 미리보기 본체. scope 는 "base" | "standard" | "all" | "feature".
/// feature 일 때 branch 는 만들 작업 브랜치 이름, 그 밖에는 지정한 기준 브랜치(없으면 자동)다.
pub fn preview(repo: &str, scope: &str, branch: Option<&str>, full: bool) -> Result<Vec<String>, String> {
    let git = Git::open(repo)?;
    if !git.q(&["rev-parse", "--show-toplevel"])?.ok() {
        return Err("git 저장소가 아닙니다.".to_string());
    }
    let mut out: Vec<String> = Vec::new();
    match scope {
        "base" => base(&git, branch, full, &mut out)?,
        "standard" | "all" => repository(&git, scope, branch, full, &mut out)?,
        "feature" => feature(&git, branch, full, &mut out)?,
        _ => return Err(format!("알 수 없는 범위입니다: {}", scope)),
    }
    out.push("미리보기는 읽기만 했습니다. 실제 결과는 만들 때 서버 상태에 따라 달라질 수 있습니다.".to_string());
    Ok(out)
}

fn base(git: &Git, branch: Option<&str>, full: bool, out: &mut Vec<String>) -> Result<(), String> {
    let label = get_repo_label(git)?;
    let heads = server_heads(git)?;
    let b = pick_base_branch(&heads, branch, &label)?;
    let tip = heads[&b].clone();
    out.push(format!("기준 브랜치 번들 — 저장소 {} / 서버 {} = {}", label, b, short(&tip)));
    let (kind, line) = judge(git, &b, &tip, full)?;
    out.push(line);
    out.push(
        match kind {
            "same" => "예상: 바뀐 것이 없어 전체를 담은 번들로 다시 만듭니다",
            "incr" => "예상: 바뀐 부분만 담은 번들",
            "full" => "예상: 전체를 담은 번들",
            _ => "예상: 만들 때 정해집니다",
        }
        .to_string(),
    );
    Ok(())
}

fn repository(git: &Git, scope: &str, branch: Option<&str>, full: bool, out: &mut Vec<String>) -> Result<(), String> {
    let label = get_repo_label(git)?;
    let heads = server_heads(git)?;
    let b = pick_base_branch(&heads, branch, &label)?;
    if scope == "all" {
        out.push(format!("전체 저장소 번들 — 저장소 {} / 서버 브랜치 {}개 / 기준 {}", label, heads.len(), b));
        out.push("예상: 전체를 담은 번들 (all 은 늘 전체를 담아 만듭니다. 태그도 모두 담습니다)".to_string());
        return Ok(());
    }
    let mut selected = vec![b.clone()];
    if b != "develop" && heads.contains_key("develop") {
        selected.push("develop".to_string());
    }
    out.push(format!("주요 브랜치 번들(standard) — 저장소 {} / 담을 브랜치 {} (+ 모든 태그)", label, selected.join(", ")));
    let mut kinds: Vec<&'static str> = Vec::new();
    if !full {
        // 만들기와 같은 조건: 지난 매니페스트가 있고 읽히며, 기본 브랜치·저장소 이름이 같아야 증분이다.
        let why = if ref_value(git, EXPORTED_STANDARD)?.is_none() {
            Some("지난번 만든 저장소 번들 기록이 없어서".to_string())
        } else {
            match Manifest::read(git, EXPORTED_STANDARD) {
                Ok(m) if m.default == b && m.label == label => None,
                Ok(_) => Some("지난번 번들과 기본 브랜치나 저장소 이름이 달라서".to_string()),
                Err(_) => Some("지난번 번들 기록을 읽을 수 없어서".to_string()),
            }
        };
        if let Some(w) = why {
            out.push(format!("{} 전체를 담아 만들 것입니다.", w));
            kinds.push("full");
        }
    }
    for n in &selected {
        let (kind, line) = judge(git, n, &heads[n], full)?;
        kinds.push(kind);
        out.push(line);
    }
    let verdict = if kinds.iter().any(|k| *k == "full") {
        "예상: 전체를 담은 번들"
    } else if kinds.iter().any(|k| *k == "unknown") {
        "예상: 만들 때 정해집니다 (서버 커밋을 받아야 알 수 있습니다)"
    } else if kinds.iter().all(|k| *k == "same") {
        "예상: 바뀐 것이 없어 전체를 담은 번들로 다시 만듭니다 (태그가 바뀌었으면 만들 때 바뀐 부분만 담은 번들이 만들어집니다)"
    } else {
        "예상: 바뀐 부분만 담은 번들 (태그 변화는 만들 때 한 번 더 확인합니다)"
    };
    out.push(verdict.to_string());
    Ok(())
}

fn feature(git: &Git, branch: Option<&str>, full: bool, out: &mut Vec<String>) -> Result<(), String> {
    let name = branch.unwrap_or("").trim();
    if !name_ok(name) {
        return Err(format!("브랜치 이름이 규칙에 맞지 않습니다: {}  ({})", name, NAME_RULE_TEXT));
    }
    out.push(format!("작업 브랜치 번들 — {}", name));
    out.push("서버에 접속하지 않고 지금 로컬 브랜치 상태로 만듭니다.".to_string());
    let st = git.q(&["status", "--porcelain"])?;
    if st.ok() && !st.out.trim().is_empty() {
        out.push("주의: 커밋되지 않은 변경이 있습니다. 만들기는 이 상태에서 멈춥니다.".to_string());
    }
    let Some(tip) = ref_commit(git, &format!("refs/heads/{}", name))? else {
        out.push(format!("주의: 로컬에 {} 브랜치가 없습니다. 만들기는 멈춥니다.", name));
        return Ok(());
    };
    out.push(format!("로컬 {} = {}", name, short(&tip)));
    match server_heads(git) {
        Ok(heads) => match heads.get(name) {
            Some(s) if *s == tip => out.push("서버의 같은 이름 브랜치와 끝이 같습니다.".to_string()),
            Some(s) => out.push(format!("서버의 같은 이름 브랜치는 {} 로 로컬과 다릅니다. 최신화로 로컬이 따라갈 수 있습니다.", short(s))),
            None => out.push("서버에는 같은 이름 브랜치가 없습니다(로컬에서만 만든 브랜치입니다).".to_string()),
        },
        Err(e) => out.push(format!("서버를 읽지 못했습니다(최신화에서 멈출 수 있습니다): {}", e.lines().next().unwrap_or(""))),
    }
    let (kind, line) = judge(git, name, &tip, full)?;
    out.push(line);
    out.push(
        match kind {
            "same" => "예상: 바뀐 것이 없어 전체를 담은 번들로 다시 만듭니다",
            "incr" => "예상: 바뀐 부분만 담은 번들",
            _ => "예상: 전체를 담은 번들",
        }
        .to_string(),
    );
    Ok(())
}
