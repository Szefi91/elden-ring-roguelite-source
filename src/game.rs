//! Reading live Elden Ring player state (runes + soul level).
//!
//! We deliberately DO NOT use the crate's `GameDataMan::instance()`: its runtime
//! version detection reads the loaded module's version resource via pelite, and
//! that path panics in-process on this setup (poisoning its LazyLock) even though
//! the version is correct (2.7.2.0, verified offline). So we resolve the singleton
//! ourselves from the known 2.7.2.0 RVA and reuse the crate's struct types only for
//! their (repr-C) field layout. Version-locked to 2.7.2.0; re-check on a game patch.

use eldenring::cs::{ChrIns, GameDataMan, ItemCategory, PlayerGameData};
use windows::core::PCWSTR;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{VirtualQuery, MEMORY_BASIC_INFORMATION};

/// GameDataMan static slot RVA for Elden Ring 2.7.2.0 (ww), taken from
/// eldenring-rs `rva_ww::RVAS.game_data_man`. The slot holds a nullable pointer
/// to the GameDataMan singleton.
const GAME_DATA_MAN_RVA: usize = 0x3d64fb8;

#[derive(Clone, Copy, Debug, Default)]
pub struct PlayerState {
    pub soul_level: u32,
    pub runes_held: u32,
    /// Unlocked talisman slots (1 at start, +1 per Talisman Pouch, up to 4). A rise in
    /// this count is what triggers a BONUS draft (Szefi, 2026-10-23: each pouch -> +1 card).
    pub talisman_slots: u8,
}

/// Cached base of eldenring.exe. GetModuleHandleW is called at most once; every
/// per-frame reader reuses the cached value (GetModuleHandleW per-frame can stall on
/// the loader lock, and we want the hot path syscall-free).
static MODULE_BASE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[inline]
fn module_base() -> usize {
    use std::sync::atomic::Ordering;
    let b = MODULE_BASE.load(Ordering::Relaxed);
    if b != 0 {
        return b;
    }
    let nb = unsafe { GetModuleHandleW(PCWSTR::null()) }
        .map(|h| h.0 as usize)
        .unwrap_or(0);
    MODULE_BASE.store(nb, Ordering::Relaxed);
    nb
}

/// Cheap, syscall-free sanity for a heap pointer we are about to dereference: non-null,
/// 8-aligned, and in the canonical user-mode range. Game-managed singleton slots hold
/// either null or a valid pointer, so this is enough to guard the hot-path derefs
/// without VirtualQuery (which is multi-millisecond in this process's huge address
/// space -- calling it per frame is what tanked the FPS).
#[inline]
fn plausible_ptr(p: *const u8) -> bool {
    let a = p as usize;
    a >= 0x1_0000 && a < 0x7FFF_FFFF_FFFF && (a & 7) == 0
}

#[inline]
unsafe fn game_data_man() -> Option<&'static GameDataMan> {
    let base = module_base();
    if base == 0 {
        return None;
    }
    let slot = (base + GAME_DATA_MAN_RVA) as *const *mut GameDataMan;
    let gdm = *slot;
    if gdm.is_null() {
        return None;
    }
    Some(&*gdm)
}

/// WorldChrMan static slot (2.7.2.0, offline-derived, two AOB variants agreed:
/// scripts WorldChrMan scan) and the offset of its `main_player` (local PlayerIns).
/// `main_player` is null at the main menu / loading and non-null ONLY when a real
/// character is loaded in the world, so it is the definitive "in-world" signal.
const WORLD_CHR_MAN_SLOT_RVA: usize = 0x3d6d018;
const WCM_MAIN_PLAYER_OFF: usize = 0x1e508;
/// Elden Ring's hard soul-level cap; used as a sanity bound against stale/garbage.
const MAX_SOUL_LEVEL: u32 = 713;

/// True ONLY when a real player character is loaded in the world. This is the fix for
/// the "overlay in the main menu / false level-up with no character" regression: the
/// GameDataMan -> PlayerGameData chain can stay non-null with stale values at the menu,
/// so we must NOT trust it alone. WorldChrMan.main_player is null at menu/loading and
/// non-null in-world.
///
/// HOT PATH: this runs every frame, so it is syscall-free -- no VirtualQuery (that is
/// multi-millisecond here and tanked the FPS). It reads the module-static WorldChrMan
/// slot (always mapped) and does two guarded derefs, exactly like `game_data_man`
/// (which has always been VQ-free and crash-free). `plausible_ptr` rejects obvious
/// garbage before each deref. On any doubt it returns false (safer to NOT trigger the
/// draft at the menu than to trigger it wrongly).
/// Read through the OS instead of dereferencing a possibly released creation/preview object.
/// catch_unwind cannot catch a native access violation; ReadProcessMemory fails safely.
fn read_memory<T: Copy>(address: usize) -> Option<T> {
    use std::{ffi::c_void, mem::MaybeUninit};
    #[link(name="kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn ReadProcessMemory(process: *mut c_void, address: *const c_void, buffer: *mut c_void, bytes: usize, read: *mut usize) -> i32;
    }
    if address == 0 { return None; }
    let mut value = MaybeUninit::<T>::uninit();
    let mut read = 0;
    unsafe {
        if ReadProcessMemory(GetCurrentProcess(), address as *const c_void, value.as_mut_ptr().cast(), std::mem::size_of::<T>(), &mut read) == 0 || read != std::mem::size_of::<T>() { return None; }
        Some(value.assume_init())
    }
}

pub fn is_in_world() -> bool {
    let base = module_base();
    if base == 0 { return false; }
    let Some(wcm) = read_memory::<usize>(base + WORLD_CHR_MAN_SLOT_RVA) else { return false; };
    if !plausible_ptr(wcm as *const u8) { return false; }
    let Some(player) = read_memory::<usize>(wcm + WCM_MAIN_PLAYER_OFF) else { return false; };
    plausible_ptr(player as *const u8)
}

// Elden Ring 2.7.2.0: CSMenuMan slot resolved from the unique RIP-relative
// instruction sequence at RVA 0x81e04c in the supported executable.
// Screen status at +0x730: 0 gameplay, 1 loading, 256 main menu.
pub fn screen_status() -> Option<(i32, u32)> {
    let menu = read_memory::<usize>(module_base() + 0x3d72840)?;
    if !plausible_ptr(menu as *const u8) { return None; }
    Some((read_memory::<i32>(menu + 0x730)?, read_memory::<u32>(menu + 0x18)?))
}

pub fn is_gameplay_screen() -> bool {
    screen_status().map(|(screen, flags)| {
        let blackscreen = flags & 1 != 0 && flags & 0x100 == 0 && flags & 0x10000 != 0;
        screen == 0 && !blackscreen
    }).unwrap_or(false)
}

/// Safe snapshot of fields used by the overlay. No references to transient preview data.
pub fn read_player_state() -> Option<PlayerState> {
    if !is_in_world() { return None; }
    let gdm = read_memory::<usize>(module_base() + GAME_DATA_MAN_RVA)?;
    if !plausible_ptr(gdm as *const u8) { return None; }
    let field = gdm + std::mem::offset_of!(GameDataMan, main_player_game_data);
    let pgd = read_memory::<usize>(field)?;
    if !plausible_ptr(pgd as *const u8) { return None; }
    let data = read_memory::<[u8; 0x100]>(pgd)?;
    // Do not gate on PlayerGameData::is_my_world: on the user's offline save this
    // field was 0 even while WorldChrMan.main_player and the stable live player-data
    // pointer were valid. The live WorldChrMan player pointer is the world gate.
    let scalar = |offset: usize| u32::from_le_bytes(data[offset..offset+4].try_into().unwrap());
    let level = scalar(std::mem::offset_of!(PlayerGameData, level));
    if level == 0 || level > MAX_SOUL_LEVEL { return None; }
    if read_memory::<usize>(field)? != pgd { return None; } // character changed during snapshot
    Some(PlayerState { soul_level: level,
        runes_held: scalar(std::mem::offset_of!(PlayerGameData, rune_count)),
        talisman_slots: data[std::mem::offset_of!(PlayerGameData, unlocked_talisman_slots)] })
}

/// Slow diagnostic for the periodic log only. Reports which live-memory gate failed
/// without dumping save contents or dereferencing untrusted addresses directly.
pub fn diagnose_player_state() -> String {
    let base = module_base();
    if base == 0 { return "module_base=0".into(); }
    let Some(wcm) = read_memory::<usize>(base + WORLD_CHR_MAN_SLOT_RVA) else {
        return format!("wcm_slot_read_failed slot=0x{:x}", base + WORLD_CHR_MAN_SLOT_RVA);
    };
    if !plausible_ptr(wcm as *const u8) { return format!("wcm_ptr_invalid value=0x{wcm:x}"); }
    let Some(player) = read_memory::<usize>(wcm + WCM_MAIN_PLAYER_OFF) else {
        return format!("main_player_read_failed wcm=0x{wcm:x} offset=0x{WCM_MAIN_PLAYER_OFF:x}");
    };
    if !plausible_ptr(player as *const u8) { return format!("main_player_ptr_invalid wcm=0x{wcm:x} value=0x{player:x}"); }
    let Some(gdm) = read_memory::<usize>(base + GAME_DATA_MAN_RVA) else {
        return format!("gdm_slot_read_failed slot=0x{:x} main_player=0x{player:x}", base + GAME_DATA_MAN_RVA);
    };
    if !plausible_ptr(gdm as *const u8) { return format!("gdm_ptr_invalid value=0x{gdm:x} main_player=0x{player:x}"); }
    let field = gdm + std::mem::offset_of!(GameDataMan, main_player_game_data);
    let Some(pgd) = read_memory::<usize>(field) else { return format!("pgd_slot_read_failed field=0x{field:x}"); };
    if !plausible_ptr(pgd as *const u8) { return format!("pgd_ptr_invalid value=0x{pgd:x} field=0x{field:x}"); }
    let Some(data) = read_memory::<[u8; 0x100]>(pgd) else { return format!("pgd_snapshot_read_failed pgd=0x{pgd:x}"); };
    let is_my_world = data[std::mem::offset_of!(PlayerGameData, is_my_world)];
    let level = u32::from_le_bytes(data[std::mem::offset_of!(PlayerGameData, level)..][..4].try_into().unwrap());
    let runes = u32::from_le_bytes(data[std::mem::offset_of!(PlayerGameData, rune_count)..][..4].try_into().unwrap());
    let pgd_after = read_memory::<usize>(field).unwrap_or(0);
    format!("snapshot_ok main_player=0x{player:x} pgd=0x{pgd:x} pgd_after=0x{pgd_after:x} is_my_world={is_my_world} level={level} runes={runes} level_valid={} pgd_stable={}", level > 0 && level <= MAX_SOUL_LEVEL, pgd == pgd_after)
}

/// Mutable pointer to the live player data, or None at menu / loading.
unsafe fn player_ptr() -> Option<*mut PlayerGameData> {
    let gdm = game_data_man()?;
    let p = gdm.main_player_game_data.as_ptr();
    if p.is_null() {
        None
    } else {
        Some(p)
    }
}

/// Apply one level-up: deduct `cost` runes and raise the soul level by one.
/// Returns true on success (enough runes, player loaded). WRITE path -- test on a
/// throwaway character first.
pub fn grant_level(cost: u64) -> bool {
    std::panic::catch_unwind(|| unsafe {
        let Some(p) = player_ptr() else { return false };
        let pgd = &mut *p;
        if (pgd.rune_count as u64) < cost {
            return false;
        }
        pgd.rune_count = (pgd.rune_count as u64 - cost) as u32;
        pgd.level = pgd.level.saturating_add(1);
        true
    })
    .unwrap_or(false)
}

// Item-give (ItemGib). RVAs confirmed by an offline AOB scan of this exact
// eldenring.exe 2.7.2.0 build using veeenu's practice-tool patterns.
//
// IMPORTANT: the inventory-grant function is FuncItemInject (0x5618b0), the one
// veeenu's ItemSpawner actually calls (config.rs: `chains.func_item_inject`). The
// earlier value 0x55bea0 was FuncItemSpawn, a *different* function that drops the
// item on the ground instead of adding it to the inventory -- that was the
// "földre dobja, nem felvehető" bug. Same call shape/struct, wrong target.
// Version-locked to 2.7.2.0; re-AOB-scan on a game patch.
const FUNC_ITEM_INJECT_RVA: usize = 0x5618b0;
const MAP_ITEM_MAN_SLOT_RVA: usize = 0x3d6eae0;

#[repr(C)]
struct SpawnRequest {
    one: u32,
    item_id: u32,
    qty: u32,
    dur: i32,
    gem: i32,
}

type SpawnItemFn =
    extern "system" fn(*const core::ffi::c_void, *mut SpawnRequest, *mut u32, u32);

/// Give `qty` of the item `item_id` (full id = category prefix | param row) to the
/// player's inventory, by calling the game's own item-spawn function. WRITE path.
pub fn give_item(item_id: u32, qty: u32) -> bool {
    std::panic::catch_unwind(|| unsafe {
        let base = module_base();
        if base == 0 {
            return None;
        }
        let slot = (base + MAP_ITEM_MAN_SLOT_RVA) as *const *const core::ffi::c_void;
        let map_item_man = *slot;
        if map_item_man.is_null() {
            return None; // not in game yet
        }
        let spawn_fn: SpawnItemFn = core::mem::transmute(base + FUNC_ITEM_INJECT_RVA);
        let mut req = SpawnRequest { one: 1, item_id, qty, dur: -1, gem: -1 };
        let mut out = 0u32;
        spawn_fn(map_item_man, &mut req as *mut _, &mut out as *mut _, 0);
        Some(())
    })
    .ok()
    .flatten()
    .is_some()
}

/// Add runes to the player's held count (test helper / future rune card).
pub fn add_runes(amount: u32) -> bool {
    std::panic::catch_unwind(|| unsafe {
        let Some(p) = player_ptr() else { return false };
        let pgd = &mut *p;
        pgd.rune_count = pgd.rune_count.saturating_add(amount);
        true
    })
    .unwrap_or(false)
}

/// Add `points` to one of the eight attributes (0=Vigor .. 7=Arcane) as a stat-card
/// reward. Returns true on success.
pub fn add_attribute(index: usize, points: u32) -> bool {
    std::panic::catch_unwind(|| unsafe {
        let Some(p) = player_ptr() else { return false };
        let pgd = &mut *p;
        let slot = match index {
            0 => &mut pgd.vigor,
            1 => &mut pgd.mind,
            2 => &mut pgd.endurance,
            3 => &mut pgd.strength,
            4 => &mut pgd.dexterity,
            5 => &mut pgd.intelligence,
            6 => &mut pgd.faith,
            7 => &mut pgd.arcane,
            _ => return false,
        };
        *slot = (*slot).saturating_add(points);
        true
    })
    .unwrap_or(false)
}

/// Drain `points` from one of the eight attributes (0=Vigor..7=Arcane) as a CURSE.
/// Floors at 1 (ER attributes never go below 1). Returns true on success. Because this
/// writes PlayerGameData, the caller must re-baseline `authorized` afterwards (the grant
/// loop already does), or the Grace-suppression will undo it.
pub fn drain_attribute(index: usize, points: u32) -> bool {
    std::panic::catch_unwind(|| unsafe {
        let Some(p) = player_ptr() else { return false };
        let pgd = &mut *p;
        let slot = match index {
            0 => &mut pgd.vigor,
            1 => &mut pgd.mind,
            2 => &mut pgd.endurance,
            3 => &mut pgd.strength,
            4 => &mut pgd.dexterity,
            5 => &mut pgd.intelligence,
            6 => &mut pgd.faith,
            7 => &mut pgd.arcane,
            _ => return false,
        };
        *slot = slot.saturating_sub(points).max(1);
        true
    })
    .unwrap_or(false)
}

/// Forfeit `pct` (0..=100) of the player's CURRENTLY held runes as a curse. Returns
/// true on success. Re-baseline `authorized` afterwards (as the grant loop does).
pub fn forfeit_runes(pct: u32) -> bool {
    std::panic::catch_unwind(|| unsafe {
        let Some(p) = player_ptr() else { return false };
        let pgd = &mut *p;
        let loss = (pgd.rune_count as u64 * pct.min(100) as u64 / 100) as u32;
        pgd.rune_count = pgd.rune_count.saturating_sub(loss);
        true
    })
    .unwrap_or(false)
}

// ---- Curse SpEffects: inflict a real status on the player (poison, bleed) ----------
// Curse cards call `apply_speffect` to drive the game's OWN status system, so the status
// procs exactly like a hazard/weapon applied it (buildup gauge, DoT, VFX, and the Grace
// "Remove <status>" cleanse all behave vanilla). The local PlayerIns begins with a
// `ChrIns` at offset 0, reachable at WorldChrMan + WCM_MAIN_PLAYER_OFF (the same pointer
// `is_in_world` reads). The native function is:
//     __fastcall apply_speffect(ChrIns* this, i32 sp_effect_id, bool dont_sync) -> u64
// RVA for 2.7.2.0 (ww), VERIFIED offline against THIS exe: the bytes at 0x3e8dc0 are a
// textbook prologue (48 8b c4 48 89 58 08 48 89 70 10 57 48 81 ec c0 ...), matching the
// same ww table that the already-proven item-inject/lua-warp RVAs come from.
// Version-locked to 2.7.2.0; re-verify on a game patch.
const CHR_INS_APPLY_SPEFFECT_RVA: usize = 0x3e8dc0;

/// ER SpEffectParam status ids (names from SpEffectParam, veeenu param dump):
///   500 = "Hemorrhage (Cycled)"        -> bleed / blood loss
///   505 = "Poison/Scarlet Rot (Cycled)" -> poison / rot
/// If these read wrong in-game, the typed damage ids are drop-in alternates:
///   6400 "Hemorrhage - Type 1" , 6500 "Poison - Type 1".
pub const SPEFFECT_BLEED: i32 = 500;
pub const SPEFFECT_POISON: i32 = 505;

/// Apply an ER SpEffect to the LOCAL player by its SpEffectParam id. Returns false
/// (no-op) if not in-world or the pointer chain is not resolvable, so a menu state or a
/// bad id can never crash. WRITE path -- calls a native game function. Must be in-world.
pub fn apply_speffect(sp_effect_id: i32) -> bool {
    if !is_in_world() {
        return false;
    }
    std::panic::catch_unwind(|| unsafe {
        let base = module_base();
        if base == 0 {
            return false;
        }
        let wcm = *((base + WORLD_CHR_MAN_SLOT_RVA) as *const *mut u8);
        if !plausible_ptr(wcm as *const u8) {
            return false;
        }
        // PlayerIns pointer; its first field IS the ChrIns the native fn expects.
        let main_player =
            *((wcm as usize + WCM_MAIN_PLAYER_OFF) as *const *mut core::ffi::c_void);
        if !plausible_ptr(main_player as *const u8) {
            return false;
        }
        type ApplyFn = unsafe extern "system" fn(*mut core::ffi::c_void, i32, bool) -> u64;
        let f: ApplyFn = core::mem::transmute(base + CHR_INS_APPLY_SPEFFECT_RVA);
        f(main_player, sp_effect_id, false);
        true
    })
    .unwrap_or(false)
}

// ---- Grace warp (intro-skip): start a new character at the Stranded Graveyard ----
// Replicates Erd-Tools' Warp(bonfireID): it drives the game's OWN Lua event system, so
// the destination grace's normal arrival (wake-up cutscene, flasks, map state) plays as
// vanilla. We do it IN-PROCESS (no ASM injection): resolve the CSLuaEventManager slot,
// then call LuaWarp with the exact register setup from Warp.asm:
//   rax = *slot;  rcx = [rax+0x18];  rdx = [rax+0x8];  r8d = bonfireID;  call LuaWarp+2
// RVAs offline-AOB-scanned on THIS 2.7.2.0 exe (each pattern matched exactly once):
//   CSLuaEventManager slot: AoB "48 83 3D ? ? ? ? 00 48 8B F9 0F 84 ? ? ? ? 48"
//   LuaWarp:                AoB "C3 ? ? ? ? ? ? 57 48 83 EC ? 48 8B FA 44" (+2)
// Version-locked to 2.7.2.0; re-scan on a game patch.
const CS_LUA_EVENT_MANAGER_SLOT_RVA: usize = 0x3d6eed8;
const LUA_WARP_RVA: usize = 0x59af10; // already the AoB match + 2

/// Warp the player to the Site of Grace whose `bonfireEntityId` (a BonfireWarpParam
/// field value, NOT the row id) is `bonfire_id`. Returns false (no-op) if the manager
/// is not resolvable, so a bad/zero id can never crash here. Must be in-world.
pub fn warp_to_grace(bonfire_id: u32) -> bool {
    if bonfire_id == 0 {
        return false; // id not configured yet -> do nothing
    }
    std::panic::catch_unwind(|| unsafe {
        let base = module_base();
        if base == 0 {
            return false;
        }
        let slot = (base + CS_LUA_EVENT_MANAGER_SLOT_RVA) as *const *mut u8;
        if !range_ok(slot as *const u8, 8, false) {
            return false;
        }
        let mgr = *slot;
        if !range_ok(mgr as *const u8, 0x20, false) {
            return false;
        }
        // Warp.asm register setup.
        let rcx = *((mgr as usize + 0x18) as *const *mut core::ffi::c_void);
        let rdx = *((mgr as usize + 0x08) as *const *mut core::ffi::c_void);
        type LuaWarpFn = unsafe extern "system" fn(
            *mut core::ffi::c_void,
            *mut core::ffi::c_void,
            u32,
        );
        let f: LuaWarpFn = std::mem::transmute(base + LUA_WARP_RVA);
        f(rcx, rdx, bonfire_id);
        true
    })
    .unwrap_or(false)
}

// ---- Draft input freeze (FD4PadManager.allow_polling) --------------------------
//
// Vendored path (option B): the published eldenring 0.14.0 crate has no pad module,
// so we reach the pads ourselves. We do NOT use from-singleton at runtime -- its
// pelite scan of the in-process PE panics on this setup (same family of failure as
// GameDataMan's version detect), and calling it per frame is what tanked the FPS to
// ~9. Instead we offline-scan eldenring.exe for the FD4-singleton getter idiom
// (scripts/find-singleton-rva.py -> pad_singletons_gen.rs: candidate slot RVAs) and,
// at runtime, pick the slot whose FD4PadManager.loaded_pad_entries (flat array at
// offset 0xe0, bounded to 9) hold objects with a known CSPad vftable. We set
// allow_polling (offset 0x20 inside each CSPad). The resolved slot is cached, so the
// probe runs at most once.
//
// FAIL-SAFE: every pointer is VirtualQuery-validated before any read, and a write
// happens ONLY when the candidate's vftable equals one of the four known 2.7.2.0
// CSPad vftables. So a wrong offset or a stale/garbage slot can at most no-op -- it
// can never cause an access violation (which catch_unwind would NOT catch) and can
// never stray-write. If nothing resolves, lib.rs keeps the message_filter fallback.
// Version-locked to 2.7.2.0 (vftable RVAs); re-check on a game patch.

/// Known CSPad-family vftable RVAs for 2.7.2.0 (from eldenring-rs rva_ww): base
/// CSPad, CSInGamePad, CSMenuViewerPad, CSDebugCamPad. A heap object is treated as a
/// real CSPad only if its vftable == base + one of these.
const CSPAD_VMT_RVAS: [usize; 4] = [0x2be5d20, 0x2be5898, 0x2be6a28, 0x2be51a0];
const LOADED_PAD_ENTRIES_OFF: usize = 0xe0; // FD4PadManager.loaded_pad_entries
const LOADED_PAD_ENTRIES_CNT: usize = 9;
const CSPAD_ALLOW_POLLING_OFF: usize = 0x20; // FD4BasePad.allow_polling inside CSPad

// Win32 memory-protection bits we care about (raw, to avoid windows newtype churn).
const MEM_COMMIT_BIT: u32 = 0x1000;
const PAGE_GUARD_BIT: u32 = 0x100;
const PAGE_NOACCESS_BIT: u32 = 0x01;
const READABLE_MASK: u32 = 0x02 | 0x04 | 0x08 | 0x20 | 0x40 | 0x80; // RO,RW,WC,XR,XRW,XWC
const WRITABLE_MASK: u32 = 0x04 | 0x08 | 0x40 | 0x80; // RW,WC,XRW,XWC

/// True if `[ptr, ptr+len)` is entirely committed and readable (and writable when
/// asked). Uses VirtualQuery so we never touch guard/no-access/reserved memory.
unsafe fn range_ok(ptr: *const u8, len: usize, writable: bool) -> bool {
    if ptr.is_null() {
        return false;
    }
    let mut mbi = MEMORY_BASIC_INFORMATION::default();
    let n = VirtualQuery(
        Some(ptr as *const core::ffi::c_void),
        &mut mbi,
        core::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
    );
    if n == 0 {
        return false;
    }
    if mbi.State.0 & MEM_COMMIT_BIT == 0 {
        return false;
    }
    let p = mbi.Protect.0;
    if p & (PAGE_GUARD_BIT | PAGE_NOACCESS_BIT) != 0 {
        return false;
    }
    if p & READABLE_MASK == 0 {
        return false;
    }
    if writable && p & WRITABLE_MASK == 0 {
        return false;
    }
    let Some(need_end) = (ptr as usize).checked_add(len) else {
        return false;
    };
    let region_end = (mbi.BaseAddress as usize).saturating_add(mbi.RegionSize);
    need_end <= region_end
}

/// Caches the resolved FD4PadManager static-slot RVA once found: 0 = not tried yet,
/// u32::MAX = tried and failed (give up, so there is no per-frame cost and no panic
/// storm), anything else = the working slot RVA.
static RESOLVED_PAD_SLOT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Set allow_polling on every CSPad reachable from this candidate manager pointer.
/// A write happens ONLY when the object's vftable is one of the four known CSPad
/// vftables, so a non-pad singleton (or a garbage slot) touches nothing. Returns how
/// many real CSPads were toggled. Caller guarantees `man` is readable up to the
/// entries span.
unsafe fn apply_pads(man: *mut u8, known: &[usize; 4], enabled: bool) -> u32 {
    let entries = man.add(LOADED_PAD_ENTRIES_OFF) as *const *mut u8;
    let mut n = 0u32;
    for i in 0..LOADED_PAD_ENTRIES_CNT {
        let pad_entry = *entries.add(i); // PadEntry* (may be uninit garbage)
        if !range_ok(pad_entry as *const u8, 8, false) {
            continue;
        }
        let cspad = *(pad_entry as *const *mut u8); // PadEntry.entry (CSPad*) @ +0
        if !range_ok(cspad as *const u8, CSPAD_ALLOW_POLLING_OFF + 1, true) {
            continue;
        }
        let vft = *(cspad as *const usize); // vftable pointer
        if !known.contains(&vft) {
            continue; // not a real CSPad -> skip (fail-safe against bad offsets)
        }
        *cspad.add(CSPAD_ALLOW_POLLING_OFF) = u8::from(enabled);
        n += 1;
    }
    n
}

/// Base + the four known CSPad vftable VAs. Small helper so both resolve and toggle
/// use the same identity set.
#[inline]
unsafe fn cspad_vmts(base: usize) -> [usize; 4] {
    [
        base + CSPAD_VMT_RVAS[0],
        base + CSPAD_VMT_RVAS[1],
        base + CSPAD_VMT_RVAS[2],
        base + CSPAD_VMT_RVAS[3],
    ]
}

/// READ-ONLY: does this candidate manager's loaded_pad_entries hold any object with a
/// known CSPad vftable? Used to identify FD4PadManager without writing anything.
unsafe fn has_cspads(man: *mut u8, known: &[usize; 4]) -> bool {
    let entries = man.add(LOADED_PAD_ENTRIES_OFF) as *const *mut u8;
    for i in 0..LOADED_PAD_ENTRIES_CNT {
        let pad_entry = *entries.add(i);
        if !range_ok(pad_entry as *const u8, 8, false) {
            continue;
        }
        let cspad = *(pad_entry as *const *mut u8);
        if !range_ok(cspad as *const u8, CSPAD_ALLOW_POLLING_OFF + 1, false) {
            continue;
        }
        if known.contains(&*(cspad as *const usize)) {
            return true;
        }
    }
    false
}

/// Resolve the FD4PadManager static slot ONCE for the whole lifetime, by a read-only
/// probe of the offline-baked candidate slots (pad_singletons_gen.rs). Picks the slot
/// whose loaded_pad_entries contain CSPad-vftable objects. Caches the result -- both
/// success (the slot RVA) AND failure (u32::MAX) -- so it NEVER runs again, and never
/// on the menu-open path. Call this from a normal in-world frame. Does NOT write
/// allow_polling (so it does not freeze input during resolution).
///
/// We deliberately do NOT use from-singleton's reflection: its pelite scan of the
/// in-process PE panics on this setup, and calling it per frame is what tanked the FPS.
pub fn resolve_pad_slot() {
    use std::sync::atomic::Ordering;
    if RESOLVED_PAD_SLOT.load(Ordering::Relaxed) != 0 {
        return; // already resolved or already gave up -> trivial atomic load, no work
    }
    let _ = std::panic::catch_unwind(|| unsafe {
        let base = module_base();
        if base == 0 {
            return;
        }
        let known = cspad_vmts(base);
        let span = LOADED_PAD_ENTRIES_OFF + 8 * LOADED_PAD_ENTRIES_CNT;
        for &rva in crate::pad_singletons_gen::FD4_SINGLETON_SLOTS.iter() {
            // Candidate slots are module-static (always mapped) -> read directly.
            let man = *((base + rva as usize) as *const *mut u8);
            // Cheap reject first; only VirtualQuery-validate plausible candidates, so
            // this one-time probe does few VQ (VirtualQuery is costly in this process).
            if !plausible_ptr(man as *const u8) {
                continue;
            }
            if !range_ok(man as *const u8, span, false) {
                continue;
            }
            if has_cspads(man, &known) {
                RESOLVED_PAD_SLOT.store(rva, Ordering::Relaxed);
                return;
            }
        }
        RESOLVED_PAD_SLOT.store(u32::MAX, Ordering::Relaxed); // give up -> fallback
    });
}

/// Toggle gameplay input polling on every CSPad (Szefi's draft freeze): with
/// `enabled == false` the game reads no movement/camera/menu/Esc input. Call this ONLY
/// on the draft open/close TRANSITION, never per frame. Uses the slot cached by
/// [`resolve_pad_slot`]; if resolution never succeeded it is a cheap no-op (one atomic
/// load) and the caller keeps the message_filter fallback. Every deref is
/// VirtualQuery-validated and every write is vftable-gated. Version-locked to 2.7.2.0.
pub fn set_pad_polling(enabled: bool) -> bool {
    use std::sync::atomic::Ordering;
    let slot = RESOLVED_PAD_SLOT.load(Ordering::Relaxed);
    if slot == 0 || slot == u32::MAX {
        return false; // not resolved -> no-op (no VirtualQuery, no probe)
    }
    std::panic::catch_unwind(|| unsafe {
        let base = module_base();
        if base == 0 {
            return false;
        }
        let known = cspad_vmts(base);
        let span = LOADED_PAD_ENTRIES_OFF + 8 * LOADED_PAD_ENTRIES_CNT;
        let slot_ptr = (base + slot as usize) as *const *mut u8;
        if !range_ok(slot_ptr as *const u8, 8, false) {
            return false;
        }
        let man = *slot_ptr;
        if !range_ok(man as *const u8, span, false) {
            return false;
        }
        apply_pads(man, &known, enabled) > 0
    })
    .unwrap_or(false)
}

/// Full player snapshot for the grace-level-up suppression (level + runes + the eight
/// attributes), so a vanilla Grace level-up can be detected and reverted.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FullState {
    pub soul_level: u32,
    pub runes_held: u32,
    pub stats: [u32; 8], // Vigor..Arcane
}

/// Read the full player snapshot, or None at menu/loading or when not in-world.
/// Never panics/null-derefs. Gated on is_in_world so the grace-suppression never
/// writes based on stale menu values.
pub fn read_full_state() -> Option<FullState> {
    if !is_in_world() {
        return None;
    }
    std::panic::catch_unwind(|| unsafe {
        let p = player_ptr()?;
        let pgd = &*p;
        if pgd.level == 0 || pgd.level > MAX_SOUL_LEVEL {
            return None; // stale/garbage sanity backstop
        }
        Some(FullState {
            soul_level: pgd.level,
            runes_held: pgd.rune_count,
            stats: [
                pgd.vigor,
                pgd.mind,
                pgd.endurance,
                pgd.strength,
                pgd.dexterity,
                pgd.intelligence,
                pgd.faith,
                pgd.arcane,
            ],
        })
    })
    .ok()
    .flatten()
}

/// Restore a previously authorized snapshot (undo a vanilla Grace level-up): write
/// level, runes and all eight attributes back. WRITE path, same fields as the stat
/// cards. Returns true on success. Never panics/null-derefs.
pub fn restore_full_state(s: &FullState) -> bool {
    std::panic::catch_unwind(|| unsafe {
        let Some(p) = player_ptr() else { return false };
        let pgd = &mut *p;
        pgd.level = s.soul_level;
        pgd.rune_count = s.runes_held;
        pgd.vigor = s.stats[0];
        pgd.mind = s.stats[1];
        pgd.endurance = s.stats[2];
        pgd.strength = s.stats[3];
        pgd.dexterity = s.stats[4];
        pgd.intelligence = s.stats[5];
        pgd.faith = s.stats[6];
        pgd.arcane = s.stats[7];
        true
    })
    .unwrap_or(false)
}

// ---- Curse side effects (Aurora, 2026-10-24) --------------------------------------
// Two write paths the curse cards use. Both are no-ops unless a real character is
// loaded, both sanity-check what they read before writing, and both are wrapped in
// catch_unwind -- same philosophy as the rest of this file: on any doubt, do nothing.

/// Curse: halve the player's CURRENT hp (floor 1). Writes ONLY `hp`; `max_hp` is left
/// alone, otherwise the HP bar itself would shrink. Returns false (no-op) at the menu,
/// on a null/garbage chain, or when the values do not look like a real character.
///
/// Chain: WorldChrMan -> main_player (ChrIns) -> modules -> data (CSChrDataModule).
/// The field layout comes from the `eldenring` crate (cs/chr_ins/module/data.rs), the
/// same crate this file already trusts for GameDataMan/PlayerGameData.
pub fn halve_hp() -> bool {
    std::panic::catch_unwind(|| unsafe {
        if !is_in_world() {
            return false;
        }
        let base = module_base();
        if base == 0 {
            return false;
        }
        let wcm = *((base + WORLD_CHR_MAN_SLOT_RVA) as *const *mut u8);
        if !plausible_ptr(wcm as *const u8) {
            return false;
        }
        let mp = *((wcm as usize + WCM_MAIN_PLAYER_OFF) as *const *mut ChrIns);
        if !plausible_ptr(mp as *const u8) {
            return false;
        }
        let modules = (*mp).modules.as_ptr();
        if !plausible_ptr(modules as *const u8) {
            return false;
        }
        let data = (*modules).data.as_ptr();
        if !plausible_ptr(data as *const u8) {
            return false;
        }
        let d = &mut *data;
        let (hp, max_hp) = (d.hp, d.max_hp);
        // Sanity backstop against a stale/garbage module: a real character has a
        // positive max HP well under 60k, and current hp never exceeds it.
        if max_hp <= 0 || max_hp > 60_000 || hp <= 0 || hp > max_hp {
            return false;
        }
        d.hp = (hp / 2).max(1);
        true
    })
    .unwrap_or(false)
}

/// Curse: take the player's flasks away by zeroing the MAX flask counts (the flask
/// slots disappear). Returns the ORIGINAL (hp_flask, fp_flask) pair so the caller can
/// put them back with `restore_flasks`. Returns (0, 0) if nothing was changed.
///
/// Deliberately the max counts, not the current charges: "the curse took your flasks"
/// is a run-level effect, and the current charge lives on the inventory side.
pub fn empty_flasks() -> (u8, u8) {
    std::panic::catch_unwind(|| unsafe {
        let Some(p) = player_ptr() else { return (0, 0) };
        let pgd = &mut *p;
        let elozo = (pgd.max_hp_flask, pgd.max_fp_flask);
        // Vanilla caps are 14 HP / 10 FP flasks; anything bigger means we are reading
        // a stale or wrong struct, so do not write.
        if elozo.0 > 20 || elozo.1 > 20 {
            return (0, 0);
        }
        pgd.max_hp_flask = 0;
        pgd.max_fp_flask = 0;
        elozo
    })
    .unwrap_or((0, 0))
}

/// Put back the flask counts returned by `empty_flasks`. No-op on (0, 0) or at the menu.
pub fn restore_flasks(hp_flask: u8, fp_flask: u8) -> bool {
    if hp_flask == 0 && fp_flask == 0 {
        return false;
    }
    std::panic::catch_unwind(|| unsafe {
        let Some(p) = player_ptr() else { return false };
        let pgd = &mut *p;
        pgd.max_hp_flask = hp_flask.min(20);
        pgd.max_fp_flask = fp_flask.min(20);
        true
    })
    .unwrap_or(false)
}

/// Curse (temporary): empty the player's CURRENT flask charges. The max counts are NOT
/// touched, so the next rest at a Site of Grace refills them normally -- this is the
/// run-level punishment Szefi asked for, and it never writes a permanent loss into the
/// save the way `empty_flasks` does.
///
/// How a flask charge is stored: the charge IS the inventory quantity of the flask goods
/// item. Crimson Tears is goods 1000..=1025 and Cerulean Tears is 1050..=1075 (one id per
/// upgrade level, from EquipParamGoods), so we match the whole range instead of a single
/// id. The Wondrous Physick (goods 250/251) is deliberately NOT touched.
///
/// Returns true only if at least one charge was actually zeroed.
pub fn empty_current_flasks() -> bool {
    std::panic::catch_unwind(|| unsafe {
        if !is_in_world() {
            return false;
        }
        let Some(p) = player_ptr() else { return false };
        let pgd = &mut *p;
        let inv = &pgd.equipment.equip_inventory_data.items_data;
        let mut valtozott = false;
        for e in inv.items_mut() {
            if e.item_id.category() != ItemCategory::Goods {
                continue;
            }
            let id = e.item_id.param_id();
            let flaska = (1000..=1025).contains(&id) || (1050..=1075).contains(&id);
            if flaska && e.quantity > 0 {
                e.quantity = 0;
                valtozott = true;
            }
        }
        valtozott
    })
    .unwrap_or(false)
}

/// DIAGNOSTIC for the frost/sleep hunt: dump the head of the player's RESIST module as
/// hex dwords. The crate does NOT map this module (it is a bare `usize` in
/// `ChrInsModuleContainer`), so the per-status gauge offsets have to be identified by
/// watching which dword moves when a KNOWN status is applied.
///
/// Field order in the container is fixed (`#[repr(C)]`, 8 bytes each):
///   0x00 data, 0x08 action_flag, 0x10 behavior_script, 0x18 time_act, 0x20 RESIST.
///
/// Procedure that identifies the array without guessing: press the already-working
/// poison button once and watch the output -- the dword that starts climbing is the
/// poison gauge. Repeat with bleed (1753) to get a second known column; the stride and
/// the order of the remaining statuses follow from those two.
pub fn dump_resist(dwords: usize) -> String {
    std::panic::catch_unwind(|| unsafe {
        if !is_in_world() {
            return "nincs karakter a vilagban".to_string();
        }
        let base = module_base();
        if base == 0 {
            return "nincs modul-bazis".to_string();
        }
        let wcm = *((base + WORLD_CHR_MAN_SLOT_RVA) as *const *mut u8);
        if !plausible_ptr(wcm as *const u8) {
            return "WorldChrMan null".to_string();
        }
        let mp = *((wcm as usize + WCM_MAIN_PLAYER_OFF) as *const *mut ChrIns);
        if !plausible_ptr(mp as *const u8) {
            return "main_player null".to_string();
        }
        let modules = (*mp).modules.as_ptr() as usize;
        if !plausible_ptr(modules as *const u8) {
            return "modules null".to_string();
        }
        let resist = *((modules + 0x20) as *const *const u8);
        if !plausible_ptr(resist) {
            return "resist modul null".to_string();
        }
        let n = dwords.min(64);
        let mut ki = String::with_capacity(n * 12);
        for i in 0..n {
            let v = *((resist as usize + i * 4) as *const i32);
            if i % 8 == 0 {
                ki.push_str(&format!("\n+{:02X}: ", i * 4));
            }
            ki.push_str(&format!("{:>8} ", v));
        }
        ki
    })
    .unwrap_or_else(|_| "PANIC a resist-olvasasban".to_string())
}

/// One-line status for the overlay while we bring the reader up.
pub fn diagnostic() -> String {
    let r = std::panic::catch_unwind(|| unsafe {
        match game_data_man() {
            None => "GameDataMan slot null (menu?)".to_string(),
            Some(gdm) => {
                let p = gdm.main_player_game_data.as_ptr();
                if p.is_null() {
                    "GameDataMan OK, player ptr NULL".to_string()
                } else {
                    let pgd = &*p;
                    format!("OK  lvl={} runes={}", pgd.level, pgd.rune_count)
                }
            }
        }
    });
    r.unwrap_or_else(|_| "PANIC in read".to_string())
}
