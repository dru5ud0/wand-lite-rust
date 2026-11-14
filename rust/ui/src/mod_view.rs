//! Pure presentation helpers for the trainer modification list.

use crate::trainer::ModRow;

#[derive(Debug, Clone, PartialEq)]
pub enum ModListEntry {
    Group { title: String, count: usize },
    Mod { index: usize },
}

const GROUP_ORDER: &[&str] = &["player", "stats", "weapons", "enemies", "game", "physics", "vehicle", "world"];

fn group_key(name: &str) -> (u8, usize, String) {
    match GROUP_ORDER.iter().position(|known| *known == name) {
        Some(pos) => (0, pos, String::new()),
        None => (1, 0, name.to_lowercase()),
    }
}

pub fn groups(mods: &[ModRow]) -> Vec<String> {
    let mut distinct: Vec<String> = Vec::new();
    for row in mods {
        if !distinct.iter().any(|e| e == &row.category) {
            distinct.push(row.category.clone());
        }
    }
    distinct.sort_by(|a, b| group_key(a).cmp(&group_key(b)));
    distinct
}

pub fn active_count(mods: &[ModRow]) -> usize {
    mods.iter().filter(|r| r.enabled).count()
}

pub fn summary(mods: &[ModRow]) -> String {
    format!("{} of {} active", active_count(mods), mods.len())
}

pub fn status_pill(raw: &str) -> &'static str {
    if raw == "confirmed" {
        "on"
    } else if raw.starts_with("request ") {
        "pending"
    } else if raw.starts_with("rejected (") && raw.ends_with(')') {
        "failed"
    } else {
        ""
    }
}

pub fn build_list(mods: &[ModRow], filter: &str, group: Option<&str>) -> Vec<ModListEntry> {
    let filter_lower = filter.to_lowercase();
    let matches_filter = |row: &ModRow| -> bool {
        filter.is_empty()
            || row.name.to_lowercase().contains(&filter_lower)
            || row.category.to_lowercase().contains(&filter_lower)
            || row.target.to_lowercase().contains(&filter_lower)
            || row.hotkey.to_lowercase().contains(&filter_lower)
    };

    let mut passing: Vec<usize> = Vec::new();
    for (i, row) in mods.iter().enumerate() {
        if let Some(wanted) = group {
            if row.category != wanted {
                continue;
            }
        }
        if !matches_filter(row) {
            continue;
        }
        passing.push(i);
    }

    let mut passing_groups: Vec<String> = Vec::new();
    for &i in &passing {
        let c = mods[i].category.clone();
        if !passing_groups.iter().any(|e| e == &c) {
            passing_groups.push(c);
        }
    }
    passing_groups.sort_by(|a, b| group_key(a).cmp(&group_key(b)));

    let mut result: Vec<ModListEntry> = Vec::new();
    for category in passing_groups {
        let indices: Vec<usize> = passing.iter().copied().filter(|&i| mods[i].category == category).collect();
        result.push(ModListEntry::Group { title: category.clone(), count: indices.len() });
        for i in indices {
            result.push(ModListEntry::Mod { index: i });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, category: &str, target: &str, enabled: bool, hotkey: &str) -> ModRow {
        ModRow { name: name.into(), category: category.into(), target: target.into(), enabled,
            value: 0.0, min: 0.0, max: 1.0, step: 1.0, kind: "toggle".into(), status: String::new(), hotkey: hotkey.into() }
    }

    #[test]
    fn groups_documented_order() {
        let mods = vec![row("a", "world", "t1", false, ""), row("b", "player", "t2", false, ""),
            row("c", "zzz", "t3", false, ""), row("d", "stats", "t4", false, ""), row("e", "custom", "t5", false, "")];
        assert_eq!(groups(&mods), vec!["player", "stats", "world", "custom", "zzz"]);
    }

    #[test]
    fn build_list_no_filter() {
        let mods = vec![row("God Mode", "player", "god", true, "F1"), row("Ammo", "weapons", "ammo", false, "")];
        let list = build_list(&mods, "", None);
        assert_eq!(list, vec![ModListEntry::Group { title: "player".into(), count: 1 }, ModListEntry::Mod { index: 0 },
            ModListEntry::Group { title: "weapons".into(), count: 1 }, ModListEntry::Mod { index: 1 }]);
    }

    #[test]
    fn build_list_filter_matches() {
        let mods = vec![row("God Mode", "player", "god_mode", true, "F1"),
            row("Ammo", "weapons", "ammo", false, "F2"), row("Money", "stats", "money", false, "")];
        assert_eq!(build_list(&mods, "god", None), vec![ModListEntry::Group { title: "player".into(), count: 1 }, ModListEntry::Mod { index: 0 }]);
        assert_eq!(build_list(&mods, "ammo", None), vec![ModListEntry::Group { title: "weapons".into(), count: 1 }, ModListEntry::Mod { index: 1 }]);
        assert_eq!(build_list(&mods, "f2", None), vec![ModListEntry::Group { title: "weapons".into(), count: 1 }, ModListEntry::Mod { index: 1 }]);
    }

    #[test]
    fn build_list_group_filter() {
        let mods = vec![row("God Mode", "player", "god", true, ""), row("Ammo", "weapons", "ammo", false, "")];
        assert_eq!(build_list(&mods, "", Some("weapons")), vec![ModListEntry::Group { title: "weapons".into(), count: 1 }, ModListEntry::Mod { index: 1 }]);
        assert!(build_list(&mods, "", Some("nope")).is_empty());
    }

    #[test]
    fn active_count_and_summary() {
        let mods = vec![row("a", "player", "t1", true, ""), row("b", "player", "t2", true, ""),
            row("c", "player", "t3", true, ""), row("d", "player", "t4", false, ""), row("e", "player", "t5", false, "")];
        assert_eq!(active_count(&mods), 3);
        assert_eq!(summary(&mods), "3 of 5 active");
        let empty: Vec<ModRow> = Vec::new();
        assert_eq!(summary(&empty), "0 of 0 active");
    }

    #[test]
    fn status_pill_mapping() {
        assert_eq!(status_pill("confirmed"), "on");
        assert_eq!(status_pill("request 5"), "pending");
        assert_eq!(status_pill("rejected (2)"), "failed");
        assert_eq!(status_pill(""), "");
    }

    #[test]
    fn non_ascii_filter_no_panic() {
        let mods = vec![row("Café", "player", "t", false, "")];
        let _ = build_list(&mods, "CAFÉ", None);
        let _ = build_list(&mods, "é", None);
    }
}