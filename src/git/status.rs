use super::GitError;
use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};

#[derive(Debug, Clone)]
pub struct StatusEntry {
    pub code: [u8; 2],
    pub path: PathBuf,
    pub original_path: Option<PathBuf>,
}
pub fn parse(raw: &[u8]) -> Result<Vec<StatusEntry>, GitError> {
    let mut records = raw.split(|b| *b == 0).filter(|r| !r.is_empty());
    let mut entries = vec![];
    while let Some(record) = records.next() {
        if record.len() < 4 || record[2] != b' ' {
            return Err(GitError::Invalid("invalid Git status record".into()));
        }
        let code = [record[0], record[1]];
        let path = PathBuf::from(OsString::from_vec(record[3..].to_vec()));
        let original_path = if code.contains(&b'R') || code.contains(&b'C') {
            Some(PathBuf::from(OsString::from_vec(
                records
                    .next()
                    .ok_or_else(|| GitError::Invalid("missing rename path".into()))?
                    .to_vec(),
            )))
        } else {
            None
        };
        entries.push(StatusEntry {
            code,
            path,
            original_path,
        });
    }
    Ok(entries)
}
