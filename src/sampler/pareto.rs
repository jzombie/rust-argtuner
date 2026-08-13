//! Pareto-dominance utilities for multi-objective runs.
//!
//! All dominance comparisons operate on **normalized** score vectors (every
//! entry lower-is-better; `maximize` objectives are negated at ingestion). Raw
//! metric values are never mutated here — CLI/TUI/persistence keep them as-is.

use crate::Objective;

/// True when `a` Pareto-dominates `b`: every objective of `a` is <= `b`'s and
/// at least one is strictly better. Operates on normalized vectors.
pub fn dominates(a: &[f64], b: &[f64]) -> bool {
    let mut strictly_better = false;
    for (x, y) in a.iter().zip(b.iter()) {
        if x > y {
            return false;
        }
        if x < y {
            strictly_better = true;
        }
    }
    strictly_better
}

/// Normalize a raw score vector by negating `maximize` objectives, yielding
/// all-lower-is-better scores.
pub fn normalize(scores: &[f64], objectives: &[Objective]) -> Vec<f64> {
    scores
        .iter()
        .zip(objectives)
        .map(|(s, objective)| match objective.goal {
            crate::Goal::Min => *s,
            crate::Goal::Max => -*s,
        })
        .collect()
}

/// Deb's fast non-dominated sort over normalized vectors. Returns ranks as
/// lists of indices, front 0 being the non-dominated set.
pub fn fast_nondominated_sort(normalized: &[Vec<f64>]) -> Vec<Vec<usize>> {
    let n = normalized.len();
    let mut dominated_by: Vec<usize> = vec![0; n];
    let mut dominates_set: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut current: Vec<usize> = Vec::new();

    for i in 0..n {
        for j in 0..n {
            if i == j {
                continue;
            }
            if dominates(&normalized[i], &normalized[j]) {
                dominates_set[i].push(j);
            } else if dominates(&normalized[j], &normalized[i]) {
                dominated_by[i] += 1;
            }
        }
        if dominated_by[i] == 0 {
            current.push(i);
        }
    }

    let mut fronts: Vec<Vec<usize>> = Vec::new();
    while !current.is_empty() {
        let mut next: Vec<usize> = Vec::new();
        for &i in &current {
            for &j in &dominates_set[i] {
                dominated_by[j] -= 1;
                if dominated_by[j] == 0 {
                    next.push(j);
                }
            }
        }
        fronts.push(std::mem::take(&mut current));
        current = next;
    }
    fronts
}

/// Crowding distance for a front (indices into `normalized`). Endpoints get
/// infinite distance; interior points accumulate per-objective relative spans.
/// Objectives with zero variance across the front contribute nothing (the
/// span guard avoids `NaN`/`Inf` from dividing by zero).
#[allow(clippy::needless_range_loop)]
pub fn crowding_distance(front: &[usize], normalized: &[Vec<f64>]) -> Vec<f64> {
    let mut distances = vec![0.0; front.len()];
    if front.len() <= 2 {
        return distances;
    }
    let dims = normalized.first().map_or(0, |v| v.len());
    for dim in 0..dims {
        let mut order: Vec<usize> = (0..front.len()).collect();
        order.sort_by(|&a, &b| {
            normalized[front[a]][dim].total_cmp(&normalized[front[b]][dim])
        });
        let min = normalized[front[order[0]]][dim];
        let max = normalized[front[order[order.len() - 1]]][dim];
        let span = max - min;
        if span.abs() < 1e-12 {
            continue;
        }
        distances[order[0]] = f64::INFINITY;
        distances[order[order.len() - 1]] = f64::INFINITY;
        for k in 1..order.len() - 1 {
            let prev = normalized[front[order[k - 1]]][dim];
            let next = normalized[front[order[k + 1]]][dim];
            distances[order[k]] += (next - prev) / span;
        }
    }
    distances
}

/// Incrementally maintained non-dominated front of evaluated trials.
#[derive(Debug)]
pub struct ParetoFront {
    entries: Vec<FrontEntry>,
}

#[derive(Debug)]
struct FrontEntry {
    trial_id: usize,
    scores: Vec<f64>,
}

impl ParetoFront {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Insert a trial's normalized scores, evicting any trial it now
    /// dominates. Returns the evicted trial ids.
    pub fn update(&mut self, trial_id: usize, scores: Vec<f64>) -> Vec<usize> {
        let mut removed = Vec::new();
        self.entries.retain(|entry| {
            if dominates(&scores, &entry.scores) {
                removed.push(entry.trial_id);
                false
            } else {
                true
            }
        });
        // A trial dominated by a survivor is not part of the front.
        if self.entries.iter().any(|entry| dominates(&entry.scores, &scores)) {
            return removed;
        }
        self.entries.push(FrontEntry { trial_id, scores });
        removed
    }

    /// Trial ids currently on the front, in insertion order.
    pub fn trial_ids(&self) -> Vec<usize> {
        self.entries.iter().map(|entry| entry.trial_id).collect()
    }
}

impl Default for ParetoFront {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dominates_is_strict() {
        assert!(dominates(&[1.0, 2.0], &[2.0, 3.0]));
        assert!(dominates(&[1.0, 2.0], &[1.0, 3.0])); // equal on one axis, better on other
        assert!(!dominates(&[2.0, 2.0], &[1.0, 3.0]));
        assert!(!dominates(&[1.0, 2.0], &[1.0, 2.0])); // identical: no
        assert!(!dominates(&[1.0, 3.0], &[1.0, 2.0])); // worse on one axis
    }

    #[test]
    fn normalize_negates_maximize() {
        let objectives = vec![
            Objective {
                name: "loss".into(),
                goal: crate::Goal::Min,
                primary: true,
            },
            Objective {
                name: "acc".into(),
                goal: crate::Goal::Max,
                primary: false,
            },
        ];
        assert_eq!(normalize(&[0.5, 0.9], &objectives), vec![0.5, -0.9]);
    }

    #[test]
    fn deb_sort_finds_non_dominated_front() {
        // [1,2] and [2,1] are incomparable; [3,3] is dominated by both.
        let normalized = vec![vec![1.0, 2.0], vec![2.0, 1.0], vec![3.0, 3.0]];
        let fronts = fast_nondominated_sort(&normalized);
        assert_eq!(fronts[0], vec![0, 1]);
        assert_eq!(fronts[1], vec![2]);
    }

    #[test]
    fn crowding_distance_handles_zero_variance() {
        // Two identical points on objective 0 -> span 0 must not NaN.
        let normalized = vec![vec![1.0, 0.0], vec![1.0, 1.0], vec![1.0, 2.0]];
        let dist = crowding_distance(&[0, 1, 2], &normalized);
        assert!(dist.iter().all(|d| d.is_finite() || d.is_infinite()));
        assert_eq!(dist[0], f64::INFINITY);
        assert_eq!(dist[2], f64::INFINITY);
    }

    #[test]
    fn front_update_evicts_dominated() {
        let mut front = ParetoFront::new();
        front.update(0, vec![1.0, 2.0]);
        front.update(1, vec![2.0, 1.0]);
        front.update(2, vec![0.5, 0.5]); // dominates both
        assert_eq!(front.len(), 1);
        assert_eq!(front.trial_ids(), vec![2]);
    }

    #[test]
    fn front_update_ignores_dominated_new_trial() {
        let mut front = ParetoFront::new();
        front.update(0, vec![1.0, 1.0]);
        front.update(1, vec![2.0, 2.0]); // dominated
        assert_eq!(front.len(), 1);
        assert_eq!(front.trial_ids(), vec![0]);
    }
}
