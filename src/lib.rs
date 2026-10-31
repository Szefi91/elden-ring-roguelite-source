//! Elden Ring Roguelite mode -- Phase 1 PoC.
//!
//! Goal of this file right now: prove the two hardest unknowns at once.
//!   1. We can inject a DLL into a running Elden Ring and draw our own UI on top.
//!   2. From that same DLL we can later read/write game state (runes, level, items).
//!
//! This PoC only does (1): it renders a small window that confirms the overlay is
//! alive and shows a frame counter. The leveling loop and card draft land on top of
//! this in Phase 2/3.

mod release_support;
use hudhook::hooks::dx12::ImguiDx12Hooks;
use hudhook::{Hudhook, ImguiRenderLoop, MessageFilter, RenderContext};
use imgui::TextureId;
use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

/// Per-type card-frame PNGs (1080p), embedded at build time. Each card is drawn
/// with the frame of its kind.
use cards::{CardKind, CurseEffect};
const FRAMES: [(CardKind, &[u8]); 8] = [
    (CardKind::Stat, include_bytes!("../refs/kartya-dizajn/jatek-meretek/1080p_320x480/card-frame-stat.png")),
    (CardKind::Weapon, include_bytes!("../refs/kartya-dizajn/jatek-meretek/1080p_320x480/card-frame-fegyver.png")),
    (CardKind::Armor, include_bytes!("../refs/kartya-dizajn/jatek-meretek/1080p_320x480/card-frame-pancel.png")),
    (CardKind::Shield, include_bytes!("../refs/kartya-dizajn/jatek-meretek/1080p_320x480/card-frame-pajzs.png")),
    (CardKind::AshOfWar, include_bytes!("../refs/kartya-dizajn/jatek-meretek/1080p_320x480/card-frame-ashofwar.png")),
    (CardKind::Spell, include_bytes!("../refs/kartya-dizajn/jatek-meretek/1080p_320x480/card-frame-varazslat.png")),
    (CardKind::Talisman, include_bytes!("../refs/kartya-dizajn/jatek-meretek/1080p_320x480/card-frame-talizman.png")),
    // Curse cards reuse the legendary (red) frame; the black smoke aura marks them.
    (CardKind::Curse, include_bytes!("../refs/kartya-dizajn/jatek-meretek/1080p_320x480/card-frame-legendary.png")),
];

/// EB Garamond (SIL OFL) -- close to Elden Ring's Agmena, for all overlay text.
const FONT_GARAMOND: &[u8] = include_bytes!("../refs/fonts/EBGaramond-VariableFont_wght.ttf");
/// Cinzel (SIL OFL) -- the display serif for the "LEVEL UP" title, card names and the
/// stat +N, matching the approved web design (Aurora, IN-GAME-SPEC.md, 2026-10-24).
const FONT_CINZEL: &[u8] = include_bytes!("../refs/fonts/Cinzel-VariableFont_wght.ttf");

/// Confirm button: the game's OWN menu-button sprites (Aurora extracted them),
/// composited with a horizontal 3-slice. Caps: frame 144/143, glow 108/109,
/// corner 38/40 (never stretched vertically).
const BTN_FRAME: &[u8] = // MENU_FL_Enterbutton 354x94
    include_bytes!("../refs/kartya-dizajn/overlay-gomb/gomb-keret-jatek.png");
const BTN_GLOW: &[u8] = // MENU_FL_Cursor_Ent 428x110 (hover)
    include_bytes!("../refs/kartya-dizajn/overlay-gomb/gomb-hover-feny-jatek.png");
const BTN_CORNER: &[u8] = // MENU_FL_Cursor_EntWaku 428x108 (hover corners)
    include_bytes!("../refs/kartya-dizajn/overlay-gomb/gomb-hover-sarokjel-jatek.png");
const BTN_LABEL: &[u8] = // "Confirm" in the game's own font
    include_bytes!("../refs/kartya-dizajn/overlay-gomb/1080p/confirm-label.png");

/// Curse-card smoke: the game's OWN smoke FLIPBOOK (sfxbnd_commoneffects s21001_a,
/// an 8x8 = 64-frame sprite sheet of a billowing smoke puff). We play it frame by
/// frame on several desynced "puff" particles that spawn around the card edges, so
/// the smoke genuinely billows and churns behind the card (the proper technique, not
/// a single texture scaling) -- Szefi, 2026-10-24.
/// Curse-card flame: Aurora's dedicated BLACK-WHITE flame flipbook (same 8x8/128px/64
/// geometry as the game smoke sheet, so the index math is unchanged), tinted white.
const SMOKE_FX: &[u8] = include_bytes!("../refs/kartya-dizajn/fx/feketelang-flipbook.png");
/// Flipbook grid dimensions (8 columns x 8 rows = 64 frames).
const SMOKE_COLS: u32 = 8;
const SMOKE_ROWS: u32 = 8;
const SMOKE_FRAMES: u32 = SMOKE_COLS * SMOKE_ROWS;

/// The curse card's ITEM-FREE frame (skull overflowing the top, red frame, Szefi's
/// purple nebula, the medallion ring + dark disc, and a baked CURSED label). It carries
/// NO item icon / name / description / effect -- the game draws those live, so the curse
/// card is random every draft (Szefi, 2026-10-24). 1080p variant; atok-keret-JEGYZEK.json.
/// Canvas 364x651, panel (0,104) 364x547 -- the panel is the standard card size, the
/// canvas is taller only by the skull band on top. Content geometry (fractions of the
/// PANEL) is in ATOK_* below, from the JEGYZEK _geometria.
const ATOK_KERET: &[u8] = include_bytes!("../refs/kartya-dizajn/jatek-meretek/atok-keret-1080p.png");
const KERET_CANVAS_W: f32 = 364.0;
const KERET_CANVAS_H: f32 = 651.0;
const KERET_PANEL_X: f32 = 0.0;
const KERET_PANEL_Y: f32 = 104.0;
const KERET_PANEL_W: f32 = 364.0;
const KERET_PANEL_H: f32 = 547.0;

fn type_label(k: CardKind) -> &'static str {
    match k {
        CardKind::Weapon => "WEAPON",
        CardKind::Armor => "ARMOUR",
        CardKind::Shield => "SHIELD",
        CardKind::Stat => "ATTRIBUTE",
        CardKind::Spell => "SORCERY",
        CardKind::AshOfWar => "ASH OF WAR",
        CardKind::Talisman => "TALISMAN",
        CardKind::Ability => "ABILITY",
        CardKind::Curse => "CURSE",
    }
}

/// Inflict a cursed card's effect on the player when it is taken. Poison/Bleed drive the
/// game's native status system; Frailty/Weakness/RuneForfeit write player data (the grant
/// loop re-baselines `authorized` right after, so the Grace-suppression keeps them).
fn apply_curse(curse: CurseEffect) {
    let what = match curse {
        CurseEffect::Poison => {
            // Buildup status: Szefi needed ~30 taps on the test key to fill the bar, so
            // stack plenty to guarantee the proc on one curse.
            for _ in 0..80 {
                game::apply_speffect(SP_POISON);
            }
            "poison".to_string()
        }
        CurseEffect::Bleed => {
            // Hemorrhage procs on a single self-application (1753).
            game::apply_speffect(SP_BLEED);
            "bleed".to_string()
        }
        CurseEffect::Madness => {
            // Buildup status (1782). ~30 taps proc'd it for Szefi; stack to be sure.
            for _ in 0..80 {
                game::apply_speffect(SP_MADNESS);
            }
            "madness".to_string()
        }
        CurseEffect::ScarletRot => {
            // Buildup status (4054). Same as madness/poison -- many applications.
            for _ in 0..80 {
                game::apply_speffect(SP_ROT);
            }
            "scarlet rot".to_string()
        }
        CurseEffect::DeathBlight => {
            // Instant death on proc (1794). A handful of applications fills the bar.
            for _ in 0..20 {
                game::apply_speffect(SP_BLIGHT);
            }
            "death blight".to_string()
        }
        CurseEffect::Frostbite => {
            // DEFERRED: no working self/environmental frost id yet (20500360/1800/1801
            // never proc). Szefi's plan: a custom param row cloned from 1753 with
            // freezeAttackPower. Harmless no-op until that id exists.
            for _ in 0..5 {
                game::apply_speffect(SP_FROST);
            }
            "frostbite".to_string()
        }
        CurseEffect::Teleport => {
            let i = (entropy_seed(0) % GRACES.len() as u64) as usize;
            let invoked = game::warp_to_grace(GRACES[i]);
            format!("teleport request to grace {}; catalog_size={}; call_invoked={invoked}", GRACES[i], GRACES.len())
        }
        CurseEffect::HalveHp => {
            game::halve_hp();
            "halve current HP".to_string()
        }
        CurseEffect::EmptyFlasks => {
            // NOTE: persists to the save (max flask count). Not in the auto-roll yet --
            // used only when explicitly chosen, until we make it restore on next rest.
            let (hp, fp) = game::empty_flasks();
            format!("empty flasks (was {hp}/{fp})")
        }
        CurseEffect::Frailty(n) => {
            game::drain_attribute(0, n); // 0 = Vigor -> lower max HP
            format!("frailty -{n} vigor")
        }
        CurseEffect::Weakness(attr, n) => {
            game::drain_attribute(attr, n);
            format!("weakness -{n} attr{attr}")
        }
        CurseEffect::RuneForfeit(pct) => {
            game::forfeit_runes(pct);
            format!("rune forfeit {pct}%")
        }
    };
    hook_log(&format!("curse applied: {what}"));
}

/// Word-wrap `text` to at most `max_w` pixels per line and `max_lines` lines.
fn wrap_text(ui: &imgui::Ui, text: &str, max_w: f32, max_lines: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let trial = if cur.is_empty() {
            word.to_string()
        } else {
            format!("{cur} {word}")
        };
        if cur.is_empty() || ui.calc_text_size(&trial)[0] <= max_w {
            cur = trial;
        } else {
            lines.push(std::mem::take(&mut cur));
            if lines.len() >= max_lines {
                return lines;
            }
            cur = word.to_string();
        }
    }
    if !cur.is_empty() && lines.len() < max_lines {
        lines.push(cur);
    }
    lines
}

/// Horizontal 3-slice: draw `tex` (w x h, left/right caps) into dest (dx,dy,dw,dh)
/// stretching only the middle. Height is scaled from the sprite, caps keep aspect.
#[allow(clippy::too_many_arguments)]
fn draw_hslice(
    bg: &imgui::DrawListMut,
    tex: TextureId,
    w: u32,
    h: u32,
    cap_l: f32,
    cap_r: f32,
    dx: f32,
    dy: f32,
    dw: f32,
    dh: f32,
    alpha: f32,
) {
    let s = dh / h as f32;
    let (cl, cr) = (cap_l * s, cap_r * s);
    let (ul, ur) = (cap_l / w as f32, (w as f32 - cap_r) / w as f32);
    let col = [1.0, 1.0, 1.0, alpha];
    bg.add_image(tex, [dx, dy], [dx + cl, dy + dh]).uv_min([0.0, 0.0]).uv_max([ul, 1.0]).col(col).build();
    bg.add_image(tex, [dx + cl, dy], [dx + dw - cr, dy + dh]).uv_min([ul, 0.0]).uv_max([ur, 1.0]).col(col).build();
    bg.add_image(tex, [dx + dw - cr, dy], [dx + dw, dy + dh]).uv_min([ur, 0.0]).uv_max([1.0, 1.0]).col(col).build();
}

/// Decode a PNG to tightly-packed RGBA8 bytes for `RenderContext::load_texture`.
/// imgui has no letter-spacing; the Elden Ring menu character comes from the wide
/// tracking on the title/type/rarity lines. So we draw those strings one glyph at a
/// time, adding `extra` px after each. Caller pushes the font and passes the colour.
/// Returns the total advance width (no trailing gap).
fn add_text_spaced(
    bg: &imgui::DrawListMut,
    ui: &imgui::Ui,
    pos: [f32; 2],
    col: [f32; 4],
    text: &str,
    extra: f32,
) -> f32 {
    let extra = extra * ui.io().font_global_scale;
    let mut x = pos[0];
    for ch in text.chars() {
        let mut buf = [0u8; 4];
        let s: &str = ch.encode_utf8(&mut buf);
        bg.add_text([x, pos[1]], col, s);
        x += ui.calc_text_size(s)[0] + extra;
    }
    if text.chars().next().is_some() {
        (x - extra - pos[0]).max(0.0)
    } else {
        0.0
    }
}

/// Width `add_text_spaced` would occupy, so a spaced string can be centred.
fn text_spaced_width(ui: &imgui::Ui, text: &str, extra: f32) -> f32 {
    let extra = extra * ui.io().font_global_scale;
    let mut w = 0.0;
    let mut any = false;
    for ch in text.chars() {
        any = true;
        let mut buf = [0u8; 4];
        let s: &str = ch.encode_utf8(&mut buf);
        w += ui.calc_text_size(s)[0] + extra;
    }
    if any {
        (w - extra).max(0.0)
    } else {
        0.0
    }
}

/// The gold "effect" line under a card's description, matching the web design's
/// `hatas` slot. Stat cards state what they raise; the weapon card states its scaling
/// (the exact grade is read from the param at apply time, the wording is fixed here).
fn effect_label(card: &card_data::CardInstance) -> String {
    if let Some((attr, _pts)) = card.stat {
        return match attr {
            0 => "Max HP up",
            1 => "Max FP up",
            2 => "Max stamina up",
            3 => "Strength up",
            4 => "Dexterity up",
            5 => "Intelligence up",
            6 => "Faith up",
            7 => "Arcane up",
            _ => "Attribute up",
        }
        .to_string();
    }
    if !card.costs.is_empty() {
        return "Price hidden".to_string();
    }
    match card.kind {
        CardKind::Weapon => "Scales \u{00b7} Str / Dex".to_string(),
        CardKind::Armor => "High damage negation".to_string(),
        CardKind::Shield => "Guard boost".to_string(),
        CardKind::AshOfWar => "Weapon skill".to_string(),
        CardKind::Spell => "Sorcery".to_string(),
        CardKind::Talisman => "Talisman effect".to_string(),
        _ => String::new(),
    }
}

/// Rarity tier label (spaced caps on the bottom line, like the web).
fn rarity_label(r: u8) -> &'static str {
    match r {
        0 | 1 => "COMMON",
        2 => "UNCOMMON",
        3 => "RARE",
        _ => "LEGENDARY",
    }
}
/// Rarity tier colour (web tokens: common / rare-gold / legendary-orange).
fn rarity_color(r: u8) -> [f32; 3] {
    match r {
        0 | 1 => [0.608, 0.573, 0.506],
        2 | 3 => [0.765, 0.635, 0.298],
        _ => [0.824, 0.443, 0.165],
    }
}

fn decode_rgba(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let decoder = png::Decoder::new(bytes);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    match info.color_type {
        png::ColorType::Rgba => {
            buf.truncate((w * h * 4) as usize);
            Some((buf, w, h))
        }
        png::ColorType::Rgb => {
            let mut rgba = Vec::with_capacity((w * h * 4) as usize);
            for px in buf[..(w * h * 3) as usize].chunks_exact(3) {
                rgba.extend_from_slice(px);
                rgba.push(255);
            }
            Some((rgba, w, h))
        }
        _ => None,
    }
}

// Win32 TRUE/FALSE as the raw i32 the loader expects (avoids the `windows` crate's
// BOOL type, which moved between versions).
const TRUE_I32: i32 = 1;

mod card_data; // the item pool (real ER items: names, descriptions, ids)
mod cards; // card kinds + pick helpers
mod game; // reads/writes live player state (runes, soul level, stats) in Elden Ring
mod icons_gen; // GENERATED: embedded item-medallion icons (MENU_ItemIcon_<id>.png)
mod leveling; // rune-threshold -> auto level-up math
mod pad_singletons_gen; // GENERATED: candidate FD4-singleton slot RVAs (pad freeze)

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::Input::XboxController::{
    XInputGetState, XINPUT_GAMEPAD_A, XINPUT_GAMEPAD_B, XINPUT_GAMEPAD_DPAD_DOWN,
    XINPUT_GAMEPAD_DPAD_LEFT, XINPUT_GAMEPAD_DPAD_RIGHT, XINPUT_GAMEPAD_DPAD_UP,
    XINPUT_GAMEPAD_RIGHT_SHOULDER, XINPUT_STATE,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

// Virtual-key codes, mapped to Elden Ring's own menu keys so it feels native:
// A/D move, E confirms/selects, Q backs out; I opens the draft (G is the map).
const VK_A: i32 = 0x41;
const VK_D: i32 = 0x44;
const VK_W: i32 = 0x57;
const VK_S: i32 = 0x53;
const VK_E: i32 = 0x45;
const VK_Q: i32 = 0x51;
const VK_I: i32 = 0x49;
const WATCHED_KEYS: [i32; 7] = [VK_A, VK_D, VK_W, VK_S, VK_E, VK_Q, VK_I];

/// BonfireWarpParam bonfireEntityId for the Stranded Graveyard Site of Grace (the
/// intro start). Read from the live regulation by Aurora (row 180001), cross-checked.
const STRANDED_GRAVEYARD_BONFIRE: u32 = 18001951;

/// Strong status-proc SpEffects (Aurora, measured 2.7.2.0): the old 500/505 were cyclic
/// damage on an already-applied status, NOT a proc, so they never showed. 1753 is the
/// game's own self-bleed and procs visibly on a single call; poison/frost may need 2-3.
const SP_BLEED: i32 = 1753;
const SP_POISON: i32 = 20_500_330;
const SP_FROST: i32 = 20_500_360;
// Confirmed working self-status buildup ids (Szefi live-tested via the N file-reader,
// 2026-10-24). Buildup statuses need MANY applications to fill the bar and proc, so
// apply_curse stacks them; Death Blight is instant on proc, so it needs fewer.
const SP_MADNESS: i32 = 1782; // Vyke self-madness
const SP_ROT: i32 = 4054; // Lake of Rot scarlet rot
const SP_BLIGHT: i32 = 1794; // Eclipse Shotel deathblight -- INSTANT DEATH

// Full base-game teleport destination catalog.
include!("graces_gen.rs");

fn key_down(vk: i32) -> bool {
    (unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000) != 0
}

/// True only when the game's OWN window is the foreground (focused) window.
///
/// `GetAsyncKeyState` is GLOBAL: it reports a key as down even when the game is
/// alt-tabbed away and the player is typing in another app. Without this check the
/// draft hotkeys (I to open, P to top-up runes) would fire outside the game, exactly
/// what Szefi hit ("ha kimegyek a jatekbol es nyomok egy i betut akkor is bejon").
/// The game's native keys (e.g. G for the map) only react while the window is
/// focused; routing our keys through this makes them behave the same. Falls back to
/// `false` on any doubt (null hwnd / zero pid) -- safer to ignore a key than to fire
/// one when the game is not in front.
fn game_window_focused() -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid != 0 && pid == GetCurrentProcessId()
    }
}

/// A non-deterministic 64-bit seed for the card draw. The old code seeded the RNG
/// with a FIXED constant, so every launch dealt the exact same early hands -- that is
/// why Szefi kept getting the same clothes/talisman "az elejen". `_rdtsc` (the CPU
/// cycle counter) is different on every launch and every call; we avalanche it with a
/// splitmix step and fold in the frame counter so each level-up draws fresh too.
fn entropy_seed(frames: u64) -> u64 {
    let t = unsafe { core::arch::x86_64::_rdtsc() };
    let mut x = t.rotate_left(27)
        ^ frames.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ 0xD1B5_4A32_D192_ED03;
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    x | 1 // never zero: a zero state would freeze the xorshift in card_data
}

// ---- Controller (XInput) navigation -------------------------------------------
// The draft must be navigable AND selectable with a controller, not just the
// keyboard (Szefi, 2026-10-23). We poll XInput port 0 each frame, build a logical
// button mask, and map it onto the SAME virtual keys the keyboard nav uses, so the
// draft state machine needs no controller-specific branches.
//
// NOTE: like the keyboard, these inputs also reach the game while a draft is open
// (the in-world character freeze is a separate, still-open item). The pad only
// DRIVES the overlay; it does not suppress the game's own reading of the stick/buttons.
const PAD_LEFT: u16 = 1 << 0;
const PAD_RIGHT: u16 = 1 << 1;
const PAD_DOWN: u16 = 1 << 2;
const PAD_UP: u16 = 1 << 3;
const PAD_CONFIRM: u16 = 1 << 4;
const PAD_BACK: u16 = 1 << 5;
/// The open-the-draft CHORD: B + RB (right shoulder) held together (Szefi,
/// 2026-10-23: "apszolult legyen b plusz rb"). No single button opens the overlay.
/// B doubles as the back button (PAD_BACK); a lone B only backs out of an already
/// open draft, so sharing the bit is safe. Here we add only the RB half; the B half
/// is PAD_BACK, which read_pad_mask already sets.
const PAD_OPEN_RB: u16 = 1 << 6;
const PAD_OPEN_CHORD: u16 = PAD_BACK | PAD_OPEN_RB;

/// Virtual-bit -> keyboard VK, so pad edges flow into the same `edges` set as keys.
/// The open-chord is deliberately NOT here: it is matched as a combination in
/// `poll_edges`, not as a single button.
const PAD_MAP: [(u16, i32); 6] = [
    (PAD_LEFT, VK_A),
    (PAD_RIGHT, VK_D),
    (PAD_DOWN, VK_S),
    (PAD_UP, VK_W),
    (PAD_CONFIRM, VK_E),
    (PAD_BACK, VK_Q),
];

// ---- XInput hook: block the pad from the GAME while a draft is open ----------
// Measured on the live game (2026-10-23): the equipment-menu "freeze" is NOT a pad
// flag -- the CSPad struct is byte-identical with the menu open, so the game keeps
// polling the pad and gates gameplay through an internal menu flag that is fragile to
// fake. Instead we intercept XInputGetState itself. While a draft is open the GAME
// gets a neutral (zeroed) pad -> the character never jumps or swaps weapons; our own
// nav reads the REAL pad through the detour trampoline. This is a single branch per
// call on a function the game already calls each frame: no scan, no polling, no loop,
// so it is CHEAPER than the old allow_polling resolve (which caused the open hitch).
// Version-independent (OS API), and it writes NO game memory.
static DRAFT_INPUT_BLOCKED: AtomicBool = AtomicBool::new(false);
static XINPUT_HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);
/// True while a level is earned and the draft is openable (Phase::Ready). The hook
/// uses this to swallow the B+RB OPEN chord from the GAME so opening the lap does not
/// also roll+attack -- our own nav reads the chord via the trampoline, so it still
/// opens. Scoped to Ready so ordinary combat input is untouched.
static DRAFT_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Exact ABIs of the functions we detour. Each target and its detour share the type.
type XInputGetStateFn = unsafe extern "system" fn(u32, *mut XINPUT_STATE) -> u32;
type GetKeyStateFn = unsafe extern "system" fn(i32) -> i16;
type GetKeyboardStateFn =
    unsafe extern "system" fn(*mut u8) -> i32;
/// IDirectInputDevice8W::GetDeviceState impl (shared by all keyboard devices): the ER
/// movement keys come through here, not user32. ABI matches the COM vtable slot.
type GetDeviceStateFn = unsafe extern "system" fn(
    *mut core::ffi::c_void,
    u32,
    *mut core::ffi::c_void,
) -> windows::core::HRESULT;

/// Holds an installed detour so both the detour body and our own nav can reach the
/// trampoline (`.call`). GenericDetour is not auto-Sync (it holds raw pointers), but it
/// is only read after one-time install and the game calls the detour from its own
/// threads, so sharing it immutably is sound -- we assert that here.
struct DetourCell<F: retour::Function>(retour::GenericDetour<F>);
unsafe impl<F: retour::Function> Send for DetourCell<F> {}
unsafe impl<F: retour::Function> Sync for DetourCell<F> {}
static XINPUT_DETOUR: once_cell::sync::OnceCell<DetourCell<XInputGetStateFn>> =
    once_cell::sync::OnceCell::new();
static GETKEYSTATE_DETOUR: once_cell::sync::OnceCell<DetourCell<GetKeyStateFn>> =
    once_cell::sync::OnceCell::new();
static GETKBDSTATE_DETOUR: once_cell::sync::OnceCell<DetourCell<GetKeyboardStateFn>> =
    once_cell::sync::OnceCell::new();
static DINPUT_GDS_DETOUR: once_cell::sync::OnceCell<DetourCell<GetDeviceStateFn>> =
    once_cell::sync::OnceCell::new();

/// The detour the GAME hits. Draft open -> neutral pad + success (no input reaches
/// gameplay); draft closed -> the real function via the trampoline.
unsafe extern "system" fn xinput_get_state_detour(index: u32, state: *mut XINPUT_STATE) -> u32 {
    if DRAFT_INPUT_BLOCKED.load(Ordering::Relaxed) {
        if !state.is_null() {
            *state = XINPUT_STATE::default(); // zeroed buttons/sticks/triggers
        }
        return 0; // ERROR_SUCCESS with neutral data
    }
    let r = match XINPUT_DETOUR.get() {
        Some(d) => d.0.call(index, state),
        None => return 0,
    };
    // Swallow the B+RB OPEN chord from the GAME while a draft is available, so opening
    // the lap does not also make the character roll+attack. This runs at the game's own
    // read time (unlike the overlay, which reacts a frame later), so even the first
    // chord frame never reaches gameplay. Our nav uses the trampoline directly and
    // still sees the chord. Only the exact simultaneous B+RB is swallowed.
    if r == 0 && DRAFT_AVAILABLE.load(Ordering::Relaxed) && !state.is_null() {
        let b = (*state).Gamepad.wButtons.0;
        if (b & XINPUT_GAMEPAD_B.0) != 0 && (b & XINPUT_GAMEPAD_RIGHT_SHOULDER.0) != 0 {
            *state = XINPUT_STATE::default();
        }
    }
    r
}

/// Which of A / B / RB are currently held on the first connected pad (real state via
/// the trampoline). Used to DRAIN the freeze on close: we keep the game's pad blocked
/// after a draft closes until the closing/confirming button is released, so a held B
/// never rolls the character back and a held A never makes it jump.
fn pad_buttons_held() -> (bool, bool, bool) {
    for port in 0..4u32 {
        let mut st = XINPUT_STATE::default();
        if read_real_pad(port, &mut st) == 0 {
            let b = st.Gamepad.wButtons.0;
            return (
                b & XINPUT_GAMEPAD_A.0 != 0,
                b & XINPUT_GAMEPAD_B.0 != 0,
                b & XINPUT_GAMEPAD_RIGHT_SHOULDER.0 != 0,
            );
        }
    }
    (false, false, false)
}

/// Read the REAL pad for our own navigation. Uses the trampoline when the hook is in
/// (so we bypass our own neutral-pad block); falls back to the raw import otherwise.
fn read_real_pad(index: u32, state: *mut XINPUT_STATE) -> u32 {
    match XINPUT_DETOUR.get() {
        Some(d) => unsafe { d.0.call(index, state) },
        None => unsafe { XInputGetState(index, &mut *state) },
    }
}

/// The GAME reads the keyboard via user32 GetKeyState/GetKeyboardState (and DInput8),
/// NOT GetAsyncKeyState -- which is what OUR nav uses. So hooking these two blocks the
/// game's keyboard while a draft is open without touching our own navigation. While
/// blocked, GetKeyState reports every key up.
unsafe extern "system" fn get_key_state_detour(vk: i32) -> i16 {
    if DRAFT_INPUT_BLOCKED.load(Ordering::Relaxed) {
        return 0; // key up, toggles off
    }
    match GETKEYSTATE_DETOUR.get() {
        Some(d) => d.0.call(vk),
        None => 0,
    }
}

/// While blocked, zero the 256-byte key-state array the game reads, so no key registers.
unsafe extern "system" fn get_keyboard_state_detour(
    lp: *mut u8,
) -> i32 {
    let r = match GETKBDSTATE_DETOUR.get() {
        Some(d) => d.0.call(lp),
        None => return 0,
    };
    if DRAFT_INPUT_BLOCKED.load(Ordering::Relaxed) && !lp.is_null() {
        std::ptr::write_bytes(lp, 0, 256); // all keys up
    }
    r
}

/// The GAME's keyboard movement comes through IDirectInputDevice8::GetDeviceState
/// (the DInput8 keyboard device). While a draft is open we let the real call fill the
/// buffer, then zero it, so the game sees every key up -> no WASD movement, no action.
/// All keyboard device instances share this impl, so hooking it once covers the game.
unsafe extern "system" fn dinput_get_device_state_detour(
    this: *mut core::ffi::c_void,
    cb: u32,
    data: *mut core::ffi::c_void,
) -> windows::core::HRESULT {
    let r = match DINPUT_GDS_DETOUR.get() {
        Some(d) => d.0.call(this, cb, data),
        None => return windows::core::HRESULT(0),
    };
    if DRAFT_INPUT_BLOCKED.load(Ordering::Relaxed) && !data.is_null() {
        // Clamp to the keyboard state size so a bogus cb can never overrun.
        let n = (cb as usize).min(256);
        std::ptr::write_bytes(data as *mut u8, 0, n);
    }
    r
}

/// Install the DInput8 keyboard GetDeviceState detour. We make our OWN throwaway
/// keyboard device only to read the shared vtable's GetDeviceState pointer (the impl
/// lives in dinput8.dll, valid for the process lifetime); we never poll our device.
/// Best-effort: on any failure the keyboard movement stays unfrozen (no crash).
fn install_dinput_keyboard_hook() {
    use windows::core::Interface;
    use windows::Win32::Devices::HumanInterfaceDevice::{
        DirectInput8Create, IDirectInput8W, IDirectInputDevice8W, DIRECTINPUT_VERSION,
        GUID_SysKeyboard,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    unsafe {
        let hinst = match GetModuleHandleW(None) {
            Ok(h) => h,
            Err(e) => {
                hook_log(&format!("DInput hook FAIL: GetModuleHandleW ({e:?})"));
                return;
            }
        };
        let mut di_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        if let Err(e) = DirectInput8Create(
            hinst.into(),
            DIRECTINPUT_VERSION,
            &IDirectInput8W::IID,
            &mut di_ptr,
            None,
        ) {
            hook_log(&format!("DInput hook FAIL: DirectInput8Create ({e:?})"));
            return;
        }
        let di = IDirectInput8W::from_raw(di_ptr);
        let mut dev: Option<IDirectInputDevice8W> = None;
        if let Err(e) = di.CreateDevice(&GUID_SysKeyboard, &mut dev, None) {
            hook_log(&format!("DInput hook FAIL: CreateDevice ({e:?})"));
            return;
        }
        let Some(dev) = dev else {
            hook_log("DInput hook FAIL: CreateDevice returned none");
            return;
        };
        // The shared impl address straight out of the COM vtable slot.
        let target: GetDeviceStateFn = Interface::vtable(&dev).GetDeviceState;
        match retour::GenericDetour::<GetDeviceStateFn>::new(
            target,
            dinput_get_device_state_detour,
        ) {
            Ok(d) => {
                if d.enable().is_ok() && DINPUT_GDS_DETOUR.set(DetourCell(d)).is_ok() {
                    hook_log(&format!(
                        "hook OK: dinput8 keyboard GetDeviceState @ {:p} detoured (movement)",
                        target as *const ()
                    ));
                } else {
                    hook_log("DInput hook FAIL: GetDeviceState enable/set");
                }
            }
            Err(e) => hook_log(&format!("DInput hook FAIL: GetDeviceState new ({e:?})")),
        }
        // Keep the DInput objects alive for the process lifetime (the detour's
        // trampoline points into dinput8 code, but holding these avoids any teardown).
        std::mem::forget(dev);
        std::mem::forget(di);
    }
}

/// Resolve the real input functions and install the detours, once, from `initialize`.
/// On any single failure that one input path simply is not frozen (no crash). Logs each
/// outcome. XInput covers the controller; GetKeyState/GetKeyboardState cover the
/// keyboard; message_filter(InputAll) covers the mouse.
fn install_xinput_hook() {
    use windows::core::s;
    use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
    unsafe {
        // --- controller: xinput1_4!XInputGetState ---
        match GetModuleHandleA(s!("xinput1_4.dll")) {
            Ok(module) => match GetProcAddress(module, s!("XInputGetState")) {
                Some(proc) => {
                    let target: XInputGetStateFn = std::mem::transmute(proc);
                    match retour::GenericDetour::<XInputGetStateFn>::new(
                        target,
                        xinput_get_state_detour,
                    ) {
                        Ok(detour) => {
                            if let Err(e) = detour.enable() {
                                hook_log(&format!("XInput hook FAIL: enable ({e:?})"));
                            } else if XINPUT_DETOUR.set(DetourCell(detour)).is_err() {
                                hook_log("XInput hook FAIL: cell already set");
                            } else {
                                XINPUT_HOOK_INSTALLED.store(true, Ordering::Relaxed);
                                hook_log("hook OK: xinput1_4!XInputGetState detoured (controller)");
                            }
                        }
                        Err(e) => hook_log(&format!("XInput hook FAIL: new ({e:?})")),
                    }
                }
                None => hook_log("XInput hook FAIL: GetProcAddress null"),
            },
            Err(e) => hook_log(&format!("XInput hook FAIL: xinput1_4.dll ({e:?})")),
        }

        // --- keyboard: user32!GetKeyState + user32!GetKeyboardState ---
        let user32 = match GetModuleHandleA(s!("user32.dll")) {
            Ok(m) => m,
            Err(e) => {
                hook_log(&format!("keyboard hook FAIL: user32.dll ({e:?})"));
                return;
            }
        };
        if let Some(proc) = GetProcAddress(user32, s!("GetKeyState")) {
            let target: GetKeyStateFn = std::mem::transmute(proc);
            match retour::GenericDetour::<GetKeyStateFn>::new(target, get_key_state_detour) {
                Ok(d) => {
                    if d.enable().is_ok() && GETKEYSTATE_DETOUR.set(DetourCell(d)).is_ok() {
                        hook_log("hook OK: user32!GetKeyState detoured (keyboard)");
                    } else {
                        hook_log("keyboard hook FAIL: GetKeyState enable/set");
                    }
                }
                Err(e) => hook_log(&format!("keyboard hook FAIL: GetKeyState new ({e:?})")),
            }
        } else {
            hook_log("keyboard hook FAIL: GetKeyState GetProcAddress null");
        }
        if let Some(proc) = GetProcAddress(user32, s!("GetKeyboardState")) {
            let target: GetKeyboardStateFn = std::mem::transmute(proc);
            match retour::GenericDetour::<GetKeyboardStateFn>::new(
                target,
                get_keyboard_state_detour,
            ) {
                Ok(d) => {
                    if d.enable().is_ok() && GETKBDSTATE_DETOUR.set(DetourCell(d)).is_ok() {
                        hook_log("hook OK: user32!GetKeyboardState detoured (keyboard)");
                    } else {
                        hook_log("keyboard hook FAIL: GetKeyboardState enable/set");
                    }
                }
                Err(e) => hook_log(&format!("keyboard hook FAIL: GetKeyboardState new ({e:?})")),
            }
        } else {
            hook_log("keyboard hook FAIL: GetKeyboardState GetProcAddress null");
        }
    }
}

/// Append one line (with a wall-clock timestamp) to a log beside the game so Szefi can
/// SEE exactly when the pad block flips and that the hook is live. Best-effort: a
/// failed write is ignored.
fn hook_log(msg: &str) { release_support::log(msg); }

/// Read the first connected XInput controller into a logical mask (D-pad OR left
/// stick for direction; A = confirm/select, B = back, B+RB = open the draft). We scan
/// ALL FOUR ports, not just port 0: a pad does not always enumerate on slot 0 (Steam
/// Input, a second device, a reconnect can land it on 1-3), and reading only port 0
/// made the nav look dead even with a controller plugged in. Returns 0 when NO port
/// has a controller, so a keyboard-only player is unaffected.
fn read_pad_mask() -> u16 {
    unsafe {
        let mut gamepad = None;
        for port in 0..4u32 {
            let mut st = XINPUT_STATE::default();
            // Read the REAL pad through the detour trampoline, NOT the hooked export:
            // while a draft is open the hook feeds the GAME a neutral pad, but OUR
            // navigation must still see the live buttons. `read_real_pad` routes to the
            // trampoline when the hook is installed, else to the raw import.
            if read_real_pad(port, &mut st) == 0 {
                gamepad = Some(st.Gamepad);
                break;
            }
        }
        let g = match gamepad {
            Some(g) => g,
            None => return 0, // no controller on any port
        };
        let b = g.wButtons.0;
        const DZ: i16 = 12000; // stick dead-zone
        let mut m = 0u16;
        if (b & XINPUT_GAMEPAD_DPAD_LEFT.0) != 0 || g.sThumbLX < -DZ {
            m |= PAD_LEFT;
        }
        if (b & XINPUT_GAMEPAD_DPAD_RIGHT.0) != 0 || g.sThumbLX > DZ {
            m |= PAD_RIGHT;
        }
        if (b & XINPUT_GAMEPAD_DPAD_DOWN.0) != 0 || g.sThumbLY < -DZ {
            m |= PAD_DOWN;
        }
        if (b & XINPUT_GAMEPAD_DPAD_UP.0) != 0 || g.sThumbLY > DZ {
            m |= PAD_UP;
        }
        if (b & XINPUT_GAMEPAD_A.0) != 0 {
            m |= PAD_CONFIRM;
        }
        if (b & XINPUT_GAMEPAD_B.0) != 0 {
            m |= PAD_BACK;
        }
        if (b & XINPUT_GAMEPAD_RIGHT_SHOULDER.0) != 0 {
            m |= PAD_OPEN_RB;
        }
        m
    }
}

/// imgui's `FontId` wraps a `*const Font`, which makes it `!Send`. The pointer is
/// valid for the life of the imgui context and we only touch it on the render
/// thread, so wrapping it to assert `Send`/`Sync` is sound here.
struct SendFontId(imgui::FontId);
unsafe impl Send for SendFontId {}
unsafe impl Sync for SendFontId {}

/// Where the draft flow is right now.
#[derive(PartialEq, Clone, Copy, Debug)]
enum Phase {
    /// Nothing due; overlay quiet.
    Idle,
    /// A level is earned; a pulsing LEVEL UP prompt invites opening the draft.
    Ready,
    /// The card screen is open; player navigates and picks two.
    Draft,
    /// A cursed card was confirmed: the other cards clear, the hidden price is revealed,
    /// then applied, then the draft closes.
    Reveal,
}

/// One smoke "puff" particle for the curse-card aura. It plays the smoke flipbook
/// once (frame 0..SMOKE_FRAMES) while drifting outward and rising from a point on the
/// card's edge, then respawns. Positions are RELATIVE to the card (card-width units,
/// offset from the card centre) so the whole aura tracks the card.
struct Smoke {
    ox: f32,
    oy: f32,
    vx: f32,
    vy: f32,
    size: f32,
    frame: f32,
    speed: f32,
}

/// One background ember (the web `parazs` canvas): a faint gold spark rising from the
/// bottom, swaying sideways, respawning at the bottom when it leaves the top. Positions
/// are in screen pixels. Numbers match IN-GAME-SPEC.md.
struct Ember {
    x: f32,
    y: f32,
    r: f32,
    rise: f32,
    phase: f32,
    sway: f32,
    o: f32,
}

/// The overlay render loop. hudhook calls `render` once per frame.
struct ErRoguelite {
    frames: u64,
    /// Card-frame textures by kind: (kind, (id, width, height)).
    frame_tex: Vec<(CardKind, (TextureId, u32, u32))>,
    /// Confirm-button sprites: [normal, hover, label].
    confirm_tex: Vec<(TextureId, u32, u32)>,
    /// Item medallion icons by iconId: (texture, width, height).
    icon_tex: std::collections::HashMap<u32, (TextureId, u32, u32)>,
    failed_icons: HashSet<u32>,
    /// Curse-card flame flipbook texture (8x8 sprite sheet), drawn behind a curse card.
    smoke_tex: Option<(TextureId, u32, u32)>,
    /// The curse card's finished baked image (drawn instead of a frame + text).
    atok_tex: Option<(TextureId, u32, u32)>,
    /// Live smoke-puff particles for the curse-card aura.
    smoke: Vec<Smoke>,
    /// Tiny PRNG for puff spawn jitter.
    smoke_rng: u64,
    /// Background ember particles (web `parazs`), (re)seeded to the screen size.
    embers: Vec<Ember>,
    /// Tiny PRNG for ember respawn jitter.
    ember_rng: u64,
    /// The item pool (loaded once) and the current drawn hand.
    pools: Option<card_data::Pools>,
    hand: Vec<card_data::CardInstance>,
    rng: u64,
    phase: Phase,
    cursor: usize,
    selected: Vec<usize>,
    /// Keys that were held last frame, for rising-edge detection.
    keys_down: HashSet<i32>,
    /// Controller (XInput) logical button mask from last frame, for rising-edge
    /// detection so the pad navigates/selects the draft like the keyboard does.
    pad_prev: u16,
    /// EB Garamond: headline size and a smaller size for card body text.
    font: Option<SendFontId>,
    font_small: Option<SendFontId>,
    /// Cinzel display serif at three sizes: the LEVEL UP title, card names, stat +N.
    font_title: Option<SendFontId>,
    font_name: Option<SendFontId>,
    font_num: Option<SendFontId>,
    /// Frame the draft opened on, for the open/fade-in animation.
    draft_opened_at: u64,
    /// True while the draft input-freeze is active, so the RESTORE fires exactly once
    /// on close.
    draft_input_held: bool,
    /// The player state the mod has authorized (via card grants). A vanilla Grace
    /// level-up pushes the live level above this; we detect that and revert, so only
    /// cards can raise the level.
    authorized: Option<game::FullState>,
    /// Reveal phase: the frame it began, whether the costs were already applied (once),
    /// and the hidden prices to reveal + pay.
    reveal_at: u64,
    costs_applied: bool,
    pending_costs: Vec<cards::CurseEffect>,
}

impl ErRoguelite {
    fn new() -> Self {
        Self {
            frames: 0,
            frame_tex: Vec::new(),
            confirm_tex: Vec::new(),
            icon_tex: std::collections::HashMap::new(),
            failed_icons: HashSet::new(),
            smoke_tex: None,
            atok_tex: None,
            smoke: Vec::new(),
            smoke_rng: 0x9E37_79B9_7F4A_7C15,
            embers: Vec::new(),
            ember_rng: 0xD1B5_4A32_D192_ED03,
            pools: None,
            hand: Vec::new(),
            rng: entropy_seed(0),
            phase: Phase::Idle,
            cursor: 0,
            selected: Vec::new(),
            keys_down: HashSet::new(),
            pad_prev: 0,
            font: None,
            font_small: None,
            font_title: None,
            font_name: None,
            font_num: None,
            draft_opened_at: 0,
            draft_input_held: false,
            authorized: None,
            reveal_at: 0,
            costs_applied: false,
            pending_costs: Vec::new(),
        }
    }

    /// xorshift -> [0,1) for puff spawn jitter.
    fn smoke_rand(&mut self) -> f32 {
        let mut x = self.smoke_rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.smoke_rng = x;
        ((x >> 40) as f32) / ((1u64 << 24) as f32)
    }

    /// A fresh smoke puff spawned on an ellipse hugging the card edge, drifting outward
    /// and rising (positions in card-width units, offset from the card centre).
    fn spawn_smoke(&mut self) -> Smoke {
        let ang = self.smoke_rand() * std::f32::consts::TAU;
        let rad_x = 0.44 + 0.12 * self.smoke_rand();
        let rad_y = 0.60 + 0.16 * self.smoke_rand();
        let ox = ang.cos() * rad_x;
        let oy = ang.sin() * rad_y;
        let outward = 0.0035 + 0.0035 * self.smoke_rand();
        let vx = ang.cos() * outward + (self.smoke_rand() - 0.5) * 0.004;
        let vy = ang.sin() * outward * 0.6 - (0.003 + 0.004 * self.smoke_rand());
        let size = 0.55 + 0.50 * self.smoke_rand();
        let speed = 0.45 + 0.35 * self.smoke_rand();
        let frame = self.smoke_rand() * 8.0;
        Smoke { ox, oy, vx, vy, size, frame, speed }
    }

    /// Advance the curse-card smoke pool one tick: keep ~20 puffs alive, play each
    /// one's flipbook, drift/rise it, and respawn it when its flipbook has played out.
    fn update_smoke(&mut self) {
        const POOL: usize = 20;
        while self.smoke.len() < POOL {
            let s = self.spawn_smoke();
            self.smoke.push(s);
        }
        let mut dead = Vec::new();
        for (i, s) in self.smoke.iter_mut().enumerate() {
            s.frame += s.speed;
            s.ox += s.vx;
            s.oy += s.vy;
            s.vy -= 0.00012;
            if s.frame >= SMOKE_FRAMES as f32 {
                dead.push(i);
            }
        }
        for i in dead {
            self.smoke[i] = self.spawn_smoke();
        }
    }

    /// Draw a fresh draft hand, mixing in new entropy FIRST so each level-up and each
    /// session deals different cards (the fixed-seed bug dealt identical early hands).
    /// draw_hand already keeps a hand free of duplicate items; this makes the hands
    /// differ from one another too.
    fn fresh_hand(&mut self, hand_size: usize) -> Vec<card_data::CardInstance> {
        self.rng = (self.rng ^ entropy_seed(self.frames)) | 1;
        // Every 5th level reached is a CURSE level: the whole hand is cursed cards
        // (Szefi, 2026-10-24). The level about to be reached is soul_level + 1.
        let next_level = game::read_player_state().map(|s| s.soul_level + 1).unwrap_or(0);
        let curse_level = next_level > 0 && next_level % 5 == 0;
        self.pools
            .as_ref()
            .map(|p| {
                if curse_level {
                    p.draw_curse_hand(hand_size, &mut self.rng)
                } else {
                    p.draw_hand(hand_size, &mut self.rng)
                }
            })
            .unwrap_or_default()
    }

    /// Poll the watched keys once per frame; return the set that just went down.
    ///
    /// When the game window is NOT focused, we still track each key's current state
    /// but emit NO edges -- so keys only ever act while the game is in front (like the
    /// game's own keybinds), and a key held across the alt-tab boundary does not fire a
    /// spurious edge on refocus.
    fn poll_edges(&mut self) -> HashSet<i32> {
        let focused = game_window_focused();
        let mut edges = HashSet::new();
        for &vk in &WATCHED_KEYS {
            let down = key_down(vk);
            if focused && down && !self.keys_down.contains(&vk) {
                edges.insert(vk);
            }
            if down {
                self.keys_down.insert(vk);
            } else {
                self.keys_down.remove(&vk);
            }
        }
        // Controller: same rising-edge + focus rule as the keyboard. Always read the
        // pad (so a button held across alt-tab does not fire on refocus), but only turn
        // transitions into edges while the game is focused.
        let pad = read_pad_mask();
        let prev = self.pad_prev;
        self.pad_prev = pad;
        let pad_edges = if focused { pad & !prev } else { 0 };
        for (bit, vk) in PAD_MAP {
            if pad_edges & bit != 0 {
                edges.insert(vk);
            }
        }
        // Opening the draft takes the B + RB CHORD held together, never a single button:
        // every lone pad button is a real in-game action. This fires once, on the frame
        // the combination becomes complete (so holding it does not re-open). Because B is
        // also the back button, we drop its lone VK_Q edge on the frame the chord
        // completes, so opening the draft does not also register a "back".
        if focused
            && (pad & PAD_OPEN_CHORD) == PAD_OPEN_CHORD
            && (prev & PAD_OPEN_CHORD) != PAD_OPEN_CHORD
        {
            edges.insert(VK_I);
            edges.remove(&VK_Q);
        }
        edges
    }
}

impl ImguiRenderLoop for ErRoguelite {
    fn initialize(&mut self, ctx: &mut imgui::Context, render_context: &mut dyn RenderContext) {
        // Our own font so the overlay reads like the game, not like a dev tool.
        self.font = Some(SendFontId(ctx.fonts().add_font(&[imgui::FontSource::TtfData {
            data: FONT_GARAMOND,
            size_pixels: 30.0,
            config: None,
        }])));
        self.font_small = Some(SendFontId(ctx.fonts().add_font(&[imgui::FontSource::TtfData {
            data: FONT_GARAMOND,
            size_pixels: 23.0,
            config: None,
        }])));
        // Cinzel at three sizes, tuned for 1080p (title ~2.3% of 1920, name ~6% of a
        // 317 px card, +N ~20% of the card). Fixed-atlas imgui can't scale per-draw, so
        // these target the common 1080p; 1440p is a later pass.
        self.font_title = Some(SendFontId(ctx.fonts().add_font(&[imgui::FontSource::TtfData {
            data: FONT_CINZEL,
            size_pixels: 44.0,
            config: None,
        }])));
        self.font_name = Some(SendFontId(ctx.fonts().add_font(&[imgui::FontSource::TtfData {
            data: FONT_CINZEL,
            size_pixels: 25.0,
            config: None,
        }])));
        self.font_num = Some(SendFontId(ctx.fonts().add_font(&[imgui::FontSource::TtfData {
            data: FONT_CINZEL,
            size_pixels: 66.0,
            config: None,
        }])));

        // Upload one frame texture per card kind.
        for (kind, png_bytes) in FRAMES {
            if let Some((rgba, w, h)) = decode_rgba(png_bytes) {
                if let Ok(id) = render_context.load_texture(&rgba, w, h) {
                    self.frame_tex.push((kind, (id, w, h)));
                }
            }
        }

        // Upload the curse-card aura (black-white flame) texture.
        if let Some((rgba, w, h)) = decode_rgba(SMOKE_FX) {
            if let Ok(id) = render_context.load_texture(&rgba, w, h) {
                self.smoke_tex = Some((id, w, h));
            }
        }

        // Upload the curse card's item-free frame (content drawn live over it).
        if let Some((rgba, w, h)) = decode_rgba(ATOK_KERET) {
            if let Ok(id) = render_context.load_texture(&rgba, w, h) {
                self.atok_tex = Some((id, w, h));
            }
        }

        // Confirm-button sprites: [frame, glow, corner, label].
        for png_bytes in [BTN_FRAME, BTN_GLOW, BTN_CORNER, BTN_LABEL] {
            if let Some((rgba, w, h)) = decode_rgba(png_bytes) {
                if let Ok(id) = render_context.load_texture(&rgba, w, h) {
                    self.confirm_tex.push((id, w, h));
                }
            }
        }

        // Parse the item pool.
        self.pools = Some(card_data::load());

        // Install the input detours (one-time) so the GAME gets neutral input while a
        // draft is open: XInput (controller) + user32 (keyboard) + DInput8 keyboard
        // (movement). Our own nav uses GetAsyncKeyState / the trampolines, so it stays
        // live.
        install_xinput_hook();
        install_dinput_keyboard_hook();

        hook_log("HUD initialized; full base-game pool parsed; item icons load on demand.");
    }

    fn before_render<'a>(&'a mut self, _ctx: &mut imgui::Context, context: &'a mut dyn RenderContext) {
        // Only a displayed hand needs icons; do not allocate all 1339 at character creation.
        let needed: Vec<u32> = self.hand.iter().filter_map(|card| card.icon_id).collect();
        for icon in needed {
            if self.icon_tex.contains_key(&icon) || self.failed_icons.contains(&icon) { continue; }
            let Some((_, png)) = icons_gen::ICONS.iter().find(|(id,_)| *id == icon) else { continue; };
            if let Some((rgba,width,height)) = decode_rgba(png) {
                match context.load_texture(&rgba,width,height) {
                    Ok(texture) => { self.icon_tex.insert(icon,(texture,width,height)); }
                    Err(error) => { self.failed_icons.insert(icon); hook_log(&format!("Icon {icon} upload failed: {error:?}")); }
                }
            } else { self.failed_icons.insert(icon); hook_log(&format!("Icon {icon} decode failed")); }
        }
    }

    /// While the card draft is open, swallow keyboard/mouse/raw input so the game
    /// does not receive it: the character stops moving and the camera stops turning,
    /// like having a system menu open. Our own navigation reads GetAsyncKeyState, which
    /// bypasses the message queue, so A/D/E/Q still work.
    fn message_filter(&self, _io: &imgui::Io) -> MessageFilter {
        // Block input ONLY while a draft is open AND the player is in the world.
        // The in-world check is essential: without it, if the world unloads while a
        // draft is open (quit to menu / death), this would keep swallowing ALL input
        // at the main menu so nothing could be clicked, not even Quit.
        if matches!(self.phase, Phase::Draft | Phase::Reveal)
            && game::is_gameplay_screen()
            && game::read_player_state().is_some()
        {
            MessageFilter::InputAll
        } else {
            MessageFilter::empty()
        }
    }

    fn render(&mut self, ui: &mut imgui::Ui) {
        self.frames = self.frames.wrapping_add(1);
        // Player data can exist before the loading screen disappears. Gate both
        // rendering and draft actions on the actual screen state, not its pointer.
        let screen_ready = game::is_gameplay_screen();
        static LAST_SCREEN_READY: AtomicBool = AtomicBool::new(false);
        if LAST_SCREEN_READY.swap(screen_ready, Ordering::Relaxed) != screen_ready || self.frames == 1 {
            hook_log(&format!("Screen gate: gameplay_ready={} status={:?}", screen_ready, game::screen_status()));
        }
        if !screen_ready {
            DRAFT_AVAILABLE.store(false, Ordering::Relaxed);
            DRAFT_INPUT_BLOCKED.store(false, Ordering::Relaxed);
            self.draft_input_held = false;
            if game::screen_status().map(|s| s.0) == Some(256) {
                // A different save may be selected from the main menu. Never carry
                // the previous character's pending hand or authorized stats over.
                self.phase = Phase::Idle;
                self.hand.clear();
                self.selected.clear();
                self.pending_costs.clear();
                self.authorized = None;
            }
            // Pause any pending reveal/animation and consume held-key edges while
            // loading, so a key pressed there cannot open/confirm a card afterwards.
            self.reveal_at = self.reveal_at.saturating_add(1);
            self.draft_opened_at = self.draft_opened_at.saturating_add(1);
            let _ = self.poll_edges();
            return;
        }
        // All overlay text uses EB Garamond for the whole frame.
        let _font_tok = self.font.as_ref().map(|f| ui.push_font(f.0));

        // The developer debug window ("ER Roguelite -- PoC") was removed on
        // 2026-10-23 (Szefi: "úgyis tudjuk hogy működik"). The reader/leveling
        // state now drives the draft overlay directly; nothing is drawn unless a
        // level is actually earned.

        // ---- In-world gate (HARD) -----------------------------------------
        // The draft overlay AND all input manipulation run ONLY when the player is
        // actually in the world. At the main menu / loading screen the player data is
        // null; if we did anything here the overlay would bleed into the main menu and
        // (via message_filter InputAll) block input globally -- which is exactly the
        // regression that let nothing be clicked, not even Quit. So: if not in-world,
        // restore any held input freeze, reset the draft, and draw NOTHING this frame.
        let in_world = game::read_player_state().is_some();
        static LAST_IN_WORLD: AtomicBool = AtomicBool::new(false);
        static LAST_PROGRESS_SAMPLE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);
        static LAST_OUT_OF_WORLD_FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        if LAST_IN_WORLD.swap(in_world, Ordering::Relaxed) != in_world {
            hook_log(if in_world { "World ready: gameplay enabled" } else { "World/preview transition: gameplay disabled" });
        }
        if !in_world {
            let last = LAST_OUT_OF_WORLD_FRAME.load(Ordering::Relaxed);
            if self.frames.saturating_sub(last) >= 300 {
                LAST_OUT_OF_WORLD_FRAME.store(self.frames, Ordering::Relaxed);
                hook_log(&format!("Progress sample: world=false; no valid player snapshot, so rune threshold/card trigger is skipped. Read diagnostic: {}", game::diagnose_player_state()));
            }
            DRAFT_AVAILABLE.store(false, Ordering::Relaxed);
            if self.draft_input_held {
                DRAFT_INPUT_BLOCKED.store(false, Ordering::Relaxed);
                hook_log("pad block OFF (world unloaded while a draft was open)");
                self.draft_input_held = false;
            }
            self.phase = Phase::Idle;
            self.selected.clear();
            self.hand.clear();
            self.authorized = None;
            return;
        }

        // ---- Draft flow state machine -------------------------------------
        // A level is "earned" when held runes reach the vanilla cost of the next
        // level. Idle -> Ready (pulsing prompt) -> Draft (pick 2) -> confirm.
        let edges = self.poll_edges();
        // Public alpha: developer cheat/test hotkeys are intentionally excluded.
        let n = self.hand.len();
        // Cursor positions 0..n are cards; position n is the CONFIRM button.
        let confirm_idx = n;
        let state = game::read_player_state();
        let cost = state
            .map(|st| leveling::rune_cost_for_next_level(st.soul_level))
            .unwrap_or(u64::MAX);
        let ready = state.map(|st| st.runes_held as u64 >= cost).unwrap_or(false);
        if let Some(st) = state {
            let sample = ((st.soul_level as u64) << 32) | st.runes_held as u64;
            let previous = LAST_PROGRESS_SAMPLE.swap(sample, Ordering::Relaxed);
            if previous != sample || self.frames % 300 == 0 {
                hook_log(&format!(
                    "Progress sample: world=true level={} runes={} next_level_cost={} threshold_met={} phase={:?}",
                    st.soul_level, st.runes_held, cost, ready, self.phase
                ));
            }
        }

        // Talisman slots SIZE the level-up draft (Szefi, 2026-10-23): the more talisman
        // slots the character has unlocked (game progression), the wider the choice.
        // `unlocked_talisman_slots` is 0-based here (0 = the base 1 slot, measured on the
        // live game), capped at 3 (4 slots max in ER):
        //   0 -> 3 cards, pick 2    1 -> 4 cards, pick 2
        //   2 -> 5 cards, pick 2    3 -> 5 cards, pick 3 (the 4th slot adds a pick)
        let tal = state.map(|st| st.talisman_slots).unwrap_or(0).min(3) as usize;
        let hand_size = (cards::BASE_HAND_SIZE + tal).min(cards::MAX_HAND_SIZE);
        let pick = if tal >= 3 {
            cards::PICK_COUNT + 1
        } else {
            cards::PICK_COUNT
        };

        let phase_before = self.phase;
        match self.phase {
            Phase::Idle => {
                if ready {
                    // A level is newly earned: generate the hand ONCE, here, for
                    // this level-up event. It then PERSISTS (Szefi, 2026-10-23):
                    // opening/closing the draft shows the same cards, and only a
                    // confirmed pick consumes it (the next level-up draws a new one).
                    if self.hand.is_empty() {
                        self.hand = self.fresh_hand(hand_size);
                        self.selected.clear();
                    }
                    self.phase = Phase::Ready;
                }
            }
            Phase::Ready => {
                if !ready {
                    // Runes dropped below the threshold: fall back to Idle but KEEP
                    // the pending hand so it is not re-rolled when ready again.
                    self.phase = Phase::Idle;
                } else if edges.contains(&VK_I) {
                    // Open the draft with the EXISTING hand -- never re-roll here.
                    if self.hand.is_empty() {
                        // Safety: generate if somehow missing (e.g. pools loaded late).
                        self.hand = self.fresh_hand(hand_size);
                    }
                    if !self.hand.is_empty() {
                        self.phase = Phase::Draft;
                        self.cursor = 0;
                        self.selected.clear();
                        self.draft_opened_at = self.frames;
                    }
                }
            }
            Phase::Draft => {
                if edges.contains(&VK_Q) {
                    self.selected.clear();
                    self.phase = if ready { Phase::Ready } else { Phase::Idle };
                } else {
                    // A/D move across the cards; S jumps down to Confirm, W goes
                    // back up to the cards.
                    if edges.contains(&VK_A) && self.cursor > 0 && self.cursor <= confirm_idx {
                        self.cursor = self.cursor.min(n).saturating_sub(1);
                    }
                    if edges.contains(&VK_D) && self.cursor + 1 < n {
                        self.cursor += 1;
                    }
                    if edges.contains(&VK_S) {
                        self.cursor = confirm_idx;
                    }
                    if edges.contains(&VK_W) && self.cursor == confirm_idx {
                        self.cursor = n.saturating_sub(1);
                    }
                    if edges.contains(&VK_E) {
                        if self.cursor < n {
                            if let Some(pos) =
                                self.selected.iter().position(|&i| i == self.cursor)
                            {
                                self.selected.remove(pos);
                            } else if self.selected.len() < pick {
                                self.selected.push(self.cursor);
                            }
                        } else if self.selected.len() == pick {
                            // Confirm: deduct the level cost, grant the BOONS now (build
                            // package + stat + item), and collect any hidden curse prices
                            // to be revealed and paid in the Reveal phase.
                            if game::grant_level(cost) {
                                self.pending_costs.clear();
                                for &sel in &self.selected {
                                    if let Some(card) = self.hand.get(sel) {
                                        // Build-package boosts (curse cards): +3 x 3 stats.
                                        for &(attr, pts) in &card.pack {
                                            game::add_attribute(attr, pts);
                                        }
                                        // Stat boon (stat card) or item boon (item / curse item).
                                        if let Some((attr, pts)) = card.stat {
                                            game::add_attribute(attr, pts);
                                        } else if card.item_id != 0 {
                                            game::give_item(card.item_id, 1);
                                        }
                                        // Hidden prices: paid at the reveal, not now.
                                        for &c in &card.costs {
                                            self.pending_costs.push(c);
                                        }
                                    }
                                }
                                // Boons are now mod-authorized (Grace-suppression safe).
                                self.authorized = game::read_full_state();

                                if self.pending_costs.is_empty() {
                                    // No curse: close immediately, as before.
                                    self.selected.clear();
                                    let again = game::read_player_state()
                                        .map(|s| {
                                            s.runes_held as u64
                                                >= leveling::rune_cost_for_next_level(s.soul_level)
                                        })
                                        .unwrap_or(false);
                                    self.hand = if again {
                                        self.fresh_hand(hand_size)
                                    } else {
                                        Vec::new()
                                    };
                                    self.cursor = 0;
                                    self.draft_opened_at = self.frames;
                                    self.phase = if again && !self.hand.is_empty() {
                                        Phase::Draft
                                    } else {
                                        Phase::Idle
                                    };
                                } else {
                                    // A curse was taken: reveal the hidden price first.
                                    self.reveal_at = self.frames;
                                    self.costs_applied = false;
                                    self.phase = Phase::Reveal;
                                }
                            }
                        }
                    }
                }
            }
            Phase::Reveal => {
                // The hidden price is revealed, then paid once after a short beat, then
                    // the draft closes (E skips the wait once the price is paid).
                    let elapsed = self.frames.saturating_sub(self.reveal_at);
                    if !self.costs_applied && elapsed >= 90 {
                        for &c in &self.pending_costs {
                            apply_curse(c);
                        }
                        self.costs_applied = true;
                        // Re-baseline so the Grace-suppression keeps the curse's writes.
                        self.authorized = game::read_full_state();
                    }
                    if self.costs_applied && (elapsed >= 220 || edges.contains(&VK_E)) {
                        self.selected.clear();
                        self.pending_costs.clear();
                        let again = game::read_player_state()
                            .map(|s| {
                                s.runes_held as u64
                                    >= leveling::rune_cost_for_next_level(s.soul_level)
                            })
                            .unwrap_or(false);
                        self.hand = if again {
                            self.fresh_hand(hand_size)
                        } else {
                            Vec::new()
                        };
                        self.cursor = 0;
                        self.draft_opened_at = self.frames;
                        self.phase = if again && !self.hand.is_empty() {
                            Phase::Draft
                        } else {
                            Phase::Idle
                        };
                    }
                }
            }

        // ---- Draft input freeze (XInput detour, event-driven) -------------
        // Flip the single atomic the detour reads: draft open -> the GAME gets a
        // neutral pad; draft closed -> the game's pad flows normally. Our own nav reads
        // the real pad via the trampoline regardless. Only the TRANSITION logs, so the
        // log stays readable and there is no per-frame cost. Keyboard/mouse are frozen
        // by message_filter (InputAll) above; this covers the controller.
        let in_draft = matches!(self.phase, Phase::Draft | Phase::Reveal);
        // Let the hook swallow the B+RB open chord from the game while the draft is
        // openable (no roll+attack on open).
        DRAFT_AVAILABLE.store(self.phase == Phase::Ready, Ordering::Relaxed);
        // DRAIN: while open -> blocked. After it closes (confirm or B) -> stay blocked
        // until A/B/RB are RELEASED, so the closing/confirming press never reaches the
        // game (no jump after confirm, no backward roll after B). Then restore.
        let (a_held, b_held, rb_held) = pad_buttons_held();
        let want_block = in_draft || (self.draft_input_held && (a_held || b_held || rb_held));
        if want_block && !self.draft_input_held {
            DRAFT_INPUT_BLOCKED.store(true, Ordering::Relaxed);
            self.draft_input_held = true;
            hook_log(if XINPUT_HOOK_INSTALLED.load(Ordering::Relaxed) {
                "pad block ON (draft open): GAME now reads a neutral XInput pad; nav still live"
            } else {
                "pad block ON (draft open) BUT XInput hook NOT installed -> controller NOT frozen (keyboard still frozen via message_filter)"
            });
        } else if !want_block && self.draft_input_held {
            DRAFT_INPUT_BLOCKED.store(false, Ordering::Relaxed);
            self.draft_input_held = false;
            hook_log("pad block OFF (draft closed + buttons released): GAME reads the controller normally again");
        }

        // ---- Grace level-up suppression -----------------------------------
        // Only cards may raise the level. We track the mod-authorized player state;
        // if the live level climbs above it (a vanilla Grace "Level Up"), we revert
        // level + runes + attributes to the authorized snapshot. Pure memory writes
        // on the same fields the cards use (null-checked + catch_unwind), so it can
        // never crash; a decrease (reload / different save) just re-syncs.
        if let Some(cur) = game::read_full_state() {
            match self.authorized {
                None => self.authorized = Some(cur),
                Some(auth) if cur.soul_level > auth.soul_level => {
                    game::restore_full_state(&auth);
                }
                Some(_) => self.authorized = Some(cur),
            }
        }
        if self.phase != phase_before {
            let state_text = state.map(|st| format!("level={} runes={} threshold_met={}", st.soul_level, st.runes_held, ready)).unwrap_or_else(|| "player_state=unavailable".to_string());
            hook_log(&format!("Draft phase transition: {:?} -> {:?}; {}; hand_cards={}; selected={}", phase_before, self.phase, state_text, self.hand.len(), self.selected.len()));
        }

        // ---- Draft rendering ----------------------------------------------
        let [screen_w, screen_h] = ui.io().display_size;
        // Scale every overlay font to the resolution (sizes are tuned at 1080p). imgui's
        // atlas is fixed-size, so we drive the global font scale; it takes effect on the
        // next frame, which is seamless while the draft stays open. Clamped to sane range.
        {
            let aspect = self.frame_tex.first().map(|t| t.1.2 as f32 / t.1.1 as f32).unwrap_or(1.5);
            let fitted_card_w = (0.17 * screen_w).min(0.56 * screen_h / aspect);
            let fscale = (fitted_card_w / 326.4).clamp(0.1, 8.0);
            let io_ptr = ui.io() as *const imgui::Io as *mut imgui::Io;
            unsafe {
                (*io_ptr).font_global_scale = fscale;
            }
        }
        let bg = ui.get_background_draw_list();

        if self.phase == Phase::Ready {
            let pulse = 0.55 + 0.45 * (self.frames as f32 * 0.09).sin();
            bg.add_text(
                [screen_w * 0.5 - 48.0, screen_h * 0.085],
                [0.98, 0.82, 0.35, pulse],
                "LEVEL UP",
            );
            let prompt = "Press I or B + RB to choose a blessing";
            let _sf = self.font_small.as_ref().map(|f| ui.push_font(f.0));
            let prompt_width = ui.calc_text_size(prompt)[0];
            bg.add_text(
                [(screen_w - prompt_width) * 0.5, screen_h * 0.085 + 24.0],
                [0.92, 0.92, 0.92, pulse],
                prompt,
            );
        }

        if matches!(self.phase, Phase::Draft | Phase::Reveal)
            && n > 0
            && !self.frame_tex.is_empty()
        {
            let (_, fw, fh) = self.frame_tex[0].1;
            let card_w = (0.17 * screen_w).min(0.56 * screen_h * fw as f32 / fh as f32).max(1.0);
            let gap = (0.024 * screen_w).min(0.14 * card_w).max(1.0);
            let card_h = card_w * (fh as f32 / fw as f32);
            let row_w = n as f32 * card_w + (n as f32 - 1.0) * gap;
            let origin_x = (screen_w - row_w) * 0.5;
            let elapsed = self.frames.saturating_sub(self.draft_opened_at) as f32;
            let fade = (elapsed / 12.0).min(1.0);
            let origin_y = (screen_h - card_h) * 0.5 + (1.0 - fade) * 22.0;

            // Solid void background (#0a0806), not the old 0.66 see-through (Szefi, 2026-10-24).
            bg.add_rect([0.0, 0.0], [screen_w, screen_h], [0.039, 0.031, 0.024, fade])
                .filled(true)
                .build();

            // ---- Ember particles (web `parazs`) -------------------------------
            {
                let want = ((screen_w / 22.0).round() as i32).clamp(22, 66) as usize;
                let mut s = self.ember_rng;
                let mut nf = || {
                    s ^= s << 13;
                    s ^= s >> 7;
                    s ^= s << 17;
                    ((s >> 11) as f64 / (1u64 << 53) as f64) as f32
                };
                if self.embers.len() != want {
                    self.embers.clear();
                    for _ in 0..want {
                        self.embers.push(Ember {
                            x: nf() * screen_w,
                            y: nf() * screen_h,
                            r: 0.5 + nf() * 1.5,
                            rise: 0.07 + nf() * 0.28,
                            phase: nf() * 6.2832,
                            sway: 0.04 + nf() * 0.16,
                            o: 0.12 + nf() * 0.45,
                        });
                    }
                }
                for e in self.embers.iter_mut() {
                    e.y -= e.rise;
                    e.phase += 0.012;
                    e.x += e.phase.sin() * e.sway;
                    if e.y < -8.0 {
                        e.y = screen_h + 8.0;
                        e.x = nf() * screen_w;
                        e.r = 0.5 + nf() * 1.5;
                        e.rise = 0.07 + nf() * 0.28;
                        e.o = 0.12 + nf() * 0.45;
                    }
                    let a = e.o * fade;
                    bg.add_circle([e.x, e.y], e.r * 3.4, [0.886, 0.667, 0.337, a * 0.22]).filled(true).build();
                    bg.add_circle([e.x, e.y], e.r * 1.4, [0.886, 0.667, 0.337, a * 0.55]).filled(true).build();
                    bg.add_circle([e.x, e.y], (e.r * 0.6).max(0.6), [0.96, 0.82, 0.55, a]).filled(true).build();
                }
                self.ember_rng = s;
            }

            // Vignette: a warm glow high-centre, darkening to the edges, like the web.
            bg.add_rect_filled_multicolor(
                [0.0, 0.0],
                [screen_w, screen_h],
                [0.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0, 0.0],
                [0.024, 0.02, 0.016, 0.55 * fade],
                [0.024, 0.02, 0.016, 0.55 * fade],
            );

            // ---- Header: LEVEL UP + subtitle + separator ----------------------
            // The "Level 12 reached - runes spent" line is intentionally gone (Szefi).
            {
                let title = game::read_player_state()
                    .map(|state| format!("LEVEL UP {} -> {}", state.soul_level, state.soul_level.saturating_add(1)))
                    .unwrap_or_else(|| "LEVEL UP".to_owned());
                let _tf = self.font_title.as_ref().map(|f| ui.push_font(f.0));
                let sp = 44.0 * 0.13; // 0.13em tracking
                let tw = text_spaced_width(ui, &title, sp);
                let tx = (screen_w - tw) * 0.5;
                let ty = screen_h * 0.05;
                // Soft gold glow behind the title.
                for off in [[-2.0f32, 0.0f32], [2.0, 0.0], [0.0, -2.0], [0.0, 2.0]] {
                    add_text_spaced(&bg, ui, [tx + off[0], ty + off[1]], [0.765, 0.635, 0.298, 0.12 * fade], &title, sp);
                }
                add_text_spaced(&bg, ui, [tx, ty], [0.925, 0.863, 0.651, fade], &title, sp);
            }
            {
                let sub = "Choose two blessings.";
                let _sf = self.font_small.as_ref().map(|f| ui.push_font(f.0));
                let sw = ui.calc_text_size(sub)[0];
                bg.add_text([(screen_w - sw) * 0.5, screen_h * 0.05 + 54.0], [0.420, 0.388, 0.329, fade], sub);
            }
            {
                let cx = screen_w * 0.5;
                let cy = screen_h * 0.05 + 84.0;
                let span = (screen_w * 0.13).min(340.0);
                let line = [0.290, 0.247, 0.157, fade];
                bg.add_line([cx - span, cy], [cx - 11.0, cy], line).thickness(1.0).build();
                bg.add_line([cx + 11.0, cy], [cx + span, cy], line).thickness(1.0).build();
                let d = 4.0;
                let dia = [0.541, 0.459, 0.220, fade];
                bg.add_triangle([cx, cy - d], [cx + d, cy], [cx - d, cy], dia).filled(true).build();
                bg.add_triangle([cx, cy + d], [cx + d, cy], [cx - d, cy], dia).filled(true).build();
            }

            let fallback = self.frame_tex[0].1 .0;
            // Advance the curse-card smoke pool once per frame (before drawing the cards).
            self.update_smoke();
            for (i, card) in self.hand.iter().enumerate() {
                // During the reveal, only the chosen card(s) stay -- the rest vanish.
                if self.phase == Phase::Reveal && !self.selected.contains(&i) {
                    continue;
                }
                let x = origin_x + i as f32 * (card_w + gap);
                let is_cursor = i == self.cursor;
                // Subtle, symmetric grow on hover (no big upward lift, which made the
                // card and its number visibly jump -- Szefi, 2026-10-24).
                let grow = if is_cursor { card_w * 0.035 } else { 0.0 };
                let p_min = [x - grow, origin_y - grow];
                let p_max = [x + card_w + grow, origin_y + card_h + grow];
                let tex = self
                    .frame_tex
                    .iter()
                    .find(|(k, _)| *k == card.kind)
                    .map(|(_, t)| t.0)
                    .unwrap_or(fallback);
                // ---- Curse card: item-free skull frame + its flame + LIVE content ------
                // DYNAMIC (Szefi: the card is only a sample, the game fills it at random
                // each draft). The frame (skull, red frame, nebula, empty medallion,
                // baked CURSED) is a texture; the item icon / name / description / effect
                // are drawn live over it at the JEGYZEK _geometria (fractions of the panel).
                // The flame is ONE instance fitted so the sprite's card rect (145,138,
                // 197,305 of 488x586) maps onto the slot; its own mark, no gold glow.
                if card.kind == CardKind::Curse {
                    let panel_x = p_min[0];
                    let panel_y = p_min[1];
                    let panel_w = p_max[0] - p_min[0];
                    let panel_h = p_max[1] - p_min[1];
                    let ccx0 = panel_x + panel_w * 0.5;

                    // Item-free frame: canvas taller than panel, skull overflows up.
                    // (The black flame was dropped on 2026-10-24 -- Szefi: it never read
                    // right on pure black; a red selection glow marks this card instead.)
                    if let Some((atex, _, _)) = self.atok_tex {
                        let kscale = panel_w / KERET_PANEL_W;
                        let dx = panel_x - KERET_PANEL_X * kscale;
                        let dy = panel_y - KERET_PANEL_Y * kscale;
                        let dw = KERET_CANVAS_W * kscale;
                        let dh = KERET_CANVAS_H * kscale;
                        bg.add_image(atex, [dx, dy], [dx + dw, dy + dh])
                            .col([1.0, 1.0, 1.0, fade])
                            .build();
                    }

                    // Live medallion icon (centre 0.50/0.30 of the panel, box 0.345 wide).
                    if let Some(id) = card.icon_id {
                        if let Some(&(itex, _, _)) = self.icon_tex.get(&id) {
                            let d = panel_w * 0.345;
                            let mcx = panel_x + panel_w * 0.50;
                            let mcy = panel_y + panel_h * 0.30;
                            bg.add_image(itex, [mcx - d * 0.5, mcy - d * 0.5], [mcx + d * 0.5, mcy + d * 0.5])
                                .col([1.0, 1.0, 1.0, fade])
                                .build();
                        }
                    }
                    // The visible boon: a big +N in the medallion (the cursed blessing's
                    // reward; shown like a stat card since an attribute boon has no icon).
                    if let Some((_, pts)) = card.stat {
                        let _bf = self.font_num.as_ref().map(|f| ui.push_font(f.0));
                        let s = format!("+{pts}");
                        let ts = ui.calc_text_size(&s);
                        let mcx = panel_x + panel_w * 0.50;
                        let mcy = panel_y + panel_h * 0.30;
                        bg.add_text([mcx - ts[0] * 0.5, mcy - ts[1] * 0.5], [0.925, 0.910, 0.871, fade], &s);
                    }
                    // Name (Cinzel, #e9dbb8, tracking 0.06em, centre y 0.527).
                    {
                        let _nf = self.font_name.as_ref().map(|f| ui.push_font(f.0));
                        let sp = 20.0 * 0.06;
                        let nw = text_spaced_width(ui, &card.name, sp);
                        let nh = ui.calc_text_size(&card.name)[1];
                        add_text_spaced(&bg, ui, [ccx0 - nw * 0.5, panel_y + panel_h * 0.527 - nh * 0.5], [0.914, 0.859, 0.722, fade], &card.name, sp);
                    }
                    // Boon: the build-stat package (gold) + a short flavor line; the red
                    // "Price hidden" marks there is a cost (shown only after Confirm).
                    {
                        let _sf = self.font_small.as_ref().map(|f| ui.push_font(f.0));
                        if !card.pack.is_empty() {
                            let abbr = ["Vig", "Mnd", "End", "Str", "Dex", "Int", "Fth", "Arc"];
                            let names: Vec<&str> = card
                                .pack
                                .iter()
                                .map(|&(a, _)| abbr.get(a).copied().unwrap_or("?"))
                                .collect();
                            let pts = card.pack.first().map(|&(_, p)| p).unwrap_or(3);
                            let s = format!("+{pts} {}", names.join(" / "));
                            let w = ui.calc_text_size(&s)[0];
                            bg.add_text([ccx0 - w * 0.5, panel_y + panel_h * 0.655], [0.765, 0.635, 0.298, fade], &s);
                        }
                        let tw = panel_w * 0.84;
                        if let Some(line) = wrap_text(ui, &card.desc, tw, 2).first() {
                            let w = ui.calc_text_size(line)[0];
                            bg.add_text([ccx0 - w * 0.5, panel_y + panel_h * 0.715], [0.639, 0.596, 0.502, fade], line);
                        }
                        let eff = effect_label(card);
                        if !eff.is_empty() {
                            let w = ui.calc_text_size(&eff)[0];
                            bg.add_text([ccx0 - w * 0.5, panel_y + panel_h * 0.833], [0.776, 0.173, 0.196, fade], &eff);
                        }
                    }

                    // Selection glow: the curse card glows RED (not gold), on hover and
                    // when selected (Szefi, 2026-10-24). Red 226,60,55, a shade lighter
                    // than the frame's 198,44,50 so it reads as a glow, not the border.
                    if self.selected.contains(&i) {
                        for k in 0..6 {
                            let g = 1.0 + k as f32 * 3.0;
                            let a = 0.42 * (1.0 - k as f32 / 6.0) * fade;
                            bg.add_rect([panel_x - g, panel_y - g], [p_max[0] + g, p_max[1] + g], [0.95, 0.30, 0.27, a])
                                .rounding(4.0)
                                .thickness(3.0)
                                .build();
                        }
                        let d = panel_w * 0.09;
                        let ccx = p_max[0] - panel_w * 0.11 - d * 0.5;
                        let ccy = panel_y + panel_h * 0.062 + d * 0.5;
                        bg.add_circle([ccx, ccy], d * 0.5, [0.071, 0.055, 0.035, 0.92 * fade]).filled(true).build();
                        bg.add_circle([ccx, ccy], d * 0.5, [0.925, 0.863, 0.651, fade]).thickness(1.5).build();
                        let _cf = self.font_small.as_ref().map(|f| ui.push_font(f.0));
                        let chk = "\u{2713}";
                        let cs = ui.calc_text_size(chk);
                        bg.add_text([ccx - cs[0] * 0.5, ccy - cs[1] * 0.5], [0.925, 0.863, 0.651, fade], chk);
                    } else if is_cursor {
                        for k in 0..4 {
                            let g = 1.0 + k as f32 * 3.0;
                            let a = 0.30 * (1.0 - k as f32 / 4.0) * fade;
                            bg.add_rect([panel_x - g, panel_y - g], [p_max[0] + g, p_max[1] + g], [0.95, 0.30, 0.27, a])
                                .rounding(4.0)
                                .thickness(2.5)
                                .build();
                        }
                    }
                    continue;
                }
                bg.add_image(tex, p_min, p_max)
                    .col([1.0, 1.0, 1.0, fade])
                    .build();

                // Card body text, in the small font, centred like the design.
                let cx = (p_min[0] + p_max[0]) * 0.5;
                let drawn_w = p_max[0] - p_min[0];
                let drawn_h = p_max[1] - p_min[1];

                // Item icon in the medallion (geometry.json: centre at 0.30 of the
                // card height, diameter 0.30 of the card width). Item cards only;
                // stat cards draw the big +N below instead. No icon -> empty circle.
                if let Some(id) = card.icon_id {
                    if let Some(&(itex, _, _)) = self.icon_tex.get(&id) {
                        let d = drawn_w * 0.30;
                        let mcy = p_min[1] + drawn_h * 0.30;
                        bg.add_image(itex, [cx - d * 0.5, mcy - d * 0.5], [cx + d * 0.5, mcy + d * 0.5])
                            .col([1.0, 1.0, 1.0, fade])
                            .build();
                    }
                }
                let text_w = (p_max[0] - p_min[0]) * 0.84;
                // Type, description, effect and rarity in Garamond (per IN-GAME-SPEC y/colours).
                {
                    let _sf = self.font_small.as_ref().map(|f| ui.push_font(f.0));
                    let centred = |s: &str, y: f32, col: [f32; 4]| {
                        let tw = ui.calc_text_size(s)[0];
                        bg.add_text([cx - tw * 0.5, y], col, s);
                    };
                    // Type (0.3em tracking), y 7.5%.
                    let tl = type_label(card.kind);
                    let tsp = 23.0 * 0.3;
                    let tlw = text_spaced_width(ui, tl, tsp);
                    add_text_spaced(&bg, ui, [cx - tlw * 0.5, p_min[1] + drawn_h * 0.075], [0.420, 0.388, 0.329, fade], tl, tsp);
                    // Larger, card-scaled description. Centre the whole wrapped block so
                    // one-line and three-line curses stay balanced and clear of the effect.
                    let desc_lines = wrap_text(ui, &card.desc, text_w, 3);
                    let line_h = ui.calc_text_size("Ag")[1];
                    let line_step = line_h * 1.28;
                    let block_h = line_h + line_step * desc_lines.len().saturating_sub(1) as f32;
                    let first_y = p_min[1] + drawn_h * 0.68 - block_h * 0.5;
                    for (li, line) in desc_lines.iter().enumerate() {
                        centred(line, first_y + li as f32 * line_step, [0.631, 0.592, 0.498, fade]);
                    }
                    // Effect line (gold), y 82%.
                    let eff = effect_label(card);
                    if !eff.is_empty() {
                        centred(&eff, p_min[1] + drawn_h * 0.82 - ui.calc_text_size(&eff)[1] * 0.5, [0.765, 0.635, 0.298, fade]);
                    }
                    // Rarity sits ABOVE the baked footer line (y 88.1%) and diamond (92.5%).
                    // Use actual drawn height, including hover growth; vertically center text.
                    let rl = rarity_label(card.rarity);
                    let rsp = 23.0 * 0.28;
                    let rc = rarity_color(card.rarity);
                    let rlw = text_spaced_width(ui, rl, rsp);
                    add_text_spaced(&bg, ui, [cx - rlw * 0.5, p_min[1] + drawn_h * 0.858 - ui.calc_text_size(rl)[1] * 0.5], [rc[0], rc[1], rc[2], fade], rl, rsp);
                }
                // Card name in Cinzel (gold-hi), centred at y 50.5%.
                {
                    let _nf = self.font_name.as_ref().map(|f| ui.push_font(f.0));
                    let ns = ui.calc_text_size(&card.name);
                    bg.add_text([cx - ns[0] * 0.5, p_min[1] + drawn_h * 0.505 - ns[1] * 0.5], [0.925, 0.863, 0.651, fade], &card.name);
                }
                // Stat card: big +N in the medallion, Cinzel (#ece8de), centred at 30%.
                if let Some((_, pts)) = card.stat {
                    let _bf = self.font_num.as_ref().map(|f| ui.push_font(f.0));
                    let s = format!("+{pts}");
                    let ts = ui.calc_text_size(&s);
                    bg.add_text([cx - ts[0] * 0.5, p_min[1] + drawn_h * 0.30 - ts[1] * 0.5], [0.925, 0.910, 0.871, fade], &s);
                }

                // Highlight: gold glow on hover / selected. The curse card has NO gold
                // highlight -- its own black-white flame marks it (Szefi, 2026-10-24).
                let is_curse = card.kind == CardKind::Curse;
                if self.selected.contains(&i) {
                    // Gold glow only on the normal cards; the curse card is marked by its
                    // flame, so it gets the checkmark but no gold glow.
                    if !is_curse {
                        for k in 0..6 {
                            let g = 1.0 + k as f32 * 3.0;
                            let a = 0.40 * (1.0 - k as f32 / 6.0) * fade;
                            bg.add_rect([p_min[0] - g, p_min[1] - g], [p_max[0] + g, p_max[1] + g], [0.96, 0.84, 0.48, a])
                                .rounding(4.0)
                                .thickness(3.0)
                                .build();
                        }
                    }
                    // Checkmark disc, top-right (diameter 9% of card width).
                    let d = card_w * 0.09;
                    let ccx = p_max[0] - card_w * 0.11 - d * 0.5;
                    let ccy = p_min[1] + card_h * 0.062 + d * 0.5;
                    bg.add_circle([ccx, ccy], d * 0.5, [0.071, 0.055, 0.035, 0.92 * fade]).filled(true).build();
                    bg.add_circle([ccx, ccy], d * 0.5, [0.925, 0.863, 0.651, fade]).thickness(1.5).build();
                    let _cf = self.font_small.as_ref().map(|f| ui.push_font(f.0));
                    let chk = "\u{2713}";
                    let cs = ui.calc_text_size(chk);
                    bg.add_text([ccx - cs[0] * 0.5, ccy - cs[1] * 0.5], [0.925, 0.863, 0.651, fade], chk);
                } else if is_cursor && !is_curse {
                    for k in 0..4 {
                        let g = 1.0 + k as f32 * 3.0;
                        let a = 0.28 * (1.0 - k as f32 / 4.0) * fade;
                        bg.add_rect([p_min[0] - g, p_min[1] - g], [p_max[0] + g, p_max[1] + g], [0.96, 0.84, 0.48, a])
                            .rounding(4.0)
                            .thickness(2.5)
                            .build();
                    }
                }
            }

            // The confirm button + key hints show during the live draft; during the
            // reveal this lower area shows the hidden price instead.
            if self.phase == Phase::Draft {
            // Centre CONFIRM button, drawn with the game's own menu-button sprite
            // (Aurora extracted it) so it blends in; code rect is a fallback.
            let on_confirm = self.cursor == confirm_idx;
            let can_confirm = self.selected.len() == pick;
            let btn_w = card_w * 1.1;
            let has_sprites = self.confirm_tex.len() >= 4;
            let btn_h = if has_sprites {
                let (_, bw, bh) = self.confirm_tex[0];
                btn_w * (bh as f32 / bw as f32)
            } else {
                card_h * 0.12
            };
            let btn_x = (screen_w - btn_w) * 0.5;
            let btn_y = origin_y + card_h + 28.0 * ui.io().font_global_scale;
            let alpha = (if can_confirm { 1.0 } else { 0.45 }) * fade;

            if has_sprites {
                let hover = on_confirm && can_confirm;
                // Hover glow sits slightly outside the frame.
                if hover {
                    let (gid, gw, gh) = self.confirm_tex[1];
                    let pad = btn_h * 0.12;
                    draw_hslice(&bg, gid, gw, gh, 108.0, 109.0, btn_x - pad, btn_y - pad, btn_w + 2.0 * pad, btn_h + 2.0 * pad, alpha);
                }
                // The button frame.
                let (fid, fw, fh) = self.confirm_tex[0];
                draw_hslice(&bg, fid, fw, fh, 144.0, 143.0, btn_x, btn_y, btn_w, btn_h, alpha);
                // Hover corner marks on top of the frame.
                if hover {
                    let (cid, cw, ch) = self.confirm_tex[2];
                    draw_hslice(&bg, cid, cw, ch, 38.0, 40.0, btn_x, btn_y, btn_w, btn_h, alpha);
                }
                // "Confirm" label, centred.
                let (lid, lw, lh) = self.confirm_tex[3];
                let ls = (btn_h * 0.55) / lh as f32;
                let (lw2, lh2) = (lw as f32 * ls, lh as f32 * ls);
                let lx = btn_x + (btn_w - lw2) * 0.5;
                let ly = btn_y + (btn_h - lh2) * 0.5;
                bg.add_image(lid, [lx, ly], [lx + lw2, ly + lh2])
                    .col([1.0, 1.0, 1.0, alpha])
                    .build();
            } else {
                let c = if on_confirm {
                    [0.22, 0.45, 0.25, alpha]
                } else {
                    [0.16, 0.30, 0.18, alpha]
                };
                bg.add_rect([btn_x, btn_y], [btn_x + btn_w, btn_y + btn_h], c)
                    .filled(true)
                    .build();
            }
            // Key hints, centred under the confirm button, in the small font so they do
            // not collide (the Confirm button already shows the live count).
            {
                let _sf = self.font_small.as_ref().map(|f| ui.push_font(f.0));
                let hint = "A / D  Move      E  Select      Q  Back";
                let hw = ui.calc_text_size(hint)[0];
                bg.add_text(
                    [(screen_w - hw) * 0.5, btn_y + btn_h + 14.0 * ui.io().font_global_scale],
                    [0.420, 0.388, 0.329, fade],
                    hint,
                );
            }
            } else {
                // ---- Reveal: the hidden price the cursed card exacts ----------
                let cx = screen_w * 0.5;
                let base_y = origin_y + card_h + 26.0;
                {
                    let _tf = self.font_name.as_ref().map(|f| ui.push_font(f.0));
                    let title = "THE CURSE";
                    let sp = 20.0 * 0.1;
                    let tw = text_spaced_width(ui, title, sp);
                    add_text_spaced(&bg, ui, [cx - tw * 0.5, base_y], [0.90, 0.24, 0.22, fade], title, sp);
                }
                {
                    let _sf = self.font_small.as_ref().map(|f| ui.push_font(f.0));
                    for (li, c) in self.pending_costs.iter().enumerate() {
                        let s = c.label();
                        let w = ui.calc_text_size(s)[0];
                        bg.add_text(
                            [cx - w * 0.5, base_y + 34.0 + li as f32 * 22.0],
                            [0.86, 0.80, 0.70, fade],
                            s,
                        );
                    }
                    let n_lines = self.pending_costs.len() as f32;
                    let hint = if self.costs_applied { "E  Continue" } else { "..." };
                    let hw = ui.calc_text_size(hint)[0];
                    bg.add_text(
                        [cx - hw * 0.5, base_y + 44.0 + n_lines * 22.0],
                        [0.420, 0.388, 0.329, fade],
                        hint,
                    );
                }
            }
        }
    }
}

/// Spawned off DllMain so we never do real work on the loader lock.
/// `hmodule_addr` is the raw module address (HINSTANCE is a raw pointer and not
/// `Send`, so we move it across the thread boundary as an integer).
// Fail closed before any game-memory hooks on unsupported executable versions.
fn supported_game_version() -> bool {
    use std::os::windows::ffi::OsStrExt;
    use std::ffi::c_void;
    #[link(name = "version")]
    extern "system" {
        fn GetFileVersionInfoSizeW(path: *const u16, handle: *mut u32) -> u32;
        fn GetFileVersionInfoW(path: *const u16, handle: u32, size: u32, data: *mut c_void) -> i32;
        fn VerQueryValueW(data: *const c_void, block: *const u16, value: *mut *mut c_void, len: *mut u32) -> i32;
    }
    let Ok(path) = std::env::current_exe() else { return false; };
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let mut handle = 0;
        let size = GetFileVersionInfoSizeW(path.as_ptr(), &mut handle);
        if size == 0 { return false; }
        let mut data = vec![0u8; size as usize];
        if GetFileVersionInfoW(path.as_ptr(), 0, size, data.as_mut_ptr().cast()) == 0 { return false; }
        let mut value = std::ptr::null_mut();
        let mut len = 0;
        let block = [92u16, 0];
        if VerQueryValueW(data.as_ptr().cast(), block.as_ptr(), &mut value, &mut len) == 0 || value.is_null() || len < 52 { return false; }
        let words = value.cast::<u32>();
        std::ptr::read_unaligned(words) == 0xfeef04bd
            && std::ptr::read_unaligned(words.add(2)) == ((2 << 16) | 7)
            && std::ptr::read_unaligned(words.add(3)) == (2 << 16)
    }
}

fn start(hmodule_addr: isize) {
    if !supported_game_version() {
        hook_log("Unsupported game executable. This alpha requires FileVersion 2.7.2.0; no hooks installed.");
        return;
    }
    if !release_support::initialize() {
        hook_log("STOP: separate save profile could not be enabled; no gameplay hooks installed.");
        std::process::exit(1);
    }
    if std::env::var("ER_ROGUELITE_DIAGNOSTIC_MODE").as_deref() == Ok("save-only") {
        hook_log("Diagnostic save-only mode: HUD and gameplay hooks disabled.");
        return;
    }
    let hmodule = HINSTANCE(hmodule_addr as *mut _);
    if let Err(e) = Hudhook::builder()
        .with::<ImguiDx12Hooks>(ErRoguelite::new())
        .with_hmodule(hmodule)
        .build()
        .apply()
    {
        // No tracing subscriber guaranteed this early; keep it best-effort.
        eprintln!("[er-roguelite] failed to apply hooks: {e:?}");
    }
}

/// DLL entry point. On attach we spin up a thread that installs the overlay hook.
#[no_mangle]
pub extern "system" fn DllMain(hmodule: HINSTANCE, reason: u32, _reserved: *mut ()) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        let addr = hmodule.0 as isize;
        std::thread::spawn(move || start(addr));
    }
    TRUE_I32
}

