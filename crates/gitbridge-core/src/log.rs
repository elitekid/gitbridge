//! 실행 로그. 화면(표준 출력)과 로그 파일에 같은 줄을 남긴다.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

struct Sink {
    echo: bool,
    file: Option<File>,
}

static SINK: Mutex<Sink> = Mutex::new(Sink { echo: false, file: None });

/// 화면 출력 여부와 로그 파일을 정한다. 파일을 못 열면 화면에만 쓴다.
pub fn init(echo: bool, file_path: Option<&std::path::Path>) {
    let file = file_path.and_then(|p| OpenOptions::new().create(true).append(true).open(p).ok());
    if let Ok(mut s) = SINK.lock() {
        s.echo = echo;
        s.file = file;
    }
}

pub fn say(text: &str) {
    let line = format!("{}  {}", crate::time::now().hms(), text);
    if let Ok(mut s) = SINK.lock() {
        if s.echo {
            println!("{}", line);
        }
        if let Some(f) = s.file.as_mut() {
            let _ = f.write_all(line.replace('\n', "\r\n").as_bytes());
            let _ = f.write_all(b"\r\n");
        }
    }
}
