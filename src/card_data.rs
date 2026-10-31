//! The card pool: real Elden Ring items (names, descriptions, ids) that the draft
//! draws from. Data comes from Nagato's `card-pools.json` (the game's own English
//! text); we embed it and parse it once at startup.

use crate::cards::{CardKind, CurseEffect};
use serde_json::Value;

const POOLS_JSON: &[u8] = include_bytes!("../refs/card-pools.json");
/// Offline-baked item-row -> iconId map (scripts/ikon-map-epit.py), keyed by pool
/// kind then row string. Lets the medallion show the item's real game icon without
/// any runtime param navigation.
const ICON_MAP_JSON: &[u8] = include_bytes!("../refs/item-ikonok/ikon-map.json");

/// Item category prefix (bits 31..28) for the in-game item id, per the crate's
/// `ItemCategory` enum: Weapon 0, Protector 1, Accessory 2, Goods 4, Gem 8.
fn category_prefix(kind: CardKind) -> u32 {
    match kind {
        CardKind::Weapon | CardKind::Shield => 0x0000_0000,
        CardKind::Armor => 0x1000_0000,
        CardKind::Talisman => 0x2000_0000,
        CardKind::Spell => 0x4000_0000, // sorceries/incantations are Goods
        CardKind::AshOfWar => 0x8000_0000, // Gem
        _ => 0,
    }
}

fn kind_from_pool_key(key: &str) -> Option<CardKind> {
    match key {
        "weapon" => Some(CardKind::Weapon),
        "shield" => Some(CardKind::Shield),
        "armor" => Some(CardKind::Armor),
        "talisman" => Some(CardKind::Talisman),
        "ashofwar" => Some(CardKind::AshOfWar),
        "spell" => Some(CardKind::Spell),
        _ => None,
    }
}

/// One concrete card: a game item, or a stat boost (item_id 0, stat set).
#[derive(Clone)]
pub struct CardInstance {
    pub kind: CardKind,
    /// Full in-game item id (category prefix | param row); 0 for a stat card.
    pub item_id: u32,
    pub name: String,
    pub desc: String,
    pub rarity: u8,
    /// Stat card: which attribute (0=Vigor..7=Arcane) and how many points.
    pub stat: Option<(usize, u32)>,
    /// Build-package boosts: several (attribute, points) a curse card grants together
    /// (e.g. +3 Str / End / Vig). Empty for normal cards.
    pub pack: Vec<(usize, u32)>,
    /// The item's in-game icon id (MENU_ItemIcon_<id>.png), if known. None for a
    /// stat card or an item with no icon -> the medallion stays empty.
    pub icon_id: Option<u32>,
    /// Set on a cursed card: the hidden price(s) paid once the card is taken (revealed
    /// only after Confirm). Empty for every normal card. 1 cost usually, sometimes 2-3.
    pub costs: Vec<CurseEffect>,
}

pub struct Pools {
    items: Vec<CardInstance>,
}

/// Parse the embedded pool once. Never panics: a bad file yields an empty pool.
pub fn load() -> Pools {
    let mut items = Vec::new();
    // Offline icon map (kind -> row string -> iconId). Missing file -> no icons.
    let icon_map = serde_json::from_slice::<Value>(ICON_MAP_JSON).ok();
    if let Ok(v) = serde_json::from_slice::<Value>(POOLS_JSON) {
        if let Some(pools) = v.get("poolok").and_then(Value::as_object) {
            for (key, arr) in pools {
                let Some(kind) = kind_from_pool_key(key) else { continue };
                let Some(list) = arr.as_array() else { continue };
                for it in list {
                    let row = it.get("item_id").and_then(Value::as_u64).unwrap_or(0) as u32;
                    if row == 0 {
                        continue;
                    }
                    let icon_id = icon_map
                        .as_ref()
                        .and_then(|m| m.get("poolok"))
                        .and_then(|p| p.get(key))
                        .and_then(|rows| rows.get(row.to_string()))
                        .and_then(Value::as_u64)
                        .map(|n| n as u32);
                    items.push(CardInstance {
                        kind,
                        item_id: category_prefix(kind) | row,
                        name: it.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                        desc: it.get("desc").and_then(Value::as_str).unwrap_or("").to_string(),
                        rarity: it.get("rarity").and_then(Value::as_u64).unwrap_or(2) as u8,
                        stat: None,
                        pack: Vec::new(),
                        icon_id,
                        costs: Vec::new(),
                    });
                }
            }
        }
    }
    Pools { items }
}

const STAT_NAMES: [&str; 8] = [
    "Vigor", "Mind", "Endurance", "Strength", "Dexterity", "Intelligence", "Faith", "Arcane",
];

fn stat_card(attr: usize, points: u32) -> CardInstance {
    CardInstance {
        kind: CardKind::Stat,
        item_id: 0,
        name: format!("+{points} {}", STAT_NAMES[attr]),
        desc: format!("Raise {} by {points}.", STAT_NAMES[attr]),
        rarity: 2,
        stat: Some((attr, points)),
        pack: Vec::new(),
        icon_id: None,
        costs: Vec::new(),
    }
}

/// Roll a random stat card: a random attribute, +1..+3 points (Szefi, 2026-10-24: +4
/// was too generous).
fn random_stat_card(rng: &mut u64) -> CardInstance {
    let attr = (next(rng) % 8) as usize;
    let points = 1 + (next(rng) % 2) as u32; // +1..+2 (Szefi, 2026-10-24)
    stat_card(attr, points)
}

const COMBAT_STATS: [usize; 5] = [0, 3, 4, 5, 6]; // Vigor, Str, Dex, Int, Faith

/// Build-focused stat packages: a curse card grants +3 to three synergistic attributes
/// (Szefi, 2026-10-24). (name, [attr; 3]); attr index 0 Vigor..7 Arcane.
const BUILDS: [(&str, [usize; 3]); 6] = [
    ("Warrior", [3, 2, 0]),   // Strength, Endurance, Vigor
    ("Knight", [4, 2, 0]),    // Dexterity, Endurance, Vigor
    ("Sorcerer", [5, 2, 1]),  // Intelligence, Endurance, Mind
    ("Prophet", [6, 2, 1]),   // Faith, Endurance, Mind
    ("Occultist", [7, 2, 1]), // Arcane, Endurance, Mind
    ("Bruiser", [0, 2, 3]),   // Vigor, Endurance, Strength
];

/// Roll one hidden cost. Only CONFIRMED-working status procs are in the pool (Szefi
/// live-tested 2026-10-24): Bleed/Poison/Madness/ScarletRot plus the structural costs.
/// Frostbite/Sleep are excluded until the custom 1753-clone SpEffect rows exist.
/// EmptyFlasks is excluded (persists to the save until next-rest restore is wired).
/// Death Blight is INSTANT DEATH, so it is rare: only a 1-in-12 top slot.
fn roll_cost(rng: &mut u64) -> CurseEffect {
    match next(rng) % 12 {
        0 => CurseEffect::Poison,
        1 => CurseEffect::Bleed,
        2 => CurseEffect::Madness,
        3 => CurseEffect::ScarletRot,
        4 => CurseEffect::Teleport,
        5 => CurseEffect::HalveHp,
        6 => CurseEffect::Frailty(3 + (next(rng) % 4) as u32), // -3..-6 Vigor
        7 => {
            let attr = COMBAT_STATS[(next(rng) % COMBAT_STATS.len() as u64) as usize];
            CurseEffect::Weakness(attr, 2 + (next(rng) % 2) as u32) // -2..-3
        }
        8 => CurseEffect::DeathBlight, // rare: instant death (1-in-12)
        _ => CurseEffect::RuneForfeit(100), // zero ALL held runes (Szefi, 2026-10-24)
    }
}

/// Roll a cursed BLESSING (Szefi, 2026-10-24): a VISIBLE boon = a real pool item (icon +
/// name) PLUS a build-stat package (+3 to three synergistic attributes); and a HIDDEN
/// COST = 1 price usually, or a 1-in-4 "everything" curse of 2-3 distinct prices, revealed
/// only after Confirm.
fn random_curse_card(rng: &mut u64, items: &[CardInstance], used: &std::collections::HashSet<u32>) -> CardInstance {
    // --- hidden cost(s) ---
    let n_costs = if next(rng) % 4 == 0 {
        2 + (next(rng) % 2) as usize // 2 or 3 (the brutal "everything" curse)
    } else {
        1
    };
    let mut costs: Vec<CurseEffect> = Vec::new();
    let mut guard = 0;
    while costs.len() < n_costs && guard < 32 {
        guard += 1;
        let c = roll_cost(rng);
        if !costs.iter().any(|e| e.label() == c.label()) {
            costs.push(c); // distinct effects stack in the "everything" curse
        }
    }
    if costs.is_empty() {
        costs.push(CurseEffect::HalveHp);
    }

    // --- visible boon: a build-stat package + a real pool item ---
    let build = BUILDS[(next(rng) % BUILDS.len() as u64) as usize];
    let pack: Vec<(usize, u32)> = build.1.iter().map(|&a| (a, 2)).collect();
    let available: Vec<_> = items.iter().filter(|item| !used.contains(&item.item_id)).collect();
    let (item_id, icon_id, name) = if available.is_empty() {
        (0, None, "Cursed Gift".to_string())
    } else {
        let it = available[(next(rng) % available.len() as u64) as usize];
        (it.item_id, it.icon_id, it.name.clone())
    };

    CardInstance {
        kind: CardKind::Curse,
        item_id,
        name,
        desc: "A gift wrapped in a curse. Its hidden price is paid only once you take it.".to_string(),
        rarity: 4,
        stat: None,
        pack,
        icon_id,
        costs,
    }
}

/// Odds that any FREE hand slot rolls a stat card instead of a pool item: 1-in-N.
/// This is only a tuning knob, NOT a quota -- any type combination is possible
/// (Szefi: "legyen teljesen random, lehessen 2 stat + 2 talizmán is, a miből
/// mennyit is random"). Raise/lower freely.
const STAT_CHANCE_DENOM: u64 = 4;

/// Odds that a hand contains a cursed card: each free slot has a 1-in-N chance to roll
/// a curse, capped at ONE curse per hand so the risk stays a rare, tense choice (Szefi,
/// 2026-10-24). Raise/lower freely.
const CURSE_CHANCE_DENOM: u64 = 5;

impl Pools {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Draw a hand of `count` cards. The ONLY mandatory rule (Szefi, 2026-10-23):
    /// exactly one guaranteed stat card, so there is always a safe pick. Every
    /// remaining slot is FREE random from any pool -- it can be another stat card,
    /// a talisman, a weapon, an ash, a spell, anything. Type repetition is allowed;
    /// the only thing kept distinct is the concrete item (no identical item twice).
    pub fn draw_hand(&self, count: usize, rng: &mut u64) -> Vec<CardInstance> {
        if count == 0 { return Vec::new(); }
        let mut hand = Vec::with_capacity(count);
        // Exactly one guaranteed stat card.
        hand.push(random_stat_card(rng));

        // The rest: free random. Each slot rolls stat-vs-item, then a concrete item
        // (distinct by item so the same reward never appears twice in one hand).
        let mut used = std::collections::HashSet::new();
        let mut curse_placed = false;
        let mut guard = 0u32;
        while hand.len() < count && guard < 10_000 {
            guard += 1;
            // At most one cursed card per hand: a rare, hidden risk among the rewards.
            if !curse_placed && next(rng) % CURSE_CHANCE_DENOM == 0 {
                curse_placed = true;
                let card = random_curse_card(rng, &self.items, &used);
                if card.item_id != 0 { used.insert(card.item_id); }
                hand.push(card);
                continue;
            }
            // A stat card is always available; items run out once all are used.
            let items_available = !self.items.is_empty() && used.len() < self.items.len();
            if !items_available || next(rng) % STAT_CHANCE_DENOM == 0 {
                hand.push(random_stat_card(rng));
            } else {
                let idx = (next(rng) % self.items.len() as u64) as usize;
                if used.insert(self.items[idx].item_id) {
                    hand.push(self.items[idx].clone());
                }
            }
        }
        hand
    }

    /// Every 5th level is a CURSE level: the whole hand is cursed cards (Szefi,
    /// 2026-10-24). Each is an independent random cursed blessing.
    pub fn draw_curse_hand(&self, count: usize, rng: &mut u64) -> Vec<CardInstance> {
        let mut used = std::collections::HashSet::new();
        (0..count)
            .map(|_| {
                let card = random_curse_card(rng, &self.items, &used);
                if card.item_id != 0 { used.insert(card.item_id); }
                card
            })
            .collect()
    }
}

/// Tiny xorshift so we do not pull in a rng crate; seeded from the frame counter.
fn next(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}
