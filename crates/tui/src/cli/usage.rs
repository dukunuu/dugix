//! `nexus usage` — usage and cost analytics.

use anyhow::Result;

use nexus_core::db;
use nexus_core::space;

use super::{fmt_req, fmt_tokens, fmt_ts, out, truncate};

// --- read-only commands ---

pub(super) fn usage(
    range_key: &str,
    json: bool,
    top: u64,
    by_day: bool,
    session: Option<&str>,
) -> Result<()> {
    let space = space::Space::open()?;
    let mut db = db::Db::open(&space.db_path())?;
    let _ = db.backfill_usage_costs(); // price rows logged before pricing existed
    if let Some(session) = session {
        out(session_usage_output(&db, session, json)?);
        return Ok(());
    }
    let range = db::UsageRange::from_key(range_key);
    let since = range.since().map(|t| t.to_rfc3339());
    let totals = db.usage_totals(since.as_deref())?;
    if json {
        return usage_json(&db, range, since.as_deref(), &totals, top, by_day);
    }

    out(format!("usage — {}", range.label()));
    if totals.requests == 0 {
        out(range.empty_message());
        return Ok(());
    }
    print_usage_totals(&totals);

    let by_backend = db.usage_by_backend(since.as_deref())?;
    if !by_backend.is_empty() {
        out("");
        out("by backend");
        for b in by_backend {
            out(format!(
                "  {:<24} {:>8} req   prompt {:<8} cached {:<8} {:>5} ${:.4}",
                truncate(&b.backend, 24),
                fmt_req(b.requests),
                fmt_tokens(b.prompt_tokens),
                fmt_tokens(b.cache_read_tokens),
                fmt_cache_rate(b.rated_cache_read_tokens, b.rated_prompt_tokens),
                b.cost,
            ));
        }
    }
    let by_model = db.usage_by_model(top, since.as_deref())?;
    if !by_model.is_empty() {
        out("");
        out(format!("by model (top {top})"));
        for m in by_model {
            out(format!(
                "  {:<32} {:>8} req   prompt {:<8} cached {:<8} {:>5} {:<8} ${:.4}",
                truncate(&m.model, 32),
                fmt_req(m.requests),
                fmt_tokens(m.prompt_tokens),
                fmt_tokens(m.cache_read_tokens),
                fmt_cache_rate(m.rated_cache_read_tokens, m.rated_prompt_tokens),
                caching_mode(&m.model),
                m.cost,
            ));
        }
    }
    if by_day {
        let days = db.usage_by_day(top, since.as_deref())?;
        if !days.is_empty() {
            out("");
            out(format!("by day (top {top})"));
            for d in days {
                out(format!(
                    "  {:<12} {:>8} req   prompt {:<8} completion {:<8} ${:.4}",
                    d.day,
                    fmt_req(d.requests),
                    fmt_tokens(d.prompt_tokens),
                    fmt_tokens(d.completion_tokens),
                    d.cost,
                ));
            }
        }
    }
    let recent = db.usage_recent(8, since.as_deref())?;
    if !recent.is_empty() {
        out("");
        out("recent requests");
        for r in recent {
            let cost = r.cost.map(|c| format!(" ${c:.4}")).unwrap_or_default();
            out(format!(
                "  {}  {:<16}  {:<32}  {}→{}  {}",
                fmt_ts(&r.created_at),
                truncate(&r.backend, 16),
                truncate(&r.model, 32),
                fmt_tokens(r.prompt_tokens),
                fmt_tokens(r.completion_tokens),
                cost,
            ));
        }
    }
    Ok(())
}

pub(super) fn session_usage_output(db: &db::Db, session: &str, json: bool) -> Result<String> {
    use nexus_core::app::usage::{SessionUsageTotals, session_usage_lines};
    let requests = db.usage_for_session(session)?;
    if json {
        return Ok(serde_json::to_string_pretty(&serde_json::json!({
            "session_id": session,
            "totals": SessionUsageTotals::from_rows(&requests),
            "requests": requests,
        }))?);
    }
    Ok(format!(
        "usage — session {session}\ninput / cached / output · USD\n{}",
        session_usage_lines(&requests).join("\n")
    ))
}

/// The headline block: counts, cost, and the hit rate over the rows that can
/// actually support one.
fn print_usage_totals(totals: &nexus_core::db::UsageTotals) {
    out(format!("requests: {}", fmt_req(totals.requests)));
    out(format!(
        "prompt: {}   completion: {}   cached reads: {}   cached writes: {}",
        fmt_tokens(totals.prompt_tokens),
        fmt_tokens(totals.completion_tokens),
        fmt_tokens(totals.cache_read_tokens),
        fmt_tokens(totals.cache_creation_tokens),
    ));
    out(format!("cost: ${:.4}", totals.cost));
    out(format!(
        "cache hit rate: {}",
        fmt_cache_rate(totals.rated_cache_read_tokens, totals.rated_prompt_tokens)
    ));
    let unrated = totals
        .prompt_tokens
        .saturating_sub(totals.rated_prompt_tokens);
    if unrated > 0 {
        out(format!(
            "  ({} prompt tokens excluded: provider reported no cache accounting, \
             or the row predates convention normalization)",
            fmt_tokens(unrated)
        ));
    }
}

/// `82%`, or `—` when nothing in the window can be rated. Never a confident
/// `0%` for "the provider told us nothing".
fn fmt_cache_rate(rated_cache_read: u64, rated_prompt: u64) -> String {
    if rated_prompt == 0 {
        return "—".to_string();
    }
    #[allow(clippy::cast_precision_loss)] // a ratio of token counts is exact enough
    let rate = rated_cache_read as f64 / rated_prompt as f64;
    format!("{:.0}%", rate * 100.0)
}

/// How a model's prompt cache is engaged: `explicit` when the request has to
/// mark a breakpoint, `auto` when the provider caches on its own. A model
/// showing 0% under `auto` really did miss; under `explicit` check that
/// breakpoints are reaching it.
fn caching_mode(model: &str) -> &'static str {
    if nexus_core::app::model_needs_cache_breakpoints(model) {
        "explicit"
    } else {
        "auto"
    }
}

fn usage_json(
    db: &db::Db,
    range: db::UsageRange,
    since: Option<&str>,
    totals: &db::UsageTotals,
    top: u64,
    by_day: bool,
) -> Result<()> {
    let by_backend = db.usage_by_backend(since)?;
    let by_model = db.usage_by_model(top, since)?;
    let recent = db.usage_recent(8, since)?;
    let mut v = serde_json::json!({
        "range": range.label(),
        "totals": {
            "requests": totals.requests,
            "prompt_tokens": totals.prompt_tokens,
            "completion_tokens": totals.completion_tokens,
            "cache_read_tokens": totals.cache_read_tokens,
            "cache_creation_tokens": totals.cache_creation_tokens,
            // The rate's numerator and denominator, restricted to rows whose
            // prompt-token convention is known. Divide these, not the raw
            // totals: the raw pair can mix conventions.
            "rated_prompt_tokens": totals.rated_prompt_tokens,
            "rated_cache_read_tokens": totals.rated_cache_read_tokens,
            "cost": totals.cost,
        },
        "by_backend": by_backend.iter().map(|b| serde_json::json!({
            "backend": b.backend, "requests": b.requests,
            "prompt_tokens": b.prompt_tokens, "completion_tokens": b.completion_tokens,
            "cache_read_tokens": b.cache_read_tokens,
            "rated_prompt_tokens": b.rated_prompt_tokens,
            "rated_cache_read_tokens": b.rated_cache_read_tokens, "cost": b.cost,
        })).collect::<Vec<_>>(),
        "by_model": by_model.iter().map(|m| serde_json::json!({
            "model": m.model, "requests": m.requests,
            "prompt_tokens": m.prompt_tokens, "completion_tokens": m.completion_tokens,
            "cache_read_tokens": m.cache_read_tokens,
            "rated_prompt_tokens": m.rated_prompt_tokens,
            "rated_cache_read_tokens": m.rated_cache_read_tokens,
            "caching_mode": caching_mode(&m.model), "cost": m.cost,
        })).collect::<Vec<_>>(),
        "recent": recent.iter().map(|r| serde_json::json!({
            "created_at": r.created_at, "backend": r.backend, "model": r.model,
            "prompt_tokens": r.prompt_tokens, "completion_tokens": r.completion_tokens,
            "cache_read_tokens": r.cache_read_tokens,
            "cache_creation_tokens": r.cache_creation_tokens,
            "prompt_convention": r.prompt_convention.as_str(), "cost": r.cost,
        })).collect::<Vec<_>>(),
    });
    if by_day {
        let days = db.usage_by_day(top, since)?;
        v["by_day"] = serde_json::json!(
            days.iter()
                .map(|d| serde_json::json!({
                    "day": d.day, "requests": d.requests,
                    "prompt_tokens": d.prompt_tokens, "completion_tokens": d.completion_tokens,
                    "cache_read_tokens": d.cache_read_tokens,
                    "rated_prompt_tokens": d.rated_prompt_tokens,
                    "rated_cache_read_tokens": d.rated_cache_read_tokens, "cost": d.cost,
                }))
                .collect::<Vec<_>>()
        );
    }
    out(serde_json::to_string_pretty(&v)?);
    Ok(())
}
