//! Limited literal reference discovery, never shell interpretation.
//! Recognized references must be collected successfully or Bash fails closed.

use std::fs::File;
use std::io::Read;
use std::path::Path;
#[cfg(unix)]
use std::{
    ffi::CString,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::ffi::OsStrExt,
};

use crate::clients::judge::ScriptEvidence;
use crate::clients::tools::{ToolContext, validate_path_in_cwd};

const MAX_SCRIPT_BYTES: u64 = 32 * 1024;

pub(super) fn collect(
    context: &ToolContext,
    command: &str,
) -> Result<Option<ScriptEvidence>, String> {
    let Some(reference) = script_reference(command) else {
        return Ok(None);
    };
    let candidate = context.cwd.join(&reference);
    let candidate = candidate.to_str().ok_or("script path is not UTF-8")?;
    let path = validate_path_in_cwd(context, candidate)?;
    // A UTF-8 symlink name can resolve to a non-UTF-8 canonical path.
    let path_text = path
        .to_str()
        .ok_or("canonical script path is not UTF-8")?
        .to_owned();
    let contents = read_bounded(&path)
        .map_err(|detail| format!("script {}: {detail}", serde_json::json!(reference)))?;
    Ok(Some(ScriptEvidence {
        path: path_text,
        contents,
    }))
}

fn read_bounded(path: &Path) -> Result<String, String> {
    let file = open_without_symlinks(path)
        .map_err(|error| format!("script could not be opened: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("script metadata unavailable: {error}"))?;
    if !metadata.is_file() || metadata.len() > MAX_SCRIPT_BYTES {
        return Err(format!(
            "script must be a regular file of at most {MAX_SCRIPT_BYTES} bytes"
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_SCRIPT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("script could not be read: {error}"))?;
    if bytes.len() as u64 > MAX_SCRIPT_BYTES || bytes.contains(&0) {
        return Err(format!(
            "script exceeds {MAX_SCRIPT_BYTES} bytes or contains binary data"
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| format!("script must contain UTF-8 text: {}", error.utf8_error()))
}

/// Walk the canonical absolute path using directory handles so a concurrent
/// ancestor symlink replacement cannot redirect the read outside its grant.
#[cfg(unix)]
fn open_without_symlinks(path: &Path) -> std::io::Result<File> {
    let mut directory = File::open("/")?;
    let mut components = path
        .strip_prefix("/")
        .map_err(std::io::Error::other)?
        .components()
        .peekable();
    while let Some(component) = components.next() {
        let std::path::Component::Normal(name) = component else {
            return Err(std::io::Error::other("script path must be canonical"));
        };
        let name = CString::new(name.as_bytes()).map_err(std::io::Error::other)?;
        let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
        if components.peek().is_some() {
            flags |= libc::O_DIRECTORY;
        }
        // SAFETY: directory owns a live fd; name is a NUL-terminated C string.
        // No creation flag is used, so openat does not require a mode argument.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: successful openat returned a new, uniquely owned descriptor.
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok(directory)
}

#[cfg(not(unix))]
fn open_without_symlinks(_path: &Path) -> std::io::Result<File> {
    Err(std::io::Error::other(
        "script observations require a supported Unix platform",
    ))
}

fn script_reference(command: &str) -> Option<String> {
    let words = literal_words(command)?;
    let interpreter = words.first()?.as_str();
    let interpreter = interpreter
        .strip_prefix("/bin/")
        .or_else(|| interpreter.strip_prefix("/usr/bin/"))
        .unwrap_or(interpreter);
    if !matches!(
        interpreter,
        "bash" | "sh" | "zsh" | "dash" | "ksh" | "ksh93" | "ash" | "mksh" | "pdksh"
    ) {
        return None;
    }
    let index = if words.get(1)? == "--" { 2 } else { 1 };
    let path = words.get(index)?;
    if path.is_empty() || path.starts_with(['-', '+']) {
        return None;
    }
    Some(path.clone())
}

/// Only plain literal words and whole single/double-quoted words. Reject shell
/// operators/expansions even in quotes rather than guessing execution semantics.
fn literal_words(command: &str) -> Option<Vec<String>> {
    if command.contains([
        '$', '`', '\\', '\n', '\r', ';', '|', '&', '<', '>', '(', ')', '{', '}', '*', '?', '[',
        ']', '~', '#', '\0',
    ]) {
        return None;
    }
    let mut chars = command.chars().peekable();
    let mut words = Vec::new();
    while let Some(first) = chars.next() {
        if matches!(first, ' ' | '\t') {
            continue;
        }
        words.push(literal_word(first, &mut chars)?);
    }
    Some(words)
}

fn literal_word(
    first: char,
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Option<String> {
    let mut word = String::new();
    if matches!(first, '\'' | '"') {
        loop {
            let ch = chars.next()?;
            if ch == first {
                break;
            }
            word.push(ch);
        }
        if chars.peek().is_some_and(|ch| !matches!(ch, ' ' | '\t')) {
            return None;
        }
    } else {
        word.push(first);
        while chars.peek().is_some_and(|ch| !matches!(ch, ' ' | '\t')) {
            let ch = chars.next()?;
            if matches!(ch, '\'' | '"') {
                return None;
            }
            word.push(ch);
        }
    }
    Some(word)
}

#[cfg(test)]
#[path = "script_evidence_tests.rs"]
mod tests;
