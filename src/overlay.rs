//! The cheat-sheet overlay: laid out with DirectWrite, drawn with Direct2D into a 32-bit DIB and shown
//! with UpdateLayeredWindow (per-pixel alpha, click-through, never takes focus). `render_png` reuses the
//! same drawing code for the README / website screenshots.
use std::cell::RefCell;

use windows::core::{w as wlit, Interface, Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM, GENERIC_WRITE};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::Vector2;

use crate::data::{KeyHint, Profile};
use crate::glyphs;
use crate::i18n::t;
use crate::util::{w, WStr};

// ───────────────────────── model & palette ─────────────────────────

pub struct OverlayModel<'a> {
    pub profile: Option<&'a Profile>,
    pub status: String,
    pub device_label: String,
    pub ambiguous: bool,
}

#[derive(Clone, Copy)]
pub struct Palette {
    panel: u32, panel_border: u32, text: u32, sub: u32, cap: u32, cap_border: u32, cap_text: u32, accent: u32, accent_text: u32, chip: u32, warn: u32,
}

pub const DARK: Palette = Palette {
    panel: 0xFF1C1D22, panel_border: 0xFF3A3C44, text: 0xFFF3F4F6, sub: 0xFFA3A8B3, cap: 0xFF2A2C33, cap_border: 0xFF41444D,
    cap_text: 0xFFC9CDD6, accent: 0xFF5CC8FF, accent_text: 0xFF0B1220, chip: 0xFF33363E, warn: 0xFFFFC857,
};
pub const LIGHT: Palette = Palette {
    panel: 0xFFF8F9FB, panel_border: 0xFFD6D9E0, text: 0xFF17181C, sub: 0xFF5B606B, cap: 0xFFFFFFFF, cap_border: 0xFFCDD1D9,
    cap_text: 0xFF3A3E47, accent: 0xFF0067C0, accent_text: 0xFFFFFFFF, chip: 0xFFE9ECF1, warn: 0xFF9A5B00,
};

pub fn palette_for(setting: &str) -> Palette {
    match setting {
        "dark" => DARK,
        "light" => LIGHT,
        _ => {
            if crate::app::apps_use_light_theme() { LIGHT } else { DARK }
        }
    }
}

fn color(argb: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((argb >> 16) & 0xFF) as f32 / 255.0,
        g: ((argb >> 8) & 0xFF) as f32 / 255.0,
        b: (argb & 0xFF) as f32 / 255.0,
        a: (argb >> 24) as f32 / 255.0,
    }
}

// ───────────────────────── text helpers ─────────────────────────

const UI_FONT: &str = "Segoe UI";
const TOP_ORDER: [&str; 13] = ["Esc", "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12"];
const PAD: f32 = 22.0;
const MARGIN: f32 = 18.0; // room for the shadow
const CAP_W: f32 = 76.0;
const CAP_H: f32 = 88.0;
const CAP_GAP: f32 = 4.0;
const CAPS_PER_ROW: usize = 15;

fn is_hangul(c: char) -> bool {
    matches!(c as u32, 0xAC00..=0xD7A3 | 0x1100..=0x11FF | 0x3130..=0x318F)
}

/// Korean breaks between any two syllables by default. Joining each word's letters with WORD JOINER makes
/// DirectWrite break only at spaces (CSS `word-break: keep-all`).
pub fn keep_all(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() * 2);
    for (i, &c) in chars.iter().enumerate() {
        out.push(c);
        if let Some(&n) = chars.get(i + 1) {
            if !c.is_whitespace() && !n.is_whitespace() && (is_hangul(c) || is_hangul(n)) {
                out.push('\u{2060}');
            }
        }
    }
    out
}

fn sup(n: usize) -> String {
    const D: [char; 10] = ['\u{2070}', '\u{00B9}', '\u{00B2}', '\u{00B3}', '\u{2074}', '\u{2075}', '\u{2076}', '\u{2077}', '\u{2078}', '\u{2079}'];
    n.to_string().chars().map(|c| D[c as usize - '0' as usize]).collect()
}

struct Gfx {
    d2d: ID2D1Factory,
    dw: IDWriteFactory,
}

thread_local! {
    static GFX: RefCell<Option<Gfx>> = const { RefCell::new(None) };
}

/// Releases the Direct2D/DirectWrite factories. Call before exiting: releasing COM objects from thread-local
/// destructors during process teardown (after their DLLs detach) fails with STATUS_INVALID_PARAMETER.
pub fn shutdown() {
    GFX.with(|g| {
        if let Ok(mut g) = g.try_borrow_mut() {
            *g = None;
        }
    });
}

fn with_gfx<R>(f: impl FnOnce(&Gfx) -> Result<R>) -> Result<R> {
    GFX.with(|g| {
        let mut g = g.borrow_mut();
        if g.is_none() {
            unsafe {
                let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
                let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
                *g = Some(Gfx { d2d, dw });
            }
        }
        f(g.as_ref().unwrap())
    })
}

struct Text {
    layout: IDWriteTextLayout,
    w: f32,
    h: f32,
}

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Center,
}

fn text(g: &Gfx, s: &str, size: f32, bold: bool, font: &str, max_w: f32, align: Align, wrap: bool) -> Result<Text> {
    unsafe {
        let fam = w(font);
        let loc = w(if crate::i18n::korean() { "ko-KR" } else { "en-US" });
        let fmt = g.dw.CreateTextFormat(
            fam.pcwstr(),
            None,
            if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL },
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            loc.pcwstr(),
        )?;
        fmt.SetWordWrapping(if wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP })?;
        fmt.SetTextAlignment(if align == Align::Center { DWRITE_TEXT_ALIGNMENT_CENTER } else { DWRITE_TEXT_ALIGNMENT_LEADING })?;
        let s = keep_all(s);
        let units = WStr::new(&s);
        let layout = g.dw.CreateTextLayout(units.units(), &fmt, max_w, 4000.0)?;
        let mut m = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut m)?;
        let w = if align == Align::Center { max_w } else { m.widthIncludingTrailingWhitespace.min(max_w) };
        Ok(Text { layout, w, h: m.height })
    }
}

// ───────────────────────── layout ─────────────────────────

enum Op {
    Rect { x: f32, y: f32, w: f32, h: f32, r: f32, fill: u32, stroke: Option<u32> },
    Text { t: Text, x: f32, y: f32, color: u32 },
}

struct Scene {
    ops: Vec<Op>,
    w: f32,
    h: f32,
}

fn build(g: &Gfx, m: &OverlayModel, pal: &Palette) -> Result<Scene> {
    let mut ops: Vec<Op> = Vec::new();
    let x0 = MARGIN + PAD;
    let mut y = MARGIN + PAD;

    // Header: [Fn] title / status
    let badge = text(g, "Fn", 18.0, true, UI_FONT, 200.0, Align::Left, false)?;
    let bw = badge.w + 20.0;
    let bh = badge.h + 6.0;
    let title_s = match m.profile {
        Some(p) => p.short_name().to_string(),
        None => t("Fn keys for this laptop are not set yet", "이 노트북의 Fn 키 정보가 아직 정해지지 않았어요").to_string(),
    };
    let title = text(g, &title_s, 17.0, true, UI_FONT, 900.0, Align::Left, false)?;
    let status = if m.status.is_empty() { None } else { Some(text(g, &m.status, 12.0, false, UI_FONT, 900.0, Align::Left, false)?) };
    let head_h = (title.h + status.as_ref().map(|s| s.h + 2.0).unwrap_or(0.0)).max(bh);
    let mut content_w = bw + 12.0 + title.w.max(status.as_ref().map(|s| s.w).unwrap_or(0.0));
    let by = y + (head_h - bh) / 2.0;
    ops.push(Op::Rect { x: x0, y: by, w: bw, h: bh, r: 8.0, fill: pal.accent, stroke: None });
    ops.push(Op::Text { x: x0 + 10.0, y: by + 3.0, t: badge, color: pal.accent_text });
    let tx = x0 + bw + 12.0;
    let ty = y + (head_h - (title.h + status.as_ref().map(|s| s.h + 2.0).unwrap_or(0.0))) / 2.0;
    let th = title.h;
    ops.push(Op::Text { x: tx, y: ty, t: title, color: pal.text });
    if let Some(s) = status {
        ops.push(Op::Text { x: tx, y: ty + th + 2.0, t: s, color: pal.sub });
    }
    y += head_h + 12.0;

    let Some(p) = m.profile else {
        let body_w = 620.0f32;
        let lines = [
            (if m.ambiguous {
                t("Several models match this PC and Fn Cheatsheet does not guess. Pick yours once in Settings.",
                  "이 PC에 맞는 모델이 여러 개라서 추측하지 않았어요. 설정에서 한 번만 골라 주세요.")
            } else {
                t("This model is not in the list yet, and Fn Cheatsheet does not guess. Pick a model with the same keyboard in Settings, or send us your model info.",
                  "아직 목록에 없는 모델이라 추측하지 않았어요. 설정에서 키보드가 같은 모델을 고르거나, 모델 정보를 보내 주세요.")
            }.to_string(), 14.0, pal.text),
            (format!("{}{}", t("This PC: ", "이 PC: "), m.device_label), 12.0, pal.sub),
            (t("Tray icon → Settings → Device", "트레이 아이콘 → 설정 → 기기").to_string(), 13.0, pal.accent),
        ];
        for (s, size, c) in lines {
            let tl = text(g, &s, size, false, UI_FONT, body_w, Align::Left, true)?;
            let h = tl.h;
            ops.push(Op::Text { x: x0, y, t: tl, color: c });
            y += h + 8.0;
        }
        content_w = content_w.max(body_w);
        return Ok(finish(ops, content_w, y - 8.0, pal));
    };

    let mut notes: Vec<String> = Vec::new();
    let mark = |k: &KeyHint, notes: &mut Vec<String>| -> String {
        let n = k.note().trim();
        if n.is_empty() {
            return String::new();
        }
        let i = match notes.iter().position(|x| x == n) {
            Some(i) => i,
            None => {
                notes.push(n.to_string());
                notes.len() - 1
            }
        };
        sup(i + 1)
    };

    // Top row
    let mut top: Vec<&KeyHint> = p.keys.iter().filter(|k| k.is_top()).collect();
    top.sort_by_key(|k| TOP_ORDER.iter().position(|t| t.eq_ignore_ascii_case(&k.key)).unwrap_or(100));
    if !top.is_empty() {
        let mut title = t("Top row", "윗줄 키").to_string();
        if !p.fn_lock.combo.is_empty() {
            title += "  ·  ";
            title += t("works with or without Fn depending on Fn Lock", "Fn 잠금 상태에 따라 Fn 없이 또는 Fn과 함께");
        }
        let st = text(g, &title, 12.0, false, UI_FONT, 1100.0, Align::Left, false)?;
        let h = st.h;
        ops.push(Op::Text { x: x0 + 2.0, y, t: st, color: pal.sub });
        y += h + 6.0;
        let per_row = top.len().min(CAPS_PER_ROW);
        content_w = content_w.max(per_row as f32 * (CAP_W + CAP_GAP) - CAP_GAP);
        for (i, k) in top.iter().enumerate() {
            let cx = x0 + (i % CAPS_PER_ROW) as f32 * (CAP_W + CAP_GAP);
            let cy = y + (i / CAPS_PER_ROW) as f32 * (CAP_H + CAP_GAP);
            ops.push(Op::Rect { x: cx, y: cy, w: CAP_W, h: CAP_H, r: 8.0, fill: pal.cap, stroke: Some(pal.cap_border) });
            let key = text(g, &format!("{}{}", k.key, mark(k, &mut notes)), 10.5, true, UI_FONT, CAP_W - 12.0, Align::Left, false)?;
            ops.push(Op::Text { x: cx + 6.0, y: cy + 4.0, t: key, color: pal.cap_text });
            if let Some(gl) = glyphs::glyph(&k.icon) {
                let gt = text(g, &gl.to_string(), 20.0, false, glyphs::FONT, CAP_W, Align::Center, false)?;
                ops.push(Op::Text { x: cx, y: cy + 22.0, t: gt, color: pal.text });
            }
            // Shrink until the longest word fits (e.g. "ExpertWidget").
            let label = k.label();
            let longest = label.split(' ').max_by_key(|w| w.chars().count()).unwrap_or("");
            let mut size = 10.5f32;
            while size > 8.5 {
                if text(g, longest, size, false, UI_FONT, 1000.0, Align::Left, false)?.w <= CAP_W - 12.0 {
                    break;
                }
                size -= 0.5;
            }
            let lt = text(g, label, size, false, UI_FONT, CAP_W - 8.0, Align::Center, true)?;
            ops.push(Op::Text { x: cx + 4.0, y: cy + 50.0, t: lt, color: pal.text });
        }
        let rows = top.len().div_ceil(CAPS_PER_ROW);
        y += rows as f32 * (CAP_H + CAP_GAP) + 10.0;
    }

    // Fn + key combos
    let combos: Vec<&KeyHint> = p.keys.iter().filter(|k| !k.is_top()).collect();
    if !combos.is_empty() {
        let st = text(g, t("Hold Fn and press", "Fn을 누른 채로"), 12.0, false, UI_FONT, 600.0, Align::Left, false)?;
        let h = st.h;
        ops.push(Op::Text { x: x0 + 2.0, y, t: st, color: pal.sub });
        y += h + 6.0;
        struct Item { parts: Vec<Op>, w: f32 }
        let mut items: Vec<Item> = Vec::new();
        for k in &combos {
            let mut parts = Vec::new();
            let mut x = 0.0f32;
            for (i, chip) in ["Fn", k.key.as_str()].iter().enumerate() {
                if i == 1 {
                    let plus = text(g, "+", 13.0, false, UI_FONT, 40.0, Align::Left, false)?;
                    let pw = plus.w;
                    parts.push(Op::Text { x: x + 4.0, y: 3.0, t: plus, color: pal.sub });
                    x += pw + 8.0;
                }
                let ct = text(g, chip, 12.0, true, UI_FONT, 300.0, Align::Left, false)?;
                let cw = (ct.w + 14.0).max(28.0);
                parts.push(Op::Rect { x, y: 0.0, w: cw, h: 24.0, r: 6.0, fill: pal.chip, stroke: Some(pal.cap_border) });
                let ctw = ct.w;
                parts.push(Op::Text { x: x + (cw - ctw) / 2.0, y: 4.0, t: ct, color: pal.text });
                x += cw;
            }
            if let Some(gl) = glyphs::glyph(&k.icon) {
                let gt = text(g, &gl.to_string(), 14.0, false, glyphs::FONT, 40.0, Align::Left, false)?;
                let gw = gt.w;
                parts.push(Op::Text { x: x + 10.0, y: 4.0, t: gt, color: pal.sub });
                x += 10.0 + gw;
            }
            let lt = text(g, &format!("{}{}", k.label(), mark(k, &mut notes)), 13.0, false, UI_FONT, 420.0, Align::Left, false)?;
            let lw = lt.w;
            parts.push(Op::Text { x: x + 8.0, y: 2.0, t: lt, color: pal.text });
            x += 8.0 + lw;
            items.push(Item { parts, w: x });
        }
        let cols = if items.len() > 8 { 3 } else { 2 };
        let col_w = items.iter().map(|i| i.w).fold(0.0, f32::max) + 24.0;
        content_w = content_w.max(col_w * cols as f32 - 24.0);
        let col_w = col_w.max((content_w + 24.0) / cols as f32);
        let n = items.len();
        for (i, item) in items.into_iter().enumerate() {
            let ix = x0 + (i % cols) as f32 * col_w;
            let iy = y + (i / cols) as f32 * 30.0;
            for op in item.parts {
                ops.push(match op {
                    Op::Rect { x, y, w, h, r, fill, stroke } => Op::Rect { x: x + ix, y: y + iy, w, h, r, fill, stroke },
                    Op::Text { t, x, y, color } => Op::Text { t, x: x + ix, y: y + iy, color },
                });
            }
        }
        y += n.div_ceil(cols) as f32 * 30.0 + 4.0;
    }

    // Footer: Fn Lock line, numbered notes, verification warning
    let mut footer: Vec<(String, f32, u32)> = Vec::new();
    if !p.fn_lock.combo.is_empty() {
        let mut s = format!("{}{}", t("Fn Lock: ", "Fn 잠금: "), p.fn_lock.combo);
        if !p.fn_lock_default().is_empty() {
            s += "  ·  ";
            s += p.fn_lock_default();
        }
        footer.push((s, 12.0, pal.sub));
    }
    for (i, n) in notes.iter().enumerate() {
        footer.push((format!("{} {}", sup(i + 1), n), 11.5, pal.sub));
    }
    if p.verified == "partial" {
        footer.push((t("Partly verified — some keys may differ on your exact model.", "일부만 검증된 정보예요. 정확한 모델에 따라 다를 수 있어요.").to_string(), 11.5, pal.warn));
    }
    if !footer.is_empty() {
        y += 4.0;
        for (s, size, c) in footer {
            let ft = text(g, &s, size, false, UI_FONT, content_w, Align::Left, true)?;
            let h = ft.h;
            ops.push(Op::Text { x: x0, y, t: ft, color: c });
            y += h + 3.0;
        }
    }
    Ok(finish(ops, content_w, y, pal))
}

/// Adds the panel (and its soft shadow) underneath everything and returns the scene size.
fn finish(ops: Vec<Op>, content_w: f32, bottom: f32, pal: &Palette) -> Scene {
    let pw = content_w + PAD * 2.0;
    let ph = bottom - MARGIN + PAD - 6.0;
    let mut all = Vec::with_capacity(ops.len() + 8);
    for i in (1..=6).rev() {
        let s = i as f32 * 2.0;
        all.push(Op::Rect { x: MARGIN - s, y: MARGIN - s + 5.0, w: pw + s * 2.0, h: ph + s * 2.0, r: 18.0 + s, fill: 0x0A000000, stroke: None });
    }
    all.push(Op::Rect { x: MARGIN, y: MARGIN, w: pw, h: ph, r: 18.0, fill: pal.panel, stroke: Some(pal.panel_border) });
    all.extend(ops);
    Scene { ops: all, w: pw + MARGIN * 2.0, h: ph + MARGIN * 2.0 }
}

fn draw(rt: &ID2D1RenderTarget, scene: &Scene) -> Result<()> {
    unsafe {
        rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        rt.BeginDraw();
        rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        let brush = rt.CreateSolidColorBrush(&color(0xFFFFFFFF), None)?;
        for op in &scene.ops {
            match op {
                Op::Rect { x, y, w, h, r, fill, stroke } => {
                    let rr = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: *x, top: *y, right: x + w, bottom: y + h }, radiusX: *r, radiusY: *r };
                    brush.SetColor(&color(*fill));
                    rt.FillRoundedRectangle(&rr, &brush);
                    if let Some(s) = stroke {
                        let inner = D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F { left: x + 0.5, top: y + 0.5, right: x + w - 0.5, bottom: y + h - 0.5 },
                            radiusX: r - 0.5,
                            radiusY: r - 0.5,
                        };
                        brush.SetColor(&color(*s));
                        rt.DrawRoundedRectangle(&inner, &brush, 1.0, None);
                    }
                }
                Op::Text { t, x, y, color: c } => {
                    brush.SetColor(&color(*c));
                    rt.DrawTextLayout(Vector2 { X: *x, Y: *y }, &t.layout, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
            }
        }
        rt.EndDraw(None, None)
    }
}

// ───────────────────────── window ─────────────────────────

pub struct Overlay {
    pub hwnd: HWND,
    pinned: bool,
}

const CLASS: PCWSTR = wlit!("FnCheatsheetOverlay");

impl Overlay {
    pub fn create() -> Overlay {
        unsafe {
            let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default();
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(overlay_proc),
                hInstance: hinst.into(),
                lpszClassName: CLASS,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..Default::default()
            };
            RegisterClassExW(&wc);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                CLASS,
                wlit!("Fn Cheatsheet"),
                WS_POPUP,
                0, 0, 1, 1,
                None, None, Some(hinst.into()), None,
            )
            .unwrap_or_default();
            Overlay { hwnd, pinned: false }
        }
    }

    pub fn show(&mut self, m: &OverlayModel, pal: &Palette, user_scale: f32, pinned: bool) {
        if let Err(e) = self.show_inner(m, pal, user_scale, pinned) {
            crate::util::log(&format!("overlay show failed: {e}"));
        }
        // The layered window keeps its own copy of the pixels; drop the factories and their caches so the
        // tray app goes back to its idle footprint. Recreating them on the next hold takes a few ms.
        shutdown();
    }

    fn show_inner(&mut self, m: &OverlayModel, pal: &Palette, user_scale: f32, pinned: bool) -> Result<()> {
        unsafe {
            // Monitor where the user is working: the foreground window's, else the cursor's.
            let fg = GetForegroundWindow();
            let mon = if !fg.0.is_null() && fg != self.hwnd {
                MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST)
            } else {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST)
            };
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut mi);
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            let mut scale = dx as f32 / 96.0 * user_scale;
            let wa = mi.rcWork;
            let (aw, ah) = ((wa.right - wa.left) as f32, (wa.bottom - wa.top) as f32);

            let scene = with_gfx(|g| build(g, m, pal))?;
            // Never larger than the work area.
            let fit = (aw / (scene.w * scale)).min(ah / (scene.h * scale)).min(1.0);
            scale *= fit;
            let pw = (scene.w * scale).ceil() as i32;
            let ph = (scene.h * scale).ceil() as i32;

            let screen = GetDC(None);
            let mem = CreateCompatibleDC(Some(screen));
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: pw,
                    biHeight: -ph,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let dib = CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let old = SelectObject(mem, dib.into());

            let result = with_gfx(|g| {
                // Software rasterizer: drawing one small sheet doesn't justify loading the GPU driver stack
                // (tens of MB that would stay resident in a tray app).
                let props = D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                    dpiX: 96.0 * scale,
                    dpiY: 96.0 * scale,
                    usage: D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                };
                let rt = g.d2d.CreateDCRenderTarget(&props)?;
                rt.BindDC(mem, &RECT { left: 0, top: 0, right: pw, bottom: ph })?;
                draw(&rt.cast()?, &scene)
            });

            if result.is_ok() {
                let pos = POINT { x: wa.left + ((wa.right - wa.left - pw) / 2).max(0), y: wa.top + ((wa.bottom - wa.top - ph) / 2).max(0) };
                let size = SIZE { cx: pw, cy: ph };
                let src = POINT::default();
                let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
                self.set_pinned(pinned);
                let _ = UpdateLayeredWindow(self.hwnd, Some(screen), Some(&pos), Some(&size), Some(mem), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                let _ = ShowWindow(self.hwnd, if pinned { SW_SHOW } else { SW_SHOWNOACTIVATE });
                if pinned {
                    let _ = SetForegroundWindow(self.hwnd);
                }
            }

            SelectObject(mem, old);
            let _ = DeleteObject(dib.into());
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            result
        }
    }

    fn set_pinned(&mut self, pinned: bool) {
        self.pinned = pinned;
        unsafe {
            let mut ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            let flags = (WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0) as isize;
            if pinned { ex &= !flags } else { ex |= flags }
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, ex);
        }
    }

    pub fn hide(&mut self) {
        self.pinned = false;
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }
}

unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        // Pinned mode (opened from the tray): any key, click or focus loss closes it.
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            let _ = ShowWindow(hwnd, SW_HIDE);
            LRESULT(0)
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) == WA_INACTIVE as usize => {
            let _ = ShowWindow(hwnd, SW_HIDE);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_ACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// ───────────────────────── PNG (docs) ─────────────────────────

pub fn render_png(m: &OverlayModel, pal: &Palette, path: &str, scale: f32) -> Result<()> {
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let scene = with_gfx(|g| build(g, m, pal))?;
        let pw = (scene.w * scale).ceil() as u32;
        let ph = (scene.h * scale).ceil() as u32;
        let bmp = wic.CreateBitmap(pw, ph, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)?;
        with_gfx(|g| {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0 * scale,
                dpiY: 96.0 * scale,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = g.d2d.CreateWicBitmapRenderTarget(&bmp, &props)?;
            draw(&rt, &scene)
        })?;
        let stream = wic.CreateStream()?;
        let p = w(path);
        stream.InitializeFromFilename(p.pcwstr(), GENERIC_WRITE.0)?;
        let enc = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        let mut bag = None;
        enc.CreateNewFrame(&mut frame, &mut bag)?;
        let frame = frame.unwrap();
        frame.Initialize(bag.as_ref())?;
        frame.SetSize(pw, ph)?;
        let mut fmt = GUID_WICPixelFormat32bppPBGRA;
        frame.SetPixelFormat(&mut fmt)?;
        frame.WriteSource(&bmp, std::ptr::null())?;
        frame.Commit()?;
        enc.Commit()
    }
}
