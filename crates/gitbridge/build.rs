//! Windows 실행 파일에 아이콘을 넣는다(1.0.5 와 같은 git-bridge.ico).
//! 새 크레이트 없이 Windows SDK 의 rc.exe 로 .res 를 만들어 링크한다. rc.exe 가 없으면 경고만 내고 아이콘 없이 빌드한다.

use std::path::{Path, PathBuf};
use std::process::Command;

fn find_rc() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RC") {
        if Path::new(&p).is_file() {
            return Some(PathBuf::from(p));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&path) {
            let c = d.join("rc.exe");
            if c.is_file() {
                return Some(c);
            }
        }
    }
    let pf = std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".to_string());
    let bin = Path::new(&pf).join("Windows Kits").join("10").join("bin");
    let mut vers: Vec<PathBuf> = std::fs::read_dir(&bin).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    vers.sort();
    for v in vers.iter().rev() {
        let c = v.join("x64").join("rc.exe");
        if c.is_file() {
            return Some(c);
        }
    }
    let c = bin.join("x64").join("rc.exe");
    c.is_file().then_some(c)
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/git-bridge.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe(Windows SDK)를 찾지 못해 실행 파일 아이콘을 넣지 않고 빌드한다.");
        return;
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc_file = out.join("icon.rc");
    let ico = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets").join("git-bridge.ico");
    let ico_s = ico.to_string_lossy().replace('\\', "/");
    if std::fs::write(&rc_file, format!("1 ICON \"{}\"\r\n", ico_s)).is_err() {
        println!("cargo:warning=아이콘 리소스 파일을 쓰지 못해 아이콘 없이 빌드한다.");
        return;
    }
    let res = out.join("icon.res");
    match Command::new(&rc).arg("/nologo").arg("/fo").arg(&res).arg(&rc_file).status() {
        Ok(s) if s.success() && res.is_file() => println!("cargo:rustc-link-arg-bins={}", res.display()),
        _ => println!("cargo:warning=rc.exe 실행에 실패해 실행 파일 아이콘 없이 빌드한다."),
    }
}
