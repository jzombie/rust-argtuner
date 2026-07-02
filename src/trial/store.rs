use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use similar::TextDiff;

use crate::command::CommandTemplate;
use crate::constants::{
    CONFIG_FILENAME, FIELD_METRIC, FIELD_SCORE, FIELD_TRIAL_BUDGET_STEP, FIELD_TRIAL_BUDGET_TOTAL,
    FIELD_TRIAL_CONFIG_ID, FIELD_TRIAL_ELAPSED_MS, FIELD_TRIAL_ERROR, FIELD_TRIAL_ID,
    FIELD_TRIAL_STATUS, FIELD_TRIAL_TIME, HP_PREFIX, METRIC_NAMESPACE, MODEL_NAMESPACE,
    TRIAL_PREFIX,
};
use crate::trial::db::TrialDb;
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
    /// In-memory cache of step records per trial, keyed by trial_id.
    /// Steps are accumulated during an epoch and flushed to DB at epoch end.
    step_cache: Arc<Mutex<HashMap<usize, Vec<TrialRecord>>>>,
    /// Optional TCP publisher for pushing step data to the TUI in real-time.
    step_publisher: Option<StepPublisher>,
}

/// TCP publisher that pushes step data to connected TUI processes.
/// When a new TUI connects mid-epoch, it sends a catchup message with
/// all steps currently in the cache so no data is missed.
#[derive(Debug, Clone)]
pub struct StepPublisher {
    clients: Arc<Mutex<Vec<TcpStream>>>,
}

impl StepPublisher {
    /// Try to bind on the given port, sharing the step cache for catchup.
    pub fn bind(
        port: u16,
        step_cache: Arc<Mutex<HashMap<usize, Vec<TrialRecord>>>>,
    ) -> Option<(Self, u16)> {
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
            let clients: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
            let clients_clone = clients.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(mut stream) => {
                            if let Ok(peer) = stream.peer_addr() {
                                eprintln!("step publisher: TUI connected from {peer}");
                            }
                            // Send catchup: dump all currently cached steps
                            let guard = step_cache.lock().unwrap();
                            for (trial_id, records) in guard.iter() {
                                if let Ok(json) = serde_json::to_string(&serde_json::json!({
                                    "trial_id": trial_id,
                                    "steps": records.iter().map(|r| &r.fields).collect::<Vec<_>>(),
                                    "catchup": true,
                                })) {
                                    let mut line = json;
                                    line.push('\n');
                                    let _ = stream.write_all(line.as_bytes());
                                }
                            }
                            drop(guard);
                            clients_clone.lock().unwrap().push(stream);
                        }
                        Err(e) => {
                            eprintln!("step publisher: accept error: {e}");
                        }
                    }
                }
            });
            let publisher = StepPublisher { clients };
            Some((publisher, port))
        } else {
            None
        }
    }

    /// Push a JSON message to all connected TUI clients.
    /// Silently drops disconnected clients.
    pub fn push(&self, message: &str) {
        let mut clients = self.clients.lock().unwrap();
        clients.retain(|mut stream| {
            let mut line = message.to_string();
            line.push('\n'); // TODO: Use line-ending crate
            stream.write_all(line.as_bytes()).is_ok()
        });
    }
}

/// Subscriber that connects to a StepPublisher and receives step data.
/// Used by the TUI process.
#[derive(Debug)]
pub struct StepSubscriber {
    stream: Option<TcpStream>,
    reader: Option<BufReader<TcpStream>>,
}

impl Default for StepSubscriber {
    fn default() -> Self {
        Self::new()
    }
}

impl StepSubscriber {
    pub fn new() -> Self {
        Self {
            stream: None,
            reader: None,
        }
    }

    /// Try to connect to a StepPublisher on the given port.
    /// Returns true if connected.
    pub fn connect(&mut self, port: u16) -> bool {
        if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)) {
            if let Ok(peer) = stream.peer_addr() {
                eprintln!("step subscriber: connected to publisher on port {port} from {peer}");
            }
            let reader = BufReader::new(stream.try_clone().unwrap());
            let timeout = Some(std::time::Duration::from_millis(1));
            let _ = stream.set_read_timeout(timeout);
            let _ = reader.get_ref().set_read_timeout(timeout);
            self.stream = Some(stream);
            self.reader = Some(reader);
            true
        } else {
            false
        }
    }

    /// Try to read a JSON message from the publisher (non-blocking).
    /// Returns None if no data is available or the connection is lost.
    pub fn try_recv(&mut self) -> Option<String> {
        let reader = self.reader.as_mut()?;
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                self.stream = None;
                self.reader = None;
                None
            }
            Ok(_) => {
                let trimmed = line.trim().to_string();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                None
            }
            Err(_) => {
                self.stream = None;
                self.reader = None;
                None
            }
        }
    }
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
            step_cache: Arc::new(Mutex::new(HashMap::new())),
            step_publisher: None,
        }
    }

    /// Attach a step publisher for real-time TUI communication.
    pub fn with_step_publisher(mut self, publisher: StepPublisher) -> Self {
        self.step_publisher = Some(publisher);
        self
    }

    /// Run a write operation against the DB, then atomically rewrite the CSV
    /// snapshot.  Every public write method must go through this helper so that
    /// the CSV is never stale after a write.
    fn write<F>(&self, f: F) -> std::io::Result<()>
    where
        F: FnOnce(&TrialDb) -> std::io::Result<()>,
    {
        f(&self.db)?;
        self.sync_csv()
    }

    pub fn append(&self, record: &TrialRecord) -> std::io::Result<()> {
        let record = record_with_time(record);
        self.write(|db| db.upsert_record(&record))
    }

    pub fn append_epoch(&self, record: &TrialRecord) -> std::io::Result<()> {
        let record = record_with_time(record);
        self.write(|db| db.insert_epoch_record(&record))
    }

    pub fn update(&self, record: &TrialRecord) -> std::io::Result<()> {
        let record = record_with_time(record);
        self.write(|db| db.upsert_record(&record))
    }

    pub fn reset_trial(&self, trial_id: usize) -> std::io::Result<()> {
        self.write(|db| db.reset_trial(trial_id))
    }

    /// Cache a step record in memory for the given trial.
    /// Steps are accumulated here and flushed to the DB at epoch end.
    pub fn cache_step(&self, trial_id: usize, record: TrialRecord) {
        let mut cache = self.step_cache.lock().unwrap();
        cache.entry(trial_id).or_default().push(record);
    }

    /// Flush all cached steps for the given trial to the DB.
    /// After flushing, the cache is cleared for that trial.
    pub fn flush_steps(&self, trial_id: usize) -> std::io::Result<()> {
        let mut steps: Vec<TrialRecord> = {
            let mut cache = self.step_cache.lock().unwrap();
            cache.remove(&trial_id).unwrap_or_default()
        };
        for record in &mut steps {
            let cached = record_with_time(record);
            self.db.insert_step_record(&cached)?;
        }
        if !steps.is_empty() {
            // Push step data to TUI via TCP publisher
            if let Some(ref publisher) = self.step_publisher
                && let Ok(json) = serde_json::to_string(&serde_json::json!({
                    "trial_id": trial_id,
                    "steps": steps.iter().map(|r| &r.fields).collect::<Vec<_>>(),
                })) {
                    publisher.push(&json);
                }
        }
        Ok(())
    }

    /// Load all step records from the DB.
    pub fn load_step_rows(&self) -> std::io::Result<Vec<TrialRecord>> {
        self.db.load_step_records()
    }

    /// Access the step cache (used by StepPublisher for catchup on new TUI connections).
    pub fn step_cache_handle(&self) -> Arc<Mutex<HashMap<usize, Vec<TrialRecord>>>> {
        self.step_cache.clone()
    }

    /// Load step records grouped by trial_id.
    pub fn load_step_rows_by_trial(&self) -> std::io::Result<BTreeMap<usize, Vec<TrialRecord>>> {
        let records = self.db.load_step_records()?;
        let mut by_trial: BTreeMap<usize, Vec<TrialRecord>> = BTreeMap::new();
        for record in records {
            by_trial.entry(record.trial_id).or_default().push(record);
        }
        Ok(by_trial)
    }

    pub fn rebuild_csv(&self) -> std::io::Result<()> {
        self.sync_csv()
    }

    pub fn ensure_project_config(
        &self,
        config_text: &str,
        allow_override: bool,
    ) -> std::io::Result<()> {
        let stored = self.db.load_project_config()?;
        match stored {
            None => self.db.save_project_config(config_text),
            Some(existing) => {
                if existing == config_text {
                    return Ok(());
                }
                if allow_override {
                    return self.db.save_project_config(config_text);
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
            // Only completed (Ok) trials should block re-evaluation.
            // Running/Error trials were never successfully completed.
            if row
                .get(FIELD_TRIAL_STATUS)
                .and_then(|s| s.parse::<TrialStatus>().ok())
                != Some(TrialStatus::Ok)
            {
                continue;
            }
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

    pub fn save_metadata(&self, key: &str, value: &str) -> std::io::Result<()> {
        self.db.save_metadata(key, value)
    }

    pub fn load_metadata(&self, key: &str) -> std::io::Result<Option<String>> {
        self.db.load_metadata(key)
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
    use super::{StepPublisher, StepSubscriber, TrialRecord, TrialStatus, TrialStore};
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

    #[test]
    fn steps_are_cached_and_not_in_db_before_flush() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(TRIALS_CSV_FILENAME);
        let template = crate::CommandTemplate::new("".to_string());
        let store = TrialStore::new(&path, template);

        let mut fields = BTreeMap::new();
        fields.insert("metric.loss".to_string(), "0.5".to_string());
        store.cache_step(
            0,
            TrialRecord {
                trial_id: 0,
                status: TrialStatus::Running,
                elapsed_ms: 10,
                error: None,
                fields: fields.clone(),
            },
        );
        store.cache_step(
            0,
            TrialRecord {
                trial_id: 0,
                status: TrialStatus::Running,
                elapsed_ms: 20,
                error: None,
                fields,
            },
        );

        // Steps should NOT be in DB before flush
        let db_steps = store.load_step_rows().expect("load steps");
        assert!(
            db_steps.is_empty(),
            "steps should not be in DB before flush"
        );

        // After flush, steps should be in DB
        store.flush_steps(0).expect("flush steps");
        let db_steps = store.load_step_rows().expect("load steps");
        assert_eq!(db_steps.len(), 2, "two steps should be in DB after flush");
        assert_eq!(db_steps[0].trial_id, 0);
        assert_eq!(db_steps[1].trial_id, 0);
    }

    #[test]
    fn steps_do_not_appear_in_csv() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(TRIALS_CSV_FILENAME);
        let template = crate::CommandTemplate::new("".to_string());
        let store = TrialStore::new(&path, template);

        // Add a trial + epoch record (these go to CSV)
        let mut trial_fields = BTreeMap::new();
        trial_fields.insert("hp.lr".to_string(), "0.1".to_string());
        trial_fields.insert("metric.loss".to_string(), "0.5".to_string());
        store
            .append(&TrialRecord {
                trial_id: 0,
                status: TrialStatus::Ok,
                elapsed_ms: 10,
                error: None,
                fields: trial_fields,
            })
            .expect("append");

        let mut epoch_fields = BTreeMap::new();
        epoch_fields.insert("metric.loss".to_string(), "0.4".to_string());
        store
            .append_epoch(&TrialRecord {
                trial_id: 0,
                status: TrialStatus::Running,
                elapsed_ms: 5,
                error: None,
                fields: epoch_fields,
            })
            .expect("append_epoch");

        // Add and flush step records
        let mut step_fields = BTreeMap::new();
        step_fields.insert("metric.loss".to_string(), "0.6".to_string());
        store.cache_step(
            0,
            TrialRecord {
                trial_id: 0,
                status: TrialStatus::Running,
                elapsed_ms: 1,
                error: None,
                fields: step_fields,
            },
        );
        store.flush_steps(0).expect("flush steps");

        // Verify CSV has only trial + epoch rows (2 rows), NOT step rows
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .from_path(&path)
            .expect("reader");
        let csv_rows: Vec<_> = reader
            .records()
            .collect::<Result<Vec<_>, _>>()
            .expect("csv");
        assert_eq!(
            csv_rows.len(),
            2,
            "CSV should have trial + epoch, not step rows"
        );
    }

    #[test]
    fn flush_empty_cache_is_noop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(TRIALS_CSV_FILENAME);
        let template = crate::CommandTemplate::new("".to_string());
        let store = TrialStore::new(&path, template);

        // Flushing with no cached steps should not error
        store.flush_steps(0).expect("flush empty cache");
        let db_steps = store.load_step_rows().expect("load steps");
        assert!(db_steps.is_empty());
    }

    #[test]
    fn flush_steps_pushes_to_subscriber() {
        // Full integration: cache_step → flush_steps → publisher.push → subscriber receives
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(TRIALS_CSV_FILENAME);
        let template = crate::CommandTemplate::new("".to_string());
        let store = TrialStore::new(&path, template);

        let (publisher, port) = StepPublisher::bind(45120, store.step_cache_handle())
            .expect("should bind a port in range");
        let store = store.with_step_publisher(publisher);

        let mut subscriber = StepSubscriber::new();
        assert!(subscriber.connect(port), "should connect");

        // Give publisher thread time to accept the connection
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Cache steps and flush — this is the real code path
        let mut f1 = BTreeMap::new();
        f1.insert("metric.loss".to_string(), "0.5".to_string());
        store.cache_step(
            1,
            TrialRecord {
                trial_id: 1,
                status: TrialStatus::Running,
                elapsed_ms: 10,
                error: None,
                fields: f1,
            },
        );
        store.flush_steps(1).expect("flush steps");

        // Subscriber should receive the pushed message
        let mut msg = None;
        for _ in 0..20 {
            if let Some(m) = subscriber.try_recv() {
                msg = Some(m);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(msg.is_some(), "should receive pushed message after flush_steps");
        let received = msg.unwrap();
        assert!(
            received.contains("trial_id"),
            "message should contain trial_id"
        );
        assert!(received.contains("0.5"), "message should contain step data");
    }

    #[test]
    fn subscriber_gets_catchup_for_cached_steps() {
        // Simulate mid-epoch connect: steps are cached via the store API
        // before the subscriber connects.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(TRIALS_CSV_FILENAME);
        let template = crate::CommandTemplate::new("".to_string());
        let store = TrialStore::new(&path, template);

        let mut f1 = BTreeMap::new();
        f1.insert("metric.loss".to_string(), "0.9".to_string());
        store.cache_step(
            1,
            TrialRecord {
                trial_id: 1,
                status: TrialStatus::Running,
                elapsed_ms: 10,
                error: None,
                fields: f1,
            },
        );

        let mut f2 = BTreeMap::new();
        f2.insert("metric.loss".to_string(), "0.8".to_string());
        store.cache_step(
            1,
            TrialRecord {
                trial_id: 1,
                status: TrialStatus::Running,
                elapsed_ms: 20,
                error: None,
                fields: f2,
            },
        );

        let (_publisher, port) =
            StepPublisher::bind(45121, store.step_cache_handle()).expect("should bind");
        let mut subscriber = StepSubscriber::new();
        assert!(subscriber.connect(port), "should connect");

        // Subscriber should receive catchup messages with cached steps
        let mut received_steps = Vec::new();
        for _ in 0..20 {
            while let Some(line) = subscriber.try_recv() {
                if let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line)
                    && msg.get("catchup") == Some(&serde_json::Value::Bool(true))
                    && let Some(steps) = msg.get("steps").and_then(|v| v.as_array())
                {
                    for step in steps {
                        if let Some(fields) = step.as_object() {
                            for (k, v) in fields {
                                received_steps
                                    .push((k.clone(), v.as_str().unwrap_or("").to_string()));
                            }
                        }
                    }
                }
            }
            if received_steps.len() >= 2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert!(
            received_steps.contains(&("metric.loss".to_string(), "0.9".to_string())),
            "should receive first cached step"
        );
        assert!(
            received_steps.contains(&("metric.loss".to_string(), "0.8".to_string())),
            "should receive second cached step"
        );
        assert!(
            received_steps.len() >= 2,
            "should have received at least 2 step fields from catchup, got {}",
            received_steps.len()
        );
    }
}
