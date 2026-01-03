use std::collections::{BTreeMap, BTreeSet};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use similar::TextDiff;

use crate::command::CommandTemplate;
use crate::constants::{
    CONFIG_FILENAME, FIELD_METRIC, FIELD_SCORE, FIELD_TRIAL_BUDGET_STEP, FIELD_TRIAL_BUDGET_TOTAL,
    FIELD_TRIAL_CONFIG_ID, FIELD_TRIAL_ELAPSED_MS, FIELD_TRIAL_ERROR, FIELD_TRIAL_ID,
    FIELD_TRIAL_STATUS, FIELD_TRIAL_TIME, HP_PREFIX, METRIC_NAMESPACE, MODEL_NAMESPACE,
    TRIAL_PREFIX,
};
use crate::db::TrialDb;
use chrono::Utc;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TrialStatus {
    Running,
    Ok,
    Error,
}

impl TrialStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TrialStatus::Running => "running",
            TrialStatus::Ok => "ok",
            TrialStatus::Error => "error",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        value.parse().ok()
    }
}

impl std::str::FromStr for TrialStatus {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "running" => Ok(TrialStatus::Running),
            "ok" => Ok(TrialStatus::Ok),
            "error" => Ok(TrialStatus::Error),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrialRecord {
    pub trial_id: usize,
    pub status: TrialStatus,
    pub elapsed_ms: u128,
    pub error: Option<String>,
    pub fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct TrialStore {
    csv_path: PathBuf,
    db: TrialDb,
    template: CommandTemplate,
}

#[derive(Debug, Clone)]
pub struct PlaceholderValue {
    pub name: String,
    pub value: String,
    pub source: String,
}

impl TrialStore {
    pub fn new<P: AsRef<Path>>(path: P, template: CommandTemplate) -> Self {
        let csv_path = path.as_ref().to_path_buf();
        let db_path = csv_path.with_extension("sqlite");
        Self {
            csv_path,
            db: TrialDb::new(db_path),
            template,
        }
    }

    pub fn append(&self, record: &TrialRecord) -> std::io::Result<()> {
        let record = record_with_time(record);
        self.db.upsert_record(&record)?;
        self.sync_csv()
    }

    pub fn append_epoch(&self, record: &TrialRecord) -> std::io::Result<()> {
        let record = record_with_time(record);
        self.db.insert_epoch_record(&record)?;
        self.sync_csv()
    }

    pub fn update(&self, record: &TrialRecord) -> std::io::Result<()> {
        let record = record_with_time(record);
        self.db.upsert_record(&record)?;
        self.sync_csv()
    }

    pub fn rebuild_csv(&self) -> std::io::Result<()> {
        self.sync_csv()
    }

    pub fn ensure_project_config(&self, config_text: &str) -> std::io::Result<()> {
        let stored = self.db.load_project_config()?;
        match stored {
            None => self.db.save_project_config(config_text),
            Some(existing) => {
                if existing == config_text {
                    return Ok(());
                }
                if !self.db.has_any_trials()? {
                    return self.db.save_project_config(config_text);
                }
                let diff = diff_lines(&existing, config_text);
                let message = format!(
                    "{CONFIG_FILENAME} changed since the last run; refusing to resume.\n{diff}"
                );
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    message,
                ))
            }
        }
    }

    pub fn command_for_trial(&self, trial_id: usize) -> std::io::Result<Option<String>> {
        Ok(self
            .render_command_with_values(trial_id)?
            .map(|rendered| rendered.0))
    }

    pub fn render_command_with_values(
        &self,
        trial_id: usize,
    ) -> std::io::Result<Option<(String, Vec<PlaceholderValue>)>> {
        let placeholders = self.template.placeholders().unwrap_or_default();
        let row_map = match self.load_fields(trial_id)? {
            Some(row) => row,
            None => return Ok(None),
        };
        let mut values = std::collections::HashMap::new();
        let mut resolved = Vec::new();
        for placeholder in placeholders {
            let resolved_value = value_for_placeholder(&row_map, &placeholder);
            if let Some((value, source)) = resolved_value {
                values.insert(placeholder.clone(), value.clone());
                resolved.push(PlaceholderValue {
                    name: placeholder,
                    value,
                    source,
                });
            } else {
                return Ok(None);
            }
        }
        let command = self
            .template
            .render(&values)
            .map_err(|err| std::io::Error::other(err.to_string()))?;
        Ok(Some((command, resolved)))
    }

    pub fn load_fields(
        &self,
        trial_id: usize,
    ) -> std::io::Result<Option<BTreeMap<String, String>>> {
        let records = self.db.load_records()?;
        if records.is_empty() {
            return Ok(None);
        }
        let (_, mut rows) = rows_with_headers(&records);
        let row = rows
            .iter_mut()
            .find(|row| {
                row.get(FIELD_TRIAL_ID)
                    .and_then(|v| v.parse::<usize>().ok())
                    == Some(trial_id)
            })
            .map(|row| row.clone());
        Ok(row)
    }

    pub fn load_rows(&self) -> std::io::Result<Vec<BTreeMap<String, String>>> {
        let records = self.db.load_records()?;
        let (_, rows) = rows_with_headers(&records);
        Ok(rows)
    }

    pub fn find_duplicate_config(
        &self,
        config_id: Option<usize>,
        fields: &BTreeMap<String, String>,
    ) -> std::io::Result<Option<usize>> {
        let candidate = hp_fields(fields);
        let rows = self.load_rows()?;
        for row in rows {
            let row_config_id = row
                .get(FIELD_TRIAL_CONFIG_ID)
                .and_then(|value| value.parse::<usize>().ok());
            if config_id.is_some() && config_id == row_config_id {
                continue;
            }
            if hp_fields(&row) == candidate {
                let trial_id = row
                    .get(FIELD_TRIAL_ID)
                    .and_then(|value| value.parse::<usize>().ok());
                return Ok(trial_id);
            }
        }
        Ok(None)
    }

    pub fn next_trial_id(&self) -> std::io::Result<usize> {
        self.db.next_trial_id()
    }

    pub fn find_last_trial_for_config(&self, config_id: usize) -> std::io::Result<Option<usize>> {
        let records = self.db.load_records()?;
        let mut last_trial_id = None;
        for record in records {
            let row_config_id = record
                .fields
                .get(FIELD_TRIAL_CONFIG_ID)
                .and_then(|v| v.parse::<usize>().ok());
            if row_config_id == Some(config_id) {
                last_trial_id = Some(record.trial_id);
            }
        }
        Ok(last_trial_id)
    }

    fn sync_csv(&self) -> std::io::Result<()> {
        let mut records = self.db.load_records()?;
        let mut epoch_records = self.db.load_epoch_records()?;
        records.append(&mut epoch_records);
        let (headers, rows) = rows_with_headers(&records);
        write_csv_snapshot(&self.csv_path, &headers, &rows)
    }
}

fn diff_lines(old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let stored_label = format!("stored/{CONFIG_FILENAME}");
    let current_label = format!("current/{CONFIG_FILENAME}");
    diff.unified_diff()
        .header(&stored_label, &current_label)
        .to_string()
}

fn record_with_time(record: &TrialRecord) -> TrialRecord {
    let mut fields = record.fields.clone();
    if !fields.contains_key(FIELD_TRIAL_TIME) {
        fields.insert(FIELD_TRIAL_TIME.to_string(), Utc::now().to_rfc3339());
    }
    TrialRecord {
        trial_id: record.trial_id,
        status: record.status,
        elapsed_ms: record.elapsed_ms,
        error: record.error.clone(),
        fields,
    }
}

fn record_to_map(record: &TrialRecord) -> BTreeMap<String, String> {
    let record = record_with_time(record);
    let mut map = BTreeMap::new();
    map.insert(FIELD_TRIAL_ID.to_string(), record.trial_id.to_string());
    map.insert(
        FIELD_TRIAL_STATUS.to_string(),
        record.status.as_str().to_string(),
    );
    map.insert(
        FIELD_TRIAL_ELAPSED_MS.to_string(),
        record.elapsed_ms.to_string(),
    );
    if let Some(err) = record.error.as_ref() {
        map.insert(FIELD_TRIAL_ERROR.to_string(), err.clone());
    }
    // Insert timestamp if not already present in fields. Use RFC3339 UTC time.
    if !record.fields.contains_key(FIELD_TRIAL_TIME) {
        map.insert(FIELD_TRIAL_TIME.to_string(), Utc::now().to_rfc3339());
    }
    for (key, value) in record.fields.iter() {
        map.insert(key.clone(), value.clone());
    }
    map
}

fn rows_with_headers(records: &[TrialRecord]) -> (Vec<String>, Vec<BTreeMap<String, String>>) {
    let mut rows = records.iter().map(record_to_map).collect::<Vec<_>>();
    let headers = ordered_headers(rows.iter().flat_map(|row| row.keys().cloned()));
    for row in &mut rows {
        *row = fill_row(row.clone(), &headers);
    }
    (headers, rows)
}

fn fill_row(mut row: BTreeMap<String, String>, headers: &[String]) -> BTreeMap<String, String> {
    for header in headers {
        row.entry(header.clone()).or_default();
    }
    row
}

fn hp_fields(fields: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    fields
        .iter()
        .filter(|(key, _)| key.starts_with(HP_PREFIX))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn write_csv_snapshot(
    path: &Path,
    headers: &[String],
    rows: &[BTreeMap<String, String>],
) -> std::io::Result<()> {
    rewrite_all(path, headers.to_vec(), |writer, headers| {
        for row in rows {
            write_record(writer, headers, row)?;
        }
        Ok(())
    })
}

fn rewrite_all<F>(path: &Path, headers: Vec<String>, mut write_rows: F) -> std::io::Result<()>
where
    F: FnMut(&mut csv::Writer<BufWriter<std::fs::File>>, &Vec<String>) -> std::io::Result<()>,
{
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temp = tempfile::NamedTempFile::new_in(parent)?;
    let mut writer = csv::WriterBuilder::new()
        .has_headers(false)
        .from_writer(BufWriter::new(temp.reopen()?));
    if !headers.is_empty() {
        writer.write_record(&headers)?;
    }
    write_rows(&mut writer, &headers)?;
    writer.flush()?;
    match temp.persist(path) {
        Ok(_) => Ok(()),
        Err(err) => {
            if err.error.kind() == std::io::ErrorKind::AlreadyExists {
                let _ = std::fs::remove_file(path);
                err.file.persist(path)?;
                Ok(())
            } else {
                Err(err.error)
            }
        }
    }
}

fn write_record(
    writer: &mut csv::Writer<BufWriter<std::fs::File>>,
    headers: &[String],
    row: &BTreeMap<String, String>,
) -> std::io::Result<()> {
    let record = headers
        .iter()
        .map(|key| row.get(key).cloned().unwrap_or_default())
        .collect::<Vec<_>>();
    writer.write_record(&record)?;
    Ok(())
}

#[cfg(test)]
fn record_to_row_map(headers: &[String], record: &csv::StringRecord) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for (key, value) in headers.iter().zip(record.iter()) {
        map.insert(key.to_string(), value.to_string());
    }
    map
}

fn ordered_headers(keys: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut set = keys.into_iter().collect::<BTreeSet<_>>();
    for key in [
        FIELD_TRIAL_ID,
        FIELD_TRIAL_STATUS,
        FIELD_TRIAL_ELAPSED_MS,
        FIELD_TRIAL_ERROR,
        FIELD_METRIC,
        FIELD_SCORE,
    ] {
        set.insert(key.to_string());
    }
    let mut metric = Vec::new();
    let mut trial = Vec::new();
    let mut other = Vec::new();
    let mut hp = Vec::new();
    let metric_prefix = format!("{METRIC_NAMESPACE}.");
    let model_prefix = format!("{MODEL_NAMESPACE}.");
    for key in set.iter() {
        if key.starts_with(&metric_prefix) || key.starts_with(&model_prefix) {
            metric.push(key.clone());
        } else if key.starts_with(TRIAL_PREFIX) {
            trial.push(key.clone());
        } else if key.starts_with(HP_PREFIX) {
            hp.push(key.clone());
        } else if !matches!(
            key.as_str(),
            FIELD_TRIAL_ID
                | FIELD_TRIAL_STATUS
                | FIELD_TRIAL_ELAPSED_MS
                | FIELD_TRIAL_ERROR
                | FIELD_METRIC
                | FIELD_SCORE
        ) {
            other.push(key.clone());
        }
    }
    metric.sort();
    trial.sort();
    other.sort();
    hp.sort();
    let mut headers = Vec::new();
    // Start with `trial_id`, then `trial.time` if present, then `status` and
    // budget columns (if present), then the remaining top-level fields.
    headers.push(FIELD_TRIAL_ID.to_string());
    if trial.iter().any(|k| k.as_str() == FIELD_TRIAL_TIME) {
        headers.push(FIELD_TRIAL_TIME.to_string());
    }
    headers.push(FIELD_TRIAL_STATUS.to_string());
    if trial.iter().any(|k| k.as_str() == FIELD_TRIAL_BUDGET_TOTAL) {
        headers.push(FIELD_TRIAL_BUDGET_TOTAL.to_string());
    }
    if trial.iter().any(|k| k.as_str() == FIELD_TRIAL_BUDGET_STEP) {
        headers.push(FIELD_TRIAL_BUDGET_STEP.to_string());
    }
    for key in [
        FIELD_TRIAL_ELAPSED_MS,
        FIELD_TRIAL_ERROR,
        FIELD_METRIC,
        FIELD_SCORE,
    ] {
        headers.push(key.to_string());
    }
    // Then include remaining trial-scoped fields (except time/budget columns already placed).
    for k in &trial {
        if k.as_str() != FIELD_TRIAL_TIME
            && k.as_str() != FIELD_TRIAL_BUDGET_TOTAL
            && k.as_str() != FIELD_TRIAL_BUDGET_STEP
        {
            headers.push(k.clone());
        }
    }
    headers.extend(metric);
    headers.extend(other);
    headers.extend(hp);
    headers
}

fn value_for_placeholder(
    row: &BTreeMap<String, String>,
    placeholder: &str,
) -> Option<(String, String)> {
    if let Some(value) = row.get(placeholder) {
        return Some((value.clone(), placeholder.to_string()));
    }
    let trial_key = format!("{TRIAL_PREFIX}{placeholder}");
    if let Some(value) = row.get(&trial_key) {
        return Some((value.clone(), trial_key));
    }
    let hp_key = format!("{HP_PREFIX}{placeholder}");
    if let Some(value) = row.get(&hp_key) {
        return Some((value.clone(), hp_key));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{TrialRecord, TrialStatus, TrialStore};
    use crate::constants::{FIELD_SCORE, FIELD_TRIAL_CONFIG_ID, HP_PREFIX, TRIALS_CSV_FILENAME};
    use std::collections::BTreeMap;

    #[test]
    fn update_overwrites_running_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(TRIALS_CSV_FILENAME);
        let template = crate::CommandTemplate::new("".to_string());
        let store = TrialStore::new(&path, template);
        let mut fields = BTreeMap::new();
        fields.insert(format!("{HP_PREFIX}lr"), "0.1".to_string());
        store
            .append(&TrialRecord {
                trial_id: 0,
                status: TrialStatus::Running,
                elapsed_ms: 0,
                error: None,
                fields: fields.clone(),
            })
            .expect("append");
        fields.insert(FIELD_SCORE.to_string(), "0.42".to_string());
        store
            .update(&TrialRecord {
                trial_id: 0,
                status: TrialStatus::Ok,
                elapsed_ms: 10,
                error: None,
                fields,
            })
            .expect("update");
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .from_path(&path)
            .expect("reader");
        let records = reader
            .records()
            .collect::<Result<Vec<_>, _>>()
            .expect("records ok");
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert!(record.iter().any(|v| v == "ok"));
        assert!(record.iter().any(|v| v == "0.42"));
    }

    #[test]
    fn next_trial_id_tracks_max() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(TRIALS_CSV_FILENAME);
        let template = crate::CommandTemplate::new("".to_string());
        let store = TrialStore::new(&path, template);
        assert_eq!(store.next_trial_id().expect("next"), 0);
        store
            .append(&TrialRecord {
                trial_id: 2,
                status: TrialStatus::Running,
                elapsed_ms: 0,
                error: None,
                fields: BTreeMap::new(),
            })
            .expect("append");
        assert_eq!(store.next_trial_id().expect("next"), 3);
    }

    #[test]
    fn headers_group_metrics_and_hparams() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(TRIALS_CSV_FILENAME);
        let template = crate::CommandTemplate::new("".to_string());
        let store = TrialStore::new(&path, template);
        let mut fields = BTreeMap::new();
        fields.insert("metric.loss".to_string(), "0.5".to_string());
        fields.insert(FIELD_TRIAL_CONFIG_ID.to_string(), "3".to_string());
        fields.insert(format!("{HP_PREFIX}lr"), "0.1".to_string());
        store
            .append(&TrialRecord {
                trial_id: 0,
                status: TrialStatus::Ok,
                elapsed_ms: 5,
                error: None,
                fields,
            })
            .expect("append");
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .from_path(&path)
            .expect("reader");
        let headers = reader
            .headers()
            .expect("headers")
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>();
        let loss_idx = headers
            .iter()
            .position(|v| v == "metric.loss")
            .expect("metric loss");
        let trial_idx = headers
            .iter()
            .position(|v| v == FIELD_TRIAL_CONFIG_ID)
            .expect("trial config id");
        let lr_idx = headers.iter().position(|v| v == "hp.lr").expect("hp.lr");
        assert!(loss_idx < lr_idx);
        assert!(trial_idx < lr_idx);
    }

    #[test]
    fn command_for_trial_returns_rendered_command() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(TRIALS_CSV_FILENAME);
        let template = crate::CommandTemplate::new("echo --lr {lr}".to_string());
        let store = TrialStore::new(&path, template);

        let mut fields = BTreeMap::new();
        fields.insert("hp.lr".to_string(), "0.5".to_string());

        store
            .append(&TrialRecord {
                trial_id: 0,
                status: TrialStatus::Ok,
                elapsed_ms: 0,
                error: None,
                fields,
            })
            .expect("append");

        let command = store
            .command_for_trial(0)
            .expect("command lookup")
            .expect("missing command");
        assert_eq!(command, "echo --lr 0.5");
    }

    #[test]
    fn csv_snapshot_matches_db_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(TRIALS_CSV_FILENAME);
        let template = crate::CommandTemplate::new("".to_string());
        let store = TrialStore::new(&path, template);

        let mut fields = BTreeMap::new();
        fields.insert(format!("{HP_PREFIX}lr"), "0.1".to_string());
        fields.insert(FIELD_TRIAL_CONFIG_ID.to_string(), "3".to_string());
        store
            .append(&TrialRecord {
                trial_id: 0,
                status: TrialStatus::Running,
                elapsed_ms: 0,
                error: None,
                fields: fields.clone(),
            })
            .expect("append");

        fields.insert(FIELD_SCORE.to_string(), "0.42".to_string());
        store
            .update(&TrialRecord {
                trial_id: 0,
                status: TrialStatus::Ok,
                elapsed_ms: 10,
                error: None,
                fields,
            })
            .expect("update");

        let db_rows = store.load_rows().expect("db rows");

        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .from_path(&path)
            .expect("reader");
        let headers = reader
            .headers()
            .expect("headers")
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>();
        let csv_rows = reader
            .records()
            .map(|record| super::record_to_row_map(&headers, &record.expect("record")))
            .collect::<Vec<_>>();

        assert_eq!(csv_rows, db_rows);
    }

    #[test]
    fn csv_resyncs_after_external_modification() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(TRIALS_CSV_FILENAME);
        let template = crate::CommandTemplate::new("".to_string());
        let store = TrialStore::new(&path, template);

        let mut fields = BTreeMap::new();
        fields.insert(format!("{HP_PREFIX}lr"), "0.1".to_string());
        store
            .append(&TrialRecord {
                trial_id: 0,
                status: TrialStatus::Ok,
                elapsed_ms: 5,
                error: None,
                fields,
            })
            .expect("append");

        std::fs::write(&path, "corrupted,data\n1,2\n").expect("corrupt csv");

        let mut fields = BTreeMap::new();
        fields.insert(format!("{HP_PREFIX}lr"), "0.2".to_string());
        fields.insert(FIELD_SCORE.to_string(), "0.5".to_string());
        store
            .append(&TrialRecord {
                trial_id: 1,
                status: TrialStatus::Ok,
                elapsed_ms: 7,
                error: None,
                fields,
            })
            .expect("append");

        let db_rows = store.load_rows().expect("db rows");
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .from_path(&path)
            .expect("reader");
        let headers = reader
            .headers()
            .expect("headers")
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>();
        let csv_rows = reader
            .records()
            .map(|record| super::record_to_row_map(&headers, &record.expect("record")))
            .collect::<Vec<_>>();

        assert_eq!(csv_rows, db_rows);
    }
}
