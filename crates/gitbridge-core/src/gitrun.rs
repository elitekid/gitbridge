//! git 실행. 저장소 경로로 Windows Git 과 WSL Git 을 자동으로 고르고,
//! 처음 쓰는 Git 마다 버전을 확인한다. 성공은 종료 코드로만 판정한다.

use crate::R;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// 엔진이 쓰는 기능 가운데 가장 새 것: fetch --atomic(2.31.0).
/// update-ref --stdin 의 start/prepare/commit 은 2.27.0 부터다.
pub const MIN_GIT: (u32, u32, u32) = (2, 31, 0);
const MIN_GIT_TEXT: &str = "2.31.0";
const MIN_GIT_WHY: &str = "fetch --atomic·update-ref --stdin 트랜잭션";

#[derive(Debug, Clone)]
pub struct GitRes {
    pub code: i32,
    pub out: String,
    pub err: String,
}

impl GitRes {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
    /// 오류 출력이 있으면 그것, 없으면 표준 출력.
    pub fn detail(&self) -> &str {
        if self.err.is_empty() {
            &self.out
        } else {
            &self.err
        }
    }
    pub fn lines(&self) -> Vec<&str> {
        self.out.lines().filter(|x| !x.is_empty()).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Loc {
    Win,
    Wsl(String),
}

/// \\wsl.localhost\D\path 또는 \\wsl$\D\path 면 (배포판, Linux 경로).
pub fn wsl_location(path: &str) -> Option<(String, String)> {
    if !cfg!(windows) || path.is_empty() {
        return None;
    }
    let p = path.replace('\\', "/");
    let rest = p.strip_prefix("//")?;
    let (host, rest) = rest.split_once('/').unwrap_or((rest, ""));
    let host_l = host.to_ascii_lowercase();
    if host_l != "wsl.localhost" && host_l != "wsl$" {
        return None;
    }
    let (distro, tail) = match rest.split_once('/') {
        Some((d, t)) => (d, format!("/{}", t)),
        None => (rest, "/".to_string()),
    };
    if distro.is_empty() {
        return None;
    }
    Some((distro.to_string(), tail))
}

pub fn is_drive_path(a: &str) -> bool {
    let b = a.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

pub fn loc_of(path: &str) -> Loc {
    match wsl_location(path) {
        Some((d, _)) => Loc::Wsl(d),
        None => Loc::Win,
    }
}

fn decode(data: &[u8]) -> String {
    // WSL 자체 오류는 UTF-16LE, Linux 명령의 출력은 UTF-8 이다.
    let mut out = String::new();
    let mut d = data;
    while d.len() > 1 && d[1] == 0 {
        let end = d.windows(2).position(|w| w == b"\n\0");
        let take = match end {
            Some(e) => e + 2,
            None => d.len() - d.len() % 2,
        };
        let units: Vec<u16> = d[..take].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        out.push_str(&String::from_utf16_lossy(&units));
        d = &d[take..];
        if end.is_none() {
            break;
        }
    }
    out.push_str(&String::from_utf8_lossy(d));
    out.trim_end().replace("\r\n", "\n")
}

fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

fn find_windows_git() -> Option<PathBuf> {
    if let Some(p) = which(if cfg!(windows) { "git.exe" } else { "git" }) {
        return Some(p);
    }
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(base) = std::env::var_os(var) {
            let p = Path::new(&base).join("Git").join("cmd").join("git.exe");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    let local = std::env::var_os("LOCALAPPDATA")?;
    let p = Path::new(&local).join("Programs").join("Git").join("cmd").join("git.exe");
    p.is_file().then_some(p)
}

fn wsl_exe() -> R<PathBuf> {
    which("wsl.exe").ok_or_else(|| "WSL 실행기(wsl.exe)를 찾을 수 없습니다. 이 경로는 WSL 안에 있어 WSL 이 필요합니다. WSL 설치 상태를 확인해 주세요.".to_string())
}

fn wsl_cmd(distro: &str, args: &[&str]) -> R<Command> {
    let mut c = Command::new(wsl_exe()?);
    c.args(["--distribution", distro, "--exec"]).args(args);
    no_window(&mut c);
    Ok(c)
}

struct Cache {
    win_git: Option<Result<PathBuf, String>>,
    checked: HashMap<Loc, Result<(), String>>,
    to_linux: HashMap<(String, String), String>,
    to_win: HashMap<(String, String), String>,
    /// Windows Git 이 없을 때 저장소 없는 호출을 대신 맡을 WSL 배포판(한 번만 고른다).
    fallback: Option<Option<String>>,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

fn with_cache<T>(f: impl FnOnce(&mut Cache) -> T) -> T {
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let c = g.get_or_insert_with(|| Cache { win_git: None, checked: HashMap::new(), to_linux: HashMap::new(), to_win: HashMap::new(), fallback: None });
    f(c)
}

/// Windows Git 이 없을 때 쓸 WSL 배포판. WSL_DISTRO_NAME, 그다음 `wsl --list --quiet` 순서로
/// docker-desktop 류를 빼고 Git 버전 확인을 통과한 첫 배포판을 고른다(1.0.5 와 같은 순서).
fn wsl_fallback() -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    if let Some(v) = with_cache(|c| c.fallback.clone()) {
        return v;
    }
    let mut names: Vec<String> = Vec::new();
    if let Ok(d) = std::env::var("WSL_DISTRO_NAME") {
        if !d.trim().is_empty() {
            names.push(d.trim().to_string());
        }
    }
    if let Ok(exe) = wsl_exe() {
        let mut c = Command::new(exe);
        c.args(["--list", "--quiet"]);
        no_window(&mut c);
        if let Ok((0, out, _)) = run_raw(c, None) {
            for line in decode(&out).lines() {
                let n = line.trim_matches(|ch: char| ch.is_whitespace() || ch == '\0').to_string();
                if !n.is_empty() && !n.to_ascii_lowercase().starts_with("docker-desktop") && !names.iter().any(|x| x.eq_ignore_ascii_case(&n)) {
                    names.push(n);
                }
            }
        }
    }
    let found = names.into_iter().find(|d| ensure_git(&Loc::Wsl(d.clone())).is_ok());
    with_cache(|c| c.fallback = Some(found.clone()));
    found
}

fn windows_git() -> R<PathBuf> {
    with_cache(|c| {
        c.win_git
            .get_or_insert_with(|| find_windows_git().ok_or_else(|| "Windows Git 을 찾을 수 없습니다. Git for Windows 가 설치돼 있는지 확인해 주세요 (2.31.0 이상 필요).".to_string()))
            .clone()
    })
}

fn run_raw(mut cmd: Command, input: Option<&[u8]>) -> R<(i32, Vec<u8>, Vec<u8>)> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = cmd.spawn().map_err(|e| format!("프로그램을 실행하지 못했습니다: {}", e))?;
    let writer = input.map(|data| {
        let mut stdin = child.stdin.take();
        let data = data.to_vec();
        std::thread::spawn(move || {
            if let Some(s) = stdin.as_mut() {
                let _ = s.write_all(&data);
            }
        })
    });
    let out = child.wait_with_output().map_err(|e| format!("프로그램 출력을 읽지 못했습니다: {}", e))?;
    if let Some(w) = writer {
        let _ = w.join();
    }
    Ok((out.status.code().unwrap_or(-1), out.stdout, out.stderr))
}

/// 중단으로 서버 받기를 끊었을 때의 오류. Outcome::stop 이 이 글을 보면 중단 안내로 바꾼다.
/// 끊긴 시점에 따라 원격 추적 참조 일부가 이미 바뀌었을 수 있다. 다음 최신화가 서버 기준으로 다시 맞춘다.
pub const CANCELLED: &str = "중단 요청으로 서버 받기를 멈췄습니다. 받던 참조 일부가 이미 바뀌었을 수 있고, 다음 서버 최신화 때 다시 맞춰집니다.";

thread_local! {
    static CANCEL: std::cell::RefCell<Option<Arc<AtomicBool>>> = const { std::cell::RefCell::new(None) };
}

/// 이 스레드에서 부르는 서버 받기(origin fetch·ls-remote)를 이 플래그가 서면 바로 끊는다.
pub fn set_thread_cancel(flag: Option<Arc<AtomicBool>>) {
    CANCEL.with(|c| *c.borrow_mut() = flag);
}

fn thread_cancel() -> Option<Arc<AtomicBool>> {
    CANCEL.with(|c| c.borrow().clone())
}

/// 서버 받기를 돌리다 중단 플래그가 서면 프로세스 묶음째 끝내고 Err(CANCELLED) 를 돌려준다.
/// Windows 에서는 git.exe 만 끝내면 git-remote-https 가 남아 연결을 계속 기다리므로 taskkill /T 로 함께 끝낸다.
/// 끊기 직전에 git 이 이미 정상으로 끝났으면 중단이 아니라 그 결과를 돌려준다.
fn run_cancellable(mut cmd: Command, flag: &AtomicBool) -> R<(i32, Vec<u8>, Vec<u8>)> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| format!("프로그램을 실행하지 못했습니다: {}", e))?;
    type Reader = std::thread::JoinHandle<std::io::Result<Vec<u8>>>;
    let read = |p: Option<Box<dyn Read + Send>>| -> Reader {
        std::thread::spawn(move || {
            let mut v = Vec::new();
            if let Some(mut p) = p {
                p.read_to_end(&mut v)?;
            }
            Ok(v)
        })
    };
    // 읽기 오류는 잘린 출력을 성공으로 넘기지 않도록 오류로 올린다(run_raw 의 wait_with_output 과 같게).
    let collect = |h: Reader| -> R<Vec<u8>> {
        h.join().map_err(|_| "프로그램 출력을 읽는 중 문제가 생겼습니다.".to_string())?.map_err(|e| format!("프로그램 출력을 읽지 못했습니다: {}", e))
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    loop {
        let st = match child.try_wait() {
            Ok(st) => st,
            Err(e) => {
                kill_tree(&mut child);
                return Err(format!("프로그램 상태를 읽지 못했습니다: {}", e));
            }
        };
        if let Some(st) = st {
            return Ok((st.code().unwrap_or(-1), collect(out)?, collect(err)?));
        }
        if flag.load(Ordering::Relaxed) {
            // 강제 종료는 종료 코드 1이다. 그 밖의 코드(정상 0, git 오류 128 등)면 끊기 전에 스스로 끝난 것이라 그 결과를 그대로 쓴다.
            if let Some(code) = kill_tree(&mut child).and_then(|s| s.code()).filter(|c| *c != 1) {
                return Ok((code, collect(out)?, collect(err)?));
            }
            // 읽는 스레드는 손자 프로세스가 파이프를 쥐고 있으면 안 끝날 수 있어 기다리지 않는다.
            return Err(CANCELLED.into());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// 프로세스 묶음을 끝내고 git 의 종료 상태를 돌려준다.
fn kill_tree(child: &mut Child) -> Option<std::process::ExitStatus> {
    if cfg!(windows) {
        let mut k = Command::new("taskkill");
        k.args(["/T", "/F", "/PID", &child.id().to_string()]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        no_window(&mut k);
        match k.status() {
            Ok(s) if s.success() => {}
            // 128: 이미 끝나 찾을 수 없음. 그 밖에는 하위 프로세스가 남았을 수 있어 기록한다.
            Ok(s) if s.code() == Some(128) => {}
            Ok(s) => crate::log::say(&format!("    taskkill 이 프로세스 묶음을 다 끝내지 못했습니다(종료 코드 {:?}).", s.code())),
            Err(e) => crate::log::say(&format!("    taskkill 을 실행하지 못했습니다: {}", e)),
        }
    }
    let _ = child.kill();
    child.wait().ok()
}

fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    // "git version 2.45.1.windows.1"
    let v = text.split_whitespace().nth(2)?;
    let mut it = v.split('.').map(|x| x.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<u32>().ok());
    Some((it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0)))
}

/// 실제로 부를 git 의 버전을 처음 한 번 확인한다. 못 쓰면 한 줄 이유를 돌려준다.
pub fn ensure_git(loc: &Loc) -> R<()> {
    if let Some(r) = with_cache(|c| c.checked.get(loc).cloned()) {
        return r;
    }
    let r = check_git(loc);
    with_cache(|c| c.checked.insert(loc.clone(), r.clone()));
    r
}

fn check_git(loc: &Loc) -> R<()> {
    let (who, res) = match loc {
        Loc::Win => {
            let exe = windows_git()?;
            let mut c = Command::new(&exe);
            c.arg("--version");
            no_window(&mut c);
            (format!("Windows Git({})", exe.display()), run_raw(c, None)?)
        }
        Loc::Wsl(d) => {
            let c = wsl_cmd(d, &["git", "--version"])?;
            let res = run_raw(c, None)?;
            if res.0 == 127 {
                return Err(format!("WSL 배포판 {} 안에 Git 이 없습니다. 그 배포판에 Git {} 이상을 설치해 주세요.", d, MIN_GIT_TEXT));
            }
            if res.0 != 0 && parse_version(&decode(&res.1)).is_none() {
                let why = decode(&res.2);
                let why = if why.is_empty() { decode(&res.1) } else { why };
                return Err(format!("WSL 배포판 {} 을 실행하지 못했습니다. 배포판 이름과 WSL 상태를 확인해 주세요. ({})", d, why.lines().next().unwrap_or("").trim()));
            }
            (format!("WSL 배포판 {} 의 Git", d), res)
        }
    };
    let text = decode(&res.1);
    match parse_version(&text) {
        Some(v) if v >= MIN_GIT => Ok(()),
        Some(v) => Err(format!("{} 이 {}.{}.{} 라 쓸 수 없습니다. {} 이상이 필요합니다 ({}).", who, v.0, v.1, v.2, MIN_GIT_TEXT, MIN_GIT_WHY)),
        None => Err(format!("{} 의 버전을 확인하지 못했습니다. {} 이상이 필요합니다. ({})", who, MIN_GIT_TEXT, if text.is_empty() { decode(&res.2) } else { text })),
    }
}

fn linux_path(distro: &str, path: &str) -> R<String> {
    if let Some((d, p)) = wsl_location(path) {
        if !d.eq_ignore_ascii_case(distro) {
            return Err("서로 다른 WSL 배포판의 경로를 한 Git 작업에서 사용할 수 없습니다.".into());
        }
        return Ok(p);
    }
    let key = (distro.to_string(), path.to_string());
    if let Some(v) = with_cache(|c| c.to_linux.get(&key).cloned()) {
        return Ok(v);
    }
    let (code, out, err) = run_raw(wsl_cmd(distro, &["wslpath", "-a", "-u", path])?, None)?;
    let out = decode(&out);
    if code != 0 || out.trim().is_empty() {
        return Err(format!("WSL 에서 파일 경로를 변환하지 못했습니다: {}\n{}", path, decode(&err)));
    }
    with_cache(|c| c.to_linux.insert(key, out.clone()));
    Ok(out)
}

fn windows_path(distro: &str, path: &str) -> R<String> {
    if !path.starts_with('/') {
        return Ok(path.to_string());
    }
    let key = (distro.to_string(), path.to_string());
    if let Some(v) = with_cache(|c| c.to_win.get(&key).cloned()) {
        return Ok(v);
    }
    let (code, out, _) = run_raw(wsl_cmd(distro, &["wslpath", "-w", path])?, None)?;
    let out = decode(&out);
    if code != 0 || out.trim().is_empty() {
        return Err(format!("WSL 의 Git 작업 폴더를 Windows 경로로 변환하지 못했습니다: {}", path));
    }
    with_cache(|c| c.to_win.insert(key, out.clone()));
    Ok(out)
}

/// 한 저장소(또는 저장소 없는 호출)의 git 실행기.
#[derive(Clone, Debug)]
pub struct Git {
    pub repo: Option<String>,
    pub loc: Loc,
}

impl Git {
    /// 저장소 경로로 Git 을 고르고 버전을 확인한다.
    pub fn open(repo: &str) -> R<Git> {
        let loc = loc_of(repo);
        ensure_git(&loc)?;
        Ok(Git { repo: Some(repo.to_string()), loc })
    }

    /// 저장소 없이 부른다(번들 읽기 등). 인자 가운데 WSL 경로가 있으면 그 배포판 Git 을 쓴다.
    /// Windows Git 이 없으면 Git 이 깔린 WSL 배포판으로 대신 부른다(드라이브 경로 인자는 exec 가 바꾼다).
    pub fn bare_for(args: &[&str]) -> R<Git> {
        let mut loc = args.iter().find_map(|a| wsl_location(a)).map(|(d, _)| Loc::Wsl(d)).unwrap_or(Loc::Win);
        if loc == Loc::Win && windows_git().is_err() {
            if let Some(d) = wsl_fallback() {
                loc = Loc::Wsl(d);
            }
        }
        ensure_git(&loc)?;
        Ok(Git { repo: None, loc })
    }

    pub fn run(&self, args: &[&str]) -> R<GitRes> {
        self.exec(args, None, false)
    }
    pub fn q(&self, args: &[&str]) -> R<GitRes> {
        self.exec(args, None, true)
    }
    pub fn input(&self, args: &[&str], input: &str, quiet: bool) -> R<GitRes> {
        self.exec(args, Some(input), quiet)
    }
    /// 성공하면 표준 출력, 아니면 msg + 오류 내용.
    pub fn out(&self, args: &[&str], msg: &str) -> R<String> {
        let r = self.q(args)?;
        if r.code != 0 {
            return Err(format!("{}\n{}", msg, r.detail()));
        }
        Ok(r.out)
    }

    fn exec(&self, args: &[&str], input: Option<&str>, quiet: bool) -> R<GitRes> {
        let mut full: Vec<String> = ["-c", "core.quotepath=false", "-c", "i18n.logOutputEncoding=UTF-8", "-c", "color.ui=false"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let cmd = match &self.loc {
            Loc::Win => {
                if let Some(repo) = &self.repo {
                    full.push("-C".into());
                    full.push(repo.clone());
                }
                full.extend(args.iter().map(|s| s.to_string()));
                let mut c = Command::new(windows_git()?);
                c.args(&full).env("GIT_PAGER", "cat").env("GIT_TERMINAL_PROMPT", "0").env("LC_ALL", "C.UTF-8");
                no_window(&mut c);
                c
            }
            Loc::Wsl(d) => {
                if args.iter().any(|a| (a.starts_with("\\\\") || a.starts_with("//")) && wsl_location(a).is_none()) {
                    return Err("WSL Git 에서는 일반 네트워크 공유 경로를 직접 사용할 수 없습니다. 번들 또는 출력 폴더를 Windows 로컬 드라이브나 같은 WSL 배포판에 두세요.".into());
                }
                if let Some(repo) = &self.repo {
                    full.push("-C".into());
                    full.push(linux_path(d, repo)?);
                }
                for a in args {
                    if wsl_location(a).is_some() || is_drive_path(a) {
                        full.push(linux_path(d, a)?);
                    } else {
                        full.push(a.to_string());
                    }
                }
                let mut pre: Vec<String> = ["env", "GIT_PAGER=cat", "GIT_TERMINAL_PROMPT=0", "LC_ALL=C.UTF-8", "git"].iter().map(|s| s.to_string()).collect();
                pre.extend(full);
                let refs: Vec<&str> = pre.iter().map(|s| s.as_str()).collect();
                wsl_cmd(d, &refs)?
            }
        };
        // 서버에서 받기만 하는 명령은 중단하면 바로 끊는다. WSL 은 wsl.exe 를 끝내도 안쪽 git 이 남을 수 있어 끊지 않는다.
        let serverbound = self.loc == Loc::Win && input.is_none() && args.contains(&"origin") && args.iter().any(|a| *a == "fetch" || *a == "ls-remote");
        let flag = if serverbound { thread_cancel() } else { None };
        let (code, out, err) = match flag {
            Some(f) => {
                if f.load(Ordering::Relaxed) {
                    crate::log::say(&format!("> git {}   [중단 요청으로 실행 안 함]", args.join(" ")));
                    return Err(CANCELLED.into());
                }
                match run_cancellable(cmd, &f) {
                    Err(e) if e == CANCELLED => {
                        crate::log::say(&format!("> git {}   [중단으로 끊음]", args.join(" ")));
                        return Err(e);
                    }
                    other => other?,
                }
            }
            None => run_raw(cmd, input.map(|s| s.as_bytes()))?,
        };
        let mut r = GitRes { code, out: decode(&out), err: decode(&err) };
        if let Loc::Wsl(d) = &self.loc {
            if r.code == 127 {
                return Err(format!("WSL 배포판 {} 에서 Linux Git 을 실행하지 못했습니다. 그 배포판에 Git 이 설치돼 있는지 확인해 주세요.\n{}", d, r.err));
            }
            if r.code == 0 {
                let first = args.first().copied().unwrap_or("");
                if first == "rev-parse" && args.iter().any(|a| matches!(*a, "--show-toplevel" | "--absolute-git-dir" | "--git-common-dir" | "--git-dir")) {
                    let mut v = Vec::new();
                    for line in r.out.lines() {
                        v.push(windows_path(d, line)?);
                    }
                    r.out = v.join("\n");
                } else if first == "worktree" && args.get(1) == Some(&"list") && args.contains(&"--porcelain") {
                    let mut v = Vec::new();
                    for line in r.out.lines() {
                        match line.strip_prefix("worktree ") {
                            Some(p) => v.push(format!("worktree {}", windows_path(d, p)?)),
                            None => v.push(line.to_string()),
                        }
                    }
                    r.out = v.join("\n");
                }
            }
        }
        if !quiet {
            let backend = match &self.loc {
                Loc::Wsl(d) => format!(" [WSL: {}]", d),
                Loc::Win => String::new(),
            };
            crate::log::say(&format!("> git{} {}   [종료 코드 {}]", backend, args.join(" "), r.code));
            for t in [&r.out, &r.err] {
                for line in t.lines() {
                    if !t.is_empty() {
                        crate::log::say(&format!("    {}", line));
                    }
                }
            }
        }
        Ok(r)
    }

    /// update-ref --stdin 트랜잭션. 줄 끝에 줄바꿈을 붙인다.
    pub fn update_refs(&self, lines: &[String], quiet: bool) -> R<GitRes> {
        let mut text = lines.join("\n");
        text.push('\n');
        self.input(&["update-ref", "--stdin"], &text, quiet)
    }
}

/// 번들 생성 잠금. 놓으면(Drop) 풀린다.
pub enum ExportLock {
    File(std::fs::File),
    Child(Child),
}

impl Drop for ExportLock {
    fn drop(&mut self) {
        match self {
            ExportLock::File(f) => {
                let _ = f.unlock();
            }
            ExportLock::Child(c) => {
                drop(c.stdin.take());
                let start = std::time::Instant::now();
                loop {
                    match c.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) if start.elapsed().as_secs() < 10 => std::thread::sleep(std::time::Duration::from_millis(50)),
                        _ => {
                            let _ = c.kill();
                            let _ = c.wait();
                            break;
                        }
                    }
                }
            }
        }
    }
}

/// 같은 저장소의 번들 생성이 공유 임시 참조를 동시에 바꾸지 못하게 한다.
/// 1.0.5 와 같은 파일(gitbridge-export.lock)을 쓴다.
pub fn acquire_export_lock(git: &Git) -> R<ExportLock> {
    let common = git.q(&["rev-parse", "--git-common-dir"])?;
    if common.code != 0 || common.out.trim().is_empty() {
        return Err("저장소의 공통 Git 폴더를 확인하지 못했습니다.".into());
    }
    let repo = git.repo.clone().unwrap_or_default();
    if let Loc::Wsl(d) = &git.loc {
        let mut lc = common.out.trim().to_string();
        if wsl_location(&lc).is_some() || is_drive_path(&lc) {
            lc = linux_path(d, &lc)?;
        }
        if !lc.starts_with('/') {
            let base = wsl_location(&repo).map(|x| x.1).unwrap_or_else(|| "/".into());
            lc = format!("{}/{}", base.trim_end_matches('/'), lc);
        }
        let lock = format!("{}/gitbridge-export.lock", lc.trim_end_matches('/'));
        let mut c = wsl_cmd(d, &["flock", "--nonblock", &lock, "sh", "-c", "printf 'locked\\n'; cat >/dev/null"])?;
        c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().map_err(|e| format!("WSL 잠금 프로세스를 실행하지 못했습니다: {}", e))?;
        let mut line = String::new();
        if let Some(out) = child.stdout.take() {
            let _ = BufReader::new(out).read_line(&mut line);
        }
        if line.trim() != "locked" {
            let mut err = Vec::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_end(&mut err);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("WSL 저장소의 동시 생성 잠금을 얻지 못했습니다. 다른 생성 작업과 flock 설치를 확인해 주세요.\n{}", decode(&err)));
        }
        return Ok(ExportLock::Child(child));
    }
    let c = common.out.trim();
    let dir = if Path::new(c).is_absolute() { PathBuf::from(c) } else { Path::new(&repo).join(c) };
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(dir.join("gitbridge-export.lock"))
        .map_err(|e| format!("잠금 파일을 열지 못했습니다: {}", e))?;
    if f.metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        let _ = (&f).write_all(b"\0");
    }
    match f.try_lock() {
        Ok(()) => Ok(ExportLock::File(f)),
        Err(_) => Err("같은 저장소에서 다른 Git Bridge가 번들을 만드는 중입니다. 끝난 뒤 다시 실행해 주세요.".into()),
    }
}
