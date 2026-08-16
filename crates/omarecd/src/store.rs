use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use omarec_core::{SessionPhase, SessionRecord};

use crate::coordinator::{CoordinatorError, SessionStore};

#[derive(Clone, Debug)]
pub struct FileSessionStore {
    directory: PathBuf,
}

impl FileSessionStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    fn path_for(&self, record: &SessionRecord) -> PathBuf {
        self.directory.join(format!("{}.json", record.session_id))
    }

    fn write_atomic(path: &Path, record: &SessionRecord) -> Result<(), CoordinatorError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|source| CoordinatorError::Store(source.to_string()))?;
        }
        let encoded = serde_json::to_vec_pretty(record)
            .map_err(|source| CoordinatorError::Store(source.to_string()))?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, encoded).map_err(|source| CoordinatorError::Store(source.to_string()))?;
        fs::rename(&tmp, path).map_err(|source| CoordinatorError::Store(source.to_string()))?;
        Ok(())
    }

    fn load_all(&self) -> Result<Vec<SessionRecord>, CoordinatorError> {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(CoordinatorError::Store(error.to_string())),
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| CoordinatorError::Store(source.to_string()))?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let source = fs::read_to_string(&path)
                .map_err(|error| CoordinatorError::Store(error.to_string()))?;
            let record: SessionRecord = serde_json::from_str(&source)
                .map_err(|error| CoordinatorError::Store(error.to_string()))?;
            records.push(record);
        }
        Ok(records)
    }
}

impl SessionStore for FileSessionStore {
    async fn create(&self, record: SessionRecord) -> Result<(), CoordinatorError> {
        let path = self.path_for(&record);
        if path.exists() {
            return Err(CoordinatorError::Store(format!(
                "durable record already exists for {}",
                record.session_id
            )));
        }
        Self::write_atomic(&path, &record)
    }

    async fn load_active(&self) -> Result<Option<SessionRecord>, CoordinatorError> {
        let mut active = self
            .load_all()?
            .into_iter()
            .filter(|record| record.phase.is_active())
            .collect::<Vec<_>>();
        match active.len() {
            0 => Ok(None),
            1 => Ok(active.pop()),
            _ => Err(CoordinatorError::Store(
                "multiple non-terminal durable session records exist".to_owned(),
            )),
        }
    }

    async fn save(&self, record: SessionRecord) -> Result<(), CoordinatorError> {
        Self::write_atomic(&self.path_for(&record), &record)
    }

    async fn load_nonterminal(&self) -> Result<Vec<SessionRecord>, CoordinatorError> {
        Ok(self
            .load_all()?
            .into_iter()
            .filter(|record| {
                !matches!(
                    record.phase,
                    SessionPhase::Idle
                        | SessionPhase::Completed
                        | SessionPhase::Cancelled
                        | SessionPhase::Failed
                )
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omarec_core::SessionId;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("omarec-store-{nanos}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[tokio::test]
    async fn create_then_load_active_roundtrip() {
        let dir = unique_dir();
        let store = FileSessionStore::new(&dir);
        let session = SessionId::new();
        let mut record = SessionRecord::new(session, PathBuf::from("/tmp/out.mp4"), 1);
        record.phase = SessionPhase::Preparing;
        store.create(record.clone()).await.unwrap();
        let loaded = store.load_active().await.unwrap().unwrap();
        assert_eq!(loaded.session_id, session);
        assert_eq!(loaded.phase, SessionPhase::Preparing);
        record.phase = SessionPhase::Completed;
        store.save(record).await.unwrap();
        assert!(store.load_active().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn duplicate_create_is_rejected() {
        let dir = unique_dir();
        let store = FileSessionStore::new(&dir);
        let record = SessionRecord::new(SessionId::new(), PathBuf::from("/tmp/out.mp4"), 1);
        store.create(record.clone()).await.unwrap();
        let error = store.create(record).await.unwrap_err();
        assert_eq!(error.code(), "store_failed");
    }
}
