//! Usage log and cost analytics.

use anyhow::Result;
use chrono::Utc;
use uuid::Uuid;

use super::Db;
use crate::provider::ModelPricing;

/// Time window for the `/usage` dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageRange {
    Day,
    Week,
    Month,
    #[default]
    All,
}

impl UsageRange {
    /// Cycle order for the popup's range key.
    pub const CYCLE: [Self; 4] = [Self::Day, Self::Week, Self::Month, Self::All];

    /// Short badge label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Day => "24h",
            Self::Week => "7d",
            Self::Month => "30d",
            Self::All => "all",
        }
    }

    /// Long form for titles and empty-state messages.
    pub const fn title(self) -> &'static str {
        match self {
            Self::Day => "last 24 hours",
            Self::Week => "last 7 days",
            Self::Month => "last 30 days",
            Self::All => "all time",
        }
    }

    /// Persisted `app_settings` key value.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::All => "all",
        }
    }

    /// Parse a persisted `app_settings` value; unknown keys fall back to the
    /// default.
    pub fn from_key(key: &str) -> Self {
        Self::CYCLE
            .iter()
            .copied()
            .find(|r| r.key() == key)
            .unwrap_or_default()
    }

    /// The next window in cycle order.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Day => Self::Week,
            Self::Week => Self::Month,
            Self::Month => Self::All,
            Self::All => Self::Day,
        }
    }

    /// The previous window in cycle order.
    #[must_use]
    pub const fn prev(self) -> Self {
        match self {
            Self::Day => Self::All,
            Self::Week => Self::Day,
            Self::Month => Self::Week,
            Self::All => Self::Month,
        }
    }

    /// Inclusive cutoff timestamp for SQL filtering; `None` = no filter.
    pub fn since(self) -> Option<chrono::DateTime<chrono::Utc>> {
        use chrono::{Duration, Utc};
        match self {
            Self::Day => Some(Utc::now() - Duration::hours(24)),
            Self::Week => Some(Utc::now() - Duration::days(7)),
            Self::Month => Some(Utc::now() - Duration::days(30)),
            Self::All => None,
        }
    }

    /// Empty-state message for the dashboard/status line.
    pub const fn empty_message(self) -> &'static str {
        match self {
            Self::Day => "no usage in the last 24 hours — ←/→ for a wider window",
            Self::Week => "no usage in the last 7 days — ←/→ for a wider window",
            Self::Month => "no usage in the last 30 days — ←/→ for a wider window",
            Self::All => "no usage logged yet — send a message first",
        }
    }
}

/// The bare model name from a `vendor/name` id, with backend prefixes stripped.
pub fn price_name(model: &str) -> &str {
    let stripped = ["go:", "openai:", "codex:", "opencode:"]
        .iter()
        .find_map(|p| model.strip_prefix(p))
        .unwrap_or(model);
    stripped.rsplit('/').next().unwrap_or(stripped)
}

/// Catalog price for a model: exact key first, then the shortest `vendor/name`
/// match, against an in-memory snapshot so a backfill needs no per-row SQL.
fn catalog_price<'a>(
    prices: &'a std::collections::HashMap<String, ModelPricing>,
    model: &str,
) -> Option<&'a ModelPricing> {
    if let Some(price) = prices.get(model) {
        return Some(price);
    }
    let name = price_name(model);
    if name.is_empty() {
        return None;
    }
    prices
        .iter()
        .filter(|(id, _)| {
            id.strip_suffix(name)
                .is_some_and(|rest| rest.ends_with('/'))
        })
        .min_by_key(|(id, _)| id.len()) // shortest vendor wins, deterministic
        .map(|(_, price)| price)
}

/// Price a token breakdown; cache buckets are subsets of the normalized prompt
/// total.
fn catalog_request_cost(
    price: ModelPricing,
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
) -> f64 {
    debug_assert!(
        cache_read_tokens + cache_creation_tokens <= prompt_tokens,
        "cache buckets exceed the prompt total: {cache_read_tokens} + \
         {cache_creation_tokens} > {prompt_tokens}"
    );
    let reads = cache_read_tokens.min(prompt_tokens);
    let writes = cache_creation_tokens.min(prompt_tokens.saturating_sub(reads));
    let ordinary = prompt_tokens.saturating_sub(reads).saturating_sub(writes);
    let read_price = price.cache_read.unwrap_or(price.prompt);
    let write_price = price.cache_write.unwrap_or(price.prompt);
    (ordinary as f64 * price.prompt
        + reads as f64 * read_price
        + writes as f64 * write_price
        + completion_tokens as f64 * price.completion)
        / 1e6
}

struct CostBackfillRow {
    id: i64,
    backend: String,
    model: String,
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    old_cost: Option<f64>,
    /// `None` means provenance predates this field.
    cost_is_provider: Option<bool>,
}

/// Lifetime token/cost totals across every logged request.
#[derive(Default)]
pub struct UsageTotals {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    /// Prompt tokens from rows whose cache accounting is trustworthy.
    pub rated_prompt_tokens: u64,
    /// Cache reads from those same rows.
    pub rated_cache_read_tokens: u64,
    /// Total USD (0 when no model had a known price).
    pub cost: f64,
}

/// One day's aggregate row from `usage_log` (CLI `--by-day`).
#[derive(Default)]
pub struct UsageDay {
    /// `YYYY-MM-DD`, from the RFC 3339 `created_at` prefix.
    pub day: String,
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    /// Prompt tokens from rows whose cache accounting is trustworthy.
    pub rated_prompt_tokens: u64,
    /// Cache reads from those same rows.
    pub rated_cache_read_tokens: u64,
    pub cost: f64,
}

/// One backend's aggregate row.
#[derive(Default)]
pub struct UsageByBackend {
    pub backend: String,
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    /// Prompt tokens from rows whose cache accounting is trustworthy.
    pub rated_prompt_tokens: u64,
    /// Cache reads from those same rows.
    pub rated_cache_read_tokens: u64,
    /// Tokens the provider had already seen on each request's cache lane.
    pub reusable_tokens: u64,
    /// Those tokens it served from cache.
    pub reused_tokens: u64,
    pub cost: f64,
}

/// One model's aggregate row.
#[derive(Default)]
pub struct UsageByModel {
    pub model: String,
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    /// Prompt tokens from rows whose cache accounting is trustworthy.
    pub rated_prompt_tokens: u64,
    /// Cache reads from those same rows.
    pub rated_cache_read_tokens: u64,
    /// Tokens the provider had already seen on each request's cache lane.
    pub reusable_tokens: u64,
    /// Those tokens it served from cache.
    pub reused_tokens: u64,
    pub cost: f64,
}

/// One logged request.
#[derive(Default, serde::Serialize)]
pub struct UsageRow {
    pub created_at: String,
    pub backend: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    /// How this row's prompt count was reported; `Unknown` claims no cache rate.
    pub prompt_convention: crate::provider::PromptConvention,
    pub cost: Option<f64>,
}

/// One session request, including content-free purpose and called tool names.
#[derive(serde::Serialize)]
pub struct SessionUsageRow {
    #[serde(flatten)]
    pub usage: UsageRow,
    pub kind: Option<String>,
    pub tools: Option<String>,
}

fn usage_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<UsageRow> {
    Ok(UsageRow {
        created_at: r.get(0)?,
        backend: r.get(1)?,
        model: r.get(2)?,
        prompt_tokens: r.get::<_, i64>(3)? as u64,
        completion_tokens: r.get::<_, i64>(4)? as u64,
        cache_read_tokens: r.get::<_, i64>(5)? as u64,
        cache_creation_tokens: r.get::<_, i64>(6)? as u64,
        prompt_convention: r.get::<_, Option<String>>(7)?.as_deref().map_or(
            crate::provider::PromptConvention::Unknown,
            crate::provider::PromptConvention::from_str_or_unknown,
        ),
        cost: r.get(8)?,
    })
}

/// Content-free attribution captured when a background request starts.
#[derive(Clone)]
pub(crate) struct UsageLog {
    pub path: std::path::PathBuf,
    pub session_id: Option<String>,
    pub space_id: String,
}

impl UsageLog {
    pub fn record(&self, backend: &str, model: &str, usage: crate::provider::Usage, kind: &str) {
        if let Ok(db) = Db::open(&self.path) {
            let _ = db.record_usage(backend, model, usage, self, kind);
        }
    }
}

impl Db {
    /// Record provider accounting with a cache-aware catalog fallback.
    pub(crate) fn record_usage(
        &self,
        backend: &str,
        model: &str,
        usage: crate::provider::Usage,
        log: &UsageLog,
        kind: &str,
    ) -> Result<i64> {
        let cost = usage.cost.or_else(|| {
            self.request_cost(
                model,
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.cache_read_tokens,
                usage.cache_creation_tokens,
            )
        });
        let id = self.log_usage(
            backend,
            model,
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.cache_read_tokens,
            usage.cache_creation_tokens,
            usage.prompt_convention,
            cost,
            usage.cost.is_some(),
            log.session_id.as_deref(),
            Some(&log.space_id),
        )?;
        self.set_usage_kind(id, kind)?;
        Ok(id)
    }

    /// Attribute an existing accounting row without retaining any content.
    pub fn set_usage_kind(&self, id: i64, kind: &str) -> Result<()> {
        self.conn
            .execute("UPDATE usage_log SET kind = ?2 WHERE id = ?1", (id, kind))?;
        Ok(())
    }

    /// Append only a called tool's name to its chat request's accounting row.
    pub fn add_usage_tool(&self, id: i64, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE usage_log SET tools = CASE WHEN tools IS NULL THEN ?2 ELSE tools || ',' || ?2 END,
             updated_at = ?3 WHERE id = ?1",
            (id, name, Utc::now().to_rfc3339()),
        )?;
        Ok(())
    }

    /// All requests attributed to a session, oldest first (including synced rows).
    pub fn usage_for_session(&self, session_id: &str) -> Result<Vec<SessionUsageRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT created_at, backend, model, prompt_tokens, completion_tokens,
             cache_read_tokens, cache_creation_tokens, prompt_convention, cost, kind, tools
             FROM usage_log WHERE session_id = ?1 ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([session_id], |r| {
            Ok(SessionUsageRow {
                usage: usage_row(r)?,
                kind: r.get(9)?,
                tools: r.get(10)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Record one completed request's usage; content-free, so it never leaks
    /// conversation text.
    #[allow(clippy::too_many_arguments)]
    pub fn log_usage(
        &self,
        backend: &str,
        model: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
        cache_read_tokens: u64,
        cache_creation_tokens: u64,
        prompt_convention: crate::provider::PromptConvention,
        cost: Option<f64>,
        cost_is_provider: bool,
        session_id: Option<&str>,
        space_id: Option<&str>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO usage_log (sync_id, created_at, session_id, space_id, backend, model,
                prompt_tokens, completion_tokens, cache_read_tokens, cache_creation_tokens,
                prompt_convention, cost, cost_is_provider, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            (
                Uuid::new_v4().to_string(),
                Utc::now().to_rfc3339(),
                session_id,
                space_id,
                backend,
                model,
                prompt_tokens as i64,
                completion_tokens as i64,
                cache_read_tokens as i64,
                cache_creation_tokens as i64,
                prompt_convention.as_str(),
                cost,
                i64::from(cost_is_provider),
                Utc::now().to_rfc3339(),
            ),
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Update a usage row written earlier in the same request; `OpenCode` Zen
    /// splits usage and cost across two streamed events.
    #[allow(clippy::too_many_arguments)]
    pub fn update_usage(
        &self,
        row_id: i64,
        prompt_tokens: u64,
        completion_tokens: u64,
        cache_read_tokens: u64,
        cache_creation_tokens: u64,
        cost: Option<f64>,
        cost_is_provider: bool,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE usage_log SET prompt_tokens = ?1, completion_tokens = ?2,
                cache_read_tokens = ?3, cache_creation_tokens = ?4, cost = ?5,
                cost_is_provider = ?6, updated_at = ?7
             WHERE id = ?8",
            (
                prompt_tokens as i64,
                completion_tokens as i64,
                cache_read_tokens as i64,
                cache_creation_tokens as i64,
                cost,
                i64::from(cost_is_provider),
                Utc::now().to_rfc3339(),
                row_id,
            ),
        )?;
        Ok(())
    }

    /// Estimated cost of one completed request in USD, or `None` when no price is
    /// known.
    pub fn request_cost(
        &self,
        model: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
        cache_read_tokens: u64,
        cache_creation_tokens: u64,
    ) -> Option<f64> {
        self.model_price(model).map(|price| {
            catalog_request_cost(
                price,
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                cache_creation_tokens,
            )
        })
    }

    /// Reconcile estimated request costs with the current catalog; provider-
    /// reported costs are never overwritten. Returns how many rows were visited.
    pub fn backfill_usage_costs(&mut self) -> Result<usize> {
        // The catalog endpoint reports USD per token while cost formulas here use
        // USD per 1M; heal a per-token-shaped catalog first.
        let max_price: f64 = self.conn.query_row(
            "SELECT COALESCE(MAX(prompt_price), 0) FROM cache.model_prices",
            [],
            |r| r.get(0),
        )?;
        if max_price > 0.0 && max_price < 0.001 {
            self.conn.execute(
                "UPDATE cache.model_prices SET
                    prompt_price = prompt_price * 1e6,
                    completion_price = completion_price * 1e6,
                    cache_read_price = cache_read_price * 1e6,
                    cache_write_price = cache_write_price * 1e6",
                [],
            )?;
        }
        // Snapshot the catalog, then rewrite every usage row in one transaction.
        let mut prices: std::collections::HashMap<String, ModelPricing> =
            std::collections::HashMap::default();
        {
            let mut stmt = self.conn.prepare(
                "SELECT model_id, prompt_price, completion_price,
                        cache_read_price, cache_write_price
                 FROM cache.model_prices",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    ModelPricing {
                        prompt: r.get(1)?,
                        completion: r.get(2)?,
                        cache_read: r.get(3)?,
                        cache_write: r.get(4)?,
                    },
                ))
            })?;
            for row in rows {
                let (model, price) = row?;
                prices.insert(model, price);
            }
        }
        let rows: Vec<CostBackfillRow> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, backend, model, prompt_tokens, completion_tokens,
                        cache_read_tokens, cache_creation_tokens, cost,
                        cost_is_provider
                 FROM usage_log",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(CostBackfillRow {
                    id: r.get(0)?,
                    backend: r.get(1)?,
                    model: r.get(2)?,
                    prompt_tokens: r.get::<_, i64>(3)? as u64,
                    completion_tokens: r.get::<_, i64>(4)? as u64,
                    cache_read_tokens: r.get::<_, i64>(5)? as u64,
                    cache_creation_tokens: r.get::<_, i64>(6)? as u64,
                    old_cost: r.get(7)?,
                    cost_is_provider: r.get::<_, Option<i64>>(8)?.map(|v| v != 0),
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let tx = self.conn.transaction()?;
        {
            let mut update = tx.prepare(
                "UPDATE usage_log SET cost = ?2, cost_is_provider = 0, updated_at = ?3 WHERE id = ?1",
            )?;
            for row in &rows {
                // Rows without a provenance bit may carry exact provider costs;
                // preserve them.
                let legacy_opencode_cost = row.cost_is_provider.is_none()
                    && row.backend == "OpenCode Go"
                    && row.old_cost.is_some();
                if row.cost_is_provider == Some(true) || legacy_opencode_cost {
                    continue;
                }
                let recomputed = catalog_price(&prices, &row.model).map(|price| {
                    catalog_request_cost(
                        *price,
                        row.prompt_tokens,
                        row.completion_tokens,
                        row.cache_read_tokens,
                        row.cache_creation_tokens,
                    )
                });
                // Fill missing rows and heal stale estimates; never erase a cost
                // when a model leaves the catalog.
                let write = match (recomputed, row.old_cost) {
                    (Some(cost), None) => Some(cost),
                    (Some(cost), Some(old)) if (cost - old).abs() > 1e-12 => Some(cost),
                    _ => None,
                };
                if let Some(cost) = write {
                    update.execute((row.id, cost, Utc::now().to_rfc3339()))?;
                }
            }
        }
        tx.commit()?;
        Ok(rows.len())
    }

    /// Batch save catalog prices in one transaction, skipping rows whose price
    /// didn't move.
    pub fn upsert_model_prices(&mut self, prices: &[(String, String, ModelPricing)]) -> Result<()> {
        if prices.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO cache.model_prices (model_id, backend, prompt_price, completion_price,
                    cache_read_price, cache_write_price, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(model_id) DO UPDATE SET
                    prompt_price = excluded.prompt_price,
                    completion_price = excluded.completion_price,
                    cache_read_price = excluded.cache_read_price,
                    cache_write_price = excluded.cache_write_price,
                    updated_at = excluded.updated_at
                 WHERE cache.model_prices.prompt_price != excluded.prompt_price
                    OR cache.model_prices.completion_price != excluded.completion_price
                    OR cache.model_prices.cache_read_price IS NOT excluded.cache_read_price
                    OR cache.model_prices.cache_write_price IS NOT excluded.cache_write_price",
            )?;
            let now = Utc::now().to_rfc3339();
            for (model, backend, price) in prices {
                stmt.execute((
                    model,
                    backend,
                    price.prompt,
                    price.completion,
                    price.cache_read,
                    price.cache_write,
                    &now,
                ))?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Catalog prices for a model in USD per 1M tokens, falling back to its
    /// `OpenRouter` twin.
    pub fn model_price(&self, model: &str) -> Option<ModelPricing> {
        let read_price = |r: &rusqlite::Row| {
            Ok(ModelPricing {
                prompt: r.get(0)?,
                completion: r.get(1)?,
                cache_read: r.get(2)?,
                cache_write: r.get(3)?,
            })
        };
        if let Ok(price) = self.conn.query_row(
            "SELECT prompt_price, completion_price, cache_read_price, cache_write_price
             FROM cache.model_prices WHERE model_id = ?1",
            [model],
            read_price,
        ) {
            return Some(price);
        }
        let name = price_name(model);
        if name.is_empty() {
            return None;
        }
        // Suffix match on `vendor/name`: the shortest vendor wins when a
        // bare name appears under several vendors.
        self.conn
            .query_row(
                "SELECT prompt_price, completion_price, cache_read_price, cache_write_price
                 FROM cache.model_prices
                 WHERE backend = 'OpenRouter'
                   AND substr(model_id, -length(?1) - 1) = '/' || ?1
                 ORDER BY length(model_id) LIMIT 1",
                [name],
                read_price,
            )
            .ok()
    }

    /// Totals across logged requests, optionally since an RFC3339 cutoff.
    pub fn usage_totals(&self, since: Option<&str>) -> Result<UsageTotals> {
        let mut sql = String::from(
            "SELECT COUNT(*),
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(cache_creation_tokens), 0),
                    COALESCE(SUM(CASE WHEN prompt_convention IN ('inclusive','normalized_from_exclusive')
                                      THEN prompt_tokens ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN prompt_convention IN ('inclusive','normalized_from_exclusive')
                                      THEN cache_read_tokens ELSE 0 END), 0),
                    COALESCE(SUM(cost), 0)
             FROM usage_log",
        );
        if since.is_some() {
            sql.push_str(" WHERE created_at >= ?1");
        }
        let map = |r: &rusqlite::Row| {
            Ok(UsageTotals {
                requests: r.get::<_, i64>(0)? as u64,
                prompt_tokens: r.get::<_, i64>(1)? as u64,
                completion_tokens: r.get::<_, i64>(2)? as u64,
                cache_read_tokens: r.get::<_, i64>(3)? as u64,
                cache_creation_tokens: r.get::<_, i64>(4)? as u64,
                rated_prompt_tokens: r.get::<_, i64>(5)? as u64,
                rated_cache_read_tokens: r.get::<_, i64>(6)? as u64,
                cost: r.get::<_, f64>(7)?,
            })
        };
        let totals = match since {
            Some(s) => self.conn.query_row(&sql, [s], map),
            None => self.conn.query_row(&sql, [], map),
        }?;
        Ok(totals)
    }

    /// Per-backend aggregates, most-used first.
    pub fn usage_by_backend(&self, since: Option<&str>) -> Result<Vec<UsageByBackend>> {
        let mut sql = String::from(
            "SELECT backend, COUNT(*),
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(CASE WHEN prompt_convention IN ('inclusive','normalized_from_exclusive')
                                      THEN prompt_tokens ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN prompt_convention IN ('inclusive','normalized_from_exclusive')
                                      THEN cache_read_tokens ELSE 0 END), 0),
                    COALESCE(SUM(cost), 0),
                    COALESCE(SUM(CASE WHEN prompt_convention IN ('inclusive','normalized_from_exclusive')
                                      THEN rc.reusable_tokens ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN prompt_convention IN ('inclusive','normalized_from_exclusive')
                                      THEN MIN(cache_read_tokens, rc.reusable_tokens) ELSE 0 END), 0)
             FROM usage_log LEFT JOIN cache.request_cache rc ON rc.usage_id = usage_log.id",
        );
        if since.is_some() {
            sql.push_str(" WHERE created_at >= ?1");
        }
        sql.push_str(" GROUP BY backend ORDER BY COUNT(*) DESC");
        let map = |r: &rusqlite::Row| {
            Ok(UsageByBackend {
                backend: r.get(0)?,
                requests: r.get::<_, i64>(1)? as u64,
                prompt_tokens: r.get::<_, i64>(2)? as u64,
                completion_tokens: r.get::<_, i64>(3)? as u64,
                cache_read_tokens: r.get::<_, i64>(4)? as u64,
                rated_prompt_tokens: r.get::<_, i64>(5)? as u64,
                rated_cache_read_tokens: r.get::<_, i64>(6)? as u64,
                cost: r.get::<_, f64>(7)?,
                reusable_tokens: r.get::<_, i64>(8)? as u64,
                reused_tokens: r.get::<_, i64>(9)? as u64,
            })
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = match since {
            Some(s) => stmt.query_map([s], map),
            None => stmt.query_map([], map),
        }?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Per-model aggregates, most-used first.
    pub fn usage_by_model(&self, limit: u64, since: Option<&str>) -> Result<Vec<UsageByModel>> {
        let mut sql = String::from(
            "SELECT model, COUNT(*),
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(CASE WHEN prompt_convention IN ('inclusive','normalized_from_exclusive')
                                      THEN prompt_tokens ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN prompt_convention IN ('inclusive','normalized_from_exclusive')
                                      THEN cache_read_tokens ELSE 0 END), 0),
                    COALESCE(SUM(cost), 0),
                    COALESCE(SUM(CASE WHEN prompt_convention IN ('inclusive','normalized_from_exclusive')
                                      THEN rc.reusable_tokens ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN prompt_convention IN ('inclusive','normalized_from_exclusive')
                                      THEN MIN(cache_read_tokens, rc.reusable_tokens) ELSE 0 END), 0)
             FROM usage_log LEFT JOIN cache.request_cache rc ON rc.usage_id = usage_log.id",
        );
        if since.is_some() {
            sql.push_str(" WHERE created_at >= ?1");
            sql.push_str(" GROUP BY model ORDER BY COUNT(*) DESC LIMIT ?2");
        } else {
            sql.push_str(" GROUP BY model ORDER BY COUNT(*) DESC LIMIT ?1");
        }
        let map = |r: &rusqlite::Row| {
            Ok(UsageByModel {
                model: r.get(0)?,
                requests: r.get::<_, i64>(1)? as u64,
                prompt_tokens: r.get::<_, i64>(2)? as u64,
                completion_tokens: r.get::<_, i64>(3)? as u64,
                cache_read_tokens: r.get::<_, i64>(4)? as u64,
                rated_prompt_tokens: r.get::<_, i64>(5)? as u64,
                rated_cache_read_tokens: r.get::<_, i64>(6)? as u64,
                cost: r.get::<_, f64>(7)?,
                reusable_tokens: r.get::<_, i64>(8)? as u64,
                reused_tokens: r.get::<_, i64>(9)? as u64,
            })
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = match since {
            Some(s) => stmt.query_map(rusqlite::params![s, limit as i64], map),
            None => stmt.query_map(rusqlite::params![limit as i64], map),
        }?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The most recent logged requests, newest first.
    pub fn usage_recent(&self, limit: u64, since: Option<&str>) -> Result<Vec<UsageRow>> {
        let mut sql = String::from(
            "SELECT created_at, backend, model, prompt_tokens, completion_tokens,
                    cache_read_tokens, cache_creation_tokens, prompt_convention, cost
             FROM usage_log",
        );
        if since.is_some() {
            sql.push_str(" WHERE created_at >= ?1");
            sql.push_str(" ORDER BY id DESC LIMIT ?2");
        } else {
            sql.push_str(" ORDER BY id DESC LIMIT ?1");
        }
        let map = usage_row;
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = match since {
            Some(s) => stmt.query_map(rusqlite::params![s, limit as i64], map),
            None => stmt.query_map(rusqlite::params![limit as i64], map),
        }?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Per-day aggregates, newest day first.
    pub fn usage_by_day(&self, limit: u64, since: Option<&str>) -> Result<Vec<UsageDay>> {
        let mut sql = String::from(
            "SELECT substr(created_at, 1, 10) AS day, COUNT(*),
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(CASE WHEN prompt_convention IN ('inclusive','normalized_from_exclusive')
                                      THEN prompt_tokens ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN prompt_convention IN ('inclusive','normalized_from_exclusive')
                                      THEN cache_read_tokens ELSE 0 END), 0),
                    COALESCE(SUM(cost), 0)
             FROM usage_log",
        );
        if since.is_some() {
            sql.push_str(" WHERE created_at >= ?1");
            sql.push_str(" GROUP BY day ORDER BY day DESC LIMIT ?2");
        } else {
            sql.push_str(" GROUP BY day ORDER BY day DESC LIMIT ?1");
        }
        let map = |r: &rusqlite::Row| {
            Ok(UsageDay {
                day: r.get(0)?,
                requests: r.get::<_, i64>(1)? as u64,
                prompt_tokens: r.get::<_, i64>(2)? as u64,
                completion_tokens: r.get::<_, i64>(3)? as u64,
                cache_read_tokens: r.get::<_, i64>(4)? as u64,
                rated_prompt_tokens: r.get::<_, i64>(5)? as u64,
                rated_cache_read_tokens: r.get::<_, i64>(6)? as u64,
                cost: r.get(7)?,
            })
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = match since {
            Some(s) => stmt.query_map(rusqlite::params![s, limit as i64], map),
            None => stmt.query_map(rusqlite::params![limit as i64], map),
        }?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
