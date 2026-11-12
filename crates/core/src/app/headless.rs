//! Headless chat and research drivers with answers on stdout and progress on stderr.

use std::io::Write as _;

use anyhow::{Context as _, Result, anyhow, bail};

use crate::db::stage_content;
use crate::provider::{StreamEvent, Usage};

use super::{App, AppCommand, AppEvent};

#[derive(Default, Clone, Copy)]
pub struct TurnOpts {
    pub stream: bool,
    pub quiet: bool,
}

pub struct AskOutcome {
    pub answer: String,
    pub session_id: String,
    pub session_title: String,
    pub usage: Option<Usage>,
}

pub struct ResearchOutcome {
    pub report: String,
    pub session_id: String,
    pub session_title: String,
}

// A closed downstream pipe is normal for callers such as `head`.
fn stream(s: &str) {
    let mut stdout = std::io::stdout();
    if stdout.write_all(s.as_bytes()).is_err() {
        std::process::exit(0);
    }
}

fn note(quiet: bool, line: impl std::fmt::Display) {
    if !quiet {
        eprintln!("{line}");
    }
}

fn add_usage(total: Option<Usage>, next: Usage) -> Usage {
    let total = total.unwrap_or_default();
    Usage {
        prompt_tokens: total.prompt_tokens + next.prompt_tokens,
        completion_tokens: total.completion_tokens + next.completion_tokens,
        total_tokens: total.total_tokens + next.total_tokens,
        cache_read_tokens: total.cache_read_tokens + next.cache_read_tokens,
        cache_creation_tokens: total.cache_creation_tokens + next.cache_creation_tokens,
        prompt_convention: next.prompt_convention,
        cost: match (total.cost, next.cost) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        },
    }
}

impl App {
    pub fn switch_space_cli(&mut self, name: &str) -> Result<()> {
        let row = self
            .db
            .list_spaces()
            .context("listing spaces")?
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| anyhow!("no space named {name:?} — `nexus spaces` lists them"))?;
        if row.id != self.active_space.id {
            self.set_active_space(row);
        }
        Ok(())
    }

    /// Drive a non-interactive chat turn to completion.
    pub async fn run_turn(
        &mut self,
        prompt: String,
        opts: TurnOpts,
    ) -> Result<(String, Option<Usage>)> {
        if !self.backends.any() {
            bail!(
                "no API key configured — set one with /login in the TUI, or export \
                 $OPENROUTER_API_KEY / $OPENAI_API_KEY / $OPENCODE_API_KEY"
            );
        }
        if self.current_model.is_none() {
            bail!("no model selected — pass --model, or pick one in the TUI first");
        }
        self.execute(AppCommand::Send { text: prompt })?;

        let mut usage: Option<Usage> = None;
        let mut last_status = String::new();
        let mut streamed_any = false;
        let mut thought = false;
        loop {
            match self.next_event().await {
                AppEvent::Stream(Some((task_id, ev))) => {
                    match &ev {
                        StreamEvent::Usage(u) => usage = Some(add_usage(usage, *u)),
                        StreamEvent::Token(t) => {
                            streamed_any = true;
                            if opts.stream {
                                stream(t);
                            }
                        }
                        StreamEvent::Reasoning(_) if !thought => {
                            thought = true;
                            note(opts.quiet, "…thinking…");
                        }
                        StreamEvent::Status(s) if *s != last_status => {
                            note(opts.quiet, s);
                            last_status.clone_from(s);
                        }
                        _ => {}
                    }
                    let parked = matches!(&ev, StreamEvent::ToolCall { result, .. } if result == crate::jobs::PARKED);
                    self.on_chat_event(task_id, ev)?;
                    if parked && self.approvals.job_approver.is_some() {
                        self.stop_stream()?;
                        return Ok((crate::jobs::PARKED.to_string(), usage));
                    }
                    if self.chat_tasks.is_empty() {
                        break;
                    }
                }
                AppEvent::Stream(None) => break,
                AppEvent::Title(t) => self.on_title_result(t),
                _ => {}
            }
        }
        if streamed_any && opts.stream {
            stream("\n");
        }

        let mut answer = None;
        for m in self.messages.iter().rev() {
            match m.role.as_str() {
                "assistant" if !m.content.is_empty() => {
                    answer = Some(m.content.clone());
                    break;
                }
                "error" => bail!("{}", m.content),
                _ => {}
            }
        }
        let Some(answer) = answer else {
            bail!("response finished without text");
        };
        if let Some(u) = usage {
            let cost = u.cost.map(|c| format!(" · ${c:.4}")).unwrap_or_default();
            note(
                opts.quiet,
                format!(
                    "tokens: {} → {} ({} cached){}",
                    u.prompt_tokens, u.completion_tokens, u.cache_read_tokens, cost
                ),
            );
        }
        Ok((answer, usage))
    }

    /// Run one turn with a bounded wait for its generated session title.
    pub async fn ask_headless(&mut self, prompt: String, opts: TurnOpts) -> Result<AskOutcome> {
        let result = self.run_turn(prompt, opts).await;
        std::mem::take(&mut self.procs).shutdown().await;
        self.refresh_toolbox();
        let (answer, usage) = result?;

        if self.title_rx.is_some() {
            let timeout = tokio::time::sleep(std::time::Duration::from_secs(15));
            tokio::pin!(timeout);
            loop {
                tokio::select! {
                    () = &mut timeout => break,
                    ev = self.next_event() => {
                        if let AppEvent::Title(t) = ev {
                            self.on_title_result(t);
                            break;
                        }
                    }
                }
            }
        }

        let session = self.session.clone().context("session vanished after ask")?;
        Ok(AskOutcome {
            answer,
            session_id: session.id,
            session_title: session.title,
            usage,
        })
    }

    /// Run a line-oriented chat REPL in one session.
    pub async fn chat_headless(&mut self, quiet: bool) -> Result<()> {
        if !self.backends.any() {
            bail!(
                "no API key configured — set one with /login in the TUI, or export \
                 $OPENROUTER_API_KEY / $OPENAI_API_KEY / $OPENCODE_API_KEY"
            );
        }
        if self.current_model.is_none() {
            bail!("no model selected — pass --model, or pick one in the TUI first");
        }
        loop {
            eprint!("> ");
            let _ = std::io::stderr().flush();
            let mut line = String::new();
            if std::io::stdin().read_line(&mut line)? == 0 {
                eprintln!();
                break;
            }
            let text = line.trim().to_string();
            if text.is_empty() {
                continue;
            }
            if matches!(text.as_str(), "/quit" | "/exit" | "/q") {
                break;
            }
            self.run_turn(
                text,
                TurnOpts {
                    stream: true,
                    quiet,
                },
            )
            .await?;
        }
        self.procs.shutdown().await;
        Ok(())
    }

    /// Run research headlessly, requiring terminal input unless approval is automatic.
    pub async fn research_headless(
        &mut self,
        topic: String,
        approve: bool,
        opts: TurnOpts,
    ) -> Result<ResearchOutcome> {
        if !self.backends.any() {
            bail!(
                "no API key configured — set one with /login in the TUI, or export \
                 $OPENROUTER_API_KEY / $OPENAI_API_KEY / $OPENCODE_API_KEY"
            );
        }
        if self.current_model.is_none() {
            bail!("no model selected — pass --model, or pick one in the TUI first");
        }
        let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin());
        self.execute(AppCommand::RunResearch {
            topic,
            gated: !approve,
        })?;
        let mut refusal = String::new();
        while let Some(ev) = self.pop_pending_event() {
            if let AppEvent::Status(s) = ev {
                refusal = s;
            }
        }
        if self.research.rx.is_none() {
            bail!("research didn't start: {refusal}");
        }

        let mut report: Option<String> = None;
        let mut error: Option<String> = None;
        loop {
            match self.next_event().await {
                AppEvent::Research(Some((session_id, space_id, space_name, update))) => {
                    if let super::research::ResearchUpdate::Stage { label, detail } = &update {
                        note(opts.quiet, stage_content(label, detail));
                    }
                    match &update {
                        super::research::ResearchUpdate::Done(Ok(text)) => {
                            report = Some(text.clone());
                        }
                        super::research::ResearchUpdate::Done(Err(e)) => error = Some(e.clone()),
                        _ => {}
                    }
                    self.on_research_done(Some((session_id, space_id, space_name, update)));
                    // The pipeline cannot emit further progress until this gate is answered.
                    if !approve && let Some(gate) = self.research.survey_gate.as_ref() {
                        if !interactive {
                            bail!(
                                "research needs your input at a gate ({}), but stdin isn't a \
                                 terminal — re-run with --approve to skip the gates",
                                match &gate.phase {
                                    super::SurveyPhase::Clarify { .. } => "survey questions",
                                    super::SurveyPhase::Approve { .. } => "plan approval",
                                }
                            );
                        }
                        stream(&format!("\n{}\n", gate.prompt_content));
                        eprint!("> ");
                        let _ = std::io::stderr().flush();
                        let mut reply = String::new();
                        if std::io::stdin().read_line(&mut reply)? == 0 {
                            bail!("research needs your input — stdin closed");
                        }
                        self.execute(AppCommand::AnswerGate { text: reply })?;
                    }
                }
                AppEvent::Research(None) => break,
                _ => {}
            }
        }

        let session = self.session.clone().context("research session vanished")?;
        let Some(report) = report else {
            bail!(
                "{}",
                error.unwrap_or_else(|| "research finished without a report".to_string())
            );
        };
        Ok(ResearchOutcome {
            report,
            session_id: session.id,
            session_title: session.title,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::space::Space;

    fn test_app() -> App {
        let root = std::env::temp_dir().join(format!("nexus-cli-{}", uuid::Uuid::new_v4()));
        App::new(Db::open_in_memory().unwrap(), Some("k"), Space { root })
    }

    #[test]
    fn add_usage_sums_requests() {
        use crate::provider::PromptConvention;

        let first = Usage {
            prompt_tokens: 10,
            completion_tokens: 2,
            total_tokens: 12,
            cache_read_tokens: 3,
            cache_creation_tokens: 1,
            prompt_convention: PromptConvention::Inclusive,
            cost: Some(0.01),
        };
        let second = Usage {
            prompt_tokens: 30,
            completion_tokens: 5,
            total_tokens: 35,
            cache_read_tokens: 11,
            cache_creation_tokens: 4,
            prompt_convention: PromptConvention::NormalizedFromExclusive,
            cost: None,
        };
        let total = add_usage(Some(add_usage(None, first)), second);
        assert_eq!(total.prompt_tokens, 40);
        assert_eq!(total.completion_tokens, 7);
        assert_eq!(total.total_tokens, 47);
        assert_eq!(total.cache_read_tokens, 14);
        assert_eq!(total.cache_creation_tokens, 5);
        assert_eq!(
            total.prompt_convention,
            PromptConvention::NormalizedFromExclusive
        );
        assert_eq!(total.cost, Some(0.01));

        let unpriced = Usage {
            cost: None,
            ..first
        };
        assert_eq!(add_usage(Some(unpriced), second).cost, None);
    }

    #[test]
    fn add_usage_sums_known_costs_after_unknown_cost() {
        let priced = Usage {
            cost: Some(0.01),
            ..Usage::default()
        };
        let total = add_usage(Some(add_usage(None, Usage::default())), priced);
        assert_eq!(total.cost, Some(0.01));
        let total = add_usage(Some(total), priced);
        assert!((total.cost.unwrap() - 0.02).abs() < f64::EPSILON);
    }

    #[test]
    fn switch_space_by_name_switches_and_is_idempotent() {
        let mut app = test_app();
        let row = app.db.create_space("research").unwrap();
        let active_before = app.active_space.id.clone();
        assert_ne!(row.id, active_before);

        app.switch_space_cli("research").unwrap();
        assert_eq!(app.active_space.id, row.id);
        app.switch_space_cli("research").unwrap();
        assert_eq!(app.active_space.id, row.id);
    }

    #[test]
    fn switch_space_unknown_name_bails() {
        let mut app = test_app();
        assert!(app.switch_space_cli("nope").is_err());
    }
}
