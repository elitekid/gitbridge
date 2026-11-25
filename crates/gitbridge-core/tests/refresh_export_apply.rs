//! 실제 git 으로 돌리는 통합 시험: 번들 만들기 앞 서버 최신화, 최신화 실패, feature 범위, 저장소 자동 생성.
//! origin 은 로컬 bare 저장소(파일 경로)다. 격리 데이터 폴더(GITBRIDGE2_HOME)를 쓰므로 한 시험 함수 안에서 차례로 돈다.

use gitbridge_core::{create, export, queue, refresh};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .current_dir(dir)
        .args(["-c", "core.autocrlf=false", "-c", "init.defaultBranch=main"])
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git 실행");
    assert!(o.status.success(), "git {:?} 실패: {}", args, String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn commit(dir: &Path, name: &str) -> String {
    std::fs::write(dir.join(name), name).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", name]);
    git(dir, &["rev-parse", "HEAD"])
}

struct Fx {
    pusher: PathBuf,
    repo: PathBuf,
    out: PathBuf,
    root: PathBuf,
    c1: String,
    c2: String,
    c3: String,
}

fn base_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("refresh_export_apply")
}

/// 서버(bare)·pusher·작업 저장소를 만든다. 작업 저장소는 c1 에서 clone 한 뒤 서버만 c2(main)·c3(develop)로 앞선다.
fn fixture(tag: &str) -> Fx {
    let root = base_dir().join(tag);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // 격리 데이터 폴더(시나리오마다 새로).
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    unsafe { std::env::set_var("GITBRIDGE2_HOME", &home) };
    let server = root.join("alpha.git");
    git(&root, &["init", "-q", "--bare", "alpha.git"]);
    let pusher = root.join("pusher");
    git(&root, &["clone", "-q", server.to_str().unwrap(), "pusher"]);
    let c1 = commit(&pusher, "c1.txt");
    git(&pusher, &["branch", "-M", "main"]);
    git(&pusher, &["branch", "develop"]);
    git(&pusher, &["tag", "v1"]);
    git(&pusher, &["push", "-q", "origin", "main", "develop", "v1"]);
    let repo = root.join("alpha");
    git(&root, &["clone", "-q", server.to_str().unwrap(), "alpha"]);
    // 서버만 앞선다.
    let c2 = commit(&pusher, "c2.txt");
    git(&pusher, &["push", "-q", "origin", "main"]);
    git(&pusher, &["checkout", "-q", "develop"]);
    let c3 = commit(&pusher, "c3.txt");
    git(&pusher, &["push", "-q", "origin", "develop"]);
    git(&pusher, &["checkout", "-q", "main"]);
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    Fx { pusher, repo, out, root, c1, c2, c3 }
}

fn r(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn bundle_files(out: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(out)
        .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_file()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn head_ref(repo: &Path, name: &str) -> String {
    git(repo, &["rev-parse", name])
}

fn heads_of(repo: &Path, bundle: &str) -> String {
    git(repo, &["bundle", "list-heads", bundle])
}

#[test]
fn scenarios() {
    let mut report: Vec<String> = Vec::new();

    // ---- B1: 서버가 앞선 상태에서 범위별로 최신화 후 번들 만들기 ----
    for scope in ["base", "standard", "all"] {
        let fx = fixture(&format!("b1_{}", scope));
        assert_eq!(head_ref(&fx.repo, "refs/remotes/origin/main"), fx.c1, "시작 때 로컬 추적은 c1");
        let outs = export::export_refreshed(r(&fx.repo), r(&fx.out), scope, None, false);
        let last = outs.last().unwrap();
        report.push(format!("B1[{}] outcomes={} last_ok={} last_msg={}", scope, outs.len(), last.ok, last.message.lines().next().unwrap_or("")));
        assert!(last.ok, "B1[{}] 번들 만들기 실패: {}", scope, last.message);
        assert_eq!(outs.len(), 2, "B1[{}] [최신화, 번들] 두 결과", scope);
        assert!(outs[0].ok);
        let om = head_ref(&fx.repo, "refs/remotes/origin/main");
        let lm = head_ref(&fx.repo, "refs/heads/main");
        report.push(format!("B1[{}] origin/main={} refs/heads/main={} (서버 c2={})", scope, &om[..8], &lm[..8], &fx.c2[..8]));
        assert_eq!(om, fx.c2, "B1[{}] origin/main 이 갱신돼야 함", scope);
        assert_eq!(lm, fx.c2, "B1[{}] 로컬 main 이 갱신돼야 함", scope);
        if scope != "base" {
            assert_eq!(head_ref(&fx.repo, "refs/remotes/origin/develop"), fx.c3, "B1[{}] origin/develop 갱신", scope);
        }
        let file = last.file.clone().expect("번들 파일");
        let heads = heads_of(&fx.repo, &file);
        report.push(format!("B1[{}] bundle={} list-heads:\n{}", scope, file, heads));
        assert!(heads.contains(&fx.c2), "B1[{}] 번들에 서버 새 커밋(c2)이 있어야 함", scope);
        if scope != "base" {
            assert!(heads.contains(&fx.c3), "B1[{}] 번들에 develop 새 커밋(c3)이 있어야 함", scope);
        }
        // 임시 스냅샷 참조가 남지 않아야 한다.
        let leftover = git(&fx.repo, &["for-each-ref", "refs/gitbridge/snapshot/"]);
        assert!(leftover.is_empty(), "B1[{}] 스냅샷 임시 참조 남음: {}", scope, leftover);
    }

    // ---- B1 대조: 최신화 없는 옛 경로(export_repository)는 추적 참조를 갱신하지 않는다 ----
    {
        let fx = fixture("b1_control");
        let o = export::export_repository(r(&fx.repo), r(&fx.out), "standard", None, false);
        let om = head_ref(&fx.repo, "refs/remotes/origin/main");
        report.push(format!("B1대조[standard, 최신화 없음] ok={} origin/main={} (c1={}, c2={})", o.ok, &om[..8], &fx.c1[..8], &fx.c2[..8]));
        assert!(o.ok);
        assert_eq!(om, fx.c1, "대조: 최신화 없이는 origin/main 이 c1 그대로");
    }

    // ---- B2: 서버가 끊겼을 때 번들을 만들지 않고 오류를 보인다 ----
    for scope in ["base", "standard", "all"] {
        let fx = fixture(&format!("b2_{}", scope));
        let before_main = head_ref(&fx.repo, "refs/heads/main");
        git(&fx.repo, &["remote", "set-url", "origin", r(&fx.root.join("no_such_server.git"))]);
        let outs = export::export_refreshed(r(&fx.repo), r(&fx.out), scope, None, false);
        let last = outs.last().unwrap();
        report.push(format!("B2[{}] outcomes={} last_ok={} kind={} msg={}", scope, outs.len(), last.ok, last.kind, last.message.lines().next().unwrap_or("")));
        assert!(!last.ok, "B2[{}] 서버가 끊겼는데 성공으로 나옴", scope);
        assert_eq!(outs.len(), 1, "B2[{}] 최신화 결과만 돌려줘야 함", scope);
        assert!(last.file.is_none());
        assert!(bundle_files(&fx.out).is_empty(), "B2[{}] 번들이 만들어짐: {:?}", scope, bundle_files(&fx.out));
        assert_eq!(head_ref(&fx.repo, "refs/heads/main"), before_main, "B2[{}] 로컬 main 이 바뀌면 안 됨", scope);
        let leftover = git(&fx.repo, &["for-each-ref", "refs/gitbridge/snapshot/"]);
        assert!(leftover.is_empty(), "B2[{}] 스냅샷 임시 참조 남음", scope);
    }

    // ---- B3: feature 범위는 최신화 없이 만들어진다(서버가 끊겨도) ----
    {
        let fx = fixture("b3_feature");
        git(&fx.repo, &["checkout", "-q", "-b", "feature/refresh-test"]);
        commit(&fx.repo, "f1.txt");
        let before_om = head_ref(&fx.repo, "refs/remotes/origin/main");
        let before_main = head_ref(&fx.repo, "refs/heads/main");
        // 서버를 끊어도 만들어져야 한다.
        git(&fx.repo, &["remote", "set-url", "origin", r(&fx.root.join("no_such_server.git"))]);
        let o = create::create_feature(r(&fx.repo), "feature/refresh-test", r(&fx.out), false);
        report.push(format!("B3[feature, 서버 끊김] ok={} kind={} msg={}", o.ok, o.kind, o.message.lines().next().unwrap_or("")));
        assert!(o.ok, "B3 feature 번들 실패: {}", o.message);
        assert_eq!(head_ref(&fx.repo, "refs/remotes/origin/main"), before_om, "B3 feature 는 origin/main 을 건드리지 않음");
        assert_eq!(head_ref(&fx.repo, "refs/heads/main"), before_main, "B3 feature 는 로컬 main 을 건드리지 않음");
        let heads = heads_of(&fx.repo, o.file.as_deref().unwrap());
        report.push(format!("B3 bundle list-heads:\n{}", heads));
    }

    // ---- C: 번들 적용 때 저장소 자동 생성 ----
    // C1: 모음 폴더 지정 + 전체 이력 저장소 번들 + 같은 이름 폴더 없음 -> 자동 생성
    {
        let fx = fixture("c1_auto");
        let outs = export::export_refreshed(r(&fx.repo), r(&fx.out), "standard", None, true);
        let bundle = outs.last().unwrap().file.clone().expect("번들");
        let coll = fx.root.join("collection");
        std::fs::create_dir_all(&coll).unwrap();
        let mut plan = queue::QueuePlan::new();
        queue::plan_add(&mut plan, &[bundle.clone()], r(&coll)).unwrap();
        let row = &plan.rows[0];
        report.push(format!("C1 kind={} repo={} create_new={} note={:?}", row.kind, row.repo, row.create_new, row.note));
        assert!(row.create_new && row.repo.ends_with("alpha"), "C1 자동 생성 대상이어야 함: {:?}", row);
        let cancel = AtomicBool::new(false);
        let res = queue::plan_run(&plan.rows, &[0], true, &cancel);
        for o in &res {
            report.push(format!("C1 적용 결과 ok={} kind={} msg={}", o.ok, o.kind, o.message.lines().next().unwrap_or("")));
        }
        assert!(res.iter().all(|o| o.ok), "C1 적용 실패: {:?}", res.iter().map(|o| o.message.clone()).collect::<Vec<_>>());
        let made = coll.join("alpha");
        assert!(made.join(".git").exists(), "C1 저장소가 만들어져야 함");
        assert_eq!(head_ref(&made, "refs/heads/main"), fx.c2, "C1 만든 저장소의 main 이 번들 값");
        report.push(format!("C1 만들어진 저장소 main={} branches={}", &head_ref(&made, "refs/heads/main")[..8], git(&made, &["branch", "--format=%(refname:short)"]).replace('\n', ",")));
    }
    // C2: 기준 번들(전체)도 자동 생성
    {
        let fx = fixture("c2_base");
        let outs = export::export_refreshed(r(&fx.repo), r(&fx.out), "base", None, true);
        let bundle = outs.last().unwrap().file.clone().expect("번들");
        let coll = fx.root.join("collection");
        std::fs::create_dir_all(&coll).unwrap();
        let mut plan = queue::QueuePlan::new();
        queue::plan_add(&mut plan, &[bundle], r(&coll)).unwrap();
        let row = &plan.rows[0];
        report.push(format!("C2 kind={} repo={} create_new={} note={:?}", row.kind, row.repo, row.create_new, row.note));
        assert!(row.create_new && row.repo.ends_with("alpha"), "C2 기준 번들 자동 생성 대상");
    }
    // C3: 모음 폴더 미지정 -> 자동 생성 안 함, 안내
    {
        let fx = fixture("c3_noroot");
        let outs = export::export_refreshed(r(&fx.repo), r(&fx.out), "standard", None, true);
        let bundle = outs.last().unwrap().file.clone().unwrap();
        let mut plan = queue::QueuePlan::new();
        queue::plan_add(&mut plan, &[bundle], "").unwrap();
        let row = &plan.rows[0];
        report.push(format!("C3 모음 폴더 미지정: repo={:?} create_new={} note={:?}", row.repo, row.create_new, row.note));
        assert!(row.repo.is_empty() && !row.create_new, "C3 자동 생성하면 안 됨");
    }
    // C4: 같은 이름 폴더가 있음 -> 새로 만들지 않고 안내
    {
        let fx = fixture("c4_exists");
        let outs = export::export_refreshed(r(&fx.repo), r(&fx.out), "standard", None, true);
        let bundle = outs.last().unwrap().file.clone().unwrap();
        let coll = fx.root.join("collection");
        std::fs::create_dir_all(coll.join("alpha")).unwrap();
        let mut plan = queue::QueuePlan::new();
        queue::plan_add(&mut plan, &[bundle], r(&coll)).unwrap();
        let row = &plan.rows[0];
        report.push(format!("C4 같은 이름 폴더: repo={:?} create_new={} note={:?}", row.repo, row.create_new, row.note));
        assert!(row.repo.is_empty() && !row.create_new && row.note.contains("같은 이름의 폴더"), "C4 안내만 해야 함");
    }
    // C5: 증분 번들 -> 자동 생성 안 함
    {
        let fx = fixture("c5_incremental");
        let first = export::export_refreshed(r(&fx.repo), r(&fx.out), "standard", None, false);
        assert!(first.last().unwrap().ok, "C5 첫 번들");
        // 서버에 새 커밋 -> 두 번째는 증분
        commit(&fx.pusher, "c4.txt");
        git(&fx.pusher, &["push", "-q", "origin", "main"]);
        let second = export::export_refreshed(r(&fx.repo), r(&fx.out), "standard", None, false);
        let o2 = second.last().unwrap();
        assert!(o2.ok, "C5 두 번째 번들: {}", o2.message);
        let bundle = o2.file.clone().unwrap();
        let coll = fx.root.join("collection");
        std::fs::create_dir_all(&coll).unwrap();
        let mut plan = queue::QueuePlan::new();
        queue::plan_add(&mut plan, &[bundle], r(&coll)).unwrap();
        let row = &plan.rows[0];
        report.push(format!("C5 증분: kind={} repo={:?} create_new={} note={:?}", row.kind, row.repo, row.create_new, row.note));
        assert!(row.repo.is_empty() && !row.create_new, "C5 증분은 자동 생성하면 안 됨");
    }
    let _ = refresh::refresh; // 사용 표시(공개 API 존재 확인)

    let text = report.join("\n");
    let _ = std::fs::write(base_dir().join("report.txt"), &text);
    println!("{}", text);
}
