//! Settings window in the style of Windows 11 Settings: a navigation sidebar, cards, toggle switches,
//! segmented buttons and sliders — all drawn with the same Direct2D/DirectWrite code as the overlay
//! (software rasterizer, no UI framework, no WebView). Only the search box is a native EDIT control.
//!
//! The UI is immediate-mode: every paint lays out the page from the current state and records hit
//! rectangles; mouse input is matched against them. No borrow of the UI state is held across Win32 calls.
use std::cell::RefCell;

use windows::core::{w as wlit, Interface, Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::Vector2;

use crate::app;
use crate::i18n::t;
use crate::matcher::MatchKind;
use crate::overlay::{color, text as dw_text, with_gfx, Align, Gfx};
use crate::settings::{Settings, VK_ANY_CTRL, VK_APPS, VK_LCONTROL, VK_RSHIFT};
use crate::util::{from_wide, url_encode, user_profiles_dir, w, APP_VERSION, REPO_URL, SITE_URL};
use crate::{hook, startup, update};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Device,
    Trigger,
    About,
}

const CLASS: PCWSTR = wlit!("FnCheatsheetSettings");
const W_DIP: f32 = 880.0;
const H_DIP: f32 = 680.0;
const SIDEBAR: f32 = 228.0;
const PAD: f32 = 36.0;
const TIMER_LEARN: usize = 1;
const ID_SEARCH: i32 = 100;
const FONT: &str = "Segoe UI";
const ICONS: &str = "Segoe MDL2 Assets";

// ───────────────────────── palette ─────────────────────────

#[derive(Clone, Copy)]
struct Pal {
    dark: bool,
    bg: u32,
    card: u32,
    card_border: u32,
    divider: u32,
    text: u32,
    sub: u32,
    accent: u32,
    on_accent: u32,
    accent_soft: u32,
    hover: u32,
    pressed: u32,
    control: u32,
    control_border: u32,
    track_off: u32,
    good: u32,
    warn: u32,
}

const LIGHT: Pal = Pal {
    dark: false, bg: 0xFFF3F3F3, card: 0xFFFFFFFF, card_border: 0xFFE5E5E5, divider: 0xFFEAEAEA, text: 0xFF1B1B1B, sub: 0xFF5F5F5F,
    accent: 0xFF0067C0, on_accent: 0xFFFFFFFF, accent_soft: 0x1A0067C0, hover: 0x0A000000, pressed: 0x14000000,
    control: 0xFFFBFBFB, control_border: 0xFFD6D6D6, track_off: 0xFF8A8A8A, good: 0xFF0F7B0F, warn: 0xFF9D5D00,
};
const DARK: Pal = Pal {
    dark: true, bg: 0xFF202020, card: 0xFF2B2B2B, card_border: 0xFF1F1F1F, divider: 0xFF3A3A3A, text: 0xFFFFFFFF, sub: 0xFFC8C8C8,
    accent: 0xFF4CC2FF, on_accent: 0xFF000000, accent_soft: 0x264CC2FF, hover: 0x10FFFFFF, pressed: 0x1AFFFFFF,
    control: 0xFF2F2F2F, control_border: 0xFF454545, track_off: 0xFF9A9A9A, good: 0xFF6CCB5F, warn: 0xFFFCE100,
};

fn pal_for(theme: &str) -> Pal {
    match theme {
        "dark" => DARK,
        "light" => LIGHT,
        _ => {
            if app::apps_use_light_theme() { LIGHT } else { DARK }
        }
    }
}

fn colorref(argb: u32) -> COLORREF {
    COLORREF(((argb & 0xFF) << 16) | (argb & 0xFF00) | ((argb >> 16) & 0xFF))
}

// ───────────────────────── state ─────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
enum Act {
    Nav(Tab),
    CopyInfo,
    Request,
    Details,
    ModeAuto,
    ModeManual,
    Row(usize),
    Preview,
    Folder,
    Reload,
    TriggerEnabled,
    Key(u32),
    HoldSlider,
    SkipFs,
    LearnStart,
    LearnUse,
    Startup,
    Lang(u8),
    Theme(u8),
    ScaleSlider,
    AutoUpdate,
    CheckNow,
    Site,
    Github,
    Issues,
    Source(usize),
    /// The native search box (a tab stop; clicking it focuses the EDIT).
    Search,
}

#[derive(Clone, Copy, Default)]
struct R {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl R {
    fn new(x: f32, y: f32, w: f32, h: f32) -> R {
        R { x, y, w, h }
    }
    fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.x + self.w, bottom: self.y + self.h }
    }
}

struct Ui {
    hwnd: HWND,
    dpi: u32,
    tab: Tab,
    hits: Vec<(R, Act)>,
    hover: Option<Act>,
    pressed: Option<Act>,
    dragging: Option<(Act, R)>,
    tracking: bool,
    /// Keyboard focus among the drawn controls, and whether to show the focus ring (keyboard use only).
    focus: Option<Act>,
    focus_visible: bool,
    /// Profile indices of the model list in display order (for arrow keys).
    row_order: Vec<usize>,
    search: HWND,
    search_font: HFONT,
    search_brush: HBRUSH,
    search_rect: R,
    query: String,
    list_rect: R,
    list_scroll: f32,
    list_h: f32,
    page_scroll: f32,
    page_h: f32,
    details: bool,
    learning: bool,
    learned: Option<(u32, u32, bool)>,
    learn_saw: bool,
    learn_msg: String,
    learn_tone: u8, // 0 neutral, 1 good, 2 warn
    icon: Option<IWICFormatConverter>,
    pal: Pal,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

fn ui<T>(f: impl FnOnce(&mut Ui) -> T) -> Option<T> {
    UI.with(|u| u.try_borrow_mut().ok().and_then(|mut u| u.as_mut().map(f)))
}

fn repaint() {
    if let Some(h) = ui(|u| u.hwnd) {
        unsafe {
            let _ = InvalidateRect(Some(h), None, false);
        }
    }
}

fn scale() -> f32 {
    ui(|u| u.dpi as f32 / 96.0).unwrap_or(1.0)
}

// ───────────────────────── data snapshot (taken per paint) ─────────────────────────

struct RowInfo {
    idx: usize,
    name: String,
    sub: String,
    candidate: bool,
    partial: bool,
    user_file: bool,
    active: bool,
}

struct Snap {
    s: Settings,
    headline: String,
    report: String,
    det_kind: MatchKind,
    det_text: String,
    active: Option<String>,
    rows: Vec<RowInfo>,
    errors: Vec<String>,
    count: usize,
    data_version: String,
    update_status: String,
    update_busy: bool,
    sources: Vec<(String, String, String)>,
    notes: String,
}

fn snapshot(query: &str) -> Option<Snap> {
    let q = query.trim().to_lowercase();
    app::with(|a| {
        let det_kind = a.detection.kind;
        let det_text = match det_kind {
            MatchKind::Matched => format!("{} · {}", t("Detected automatically", "자동 감지됨"), a.detection.candidates[0].reason),
            MatchKind::Ambiguous => {
                let top = a.detection.candidates[0].score;
                let names: Vec<&str> = a.detection.candidates.iter().filter(|c| c.score == top).map(|c| a.db.profiles[c.index].short_name()).collect();
                format!("{} {}", t("Several models match — pick yours:", "여러 모델이 똑같이 맞아요. 직접 골라 주세요:"), names.join(", "))
            }
            MatchKind::Unknown => t("Not in the list yet — pick a model with the same keyboard below", "아직 목록에 없어요. 키보드가 같은 모델을 아래에서 골라 주세요").to_string(),
        };
        let cands: Vec<usize> = a.detection.candidates.iter().map(|c| c.index).collect();
        let active_idx = a.active_index();
        let mut idx: Vec<usize> = (0..a.db.profiles.len())
            .filter(|&i| {
                let p = &a.db.profiles[i];
                q.is_empty() || format!("{} {} {} {} {}", p.vendor, p.series, p.name, p.match_rule.product.join(" "), p.match_rule.mtm.join(" ")).to_lowercase().contains(&q)
            })
            .collect();
        idx.sort_by(|&x, &y| {
            let (px, py) = (&a.db.profiles[x], &a.db.profiles[y]);
            cands.contains(&y).cmp(&cands.contains(&x)).then_with(|| px.vendor.cmp(&py.vendor)).then_with(|| px.name.cmp(&py.name))
        });
        let rows = idx
            .into_iter()
            .map(|i| {
                let p = &a.db.profiles[i];
                let codes: Vec<String> = p.match_rule.product.iter().take(4).cloned().chain(p.match_rule.mtm.iter().take(4).map(|m| format!("MT {m}"))).collect();
                let mut sub = p.vendor.clone();
                if !p.years.is_empty() {
                    sub += &format!("  ·  {}", p.years);
                }
                if !codes.is_empty() {
                    sub += &format!("  ·  {}", codes.join(", "));
                }
                RowInfo { idx: i, name: p.short_name().to_string(), sub, candidate: cands.contains(&i), partial: p.verified == "partial", user_file: !p.is_builtin(), active: Some(i) == active_idx }
            })
            .collect();
        let active = a.active();
        let headline = match (det_kind, a.detection.best()) {
            (MatchKind::Matched, Some(i)) => a.db.profiles[i].short_name().to_string(),
            _ => a.device.short_label(),
        };
        Snap {
            s: a.settings.clone(),
            headline,
            report: a.device.report(),
            det_kind,
            det_text,
            active: active.map(|p| p.short_name().to_string()),
            rows,
            errors: a.db.errors.clone(),
            count: a.db.profiles.len(),
            data_version: a.db.data_version.clone(),
            update_status: a.update_status.clone(),
            update_busy: a.update_busy,
            sources: active.map(|p| p.sources.iter().map(|s| (s.title.clone(), s.url.clone(), s.kind.clone())).collect()).unwrap_or_default(),
            notes: active.map(|p| p.notes().to_string()).unwrap_or_default(),
        }
    })
}

// ───────────────────────── drawing context ─────────────────────────

struct Dc<'a> {
    rt: &'a ID2D1RenderTarget,
    g: &'a Gfx,
    brush: ID2D1SolidColorBrush,
    p: Pal,
    hits: Vec<(R, Act)>,
    hover: Option<Act>,
    pressed: Option<Act>,
}

impl Dc<'_> {
    fn fill(&self, r: R, radius: f32, c: u32) {
        unsafe {
            self.brush.SetColor(&color(c));
            let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
            self.rt.FillRoundedRectangle(&rr, &self.brush);
        }
    }

    fn stroke(&self, r: R, radius: f32, c: u32, width: f32) {
        unsafe {
            self.brush.SetColor(&color(c));
            let h = width / 2.0;
            let rr = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: r.x + h, top: r.y + h, right: r.x + r.w - h, bottom: r.y + r.h - h }, radiusX: radius, radiusY: radius };
            self.rt.DrawRoundedRectangle(&rr, &self.brush, width, None);
        }
    }

    fn circle(&self, cx: f32, cy: f32, r: f32, c: u32) {
        unsafe {
            self.brush.SetColor(&color(c));
            let e = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r };
            self.rt.FillEllipse(&e, &self.brush);
        }
    }

    fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, c: u32) {
        unsafe {
            self.brush.SetColor(&color(c));
            let _ = self.rt.DrawLine(Vector2 { X: x0, Y: y0 }, Vector2 { X: x1, Y: y1 }, &self.brush, 1.0, None);
        }
    }

    /// Draws text and returns its size. `max_w` wraps; `align` centers within max_w.
    fn text(&self, s: &str, x: f32, y: f32, size: f32, bold: bool, c: u32, max_w: f32, wrap: bool, align: Align) -> (f32, f32) {
        let Ok(tl) = dw_text(self.g, s, size, bold, FONT, max_w, align, wrap) else { return (0.0, 0.0) };
        if !wrap {
            // Single-line text never runs past its box: end it with "…".
            unsafe {
                use windows::Win32::Graphics::DirectWrite::{IDWriteTextFormat, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER};
                if let Ok(fmt) = tl.layout.cast::<IDWriteTextFormat>() {
                    if let Ok(sign) = self.g.dw.CreateEllipsisTrimmingSign(&fmt) {
                        let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                        let _ = tl.layout.SetTrimming(&trim, &sign);
                    }
                }
            }
        }
        unsafe {
            self.brush.SetColor(&color(c));
            self.rt.DrawTextLayout(Vector2 { X: x, Y: y }, &tl.layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
        (tl.w, tl.h)
    }

    fn measure(&self, s: &str, size: f32, bold: bool, max_w: f32, wrap: bool) -> (f32, f32) {
        dw_text(self.g, s, size, bold, FONT, max_w, Align::Left, wrap).map(|t| (t.w, t.h)).unwrap_or((0.0, 0.0))
    }

    fn glyph(&self, ch: char, x: f32, y: f32, size: f32, c: u32) {
        if let Ok(tl) = dw_text(self.g, &ch.to_string(), size, false, ICONS, size * 2.0, Align::Left, false) {
            unsafe {
                self.brush.SetColor(&color(c));
                self.rt.DrawTextLayout(Vector2 { X: x, Y: y }, &tl.layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
        }
    }

    fn hit(&mut self, r: R, a: Act) {
        self.hits.push((r, a));
    }

    fn state(&self, a: Act) -> (bool, bool) {
        (self.hover == Some(a), self.pressed == Some(a))
    }

    fn card(&self, r: R) {
        self.fill(r, 8.0, self.p.card);
        self.stroke(r, 8.0, self.p.card_border, 1.0);
    }

    /// Fluent button. kind: 0 = standard, 1 = accent, 2 = hyperlink. Returns width.
    fn button(&mut self, x: f32, y: f32, label: &str, glyph: Option<char>, a: Act, kind: u8, enabled: bool) -> f32 {
        let (tw, _) = self.measure(label, 13.5, kind == 1, 600.0, false);
        let gw = if glyph.is_some() { 22.0 } else { 0.0 };
        let w = (tw + gw + 28.0).max(if kind == 2 { 0.0 } else { 96.0 });
        let r = R::new(x, y, w, 34.0);
        let (hov, prs) = self.state(a);
        let p = self.p;
        let (fg, alpha) = match kind {
            1 => {
                let base = if !enabled { p.track_off } else { p.accent };
                self.fill(r, 5.0, base);
                if enabled && (hov || prs) {
                    self.fill(r, 5.0, if prs { 0x33000000 } else if p.dark { 0x1A000000 } else { 0x1AFFFFFF });
                }
                (p.on_accent, 1.0)
            }
            2 => {
                if hov || prs {
                    self.fill(r, 5.0, if prs { p.pressed } else { p.hover });
                }
                (p.accent, 1.0)
            }
            _ => {
                self.fill(r, 5.0, p.control);
                self.stroke(r, 5.0, p.control_border, 1.0);
                if enabled && (hov || prs) {
                    self.fill(r, 5.0, if prs { p.pressed } else { p.hover });
                }
                (p.text, if enabled { 1.0 } else { 0.45 })
            }
        };
        let fg = if alpha < 1.0 { (fg & 0x00FFFFFF) | (((alpha * 255.0) as u32) << 24) } else { fg };
        let mut tx = x + (w - tw - gw) / 2.0;
        if let Some(g) = glyph {
            self.glyph(g, tx, y + 9.0, 14.0, fg);
            tx += gw;
        }
        self.text(label, tx, y + 7.5, 13.5, kind == 1, fg, 600.0, false, Align::Left);
        if enabled {
            self.hit(r, a);
        }
        w
    }

    /// Windows 11 toggle switch, 40×20, at its right edge `right`.
    fn toggle(&mut self, right: f32, cy: f32, on: bool, a: Act) {
        let p = self.p;
        let (hov, _) = self.state(a);
        let label = if on { t("On", "켬") } else { t("Off", "끔") };
        let (lw, _) = self.measure(label, 13.5, false, 100.0, false);
        let r = R::new(right - 40.0, cy - 10.0, 40.0, 20.0);
        if on {
            self.fill(r, 10.0, p.accent);
            self.circle(r.x + 30.0, cy, if hov { 7.0 } else { 6.0 }, p.on_accent);
        } else {
            self.fill(r, 10.0, if hov { p.hover } else { 0x00000000 });
            self.stroke(r, 10.0, p.track_off, 1.0);
            self.circle(r.x + 10.0, cy, if hov { 6.0 } else { 5.0 }, p.track_off);
        }
        self.text(label, r.x - lw - 12.0, cy - 9.5, 13.5, false, p.text, 100.0, false, Align::Left);
        self.hit(R::new(r.x - lw - 16.0, cy - 16.0, lw + 60.0, 32.0), a);
    }

    /// Segmented control ending at `right`. Returns its rect.
    fn segmented(&mut self, right: f32, cy: f32, items: &[(&str, Act, bool)]) -> R {
        let p = self.p;
        let widths: Vec<f32> = items.iter().map(|(l, _, _)| self.measure(l, 13.0, false, 400.0, false).0 + 24.0).collect();
        let total: f32 = widths.iter().sum::<f32>() + 4.0;
        let r = R::new(right - total, cy - 16.0, total, 32.0);
        self.fill(r, 6.0, p.control);
        self.stroke(r, 6.0, p.control_border, 1.0);
        let mut x = r.x + 2.0;
        for ((label, a, sel), w) in items.iter().zip(widths) {
            let seg = R::new(x, r.y + 2.0, w, 28.0);
            let (hov, _) = self.state(*a);
            if *sel {
                self.fill(seg, 4.0, p.accent);
            } else if hov {
                self.fill(seg, 4.0, p.hover);
            }
            self.text(label, x, r.y + 7.0, 13.0, *sel, if *sel { p.on_accent } else { p.text }, w, false, Align::Center);
            self.hit(seg, *a);
            x += w;
        }
        r
    }

    /// Slider with a value label on its right. `frac` in 0..1. Returns the track rect (for dragging).
    fn slider(&mut self, x: f32, cy: f32, w: f32, frac: f32, label: &str, a: Act) -> R {
        let p = self.p;
        let track = R::new(x, cy - 2.0, w, 4.0);
        self.fill(track, 2.0, p.track_off & 0x80FFFFFF);
        self.fill(R::new(x, cy - 2.0, w * frac, 4.0), 2.0, p.accent);
        let tx = x + w * frac;
        let (hov, prs) = self.state(a);
        self.circle(tx, cy, 10.0, p.control);
        self.circle(tx, cy, 10.5, p.control_border & 0x60FFFFFF);
        self.circle(tx, cy, 9.5, p.control);
        self.circle(tx, cy, if prs { 4.0 } else if hov { 6.0 } else { 5.0 }, p.accent);
        self.text(label, x + w + 14.0, cy - 9.5, 13.5, false, p.text, 120.0, false, Align::Left);
        let hit = R::new(x - 10.0, cy - 14.0, w + 20.0, 28.0);
        self.hit(hit, a);
        R::new(x, cy - 14.0, w, 28.0)
    }

    /// A settings row inside a card: title + optional description on the left. Returns row height.
    fn row_text(&self, x: f32, y: f32, w: f32, title: &str, desc: &str) -> f32 {
        let (_, th) = self.text(title, x, y, 14.0, false, self.p.text, w, true, Align::Left);
        let mut h = th;
        if !desc.is_empty() {
            let (_, dh) = self.text(desc, x, y + th + 2.0, 12.0, false, self.p.sub, w, true, Align::Left);
            h += dh + 2.0;
        }
        h
    }
}

// ───────────────────────── pages ─────────────────────────

const G_LAPTOP: char = '\u{E7F8}';
const G_KEYBOARD: char = '\u{E765}';
const G_INFO: char = '\u{E946}';
const G_COPY: char = '\u{E8C8}';
const G_LINK: char = '\u{E8A7}';
const G_FOLDER: char = '\u{E8B7}';
const G_REFRESH: char = '\u{E72C}';
const G_SEARCH: char = '\u{E721}';
const G_VIEW: char = '\u{E7B3}';
const G_CHECK: char = '\u{E73E}';
const G_WARN: char = '\u{E7BA}';
const G_DOWN: char = '\u{E70D}';
const G_UP: char = '\u{E70E}';
const G_SYNC: char = '\u{E895}';
const G_SHIELD: char = '\u{EA18}';

fn badge(dc: &Dc, x: f32, y: f32, label: &str, fg: u32, bg: u32) -> f32 {
    let (tw, _) = dc.measure(label, 11.0, true, 200.0, false);
    let r = R::new(x, y, tw + 12.0, 18.0);
    dc.fill(r, 9.0, bg);
    dc.text(label, x + 6.0, y + 1.5, 11.0, true, fg, 200.0, false, Align::Left);
    r.w
}

fn sidebar(dc: &mut Dc, tab: Tab, icon: Option<&ID2D1Bitmap>) {
    let p = dc.p;
    if let Some(bmp) = icon {
        unsafe {
            dc.rt.DrawBitmap(bmp, Some(&R::new(24.0, 22.0, 36.0, 36.0).d2d()), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
        }
    }
    dc.text("Fn Cheatsheet", 70.0, 22.0, 15.0, true, p.text, 160.0, false, Align::Left);
    dc.text(&format!("v{APP_VERSION}"), 70.0, 42.0, 12.0, false, p.sub, 160.0, false, Align::Left);
    let items = [
        (Tab::Device, G_LAPTOP, t("Device", "기기")),
        (Tab::Trigger, G_KEYBOARD, t("Trigger & display", "누르기·표시")),
        (Tab::About, G_INFO, t("Updates & about", "업데이트·정보")),
    ];
    let mut y = 86.0;
    for (tb, g, label) in items {
        let r = R::new(12.0, y, SIDEBAR - 24.0, 38.0);
        let a = Act::Nav(tb);
        let (hov, prs) = dc.state(a);
        if tb == tab {
            dc.fill(r, 5.0, if p.dark { 0x0FFFFFFF } else { 0x0A000000 });
            dc.fill(R::new(r.x, r.y + 11.0, 3.0, 16.0), 1.5, p.accent);
        } else if hov || prs {
            dc.fill(r, 5.0, if prs { p.pressed } else { p.hover });
        }
        dc.glyph(g, r.x + 16.0, r.y + 11.0, 16.0, p.text);
        dc.text(label, r.x + 46.0, r.y + 9.0, 14.0, false, p.text, r.w - 50.0, false, Align::Left);
        dc.hit(r, a);
        y += 42.0;
    }
}

fn page_title(dc: &Dc, x: f32, y: f32, s: &str) -> f32 {
    dc.text(s, x, y, 26.0, true, dc.p.text, 600.0, false, Align::Left).1
}

/// Device page. Returns the search box rect (for the native EDIT) and list rect.
fn page_device(dc: &mut Dc, s: &Snap, u: &UiView, x0: f32, cw: f32, h: f32) -> (R, R, f32) {
    let p = dc.p;
    let mut y = 28.0;
    y += page_title(dc, x0, y, t("Device", "기기")) + 18.0;

    // Hero card: this PC + detection
    let det_good = s.det_kind == MatchKind::Matched;
    let report_lines = s.report.lines().count() as f32;
    let hero_h = 112.0 + if u.details { report_lines * 18.0 + 14.0 } else { 0.0 };
    let hero = R::new(x0, y, cw, hero_h);
    dc.card(hero);
    dc.circle(x0 + 44.0, y + 44.0, 24.0, p.accent_soft);
    dc.glyph(G_LAPTOP, x0 + 32.0, y + 32.0, 24.0, p.accent);
    dc.text(t("This PC", "이 PC"), x0 + 82.0, y + 16.0, 12.0, false, p.sub, cw - 100.0, false, Align::Left);
    dc.text(&s.headline, x0 + 82.0, y + 32.0, 18.0, true, p.text, cw - 100.0, false, Align::Left);
    let (tone, g) = if det_good { (p.good, G_CHECK) } else { (p.warn, G_WARN) };
    dc.glyph(g, x0 + 82.0, y + 63.0, 13.0, tone);
    dc.text(&s.det_text, x0 + 102.0, y + 60.0, 13.0, false, tone, cw - 130.0, false, Align::Left);
    let by = y + 90.0 - 4.0;
    let _ = by;
    // buttons on the right of the headline
    let bw = dc.measure(t("Request this model", "이 모델 추가 요청"), 13.5, false, 600.0, false).0 + 50.0;
    let cw2 = dc.measure(t("Copy info", "정보 복사"), 13.5, false, 600.0, false).0 + 50.0;
    let bx = x0 + cw - 16.0 - bw;
    dc.button(bx, y + 18.0, t("Request this model", "이 모델 추가 요청"), Some(G_LINK), Act::Request, 0, true);
    dc.button(bx - 8.0 - cw2.max(96.0), y + 18.0, t("Copy info", "정보 복사"), Some(G_COPY), Act::CopyInfo, 0, true);
    // details toggle
    let dl = if u.details { t("Hide details", "세부 정보 숨기기") } else { t("Show details", "세부 정보 보기") };
    let (dw, _) = dc.measure(dl, 12.5, false, 300.0, false);
    let dr = R::new(x0 + 76.0, y + 82.0, dw + 30.0, 24.0);
    let (hov, _) = dc.state(Act::Details);
    if hov {
        dc.fill(dr, 4.0, p.hover);
    }
    dc.text(dl, dr.x + 6.0, dr.y + 3.5, 12.5, false, p.accent, 300.0, false, Align::Left);
    dc.glyph(if u.details { G_UP } else { G_DOWN }, dr.x + dw + 12.0, dr.y + 7.0, 10.0, p.accent);
    dc.hit(dr, Act::Details);
    if u.details {
        let mut ly = y + 112.0;
        dc.line(x0 + 16.0, ly - 4.0, x0 + cw - 16.0, ly - 4.0, p.divider);
        for l in s.report.lines() {
            dc.text(l, x0 + 82.0, ly + 2.0, 12.0, false, p.sub, cw - 100.0, false, Align::Left);
            ly += 18.0;
        }
    }
    y += hero_h + 14.0;

    // Model card
    let bottom_bar = 50.0;
    let card = R::new(x0, y, cw, (h - y - 20.0 - bottom_bar).max(160.0));
    dc.card(card);
    dc.text(t("Model to show", "보여 줄 모델"), x0 + 20.0, y + 16.0, 14.0, true, p.text, cw - 300.0, false, Align::Left);
    let in_use = match &s.active {
        Some(n) => format!("{} {}", t("In use:", "사용 중:"), n),
        None => t("None selected — the sheet will ask you to pick one", "선택 안 됨 — 화면에 모델을 고르라는 안내가 나와요").to_string(),
    };
    dc.text(&in_use, x0 + 20.0, y + 38.0, 12.5, false, if s.active.is_some() { p.good } else { p.warn }, cw - 300.0, false, Align::Left);
    let auto_ok = det_good;
    let manual = s.s.profile_mode == "manual" || !auto_ok;
    dc.segmented(
        x0 + cw - 20.0,
        y + 32.0,
        &[(t("Automatic", "자동 감지"), Act::ModeAuto, !manual), (t("Choose myself", "직접 선택"), Act::ModeManual, manual)],
    );
    if !auto_ok {
        // automatic isn't available for this PC: remove its hit target
        dc.hits.retain(|(_, a)| *a != Act::ModeAuto);
    }
    // search box (native EDIT sits inside)
    let sr = R::new(x0 + 20.0, y + 66.0, cw - 40.0, 34.0);
    dc.hit(sr, Act::Search);
    dc.fill(sr, 5.0, p.control);
    dc.stroke(sr, 5.0, p.control_border, 1.0);
    dc.fill(R::new(sr.x + 1.0, sr.y + sr.h - 2.0, sr.w - 2.0, 1.0), 0.0, p.track_off & 0x60FFFFFF);
    dc.glyph(G_SEARCH, sr.x + sr.w - 30.0, sr.y + 10.0, 14.0, p.sub);
    // list
    let lr = R::new(x0 + 8.0, y + 110.0, cw - 16.0, card.h - 118.0);
    let row_h = 54.0;
    let list_h = s.rows.len() as f32 * row_h;
    unsafe {
        dc.rt.PushAxisAlignedClip(&lr.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
    }
    let first = (u.list_scroll / row_h).floor().max(0.0) as usize;
    let mut ry = lr.y - (u.list_scroll - first as f32 * row_h);
    for row in s.rows.iter().skip(first) {
        if ry > lr.y + lr.h {
            break;
        }
        let rr = R::new(lr.x, ry, lr.w - 10.0, row_h - 4.0);
        let a = Act::Row(row.idx);
        let (hov, prs) = dc.state(a);
        if row.active {
            dc.fill(rr, 5.0, p.accent_soft);
            dc.fill(R::new(rr.x, rr.y + 15.0, 3.0, 20.0), 1.5, p.accent);
        } else if hov || prs {
            dc.fill(rr, 5.0, if prs { p.pressed } else { p.hover });
        }
        let (nw, _) = dc.text(&row.name, rr.x + 16.0, rr.y + 6.0, 14.0, row.active, p.text, rr.w - 200.0, false, Align::Left);
        let mut bx = rr.x + 16.0 + nw + 8.0;
        if row.candidate {
            bx += badge(dc, bx, rr.y + 8.0, t("This PC", "이 PC"), p.on_accent, p.accent) + 6.0;
        }
        if row.partial {
            bx += badge(dc, bx, rr.y + 8.0, t("Partly verified", "일부 확인"), if p.dark { 0xFF000000 } else { 0xFFFFFFFF }, p.warn) + 6.0;
        }
        if row.user_file {
            badge(dc, bx, rr.y + 8.0, t("My file", "내 파일"), p.text, p.hover | 0x20000000);
        }
        dc.text(&row.sub, rr.x + 16.0, rr.y + 27.0, 12.0, false, p.sub, rr.w - 30.0, false, Align::Left);
        if rr.y + rr.h > lr.y && rr.y < lr.y + lr.h {
            let vis = R::new(rr.x, rr.y.max(lr.y), rr.w, (rr.y + rr.h).min(lr.y + lr.h) - rr.y.max(lr.y));
            dc.hit(vis, a);
        }
        ry += row_h;
    }
    if s.rows.is_empty() {
        dc.text(t("No matching model.", "맞는 모델이 없어요."), lr.x + 16.0, lr.y + 12.0, 13.0, false, p.sub, lr.w, false, Align::Left);
    }
    unsafe {
        dc.rt.PopAxisAlignedClip();
    }
    // scrollbar
    if list_h > lr.h {
        let th = (lr.h * lr.h / list_h).max(30.0);
        let ty = lr.y + (lr.h - th) * (u.list_scroll / (list_h - lr.h)).clamp(0.0, 1.0);
        dc.fill(R::new(lr.x + lr.w - 5.0, ty, 3.0, th), 1.5, p.track_off & 0x99FFFFFF);
    }

    // bottom bar
    let yb = card.y + card.h + 12.0;
    let mut bx = x0 + cw;
    let pw = dc.measure(t("Preview", "미리 보기"), 13.5, true, 600.0, false).0 + 50.0;
    bx -= pw.max(96.0);
    dc.button(bx, yb, t("Preview", "미리 보기"), Some(G_VIEW), Act::Preview, 1, true);
    let rw = dc.measure(t("Reload", "다시 불러오기"), 13.5, false, 600.0, false).0 + 50.0;
    bx -= rw.max(96.0) + 8.0;
    dc.button(bx, yb, t("Reload", "다시 불러오기"), Some(G_REFRESH), Act::Reload, 0, true);
    let fw = dc.measure(t("My profiles folder", "내 프로필 폴더"), 13.5, false, 600.0, false).0 + 50.0;
    bx -= fw.max(96.0) + 8.0;
    dc.button(bx, yb, t("My profiles folder", "내 프로필 폴더"), Some(G_FOLDER), Act::Folder, 0, true);
    if !s.errors.is_empty() {
        dc.text(&format!("{} {}", t("Some profile files failed to load:", "불러오지 못한 프로필:"), s.errors.join("; ")), x0, yb + 8.0, 12.0, false, p.warn, bx - x0 - 12.0, false, Align::Left);
    }
    (sr, lr, list_h)
}

struct UiView {
    query: String,
    list_scroll: f32,
    details: bool,
    learn_msg: String,
    learn_tone: u8,
    learn_ready: bool,
    learning: bool,
}

/// Settings-card row helper: draws the divider and returns the y of the next row.
fn divider(dc: &Dc, x0: f32, cw: f32, y: f32) {
    dc.line(x0 + 1.0, y, x0 + cw - 1.0, y, dc.p.divider);
}

fn page_trigger(dc: &mut Dc, s: &Snap, u: &UiView, x0: f32, cw: f32, y0: f32) -> (f32, Vec<(Act, R)>) {
    let p = dc.p;
    let mut sliders = Vec::new();
    let mut y = y0 + 28.0;
    y += page_title(dc, x0, y, t("Trigger & display", "누르기·표시")) + 18.0;
    let text_w = cw - 330.0;

    // Card 1: hold to show
    let info = t(
        "The Fn key is handled inside the keyboard controller on almost every ASUS and Lenovo laptop, so Windows never sees it. Hold the key right next to it instead.",
        "ASUS·레노버 노트북 대부분은 Fn 키를 키보드 안에서 처리해서 Windows가 알 수 없어요. 그래서 바로 옆의 키를 길게 눌러요.",
    );
    let (_, ih) = dc.measure(info, 12.5, false, cw - 40.0, true);
    let rows = 4.0;
    let c1 = R::new(x0, y, cw, 20.0 + ih + 16.0 + rows * 68.0);
    dc.card(c1);
    dc.glyph(G_INFO, x0 + 20.0, y + 21.0, 13.0, p.sub);
    dc.text(info, x0 + 42.0, y + 18.0, 12.5, false, p.sub, cw - 62.0, true, Align::Left);
    let mut ry = y + 20.0 + ih + 16.0;
    // row: enabled
    divider(dc, x0, cw, ry);
    dc.row_text(x0 + 20.0, ry + 14.0, text_w, t("Show the sheet while I hold the key", "키를 길게 누르면 치트시트 보기"), t("Releasing it hides the sheet", "손을 떼면 사라져요"));
    dc.toggle(x0 + cw - 20.0, ry + 34.0, s.s.trigger_enabled, Act::TriggerEnabled);
    ry += 68.0;
    // row: key
    divider(dc, x0, cw, ry);
    dc.row_text(x0 + 20.0, ry + 14.0, cw - 380.0, t("Key", "누를 키"), t("Left Ctrl sits right next to Fn", "왼쪽 Ctrl은 Fn 바로 옆에 있어요"));
    let presets = [(VK_LCONTROL, t("Left Ctrl", "왼쪽 Ctrl")), (VK_ANY_CTRL, t("Either Ctrl", "양쪽 Ctrl")), (VK_RSHIFT, t("Right Shift", "오른쪽 Shift")), (VK_APPS, t("Menu", "메뉴 키"))];
    let custom = s.s.trigger_scan != 0 || !presets.iter().any(|(vk, _)| *vk == s.s.trigger_vk);
    let mut items: Vec<(&str, Act, bool)> = presets.iter().map(|(vk, l)| (*l, Act::Key(*vk), !custom && *vk == s.s.trigger_vk)).collect();
    if custom {
        items.push((t("Captured key", "직접 등록한 키"), Act::Key(0), true));
    }
    dc.segmented(x0 + cw - 20.0, ry + 34.0, &items);
    ry += 68.0;
    // row: hold time
    divider(dc, x0, cw, ry);
    dc.row_text(x0 + 20.0, ry + 14.0, text_w, t("Hold for", "누르고 있을 시간"), t("Shorter shows faster; longer avoids surprises", "짧을수록 빨리 뜨고, 길수록 실수로 뜨지 않아요"));
    let frac = (s.s.hold_ms as f32 - 300.0) / 1700.0;
    let tr = dc.slider(x0 + cw - 250.0, ry + 34.0, 170.0, frac.clamp(0.0, 1.0), &format!("{:.1}{}", s.s.hold_ms as f32 / 1000.0, t(" s", "초")), Act::HoldSlider);
    sliders.push((Act::HoldSlider, tr));
    ry += 68.0;
    // row: full screen
    divider(dc, x0, cw, ry);
    dc.row_text(x0 + 20.0, ry + 14.0, text_w, t("Not over full-screen apps and games", "전체 화면 앱·게임 위에는 띄우지 않기"), t("Holding Ctrl to crouch won't cover the game", "게임에서 Ctrl(앉기)을 오래 눌러도 가리지 않아요"));
    dc.toggle(x0 + cw - 20.0, ry + 34.0, s.s.skip_fullscreen, Act::SkipFs);
    y = c1.y + c1.h + 14.0;

    // Card 2: capture
    let (_, mh) = if u.learn_msg.is_empty() { (0.0, 0.0) } else { dc.measure(&u.learn_msg, 13.0, true, cw - 40.0, true) };
    let c2 = R::new(x0, y, cw, 118.0 + if mh > 0.0 { mh + 12.0 } else { 0.0 });
    dc.card(c2);
    dc.text(t("Does my Fn key send anything?", "내 Fn 키가 신호를 보내나요?"), x0 + 20.0, y + 16.0, 14.0, true, p.text, cw - 40.0, false, Align::Left);
    dc.text(t("Press Start, then hold Fn alone for two seconds. If anything arrives you can use it as the key.", "시작을 누르고 Fn 키만 2초 동안 눌러 보세요. 신호가 오면 그 키를 누를 키로 쓸 수 있어요."), x0 + 20.0, y + 38.0, 12.5, false, p.sub, cw - 40.0, true, Align::Left);
    let bx = x0 + 20.0;
    let w1 = dc.button(bx, y + 70.0, if u.learning { t("Listening…", "듣는 중…") } else { t("Start", "시작") }, Some(G_KEYBOARD), Act::LearnStart, 1, !u.learning);
    dc.button(bx + w1 + 8.0, y + 70.0, t("Use this key", "이 키로 쓰기"), None, Act::LearnUse, 0, u.learn_ready);
    if mh > 0.0 {
        let tone = match u.learn_tone {
            1 => p.good,
            2 => p.warn,
            _ => p.text,
        };
        dc.text(&u.learn_msg, x0 + 20.0, y + 114.0, 13.0, true, tone, cw - 40.0, true, Align::Left);
    }
    y = c2.y + c2.h + 14.0;

    // Card 3: general
    let c3 = R::new(x0, y, cw, 4.0 * 68.0 + 50.0);
    dc.card(c3);
    dc.text(t("General", "일반"), x0 + 20.0, y + 16.0, 14.0, true, p.text, cw - 40.0, false, Align::Left);
    let mut ry = y + 50.0;
    divider(dc, x0, cw, ry);
    dc.row_text(x0 + 20.0, ry + 14.0, text_w, t("Start with Windows", "Windows 시작 시 실행"), t("Stays in the tray, uses a few MB", "트레이에 머물고 메모리는 몇 MB만 써요"));
    dc.toggle(x0 + cw - 20.0, ry + 34.0, startup::is_enabled(), Act::Startup);
    ry += 68.0;
    divider(dc, x0, cw, ry);
    dc.row_text(x0 + 20.0, ry + 14.0, cw - 360.0, t("Language", "언어"), "");
    let lang = s.s.language.as_str();
    dc.segmented(x0 + cw - 20.0, ry + 34.0, &[(t("Windows", "Windows 따라"), Act::Lang(0), lang == "auto"), ("한국어", Act::Lang(1), lang == "ko"), ("English", Act::Lang(2), lang == "en")]);
    ry += 68.0;
    divider(dc, x0, cw, ry);
    dc.row_text(x0 + 20.0, ry + 14.0, cw - 360.0, t("Theme", "테마"), t("For the sheet and this window", "치트시트와 이 창에 적용돼요"));
    let th = s.s.theme.as_str();
    dc.segmented(x0 + cw - 20.0, ry + 34.0, &[(t("Windows", "Windows 따라"), Act::Theme(0), th == "auto"), (t("Dark", "어둡게"), Act::Theme(1), th == "dark"), (t("Light", "밝게"), Act::Theme(2), th == "light")]);
    ry += 68.0;
    divider(dc, x0, cw, ry);
    dc.row_text(x0 + 20.0, ry + 14.0, cw - 470.0, t("Sheet size", "치트시트 크기"), "");
    let frac = (s.s.overlay_scale - 0.7) / 0.9;
    let tr = dc.slider(x0 + cw - 380.0, ry + 34.0, 170.0, frac.clamp(0.0, 1.0), &format!("{}%", (s.s.overlay_scale * 100.0).round()), Act::ScaleSlider);
    sliders.push((Act::ScaleSlider, tr));
    dc.button(x0 + cw - 20.0 - 116.0, ry + 17.0, t("Preview", "미리 보기"), Some(G_VIEW), Act::Preview, 0, true);
    y = c3.y + c3.h + 28.0;
    (y - y0, sliders)
}

fn page_about(dc: &mut Dc, s: &Snap, x0: f32, cw: f32, y0: f32, icon: Option<&ID2D1Bitmap>) -> f32 {
    let p = dc.p;
    let mut y = y0 + 28.0;
    // hero
    if let Some(bmp) = icon {
        unsafe {
            dc.rt.DrawBitmap(bmp, Some(&R::new(x0, y, 64.0, 64.0).d2d()), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
        }
    }
    dc.text("Fn Cheatsheet", x0 + 80.0, y + 2.0, 24.0, true, p.text, cw - 80.0, false, Align::Left);
    dc.text(
        &format!("{} {}  ·  {} {} · {}", t("Version", "버전"), APP_VERSION, s.count, t("models", "개 모델"), s.data_version),
        x0 + 80.0,
        y + 36.0,
        12.5,
        false,
        p.sub,
        cw - 80.0,
        false,
        Align::Left,
    );
    y += 84.0;

    // Updates
    let c1 = R::new(x0, y, cw, 50.0 + 2.0 * 68.0);
    dc.card(c1);
    dc.glyph(G_SYNC, x0 + 20.0, y + 18.0, 14.0, p.text);
    dc.text(t("Updates", "업데이트"), x0 + 44.0, y + 15.0, 14.0, true, p.text, cw - 60.0, false, Align::Left);
    let mut ry = y + 50.0;
    divider(dc, x0, cw, ry);
    dc.row_text(x0 + 20.0, ry + 14.0, cw - 200.0, t("Update automatically", "자동 업데이트"), t("Checks GitHub once a day; verified before installing", "하루 한 번 GitHub를 확인하고, 검증한 뒤 설치해요"));
    dc.toggle(x0 + cw - 20.0, ry + 34.0, s.s.auto_update, Act::AutoUpdate);
    ry += 68.0;
    divider(dc, x0, cw, ry);
    let status = if s.update_status.is_empty() { t("Not checked yet in this session", "이번 실행에서는 아직 확인하지 않았어요").to_string() } else { s.update_status.clone() };
    dc.row_text(x0 + 20.0, ry + 14.0, cw - 220.0, t("Status", "상태"), &status);
    let label = if s.update_busy { t("Checking…", "확인 중…") } else { t("Check now", "지금 확인") };
    let lw = dc.measure(label, 13.5, false, 600.0, false).0 + 50.0;
    dc.button(x0 + cw - 20.0 - lw.max(96.0), ry + 17.0, label, Some(G_SYNC), Act::CheckNow, 0, !s.update_busy);
    y = c1.y + c1.h + 14.0;

    // Privacy
    let ptxt = t(
        "Network is used only for the update check (api.github.com, github.com). Only your PC's model strings are read — no serial numbers. The keyboard hook only checks whether the trigger key is held alone and never records what you type. Never runs as administrator.",
        "네트워크는 업데이트 확인(api.github.com, github.com)에만 써요. PC 모델 문자열만 읽고 일련번호는 읽지 않아요. 키보드 후킹은 트리거 키를 혼자 누르고 있는지만 보고, 입력 내용은 기록하지 않아요. 관리자 권한으로 실행하지 않아요.",
    );
    let (_, ph) = dc.measure(ptxt, 12.5, false, cw - 40.0, true);
    let c2 = R::new(x0, y, cw, 50.0 + ph + 16.0);
    dc.card(c2);
    dc.glyph(G_SHIELD, x0 + 20.0, y + 18.0, 14.0, p.text);
    dc.text(t("Privacy", "개인정보"), x0 + 44.0, y + 15.0, 14.0, true, p.text, cw - 60.0, false, Align::Left);
    dc.text(ptxt, x0 + 20.0, y + 44.0, 12.5, false, p.sub, cw - 40.0, true, Align::Left);
    y = c2.y + c2.h + 14.0;

    // Sources
    let notes_h = if s.notes.is_empty() { 0.0 } else { dc.measure(&s.notes, 12.0, false, cw - 40.0, true).1 + 12.0 };
    let n = s.sources.len().max(1) as f32;
    let c3 = R::new(x0, y, cw, 50.0 + n * 36.0 + notes_h + 10.0);
    dc.card(c3);
    dc.text(t("Sources for the model in use", "사용 중인 모델의 출처"), x0 + 20.0, y + 15.0, 14.0, true, p.text, cw - 40.0, false, Align::Left);
    let mut ry = y + 46.0;
    if s.sources.is_empty() {
        dc.text(t("No model selected.", "선택된 모델이 없어요."), x0 + 20.0, ry + 6.0, 13.0, false, p.sub, cw - 40.0, false, Align::Left);
        ry += 36.0;
    }
    unsafe {
        dc.rt.PushAxisAlignedClip(&R::new(x0 + 1.0, y, cw - 2.0, c3.h).d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
    }
    for (i, (title, url, kind)) in s.sources.iter().enumerate() {
        let r = R::new(x0 + 10.0, ry, cw - 20.0, 32.0);
        let a = Act::Source(i);
        let (hov, _) = dc.state(a);
        if hov {
            dc.fill(r, 4.0, p.hover);
        }
        dc.glyph(G_LINK, r.x + 10.0, r.y + 9.0, 13.0, p.accent);
        let label = format!("{}{}", if title.is_empty() { url } else { title }, match kind.as_str() { "community" => t("  (community)", "  (커뮤니티)"), "device-test" => t("  (tested on a real device)", "  (실기 확인)"), _ => "" });
        dc.text(&label, r.x + 34.0, r.y + 6.5, 13.0, false, p.accent, r.w - 44.0, false, Align::Left);
        dc.hit(r, a);
        ry += 36.0;
    }
    unsafe {
        dc.rt.PopAxisAlignedClip();
    }
    if !s.notes.is_empty() {
        dc.text(&s.notes, x0 + 20.0, ry + 4.0, 12.0, false, p.sub, cw - 40.0, true, Align::Left);
    }
    y = c3.y + c3.h + 14.0;

    // Links
    let mut bx = x0;
    bx += dc.button(bx, y, t("Website & model list", "소개 페이지·모델 목록"), Some(G_LINK), Act::Site, 2, true) + 4.0;
    bx += dc.button(bx, y, "GitHub", Some(G_LINK), Act::Github, 2, true) + 4.0;
    dc.button(bx, y, t("Report a problem", "문제 신고"), Some(G_LINK), Act::Issues, 2, true);
    y += 44.0;
    dc.text(t("MIT License · © 2026 jeiel85 · Not affiliated with ASUS or Lenovo.", "MIT License · © 2026 jeiel85 · ASUS·Lenovo와 관계없는 비공식 프로젝트예요."), x0, y, 12.0, false, p.sub, cw, false, Align::Left);
    y + 40.0 - y0
}

// ───────────────────────── paint ─────────────────────────

fn icon_source(px: i32) -> Option<IWICFormatConverter> {
    unsafe {
        let hinst = GetModuleHandleW(None).ok()?;
        let h = LoadImageW(Some(hinst.into()), PCWSTR(1 as _), IMAGE_ICON, px, px, LR_DEFAULTCOLOR).ok()?;
        let hicon = HICON(h.0);
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let bmp = wic.CreateBitmapFromHICON(hicon).ok();
        let _ = DestroyIcon(hicon);
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(&bmp?, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeMedianCut).ok()?;
        Some(conv)
    }
}

fn paint(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let (pw, ph) = (rc.right - rc.left, rc.bottom - rc.top);
        if pw > 0 && ph > 0 {
            if let Err(e) = paint_into(hwnd, hdc, pw, ph) {
                crate::util::log(&format!("settings paint failed: {e}"));
            }
        }
        let _ = EndPaint(hwnd, &ps);
    }
}

fn paint_into(hwnd: HWND, hdc: HDC, pw: i32, ph: i32) -> Result<()> {
    let Some((dpi, tab, hover, pressed, view, page_scroll, icon_src)) = ui(|u| {
        (
            u.dpi,
            u.tab,
            u.hover,
            u.pressed,
            UiView {
                query: u.query.clone(),
                list_scroll: u.list_scroll,
                details: u.details,
                learn_msg: u.learn_msg.clone(),
                learn_tone: u.learn_tone,
                learn_ready: u.learned.map(|(vk, _, _)| !is_typing_key(vk)).unwrap_or(false) && !u.learning,
                learning: u.learning,
            },
            u.page_scroll,
            u.icon.clone(),
        )
    }) else {
        return Ok(());
    };
    let Some(snap) = snapshot(&view.query) else { return Ok(()) };
    let row_order: Vec<usize> = snap.rows.iter().map(|r| r.idx).collect();
    let (focus, focus_visible) = ui(|u| (u.focus, u.focus_visible)).unwrap_or((None, false));
    let pal = pal_for(&snap.s.theme);
    let sc = dpi as f32 / 96.0;
    let (wd, hd) = (pw as f32 / sc, ph as f32 / sc);

    unsafe {
        let mem = CreateCompatibleDC(Some(hdc));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: pw, biHeight: -ph, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let dib = CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let old = SelectObject(mem, dib.into());

        let mut out: Option<(Vec<(R, Act)>, R, R, f32, f32, Vec<(Act, R)>)> = None;
        let res = with_gfx(|g| {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: dpi as f32,
                dpiY: dpi as f32,
                usage: D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let dcrt = g.d2d.CreateDCRenderTarget(&props)?;
            dcrt.BindDC(mem, &RECT { left: 0, top: 0, right: pw, bottom: ph })?;
            let rt: ID2D1RenderTarget = dcrt.cast()?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
            let icon = icon_src.as_ref().and_then(|src| rt.CreateBitmapFromWicBitmap(src, None).ok());
            rt.BeginDraw();
            rt.Clear(Some(&color(pal.bg)));
            let brush = rt.CreateSolidColorBrush(&color(pal.text), None)?;
            let mut dc = Dc { rt: &rt, g, brush, p: pal, hits: Vec::new(), hover, pressed };
            sidebar(&mut dc, tab, icon.as_ref());
            let x0 = SIDEBAR + 8.0;
            let cw = wd - x0 - PAD;
            let mut search = R::default();
            let mut list = R::default();
            let mut list_h = 0.0;
            let mut page_h = hd;
            let mut sliders = Vec::new();
            // content area clip + page scroll
            rt.PushAxisAlignedClip(&R::new(SIDEBAR, 0.0, wd - SIDEBAR, hd).d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
            match tab {
                Tab::Device => {
                    let (s, l, lh) = page_device(&mut dc, &snap, &view, x0, cw, hd);
                    search = s;
                    list = l;
                    list_h = lh;
                }
                Tab::Trigger => {
                    let (h, sl) = page_trigger(&mut dc, &snap, &view, x0, cw, -page_scroll);
                    page_h = h;
                    sliders = sl;
                }
                Tab::About => {
                    page_h = page_about(&mut dc, &snap, x0, cw, -page_scroll, icon.as_ref());
                }
            }
            rt.PopAxisAlignedClip();
            // page scrollbar
            if tab != Tab::Device && page_h > hd {
                let th = (hd * hd / page_h).max(40.0);
                let ty = (hd - th) * (page_scroll / (page_h - hd)).clamp(0.0, 1.0);
                dc.fill(R::new(wd - 6.0, ty + 2.0, 3.0, th - 4.0), 1.5, pal.track_off & 0x99FFFFFF);
            }
            if focus_visible {
                if let Some((r, _)) = dc.hits.iter().find(|(_, a)| Some(*a) == focus) {
                    let ring = R::new(r.x - 3.0, r.y - 3.0, r.w + 6.0, r.h + 6.0);
                    dc.stroke(ring, 7.0, pal.text, 2.0);
                }
            }
            out = Some((std::mem::take(&mut dc.hits), search, list, list_h, page_h, sliders));
            rt.EndDraw(None, None)
        });
        if res.is_ok() {
            let _ = BitBlt(hdc, 0, 0, pw, ph, Some(mem), 0, 0, SRCCOPY);
        }
        SelectObject(mem, old);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(mem);

        if let Some((hits, search, list, list_h, page_h, sliders)) = out {
            let edit = ui(|u| {
                u.hits = hits;
                u.row_order = row_order.clone();
                u.list_rect = list;
                u.list_h = list_h;
                u.page_h = page_h;
                u.pal = pal;
                if let Some((a, _)) = u.dragging {
                    if let Some((_, r)) = sliders.iter().find(|(x, _)| *x == a) {
                        u.dragging = Some((a, *r));
                    }
                }
                let changed = (search.x - u.search_rect.x).abs() > 0.1 || (search.y - u.search_rect.y).abs() > 0.1 || (search.w - u.search_rect.w).abs() > 0.1;
                u.search_rect = search;
                (u.search, changed, tab == Tab::Device)
            });
            if let Some((edit, changed, show)) = edit {
                if changed && show {
                    let _ = SetWindowPos(
                        edit,
                        None,
                        ((search.x + 12.0) * sc) as i32,
                        ((search.y + 8.0) * sc) as i32,
                        ((search.w - 48.0) * sc) as i32,
                        (20.0 * sc) as i32,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                let visible = IsWindowVisible(edit).as_bool();
                if show != visible {
                    let _ = ShowWindow(edit, if show { SW_SHOWNA } else { SW_HIDE });
                }
            }
        }
        let _ = hwnd;
        res
    }
}

fn is_typing_key(vk: u32) -> bool {
    matches!(vk, 0x08 | 0x09 | 0x0D | 0x20) || (0x30..=0x5A).contains(&vk)
}

// ───────────────────────── window ─────────────────────────

pub fn open(tab: Option<Tab>) {
    if let Some(h) = ui(|u| u.hwnd) {
        if let Some(t) = tab {
            ui(|u| {
                u.tab = t;
                u.page_scroll = 0.0;
            });
            repaint();
        }
        unsafe {
            let _ = ShowWindow(h, SW_RESTORE);
            let _ = SetForegroundWindow(h);
        }
        return;
    }
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(proc_),
            hInstance: hinst.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: LoadImageW(Some(hinst.into()), PCWSTR(1 as _), IMAGE_ICON, 0, 0, LR_DEFAULTSIZE).map(|h| HICON(h.0)).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let title = w(&format!("Fn Cheatsheet — {}", t("Settings", "설정")));
        let Ok(hwnd) = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, title.pcwstr(), WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN, CW_USEDEFAULT, CW_USEDEFAULT, 100, 100, None, None, Some(hinst.into()), None) else {
            return;
        };
        let dpi = GetDpiForWindow(hwnd).max(96);
        let search = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            wlit!("EDIT"),
            PCWSTR::null(),
            WINDOW_STYLE(ES_AUTOHSCROLL as u32) | WS_CHILD | WS_TABSTOP,
            0, 0, 10, 10,
            Some(hwnd),
            Some(HMENU(ID_SEARCH as isize as _)),
            None,
            None,
        )
        .unwrap_or_default();
        // Placeholder text drawn by the edit control itself (EM_SETCUEBANNER).
        let cue = w(t("Search model, code or machine type (e.g. P1503, T14 Gen 4, 21HM)", "모델명·모델 코드·머신 타입 검색 (예: P1503, T14 Gen 4, 21HM)"));
        SendMessageW(search, 0x1501 /* EM_SETCUEBANNER */, Some(WPARAM(1)), Some(LPARAM(cue.pcwstr().0 as isize)));
        let theme = app::with(|a| a.settings.theme.clone()).unwrap_or_default();
        let pal = pal_for(&theme);
        UI.with(|u| {
            *u.borrow_mut() = Some(Ui {
                hwnd, dpi, tab: tab.unwrap_or(Tab::Device), hits: vec![], hover: None, pressed: None, dragging: None, tracking: false,
                search, search_font: HFONT::default(), search_brush: HBRUSH::default(), search_rect: R::default(), query: String::new(),
                list_rect: R::default(), list_scroll: 0.0, list_h: 0.0, page_scroll: 0.0, page_h: 0.0, details: false,
                learning: false, learned: None, learn_saw: false, learn_msg: String::new(), learn_tone: 0,
                icon: icon_source((64.0 * dpi as f32 / 96.0) as i32), pal, focus: None, focus_visible: false, row_order: vec![],
            })
        });
        apply_dpi_and_theme(hwnd, dpi, None);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Window size, search font/brush and title-bar colors for the current DPI and theme.
fn apply_dpi_and_theme(hwnd: HWND, dpi: u32, suggested: Option<&RECT>) {
    let theme = app::with(|a| a.settings.theme.clone()).unwrap_or_default();
    let pal = pal_for(&theme);
    unsafe {
        // title bar matches the window background
        let dark: i32 = pal.dark as i32;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
        let cap = colorref(pal.bg);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &cap as *const _ as _, 4);
        let border = colorref(pal.card_border);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as _, 4);

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut r = RECT { left: 0, top: 0, right: (W_DIP * dpi as f32 / 96.0) as i32, bottom: (H_DIP * dpi as f32 / 96.0) as i32 };
        let _ = AdjustWindowRectExForDpi(&mut r, style, false, WINDOW_EX_STYLE(0), dpi);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        match suggested {
            Some(s) => {
                let _ = SetWindowPos(hwnd, None, s.left, s.top, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            None => {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
                let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
                let _ = GetMonitorInfoW(mon, &mut mi);
                let wa = mi.rcWork;
                let _ = SetWindowPos(hwnd, None, wa.left + ((wa.right - wa.left - w) / 2).max(0), wa.top + ((wa.bottom - wa.top - h) / 2).max(0), w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }

        // search box font and colors
        let mut lf = LOGFONTW { lfHeight: -((13.0 * dpi as f32 / 96.0).round() as i32), lfWeight: 400, lfCharSet: DEFAULT_CHARSET, lfQuality: CLEARTYPE_QUALITY, ..Default::default() };
        let face: Vec<u16> = FONT.encode_utf16().collect();
        lf.lfFaceName[..face.len()].copy_from_slice(&face);
        let font = CreateFontIndirectW(&lf);
        let brush = CreateSolidBrush(colorref(pal.control));
        let old = ui(|u| {
            u.dpi = dpi;
            u.pal = pal;
            u.search_rect = R::default(); // force reposition
            let old = (u.search_font, u.search_brush, u.search);
            u.search_font = font;
            u.search_brush = brush;
            old
        });
        if let Some((of, ob, edit)) = old {
            SendMessageW(edit, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            if !of.0.is_null() {
                let _ = DeleteObject(of.into());
            }
            if !ob.0.is_null() {
                let _ = DeleteObject(ob.into());
            }
        }
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn hit_at(lp: LPARAM) -> (f32, f32, Option<Act>) {
    let sc = scale();
    let x = (lp.0 & 0xFFFF) as i16 as f32 / sc;
    let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / sc;
    let a = ui(|u| u.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, a)| *a)).flatten();
    (x, y, a)
}

fn slider_value(act: Act, frac: f32) {
    let frac = frac.clamp(0.0, 1.0);
    app::with(|a| match act {
        Act::HoldSlider => a.settings.hold_ms = ((300.0 + frac * 1700.0) / 100.0).round() as u32 * 100,
        Act::ScaleSlider => a.settings.overlay_scale = ((0.7 + frac * 0.9) * 10.0).round() / 10.0,
        _ => {}
    });
}

fn act(a: Act) {
    let hwnd = ui(|u| u.hwnd).unwrap_or_default();
    match a {
        Act::Nav(t) => {
            ui(|u| {
                u.tab = t;
                u.page_scroll = 0.0;
            });
        }
        Act::CopyInfo => {
            let r = app::with(|a| a.device.report()).unwrap_or_default();
            copy_to_clipboard(hwnd, &r);
            app::balloon("Fn Cheatsheet", t("Device info copied", "기기 정보를 복사했어요"));
        }
        Act::Request => request_model(),
        Act::Details => {
            ui(|u| u.details = !u.details);
        }
        Act::ModeAuto => {
            app::with(|a| a.settings.profile_mode = "auto".into());
            app::apply_settings();
        }
        Act::ModeManual => {
            app::with(|a| {
                a.settings.profile_mode = "manual".into();
                if a.db.find(&a.settings.manual_profile_id).is_none() {
                    let pick = a.active_index().or_else(|| a.detection.candidates.first().map(|c| c.index));
                    if let Some(i) = pick {
                        a.settings.manual_profile_id = a.db.profiles[i].id.clone();
                    }
                }
            });
            app::apply_settings();
        }
        Act::Row(i) => {
            app::with(|a| {
                a.settings.profile_mode = "manual".into();
                a.settings.manual_profile_id = a.db.profiles[i].id.clone();
            });
            app::apply_settings();
        }
        Act::Preview => app::show_overlay(true),
        Act::Folder => open_profiles_folder(),
        Act::Reload => {
            app::with(|a| a.reload_profiles());
            app::update_tray_tip();
        }
        Act::TriggerEnabled => {
            app::with(|a| a.settings.trigger_enabled = !a.settings.trigger_enabled);
            app::apply_settings();
        }
        Act::SkipFs => {
            app::with(|a| a.settings.skip_fullscreen = !a.settings.skip_fullscreen);
            app::apply_settings();
        }
        Act::Key(vk) => {
            if vk != 0 {
                app::with(|a| {
                    a.settings.trigger_vk = vk;
                    a.settings.trigger_scan = 0;
                    a.settings.trigger_extended = false;
                });
                app::apply_settings();
            }
        }
        Act::LearnStart => start_learning(),
        Act::LearnUse => use_learned(),
        Act::Startup => startup::set(!startup::is_enabled()),
        Act::Lang(i) => {
            app::with(|a| a.settings.language = ["auto", "ko", "en"][i as usize].into());
            app::apply_settings();
            unsafe {
                let title = w(&format!("Fn Cheatsheet — {}", t("Settings", "설정")));
                let _ = SetWindowTextW(hwnd, title.pcwstr());
            }
        }
        Act::Theme(i) => {
            app::with(|a| a.settings.theme = ["auto", "dark", "light"][i as usize].into());
            app::apply_settings();
            let dpi = ui(|u| u.dpi).unwrap_or(96);
            let mut wr = RECT::default();
            unsafe {
                let _ = GetWindowRect(hwnd, &mut wr);
            }
            apply_dpi_and_theme(hwnd, dpi, Some(&wr));
        }
        Act::AutoUpdate => {
            app::with(|a| a.settings.auto_update = !a.settings.auto_update);
            app::apply_settings();
        }
        Act::CheckNow => {
            if let Some(main) = app::with(|a| a.hwnd) {
                update::check_now(main, true);
            }
        }
        Act::Site => crate::util::open_url(SITE_URL),
        Act::Github => crate::util::open_url(REPO_URL),
        Act::Issues => crate::util::open_url(&format!("{REPO_URL}/issues")),
        Act::Source(i) => {
            let url = app::with(|a| a.active().and_then(|p| p.sources.get(i).map(|s| s.url.clone()))).flatten();
            if let Some(u) = url {
                crate::util::open_url(&u);
            }
        }
        Act::Search => {
            if let Some(e) = ui(|u| u.search) {
                unsafe {
                    let _ = SetFocus(Some(e));
                }
            }
        }
        Act::HoldSlider | Act::ScaleSlider => {}
    }
    repaint();
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            let (x, y, a) = hit_at(lp);
            let (changed, track, drag) = ui(|u| {
                let changed = u.hover != a;
                u.hover = a;
                let track = !u.tracking;
                u.tracking = true;
                (changed, track, u.dragging)
            })
            .unwrap_or((false, false, None));
            let _ = y;
            if let Some((da, r)) = drag {
                slider_value(da, (x - r.x) / r.w);
                repaint();
            } else if changed {
                repaint();
            }
            if track {
                let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                let _ = TrackMouseEvent(&mut tme);
            }
            LRESULT(0)
        }
        0x02A3 /* WM_MOUSELEAVE */ => {
            ui(|u| {
                u.hover = None;
                u.tracking = false;
            });
            repaint();
            LRESULT(0)
        }
        WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
            let hand = ui(|u| u.hover.is_some()).unwrap_or(false);
            let _ = SetCursor(LoadCursorW(None, if hand { IDC_HAND } else { IDC_ARROW }).ok());
            LRESULT(1)
        }
        WM_LBUTTONDOWN => {
            let (x, _, a) = hit_at(lp);
            let _ = SetFocus(Some(hwnd));
            ui(|u| {
                u.focus = a;
                u.focus_visible = false;
            });
            if let Some(a) = a {
                let slider = matches!(a, Act::HoldSlider | Act::ScaleSlider);
                let rect = ui(|u| {
                    u.pressed = Some(a);
                    u.hits.iter().find(|(_, h)| *h == a).map(|(r, _)| *r)
                })
                .flatten();
                SetCapture(hwnd);
                if slider {
                    if let Some(r) = rect {
                        // hit rect is the track widened by 10 DIP on each side
                        let track = R::new(r.x + 10.0, r.y, r.w - 20.0, r.h);
                        ui(|u| u.dragging = Some((a, track)));
                        slider_value(a, (x - track.x) / track.w);
                    }
                }
                repaint();
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (_, _, a) = hit_at(lp);
            let (pressed, drag) = ui(|u| (u.pressed.take(), u.dragging.take())).unwrap_or((None, None));
            let _ = ReleaseCapture();
            if drag.is_some() {
                app::apply_settings();
                repaint();
            } else if let (Some(p), Some(a)) = (pressed, a) {
                if p == a {
                    act(a);
                }
            } else {
                repaint();
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as f32;
            let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt);
            let sc = scale();
            let (x, y) = (pt.x as f32 / sc, pt.y as f32 / sc);
            let step = -delta / 120.0 * 54.0 * 1.5;
            ui(|u| {
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let hd = (rc.bottom - rc.top) as f32 / sc;
                if u.tab == Tab::Device && u.list_rect.contains(x, y) {
                    u.list_scroll = (u.list_scroll + step).clamp(0.0, (u.list_h - u.list_rect.h).max(0.0));
                } else if u.tab != Tab::Device {
                    u.page_scroll = (u.page_scroll + step).clamp(0.0, (u.page_h - hd).max(0.0));
                }
            });
            repaint();
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wp.0 & 0xFFFF) as i32;
            let code = ((wp.0 >> 16) & 0xFFFF) as u32;
            if id == ID_SEARCH {
                if code == EN_CHANGE {
                    let h = ui(|u| u.search).unwrap_or_default();
                    let mut buf = vec![0u16; 256];
                    let n = GetWindowTextW(h, &mut buf);
                    let q = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
                    ui(|u| {
                        u.query = q;
                        u.list_scroll = 0.0;
                    });
                }
                repaint(); // focus changes too (placeholder)
            }
            LRESULT(0)
        }
        WM_CTLCOLOREDIT => {
            let Some((brush, pal)) = ui(|u| (u.search_brush, u.pal)) else { return DefWindowProcW(hwnd, msg, wp, lp) };
            let hdc = HDC(wp.0 as _);
            SetTextColor(hdc, colorref(pal.text));
            SetBkColor(hdc, colorref(pal.control));
            LRESULT(brush.0 as isize)
        }
        WM_KEYDOWN if wp.0 as u16 == VK_ESCAPE.0 => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if on_key(wp.0 as u16) { LRESULT(0) } else { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_GETDLGCODE => LRESULT((DLGC_WANTALLKEYS | DLGC_WANTARROWS | DLGC_WANTTAB) as isize),
        WM_TIMER if wp.0 == TIMER_LEARN => {
            finish_learning();
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let dpi = (wp.0 & 0xFFFF) as u32;
            ui(|u| u.icon = icon_source((64.0 * dpi as f32 / 96.0) as i32));
            apply_dpi_and_theme(hwnd, dpi, Some(&*(lp.0 as *const RECT)));
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            // Windows light/dark switch
            let dpi = ui(|u| u.dpi).unwrap_or(96);
            let mut wr = RECT::default();
            let _ = GetWindowRect(hwnd, &mut wr);
            apply_dpi_and_theme(hwnd, dpi, Some(&wr));
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            hook::set_learning(false);
            let res = ui(|u| (u.search_font, u.search_brush));
            UI.with(|u| {
                if let Ok(mut u) = u.try_borrow_mut() {
                    *u = None;
                }
            });
            if let Some((f, b)) = res {
                if !f.0.is_null() {
                    let _ = DeleteObject(f.into());
                }
                if !b.0.is_null() {
                    let _ = DeleteObject(b.into());
                }
            }
            crate::overlay::shutdown();
            app::update_tray_tip();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// ───────────────────────── helpers kept from the classic UI ─────────────────────────

/// Keyboard navigation over the drawn controls. Returns true when the key was handled.
fn on_key(vk: u16) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_DOWN, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP};
    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
    let Some((hits, focus, tab, rows, hwnd, edit)) = ui(|u| (u.hits.clone(), u.focus, u.tab, u.row_order.clone(), u.hwnd, u.search)) else { return false };

    // Ctrl+Tab / Ctrl+PgDn switch pages.
    if ctrl && (vk == VK_TAB.0 || vk == VK_NEXT.0 || vk == VK_PRIOR.0) {
        let back = shift || vk == VK_PRIOR.0;
        let next = match (tab, back) {
            (Tab::Device, false) | (Tab::About, true) => Tab::Trigger,
            (Tab::Trigger, false) | (Tab::Device, true) => Tab::About,
            _ => Tab::Device,
        };
        ui(|u| {
            u.tab = next;
            u.page_scroll = 0.0;
            u.focus = Some(Act::Nav(next));
            u.focus_visible = true;
        });
        repaint();
        return true;
    }

    // Tab order = paint order; the model list is a single stop (its focused or active row).
    let active_row = rows.iter().copied().find(|i| hits.iter().any(|(_, a)| *a == Act::Row(*i)));
    let mut order: Vec<Act> = Vec::new();
    let mut row_done = false;
    for (_, a) in &hits {
        match a {
            Act::Row(_) => {
                if !row_done {
                    row_done = true;
                    order.push(match focus {
                        Some(Act::Row(f)) => Act::Row(f),
                        _ => active_row.map(Act::Row).unwrap_or(*a),
                    });
                }
            }
            _ if !order.contains(a) => order.push(*a),
            _ => {}
        }
    }
    if order.is_empty() {
        return false;
    }
    let pos = focus.and_then(|f| order.iter().position(|a| *a == f));
    let move_to = |a: Act| {
        ui(|u| {
            u.focus = Some(a);
            u.focus_visible = true;
        });
        unsafe {
            let _ = SetFocus(Some(if a == Act::Search { edit } else { hwnd }));
        }
        scroll_into_view(a);
        repaint();
    };

    match vk {
        v if v == VK_TAB.0 => {
            let n = order.len();
            let i = match pos {
                Some(p) if shift => (p + n - 1) % n,
                Some(p) => (p + 1) % n,
                None if shift => n - 1,
                None => 0,
            };
            move_to(order[i]);
            true
        }
        v if v == VK_RETURN.0 || v == VK_SPACE.0 => {
            if let Some(f) = focus {
                ui(|u| u.focus_visible = true);
                act(f);
            }
            true
        }
        v if v == VK_UP.0 || v == VK_DOWN.0 => {
            if let Some(Act::Row(cur)) = focus {
                let k = rows.iter().position(|i| *i == cur).unwrap_or(0);
                let k2 = if v == VK_UP.0 { k.saturating_sub(1) } else { (k + 1).min(rows.len().saturating_sub(1)) };
                if let Some(&i) = rows.get(k2) {
                    move_to(Act::Row(i));
                }
            } else if let Some(p) = pos {
                let n = order.len();
                move_to(order[if v == VK_UP.0 { (p + n - 1) % n } else { (p + 1) % n }]);
            }
            true
        }
        v if v == VK_LEFT.0 || v == VK_RIGHT.0 => {
            let dir = if v == VK_LEFT.0 { -1.0 } else { 1.0 };
            match focus {
                Some(Act::HoldSlider) => {
                    app::with(|a| a.settings.hold_ms = (a.settings.hold_ms as i32 + dir as i32 * 100).clamp(300, 2000) as u32);
                    app::apply_settings();
                    repaint();
                }
                Some(Act::ScaleSlider) => {
                    app::with(|a| a.settings.overlay_scale = ((a.settings.overlay_scale + dir * 0.1) * 10.0).round().clamp(7.0, 16.0) / 10.0);
                    app::apply_settings();
                    repaint();
                }
                Some(_) => {
                    if let Some(p) = pos {
                        let n = order.len();
                        move_to(order[if dir < 0.0 { (p + n - 1) % n } else { (p + 1) % n }]);
                    }
                }
                None => move_to(order[0]),
            }
            true
        }
        _ => false,
    }
}

/// Scrolls the model list or the page so the focused control is visible.
fn scroll_into_view(a: Act) {
    let hd = ui(|u| {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(u.hwnd, &mut rc);
        }
        (rc.bottom - rc.top) as f32 / (u.dpi as f32 / 96.0)
    })
    .unwrap_or(H_DIP);
    ui(|u| {
        if let Act::Row(i) = a {
            if let Some(k) = u.row_order.iter().position(|x| *x == i) {
                let top = k as f32 * 54.0;
                if top < u.list_scroll {
                    u.list_scroll = top;
                } else if top + 54.0 > u.list_scroll + u.list_rect.h {
                    u.list_scroll = top + 54.0 - u.list_rect.h;
                }
            }
            return;
        }
        if u.tab == Tab::Device {
            return;
        }
        if let Some((r, _)) = u.hits.iter().find(|(_, h)| *h == a) {
            let max = (u.page_h - hd).max(0.0);
            if r.y < 16.0 {
                u.page_scroll = (u.page_scroll + r.y - 24.0).clamp(0.0, max);
            } else if r.y + r.h > hd - 16.0 {
                u.page_scroll = (u.page_scroll + r.y + r.h - hd + 24.0).clamp(0.0, max);
            }
        }
    });
}

pub fn is_dialog_message(msg: &MSG) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_TAB};
    if msg.message == WM_KEYDOWN && (msg.wParam.0 as u16 == VK_TAB.0 || msg.wParam.0 as u16 == VK_DOWN.0) {
        if let Some((h, edit)) = ui(|u| (u.hwnd, u.search)) {
            if msg.hwnd == edit {
                // Leave the search box: Tab continues the tab order, Down jumps into the list.
                ui(|u| u.focus = Some(Act::Search));
                unsafe {
                    let _ = SetFocus(Some(h));
                }
                if msg.wParam.0 as u16 == VK_DOWN.0 {
                    let first = ui(|u| u.row_order.first().copied()).flatten();
                    if let Some(i) = first {
                        ui(|u| {
                            u.focus = Some(Act::Row(i));
                            u.focus_visible = true;
                            u.list_scroll = 0.0;
                        });
                        repaint();
                    }
                } else {
                    on_key(VK_TAB.0);
                }
                return true;
            }
        }
    }
    // Esc in the search box closes the window like everywhere else.
    if msg.message == WM_KEYDOWN && msg.wParam.0 as u16 == VK_ESCAPE.0 {
        if let Some((h, edit)) = ui(|u| (u.hwnd, u.search)) {
            if msg.hwnd == edit {
                unsafe {
                    let _ = DestroyWindow(h);
                }
                return true;
            }
        }
    }
    false
}

pub fn refresh_update_status() {
    repaint();
}

fn copy_to_clipboard(hwnd: HWND, text: &str) {
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        if let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2) {
            let p = GlobalLock(mem) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
                let _ = GlobalUnlock(mem);
                let _ = SetClipboardData(13 /* CF_UNICODETEXT */, Some(windows::Win32::Foundation::HANDLE(mem.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

fn request_model() {
    let Some((title, body)) = app::with(|a| {
        let d = &a.device;
        let title = format!("Add model: {}{}", d.friendly_version().map(|v| format!("{v} ")).unwrap_or_default(), d.product);
        let body = format!("### Device info (from Fn Cheatsheet {APP_VERSION})\n```\n{}```\n\n### Official manual link (if you have it)\n\n\n### Notes\n", d.report().replace("\r\n", "\n"));
        (title, body)
    }) else {
        return;
    };
    crate::util::open_url(&format!("{REPO_URL}/issues/new?labels=model-request&title={}&body={}", url_encode(&title), url_encode(&body)));
}

fn open_profiles_folder() {
    let dir = user_profiles_dir();
    let _ = std::fs::create_dir_all(&dir);
    let readme = dir.join("README.txt");
    if !readme.exists() {
        let _ = std::fs::write(
            &readme,
            format!("Put your own profile files (*.json) here. Same format as data/profiles.json in the repository:\r\n{REPO_URL}/blob/main/data/profiles.json\r\nA profile with the same id as a built-in one replaces it. Then press Reload in Settings.\r\n"),
        );
    }
    let d = w(&dir.display().to_string());
    unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(None, wlit!("open"), d.pcwstr(), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

pub fn describe_key(vk: u32, scan: u32, ext: bool) -> String {
    let mut name = String::new();
    if scan != 0 {
        let mut buf = [0u16; 64];
        let lp = ((scan & 0xFF) << 16) | if ext { 1 << 24 } else { 0 };
        let n = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyNameTextW(lp as i32, &mut buf) };
        if n > 0 {
            name = from_wide(&buf);
        }
    }
    let hex = format!("VK 0x{vk:02X}{}", if scan != 0 { format!(", scan 0x{scan:02X}{}", if ext { " E0" } else { "" }) } else { String::new() });
    if name.is_empty() { hex } else { format!("{name} ({hex})") }
}

fn start_learning() {
    let Some(hwnd) = ui(|u| {
        u.learning = true;
        u.learned = None;
        u.learn_saw = false;
        u.learn_tone = 0;
        u.learn_msg = t("Listening… hold Fn now.", "듣는 중… 지금 Fn 키를 누르고 있어 보세요.").into();
        u.hwnd
    }) else {
        return;
    };
    hook::set_learning(true);
    unsafe {
        SetTimer(Some(hwnd), TIMER_LEARN, 5000, None);
    }
}

/// Key events from the hook while capturing. Shown only in this window; nothing is stored.
pub fn on_learn_key(up: bool, lp: isize) {
    if up {
        return;
    }
    let (vk, scan, flags) = ((lp & 0xFFFF) as u32, ((lp >> 16) & 0xFFFF) as u32, ((lp >> 32) & 0xFFFF_FFFF) as u32);
    let ext = flags & 0x01 != 0;
    let injected = flags & 0x10 != 0;
    let shown = ui(|u| {
        if !u.learning {
            return false;
        }
        u.learn_saw = true;
        if u.learned.is_none() {
            u.learned = Some((vk, scan, ext));
        }
        u.learn_msg = format!("{}{}{}", t("Received: ", "받은 신호: "), describe_key(vk, scan, ext), if injected { t(" (sent by software)", " (소프트웨어가 보낸 입력)") } else { "" });
        true
    });
    if shown == Some(true) {
        repaint();
    }
}

fn finish_learning() {
    hook::set_learning(false);
    ui(|u| {
        u.learning = false;
        unsafe {
            let _ = KillTimer(Some(u.hwnd), TIMER_LEARN);
        }
        match u.learned {
            None => {
                u.learn_tone = 2;
                u.learn_msg = t(
                    "Nothing arrived — this laptop's Fn key doesn't reach Windows (normal for most models). Use Left Ctrl, right next to Fn.",
                    "아무 신호도 오지 않았어요. 이 노트북의 Fn 키는 Windows에 신호를 보내지 않아요(대부분 그래요). Fn 바로 옆의 왼쪽 Ctrl을 쓰세요.",
                )
                .into();
            }
            Some((vk, scan, ext)) => {
                let risky = is_typing_key(vk);
                u.learn_tone = if risky { 2 } else { 1 };
                u.learn_msg = format!(
                    "{}{}{}",
                    t("First key: ", "처음 받은 키: "),
                    describe_key(vk, scan, ext),
                    if risky { t(" — a typing key would repeat while held. Pick another key.", " — 글자 입력 키라 길게 누르면 계속 입력돼요. 다른 키를 고르세요.") } else { "" }
                );
            }
        }
    });
    repaint();
}

fn use_learned() {
    let Some(Some((vk, scan, ext))) = ui(|u| u.learned) else { return };
    app::with(|a| {
        a.settings.trigger_vk = vk;
        a.settings.trigger_scan = scan;
        a.settings.trigger_extended = ext;
    });
    app::apply_settings();
    ui(|u| {
        u.learn_tone = 1;
        u.learn_msg = format!("{} {}", t("Now using", "이제 이 키를 써요:"), describe_key(vk, scan, ext));
        u.learned = None;
    });
    repaint();
}
