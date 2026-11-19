//! 새 버전 알림과 교체. GitHub 릴리스의 update.txt·update.txt.sig 를 curl 로 받아 서명(Ed25519)을 확인하고,
//! 서명된 버전이 지금보다 높을 때만 알린다. 사용자가 누르면 GitBridge.exe 를 받아 SHA-256 을 확인한 뒤 교체하고 다시 시작한다.

use super::theme::Bold;
use super::{theme, widgets, AppState};
use ed25519_dalek::{Signature, VerifyingKey};
use eframe::egui::{self, RichText};
use gitbridge_core::store;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// 릴리스 서명 공개키(Ed25519, 32바이트).
const PUBLIC_KEY: [u8; 32] = [
    0x1b, 0x23, 0x69, 0x07, 0x43, 0xfd, 0x48, 0x41, 0x77, 0x7d, 0x1f, 0x94, 0xab, 0x0c, 0x8e, 0x63, 0x7e, 0x54, 0x43, 0x07, 0x39, 0x5e, 0x89, 0x88, 0x93, 0x33, 0x1e,
    0x1f, 0xbb, 0x22, 0x93, 0x4f,
];

const BASE_URL: &str = "https://github.com/elitekid/gitbridge/releases/latest/download/";

/// 서명이 확인된 update.txt 의 내용.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version: String,
    pub sha256: String,
}

/// "X.Y.Z"(각 부분 10진 숫자)를 숫자 셋으로.
fn parse_version(s: &str) -> Result<(u64, u64, u64), String> {
    let bad = || format!("버전 형식이 올바르지 않습니다: {}", s);
    let mut it = s.split('.');
    let mut next = || -> Result<u64, String> {
        let p = it.next().ok_or_else(bad)?;
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        p.parse::<u64>().map_err(|_| bad())
    };
    let v = (next()?, next()?, next()?);
    if it.next().is_some() {
        return Err(bad());
    }
    Ok(v)
}

/// update.txt 를 엄격히 읽는다. (product, Manifest)
fn parse_manifest(txt: &[u8]) -> Result<(String, Manifest), String> {
    let text = std::str::from_utf8(txt).map_err(|_| "update.txt 가 UTF-8 이 아닙니다.".to_string())?;
    if text.contains('\r') {
        return Err("update.txt 에 줄바꿈(CR) 문자가 있습니다.".into());
    }
    let mut map: HashMap<&str, &str> = HashMap::new();
    for line in text.split('\n') {
        if line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err("update.txt 에 '=' 없는 줄이 있습니다.".into());
        };
        if map.insert(k, v).is_some() {
            return Err(format!("update.txt 에 같은 키가 두 번 있습니다: {}", k));
        }
    }
    let need = |k: &str| map.get(k).copied().ok_or_else(|| format!("update.txt 에 {} 가 없습니다.", k));
    let product = need("product")?.to_string();
    let version = need("version")?;
    parse_version(version)?;
    let sha = need("sha256")?;
    if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err("update.txt 의 sha256 형식이 올바르지 않습니다.".into());
    }
    Ok((product, Manifest { version: version.to_string(), sha256: sha.to_string() }))
}

/// 서명·형식·product 를 확인하고, 서명된 버전이 current 보다 높을 때만 Some.
pub fn check_manifest(txt: &[u8], sig: &[u8], current: &str) -> Result<Option<Manifest>, String> {
    let sig: [u8; 64] = sig.try_into().map_err(|_| "서명 파일의 길이가 올바르지 않습니다.".to_string())?;
    let vk = VerifyingKey::from_bytes(&PUBLIC_KEY).map_err(|_| "내장된 공개키를 읽지 못했습니다.".to_string())?;
    let signature = Signature::from_bytes(&sig);
    vk.verify_strict(txt, &signature).map_err(|_| "서명이 맞지 않아 믿을 수 없습니다.".to_string())?;
    let (product, m) = parse_manifest(txt)?;
    if product != "GitBridge" {
        return Err("다른 제품의 업데이트 정보입니다.".into());
    }
    let newer = parse_version(&m.version)?;
    let now = parse_version(current)?;
    Ok(if newer > now { Some(m) } else { None })
}

fn with_suffix(exe: &Path, suffix: &str) -> PathBuf {
    let mut s = exe.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}
/// 다른 프로세스가 못 여는 파일 핸들을 얻는다(Windows). 프로세스가 죽으면 핸들이 풀린다.
fn lock_exclusive(path: &Path) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true).write(true).create(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(0);
    }
    o.open(path)
}

/// 이 기능이 만드는 임시 폴더 이름의 앞부분.
const WORK_PREFIX: &str = "gitbridge-update-";

/// 임시 폴더 안에서 쓰는 중임을 알리는 잠금 파일 이름.
const IN_USE: &str = "in-use";

/// 시작할 때 지난 교체가 남긴 <exe>.old, <exe>.new 와 하루 넘게 남은 임시 폴더를 지운다. 실패는 무시한다.
pub fn cleanup_leftovers() {
    if let Ok(exe) = std::env::current_exe() {
        let lock_path = with_suffix(&exe, ".update-lock");
        if let Ok(lock) = lock_exclusive(&lock_path) {
            let _ = std::fs::remove_file(with_suffix(&exe, ".old"));
            let _ = std::fs::remove_file(with_suffix(&exe, ".new"));
            drop(lock);
            let _ = std::fs::remove_file(&lock_path);
        }
    }
    let day = std::time::Duration::from_secs(24 * 60 * 60);
    for base in work_bases() {
        let Ok(rd) = std::fs::read_dir(&base) else { continue };
        for ent in rd.flatten() {
            if !ent.file_name().to_string_lossy().starts_with(WORK_PREFIX) {
                continue;
            }
            let old = ent.metadata().ok().and_then(|md| md.modified().ok()).and_then(|t| t.elapsed().ok()).map(|age| age > day).unwrap_or(false);
            if !old {
                continue;
            }
            let flag = ent.path().join(IN_USE);
            // 쓰는 실행이 있으면 잠금을 열 수 없다. 파일이 아예 없으면 나이 조건만 본다.
            let free = if flag.exists() { lock_exclusive(&flag).map(drop).is_ok() } else { true };
            if free {
                let _ = std::fs::remove_dir_all(ent.path());
            }
        }
    }
}

/// settings.json 의 updateAutoCheck(없으면 거짓).
pub fn auto_check(settings: &Value) -> bool {
    settings.get("updateAutoCheck").and_then(|v| v.as_bool()).unwrap_or(false)
}

fn work_bases() -> Vec<PathBuf> {
    let mut bases = Vec::new();
    if let Ok(b) = store::temp_dir() {
        bases.push(b);
    }
    bases.push(std::env::temp_dir());
    bases
}

/// 이번 실행 전용 임시 폴더. 안의 in-use 파일을 쥐고 있어 정리가 지우지 않는다. 버려지면 지운다.
struct WorkDir {
    path: PathBuf,
    lock: Option<File>,
}

impl WorkDir {
    fn create() -> Result<WorkDir, String> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let name = format!("{}{}-{}", WORK_PREFIX, std::process::id(), nanos);
        let mut last = String::new();
        for b in work_bases() {
            let dir = b.join(&name);
            match std::fs::create_dir_all(&dir) {
                Ok(()) => match lock_exclusive(&dir.join(IN_USE)) {
                    Ok(f) => return Ok(WorkDir { path: dir, lock: Some(f) }),
                    Err(e) => {
                        let _ = std::fs::remove_dir_all(&dir);
                        last = e.to_string();
                    }
                },
                Err(e) => last = e.to_string(),
            }
        }
        Err(format!("임시 폴더를 만들지 못했습니다: {}", last))
    }
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        drop(self.lock.take());
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// curl.exe 를 띄우는 유일한 곳. 끄기는 같은 잠금으로 false 를 쓰므로, 끄기가 끝난 뒤에는 새 curl 이 뜨지 않는다.
fn run_curl(gate: &Mutex<bool>, file: &str, out: &Path, max_time: u32, revoke_opt: bool) -> Result<std::process::Output, String> {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let exe = Path::new(&root).join("System32").join("curl.exe");
    let mut cmd = Command::new(exe);
    cmd.args(["-fsSL", "--proto", "=https", "--proto-redir", "=https"]);
    if revoke_opt {
        cmd.arg("--ssl-revoke-best-effort");
    }
    cmd.args(["--connect-timeout", "10", "--max-time"])
        .arg(max_time.to_string())
        .arg("-o")
        .arg(out)
        .arg(format!("{}{}", BASE_URL, file))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let child = {
        let g = gate.lock().unwrap_or_else(|p| p.into_inner());
        if !*g {
            return Err("새 버전 자동 확인이 꺼져 있어 접속하지 않았습니다.".to_string());
        }
        cmd.spawn().map_err(|e| format!("curl 을 실행하지 못했습니다: {}", e))?
    };
    child.wait_with_output().map_err(|e| format!("curl 을 실행하지 못했습니다: {}", e))
}

/// curl 로 한 파일을 받는다. 실패 사유에는 curl 의 오류 줄을 짧게 담는다.
/// 오래된 curl 이 --ssl-revoke-best-effort 를 모르면(종료 코드 2) 그 옵션 없이 한 번 더 받는다.
fn curl(gate: &Mutex<bool>, file: &str, out: &Path, max_time: u32) -> Result<(), String> {
    let mut o = run_curl(gate, file, out, max_time, true)?;
    if o.status.code() == Some(2) {
        o = run_curl(gate, file, out, max_time, false)?;
    }
    if o.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&o.stderr);
    let short: String = err.trim().chars().take(200).collect();
    Err(format!("{} 를 받지 못했습니다: {}", file, if short.is_empty() { "알 수 없는 오류".to_string() } else { short }))
}

fn write_hint(exe: &Path, e: &std::io::Error) -> String {
    let dir = exe.parent().map(|p| p.display().to_string()).unwrap_or_default();
    format!("이 폴더에 쓸 수 없어 바꾸지 못했습니다: {}. 쓰기 권한이 있는 곳에 두거나 새 exe 를 직접 받아 바꿔 주세요. ({})", dir, e)
}

/// 새 버전 확인(스레드에서 부른다).
fn fetch_check(gate: &Mutex<bool>) -> Result<Option<Manifest>, String> {
    let work = WorkDir::create()?;
    let (t, s) = (work.path().join("update.txt"), work.path().join("update.txt.sig"));
    curl(gate, "update.txt", &t, 30)?;
    curl(gate, "update.txt.sig", &s, 30)?;
    let txt = std::fs::read(&t).map_err(|e| format!("받은 update.txt 를 읽지 못했습니다: {}", e))?;
    let sig = std::fs::read(&s).map_err(|e| format!("받은 서명 파일을 읽지 못했습니다: {}", e))?;
    check_manifest(&txt, &sig, env!("CARGO_PKG_VERSION"))
}

/// 받아서 SHA-256 을 확인한 새 exe. 이 실행 전용 임시 폴더에 두고, 버려지면 폴더째 지운다.
pub struct Prepared {
    work: WorkDir,
}

impl Prepared {
    fn file(&self) -> PathBuf {
        self.work.path().join("GitBridge.exe")
    }
}

fn sha_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// 새 exe 를 이 실행 전용 임시 폴더에 받아 SHA-256 을 확인한다(스레드에서 부른다).
fn prepare(gate: &Mutex<bool>, m: &Manifest) -> Result<Prepared, String> {
    let p = Prepared { work: WorkDir::create()? };
    let got = p.file();
    curl(gate, "GitBridge.exe", &got, 300)?;
    let bytes = std::fs::read(&got).map_err(|e| format!("받은 파일을 읽지 못했습니다: {}", e))?;
    if sha_hex(&bytes) != m.sha256 {
        return Err("받은 파일의 SHA-256 이 서명된 값과 달라 쓰지 않고 지웠습니다. 새 릴리스가 올라왔을 수 있으니 업데이트 탭의 [지금 확인]으로 다시 확인해 주세요.".into());
    }
    Ok(p)
}

/// 교체 실패. Retry 는 원래 상태로 돌아왔고(받아 둔 파일은 그대로 쓴다), Redownload 는 받아 둔 파일을 못 써서 새로 받아야 하며,
/// Stuck 은 되돌리지 못해 사용자가 손봐야 한다.
enum SwapFail {
    Retry(String),
    Redownload(String),
    Stuck(String),
}

/// 실행 파일 단위 잠금을 쥔 채 교체한다. 되돌리기까지 모두 끝나면 잠금을 풀고 잠금 파일을 지워 본다.
fn replace_and_start(p: &Prepared, m: &Manifest) -> Result<(), SwapFail> {
    let exe = std::env::current_exe().map_err(|e| SwapFail::Retry(format!("실행 파일 위치를 알 수 없습니다: {}", e)))?;
    let lock_path = with_suffix(&exe, ".update-lock");
    let lock = lock_exclusive(&lock_path)
        .map_err(|e| SwapFail::Retry(format!("다른 GitBridge 가 바꾸는 중이거나 이 폴더에 쓸 수 없습니다. 잠시 뒤 다시 눌러 주세요. ({})", e)))?;
    let r = swap_files(&exe, p, m);
    drop(lock);
    let _ = std::fs::remove_file(&lock_path);
    r
}

/// 받아 둔 새 exe 를 다시 확인해 <exe>.new 로 쓰고 바로 바꾼 뒤 새 exe 를 띄운다(화면 스레드, 잠금을 쥔 채).
/// 실패하면 되돌리고, 되돌리기마저 실패하면 남은 파일 위치를 알린다.
fn swap_files(exe: &Path, p: &Prepared, m: &Manifest) -> Result<(), SwapFail> {
    let (new, old) = (with_suffix(exe, ".new"), with_suffix(exe, ".old"));
    let bytes = std::fs::read(p.file()).map_err(|e| SwapFail::Redownload(format!("받아 둔 새 파일을 읽지 못했습니다. 다시 눌러 새로 받아 주세요. ({})", e)))?;
    if sha_hex(&bytes) != m.sha256 {
        return Err(SwapFail::Redownload("받아 둔 새 파일이 바뀌어 쓰지 않았습니다. 다시 눌러 새로 받아 주세요.".into()));
    }
    if new.exists() {
        std::fs::remove_file(&new).map_err(|e| SwapFail::Retry(write_hint(&exe, &e)))?;
    }
    if let Err(e) = std::fs::write(&new, &bytes) {
        let _ = std::fs::remove_file(&new);
        return Err(SwapFail::Retry(write_hint(&exe, &e)));
    }
    if old.exists() {
        let _ = std::fs::remove_file(&old);
    }
    if let Err(e) = std::fs::rename(&exe, &old) {
        let _ = std::fs::remove_file(&new);
        return Err(SwapFail::Retry(write_hint(&exe, &e)));
    }
    if let Err(e) = std::fs::rename(&new, &exe) {
        return match std::fs::rename(&old, &exe) {
            Ok(()) => {
                let _ = std::fs::remove_file(&new);
                Err(SwapFail::Retry(write_hint(&exe, &e)))
            }
            Err(e2) => Err(SwapFail::Stuck(format!(
                "바꾸다 멈췄고 되돌리지도 못했습니다. 원래 파일이 {} 로 남아 있으니 이 프로그램을 닫은 뒤 이름을 {} 로 바꿔 주세요. ({}; {})",
                old.display(),
                exe.display(),
                e,
                e2
            ))),
        };
    }
    if let Err(e) = Command::new(&exe).spawn() {
        if let Err(e2) = std::fs::rename(&exe, &new) {
            return Err(SwapFail::Stuck(format!(
                "새 버전으로 바꿨지만 시작하지 못했고 되돌리지도 못했습니다. 이 프로그램을 닫은 뒤 {} 를 다시 실행해 보고, 안 되면 {} 를 지우고 {} 의 이름을 {} 로 바꿔 주세요. ({}; {})",
                exe.display(),
                exe.display(),
                old.display(),
                exe.display(),
                e,
                e2
            )));
        }
        if let Err(e2) = std::fs::rename(&old, &exe) {
            return Err(SwapFail::Stuck(format!(
                "새 버전을 시작하지 못했고 되돌리지도 못했습니다. 이 프로그램을 닫은 뒤 {} 를 {} 로 바꿔 주세요. ({}; {})",
                old.display(),
                exe.display(),
                e,
                e2
            )));
        }
        let _ = std::fs::remove_file(&new);
        return Err(SwapFail::Retry(format!("새 버전을 시작하지 못해 원래대로 되돌렸습니다: {}", e)));
    }
    Ok(())
}

#[derive(Clone)]
pub enum Phase {
    Idle,
    Checking,
    /// 확인한 결과 지금이 최신(버전 문자열).
    UpToDate(String),
    Available(Manifest),
    Downloading(Manifest),
    /// 새 파일을 받아 두었다. 작업이 끝나면 바꿀 수 있다.
    Ready(Manifest),
    CheckFailed(String),
    ReplaceFailed(Manifest, String),
    /// 교체를 되돌리지 못했다. 사용자가 안내대로 손봐야 하므로 다시 시도하지 않는다.
    Broken(Manifest, String),
    /// 바꾸고 새 exe 를 띄웠다. 이 창은 곧 닫힌다.
    Restarting(Manifest),
}

/// 스레드가 보내는 결과. 첫 값은 보낸 시점의 세대.
enum Msg {
    Checked(u64, Result<Option<Manifest>, String>),
    Prepared(u64, Manifest, Result<Prepared, String>),
}

pub struct UpdateState {
    pub phase: Phase,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    /// 새 파일이 방금 준비되어 교체를 시도할 차례.
    ready_now: bool,
    /// Ready·ReplaceFailed 일 때 받아 둔 새 exe.
    prepared: Option<Prepared>,
    /// 자동 확인이 켜져 있는가. 스레드는 시작할 때의 gate 를 들고 있고, 끄면 gate 를 갈아 끼운다.
    gate: Arc<Mutex<bool>>,
    /// 켜고 끌 때마다 올라가는 세대. 옛 세대의 결과는 버린다.
    gen: u64,
}

impl UpdateState {
    pub fn new(enabled: bool) -> UpdateState {
        let (tx, rx) = channel();
        UpdateState { phase: Phase::Idle, tx, rx, ready_now: false, prepared: None, gate: Arc::new(Mutex::new(enabled)), gen: 0 }
    }

    pub fn enabled(&self) -> bool {
        *self.gate.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 설정에 맞춰 켜고 끈다. 끄면 이미 도는 스레드의 접속도 막고 상태를 비운다.
    pub fn set_enabled(&mut self, on: bool) {
        if self.enabled() == on {
            return;
        }
        if on {
            *self.gate.lock().unwrap_or_else(|p| p.into_inner()) = true;
        } else {
            // 잠금을 얻어 쓰므로, spawn 중인 스레드가 있으면 그 spawn 이 끝난 뒤에 끝난다.
            {
                let mut g = self.gate.lock().unwrap_or_else(|p| p.into_inner());
                *g = false;
            }
            self.gate = Arc::new(Mutex::new(false));
            self.gen += 1;
            self.prepared = None;
            self.ready_now = false;
            // 창이 닫히는 중이거나 되돌리지 못해 손봐야 하는 상태는 끄고 켜도 지우지 않는다.
            if !matches!(self.phase, Phase::Restarting(_) | Phase::Broken(_, _)) {
                self.phase = Phase::Idle;
            }
        }
    }

    /// 바꾸고 다시 시작해 이 창이 닫히는 중인가.
    pub fn restarting(&self) -> bool {
        matches!(self.phase, Phase::Restarting(_))
    }

    /// [지금 확인]을 받을 수 있는가. 이미 새 버전을 알고 받거나 바꾸는 중이면 받지 않는다.
    pub fn can_check(&self) -> bool {
        self.enabled() && !matches!(self.phase, Phase::Checking | Phase::Available(_) | Phase::Downloading(_) | Phase::Ready(_) | Phase::Broken(_, _) | Phase::Restarting(_))
    }

    fn spawn<F>(&self, ctx: &egui::Context, work: F) -> Result<(), String>
    where
        F: FnOnce() -> Msg + Send + 'static,
    {
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("gitbridge-update".into())
            .spawn(move || {
                let m = work();
                let _ = tx.send(m);
                ctx.request_repaint();
            })
            .map(|_| ())
            .map_err(|e| format!("확인을 시작하지 못했습니다: {}", e))
    }

    /// 새 버전을 확인한다. can_check 가 거짓이면 아무것도 하지 않는다.
    pub fn start_check(&mut self, ctx: &egui::Context) {
        if !self.enabled() || !self.can_check() {
            return;
        }
        self.prepared = None;
        self.phase = Phase::Checking;
        let (gate, gen) = (self.gate.clone(), self.gen);
        if let Err(e) = self.spawn(ctx, move || Msg::Checked(gen, fetch_check(&gate))) {
            self.phase = Phase::CheckFailed(e);
        }
    }

    /// 새 exe 를 받아 준비한다.
    pub fn start_download(&mut self, ctx: &egui::Context) {
        if !self.enabled() {
            return;
        }
        let m = match &self.phase {
            Phase::Available(m) | Phase::ReplaceFailed(m, _) => m.clone(),
            _ => return,
        };
        self.prepared = None;
        self.phase = Phase::Downloading(m.clone());
        let job = m.clone();
        let (gate, gen) = (self.gate.clone(), self.gen);
        if let Err(e) = self.spawn(ctx, move || {
            let r = prepare(&gate, &job);
            Msg::Prepared(gen, job, r)
        }) {
            self.phase = Phase::ReplaceFailed(m, e);
        }
    }

    /// 스레드가 보낸 결과를 받는다(매 프레임).
    pub fn poll(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            let from = match &msg {
                Msg::Checked(g, _) => *g,
                Msg::Prepared(g, _, _) => *g,
            };
            if from != self.gen || !self.enabled() {
                // 끄기 전에 시작한 확인·다운로드의 결과다. 버린다(받아 둔 파일의 임시 폴더도 지워진다).
                continue;
            }
            match msg {
                Msg::Checked(_, Ok(Some(m))) => self.phase = Phase::Available(m),
                Msg::Checked(_, Ok(None)) => self.phase = Phase::UpToDate(env!("CARGO_PKG_VERSION").to_string()),
                Msg::Checked(_, Err(e)) => self.phase = Phase::CheckFailed(e),
                Msg::Prepared(_, m, Ok(p)) => {
                    self.prepared = Some(p);
                    self.phase = Phase::Ready(m);
                    self.ready_now = true;
                }
                Msg::Prepared(_, m, Err(e)) => self.phase = Phase::ReplaceFailed(m, e),
            }
        }
    }

    fn banner_manifest(&self) -> Option<&Manifest> {
        match &self.phase {
            Phase::Available(m) | Phase::Downloading(m) | Phase::Ready(m) | Phase::ReplaceFailed(m, _) | Phase::Broken(m, _) | Phase::Restarting(m) => Some(m),
            _ => None,
        }
    }

    pub fn banner_visible(&self) -> bool {
        self.enabled() && self.banner_manifest().is_some()
    }
}

/// 매 프레임: 설정과 맞추고, 결과를 받고, 방금 준비된 새 파일이 있으면(작업 중이 아닐 때) 바로 교체한다.
pub fn drive(st: &mut AppState, ctx: &egui::Context) {
    let on = auto_check(&st.settings);
    st.upd.set_enabled(on);
    st.upd.poll();
    if st.upd.ready_now {
        st.upd.ready_now = false;
        if !st.busy() {
            swap_now(st, ctx);
        }
    }
}

/// 준비된 새 파일로 바꾸고 다시 시작한다. 작업 중이면 하지 않는다.
/// 성공하면 Restarting 으로 두어 남은 프레임에서 새 작업이 시작되지 않게 한다.
fn swap_now(st: &mut AppState, ctx: &egui::Context) {
    if st.busy() {
        return;
    }
    let m = match &st.upd.phase {
        Phase::Ready(m) | Phase::ReplaceFailed(m, _) => m.clone(),
        _ => return,
    };
    let result = match st.upd.prepared.as_ref() {
        Some(p) => replace_and_start(p, &m),
        None => {
            st.upd.phase = Phase::ReplaceFailed(m, "받아 둔 새 파일이 없습니다. 다시 눌러 새로 받아 주세요.".into());
            return;
        }
    };
    match result {
        Ok(()) => {
            st.upd.phase = Phase::Restarting(m);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        Err(SwapFail::Retry(e)) => st.upd.phase = Phase::ReplaceFailed(m, e),
        Err(SwapFail::Redownload(e)) => {
            st.upd.prepared = None;
            st.upd.phase = Phase::ReplaceFailed(m, e);
        }
        Err(SwapFail::Stuck(e)) => {
            st.upd.prepared = None;
            st.upd.phase = Phase::Broken(m, e);
        }
    }
}

/// 위쪽 배너 한 줄.
pub fn banner(st: &mut AppState, ui: &mut egui::Ui, ctx: &egui::Context) {
    let busy = st.busy();
    let phase = st.upd.phase.clone();
    let mut press = false;
    ui.horizontal_wrapped(|ui| {
        if let Some(m) = st.upd.banner_manifest() {
            ui.label(RichText::new(format!("새 버전 {} 가 있습니다", m.version)).bold().color(theme::ACCENT));
        }
        match &phase {
            Phase::Downloading(_) => {
                ui.spinner();
                ui.label(RichText::new("새 버전을 받는 중…").color(theme::SUB));
            }
            Phase::Restarting(_) => {
                ui.spinner();
                ui.label(RichText::new("다시 시작하는 중…").color(theme::SUB));
            }
            Phase::Available(_) | Phase::Ready(_) => {
                if widgets::tool(ui, "바꾸고 다시 시작", !busy).clicked() {
                    press = true;
                }
                if busy {
                    ui.label(RichText::new("작업이 끝나면 바꿀 수 있습니다").color(theme::WARN));
                }
            }
            Phase::ReplaceFailed(_, why) => {
                if widgets::tool(ui, "바꾸고 다시 시작", !busy).clicked() {
                    press = true;
                }
                if busy {
                    ui.label(RichText::new("작업이 끝나면 바꿀 수 있습니다").color(theme::WARN));
                }
                ui.label(RichText::new(why).color(theme::ERR));
            }
            Phase::Broken(_, why) => {
                ui.label(RichText::new(why).color(theme::ERR));
            }
            _ => {}
        }
    });
    if press {
        match phase {
            Phase::Ready(_) => swap_now(st, ctx),
            Phase::ReplaceFailed(_, _) => {
                if st.upd.prepared.is_some() {
                    swap_now(st, ctx);
                } else {
                    st.upd.start_download(ctx);
                }
            }
            Phase::Available(_) => st.upd.start_download(ctx),
            _ => {}
        }
    }
}
