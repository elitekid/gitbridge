//! 데이터 폴더(9.1): %APPDATA%\GitBridge2 settings.json·history.jsonl, %LOCALAPPDATA%\GitBridge2 logs·temp.

use crate::refs::Outcome;
use crate::R;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub struct Paths {
    pub roaming: PathBuf,
    pub local: PathBuf,
}

const SETTINGS: &str = "settings.json";
const HISTORY: &str = "history.jsonl";
/// 옛 폴더에서 한 번 복사했음을 알리는 표식 파일(새 roaming 폴더 안).
const MARK: &str = ".migrated-from-1.x";

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

#[cfg(windows)]
fn default_dirs() -> R<(PathBuf, PathBuf, Option<PathBuf>)> {
    let roaming = env_path("APPDATA").ok_or("APPDATA 환경변수를 찾을 수 없습니다.")?;
    let local = env_path("LOCALAPPDATA").ok_or("LOCALAPPDATA 환경변수를 찾을 수 없습니다.")?;
    let legacy = roaming.join("GitBridge");
    Ok((roaming.join("GitBridge2"), local.join("GitBridge2"), Some(legacy)))
}

#[cfg(not(windows))]
fn default_dirs() -> R<(PathBuf, PathBuf, Option<PathBuf>)> {
    let home = env_path("HOME").ok_or("HOME 환경변수를 찾을 수 없습니다.")?;
    Ok((
        home.join(".config").join("GitBridge2"),
        home.join(".local").join("share").join("GitBridge2"),
        Some(home.join(".config").join("GitBridge")),
    ))
}

/// 폴더를 정하고 만든다. 처음이면 옛 %APPDATA%\GitBridge 에서 읽기만 해서 복사해 온다(GITBRIDGE2_HOME 이면 옮기지 않음).
pub fn paths() -> R<Paths> {
    let (roaming, local, legacy) = if let Some(home) = env_path("GITBRIDGE2_HOME") {
        (home.join("roaming"), home.join("local"), None)
    } else {
        default_dirs()?
    };
    let fresh = !roaming.join(SETTINGS).exists() && !roaming.join(HISTORY).exists() && !roaming.join(MARK).exists();
    for d in [&roaming, &local, &local.join("logs"), &local.join("temp")] {
        fs::create_dir_all(d).map_err(|e| format!("폴더를 만들 수 없습니다: {} ({})", d.display(), e))?;
    }
    if let (true, Some(old)) = (fresh, legacy) {
        migrate(&old, &roaming);
    }
    Ok(Paths { roaming, local })
}

/// 옛 파일을 읽기만 해서 새 폴더로 복사한다. 실패해도 실행은 계속한다. 옛 파일은 건드리지 않는다.
fn migrate(old: &Path, new: &Path) {
    for name in [SETTINGS, HISTORY] {
        let src = old.join(name);
        let dst = new.join(name);
        if dst.exists() || !src.is_file() {
            continue;
        }
        if let Ok(bytes) = fs::read(&src) {
            let tmp = new.join(format!("{}.tmp", name));
            if fs::write(&tmp, &bytes).is_ok() && fs::rename(&tmp, &dst).is_err() {
                let _ = fs::remove_file(&tmp);
            }
        }
    }
    // 표식: 다음부터는 새 폴더에서 파일이 지워져도 다시 가져오지 않는다.
    let _ = fs::write(new.join(MARK), b"copied from GitBridge 1.x (read-only)\n");
}

pub fn log_file() -> R<PathBuf> {
    let p = paths()?;
    Ok(p.local.join("logs").join(format!("GitBridge2-{}.log", crate::time::now().date())))
}

pub fn temp_dir() -> R<PathBuf> {
    let p = paths()?;
    Ok(p.local.join("temp"))
}

/// 설정을 읽는다. 없거나 깨졌거나 객체가 아니면 빈 객체.
pub fn load_settings() -> R<Value> {
    let p = paths()?.roaming.join(SETTINGS);
    let text = match fs::read_to_string(&p) {
        Ok(t) => t,
        Err(_) => return Ok(Value::Object(Map::new())),
    };
    match serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) {
        Ok(v @ Value::Object(_)) => Ok(v),
        _ => Ok(Value::Object(Map::new())),
    }
}

/// 임시 파일에 쓰고 바꾼다.
pub fn save_settings(v: &Value) -> R<()> {
    let p = paths()?.roaming;
    let dst = p.join(SETTINGS);
    let tmp = p.join(format!("{}.tmp", SETTINGS));
    // 1.0.5 와 같게 4칸 들여쓰기.
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, serde_json::ser::PrettyFormatter::with_indent(b"    "));
    serde::Serialize::serialize(v, &mut ser).map_err(|e| format!("설정을 직렬화할 수 없습니다: {}", e))?;
    let text = String::from_utf8(buf).map_err(|e| format!("설정을 직렬화할 수 없습니다: {}", e))?;
    let res = (|| -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, &dst)
    })();
    res.map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("설정을 저장할 수 없습니다: {} ({})", dst.display(), e)
    })
}

fn file_sha256(path: &str) -> Option<String> {
    let mut f = fs::File::open(path).ok()?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Some(h.finalize().iter().map(|b| format!("{:02x}", b)).collect())
}

/// history.jsonl 에 한 줄 덧붙인다. 키 14개 순서 고정.
pub fn append_history(o: &Outcome, repo: &str, bundle: Option<&str>) -> R<()> {
    append_history_shown(o, repo, bundle, bundle.unwrap_or(""))
}

/// bundle 은 실제 파일(이름·SHA 계산용), shown 은 기록의 bundle 칸에 보일 값.
pub fn append_history_shown(o: &Outcome, repo: &str, bundle: Option<&str>, shown: &str) -> R<()> {
    let bundle_path = bundle.unwrap_or("");
    let name = if bundle_path.is_empty() {
        String::new()
    } else {
        Path::new(bundle_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    let sha = if bundle_path.is_empty() { String::new() } else { file_sha256(bundle_path).unwrap_or_default() };

    // preserve_order 이므로 넣은 순서가 그대로 나간다.
    let mut m = Map::new();
    m.insert("time".into(), json!(crate::time::now().display()));
    m.insert("kind".into(), json!(o.kind));
    m.insert("ok".into(), json!(o.ok));
    m.insert("branch".into(), json!(o.branch.clone().unwrap_or_default()));
    m.insert("message".into(), json!(o.message));
    m.insert("pushNote".into(), json!(o.push_note.clone().unwrap_or_default()));
    m.insert("commits".into(), json!(o.commits));
    m.insert("branchDetails".into(), json!(o.branch_details));
    m.insert("mainAhead".into(), json!(o.main_ahead));
    m.insert("bundle".into(), json!(shown));
    m.insert("bundleName".into(), json!(name));
    m.insert("bundleSha".into(), json!(sha));
    m.insert("repo".into(), json!(repo));
    // 2.0.0 은 원격에 쓰지 않는다 → 푸시 없음.
    m.insert("push".into(), json!(false));

    let mut line = serde_json::to_string(&Value::Object(m)).map_err(|e| format!("기록을 직렬화할 수 없습니다: {}", e))?;
    line.push('\n');
    let p = paths()?.roaming.join(HISTORY);
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
        .map_err(|e| format!("기록 파일을 열 수 없습니다: {} ({})", p.display(), e))?;
    f.write_all(line.as_bytes()).map_err(|e| format!("기록을 쓸 수 없습니다: {}", e))
}

/// 최근 limit 건, 최신이 앞. 깨진 줄은 건너뛴다.
pub fn read_history(limit: usize) -> R<Vec<Value>> {
    let p = paths()?.roaming.join(HISTORY);
    let bytes = match fs::read(&p) {
        Ok(b) => b,
        Err(_) => return Ok(Vec::new()),
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut out: Vec<Value> = Vec::new();
    for line in text.lines().rev() {
        if out.len() >= limit {
            break;
        }
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(line) {
            out.push(v);
        }
    }
    Ok(out)
}
