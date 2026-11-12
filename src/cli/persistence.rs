//! Execute session persistence plans while retaining one locked file handle.

use std::{fs, fs::File, path::Path};

use uuid::Uuid;

use crate::cli::SessionPersistencePlan;
use crate::config::{DataDir, Session};
use crate::types::SessionRecord;

/// Execute one persistence plan and return its locked append handle.
///
/// `None` is the complete no-file plan for an ephemeral run. A create plan
/// writes metadata and every seed record before returning the same locked
/// handle for live session writes.
pub fn execute_persistence_plan(
    plan: Option<SessionPersistencePlan>,
    data_dir: &DataDir,
    session: &Session,
    tools: Vec<String>,
) -> anyhow::Result<Option<File>> {
    let Some(plan) = plan else {
        return Ok(None);
    };

    let file = match plan {
        SessionPersistencePlan::Create {
            seed_records,
            blob_source,
        } => create_seeded_session_file(
            data_dir,
            session,
            SeedRequest {
                tools,
                seed_records: &seed_records,
                blob_source,
            },
            Session::append_records,
        )?,
        SessionPersistencePlan::Append => data_dir.open_session_for_append(session.id)?,
    };

    Ok(Some(file))
}

struct SeedRequest<'a> {
    tools: Vec<String>,
    seed_records: &'a [SessionRecord],
    blob_source: Option<Uuid>,
}

/// Create and seed a session under one lock.
///
/// A fork links the parent's referenced blobs into the child and externalizes
/// any legacy inline seed images before the child file exists. A failure after
/// provisioning begins removes the incomplete file and any blob directory this
/// fork created. If cleanup also fails, the returned error reports both
/// failures.
fn create_seeded_session_file(
    data_dir: &DataDir,
    session: &Session,
    request: SeedRequest<'_>,
    append_seed_records: impl FnOnce(&mut File, &[SessionRecord]) -> anyhow::Result<()>,
) -> anyhow::Result<File> {
    let SeedRequest {
        tools,
        seed_records,
        blob_source,
    } = request;
    if seed_records
        .iter()
        .any(|record| matches!(record, SessionRecord::SessionMeta { .. }))
    {
        anyhow::bail!("Cannot seed a new session with a session_meta record");
    }

    let blob_dir = blob_source.map(|_| data_dir.session_blob_dir(session.id));
    // Capture this before linking; only a directory this fork creates is
    // removed on failure. A pre-existing directory may hold another writer's
    // data and is left in place.
    let created_blob_dir = blob_dir.as_deref().filter(|path| {
        matches!(
            fs::symlink_metadata(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        )
    });
    let prepared_records = blob_source.map_or_else(
        || Ok(None),
        |source_id| {
            data_dir
                .session_blob_store(session.id)
                .prepare_fork_seed_records(&data_dir.session_blob_store(source_id), seed_records)
                .map(Some)
        },
    );
    let prepared_records = match prepared_records {
        Ok(prepared_records) => prepared_records,
        Err(error) => {
            let error = error.context(format!(
                "Failed to prepare image blobs for new session {}",
                session.id
            ));
            return Err(attach_cleanup_errors(
                error,
                cleanup_created_blob_dir(created_blob_dir),
            ));
        },
    };
    let seed_records = prepared_records.as_deref().unwrap_or(seed_records);

    let path = data_dir.session_path(session.id);
    let path_was_absent = matches!(
        fs::symlink_metadata(&path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    );
    let mut file = match data_dir.create_session_file(session, tools) {
        Ok(file) => file,
        Err(error) => {
            let error = error.context(format!("Failed to create new session {}", session.id));
            let cleanup_errors = if path_was_absent {
                cleanup_incomplete_create(&path, created_blob_dir)
            } else {
                cleanup_created_blob_dir(created_blob_dir)
            };
            return Err(attach_cleanup_errors(error, cleanup_errors));
        },
    };

    if let Err(error) = append_seed_records(&mut file, seed_records) {
        let error = error.context(format!("Failed to seed new session {}", session.id));
        drop(file);
        return Err(attach_cleanup_errors(
            error,
            cleanup_incomplete_create(&path, created_blob_dir),
        ));
    }

    Ok(file)
}

fn cleanup_incomplete_create(path: &Path, created_blob_dir: Option<&Path>) -> Vec<String> {
    let mut errors = Vec::new();
    if let Err(error) = fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        errors.push(format!(
            "Failed to remove incomplete session file {}: {error}",
            path.display()
        ));
    }
    errors.extend(cleanup_created_blob_dir(created_blob_dir));
    errors
}

fn cleanup_created_blob_dir(created_blob_dir: Option<&Path>) -> Vec<String> {
    let Some(blob_dir) = created_blob_dir else {
        return Vec::new();
    };
    match fs::remove_dir_all(blob_dir) {
        Ok(()) => Vec::new(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => vec![format!(
            "Failed to remove incomplete blob directory {}: {error}",
            blob_dir.display()
        )],
    }
}

fn attach_cleanup_errors(mut error: anyhow::Error, cleanup_errors: Vec<String>) -> anyhow::Error {
    for cleanup_error in cleanup_errors {
        error = error.context(cleanup_error);
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Role;
    use crate::types::session::MessageData;

    fn test_session(working_dir: &std::path::Path) -> Session {
        let mut session = Session::new(uuid::Uuid::new_v4(), working_dir.to_path_buf());
        session.model = Some("test-model".to_string());
        session
    }

    fn seed_record(name: &str) -> SessionRecord {
        SessionRecord::SkillActivated {
            session_id: uuid::Uuid::new_v4().to_string(),
            task_id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            name: name.to_string(),
            path: std::path::PathBuf::from(format!("/skills/{name}/SKILL.md")),
        }
    }

    #[test]
    fn create_writes_one_metadata_record_and_ordered_seeds_under_one_lock() {
        let root = tempfile::tempdir().expect("temp data dir");
        let data_dir = DataDir::new_in_dir(root.path());
        let session = test_session(root.path());
        let path = data_dir.session_path(session.id);
        let plan = SessionPersistencePlan::Create {
            seed_records: vec![seed_record("first"), seed_record("second")],
            blob_source: None,
        };

        let file = execute_persistence_plan(Some(plan), &data_dir, &session, Vec::new())
            .expect("create plan should succeed")
            .expect("create plan should return a file");

        let loaded = Session::load(&path).expect("created session should load");
        assert_eq!(loaded.records.len(), 3);
        assert_eq!(
            loaded
                .records
                .iter()
                .filter(|record| matches!(record, SessionRecord::SessionMeta { .. }))
                .count(),
            1
        );
        assert!(matches!(
            &loaded.records[1],
            SessionRecord::SkillActivated { name, .. } if name == "first"
        ));
        assert!(matches!(
            &loaded.records[2],
            SessionRecord::SkillActivated { name, .. } if name == "second"
        ));

        let lock_error = Session::open_for_append(&path)
            .expect_err("returned create handle should retain the lock");
        assert!(lock_error.to_string().contains("Another cake invocation"));

        drop(file);
        Session::open_for_append(&path).expect("dropping the handle should release the lock");
    }

    #[test]
    fn append_reuses_existing_metadata_and_returns_locked_handle() {
        let root = tempfile::tempdir().expect("temp data dir");
        let data_dir = DataDir::new_in_dir(root.path());
        let session = test_session(root.path());
        let path = data_dir.session_path(session.id);
        drop(
            data_dir
                .create_session_file(&session, Vec::new())
                .expect("session fixture should be created"),
        );

        let mut file = execute_persistence_plan(
            Some(SessionPersistencePlan::Append),
            &data_dir,
            &session,
            Vec::new(),
        )
        .expect("append plan should succeed")
        .expect("append plan should return a file");
        Session::append_record(
            &mut file,
            &SessionRecord::Message(MessageData {
                role: Role::User,
                content: "continued".to_string(),
                id: None,
                status: None,
                timestamp: None,
                images: Vec::new(),
            }),
        )
        .expect("append handle should remain writable");

        let loaded = Session::load(&path).expect("appended session should load");
        assert_eq!(loaded.records.len(), 2);
        assert!(matches!(
            loaded.records.as_slice(),
            [SessionRecord::SessionMeta { .. }, SessionRecord::Message(message)]
                if message.content == "continued"
        ));
        assert!(Session::open_for_append(&path).is_err());
    }

    #[test]
    fn no_plan_creates_no_session_file() {
        let root = tempfile::tempdir().expect("temp data dir");
        let data_dir = DataDir::new_in_dir(root.path());
        let session = test_session(root.path());

        let file = execute_persistence_plan(None, &data_dir, &session, Vec::new())
            .expect("no-file plan should succeed");

        assert!(file.is_none());
        assert!(!data_dir.session_path(session.id).exists());
    }

    #[test]
    fn seed_write_failure_removes_incomplete_file_after_releasing_lock() {
        let root = tempfile::tempdir().expect("temp data dir");
        let data_dir = DataDir::new_in_dir(root.path());
        let session = test_session(root.path());
        let path = data_dir.session_path(session.id);
        let seeds = vec![seed_record("unwritten")];

        let error = create_seeded_session_file(
            &data_dir,
            &session,
            SeedRequest {
                tools: Vec::new(),
                seed_records: &seeds,
                blob_source: None,
            },
            |file, records| {
                assert!(path.exists(), "metadata file should exist before seeding");
                assert!(
                    Session::open_for_append(&path).is_err(),
                    "create lock should remain held while seeds are written"
                );
                Session::append_record(file, &records[0])?;
                Err(anyhow::anyhow!("forced seed failure"))
            },
        )
        .expect_err("seed failure should abort create");

        assert!(error.to_string().contains("Failed to seed new session"));
        assert!(format!("{error:#}").contains("forced seed failure"));
        assert!(
            !path.exists(),
            "failed create should remove its partial file"
        );
    }

    #[test]
    fn create_rejects_metadata_in_seed_records_before_creating_a_file() {
        let root = tempfile::tempdir().expect("temp data dir");
        let data_dir = DataDir::new_in_dir(root.path());
        let source = test_session(root.path());
        let source_path = data_dir.session_path(source.id);
        drop(
            data_dir
                .create_session_file(&source, Vec::new())
                .expect("source session should be created"),
        );
        let source_meta = Session::load(&source_path)
            .expect("source session should load")
            .records
            .remove(0);

        let target = test_session(root.path());
        let target_path = data_dir.session_path(target.id);
        let plan = SessionPersistencePlan::Create {
            seed_records: vec![source_meta],
            blob_source: None,
        };
        let error = execute_persistence_plan(Some(plan), &data_dir, &target, Vec::new())
            .expect_err("metadata seed should be rejected");

        assert!(error.to_string().contains("session_meta"));
        assert!(!target_path.exists());
    }
}
