//! Content-free usage analytics from per-request accounting.

use super::App;

use crate::db::{SessionUsageRow, UsageByBackend, UsageByModel, UsageRange, UsageRow, UsageTotals};

#[derive(Default, serde::Serialize)]
pub struct SessionUsageTotals {
    pub requests: usize,
    pub prompt_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub completion_tokens: u64,
    pub cost: Option<f64>,
    pub unknown_cost_requests: usize,
}

impl SessionUsageTotals {
    #[must_use]
    pub fn from_rows(rows: &[SessionUsageRow]) -> Self {
        let mut total = Self {
            requests: rows.len(),
            ..Self::default()
        };
        for row in rows {
            let usage = &row.usage;
            total.prompt_tokens += usage.prompt_tokens;
            total.cache_read_tokens += usage.cache_read_tokens;
            total.cache_creation_tokens += usage.cache_creation_tokens;
            total.completion_tokens += usage.completion_tokens;
            if let Some(cost) = usage.cost {
                total.cost = Some(total.cost.unwrap_or_default() + cost);
            } else {
                total.unknown_cost_requests += 1;
            }
        }
        total
    }
}

/// Format session usage with local timestamps.
#[must_use]
pub fn session_usage_lines(rows: &[SessionUsageRow]) -> Vec<String> {
    let cost = |value: Option<f64>| value.map_or_else(|| "—".to_string(), |v| format!("${v:.4}"));
    let mut lines = Vec::new();
    if rows.is_empty() {
        lines.push("No requests in this session.".to_string());
    }
    for row in rows {
        let usage = &row.usage;
        let time = chrono::DateTime::parse_from_rfc3339(&usage.created_at).map_or_else(
            |_| "--:--:--".to_string(),
            |t| {
                t.with_timezone(&chrono::Local)
                    .format("%H:%M:%S")
                    .to_string()
            },
        );
        let tools = row
            .tools
            .as_ref()
            .map_or_else(String::new, |names| format!(" · {names}"));
        lines.push(format!(
            "{time}  {}{tools}",
            row.kind.as_deref().unwrap_or("—")
        ));
        lines.push(format!(
            "  {}  {} / {} / {}  {}",
            usage.model,
            usage.prompt_tokens,
            usage.cache_read_tokens,
            usage.completion_tokens,
            cost(usage.cost)
        ));
    }
    let total = SessionUsageTotals::from_rows(rows);
    lines.push(format!(
        "Total: {} requests · {} tokens · {}",
        total.requests,
        total.prompt_tokens + total.completion_tokens,
        cost(total.cost)
    ));
    if total.unknown_cost_requests > 0 {
        lines.push(format!(
            "  {} request(s) with unknown cost; total includes known costs only",
            total.unknown_cost_requests
        ));
    }
    lines
}

pub struct UsageData {
    pub totals: UsageTotals,
    pub by_backend: Vec<UsageByBackend>,
    pub by_model: Vec<UsageByModel>,
    pub recent: Vec<UsageRow>,
}

impl App {
    pub(crate) fn usage_log(&self) -> Option<crate::db::UsageLog> {
        (!self.incognito).then(|| crate::db::UsageLog {
            path: self.space.db_path(),
            session_id: self.session.as_ref().map(|s| s.id.clone()),
            space_id: self.active_space.id.clone(),
        })
    }

    /// Attribute indexing usage to the space rather than the viewed session.
    pub(crate) fn background_usage_log(&self) -> Option<crate::db::UsageLog> {
        self.usage_log().map(|log| crate::db::UsageLog {
            session_id: None,
            ..log
        })
    }

    pub(crate) fn usage_provider(
        &self,
        provider: crate::provider::openrouter::OpenRouter,
        kind: &str,
    ) -> crate::provider::openrouter::OpenRouter {
        provider.logged(self.usage_log(), kind)
    }

    pub fn load_usage(&self) -> UsageData {
        self.load_usage_for_range(self.usage_range)
    }

    pub fn load_usage_for_range(&self, range: UsageRange) -> UsageData {
        let since = range.since().map(|t| t.to_rfc3339());
        UsageData {
            totals: self.db.usage_totals(since.as_deref()).unwrap_or_default(),
            by_backend: self
                .db
                .usage_by_backend(since.as_deref())
                .unwrap_or_default(),
            by_model: self
                .db
                .usage_by_model(10, since.as_deref())
                .unwrap_or_default(),
            recent: self
                .db
                .usage_recent(200, since.as_deref())
                .unwrap_or_default(),
        }
    }

    pub fn persist_usage_range(&mut self) {
        let _ = self.db.set_setting("usage_range", self.usage_range.key());
    }

    pub fn backfill_usage_costs(&mut self) {
        let _ = self.db.backfill_usage_costs();
    }
}
