//! Reuse accounting for chat requests: what share of the tokens a provider had
//! already seen on a session's cache lane it served from cache, and why not.

use chrono::Utc;

use super::App;
use crate::db::{Classified, LaneState, RequestFacts, classify};
use crate::provider::{BackendTag, Usage};

/// The prefix a chat turn was sent with, noted when the turn starts.
#[derive(Clone)]
pub(super) struct TurnLane {
    pub cache_key: String,
    pub generation: u64,
    pub system_hash: String,
    pub tools_hash: String,
}

/// One request's classification, ready to be stored beside its usage row.
pub(super) struct AccountedRequest {
    pub session_id: String,
    pub lane: TurnLane,
    pub facts: RequestFacts,
    pub classified: Classified,
}

impl App {
    /// Remember which prefix and cache lane the active session's turn uses.
    pub(super) fn note_turn_lane(&mut self) {
        let Some(session_id) = self.session.as_ref().map(|s| s.id.clone()) else {
            return;
        };
        let Some(prefix) = self.active_prefix() else {
            return;
        };
        let lane = TurnLane {
            generation: prefix.generation,
            system_hash: prefix.system_hash(),
            tools_hash: prefix.tools_hash(),
            cache_key: self.prompt_cache_key_for(&session_id),
        };
        self.turn_lanes.insert(session_id, lane);
    }

    /// Classify one finished request against its lane, update the lane and the
    /// turn's reuse tally. `None` when the request has no usable cache accounting.
    pub(super) fn account_request(
        &mut self,
        session_id: &str,
        backend: BackendTag,
        model_id: &str,
        incognito: bool,
        usage: &Usage,
    ) -> Option<AccountedRequest> {
        if !usage.prompt_convention.ratio_is_meaningful() {
            return None;
        }
        let lane = self.turn_lanes.get(session_id)?.clone();
        let facts = RequestFacts {
            prompt_tokens: usage.prompt_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            system_hash: lane.system_hash.clone(),
            tools_hash: lane.tools_hash.clone(),
        };
        let previous = self.cache_lanes.get(&lane.cache_key).cloned().or_else(|| {
            (!incognito)
                .then(|| self.db.last_lane_request(session_id, &lane.cache_key))
                .flatten()
        });
        let now = Utc::now();
        let lifetime = crate::provider::cache_policy::lifetime_secs(
            backend,
            model_id,
            self.settings.cache_lifetime == "extended",
            self.settings.cache_long_openai,
        );
        let classified = classify(previous.as_ref(), now, &facts, lifetime);
        self.cache_lanes.insert(
            lane.cache_key.clone(),
            LaneState {
                prompt_tokens: facts.prompt_tokens,
                system_hash: facts.system_hash.clone(),
                tools_hash: facts.tools_hash.clone(),
                at: now,
            },
        );
        self.meter.turn_cache.observe_reuse(
            classified.reusable_tokens,
            classified.reused_tokens(facts.cache_read_tokens),
        );
        Some(AccountedRequest {
            session_id: session_id.to_string(),
            lane,
            facts,
            classified,
        })
    }

    pub(super) fn store_accounted_request(&self, usage_id: i64, request: &AccountedRequest) {
        let _ = self.db.record_request_cache(
            usage_id,
            &request.session_id,
            &request.lane.cache_key,
            request.lane.generation,
            &request.classified,
            &request.facts,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::app_with_key;
    use crate::app::{ChatTask, SpinnerColor};
    use crate::provider::{PromptConvention, StreamEvent};

    fn task(app: &App) -> ChatTask {
        ChatTask {
            id: 1,
            session_id: app.session.as_ref().unwrap().id.clone(),
            session_title: "t".into(),
            space_id: app.active_space.id.clone(),
            model: "a/one".into(),
            model_id: "gpt-5.5".into(),
            backend: BackendTag::OpenAi,
            incognito: false,
            buffer: String::new(),
            thinking: String::new(),
            tool_status: None,
            usage: None,
            usage_row_id: None,
            started: std::time::Instant::now(),
            thinking_idx: 0,
            spinner_color: SpinnerColor::Green,
            abort: tokio::spawn(async {}).abort_handle(),
        }
    }

    fn usage(prompt: u64, read: u64, convention: PromptConvention) -> StreamEvent {
        StreamEvent::Usage(Usage {
            prompt_tokens: prompt,
            completion_tokens: 5,
            cache_read_tokens: read,
            prompt_convention: convention,
            ..Usage::default()
        })
    }

    #[tokio::test]
    async fn requests_are_scored_against_their_lane_and_stored() {
        let mut app = app_with_key();
        app.current_model = Some("a/one".into());
        app.ensure_session("hello", "a/one".into()).unwrap();
        app.ensure_prompt_prefix();
        app.note_turn_lane();
        let session = app.session.as_ref().unwrap().id.clone();
        app.chat_tasks.insert(1, task(&app));

        app.on_chat_event(1, usage(1_000, 0, PromptConvention::Inclusive))
            .unwrap();
        assert!(
            app.meter.turn_cache.reuse_rate().is_none(),
            "nothing was reusable yet"
        );
        app.on_chat_event(1, usage(1_500, 950, PromptConvention::Inclusive))
            .unwrap();
        let tally = app.meter.turn_cache;
        assert_eq!((tally.reusable_tokens, tally.reused_tokens), (1_000, 950));
        assert!((tally.reuse_rate().unwrap() - 0.95).abs() < 1e-9);
        // Unaccounted usage is neither reusable nor reused.
        app.on_chat_event(1, usage(2_000, 0, PromptConvention::Unknown))
            .unwrap();
        assert_eq!(app.meter.turn_cache.reusable_tokens, 1_000);
        // A refresh moves the session to a new lane, so the next request is a first.
        app.run_command("refresh").unwrap();
        app.ensure_prompt_prefix();
        app.note_turn_lane();
        app.on_chat_event(1, usage(1_700, 0, PromptConvention::Inclusive))
            .unwrap();

        let rows: Vec<(String, i64, i64)> = app
            .db
            .conn()
            .prepare(
                "SELECT cache_key, prefix_generation, reusable_tokens
                 FROM cache.request_cache ORDER BY usage_id",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            [
                (format!("{session}:0"), 0, 0),
                (format!("{session}:0"), 0, 1_000),
                (format!("{session}:1"), 1, 0),
            ]
        );
        assert_eq!(
            app.db.cache_verdict_counts(None).unwrap(),
            [("first".to_string(), 2), ("hit".to_string(), 1)]
        );
    }
}
