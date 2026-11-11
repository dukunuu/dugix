//! `nexus eval` — golden tasks against a real backend.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Result, bail};

use nexus_core::app;
use nexus_core::config;
use nexus_core::eval;

use super::EvalSetting;
use super::ask::build_app;
use super::out;

pub(super) fn parse_eval_setting(text: &str) -> Result<EvalSetting, String> {
    let (key, value) = text
        .split_once('=')
        .ok_or("expected experiments.FIELD=VALUE")?;
    let key = key
        .strip_prefix("experiments.")
        .ok_or("only experiments.FIELD overrides are supported")?;
    config::Experiments::default()
        .set(key, value)
        .map_err(|error| error.to_string())?;
    Ok(EvalSetting {
        key: key.to_string(),
        value: value.to_string(),
    })
}

pub(super) fn apply_eval_settings(
    experiments: &mut config::Experiments,
    settings: &[EvalSetting],
) -> Result<()> {
    for setting in settings {
        experiments.set(&setting.key, &setting.value)?;
    }
    Ok(())
}

pub(super) fn experiments_header(experiments: &config::Experiments) -> String {
    let fields = serde_json::to_value(experiments).expect("experiment fields serialize");
    let settings: Vec<_> = fields
        .as_object()
        .expect("experiment fields")
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    format!("experiments: {}", settings.join(", "))
}

#[derive(Default, serde::Serialize)]
pub(super) struct EvalTaskResult {
    pub(super) name: String,
    pub(super) passed: bool,
    pub(super) failures: Vec<String>,
    pub(super) tool_calls: usize,
    pub(super) invalid_tool_calls: usize,
    pub(super) prompt_tokens: u64,
    pub(super) completion_tokens: u64,
    pub(super) cache_read_tokens: u64,
    pub(super) cost: Option<f64>,
}

async fn run_eval_task(
    task: &eval::EvalTask,
    model: Option<&str>,
    settings: &[EvalSetting],
) -> EvalTaskResult {
    let mut result = EvalTaskResult {
        name: task.name.clone(),
        cost: Some(0.0),
        ..Default::default()
    };
    let outcome: Result<()> = async {
        let mut app = build_app(model, None).await?;
        apply_eval_settings(&mut app.experiments, settings)?;
        app.execute(app::AppCommand::Incognito { on: true })?;
        let workspace = task
            .workspace
            .as_deref()
            .map(eval::EvalWorkspace::copy_from)
            .transpose()?;
        if let Some(workspace) = &workspace {
            app.eval_approver = Some(std::sync::Arc::new(app::EvalApprover {
                allow_shell: task.allow_shell.clone(),
            }));
            app.enter_eval_workspace(&workspace.root())?;
        }
        let mut observed = eval::Observed {
            answer: String::new(),
            calls: Vec::new(),
        };
        for prompt in task.prompts() {
            let outcome = app
                .ask_headless(
                    prompt.clone(),
                    app::headless::TurnOpts {
                        stream: false,
                        quiet: true,
                    },
                )
                .await;
            let start = app
                .messages
                .iter()
                .rposition(|message| message.role == "user")
                .map_or(app.messages.len(), |index| index + 1);
            let turn = &app.messages[start..];
            let current = eval::observed_from_messages(
                outcome
                    .as_ref()
                    .map_or("", |outcome| outcome.answer.as_str()),
                turn,
            );
            observed.answer = current.answer;
            observed.calls.extend(current.calls);
            result.tool_calls = observed.calls.len();
            result.invalid_tool_calls = observed
                .calls
                .iter()
                .filter(|call| {
                    matches!(
                        nexus_core::tools::classify_tool_result(&call.result, ""),
                        Some("invalid_args" | "unknown_tool")
                    )
                })
                .count();
            if let Some(call) = observed.calls.iter().find(|call| {
                matches!(call.name.as_str(), "shell" | "shell_run" | "batch")
                    && call.result.contains("shell write confinement unavailable")
            }) {
                bail!("{}", call.result);
            }
            let outcome = outcome?;
            let usage = outcome.usage.unwrap_or_default();
            result.prompt_tokens += usage.prompt_tokens;
            result.completion_tokens += usage.completion_tokens;
            result.cache_read_tokens += usage.cache_read_tokens;
            let cost = usage.cost.or_else(|| {
                turn.iter()
                    .rev()
                    .find(|message| message.role == "assistant")
                    .and_then(|message| message.cost)
            });
            result.cost = result.cost.zip(cost).map(|(total, cost)| total + cost);
        }
        let root = workspace.as_ref().map(eval::EvalWorkspace::root);
        result.failures = eval::check_workspace(task, &observed, root.as_deref());
        Ok(())
    }
    .await;
    if let Err(error) = outcome {
        result.cost = None;
        result.failures.push(format!("{error:#}"));
    }
    result.passed = result.failures.is_empty();
    result
}

fn eval_cost(cost: Option<f64>) -> String {
    cost.map_or_else(|| "$unknown".to_string(), |cost| format!("${cost:.6}"))
}

fn eval_task_cells(result: &EvalTaskResult) -> String {
    format!(
        "{} | {} | {} | {} | {}",
        if result.passed { "PASS" } else { "FAIL" },
        result.tool_calls,
        result.prompt_tokens,
        result.cache_read_tokens,
        eval_cost(result.cost)
    )
}

fn print_eval_task(result: &EvalTaskResult) {
    out(format!(
        "{} {} · calls {} · invalid {} · tokens {}/{} · cached {} · {}",
        if result.passed { "PASS" } else { "FAIL" },
        result.name,
        result.tool_calls,
        result.invalid_tool_calls,
        result.prompt_tokens,
        result.completion_tokens,
        result.cache_read_tokens,
        eval_cost(result.cost)
    ));
    print_eval_failures(result, "");
}

fn print_eval_failures(result: &EvalTaskResult, side: &str) {
    for failure in &result.failures {
        for line in failure.lines() {
            out(format!("  {side}{}: {line}", result.name));
        }
    }
}

#[derive(serde::Serialize)]
pub(super) struct EvalTotals {
    passed: usize,
    total: usize,
    tool_calls: usize,
    invalid_tool_calls: usize,
    invalid_call_rate: f64,
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_read_tokens: u64,
    pub(super) cost: Option<f64>,
}

pub(super) fn eval_totals(results: &[EvalTaskResult]) -> EvalTotals {
    let tool_calls = results.iter().map(|result| result.tool_calls).sum();
    let invalid_tool_calls = results.iter().map(|result| result.invalid_tool_calls).sum();
    EvalTotals {
        passed: results.iter().filter(|result| result.passed).count(),
        total: results.len(),
        tool_calls,
        invalid_tool_calls,
        invalid_call_rate: if tool_calls == 0 {
            0.0
        } else {
            invalid_tool_calls as f64 / tool_calls as f64
        },
        prompt_tokens: results.iter().map(|result| result.prompt_tokens).sum(),
        completion_tokens: results.iter().map(|result| result.completion_tokens).sum(),
        cache_read_tokens: results.iter().map(|result| result.cache_read_tokens).sum(),
        cost: results.iter().map(|result| result.cost).sum(),
    }
}

fn eval_totals_line(totals: &EvalTotals) -> String {
    format!(
        "passed {}/{} · calls {} · invalid-call rate {:.2}% ({}/{}) · tokens {}/{} · cached {} · {}",
        totals.passed,
        totals.total,
        totals.tool_calls,
        totals.invalid_call_rate * 100.0,
        totals.invalid_tool_calls,
        totals.tool_calls,
        totals.prompt_tokens,
        totals.completion_tokens,
        totals.cache_read_tokens,
        eval_cost(totals.cost)
    )
}

#[derive(serde::Serialize)]
pub(super) struct EvalDeltas {
    pub(super) prompt_tokens: i128,
    pub(super) prompt_tokens_percent: Option<f64>,
    pub(super) cost: Option<f64>,
    pub(super) cost_percent: Option<f64>,
    pub(super) passed_before: usize,
    pub(super) passed_after: usize,
}

fn percent_delta(before: f64, after: f64) -> Option<f64> {
    (before != 0.0).then(|| (after - before) / before * 100.0)
}

pub(super) fn eval_deltas(baseline: &EvalTotals, variant: &EvalTotals) -> EvalDeltas {
    EvalDeltas {
        prompt_tokens: i128::from(variant.prompt_tokens) - i128::from(baseline.prompt_tokens),
        prompt_tokens_percent: percent_delta(
            baseline.prompt_tokens as f64,
            variant.prompt_tokens as f64,
        ),
        cost: baseline
            .cost
            .zip(variant.cost)
            .map(|(before, after)| after - before),
        cost_percent: baseline
            .cost
            .zip(variant.cost)
            .and_then(|(before, after)| percent_delta(before, after)),
        passed_before: baseline.passed,
        passed_after: variant.passed,
    }
}

pub(super) fn format_eval_compare(
    baseline: &[EvalTaskResult],
    variant: &[EvalTaskResult],
) -> String {
    format_eval_compare_labeled(baseline, variant, ("baseline", "variant"))
}

pub(super) fn format_eval_compare_labeled(
    baseline: &[EvalTaskResult],
    variant: &[EvalTaskResult],
    (baseline_label, variant_label): (&str, &str),
) -> String {
    let mut text = format!(
        "task | {baseline_label}: pass | calls | prompt | cached | cost | {variant_label}: pass | calls | prompt | cached | cost\n",
    );
    for (before, after) in baseline.iter().zip(variant) {
        let _ = writeln!(
            text,
            "{} | {} | {}",
            before.name,
            eval_task_cells(before),
            eval_task_cells(after)
        );
    }
    let before = eval_totals(baseline);
    let after = eval_totals(variant);
    let delta = eval_deltas(&before, &after);
    let percent = |value: Option<f64>| {
        value.map_or_else(
            || "unknown".to_string(),
            |value| format!("{value:+.0}%").replace('-', "−"),
        )
    };
    let _ = writeln!(
        text,
        "{baseline_label} totals: {}",
        eval_totals_line(&before)
    );
    let _ = writeln!(text, "{variant_label} totals: {}", eval_totals_line(&after));
    let _ = write!(
        text,
        "tokens {} · cost {} · passed {}/{} → {}/{}",
        percent(delta.prompt_tokens_percent),
        percent(delta.cost_percent),
        before.passed,
        before.total,
        after.passed,
        after.total
    );
    text
}

#[derive(serde::Serialize)]
pub(super) struct EvalRun {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) model: Option<String>,
    pub(super) experiments: config::Experiments,
    pub(super) totals: EvalTotals,
    pub(super) tasks: Vec<EvalTaskResult>,
}

async fn eval_run(
    tasks: &[eval::EvalTask],
    model: Option<&str>,
    settings: &[EvalSetting],
    json: bool,
    compare: bool,
) -> Result<EvalRun> {
    let mut experiments = config::load_creds_offline().experiments;
    apply_eval_settings(&mut experiments, settings)?;
    if !json {
        out(experiments_header(&experiments));
    }
    let mut results = Vec::new();
    for task in tasks {
        let result = run_eval_task(task, model, settings).await;
        if !json && !compare {
            print_eval_task(&result);
        }
        results.push(result);
    }
    Ok(EvalRun {
        model: model.map(str::to_owned),
        experiments,
        totals: eval_totals(&results),
        tasks: results,
    })
}

pub(super) async fn eval(
    dir: &Path,
    model: Option<&str>,
    filter: Option<&str>,
    settings: &[EvalSetting],
    compare: bool,
    compare_model: &[String],
    json: bool,
) -> Result<()> {
    let mut tasks = eval::load_tasks(dir)?;
    if let Some(filter) = filter {
        tasks.retain(|task| task.name.contains(filter));
    }
    if tasks.is_empty() {
        bail!("no eval tasks matched in {}", dir.display());
    }
    let labels = match compare_model {
        [before, after] => Some((before.as_str(), after.as_str())),
        _ => None,
    };
    let comparing = compare || labels.is_some();
    let (baseline_model, variant_model) = labels.map_or((model, model), |(before, after)| {
        (Some(before), Some(after))
    });
    let baseline = if comparing {
        let baseline_settings = if compare { &[] } else { settings };
        Some(eval_run(&tasks, baseline_model, baseline_settings, json, comparing).await?)
    } else {
        None
    };
    let variant = eval_run(&tasks, variant_model, settings, json, comparing).await?;
    let mut failed = variant.totals.total - variant.totals.passed;
    let output = if let Some(baseline) = baseline {
        failed += baseline.totals.total - baseline.totals.passed;
        if !json {
            out(labels.map_or_else(
                || format_eval_compare(&baseline.tasks, &variant.tasks),
                |labels| format_eval_compare_labeled(&baseline.tasks, &variant.tasks, labels),
            ));
            let (before, after) = labels.unwrap_or(("baseline", "variant"));
            for (side, results) in [(before, &baseline.tasks), (after, &variant.tasks)] {
                let side = format!("{side} ");
                for result in results {
                    print_eval_failures(result, &side);
                }
            }
        }
        serde_json::json!({ "baseline": baseline, "variant": variant, "deltas": eval_deltas(&baseline.totals, &variant.totals) })
    } else {
        if !json {
            out(eval_totals_line(&variant.totals));
        }
        serde_json::to_value(variant)?
    };
    if json {
        out(serde_json::to_string_pretty(&output)?);
    }
    if failed != 0 {
        bail!("{failed} eval task run(s) failed");
    }
    Ok(())
}
