#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use anyhow::{Result, bail};

use super::{App, SEARCH_PROVIDERS, VERBOSITY_LEVELS};

impl App {
    /// Validate, persist, and apply a setting.
    pub fn set_setting(&mut self, key: &str, value: &str) -> Result<()> {
        if !SETTING_KEYS.contains(&key) {
            bail!("unknown setting: {key}");
        }
        if !valid_setting_value(key, value) {
            bail!("invalid value for {key}: {value:?}");
        }
        self.apply_setting(key, value);
        if key == "blocked_domains" {
            // Keep this configuration with the space so it travels with its files.
            std::fs::write(
                self.space.blocked_domains_path(&self.active_space.name),
                value,
            )?;
        } else {
            self.db.set_setting(key, value)?;
        }
        self.refresh_toolbox();
        self.push_status(format!("{key} set"));
        Ok(())
    }
}

const SETTING_KEYS: [&str; 20] = [
    "remote_images",
    "show_stats",
    "show_reasoning",
    "hide_hints",
    "usage_range",
    "temperature",
    "top_p",
    "max_tokens",
    "compact_threshold",
    "searxng_url",
    "verbosity",
    "reply_style",
    "cache_lifetime",
    "cache_long_openai",
    "langsearch_key",
    "search_provider",
    "blocked_domains",
    "column_width",
    "popup_sizes",
    "image_viewer",
];

pub const COLUMN_WIDTH_MIN: u16 = 60;
pub const COLUMN_WIDTH_MAX: u16 = 400;

pub const POPUP_SIZE_MIN: u16 = 20;
pub const POPUP_SIZE_MAX: u16 = 100;

pub const POPUP_IDS: [&str; 19] = [
    "model",
    "session",
    "copy",
    "key",
    "settings",
    "space",
    "skills",
    "files",
    "apps",
    "research",
    "swarm",
    "usage",
    "jobs",
    "harness",
    "procs",
    "login",
    "local",
    "help",
    "connectors",
];

pub(super) fn valid_column_width(value: &str) -> bool {
    matches!(value, "auto" | "full")
        || (!value.is_empty()
            && value.bytes().all(|b| b.is_ascii_digit())
            && value
                .parse::<u16>()
                .is_ok_and(|n| (COLUMN_WIDTH_MIN..=COLUMN_WIDTH_MAX).contains(&n)))
}

fn parse_popup_entry(entry: &str) -> Option<(&'static str, u16, u16)> {
    let (id, dims) = entry.trim().split_once(':')?;
    let id = POPUP_IDS.iter().copied().find(|known| *known == id)?;
    let (w, h) = dims.split_once('x')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(w) || !digits(h) {
        return None;
    }
    let range = POPUP_SIZE_MIN..=POPUP_SIZE_MAX;
    let (w, h) = (w.parse::<u16>().ok()?, h.parse::<u16>().ok()?);
    (range.contains(&w) && range.contains(&h)).then_some((id, w, h))
}

fn popup_entries(sizes: &str) -> Vec<(&'static str, u16, u16)> {
    let mut entries: Vec<(&'static str, u16, u16)> = Vec::new();
    for (id, w, h) in sizes.split(',').filter_map(parse_popup_entry) {
        entries.retain(|(known, ..)| *known != id);
        entries.push((id, w, h));
    }
    entries
}

fn join_popup_entries(entries: &[(&'static str, u16, u16)]) -> String {
    entries
        .iter()
        .map(|(id, w, h)| format!("{id}:{w}x{h}"))
        .collect::<Vec<_>>()
        .join(",")
}

#[must_use]
pub fn popup_size(sizes: &str, id: &str) -> Option<(u16, u16)> {
    popup_entries(sizes)
        .into_iter()
        .find(|(known, ..)| *known == id)
        .map(|(_, w, h)| (w, h))
}

#[must_use]
pub fn with_popup_size(sizes: &str, id: &str, size: Option<(u16, u16)>) -> String {
    let mut entries = popup_entries(sizes);
    entries.retain(|(known, ..)| *known != id);
    if let (Some((w, h)), Some(id)) = (size, POPUP_IDS.iter().copied().find(|k| *k == id)) {
        let clamp = |n: u16| n.clamp(POPUP_SIZE_MIN, POPUP_SIZE_MAX);
        entries.push((id, clamp(w), clamp(h)));
    }
    join_popup_entries(&entries)
}

pub(super) fn sanitize_popup_sizes(value: &str) -> String {
    join_popup_entries(&popup_entries(value))
}

fn valid_popup_sizes(value: &str) -> bool {
    value.is_empty()
        || value
            .split(',')
            .all(|entry| parse_popup_entry(entry).is_some())
}

fn valid_setting_value(key: &str, value: &str) -> bool {
    match key {
        "remote_images" | "cache_long_openai" => matches!(value, "on" | "off"),
        "cache_lifetime" => matches!(value, "default" | "extended"),
        "show_stats" | "show_reasoning" | "hide_hints" => matches!(value, "0" | "1"),
        "temperature" => {
            value.is_empty()
                || value
                    .parse::<f32>()
                    .is_ok_and(|value| value.is_finite() && (0.0..=2.0).contains(&value))
        }
        "top_p" => {
            value.is_empty()
                || value
                    .parse::<f32>()
                    .is_ok_and(|value| value.is_finite() && (0.0..=1.0).contains(&value))
        }
        "max_tokens" => value.is_empty() || value.parse::<u32>().is_ok_and(|value| value > 0),
        "compact_threshold" => value.parse::<u8>().is_ok_and(|value| value <= 100),
        "usage_range" => crate::db::UsageRange::CYCLE
            .iter()
            .any(|r| r.key() == value),
        "verbosity" => VERBOSITY_LEVELS.contains(&value),
        "reply_style" => super::REPLY_STYLES.contains(&value),
        "image_viewer" => super::IMAGE_VIEWERS.contains(&value),
        "search_provider" => SEARCH_PROVIDERS.contains(&value),
        "column_width" => valid_column_width(value),
        "popup_sizes" => valid_popup_sizes(value),
        _ => true,
    }
}
