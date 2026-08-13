use std::cmp::Ordering;
use std::collections::BTreeMap;

use line_ending::LineEnding;

use crate::trial::metric_value_field;
use crate::trial::store::TrialStatus;
use crate::{
    Goal, TrialStore,
    constants::{FIELD_METRIC, FIELD_SCORE, FIELD_TRIAL_ID, FIELD_TRIAL_STATUS},
};

struct BestTrialSnapshot {
    trial_id: usize,
    metric: String,
    score: f64,
    hparams: Vec<(String, String)>,
}

#[derive(Default, Clone, Copy)]
struct CorrelationStats {
    n: usize,
    mean_x: f64,
    mean_y: f64,
    m2_x: f64,
    m2_y: f64,
    c_xy: f64,
}

impl CorrelationStats {
    fn add(&mut self, x: f64, y: f64) {
        self.n += 1;
        let n = self.n as f64;

        let dx = x - self.mean_x;
        self.mean_x += dx / n;

        let dy = y - self.mean_y;
        self.mean_y += dy / n;

        self.m2_x += dx * (x - self.mean_x);
        self.m2_y += dy * (y - self.mean_y);

        self.c_xy += dx * (y - self.mean_y);
    }

    fn correlation(self) -> Option<f64> {
        if self.n < 2 {
            return None;
        }
        if !self.m2_x.is_finite() || !self.m2_y.is_finite() {
            return None;
        }
        if !self.c_xy.is_finite() {
            return None;
        }

        if self.m2_x <= 0.0 || self.m2_y <= 0.0 {
            return None;
        }

        let denom = (self.m2_x * self.m2_y).sqrt();
        Some(self.c_xy / denom)
    }
}

#[derive(Debug, Clone)]
struct HyperParamImpact {
    name: String,
    numeric_samples: usize,
    total_trials: usize,
    correlation: Option<f64>,
    note: Option<String>,
    range: Option<RangeImpact>,
}

#[derive(Default)]
struct ParamStats {
    stats: CorrelationStats,
    numeric_samples: usize,
    total_trials: usize,
    samples: Vec<(f64, f64)>, // (value, score)
}

#[derive(Debug, Clone)]
struct RangeBinImpact {
    label: String,
    count: usize,
    best_metric: f64,
    median_metric: f64,
}

#[derive(Debug, Clone)]
struct RangeImpact {
    bins: Vec<RangeBinImpact>,
    elbow_label: Option<String>,
}

pub fn print_top_trials(store: &TrialStore, n: usize) {
    let rows = match store.load_rows() {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("failed to load trials for summary: {}", e);
            return;
        }
    };

    // Collect valid completed trials
    let mut trials_vec: Vec<BestTrialSnapshot> = Vec::new();
    for row in rows {
        if row
            .get(FIELD_TRIAL_STATUS)
            .and_then(|s| s.parse::<TrialStatus>().ok())
            != Some(TrialStatus::Ok)
        {
            continue;
        }
        let score = row.get(FIELD_SCORE).and_then(|v| v.parse::<f64>().ok());
        let trial_id = row
            .get(FIELD_TRIAL_ID)
            .and_then(|v| v.parse::<usize>().ok());
        let metric = row
            .get(FIELD_METRIC)
            .cloned()
            .unwrap_or_else(|| "metric".to_string());

        if let (Some(score), Some(trial_id)) = (score, trial_id) {
            // collect hparams as Vec<(k,v)>
            let mut hparams: Vec<(String, String)> = Vec::new();
            for (key, value) in row.iter().filter(|(k, _)| k.starts_with(crate::HP_PREFIX)) {
                let name = key.trim_start_matches(crate::HP_PREFIX).to_string();
                hparams.push((name, value.clone()));
            }
            trials_vec.push(BestTrialSnapshot {
                trial_id,
                metric,
                score,
                hparams,
            });
        }
    }

    // Sort by score (lower is better)
    trials_vec.sort_by(|a, b| {
        a.score
            .partial_cmp(&b.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let top: Vec<&BestTrialSnapshot> = trials_vec.iter().take(n).collect();
    if top.is_empty() {
        println!(
            "{}Top {} trials: none",
            LineEnding::from_current_platform().as_str(),
            n
        );
        return;
    }

    // Print each top trial as a compact block that works on narrow terminals.
    println!(
        "{}Top {} trials:",
        LineEnding::from_current_platform().as_str(),
        top.len()
    );
    for t in top {
        println!(
            "Trial {}  Metric: {}  Score: {:.6}",
            t.trial_id, t.metric, t.score
        );
        if t.hparams.is_empty() {
            println!("  (no hyperparameters)");
        } else {
            println!("  Hyperparameters:");
            for (k, v) in &t.hparams {
                println!("    {:<20} {}", k, v);
            }
        }
        println!();
    }
}

pub fn print_hparam_impact(store: &TrialStore, goal: Goal, metric_key: &str) {
    let rows = match store.load_rows() {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!("failed to load trials for hyperparameter impact: {err}");
            return;
        }
    };
    let impacts = collect_hparam_impacts(&rows, goal, metric_key);
    eprintln!(
        "{}===== Hyperparameter Impact (heuristic) =====",
        LineEnding::from_current_platform().as_str()
    );
    eprintln!(
        "Pearson correlations vs metric; treat this as a heuristic that can be extremely inaccurate with low sample counts."
    );
    let goal_blurb = match goal {
        Goal::Min => "goal=min (lower metric is better)",
        Goal::Max => "goal=max (higher metric is better)",
    };
    eprintln!(
        "Metrics/ordering honor the project goal: {}, metric key='{}'.",
        goal_blurb, metric_key
    );
    if impacts.is_empty() {
        eprintln!("No hyperparameters observed in completed trials yet.");
        return;
    }
    for impact in impacts {
        let display_name = impact
            .name
            .strip_prefix(crate::HP_PREFIX)
            .unwrap_or(&impact.name);
        let corr_display = match impact.correlation {
            Some(value) => format!("{:+.3}", value),
            None => "n/a".to_string(),
        };
        let mut annotations: Vec<String> = Vec::new();
        if impact.numeric_samples > 0 && impact.total_trials > impact.numeric_samples {
            annotations.push("contains non-numeric values (ignored)".to_string());
        }
        if let Some(note) = &impact.note {
            annotations.push(note.clone());
        }
        let annotation = if annotations.is_empty() {
            String::new()
        } else {
            format!(" ({})", annotations.join("; "))
        };
        eprintln!(
            "{:<24} corr={} numeric_samples={} total_trials={}{}",
            display_name, corr_display, impact.numeric_samples, impact.total_trials, annotation,
        );
        if let Some(range) = &impact.range {
            for bin in &range.bins {
                let elbow_marker = range
                    .elbow_label
                    .as_ref()
                    .filter(|label| *label == &bin.label)
                    .map(|_| "  <-- elbow candidate")
                    .unwrap_or("");
                eprintln!(
                    "    {:<24} count={:<3} best_{}={:.6} median_{}={:.6}{}",
                    bin.label,
                    bin.count,
                    metric_key,
                    bin.best_metric,
                    metric_key,
                    bin.median_metric,
                    elbow_marker
                );
            }
        }
    }
}

fn collect_hparam_impacts(
    rows: &[BTreeMap<String, String>],
    goal: Goal,
    metric_key: &str,
) -> Vec<HyperParamImpact> {
    let mut stats: BTreeMap<String, ParamStats> = BTreeMap::new();
    let metric_field = metric_value_field(metric_key);
    for row in rows {
        if row
            .get(FIELD_TRIAL_STATUS)
            .and_then(|s| s.parse::<TrialStatus>().ok())
            != Some(TrialStatus::Ok)
        {
            continue;
        }
        let metric = row
            .get(&metric_field)
            .and_then(|v| v.parse::<f64>().ok())
            .or_else(|| row.get(FIELD_SCORE).and_then(|v| v.parse::<f64>().ok()));
        let Some(metric) = metric else {
            continue;
        };
        if !metric.is_finite() {
            continue;
        }
        for (key, value) in row {
            if !key.starts_with(crate::HP_PREFIX) {
                continue;
            }
            let entry = stats.entry(key.clone()).or_default();
            entry.total_trials += 1;
            if let Ok(val) = value.parse::<f64>() {
                if !val.is_finite() {
                    continue;
                }
                entry.numeric_samples += 1;
                entry.stats.add(val, metric);
                entry.samples.push((val, metric));
            }
        }
    }
    let mut impacts = stats
        .into_iter()
        .map(|(name, stat)| {
            let (correlation, note) = if stat.numeric_samples == 0 {
                (
                    None,
                    Some("non-numeric values; correlation skipped".to_string()),
                )
            } else if stat.numeric_samples < 2 {
                (None, Some("need >=2 numeric samples".to_string()))
            } else if let Some(value) = stat.stats.correlation() {
                (Some(value), None)
            } else {
                (None, Some("no variance across numeric samples".to_string()))
            };
            let range = compute_range_impact(&stat.samples, goal);
            HyperParamImpact {
                name,
                numeric_samples: stat.numeric_samples,
                total_trials: stat.total_trials,
                correlation,
                note,
                range,
            }
        })
        .collect::<Vec<_>>();
    impacts.sort_by(|a, b| {
        match (a.correlation, b.correlation) {
            (Some(ac), Some(bc)) => bc.abs().partial_cmp(&ac.abs()).unwrap_or(Ordering::Equal),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => b.numeric_samples.cmp(&a.numeric_samples),
        }
        .then_with(|| a.name.cmp(&b.name))
    });
    impacts
}

fn compute_range_impact(samples: &[(f64, f64)], goal: Goal) -> Option<RangeImpact> {
    // Need at least 3 samples to make an elbow guess meaningful.
    if samples.len() < 3 {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));

    let target_bins = 5usize;
    let bin_count = target_bins.min(sorted.len());
    let chunk_size = (sorted.len() as f64 / bin_count as f64).ceil() as usize;

    let mut bins: Vec<(f64, RangeBinImpact)> = Vec::new();
    for chunk in sorted.chunks(chunk_size) {
        if chunk.is_empty() {
            continue;
        }
        let min_v = chunk.first().unwrap().0;
        let max_v = chunk.last().unwrap().0;
        let label = format_range_label(min_v, max_v);
        let mut metrics: Vec<f64> = chunk.iter().map(|(_, metric)| *metric).collect();
        metrics.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        let best_metric = *metrics
            .iter()
            .reduce(|best, next| {
                if is_better(goal, *next, *best) {
                    next
                } else {
                    best
                }
            })
            .unwrap();
        let median_metric = median_from_sorted(&metrics);
        let center = (min_v + max_v) / 2.0;
        bins.push((
            center,
            RangeBinImpact {
                label,
                count: chunk.len(),
                best_metric,
                median_metric,
            },
        ));
    }
    bins.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
    let ordered_bins: Vec<RangeBinImpact> = bins.into_iter().map(|(_, bin)| bin).collect();
    let elbow_bin = detect_elbow(&ordered_bins, goal);
    let elbow_label = elbow_bin.and_then(|idx| ordered_bins.get(idx).map(|bin| bin.label.clone()));

    // Re-sort bins for display by best metric per goal, then median_metric, then label.
    let mut display_bins = ordered_bins.clone();
    display_bins.sort_by(|a, b| {
        match goal {
            Goal::Min => a
                .best_metric
                .partial_cmp(&b.best_metric)
                .unwrap_or(Ordering::Equal),
            Goal::Max => b
                .best_metric
                .partial_cmp(&a.best_metric)
                .unwrap_or(Ordering::Equal),
        }
        .then_with(|| {
            a.median_metric
                .partial_cmp(&b.median_metric)
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| a.label.cmp(&b.label))
    });

    Some(RangeImpact {
        bins: display_bins,
        elbow_label,
    })
}

fn median_from_sorted(values: &[f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mid = values.len() / 2;
    if values.len() % 2 == 1 {
        values[mid]
    } else {
        (values[mid - 1] + values[mid]) / 2.0
    }
}

fn is_better(goal: Goal, candidate: f64, current: f64) -> bool {
    match goal {
        Goal::Min => candidate < current,
        Goal::Max => candidate > current,
    }
}

fn improvement(goal: Goal, previous_best: f64, next_best: f64) -> f64 {
    match goal {
        Goal::Min => previous_best - next_best,
        Goal::Max => next_best - previous_best,
    }
}

fn detect_elbow(bins: &[RangeBinImpact], goal: Goal) -> Option<usize> {
    if bins.len() < 3 {
        return None;
    }
    let mut prefix_best: Vec<f64> = Vec::with_capacity(bins.len());
    let mut best = bins.first()?.best_metric;
    prefix_best.push(best);
    for bin in bins.iter().skip(1) {
        if is_better(goal, bin.best_metric, best) {
            best = bin.best_metric;
        }
        prefix_best.push(best);
    }
    let mut deltas: Vec<f64> = Vec::with_capacity(bins.len().saturating_sub(1));
    for window in prefix_best.windows(2) {
        let delta = improvement(goal, window[0], window[1]);
        deltas.push(delta.max(0.0));
    }
    let total_gain: f64 = deltas.iter().sum();
    let max_delta: f64 = deltas.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if !total_gain.is_finite() || total_gain <= 0.0 || !max_delta.is_finite() || max_delta <= 0.0 {
        return None;
    }
    let mut cumulative = 0.0;
    for (idx, delta) in deltas.iter().enumerate() {
        cumulative += *delta;
        let is_plateau = *delta < max_delta * 0.2;
        let reached_majority = cumulative >= total_gain * 0.6;
        if reached_majority && is_plateau {
            return Some(idx + 1); // bin index after the flattening step
        }
    }
    None
}

fn format_range_label(min_v: f64, max_v: f64) -> String {
    if (max_v - min_v).abs() < f64::EPSILON {
        format!("={:.4}", min_v)
    } else {
        format!("{:.4}..{:.4}", min_v, max_v)
    }
}

struct FrontierCandidate {
    trial_id: usize,
    signed: Vec<f64>,
    display: Vec<f64>,
    hparams: Vec<(String, String)>,
}

/// Print the non-dominated Pareto frontier of completed trials: each
/// non-dominated trial with its objective vector. Dominance is computed on the
/// stored signed (`score.<name>`) vectors; the displayed values are the raw
/// `metric.<name>` values when present.
pub fn print_pareto_frontier(store: &TrialStore, objectives: &[crate::Objective]) {
    let lf = LineEnding::from_current_platform().as_str();
    let rows = match store.load_rows() {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("failed to load trials for frontier: {e}");
            return;
        }
    };
    let mut candidates = Vec::new();
    for row in rows {
        if row.get(FIELD_TRIAL_STATUS).and_then(|s| s.parse::<TrialStatus>().ok())
            != Some(TrialStatus::Ok)
        {
            continue;
        }
        let Some(trial_id) = row.get(FIELD_TRIAL_ID).and_then(|v| v.parse::<usize>().ok())
        else {
            continue;
        };
        let mut signed = Vec::with_capacity(objectives.len());
        let mut display = Vec::with_capacity(objectives.len());
        let mut complete = true;
        for objective in objectives {
            let Some(value) = row
                .get(&format!("score.{}", objective.name))
                .and_then(|v| v.parse::<f64>().ok())
                .or_else(|| row.get(FIELD_SCORE).and_then(|v| v.parse::<f64>().ok()))
            else {
                complete = false;
                break;
            };
            signed.push(value);
            display.push(
                row.get(&metric_value_field(&objective.name))
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(value),
            );
        }
        if !complete {
            continue;
        }
        let hparams: Vec<(String, String)> = row
            .iter()
            .filter(|(k, _)| k.starts_with(crate::HP_PREFIX))
            .map(|(k, v)| (k.trim_start_matches(crate::HP_PREFIX).to_string(), v.clone()))
            .collect();
        candidates.push(FrontierCandidate {
            trial_id,
            signed,
            display,
            hparams,
        });
    }
    if candidates.is_empty() {
        println!("{lf}Pareto frontier: none");
        return;
    }
    let normalized: Vec<Vec<f64>> = candidates.iter().map(|c| c.signed.clone()).collect();
    let fronts = crate::sampler::pareto::fast_nondominated_sort(&normalized);
    let front = fronts.first().cloned().unwrap_or_default();
    println!(
        "{lf}Pareto frontier ({} of {} trials):",
        front.len(),
        candidates.len()
    );
    for idx in front {
        let candidate = &candidates[idx];
        let values: Vec<String> = objectives
            .iter()
            .zip(&candidate.display)
            .map(|(objective, value)| format!("{}={value:.6}", objective.name))
            .collect();
        println!("Trial {}  {}", candidate.trial_id, values.join("  "));
        if candidate.hparams.is_empty() {
            println!("  (no hyperparameters)");
        } else {
            println!("  Hyperparameters:");
            for (k, v) in &candidate.hparams {
                println!("    {:<20} {}", k, v);
            }
        }
        println!();
    }
}
