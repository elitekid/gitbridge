//! 1.0.5 theme.py 의 색·글꼴·크기를 egui 스타일로 옮긴다.

use eframe::egui::{self, epaint::text::{FontInsert, FontPriority, InsertFontFamily}, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle};

pub const BACK: Color32 = Color32::from_rgb(0xF7, 0xF7, 0xF4);
pub const CARD: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
pub const LINE: Color32 = Color32::from_rgb(0xE3, 0xE6, 0xE1);
pub const TEXT: Color32 = Color32::from_rgb(0x25, 0x2B, 0x29);
pub const SUB: Color32 = Color32::from_rgb(0x5D, 0x68, 0x5F);
pub const ACCENT: Color32 = Color32::from_rgb(0x36, 0x5B, 0x49);
pub const ACCENT_DK: Color32 = Color32::from_rgb(0x29, 0x3F, 0x33);
pub const HEADER_SUB: Color32 = Color32::from_rgb(0x77, 0x7E, 0x7A);
pub const OK: Color32 = Color32::from_rgb(0x52, 0x6F, 0x5C);
pub const WARN: Color32 = Color32::from_rgb(0xA5, 0x6A, 0x2E);
pub const ERR: Color32 = Color32::from_rgb(0xA3, 0x48, 0x3B);
pub const IDLE: Color32 = Color32::from_rgb(0x92, 0x9C, 0x94);
pub const LOG_BG: Color32 = Color32::from_rgb(0xFA, 0xFB, 0xF8);
pub const LOG_FG: Color32 = Color32::from_rgb(0x50, 0x5B, 0x52);
/// 멈춤 안내 상자 바탕(ERR 의 옅은 색).
pub const ERR_BG: Color32 = Color32::from_rgb(0xF8, 0xEC, 0xE9);
/// 선택한 줄 바탕.
pub const SEL_BG: Color32 = Color32::from_rgb(0xE6, 0xEE, 0xE9);

/// 글꼴과 스타일을 한 번에 설치한다.
pub fn install(ctx: &egui::Context) {
    load_fonts(ctx);
    apply_style(ctx);
}

/// 굵은 글씨 글꼴 가족 이름(가변 글꼴의 wght 700).
pub fn bold_family() -> FontFamily {
    FontFamily::Name("noto-bold".into())
}

/// RichText 를 굵게(가변 글꼴 wght 700)로 바꾼다. egui 의 strong 은 색만 바꿔 쓰지 않는다.
pub trait Bold {
    fn bold(self) -> egui::RichText;
}

impl Bold for egui::RichText {
    fn bold(self) -> egui::RichText {
        self.family(bold_family())
    }
}

impl Bold for &str {
    fn bold(self) -> egui::RichText {
        egui::RichText::new(self).family(bold_family())
    }
}

fn weighted(bytes: Vec<u8>, wght: f32) -> egui::FontData {
    let mut tweak = egui::epaint::text::FontTweak::default();
    tweak.coords = egui::epaint::text::VariationCoords::new([("wght", wght)]);
    egui::FontData::from_owned(bytes).tweak(tweak)
}

fn add(ctx: &egui::Context, name: &str, bytes: Vec<u8>, prop: Option<FontPriority>, mono: Option<FontPriority>) {
    let mut fams = Vec::new();
    if let Some(priority) = prop {
        fams.push(InsertFontFamily { family: FontFamily::Proportional, priority });
    }
    if let Some(priority) = mono {
        fams.push(InsertFontFamily { family: FontFamily::Monospace, priority });
    }
    ctx.add_font(FontInsert::new(name, weighted(bytes, 400.0), fams));
}

fn load_fonts(ctx: &egui::Context) {
    // 맑은 고딕은 내장 글꼴이 없는 글자를 위한 뒷순위 대비용이다.
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    for name in ["malgun.ttf", "gulim.ttc"] {
        let p = std::path::Path::new(&windir).join("Fonts").join(name);
        if let Ok(bytes) = std::fs::read(&p) {
            add(ctx, "fallback-korean", bytes, Some(FontPriority::Lowest), Some(FontPriority::Lowest));
            break;
        }
    }
    add(ctx, "noto-sans-kr", include_bytes!("../../assets/fonts/NotoSansKR-VF.ttf").to_vec(), Some(FontPriority::Highest), Some(FontPriority::Lowest));
    let font = include_bytes!("../../assets/fonts/NotoSansKR-VF.ttf").to_vec();
    ctx.add_font(FontInsert::new("noto-bold", weighted(font, 700.0), vec![InsertFontFamily { family: bold_family(), priority: FontPriority::Highest }]));
    add(ctx, "cascadia-mono", include_bytes!("../../assets/fonts/CascadiaMono.ttf").to_vec(), None, Some(FontPriority::Highest));
}

fn apply_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Light);
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(18.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(8.0, 7.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.spacing.interact_size = egui::vec2(36.0, 32.0);
        s.spacing.window_margin = egui::Margin::same(14);
        s.spacing.indent = 18.0;

        let v = &mut s.visuals;
        v.override_text_color = Some(TEXT);
        v.panel_fill = BACK;
        v.window_fill = CARD;
        v.window_stroke = Stroke::new(1.0, LINE);
        v.window_corner_radius = CornerRadius::same(8);
        v.extreme_bg_color = CARD;
        v.text_edit_bg_color = Some(CARD);
        v.faint_bg_color = Color32::from_rgb(0xF4, 0xF6, 0xF2);
        v.code_bg_color = LOG_BG;
        v.hyperlink_color = ACCENT;
        v.selection.bg_fill = Color32::from_rgb(0xCF, 0xE0, 0xD6);
        v.selection.stroke = Stroke::new(1.0, ACCENT);

        let r = CornerRadius::same(6);
        v.widgets.noninteractive.bg_fill = CARD;
        v.widgets.noninteractive.weak_bg_fill = CARD;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.noninteractive.corner_radius = r;

        v.widgets.inactive.bg_fill = CARD;
        v.widgets.inactive.weak_bg_fill = CARD;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0xCF, 0xD5, 0xCD));
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.inactive.corner_radius = r;

        v.widgets.hovered.bg_fill = Color32::from_rgb(0xEE, 0xF3, 0xEF);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0xEE, 0xF3, 0xEF);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, ACCENT_DK);
        v.widgets.hovered.corner_radius = r;
        v.widgets.hovered.expansion = 0.0;

        v.widgets.active.bg_fill = Color32::from_rgb(0xDD, 0xE8, 0xE1);
        v.widgets.active.weak_bg_fill = Color32::from_rgb(0xDD, 0xE8, 0xE1);
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT_DK);
        v.widgets.active.fg_stroke = Stroke::new(1.0, ACCENT_DK);
        v.widgets.active.corner_radius = r;
        v.widgets.active.expansion = 0.0;

        v.widgets.open = v.widgets.active;
    });
}
