//! GitBridge 2.0.0 명령줄. 인자는 손으로 해석하고 모든 일은 gitbridge_core 가 한다.

use gitbridge_core::refs::{format_result, Outcome};
use gitbridge_core::{apply, create, export, gitrun, log, queue, refresh, restore, store, undo, TOOL};
use std::collections::HashMap;
use std::process::ExitCode;

const USAGE: &str = "\
사용법: GitBridge <하위명령> [옵션]

  export  --repo P --kind base|standard|all [--branch B] [--full] --out DIR
  create  --repo P --branch B --out DIR [--full]
  refresh --repo P
  apply   --bundle F --repo P [--sync-all]
  restore --bundle F --dest P
  queue   --dir D --src-root P [--apply]
  undo list --repo P
  undo run  --repo P [--id 기록이름]

공통: --json (결과를 JSON 으로 출력)  --help  --version";

struct Args {
    vals: HashMap<String, String>,
    flags: Vec<String>,
}

impl Args {
    fn flag(&self, n: &str) -> bool {
        self.flags.iter().any(|f| f == n)
    }
    fn opt(&self, n: &str) -> Option<&str> {
        self.vals.get(n).map(|s| s.as_str())
    }
    fn need(&self, n: &str) -> Result<&str, String> {
        self.opt(n).filter(|s| !s.is_empty()).ok_or_else(|| format!("--{} 값이 필요합니다.", n))
    }
}

/// values: 값을 받는 옵션, flags: 값 없는 옵션. 공통 --json 은 항상 받는다.
fn parse(rest: &[String], values: &[&str], flags: &[&str]) -> Result<Args, String> {
    let mut a = Args { vals: HashMap::new(), flags: Vec::new() };
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i].as_str();
        let Some(name) = t.strip_prefix("--") else {
            return Err(format!("알 수 없는 인자: {}", t));
        };
        let (name, inline) = match name.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (name, None),
        };
        if name == "json" || flags.contains(&name) {
            if inline.is_some() {
                return Err(format!("--{} 는 값을 받지 않습니다.", name));
            }
            a.flags.push(name.to_string());
        } else if values.contains(&name) {
            let v = match inline {
                Some(v) => v,
                None => {
                    i += 1;
                    match rest.get(i) {
                        Some(v) => v.clone(),
                        None => return Err(format!("--{} 값이 필요합니다.", name)),
                    }
                }
            };
            a.vals.insert(name.to_string(), v);
        } else {
            return Err(format!("알 수 없는 옵션: --{}", name));
        }
        i += 1;
    }
    Ok(a)
}

fn human(o: &Outcome) -> String {
    let text = format_result(o);
    let n = o.message.lines().count().max(1);
    let lines: Vec<&str> = text.lines().collect();
    let k = n.min(lines.len());
    let mut out = lines[..k].iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
    for l in &lines[k..] {
        out.push('\n');
        out.push_str(l);
    }
    out
}

fn record(o: &Outcome, repo: &str, bundle: Option<&str>) {
    if let Err(e) = store::append_history(o, repo, bundle) {
        eprintln!("기록을 남기지 못했습니다: {}", e);
    }
}

fn show(o: &Outcome, json: bool) -> ExitCode {
    if json {
        match serde_json::to_string(o) {
            Ok(s) => println!("{}", s),
            Err(e) => {
                eprintln!("JSON 변환에 실패했습니다: {}", e);
                return ExitCode::from(1);
            }
        }
    } else {
        println!("{}", human(o));
    }
    ExitCode::from(if o.ok { 0 } else { 1 })
}

fn run(cmd: &str, rest: &[String]) -> Result<ExitCode, String> {
    match cmd {
        "export" => {
            let a = parse(rest, &["repo", "kind", "branch", "out"], &["full"])?;
            let (repo, kind, out) = (a.need("repo")?, a.need("kind")?, a.need("out")?);
            let init = |a: &Args| init_log(a.flag("json"));
            init(&a);
            let o = match kind {
                "base" => export::export_base(repo, out, a.flag("full"), a.opt("branch")),
                "standard" | "all" => export::export_repository(repo, out, kind, a.opt("branch"), a.flag("full")),
                _ => return Err("--kind 은 base, standard, all 중 하나여야 합니다.".into()),
            };
            record(&o, repo, o.file.as_deref());
            Ok(show(&o, a.flag("json")))
        }
        "create" => {
            let a = parse(rest, &["repo", "branch", "out"], &["full"])?;
            let (repo, branch, out) = (a.need("repo")?, a.need("branch")?, a.need("out")?);
            init_log(a.flag("json"));
            let o = create::create_feature(repo, branch, out, a.flag("full"));
            record(&o, repo, o.file.as_deref());
            Ok(show(&o, a.flag("json")))
        }
        "refresh" => {
            let a = parse(rest, &["repo"], &[])?;
            let repo = a.need("repo")?;
            init_log(a.flag("json"));
            let o = refresh::refresh(repo);
            record(&o, repo, None);
            Ok(show(&o, a.flag("json")))
        }
        "apply" => {
            let a = parse(rest, &["bundle", "repo"], &["sync-all"])?;
            let (bundle, repo) = (a.need("bundle")?, a.need("repo")?);
            init_log(a.flag("json"));
            let o = apply::apply(bundle, repo, a.flag("sync-all"));
            record(&o, repo, Some(bundle));
            Ok(show(&o, a.flag("json")))
        }
        "restore" => {
            let a = parse(rest, &["bundle", "dest"], &[])?;
            let (bundle, dest) = (a.need("bundle")?, a.need("dest")?);
            init_log(a.flag("json"));
            let o = restore::restore_new_repository(bundle, dest);
            record(&o, dest, Some(bundle));
            Ok(show(&o, a.flag("json")))
        }
        "queue" => {
            let a = parse(rest, &["dir", "src-root"], &["apply"])?;
            let (dir, root) = (a.need("dir")?, a.need("src-root")?);
            init_log(a.flag("json"));
            let list = queue::run_queue(dir, root, a.flag("apply"));
            if a.flag("json") {
                match serde_json::to_string(&list) {
                    Ok(s) => println!("{}", s),
                    Err(e) => {
                        eprintln!("JSON 변환에 실패했습니다: {}", e);
                        return Ok(ExitCode::from(1));
                    }
                }
            } else {
                for o in &list {
                    println!("{}", human(o));
                }
            }
            Ok(ExitCode::from(if list.iter().all(|o| o.ok) { 0 } else { 1 }))
        }
        "undo" => {
            let Some(sub) = rest.first().map(|s| s.as_str()) else {
                return Err("undo 뒤에 list 또는 run 이 필요합니다.".into());
            };
            let a = match sub {
                "list" => parse(&rest[1..], &["repo"], &[])?,
                "run" => parse(&rest[1..], &["repo", "id"], &[])?,
                _ => return Err(format!("알 수 없는 undo 하위명령: {}", sub)),
            };
            let json = a.flag("json");
            init_log(json);
            let repo = a.need("repo")?;
            let fail = |msg: String| -> ExitCode {
                if json {
                    println!("{}", serde_json::json!({"kind": "멈춤", "message": msg, "ok": false}));
                } else {
                    println!("[멈춤] {}", msg);
                }
                ExitCode::from(1)
            };
            let git = match gitrun::Git::open(repo) {
                Ok(g) => g,
                Err(e) => return Ok(fail(e)),
            };
            if sub == "list" {
                match undo::list(&git) {
                    Ok(items) => {
                        if json {
                            let v: Vec<_> = items.iter().map(|(id, n)| serde_json::json!({"id": id, "entries": n})).collect();
                            println!("{}", serde_json::Value::Array(v));
                        } else if items.is_empty() {
                            println!("되돌릴 기록이 없습니다.");
                        } else {
                            println!("되돌리기 기록 {}개 (최근 것부터):", items.len());
                            for (id, n) in &items {
                                println!("  {}  (브랜치·태그 변경 {}개)", id, n);
                            }
                        }
                        Ok(ExitCode::SUCCESS)
                    }
                    Err(e) => Ok(fail(e)),
                }
            } else {
                let id = match a.opt("id") {
                    Some(s) if !s.is_empty() => s.to_string(),
                    _ => match undo::list(&git) {
                        Ok(items) => match items.into_iter().next() {
                            Some((id, _)) => id,
                            None => return Ok(fail("되돌릴 기록이 없습니다.".into())),
                        },
                        Err(e) => return Ok(fail(e)),
                    },
                };
                match undo::run(&git, &id) {
                    Ok(msg) => {
                        if json {
                            println!("{}", serde_json::json!({"kind": "되돌리기", "message": msg, "ok": true, "undo": id}));
                        } else {
                            println!("[되돌리기] {}", msg);
                        }
                        Ok(ExitCode::SUCCESS)
                    }
                    Err(e) => Ok(fail(e)),
                }
            }
        }
        _ => Err(format!("알 수 없는 하위명령: {}", cmd)),
    }
}

fn init_log(json: bool) {
    let file = store::log_file().ok();
    log::init(!json, file.as_deref());
}

#[cfg(windows)]
fn console_utf8() {
    unsafe {
        windows_sys::Win32::System::Console::SetConsoleOutputCP(65001);
    }
}
#[cfg(not(windows))]
fn console_utf8() {}

fn main() -> ExitCode {
    console_utf8();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("{}", TOOL);
        return ExitCode::SUCCESS;
    }
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}\n\n{}", TOOL, USAGE);
        return ExitCode::SUCCESS;
    }
    match run(&args[0], &args[1..]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}\n{}", e, USAGE);
            ExitCode::from(1)
        }
    }
}
