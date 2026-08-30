use anyhow::{Context, bail};
use tmux_agent_protocol::v1;

pub(crate) fn parse_porcelain_v2_z(input: &[u8]) -> anyhow::Result<Vec<v1::GitStatusEntry>> {
    let mut records = input.split(|byte| *byte == 0).peekable();
    let mut entries = Vec::new();
    while let Some(record) = records.next() {
        if record.is_empty() || record.starts_with(b"# ") {
            continue;
        }
        match record[0] {
            b'1' => entries.push(entry_from_ordinary(
                &split_fields(record, 9)?,
                Vec::new(),
                String::new(),
            )?),
            b'2' => {
                let fields = split_fields(record, 10)?;
                let original = records
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("rename record omitted original path"))?
                    .to_vec();
                entries.push(entry_from_ordinary(
                    &fields,
                    original,
                    String::from_utf8_lossy(fields[8]).into_owned(),
                )?);
            }
            b'u' => entries.push(unmerged_entry(&split_fields(record, 11)?)?),
            b'?' | b'!' => entries.push(untracked_entry(record)?),
            other => bail!(
                "unsupported porcelain-v2 record type {:?}",
                char::from(other)
            ),
        }
    }
    Ok(entries)
}

fn split_fields(record: &[u8], count: usize) -> anyhow::Result<Vec<&[u8]>> {
    let fields: Vec<_> = record.splitn(count, |byte| *byte == b' ').collect();
    if fields.len() != count {
        bail!("malformed porcelain-v2 record");
    }
    Ok(fields)
}

fn unmerged_entry(fields: &[&[u8]]) -> anyhow::Result<v1::GitStatusEntry> {
    let path = fields[10].to_vec();
    let xy = fields[1];
    if xy.len() != 2 {
        bail!("malformed unmerged XY status");
    }
    Ok(v1::GitStatusEntry {
        path,
        index_kind: v1::GitChangeKind::Unmerged.into(),
        worktree_kind: v1::GitChangeKind::Unmerged.into(),
        index_status: String::from_utf8_lossy(&xy[..1]).into_owned(),
        worktree_status: String::from_utf8_lossy(&xy[1..]).into_owned(),
        head_mode: parse_mode(fields[3])?,
        index_mode: parse_mode(fields[4])?,
        worktree_mode: parse_mode(fields[6])?,
        conflicted: true,
        conflict_code: String::from_utf8_lossy(xy).into_owned(),
        submodule: fields[2] != b"N...",
        submodule_state: String::from_utf8_lossy(fields[2]).into_owned(),
        ..Default::default()
    })
}

fn untracked_entry(record: &[u8]) -> anyhow::Result<v1::GitStatusEntry> {
    if record.len() < 3 || record[1] != b' ' {
        bail!("malformed untracked/ignored record");
    }
    let mut path = record[2..].to_vec();
    let ignored = record[0] == b'!';
    // With `--ignored=matching`, Git appends a slash to ignored directory
    // records. The slash is a status presentation marker rather than part of
    // the repository-relative path. Remove it here so every path that leaves
    // the parser has the same canonical spelling required by mutations and
    // the worktree capability checks.
    if ignored && path.len() > 1 && path.last() == Some(&b'/') {
        path.pop();
    }
    Ok(v1::GitStatusEntry {
        path,
        untracked: !ignored,
        ignored,
        worktree_kind: if ignored {
            v1::GitChangeKind::Ignored
        } else {
            v1::GitChangeKind::Untracked
        }
        .into(),
        worktree_status: if ignored { "!" } else { "?" }.into(),
        ..Default::default()
    })
}

fn entry_from_ordinary(
    fields: &[&[u8]],
    original_path: Vec<u8>,
    rename_score: String,
) -> anyhow::Result<v1::GitStatusEntry> {
    let xy = fields[1];
    if xy.len() != 2 {
        bail!("malformed XY status");
    }
    let path = fields.last().unwrap().to_vec();
    Ok(v1::GitStatusEntry {
        path,
        original_path,
        index_kind: change_kind(xy[0]).into(),
        worktree_kind: change_kind(xy[1]).into(),
        index_status: char::from(xy[0]).to_string(),
        worktree_status: char::from(xy[1]).to_string(),
        head_mode: parse_mode(fields[3])?,
        index_mode: parse_mode(fields[4])?,
        worktree_mode: parse_mode(fields[5])?,
        submodule: fields[2] != b"N...",
        submodule_state: String::from_utf8_lossy(fields[2]).into_owned(),
        rename_score,
        ..Default::default()
    })
}

fn change_kind(value: u8) -> v1::GitChangeKind {
    match value {
        b'M' => v1::GitChangeKind::Modified,
        b'A' => v1::GitChangeKind::Added,
        b'D' => v1::GitChangeKind::Deleted,
        b'R' => v1::GitChangeKind::Renamed,
        b'C' => v1::GitChangeKind::Copied,
        b'T' => v1::GitChangeKind::TypeChanged,
        b'U' => v1::GitChangeKind::Unmerged,
        _ => v1::GitChangeKind::Unspecified,
    }
}

fn parse_mode(value: &[u8]) -> anyhow::Result<u32> {
    u32::from_str_radix(std::str::from_utf8(value)?, 8).context("invalid Git mode")
}
