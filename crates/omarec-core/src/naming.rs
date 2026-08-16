//! Collision-free output naming. Preview is advisory; start must reserve.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{Container, SessionId};

pub trait Clock {
    fn formatted_stamp(&self, pattern: &str) -> String;
}

pub trait OccupiedNames {
    fn is_occupied(&self, path: &Path) -> bool;
}

impl OccupiedNames for std::collections::BTreeSet<PathBuf> {
    fn is_occupied(&self, path: &Path) -> bool {
        self.contains(path)
    }
}

/// Occupied if the path exists, including as a dangling symlink.
#[derive(Clone, Copy, Debug, Default)]
pub struct PathOccupied;

impl OccupiedNames for PathOccupied {
    fn is_occupied(&self, path: &Path) -> bool {
        path.symlink_metadata().is_ok()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputPlan {
    pub final_output: PathBuf,
    pub staging_output: PathBuf,
    pub first_frame_timestamp: PathBuf,
    pub advisory: bool,
}

#[derive(Clone, Debug)]
pub struct OutputNamer {
    pub directory: PathBuf,
    pub filename_pattern: String,
    pub container: Container,
}

impl OutputNamer {
    pub fn preview(&self, clock: &impl Clock) -> OutputPlan {
        let final_output = self.directory.join(self.filename(clock, None));
        Self::paths(final_output, SessionId::from_uuid(uuid::Uuid::nil()), true)
    }

    pub fn reserve(
        &self,
        clock: &impl Clock,
        session_id: SessionId,
        occupied: &impl OccupiedNames,
    ) -> Result<OutputPlan, NamingError> {
        for suffix in 0..1000u32 {
            let final_output = self
                .directory
                .join(self.filename(clock, (suffix > 0).then_some(suffix)));
            if occupied.is_occupied(&final_output) {
                continue;
            }
            let plan = Self::paths(final_output, session_id, false);
            if occupied.is_occupied(&plan.staging_output) {
                continue;
            }
            return Ok(plan);
        }
        Err(NamingError::Exhausted)
    }

    fn filename(&self, clock: &impl Clock, suffix: Option<u32>) -> String {
        let stamp = clock.formatted_stamp(&self.filename_pattern);
        match suffix {
            Some(value) => format!("{stamp}-{value}.{}", self.container.extension()),
            None => format!("{stamp}.{}", self.container.extension()),
        }
    }

    fn paths(final_output: PathBuf, session_id: SessionId, advisory: bool) -> OutputPlan {
        let staging_output = staging_path(&final_output, session_id);
        let first_frame_timestamp = PathBuf::from(format!("{}.ts", staging_output.display()));
        OutputPlan {
            final_output,
            staging_output,
            first_frame_timestamp,
            advisory,
        }
    }
}

/// Reserve an explicit destination, adding `-N` before the extension on collision.
pub fn reserve_explicit(
    final_output: &Path,
    session_id: SessionId,
    occupied: &impl OccupiedNames,
) -> Result<OutputPlan, NamingError> {
    let parent = final_output
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let stem = final_output
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("capture");
    let extension = final_output
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or(Container::Mp4.extension());
    for suffix in 0..1000u32 {
        let name = if suffix == 0 {
            format!("{stem}.{extension}")
        } else {
            format!("{stem}-{suffix}.{extension}")
        };
        let candidate = parent.join(name);
        if occupied.is_occupied(&candidate) {
            continue;
        }
        let plan = OutputNamer::paths(candidate, session_id, false);
        if occupied.is_occupied(&plan.staging_output) {
            continue;
        }
        return Ok(plan);
    }
    Err(NamingError::Exhausted)
}

pub fn staging_path(final_output: &Path, session_id: SessionId) -> PathBuf {
    let parent = final_output.parent().unwrap_or_else(|| Path::new("."));
    let extension = final_output
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or(Container::Mp4.extension());
    parent.join(format!(".omarec-{session_id}.part.{extension}"))
}

/// Parse `user-dirs.dirs` without sourcing shell. Only `$HOME` / `${HOME}` are expanded.
pub fn parse_user_dirs(source: &str, home: &Path) -> BTreeMap<String, PathBuf> {
    let mut dirs = BTreeMap::new();
    for line in source.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !key.starts_with("XDG_") || !key.ends_with("_DIR") {
            continue;
        }
        let mut value = value.trim().trim_matches('"').to_owned();
        value = value.replace("${HOME}", &home.display().to_string());
        value = value.replace("$HOME", &home.display().to_string());
        if value.contains('$') {
            continue;
        }
        dirs.insert(key.to_owned(), PathBuf::from(value));
    }
    dirs
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum NamingError {
    #[error("could not reserve a collision-free output name")]
    Exhausted,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    struct FixedClock(&'static str);

    impl Clock for FixedClock {
        fn formatted_stamp(&self, _pattern: &str) -> String {
            self.0.to_owned()
        }
    }

    fn namer() -> OutputNamer {
        OutputNamer {
            directory: PathBuf::from("/home/alice/Videos/Screenrecordings"),
            filename_pattern: "%Y-%m-%d_%H-%M-%S".to_owned(),
            container: Container::Mp4,
        }
    }

    #[test]
    fn preview_is_advisory_and_does_not_need_reservation() {
        let plan = namer().preview(&FixedClock("2026-08-13_23-00-00"));
        assert!(plan.advisory);
        assert_eq!(
            plan.final_output,
            PathBuf::from("/home/alice/Videos/Screenrecordings/2026-08-13_23-00-00.mp4")
        );
    }

    #[test]
    fn reserve_skips_occupied_final_names() {
        let session = SessionId::from_uuid(uuid::Uuid::nil());
        let mut occupied = BTreeSet::new();
        occupied.insert(PathBuf::from(
            "/home/alice/Videos/Screenrecordings/2026-08-13_23-00-00.mp4",
        ));
        let plan = namer()
            .reserve(&FixedClock("2026-08-13_23-00-00"), session, &occupied)
            .unwrap();
        assert!(!plan.advisory);
        assert_eq!(
            plan.final_output,
            PathBuf::from("/home/alice/Videos/Screenrecordings/2026-08-13_23-00-00-1.mp4")
        );
        assert!(
            plan.staging_output
                .ends_with(".omarec-00000000-0000-0000-0000-000000000000.part.mp4")
        );
    }

    #[test]
    fn parse_user_dirs_expands_only_home() {
        let source = r#"
# comment
XDG_VIDEOS_DIR="$HOME/Videos"
XDG_PICTURES_DIR="${HOME}/Pictures"
XDG_DOWNLOAD_DIR="$UNSAFE/Downloads"
OMARCHY_SCREENRECORD_DIR="$HOME/ignored"
"#;
        let dirs = parse_user_dirs(source, Path::new("/home/alice"));
        assert_eq!(
            dirs.get("XDG_VIDEOS_DIR").map(PathBuf::as_path),
            Some(Path::new("/home/alice/Videos"))
        );
        assert_eq!(
            dirs.get("XDG_PICTURES_DIR").map(PathBuf::as_path),
            Some(Path::new("/home/alice/Pictures"))
        );
        assert!(!dirs.contains_key("XDG_DOWNLOAD_DIR"));
        assert!(!dirs.contains_key("OMARCHY_SCREENRECORD_DIR"));
    }
}
