//! Execute session persistence plans while retaining one locked file handle.

use std::{fs, fs::File, path::Path};

use anyhow::Context;
use uuid::Uuid;

use crate::cli::SessionPersistencePlan;
use crate::config::blob_store::{RecoveryReport, referenced_content_ids};
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
        SessionPersistencePlan::Append => {
            let file = data_dir.open_session_for_append(session.id)?;
            recover_append_blobs(data_dir, &file, session.id);
            file
        },
    };

    Ok(Some(file))
}

/// Remove unreferenced payload blobs after an append lock and tail repair.
///
/// Recovery is best-effort: a missing blob directory skips the scan, and a
/// read, parse, or removal error is reported without failing the resume.
fn recover_append_blobs(data_dir: &DataDir, file: &File, session_id: Uuid) {
    match try_recover_append_blobs(data_dir, file, session_id) {
        Ok(Some(report)) => log_recovery_report(&report, session_id),
        Ok(None) => {},
        Err(error) => tracing::warn!(
            target: "cake",
            "Skipped payload blob recovery for session {}: {error:#}",
            session_id
        ),
    }
}

fn try_recover_append_blobs(
    data_dir: &DataDir,
    file: &File,
    session_id: Uuid,
) -> anyhow::Result<Option<RecoveryReport>> {
    let blob_dir = data_dir.session_blob_dir(session_id);
    match fs::symlink_metadata(&blob_dir) {
        Ok(_) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).context(format!(
                "failed to inspect blob directory {}",
                blob_dir.display()
            ));
        },
    }

    let session_path = data_dir.session_path(session_id);
    let session = Session::load_from_append_handle(file, &session_path)
        .context("failed to read session references")?;
    let referenced = referenced_content_ids(&session.records)
        .context("failed to collect session payload references")?;
    data_dir
        .session_blob_store(session_id)
        .recover_unreferenced(&session_id.to_string(), &referenced)
        .map(Some)
}

fn log_recovery_report(report: &RecoveryReport, session_id: Uuid) {
    for error in &report.errors {
        tracing::warn!(target: "cake", "{error}");
    }
    if !report.removed_blobs.is_empty() || !report.removed_temporary_files.is_empty() {
        tracing::info!(
            target: "cake",
            "Recovered {} unreferenced blob(s) and {} temporary file(s) for session {}",
            report.removed_blobs.len(),
            report.removed_temporary_files.len(),
            session_id
        );
    }
}

struct SeedRequest<'a> {
    tools: Vec<String>,
    seed_records: &'a [SessionRecord],
    blob_source: Option<Uuid>,
}

/// Create and seed a session under one lock.
///
/// A fork links the parent's referenced blobs into the child and externalizes
/// any legacy inline seed payloads before the child file exists. A failure after
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
                "Failed to prepare payload blobs for new session {}",
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
