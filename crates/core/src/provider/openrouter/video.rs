//! Video generation parameter validation.

/// Validate basic video generation parameters against known model capabilities.
/// Returns `Ok` if the params look valid, or a descriptive error message if not.
/// This is a best-effort check — the API is the final authority.
pub fn normalize_video_params(
    model: &str,
    duration: u32,
    resolution: &str,
    aspect_ratio: &str,
) -> (u32, String, String) {
    let mut duration = duration;
    let mut resolution = resolution.to_string();
    let mut aspect_ratio = aspect_ratio.to_string();

    if model.starts_with("minimax/hailuo-3") {
        resolution = "2K".to_string();
    } else if model.starts_with("minimax/hailuo-2.3") {
        resolution = "1080p".to_string();
        aspect_ratio = "16:9".to_string();
    } else if model.starts_with("runway/gen-4.5") {
        resolution = "720p".to_string();
        if !matches!(aspect_ratio.as_str(), "16:9" | "9:16") {
            aspect_ratio = "16:9".to_string();
        }
    } else if model.starts_with("kwaivgi/kling-v3.0") || model.starts_with("kwaivgi/kling-video-o1")
    {
        resolution = "720p".to_string();
        if !matches!(aspect_ratio.as_str(), "16:9" | "9:16" | "1:1") {
            aspect_ratio = "16:9".to_string();
        }
    } else if model.starts_with("x-ai/grok-imagine-video") {
        if !matches!(resolution.as_str(), "480p" | "720p" | "1080p") {
            resolution = "720p".to_string();
        }
    } else if model.starts_with("openai/sora-2-pro") {
        if !matches!(duration, 4 | 8 | 12 | 16 | 20) {
            duration = nearest_duration(duration, &[4, 8, 12, 16, 20]);
        }
        if !matches!(resolution.as_str(), "720p" | "1080p") {
            resolution = "1080p".to_string();
        }
        if !matches!(aspect_ratio.as_str(), "16:9" | "9:16") {
            aspect_ratio = "16:9".to_string();
        }
    } else if model.starts_with("alibaba/wan-2.6") {
        if !matches!(duration, 5 | 10) {
            duration = nearest_duration(duration, &[5, 10]);
        }
        if !matches!(resolution.as_str(), "720p" | "1080p") {
            resolution = "1080p".to_string();
        }
        if !matches!(aspect_ratio.as_str(), "16:9" | "9:16") {
            aspect_ratio = "16:9".to_string();
        }
    } else if model.starts_with("google/veo-3.1") {
        if !matches!(duration, 4 | 6 | 8) {
            duration = nearest_duration(duration, &[4, 6, 8]);
        }
        if !matches!(resolution.as_str(), "720p" | "1080p" | "4K") {
            resolution = "1080p".to_string();
        }
        if !matches!(aspect_ratio.as_str(), "16:9" | "9:16") {
            aspect_ratio = "16:9".to_string();
        }
    }

    (duration, resolution, aspect_ratio)
}

fn nearest_duration(requested: u32, allowed: &[u32]) -> u32 {
    *allowed
        .iter()
        .min_by_key(|candidate| candidate.abs_diff(requested))
        .unwrap_or(&requested)
}

// Long by design (model param table).
#[allow(clippy::too_many_lines)]
pub(super) fn check_video_params(
    model: &str,
    duration: u32,
    resolution: &str,
    aspect_ratio: &str,
) -> anyhow::Result<()> {
    // Model-specific duration limits. Keys are model slugs (or prefixes).
    struct Cap {
        max_dur: u32,
        res_ok: bool,
        ar_ok: bool,
    }
    let caps = |model: &str| -> Option<Cap> {
        if model.starts_with("google/veo-3.1-lite")
            || model.starts_with("google/veo-3.1-fast")
            || model.starts_with("google/veo-3.1")
        {
            Some(Cap {
                max_dur: 8,
                res_ok: matches!(resolution, "720p" | "1080p" | "4K"),
                ar_ok: matches!(aspect_ratio, "16:9" | "9:16"),
            })
        } else if model.starts_with("alibaba/wan-2.7") {
            Some(Cap {
                max_dur: 10,
                res_ok: matches!(resolution, "720p" | "1080p"),
                ar_ok: true,
            })
        } else if model.starts_with("bytedance/seedance-2.0") {
            Some(Cap {
                max_dur: 15,
                res_ok: true,
                ar_ok: true,
            })
        } else if model.starts_with("kwaivgi/kling-v3.0")
            || model.starts_with("kwaivgi/kling-video-o1")
        {
            Some(Cap {
                max_dur: 15,
                res_ok: resolution == "720p",
                ar_ok: matches!(aspect_ratio, "16:9" | "9:16" | "1:1"),
            })
        } else if model.starts_with("openai/sora-2-pro") {
            Some(Cap {
                max_dur: 20,
                res_ok: matches!(resolution, "720p" | "1080p"),
                ar_ok: matches!(aspect_ratio, "16:9" | "9:16"),
            })
        } else if model.starts_with("runway/gen-4.5") {
            Some(Cap {
                max_dur: 10,
                res_ok: resolution == "720p",
                ar_ok: matches!(aspect_ratio, "16:9" | "9:16"),
            })
        } else if model.starts_with("x-ai/grok-imagine-video-1.5") {
            Some(Cap {
                max_dur: 15,
                res_ok: matches!(resolution, "480p" | "720p" | "1080p"),
                ar_ok: true,
            })
        } else if model.starts_with("minimax/hailuo-3") {
            Some(Cap {
                max_dur: 15,
                res_ok: resolution == "2K",
                ar_ok: true,
            })
        } else if model.starts_with("bytedance/seedance-1-5-pro") {
            Some(Cap {
                max_dur: 12,
                res_ok: matches!(resolution, "480p" | "720p" | "1080p"),
                ar_ok: true,
            })
        } else if model.starts_with("minimax/hailuo-2.3") {
            Some(Cap {
                max_dur: 10,
                res_ok: resolution == "1080p",
                ar_ok: aspect_ratio == "16:9",
            })
        } else if model.starts_with("alibaba/happyhorse") {
            Some(Cap {
                max_dur: 15,
                res_ok: matches!(resolution, "720p" | "1080p"),
                ar_ok: true,
            })
        } else if model.starts_with("x-ai/grok-imagine-video") {
            Some(Cap {
                max_dur: 15,
                res_ok: matches!(resolution, "480p" | "720p"),
                ar_ok: true,
            })
        } else {
            None // unknown model, skip validation
        }
    };
    if let Some(c) = caps(model) {
        if duration > c.max_dur {
            anyhow::bail!(
                "model {model} does not support duration {duration}s — max {max}s. Change the model in /config.",
                max = c.max_dur
            );
        }
        if !c.res_ok {
            anyhow::bail!(
                "model {model} may not support resolution {resolution}. Change the model in /config."
            );
        }
        if !c.ar_ok {
            anyhow::bail!(
                "model {model} may not support aspect ratio {aspect_ratio}. Change the model in /config."
            );
        }
    }
    Ok(())
}
