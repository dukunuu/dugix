//! Per-request prompt-cache accounting kept in the device-local cache db.
//!
//! The provider can only reuse what it has already seen on a cache lane, so
//! reuse is measured against the previous request of the same session and
//! cache key, and each miss is given a reason.

use anyhow::Result;
use chrono::{DateTime, Utc};

use super::Db;

/// Reads at or above this share of the reusable tokens count as a hit.
const HIT_PERCENT: u64 = 95;

/// Why a request did or did not reuse what the provider had already seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing was seen on this lane before.
    First,
    /// The provider served the reusable prefix from cache.
    Hit,
    /// The gap since the previous request outlived the provider's cache lifetime.
    Expired,
    /// The system prompt or tool definitions differ from the previous request.
    PrefixChanged,
    /// Nothing explains the miss on dugix's side.
    ProviderMiss,
}

impl Verdict {
    pub const ALL: [Self; 5] = [
        Self::First,
        Self::Hit,
        Self::Expired,
        Self::PrefixChanged,
        Self::ProviderMiss,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Hit => "hit",
            Self::Expired => "expired",
            Self::PrefixChanged => "prefix_changed",
            Self::ProviderMiss => "provider_miss",
        }
    }
}

/// What the previous request on a cache lane looked like.
#[derive(Debug, Clone)]
pub struct LaneState {
    pub prompt_tokens: u64,
    pub system_hash: String,
    pub tools_hash: String,
    pub at: DateTime<Utc>,
}

/// One request's accounting, as the provider reported it.
#[derive(Debug, Clone)]
pub struct RequestFacts {
    pub prompt_tokens: u64,
    pub cache_read_tokens: u64,
    pub system_hash: String,
    pub tools_hash: String,
}

/// The outcome of comparing a request with its lane's previous request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classified {
    /// Tokens the provider had already seen on this lane.
    pub reusable_tokens: u64,
    pub gap_secs: u64,
    pub verdict: Verdict,
}

impl Classified {
    /// Tokens actually served from cache, never more than were reusable.
    #[must_use]
    pub fn reused_tokens(&self, cache_read_tokens: u64) -> u64 {
        cache_read_tokens.min(self.reusable_tokens)
    }
}

/// Classify a request against its lane's previous request.
#[must_use]
pub fn classify(
    previous: Option<&LaneState>,
    now: DateTime<Utc>,
    facts: &RequestFacts,
    lifetime_secs: u64,
) -> Classified {
    let Some(previous) = previous.filter(|p| p.prompt_tokens > 0) else {
        return Classified {
            reusable_tokens: 0,
            gap_secs: 0,
            verdict: Verdict::First,
        };
    };
    let reusable = facts.prompt_tokens.min(previous.prompt_tokens);
    let gap_secs = u64::try_from((now - previous.at).num_seconds()).unwrap_or(0);
    let verdict = if facts.cache_read_tokens * 100 >= reusable * HIT_PERCENT {
        Verdict::Hit
    } else if gap_secs > lifetime_secs {
        Verdict::Expired
    } else if facts.system_hash != previous.system_hash || facts.tools_hash != previous.tools_hash {
        Verdict::PrefixChanged
    } else {
        Verdict::ProviderMiss
    };
    Classified {
        reusable_tokens: reusable,
        gap_secs,
        verdict,
    }
}

impl Db {
    /// Store a request's reuse accounting beside its usage row.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_request_cache(
        &self,
        usage_id: i64,
        session_id: &str,
        cache_key: &str,
        generation: u64,
        classified: &Classified,
        facts: &RequestFacts,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO cache.request_cache
             (usage_id, session_id, cache_key, prefix_generation, reusable_tokens,
              system_hash, tools_hash, gap_secs, verdict)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                usage_id,
                session_id,
                cache_key,
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::try_from(classified.reusable_tokens).unwrap_or(i64::MAX),
                facts.system_hash,
                facts.tools_hash,
                i64::try_from(classified.gap_secs).unwrap_or(i64::MAX),
                classified.verdict.as_str(),
            ],
        )?;
        Ok(())
    }

    /// The latest recorded request on a session's cache lane.
    pub(crate) fn last_lane_request(&self, session_id: &str, cache_key: &str) -> Option<LaneState> {
        self.conn()
            .query_row(
                "SELECT u.prompt_tokens, u.created_at, rc.system_hash, rc.tools_hash
                 FROM cache.request_cache rc JOIN usage_log u ON u.id = rc.usage_id
                 WHERE rc.session_id = ?1 AND rc.cache_key = ?2
                 ORDER BY rc.usage_id DESC LIMIT 1",
                [session_id, cache_key],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .ok()
            .and_then(|(prompt, at, system_hash, tools_hash)| {
                Some(LaneState {
                    prompt_tokens: u64::try_from(prompt).ok()?,
                    system_hash,
                    tools_hash,
                    at: at.parse().ok()?,
                })
            })
    }

    /// Requests per verdict since an RFC 3339 cutoff, in `Verdict::ALL` order.
    pub fn cache_verdict_counts(&self, since: Option<&str>) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn().prepare(
            "SELECT rc.verdict, COUNT(*)
             FROM cache.request_cache rc JOIN usage_log u ON u.id = rc.usage_id
             WHERE (?1 IS NULL OR u.created_at >= ?1)
             GROUP BY rc.verdict",
        )?;
        let found = stmt
            .query_map([since], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Verdict::ALL
            .into_iter()
            .filter_map(|verdict| {
                let count = found
                    .iter()
                    .find(|(name, _)| name == verdict.as_str())
                    .map_or(0, |(_, count)| u64::try_from(*count).unwrap_or(0));
                (count > 0).then(|| (verdict.as_str().to_string(), count))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn lane(prompt: u64, at: DateTime<Utc>) -> LaneState {
        LaneState {
            prompt_tokens: prompt,
            system_hash: "s".into(),
            tools_hash: "t".into(),
            at,
        }
    }

    fn facts(prompt: u64, read: u64, system: &str, tools: &str) -> RequestFacts {
        RequestFacts {
            prompt_tokens: prompt,
            cache_read_tokens: read,
            system_hash: system.into(),
            tools_hash: tools.into(),
        }
    }

    fn verdict(previous: Option<&LaneState>, secs_later: i64, facts: &RequestFacts) -> Classified {
        let now = Utc::now();
        let previous = previous.map(|p| LaneState {
            at: now - Duration::seconds(secs_later),
            ..p.clone()
        });
        classify(previous.as_ref(), now, facts, 300)
    }

    #[test]
    fn each_verdict_comes_from_its_own_sequence() {
        let base = lane(10_000, Utc::now());
        let first = verdict(None, 0, &facts(10_000, 0, "s", "t"));
        assert_eq!((first.verdict, first.reusable_tokens), (Verdict::First, 0));
        let empty = lane(0, Utc::now());
        assert_eq!(
            verdict(Some(&empty), 1, &facts(10_000, 0, "s", "t")).verdict,
            Verdict::First
        );

        let hit = verdict(Some(&base), 10, &facts(12_000, 9_500, "s", "t"));
        assert_eq!((hit.verdict, hit.reusable_tokens), (Verdict::Hit, 10_000));
        assert_eq!(hit.reused_tokens(9_500), 9_500);
        assert_eq!(hit.reused_tokens(12_000), 10_000);

        let expired = verdict(Some(&base), 301, &facts(12_000, 0, "s", "t"));
        assert_eq!(expired.verdict, Verdict::Expired);
        assert_eq!(expired.gap_secs, 301);

        assert_eq!(
            verdict(Some(&base), 10, &facts(12_000, 0, "other", "t")).verdict,
            Verdict::PrefixChanged
        );
        assert_eq!(
            verdict(Some(&base), 10, &facts(12_000, 0, "s", "other")).verdict,
            Verdict::PrefixChanged
        );
        assert_eq!(
            verdict(Some(&base), 10, &facts(12_000, 4_000, "s", "t")).verdict,
            Verdict::ProviderMiss
        );
    }

    #[test]
    fn reusable_never_exceeds_the_smaller_prompt() {
        let base = lane(10_000, Utc::now());
        let shrunk = verdict(Some(&base), 10, &facts(6_000, 6_000, "s", "t"));
        assert_eq!(
            (shrunk.verdict, shrunk.reusable_tokens),
            (Verdict::Hit, 6_000)
        );
    }

    #[test]
    fn rows_round_trip_and_verdicts_count() {
        let db = Db::open_in_memory().unwrap();
        let mut ids = Vec::new();
        for (prompt, read) in [(1_000, 0), (1_200, 1_000), (1_300, 100)] {
            ids.push(
                db.log_usage(
                    "OpenAI",
                    "gpt-5.5",
                    prompt,
                    10,
                    read,
                    0,
                    crate::provider::PromptConvention::Inclusive,
                    None,
                    false,
                    Some("s1"),
                    Some("space"),
                )
                .unwrap(),
            );
        }
        let now = Utc::now();
        let mut previous: Option<LaneState> = None;
        for (id, (prompt, read)) in ids.iter().zip([(1_000, 0), (1_200, 1_000), (1_300, 100)]) {
            let request = facts(prompt, read, "s", "t");
            let classified = classify(previous.as_ref(), now, &request, 300);
            db.record_request_cache(*id, "s1", "s1:0", 0, &classified, &request)
                .unwrap();
            previous = db.last_lane_request("s1", "s1:0");
            assert_eq!(previous.as_ref().unwrap().prompt_tokens, prompt);
        }
        assert!(db.last_lane_request("s1", "s1:1").is_none());
        assert_eq!(
            db.cache_verdict_counts(None).unwrap(),
            [
                ("first".to_string(), 1),
                ("hit".to_string(), 1),
                ("provider_miss".to_string(), 1)
            ]
        );
        let future = (now + Duration::hours(1)).to_rfc3339();
        assert!(db.cache_verdict_counts(Some(&future)).unwrap().is_empty());
    }
}
