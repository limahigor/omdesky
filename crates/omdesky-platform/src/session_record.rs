use crate::state::{StateError, atomic_write_json, read_json};
use omdesky_core::{InputMode, SessionId};
use serde::{Deserialize, Serialize};
use std::path::Path;
use time::OffsetDateTime;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: SessionId,
    pub remote_node: String,
    pub input_mode: InputMode,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moonlight: Option<SupervisedProcess>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SupervisedProcess {
    pub pid: u32,
    pub start_ticks: u64,
}

impl SessionRecord {
    pub fn new(remote_node: impl Into<String>, input_mode: InputMode) -> Self {
        Self {
            session_id: SessionId::new(),
            remote_node: remote_node.into(),
            input_mode,
            started_at: OffsetDateTime::now_utc(),
            moonlight: None,
        }
    }

    pub fn write(&self, path: &Path) -> Result<(), StateError> {
        atomic_write_json(path, self, false)
    }

    pub fn read(path: &Path) -> Result<Self, StateError> {
        read_json(path)
    }

    pub fn remove(path: &Path) {
        let _ = std::fs::remove_file(path);
    }

    pub fn remove_if_owned(path: &Path, session_id: SessionId) {
        if Self::read(path).is_ok_and(|record| record.session_id == session_id) {
            Self::remove(path);
        }
    }
}

impl SupervisedProcess {
    pub fn observe(pid: u32) -> Option<Self> {
        process_start_ticks(pid).map(|start_ticks| Self { pid, start_ticks })
    }

    pub fn is_still_running(&self) -> bool {
        process_start_ticks(self.pid) == Some(self.start_ticks)
    }
}

#[cfg(target_os = "linux")]
pub fn process_start_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;

    parse_start_ticks(&stat)
}

#[cfg(not(target_os = "linux"))]
pub fn process_start_ticks(_pid: u32) -> Option<u64> {
    None
}

const START_TIME_FIELDS_AFTER_COMM: usize = 19;

pub fn parse_start_ticks(stat: &str) -> Option<u64> {
    let after_comm = stat.rsplit_once(')')?.1;

    after_comm
        .split_whitespace()
        .nth(START_TIME_FIELDS_AFTER_COMM)?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat_line(start_ticks: u64) -> String {
        let mut fields = vec!["S".to_owned()];
        fields.extend((3..=21).map(|index| index.to_string()));
        fields[19] = start_ticks.to_string();

        format!("4242 (moonlight (qt)) {}", fields.join(" "))
    }

    #[test]
    fn test_start_ticks_are_read_past_a_command_name_containing_parentheses() {
        assert_eq!(parse_start_ticks(&stat_line(987_654)), Some(987_654));
    }

    #[test]
    fn test_malformed_stat_yields_no_start_time() {
        assert_eq!(parse_start_ticks(""), None);
        assert_eq!(parse_start_ticks("4242 (moonlight) S"), None);
    }

    #[test]
    fn test_this_process_reports_a_stable_start_time() {
        let observed = SupervisedProcess::observe(std::process::id())
            .expect("the current process has a start time");

        assert!(observed.is_still_running());
    }

    #[test]
    fn test_a_record_round_trips_and_is_only_removed_by_its_owner() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("current-session.json");

        let mut record = SessionRecord::new("workstation", InputMode::Remote);
        record.moonlight = Some(SupervisedProcess {
            pid: 4242,
            start_ticks: 99,
        });
        record.write(&path).expect("record written");

        let loaded = SessionRecord::read(&path).expect("record read");
        assert_eq!(loaded, record);

        SessionRecord::remove_if_owned(&path, SessionId::new());
        assert!(path.exists());

        SessionRecord::remove_if_owned(&path, record.session_id);
        assert!(!path.exists());
    }
}
