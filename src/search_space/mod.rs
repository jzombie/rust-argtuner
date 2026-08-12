use crate::constants::HP_PREFIX;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchSpace {
    pub params: Vec<ParamSpec>,
}

impl SearchSpace {
    pub fn dims(&self) -> usize {
        self.params.len()
    }

    pub fn validate_specs(&self) -> Result<(), String> {
        for spec in &self.params {
            spec.validate_spec()?;
        }
        Ok(())
    }

    pub fn values_from_unit(&self, coords: &[f64]) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for (spec, coord) in self.params.iter().zip(coords.iter().cloned()) {
            let value = spec.value_from_unit(coord);
            out.insert(spec.name().to_string(), value);
        }
        out
    }

    pub fn fields_from_unit(&self, coords: &[f64]) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for (spec, coord) in self.params.iter().zip(coords.iter().cloned()) {
            let value = spec.value_from_unit(coord);
            out.insert(format!("{}{}", HP_PREFIX, spec.name()), value);
        }
        out
    }

    pub fn validate_value(&self, name: &str, value: &str) -> bool {
        if let Some(spec) = self.params.iter().find(|p| p.name() == name) {
            spec.validate(value)
        } else {
            // Unknown parameter, assume valid (or invalid? usually we ignore unknown params)
            true
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "ParamSpecPre")]
#[serde(tag = "type")]
pub enum ParamSpec {
    Float {
        name: String,
        min: f64,
        max: f64,
        #[serde(default, alias = "log")]
        log_scale: bool,
        #[serde(default)]
        step: Option<f64>,
        #[serde(default)]
        format: Option<String>,
    },
    Int {
        name: String,
        min: i64,
        max: i64,
        #[serde(default)]
        step: Option<i64>,
    },
    Choice {
        name: String,
        values: Vec<String>,
    },
    Bool {
        name: String,
    },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ParamSpecPre {
    Tagged(TaggedParamSpec),
    Int {
        name: String,
        min: i64,
        max: i64,
        #[serde(default)]
        step: Option<i64>,
    },
    Float {
        name: String,
        min: f64,
        max: f64,
        #[serde(default, alias = "log")]
        log_scale: bool,
        #[serde(default)]
        step: Option<f64>,
        #[serde(default)]
        format: Option<String>,
    },
    Choice {
        name: String,
        #[serde(default)]
        values: Vec<serde_json::Value>,
        #[serde(default)]
        value: Option<serde_json::Value>,
    },
    Bool {
        name: String,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum TaggedParamSpec {
    Float {
        name: String,
        min: f64,
        max: f64,
        #[serde(default, alias = "log")]
        log_scale: bool,
        #[serde(default)]
        step: Option<f64>,
        #[serde(default)]
        format: Option<String>,
    },
    Int {
        name: String,
        min: i64,
        max: i64,
        #[serde(default)]
        step: Option<i64>,
    },
    Choice {
        name: String,
        #[serde(default)]
        values: Vec<serde_json::Value>,
        #[serde(default)]
        value: Option<serde_json::Value>,
    },
    Bool {
        name: String,
    },
}

impl From<ParamSpecPre> for ParamSpec {
    fn from(pre: ParamSpecPre) -> Self {
        match pre {
            ParamSpecPre::Tagged(tagged) => match tagged {
                TaggedParamSpec::Float {
                    name,
                    min,
                    max,
                    log_scale,
                    step,
                    format,
                } => ParamSpec::Float {
                    name,
                    min,
                    max,
                    log_scale,
                    step,
                    format,
                },
                TaggedParamSpec::Int {
                    name,
                    min,
                    max,
                    step,
                } => ParamSpec::Int {
                    name,
                    min,
                    max,
                    step,
                },
                TaggedParamSpec::Choice {
                    name,
                    values,
                    value,
                } => ParamSpec::Choice {
                    name,
                    values: resolve_values(values, value),
                },
                TaggedParamSpec::Bool { name } => ParamSpec::Bool { name },
            },
            ParamSpecPre::Choice {
                name,
                values,
                value,
            } => ParamSpec::Choice {
                name,
                values: resolve_values(values, value),
            },
            ParamSpecPre::Int {
                name,
                min,
                max,
                step,
            } => ParamSpec::Int {
                name,
                min,
                max,
                step,
            },
            ParamSpecPre::Float {
                name,
                min,
                max,
                log_scale,
                step,
                format,
            } => ParamSpec::Float {
                name,
                min,
                max,
                log_scale,
                step,
                format,
            },
            ParamSpecPre::Bool { name } => ParamSpec::Bool { name },
        }
    }
}

fn resolve_values(values: Vec<serde_json::Value>, value: Option<serde_json::Value>) -> Vec<String> {
    if let Some(v) = value {
        vec![value_to_string(v)]
    } else {
        values_to_strings(values)
    }
}

fn value_to_string(v: serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s,
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        _ => v.to_string(),
    }
}

fn values_to_strings(values: Vec<serde_json::Value>) -> Vec<String> {
    values.into_iter().map(value_to_string).collect()
}

impl ParamSpec {
    pub fn name(&self) -> &str {
        match self {
            ParamSpec::Float { name, .. } => name,
            ParamSpec::Int { name, .. } => name,
            ParamSpec::Choice { name, .. } => name,
            ParamSpec::Bool { name } => name,
        }
    }

    pub fn validate_spec(&self) -> Result<(), String> {
        match self {
            ParamSpec::Float {
                name,
                log_scale,
                step,
                ..
            } => {
                if *log_scale && step.is_some() {
                    return Err(format!(
                        "parameter '{}' cannot use step with log_scale",
                        name
                    ));
                }
                if let Some(step) = step
                    && *step <= 0.0
                {
                    return Err(format!("parameter '{}' step must be > 0", name));
                }
                Ok(())
            }
            ParamSpec::Int { name, step, .. } => {
                if let Some(step) = step
                    && *step <= 0
                {
                    return Err(format!("parameter '{}' step must be > 0", name));
                }
                Ok(())
            }
            ParamSpec::Choice { .. } => Ok(()),
            ParamSpec::Bool { .. } => Ok(()),
        }
    }

    pub fn validate(&self, value: &str) -> bool {
        match self {
            ParamSpec::Float { min, max, step, .. } => {
                if let Ok(v) = value.parse::<f64>() {
                    if v < *min || v > *max {
                        return false;
                    }
                    if let Some(step) = step.filter(|s| *s > 0.0) {
                        let steps = (v - *min) / step;
                        let delta = (steps - steps.round()).abs();
                        let tol = 1e-9_f64.max(steps.abs() * 1e-9);
                        delta <= tol
                    } else {
                        true
                    }
                } else {
                    false
                }
            }
            ParamSpec::Int { min, max, step, .. } => {
                if let Ok(v) = value.parse::<i64>() {
                    if v < *min || v > *max {
                        return false;
                    }
                    if let Some(step) = step.filter(|s| *s > 0) {
                        (v - *min) % step == 0
                    } else {
                        true
                    }
                } else {
                    false
                }
            }
            ParamSpec::Choice { values, .. } => values.contains(&value.to_string()),
            ParamSpec::Bool { .. } => matches!(value, "true" | "false"),
        }
    }

    pub fn value_from_unit(&self, coord: f64) -> String {
        let c = coord.clamp(0.0, 1.0);
        match self {
            ParamSpec::Float {
                min,
                max,
                log_scale,
                step,
                format,
                ..
            } => {
                let value = if *log_scale {
                    let min = min.max(f64::MIN_POSITIVE).ln();
                    let max = max.max(f64::MIN_POSITIVE).ln();
                    (min + (max - min) * c).exp()
                } else {
                    min + (max - min) * c
                };
                let value = apply_float_step(value, *min, *max, *step);
                format_value(value, format.as_deref())
            }
            ParamSpec::Int { min, max, step, .. } => {
                let min_f = *min as f64;
                let max_f = *max as f64;
                let value = min_f + (max_f - min_f) * c;
                let value = apply_int_step(value, *min, *max, *step);
                value.to_string()
            }
            ParamSpec::Choice { values, .. } => {
                if values.is_empty() {
                    String::new()
                } else {
                    let idx = ((values.len() - 1) as f64 * c).round() as usize;
                    values[idx.min(values.len() - 1)].clone()
                }
            }
            ParamSpec::Bool { .. } => {
                if c < 0.5 {
                    "false".to_string()
                } else {
                    "true".to_string()
                }
            }
        }
    }

    pub fn discrete_value_count(&self) -> Option<usize> {
        match self {
            ParamSpec::Float { min, max, step, .. } => {
                if let Some(step) = step.filter(|s| *s > 0.0) {
                    if max < min {
                        return Some(0);
                    }
                    let range = max - min;
                    let steps = (range / step).ceil() as usize;
                    steps.checked_add(1)
                } else if (*max - *min).abs() <= f64::EPSILON {
                    Some(1)
                } else {
                    None
                }
            }
            ParamSpec::Int { min, max, step, .. } => {
                let step = step.unwrap_or(1);
                if step <= 0 {
                    return None;
                }
                if max < min {
                    return Some(0);
                }
                let range = (*max as i128) - (*min as i128);
                let step = step as i128;
                let steps = (range + step - 1) / step;
                usize::try_from(steps + 1).ok()
            }
            ParamSpec::Choice { values, .. } => Some(values.len()),
            ParamSpec::Bool { .. } => Some(2),
        }
    }

    pub fn discrete_values(&self) -> Option<Vec<String>> {
        match self {
            ParamSpec::Float {
                min,
                max,
                step,
                format,
                ..
            } => {
                if let Some(step) = step.filter(|s| *s > 0.0) {
                    if max < min {
                        return Some(Vec::new());
                    }
                    let steps = ((max - min) / step).ceil() as i64;
                    let mut out = Vec::new();
                    let mut seen = HashSet::new();
                    for idx in 0..=steps {
                        let value = min + (idx as f64) * step;
                        let value = value.clamp(*min, *max);
                        let formatted = format_value(value, format.as_deref());
                        if seen.insert(formatted.clone()) {
                            out.push(formatted);
                        }
                    }
                    Some(out)
                } else if (*max - *min).abs() <= f64::EPSILON {
                    Some(vec![format_value(*min, format.as_deref())])
                } else {
                    None
                }
            }
            ParamSpec::Int { min, max, step, .. } => {
                let step = step.unwrap_or(1);
                if step <= 0 {
                    return None;
                }
                if max < min {
                    return Some(Vec::new());
                }
                let range = (*max as i128) - (*min as i128);
                let step = step as i128;
                let steps = (range + step - 1) / step;
                let mut out = Vec::new();
                let mut seen = HashSet::new();
                for idx in 0..=steps {
                    let value = (*min as i128) + idx * step;
                    let value = value.clamp(*min as i128, *max as i128);
                    let value = value as i64;
                    if seen.insert(value) {
                        out.push(value.to_string());
                    }
                }
                Some(out)
            }
            ParamSpec::Choice { values, .. } => Some(values.clone()),
            ParamSpec::Bool { .. } => Some(vec!["false".to_string(), "true".to_string()]),
        }
    }
}

fn apply_float_step(value: f64, min: f64, max: f64, step: Option<f64>) -> f64 {
    let mut value = value;
    if let Some(step) = step.filter(|s| *s > 0.0) {
        let steps = ((value - min) / step).round();
        value = min + steps * step;
    }
    value.clamp(min, max)
}

fn apply_int_step(value: f64, min: i64, max: i64, step: Option<i64>) -> i64 {
    let mut value = value;
    if let Some(step) = step.filter(|s| *s > 0) {
        let min_f = min as f64;
        let steps = ((value - min_f) / step as f64).round();
        value = min_f + steps * step as f64;
    }
    let value = value.round() as i64;
    value.clamp(min, max)
}

fn format_value(value: f64, format: Option<&str>) -> String {
    match format {
        Some(fmt) => {
            if fmt == "{:.6}" {
                format!("{value:.6}")
            } else if fmt == "{:.4}" {
                format!("{value:.4}")
            } else if fmt == "{:.3}" {
                format!("{value:.3}")
            } else {
                format!("{value}")
            }
        }
        None => format!("{value}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{ParamSpec, SearchSpace};

    #[test]
    fn maps_unit_to_values() {
        let space = SearchSpace {
            params: vec![
                ParamSpec::Float {
                    name: "lr".to_string(),
                    min: 0.001,
                    max: 0.01,
                    log_scale: false,
                    step: None,
                    format: None,
                },
                ParamSpec::Int {
                    name: "steps".to_string(),
                    min: 10,
                    max: 20,
                    step: None,
                },
                ParamSpec::Choice {
                    name: "mode".to_string(),
                    values: vec!["a".to_string(), "b".to_string()],
                },
            ],
        };
        let values = space.values_from_unit(&[0.5, 0.0, 1.0]);
        assert_eq!(values.get("steps").map(String::as_str), Some("10"));
        assert_eq!(values.get("mode").map(String::as_str), Some("b"));
        assert!(values.get("lr").unwrap().starts_with("0.00"));
    }

    #[test]
    fn maps_unit_to_stepped_values() {
        let space = SearchSpace {
            params: vec![
                ParamSpec::Float {
                    name: "ratio".to_string(),
                    min: 0.0,
                    max: 1.0,
                    log_scale: false,
                    step: Some(0.1),
                    format: Some("{:.3}".to_string()),
                },
                ParamSpec::Int {
                    name: "buckets".to_string(),
                    min: 0,
                    max: 10,
                    step: Some(2),
                },
            ],
        };
        let values = space.values_from_unit(&[0.26, 0.26]);
        assert_eq!(values.get("ratio").map(String::as_str), Some("0.300"));
        assert_eq!(values.get("buckets").map(String::as_str), Some("2"));
        assert!(space.validate_value("ratio", "0.5"));
        assert!(!space.validate_value("ratio", "0.55"));
        assert!(space.validate_value("buckets", "6"));
        assert!(!space.validate_value("buckets", "7"));
    }

    #[test]
    fn rejects_log_scale_step_combo() {
        let space = SearchSpace {
            params: vec![ParamSpec::Float {
                name: "lr".to_string(),
                min: 1e-5,
                max: 1e-2,
                log_scale: true,
                step: Some(1e-5),
                format: None,
            }],
        };
        let err = space
            .validate_specs()
            .expect_err("step + log_scale invalid");
        assert!(err.contains("step"));
        assert!(err.contains("log_scale"));
    }

    #[test]
    fn discrete_values_skip_continuous_float() {
        let spec = ParamSpec::Float {
            name: "ratio".to_string(),
            min: 0.0,
            max: 1.0,
            log_scale: false,
            step: None,
            format: None,
        };
        assert!(spec.discrete_values().is_none());
    }

    #[test]
    fn bool_maps_unit_to_values() {
        let spec = ParamSpec::Bool {
            name: "use_dropout".to_string(),
        };
        assert_eq!(spec.value_from_unit(0.0), "false");
        assert_eq!(spec.value_from_unit(0.49), "false");
        assert_eq!(spec.value_from_unit(0.5), "true");
        assert_eq!(spec.value_from_unit(1.0), "true");
    }

    #[test]
    fn bool_validates_only_true_false() {
        let spec = ParamSpec::Bool {
            name: "use_dropout".to_string(),
        };
        assert!(spec.validate("true"));
        assert!(spec.validate("false"));
        assert!(!spec.validate("yes"));
        assert!(!spec.validate("1"));
        assert!(!spec.validate("TRUE"));
    }

    #[test]
    fn bool_is_discrete_with_two_values() {
        let spec = ParamSpec::Bool {
            name: "use_dropout".to_string(),
        };
        assert_eq!(spec.discrete_value_count(), Some(2));
        assert_eq!(
            spec.discrete_values(),
            Some(vec!["false".to_string(), "true".to_string()])
        );
    }

    #[test]
    fn bool_serde_round_trips_type_tag() {
        let spec = ParamSpec::Bool {
            name: "use_dropout".to_string(),
        };
        let toml_text = toml::to_string(&spec).expect("serialize");
        assert!(toml_text.contains("type = \"Bool\""), "toml: {toml_text}");
        assert!(toml_text.contains("name = \"use_dropout\""), "toml: {toml_text}");
        let back: ParamSpec = toml::from_str(&toml_text).expect("deserialize");
        assert!(matches!(back, ParamSpec::Bool { .. }));
    }
}
