//! 긴 git 작업을 백그라운드 스레드로 돌리고, 끝나면 화면 스레드에서 결과를 반영한다.
//! 한 번에 작업 하나만 돈다(1.0.5 와 같게 실행 중에는 다른 실행 단추를 잠근다).

use super::AppState;
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// 작업이 끝난 뒤 화면 스레드에서 상태에 반영할 일.
pub type Apply = Box<dyn FnOnce(&mut AppState) + Send>;

/// 작업 스레드가 진행 문구를 올리고 취소 요청을 읽는 곳.
#[derive(Default)]
pub struct Progress {
    stage: Mutex<String>,
    cancel: Arc<AtomicBool>,
}

impl Progress {
    /// 진행 줄 문구를 바꾼다(작업 스레드에서).
    pub fn set(&self, text: impl Into<String>) {
        if let Ok(mut s) = self.stage.lock() {
            *s = text.into();
        }
    }
    pub fn stage(&self) -> String {
        self.stage.lock().map(|s| s.clone()).unwrap_or_default()
    }
    /// 사용자가 취소를 눌렀는지. 여러 건을 차례로 돌릴 때 다음 건 앞에서 확인한다.
    /// 서버 받기(origin fetch·ls-remote)는 작업 스레드에 걸어 둔 같은 플래그로 그 자리에서 끊긴다.
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    /// 엔진 큐에 그대로 넘길 취소 플래그(중단 단추가 세운다).
    pub fn cancel_flag(&self) -> &AtomicBool {
        &*self.cancel
    }
}

pub struct Running {
    #[allow(dead_code)]
    pub label: String,
    pub progress: Arc<Progress>,
    pub started: std::time::Instant,
}

pub struct Jobs {
    tx: Sender<Apply>,
    rx: Receiver<Apply>,
    pub running: Option<Running>,
    ctx: Option<egui::Context>,
}

impl Default for Jobs {
    fn default() -> Self {
        let (tx, rx) = channel();
        Jobs { tx, rx, running: None, ctx: None }
    }
}

impl Jobs {
    pub fn set_ctx(&mut self, ctx: egui::Context) {
        self.ctx = Some(ctx);
    }

    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// 작업을 띄운다. 이미 도는 작업이 있으면 띄우지 않고 거짓을 돌려준다.
    /// work 는 작업 스레드에서 돌고, 돌려준 Apply 는 화면 스레드에서 AppState 에 반영된다.
    pub fn spawn<F>(&mut self, label: &str, work: F) -> bool
    where
        F: FnOnce(&Progress) -> Apply + Send + 'static,
    {
        if self.running.is_some() {
            return false;
        }
        let progress = Arc::new(Progress::default());
        progress.set(label);
        let p = progress.clone();
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let spawned = std::thread::Builder::new().name("gitbridge-job".into()).spawn(move || {
            gitbridge_core::gitrun::set_thread_cancel(Some(p.cancel.clone()));
            // 작업이 패닉해도 끝났다는 신호는 반드시 보낸다. 안 그러면 running 이 영영 남아 실행 단추가 잠긴다.
            let apply: Apply = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&p))) {
                Ok(a) => a,
                Err(e) => {
                    let why = e.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| e.downcast_ref::<String>().cloned()).unwrap_or_else(|| "알 수 없는 오류".to_string());
                    Box::new(move |st: &mut AppState| {
                        st.notice = format!("작업이 예기치 않게 멈췄습니다(실패): {}", why);
                    })
                }
            };
            let _ = tx.send(apply);
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
        if spawned.is_err() {
            return false;
        }
        self.running = Some(Running { label: label.to_string(), progress, started: std::time::Instant::now() });
        true
    }

    /// 끝난 작업의 반영 일을 꺼낸다(화면 스레드에서 매 프레임).
    pub fn poll(&mut self) -> Vec<Apply> {
        let mut out = Vec::new();
        while let Ok(a) = self.rx.try_recv() {
            out.push(a);
        }
        if !out.is_empty() {
            self.running = None;
        }
        out
    }
}
