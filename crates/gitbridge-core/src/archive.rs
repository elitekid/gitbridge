//! 번들 묶음 ZIP(1.0.5 bundle_archive.py). queue.rs 만 쓴다.
//!
//! ZIP은 실행 형식이 아니라 운반 상자다. 루트의 .bundle 파일만 허용하고,
//! 실제 Git 판정은 기존 번들 적용 경로가 각 파일에 그대로 수행한다.
//! 형식: 루트에 .bundle 만, 압축 없음(STORED), 이름은 파일 이름 그대로.

use crate::R;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const MAX_BUNDLES: usize = 500;
pub const MAX_TOTAL_SIZE: u64 = 50 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct BundleInput {
    pub path: String,
    pub archive: String,
    pub temp_dir: String,
}

#[derive(Debug, Default)]
pub struct ExpandedInputs {
    pub bundles: Vec<BundleInput>,
    pub archives: usize,
    /// (입력 경로, 오류 문구)
    pub errors: Vec<(String, String)>,
    pub temp_dirs: Vec<String>,
}

/// ZIP 을 풀어 두는 임시 폴더의 뿌리.
/// 처음 부를 때 한 번, 비정상 종료로 남은 오래된 해제 폴더를 치운다(1.0.5 store.get_import_temp_dir).
pub fn import_temp_dir() -> R<PathBuf> {
    static SWEPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let d = crate::store::temp_dir()?;
    fs::create_dir_all(&d).map_err(|e| format!("폴더를 만들 수 없습니다: {} ({})", d.display(), e))?;
    if !SWEPT.swap(true, std::sync::atomic::Ordering::SeqCst) {
        sweep_old_imports(&d, std::time::Duration::from_secs(24 * 3600));
    }
    Ok(d)
}

/// bundle- 로 시작하는 폴더 가운데 max_age 보다 오래된 것만 지운다. 다른 창이 쓰는 최근 폴더는 두고, 못 지운 것은 다음에 다시 본다.
fn sweep_old_imports(d: &Path, max_age: std::time::Duration) {
    let Ok(entries) = fs::read_dir(d) else { return };
    let now = std::time::SystemTime::now();
    for e in entries.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if !ft.is_dir() || !e.file_name().to_string_lossy().starts_with("bundle-") {
            continue;
        }
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age > max_age);
        if old {
            if let Err(err) = fs::remove_dir_all(e.path()) {
                crate::log::say(&format!("오래된 ZIP 해제 폴더를 지우지 못했습니다: {} ({})", e.path().display(), err));
            }
        }
    }
}

fn abs(p: &str) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| PathBuf::from(p))
}

/// 경로 비교용 키(Windows 는 대소문자 무시).
fn norm_key(p: &Path) -> String {
    let s = p.to_string_lossy().to_string();
    if cfg!(windows) {
        s.replace('/', "\\").to_lowercase()
    } else {
        s
    }
}

fn file_name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

/// 폴더 바로 아래의 .bundle·.zip 파일. 하위 폴더는 뒤지지 않는다. 이름을 대문자로 바꿔 정렬한다.
pub fn list_transfer_files(folder: &str) -> Vec<String> {
    list_files_with(folder, &[".bundle", ".zip"])
}

/// 폴더 바로 아래의 .bundle 파일.
pub fn list_bundle_files(folder: &str) -> Vec<String> {
    list_files_with(folder, &[".bundle"])
}

fn list_files_with(folder: &str, exts: &[&str]) -> Vec<String> {
    let Ok(rd) = fs::read_dir(folder) else { return Vec::new() };
    let mut v: Vec<(String, String)> = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let lower = name.to_lowercase();
        if !e.path().is_file() || !exts.iter().any(|x| lower.ends_with(x)) {
            continue;
        }
        v.push((name.to_uppercase(), e.path().to_string_lossy().to_string()));
    }
    v.sort();
    v.into_iter().map(|x| x.1).collect()
}

fn next_zip_path(out_dir: &Path, stamp: &str, name: &str) -> PathBuf {
    let base = out_dir.join(format!("{}-{}.zip", stamp, name));
    if !base.exists() {
        return base;
    }
    let mut n = 2;
    loop {
        let p = out_dir.join(format!("{}-{}-{}.zip", stamp, name, n));
        if !p.exists() {
            return p;
        }
        n += 1;
    }
}

/// ZIP 만들기 결과. zip 은 만든 ZIP 경로(2개 미만이면 빈 문자열), left 는 ZIP 은 됐지만 지우지 못한 낱개 번들(경로, 까닭).
#[derive(Debug, Default)]
pub struct ZipMade {
    pub zip: String,
    pub left: Vec<(String, String)>,
}

/// 새 번들 2개 이상을 ZIP으로 묶고, 요청하면 검증 뒤 원본 번들을 지운다. 2개 미만이면 zip 이 빈 문자열.
/// ZIP 자체가 실패하면 Err(낱개 번들은 그대로). 원본을 못 지운 것은 실패가 아니라 left 로 알린다.
pub fn create_base_bundle_zip(bundle_paths: &[String], out_dir: &str, name: &str, remove_sources: bool) -> R<ZipMade> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut seen_paths = HashSet::new();
    let mut seen_names = HashSet::new();
    for value in bundle_paths {
        let path = abs(value);
        let key = norm_key(&path);
        let base = file_name_of(&path);
        if seen_paths.contains(&key) {
            continue;
        }
        if !path.is_file() || !base.to_lowercase().ends_with(".bundle") {
            return Err(format!("ZIP에 넣을 번들 파일을 찾지 못했습니다: {}", path.display()));
        }
        if seen_names.contains(&base.to_lowercase()) {
            return Err(format!("ZIP 안에서 번들 파일 이름이 겹칩니다: {}", base));
        }
        seen_paths.insert(key);
        seen_names.insert(base.to_lowercase());
        paths.push(path);
    }
    if paths.len() < 2 {
        return Ok(ZipMade::default());
    }
    fs::create_dir_all(out_dir).map_err(|e| format!("폴더를 만들 수 없습니다: {} ({})", out_dir, e))?;
    let stamp = crate::time::now().file_stem();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(format!("ZIP 이름 구분값이 올바르지 않습니다: {}", name));
    }
    // 이름이 겹쳐 create_new 가 실패하면 다음 이름으로 다시 고른다. 직접 만든 파일만 실패 때 지운다.
    let mut tries = 0;
    let (target, file) = loop {
        let target = next_zip_path(Path::new(out_dir), &stamp, name);
        match fs::OpenOptions::new().write(true).create_new(true).open(&target) {
            Ok(f) => break (target, f),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && tries < 5 => tries += 1,
            Err(e) => return Err(format!("ZIP 파일을 만들지 못했습니다: {} ({})", target.display(), e)),
        }
    };
    // bundle 자체가 이미 압축돼 있으므로 다시 압축하지 않는다.
    let res = write_zip(file, &paths).and_then(|_| verify_zip(&target, &paths));
    if let Err(e) = res {
        let _ = fs::remove_file(&target);
        return Err(e);
    }
    let mut left = Vec::new();
    if remove_sources {
        for p in &paths {
            if let Err(e) = fs::remove_file(p) {
                left.push((p.display().to_string(), e.to_string()));
            }
        }
    }
    Ok(ZipMade { zip: target.to_string_lossy().to_string(), left })
}

fn write_zip(f: fs::File, paths: &[PathBuf]) -> R<()> {
    use zip::write::SimpleFileOptions;
    let mut zw = zip::ZipWriter::new(f);
    for p in paths {
        let size = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let mut opt = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        if size >= 0xFFFF_FFFF {
            opt = opt.large_file(true);
        }
        zw.start_file(file_name_of(p), opt).map_err(|e| format!("ZIP에 쓰지 못했습니다: {}", e))?;
        let mut src = fs::File::open(p).map_err(|e| format!("번들을 읽지 못했습니다: {} ({})", p.display(), e))?;
        io::copy(&mut src, &mut zw).map_err(|e| format!("ZIP에 쓰지 못했습니다: {}", e))?;
    }
    zw.finish().map_err(|e| format!("ZIP을 마무리하지 못했습니다: {}", e))?;
    Ok(())
}

/// 이름 순서가 같고 모든 항목을 끝까지 읽어(CRC 확인) 문제가 없어야 한다.
fn verify_zip(target: &Path, paths: &[PathBuf]) -> R<()> {
    const FAIL: &str = "만든 ZIP의 내부 번들 검증에 실패했습니다.";
    let f = fs::File::open(target).map_err(|_| FAIL.to_string())?;
    let mut za = zip::ZipArchive::new(f).map_err(|_| FAIL.to_string())?;
    if za.len() != paths.len() {
        return Err(FAIL.into());
    }
    for (i, p) in paths.iter().enumerate() {
        let mut e = za.by_index(i).map_err(|_| FAIL.to_string())?;
        if e.name().map_err(|_| FAIL.to_string())? != file_name_of(p) {
            return Err(FAIL.into());
        }
        io::copy(&mut e, &mut io::sink()).map_err(|_| FAIL.to_string())?;
    }
    Ok(())
}

fn safe_remove_import_dir(path: &Path, temp_root: &Path) {
    let root = abs(&temp_root.to_string_lossy());
    let target = abs(&path.to_string_lossy());
    let inside = target.starts_with(&root) && norm_key(&target) != norm_key(&root);
    if inside && target.is_dir() {
        let _ = fs::remove_dir_all(&target);
    }
}

pub fn cleanup_import_dir(path: &str, temp_root: &Path) {
    safe_remove_import_dir(Path::new(path), temp_root);
}

fn make_temp_dir(temp_root: &Path) -> R<PathBuf> {
    for _ in 0..50 {
        let p = temp_root.join(format!("bundle-{}", crate::time::random_hex(4)));
        match fs::create_dir(&p) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("임시 폴더를 만들지 못했습니다: {} ({})", p.display(), e)),
        }
    }
    Err("임시 폴더를 만들지 못했습니다.".into())
}

fn extract_zip(zip_path: &Path, temp_root: &Path) -> R<(Vec<BundleInput>, PathBuf)> {
    let f = fs::File::open(zip_path).map_err(|e| format!("ZIP 파일을 열지 못했습니다: {}", e))?;
    let mut za = zip::ZipArchive::new(f).map_err(|_| "ZIP 파일이 아니거나 깨졌습니다.".to_string())?;
    let made = make_temp_dir(temp_root)?;
    let res = extract_into(&mut za, zip_path, &made);
    if res.is_err() {
        safe_remove_import_dir(&made, temp_root);
    }
    res.map(|v| (v, made))
}

fn extract_into(za: &mut zip::ZipArchive<fs::File>, zip_path: &Path, made: &Path) -> R<Vec<BundleInput>> {
    // (색인, 이름)
    let mut infos: Vec<(usize, String)> = Vec::new();
    let mut total: u64 = 0;
    for i in 0..za.len() {
        let e = za.by_index(i).map_err(|e| format!("ZIP 항목을 읽지 못했습니다: {}", e))?;
        if e.is_dir() {
            continue;
        }
        total = total.saturating_add(e.size());
        infos.push((i, e.name().map_err(|e| format!("ZIP 항목 이름을 읽지 못했습니다: {}", e))?.to_string()));
    }
    if infos.is_empty() {
        return Err("ZIP 안에 번들이 없습니다.".into());
    }
    if infos.len() > MAX_BUNDLES {
        return Err("ZIP 안 파일이 너무 많습니다. Git Bridge가 만든 묶음인지 확인해 주세요.".into());
    }
    if total > MAX_TOTAL_SIZE {
        return Err("ZIP을 풀었을 때 크기가 너무 큽니다. Git Bridge가 만든 묶음인지 확인해 주세요.".into());
    }
    let mut names = HashSet::new();
    for (i, name) in &infos {
        // 우리 형식은 ZIP 루트에 bundle만 둔다. 이 조건 하나로 경로 탈출도 함께 막는다.
        let base_ok = Path::new(name).file_name().map(|n| n.to_string_lossy() == name.as_str()).unwrap_or(false);
        if !base_ok || name.contains('/') || name.contains('\\') || !name.to_lowercase().ends_with(".bundle") {
            return Err(format!("ZIP에는 루트의 .bundle 파일만 들어갈 수 있습니다: {}", name));
        }
        let e = za.by_index(*i).map_err(|e| format!("ZIP 항목을 읽지 못했습니다: {}", e))?;
        if e.encrypted() {
            return Err(format!("암호화된 ZIP은 읽지 않습니다: {}", name));
        }
        if e.unix_mode().map(|m| m & 0o170000 == 0o120000).unwrap_or(false) {
            return Err(format!("바로가기 항목은 읽지 않습니다: {}", name));
        }
        if !names.insert(name.to_lowercase()) {
            return Err(format!("ZIP 안에서 파일 이름이 겹칩니다: {}", name));
        }
    }
    let archive = abs(&zip_path.to_string_lossy()).to_string_lossy().to_string();
    let temp = made.to_string_lossy().to_string();
    let mut out = Vec::new();
    for (i, name) in &infos {
        let dest = made.join(name);
        let mut src = za.by_index(*i).map_err(|e| format!("ZIP 항목을 읽지 못했습니다: {}", e))?;
        let mut dst = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
            .map_err(|e| format!("풀 파일을 만들지 못했습니다: {} ({})", dest.display(), e))?;
        io::copy(&mut src, &mut dst).map_err(|e| format!("ZIP을 풀지 못했습니다: {} ({})", name, e))?;
        out.push(BundleInput { path: dest.to_string_lossy().to_string(), archive: archive.clone(), temp_dir: temp.clone() });
    }
    Ok(out)
}

/// 파일·폴더 입력을 번들 목록으로 펼친다. 잘못된 ZIP은 다른 입력을 막지 않고 오류 목록으로 돌려준다.
pub fn expand_inputs(paths: &[String], temp_root: &Path) -> ExpandedInputs {
    let mut result = ExpandedInputs::default();
    let _ = fs::create_dir_all(temp_root);
    let mut inputs: Vec<PathBuf> = Vec::new();
    let mut seen = HashSet::new();
    for value in paths.iter().filter(|x| !x.is_empty()) {
        let path = abs(value);
        let candidates: Vec<PathBuf> = if path.is_dir() {
            list_transfer_files(&path.to_string_lossy()).into_iter().map(PathBuf::from).collect()
        } else {
            vec![path]
        };
        for item in candidates {
            if seen.insert(norm_key(&item)) {
                inputs.push(item);
            }
        }
    }
    for path in inputs {
        let lower = file_name_of(&path).to_lowercase();
        let ps = path.to_string_lossy().to_string();
        if path.is_file() && lower.ends_with(".bundle") {
            result.bundles.push(BundleInput { path: ps, ..Default::default() });
        } else if path.is_file() && lower.ends_with(".zip") {
            match extract_zip(&path, temp_root) {
                Ok((bundles, made)) => {
                    result.bundles.extend(bundles);
                    result.temp_dirs.push(made.to_string_lossy().to_string());
                    result.archives += 1;
                }
                Err(e) => result.errors.push((ps, e.lines().next().unwrap_or("").to_string())),
            }
        }
    }
    result
}
