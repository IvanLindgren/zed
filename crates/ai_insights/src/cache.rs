//! Symbol-description cache shared by all editors and persisted to
//! `<data_dir>/ai_insights/descriptions.json`, so reopening a file costs nothing.

use std::{path::PathBuf, sync::Arc, time::Duration};

use collections::HashMap;
use gpui::{App, AppContext as _, Global, Task};

const MAX_ENTRIES: usize = 20_000;
const SAVE_DELAY: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct DescriptionCache {
    entries: HashMap<u64, Arc<str>>,
    save_task: Option<Task<()>>,
}

impl Global for DescriptionCache {}

fn cache_path() -> PathBuf {
    paths::data_dir().join("ai_insights").join("descriptions.json")
}

/// Stable 64-bit FNV-1a, so keys survive restarts and rebuilds.
pub fn stable_hash(parts: &[&str]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for part in parts {
        for byte in part.bytes().chain(std::iter::once(0xff)) {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

pub fn init(cx: &mut App) {
    cx.set_global(DescriptionCache::default());
    let path = cache_path();
    cx.spawn(async move |cx| {
        let loaded = cx
            .background_spawn(async move {
                let bytes = std::fs::read(&path).ok()?;
                serde_json::from_slice::<std::collections::HashMap<String, String>>(&bytes).ok()
            })
            .await;
        let Some(loaded) = loaded else { return };
        cx.update(|cx| {
            let cache = cx.global_mut::<DescriptionCache>();
            for (key, value) in loaded {
                if let Ok(key) = u64::from_str_radix(&key, 16) {
                    cache.entries.entry(key).or_insert_with(|| value.into());
                }
            }
        });
    })
    .detach();
}

pub fn get(key: u64, cx: &App) -> Option<Arc<str>> {
    cx.try_global::<DescriptionCache>()?.entries.get(&key).cloned()
}

pub fn insert_many(items: impl IntoIterator<Item = (u64, Arc<str>)>, cx: &mut App) {
    let cache = cx.default_global::<DescriptionCache>();
    cache.entries.extend(items);
    if cache.entries.len() > MAX_ENTRIES {
        let excess = cache.entries.len() - MAX_ENTRIES / 2;
        let keys: Vec<u64> = cache.entries.keys().take(excess).copied().collect();
        for key in keys {
            cache.entries.remove(&key);
        }
    }
    schedule_save(cx);
}

fn schedule_save(cx: &mut App) {
    let task = cx.spawn(async move |cx| {
        cx.background_executor().timer(SAVE_DELAY).await;
        let snapshot: std::collections::HashMap<String, String> = cx.update(|cx| {
            cx.global::<DescriptionCache>()
                .entries
                .iter()
                .map(|(key, value)| (format!("{key:016x}"), value.to_string()))
                .collect()
        });
        cx.background_spawn(async move {
            let path = cache_path();
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).ok();
            }
            match serde_json::to_vec(&snapshot) {
                Ok(bytes) => {
                    if let Err(error) = std::fs::write(&path, bytes) {
                        log::warn!("ai_insights: failed to save cache: {error}");
                    }
                }
                Err(error) => log::warn!("ai_insights: failed to encode cache: {error}"),
            }
        })
        .await;
    });
    cx.global_mut::<DescriptionCache>().save_task = Some(task);
}
