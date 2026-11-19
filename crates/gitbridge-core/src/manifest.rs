//! 저장소 번들의 v2 매니페스트(refs/gitbridge/v2/manifest 가 가리키는 blob).
//! heads·tags 에는 증분이어도 그 범위의 전체 상태를 적는다.

use crate::gitrun::Git;
use crate::R;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const FORMAT: &str = "gitbridge-v2";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Manifest {
    pub format: String,
    pub label: String,
    pub default: String,
    pub scope: String,
    pub incremental: bool,
    pub heads: BTreeMap<String, String>,
    pub tags: BTreeMap<String, String>,
    pub created: String,
    pub tool: String,
}

impl Manifest {
    /// blob 으로 저장하고 OID 를 돌려준다.
    pub fn write(&self, git: &Git) -> R<String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let r = git.input(&["hash-object", "-w", "--stdin"], &(text + "\n"), true)?;
        if !r.ok() {
            return Err(format!("번들 목록 파일을 만들지 못했습니다.\n{}", r.detail()));
        }
        Ok(r.out.trim().to_string())
    }

    pub fn read(git: &Git, rev: &str) -> R<Manifest> {
        let r = git.q(&["cat-file", "-p", rev])?;
        if !r.ok() {
            return Err(format!("번들 목록 파일을 읽지 못했습니다.\n{}", r.detail()));
        }
        let m: Manifest = serde_json::from_str(&r.out).map_err(|e| format!("번들 목록 파일 형식이 맞지 않습니다: {}", e))?;
        if m.format != FORMAT {
            return Err(format!("알 수 없는 번들 목록 파일 형식입니다: {}", m.format));
        }
        Ok(m)
    }
}
