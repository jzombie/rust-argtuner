use std::collections::BTreeMap;

use crate::constants::{
    FIELD_SCORE, FIELD_TRIAL_ELAPSED_MS, FIELD_TRIAL_ERROR, FIELD_TRIAL_STATUS, HP_PREFIX,
    METRIC_NAMESPACE,
};

pub fn metric_value_field(metric_key: &str) -> String {
    format!("{METRIC_NAMESPACE}.{metric_key}")
}

pub fn merge_error_fields(
    existing: Option<&BTreeMap<String, String>>,
    rendered: &BTreeMap<String, String>,
    extra_fields: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out_fields = existing.cloned().unwrap_or_default();
    for (key, value) in rendered {
        if key.starts_with(crate::HP_PREFIX) && out_fields.contains_key(key) {
            continue;
        }
        out_fields.entry(key.clone()).or_insert(value.clone());
    }
    for (key, value) in extra_fields {
        out_fields.entry(key).or_insert(value);
    }
    if !out_fields.contains_key(FIELD_SCORE) {
        out_fields.insert(FIELD_SCORE.to_string(), "inf".to_string());
    }
    out_fields
}

pub fn merge_running_fields(
    mut existing: BTreeMap<String, String>,
    rendered: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    // Strip top-level status fields so reruns can mark the row as running.
    existing.remove(FIELD_TRIAL_STATUS);
    existing.remove(FIELD_TRIAL_ELAPSED_MS);
    existing.remove(FIELD_TRIAL_ERROR);

    for (key, value) in rendered {
        if key.starts_with(HP_PREFIX) && existing.contains_key(key) {
            continue;
        }
        existing.insert(key.clone(), value.clone());
    }
    existing
}

pub fn enforce_hp_immutability(
    existing: Option<&BTreeMap<String, String>>,
    out_fields: &mut BTreeMap<String, String>,
) {
    let Some(existing) = existing else {
        return;
    };
    for (key, value) in existing {
        if key.starts_with(crate::HP_PREFIX)
            && let Some(next) = out_fields.get(key)
            && next != value
        {
            eprintln!("warn: preserving existing {key}={value} (new value {next} ignored)");
        }
        out_fields.insert(key.clone(), value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn restarting_trial_preserves_existing_score_fields() {
        let mut existing = BTreeMap::new();
        existing.insert("score".to_string(), "0.5".to_string());
        existing.insert("metric".to_string(), "metric".to_string());
        existing.insert("hp.lr".to_string(), "0.1".to_string());
        existing.insert(crate::FIELD_TRIAL_STATUS.to_string(), "error".to_string());
        existing.insert(crate::FIELD_TRIAL_ELAPSED_MS.to_string(), "123".to_string());
        existing.insert(crate::FIELD_TRIAL_ERROR.to_string(), "bad".to_string());

        let mut rendered = BTreeMap::new();
        rendered.insert("hp.lr".to_string(), "0.2".to_string());

        let merged = merge_running_fields(existing, &rendered);
        assert_eq!(merged.get("score").map(String::as_str), Some("0.5"));
        assert_eq!(merged.get("hp.lr").map(String::as_str), Some("0.1"));
        assert!(!merged.contains_key(crate::FIELD_TRIAL_STATUS));
        assert!(!merged.contains_key(crate::FIELD_TRIAL_ELAPSED_MS));
        assert!(!merged.contains_key(crate::FIELD_TRIAL_ERROR));
    }

    #[test]
    fn enforce_hp_immutability_preserves_existing_values() {
        let mut existing = BTreeMap::new();
        existing.insert("hp.lr".to_string(), "0.1".to_string());
        existing.insert("hp.batch_size".to_string(), "16".to_string());

        let mut fields = BTreeMap::new();
        fields.insert("hp.lr".to_string(), "0.9".to_string());
        fields.insert("hp.batch_size".to_string(), "32".to_string());
        fields.insert("metric".to_string(), "loss".to_string());

        enforce_hp_immutability(Some(&existing), &mut fields);
        assert_eq!(fields.get("hp.lr").map(String::as_str), Some("0.1"));
        assert_eq!(fields.get("hp.batch_size").map(String::as_str), Some("16"));
        assert_eq!(fields.get("metric").map(String::as_str), Some("loss"));
    }

    #[test]
    fn enforce_hp_immutability_preserves_existing_values_even_if_missing() {
        let mut existing = BTreeMap::new();
        existing.insert("hp.lr".to_string(), "0.2".to_string());
        existing.insert("hp.steps".to_string(), "10".to_string());

        let mut fields = BTreeMap::new();
        fields.insert("metric".to_string(), "loss".to_string());

        // Missing hp.* values are restored from the existing row to keep them immutable.
        enforce_hp_immutability(Some(&existing), &mut fields);
        assert_eq!(fields.get("hp.lr").map(String::as_str), Some("0.2"));
        assert_eq!(fields.get("hp.steps").map(String::as_str), Some("10"));
        assert_eq!(fields.get("metric").map(String::as_str), Some("loss"));
    }

    #[test]
    fn merge_error_fields_preserves_existing_metric_and_score() {
        let mut existing = BTreeMap::new();
        existing.insert("metric".to_string(), "loss".to_string());
        existing.insert("score".to_string(), "0.25".to_string());
        existing.insert("metric.loss".to_string(), "0.25".to_string());
        existing.insert("hp.lr".to_string(), "0.01".to_string());

        let mut rendered = BTreeMap::new();
        rendered.insert("hp.lr".to_string(), "0.02".to_string());
        rendered.insert("hp.steps".to_string(), "5".to_string());

        let mut extra_fields = BTreeMap::new();
        extra_fields.insert("metric.error".to_string(), "bad".to_string());

        let merged = merge_error_fields(Some(&existing), &rendered, extra_fields);
        assert_eq!(merged.get("metric").map(String::as_str), Some("loss"));
        assert_eq!(merged.get("score").map(String::as_str), Some("0.25"));
        assert_eq!(merged.get("metric.loss").map(String::as_str), Some("0.25"));
        assert_eq!(merged.get("hp.lr").map(String::as_str), Some("0.01"));
        assert_eq!(merged.get("hp.steps").map(String::as_str), Some("5"));
        assert_eq!(merged.get("metric.error").map(String::as_str), Some("bad"));
    }
}
