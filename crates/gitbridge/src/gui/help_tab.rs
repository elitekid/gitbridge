//! 사용법 탭. 2.0.0 동작(원격에 쓰지 않음, 증분 번들, 되돌리기 기록, 상태 다섯 개)에 맞춘 안내.

use super::common::Status;
use super::{theme, widgets, AppState};
use super::theme::Bold;
use eframe::egui::{self, RichText};
use gitbridge_core::TOOL;

/// 1.0.5 AI_HANDOFF_PROMPT 원문. 1.0.5 전용 이름만 2.0.0 화면 이름([번들 만들기] 탭)으로 고쳤다.
const AI_HANDOFF_PROMPT: &str = "현재 저장소의 변경을 원본 저장소로 전달할 준비를 한다.
- main·master·develop·production 같은 기준 브랜치에는 직접 커밋하지 않고, 현재 작업에 맞는 feature 브랜치를 만들거나 확인한다.
- 변경 파일을 검토하고 관련 검증을 마친 뒤, 의도한 파일만 한국어 관례형 메시지로 커밋한다. 티켓 번호는 커밋 메시지에 넣지 않고 배포 선행조건은 본문에 적는다.
- Git bundle은 만들지 않는다. 커밋이 끝나면 사용자가 GitBridge의 [번들 만들기] 탭에서 만든다.
- 최종 feature 브랜치명과 새 커밋 해시·메시지, 아직 남은 미커밋 파일을 보고한다.
- push, merge, rebase, amend는 하지 않는다. 이미 내보낸 커밋은 고쳐 쓰지 않는다.";

/// '복사됨' 표시를 유지하는 시간.
const COPIED_MS: u64 = 1500;

#[derive(Default)]
pub struct HelpState {
    /// 요청문을 복사한 시각(잠시 '복사됨'을 보이는 데 쓴다).
    copied_at: Option<std::time::Instant>,
}

fn section<F>(ui: &mut egui::Ui, title: &str, open: bool, body: F)
where
    F: FnOnce(&mut egui::Ui),
{
    widgets::card(ui, None, |ui| {
        egui::CollapsingHeader::new(RichText::new(title).bold().size(15.0).color(theme::ACCENT))
            .id_salt(title)
            .default_open(open)
            .show(ui, |ui| {
                body(ui);
                ui.add_space(4.0);
            });
    });
    ui.add_space(8.0);
}

fn status_row(ui: &mut egui::Ui, s: Status, text: &str) {
    ui.horizontal(|ui| {
        s.badge(ui);
        ui.label(text);
    });
}

pub fn ui(st: &mut AppState, ui: &mut egui::Ui) {
    let copied = st.help.copied_at.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(COPIED_MS));
    if copied {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    } else if st.help.copied_at.is_some() {
        st.help.copied_at = None;
    }
    let mut want_copy = false;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        widgets::page_heading(ui, &format!("{} 사용법", TOOL), "서버와 내 PC 사이에서 코드를 번들 파일로 옮기는 도구입니다. 2.0.0 은 서버에 아무것도 쓰지 않습니다. 모든 작업은 내 PC의 저장소와 번들 파일에서만 일어납니다.");

        section(ui, "처음 보기", true, |ui| {
            ui.label("위쪽 탭: 번들 적용, 번들 만들기, 사용법, 업데이트.");
            ui.label("아래쪽 기록 창: 지난 이력, 이번 실행 기록, 되돌리기를 한 곳에 모았습니다.");
            ui.label("맨 아래 줄: 지금 진행 중인 작업과 진행 상황. 작업 중에는 중단을 누를 수 있습니다(서버 조회·받기는 바로 끊고, 진행 중인 로컬 단계는 끝날 때까지 기다린 뒤 멈춥니다).");
        });

        section(ui, "AI에게 feature 커밋까지 맡기기", false, |ui| {
            ui.label("개발이 끝나면 아래 요청문을 복사해 작업한 AI에게 보냅니다. AI는 의도한 변경만 feature에 커밋하고, 번들은 GitBridge가 만듭니다.");
            ui.add_space(6.0);
            egui::Frame::new()
                .fill(theme::BACK)
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(AI_HANDOFF_PROMPT);
                });
            ui.add_space(6.0);
            let label = if copied { "복사됨" } else { "AI 요청문 복사" };
            if widgets::primary(ui, label, true).clicked() {
                want_copy = true;
            }
            ui.add_space(6.0);
            ui.label("AI가 알려준 feature 브랜치명을 확인합니다. 커밋되지 않은 변경이 남아 있으면 먼저 정리합니다.");
        });

        section(ui, "번들 적용 (받는 쪽)", false, |ui| {
            ui.label("1. 저장소 폴더를 고릅니다.");
            ui.label("2. 번들 파일(.bundle 또는 .zip)을 고르거나, 번들 폴더에 모아 둔 목록을 씁니다.");
            ui.label("3. 적용하기 전에 미리보기로 무엇이 들어올지 확인합니다. 미리보기는 저장소를 바꾸지 않습니다.");
            ui.label("4. 적용을 누르면 번들 안의 브랜치가 저장소에 반영됩니다. 서버로 push 하지는 않습니다.");
            ui.label("5. 끝나면 결과가 상태 표시와 함께 나오고 기록 창에 남습니다.");
            ui.label("이미 적용한 번들이나 예전 번들은 '보류'로 표시하고 건너뜁니다.");
        });

        section(ui, "번들 만들기 (보내는 쪽)", false, |ui| {
            ui.label("1. 저장소와 내보낼 곳을 고릅니다.");
            ui.label("2. 종류(기준·표준·전체)와 브랜치를 고릅니다.");
            ui.label("3. 미리보기로 담길 내용을 확인한 뒤 만듭니다.");
            ui.label("기준·표준·전체 범위 번들은 바뀐 부분만 담는 것이 기본입니다. 지난번 보낸 뒤 달라진 것만 담아서 크기가 작습니다. 처음부터 전부 담으려면 '전체를 담은 번들'을 체크하세요. 전체를 담은 번들로는 새 저장소를 만들 수 있습니다.");
            ui.label("작업 브랜치(feature·hotfix) 번들은 1.0.5 와 같게 늘 전체 이력을 담습니다. 받는 쪽에 기준 이력이 없어도 적용됩니다.");
            ui.label("서버 최신화는 번들을 만들 때 자동으로 합니다. 따로 최신화 단추를 누를 필요가 없습니다.");
            ui.label("저장소 목록 위에서 전체 체크·체크 해제·표시 항목 체크를 고릅니다. 필요 없는 저장소는 체크하고 [체크 항목 목록에서 빼기]나 목록 위에서 Delete 키로 제외합니다. 빼기 전에 체크한 개수와 검색·필터로 숨겨진 개수를 묻습니다.");
            ui.label("뺀 저장소는 다시 찾기에서 돌아오지 않고, [목록 관리 → 뺀 저장소 모두 다시 넣기]로 기준·작업 브랜치 설정과 함께 되돌립니다. 폴더가 없어진 저장소는 [목록 관리 → 폴더 없는 저장소 빼기]로 한 번에 지웁니다.");
        });

        section(ui, "상태 표시 다섯 가지", true, |ui| {
            status_row(ui, Status::Done, "끝까지 잘 됐습니다. 따로 할 일이 없습니다.");
            status_row(ui, Status::Check, "됐지만 확인이 필요합니다. 일부만 적용됐거나, 서버 기준 브랜치가 앞서 있거나, 덧붙인 안내가 있는 경우입니다.");
            status_row(ui, Status::Stop, "중간에 멈췄거나 실패했습니다. 멈춤 안내에 적힌 이유와 다음 할 일을 따라 주세요.");
            status_row(ui, Status::Hold, "건너뛰었습니다. 이미 적용했거나 예전 번들이라 하지 않았습니다.");
            status_row(ui, Status::Wait, "아직 때가 아닙니다. 먼저 적용해야 하는 번들이 있어서 기다리는 중입니다.");
        });

        section(ui, "미리보기 · 멈춤 안내 · 되돌리기", true, |ui| {
            ui.label(RichText::new("미리보기").bold());
            ui.label("번들 적용 탭과 번들 만들기 탭에서 실행하기 전에 씁니다. 결과를 미리 보여 줄 뿐 저장소는 바뀌지 않습니다.");
            ui.add_space(4.0);
            ui.label(RichText::new("멈춤 안내").bold());
            ui.label("결과가 멈춤이면 결과 아래 붉은 칸에 멈춘 이유와 다음 할 일이 나옵니다.");
            ui.add_space(4.0);
            ui.label(RichText::new("되돌리기").bold());
            ui.label("적용하기 전 상태는 되돌리기 기록으로 남습니다. 아래 기록 창의 \"되돌리기…\"를 눌러 기록을 고르면 그때 상태로 되돌립니다.");
        });

        section(ui, "업데이트", false, |ui| {
            ui.label("업데이트 탭의 \"새 버전 자동 확인\"을 켜야만 인터넷으로 새 버전을 확인합니다. 끄면 새 버전 확인을 위해 접속하지 않습니다.");
            ui.label("서명이 맞는 더 높은 버전만 알리고, [바꾸고 다시 시작]을 눌러야 바뀝니다. 작업 중에는 바꿀 수 없습니다.");
        });

        section(ui, "명령줄", false, |ui| {
            ui.label("아무것도 붙이지 않고 실행하면 이 화면이 뜨고, 뒤에 명령을 붙이면 명령줄로 동작합니다.");
            for l in [
                "GitBridge export  --repo P [--kind base|standard|all] [--branch B] [--full] --out DIR",
                "GitBridge create  --repo P --branch B --out DIR [--full]",
                "GitBridge refresh --repo P",
                "GitBridge apply   --bundle F --repo P [--sync-all]",
                "GitBridge restore --bundle F --dest P",
                "GitBridge queue   --dir D --src-root P [--apply]",
                "GitBridge undo list --repo P",
                "GitBridge undo run  --repo P [--id 기록이름]",
            ] {
                ui.label(RichText::new(l).monospace());
            }
            ui.label("export 에서 --kind 를 빼면 base 입니다. 1.0.5 의 --scope 와 --base-branch 도 --kind, --branch 와 같은 뜻으로 받습니다.");
            ui.label("명령줄 create 는 바뀐 부분만 담는 것이 기본이고, 전체 이력은 --full 로 담습니다.");
            ui.label("공통 옵션: --json(결과를 JSON 으로 출력), --help, --version.");
        });
    });
    if want_copy {
        ui.ctx().copy_text(AI_HANDOFF_PROMPT.to_string());
        st.help.copied_at = Some(std::time::Instant::now());
    }
}
