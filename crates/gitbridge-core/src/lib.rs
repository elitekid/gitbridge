//! GitBridge 2.0.0 엔진. 1.0.5 의 번들 만들기·적용을 옮기고 증분·되돌리기를 더했다.

pub mod apply;
pub mod archive;
pub mod create;
pub mod export;
pub mod gitrun;
pub mod log;
pub mod manifest;
pub mod queue;
pub mod refresh;
pub mod refs;
pub mod restore;
pub mod snapshot;
pub mod store;
pub mod sync;
pub mod time;
pub mod undo;

/// 버전은 작업공간 Cargo.toml 의 version 하나만 고친다.
pub const TOOL: &str = concat!("GitBridge ", env!("CARGO_PKG_VERSION"));

pub type R<T> = Result<T, String>;
