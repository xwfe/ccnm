//! `session.result`'s `output`: the Agent's retained view of a stream, read
//! a page at a time (P59).
//!
//! Before P59 the only thing kept was the last 2 KiB the Agent put in its
//! report, re-cut to 8 KiB here and reported as `truncated: false` with its
//! own length as `bytes_total` -- a 3 MiB log looked like a complete 2 KiB
//! one (P57 C0/C1). Now:
//!
//! 1. The first read after a session ended copies the Agent's view of the
//!    stream here, a slice per ssh call, into `rpc/outputs/<handle>/`. The
//!    view is already redacted, valid UTF-8, capped and frozen on the Agent
//!    (see `session::view`), so every page cut from the copy is safe to send
//!    and every re-read gives the same bytes.
//! 2. Pages are cut from the end backwards, never inside a character, never
//!    over `max_bytes`, with memory bounded by one page.
//! 3. A cursor is a random name for "the part before byte N of this view"
//!    kept in this process only. Forged, another session's, another
//!    stream's, or from before a restart: it is not in the table, so it is
//!    `expired` -- nothing a caller sends is ever turned into an offset.
//! 4. When the copy cannot be made, the old tail kept in the record is paged
//!    instead, and `unavailable_reason` says so. It is never passed off as
//!    the whole output.

use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::Context;
use super::session::OutputAsk;
use super::store::{Finish, Record, State};
use super::wire::{RpcError, code};
use crate::error::{Error, ErrorCode, Result as CcnmResult};
use crate::session::view::{RAW_CAP, Stream};

/// A page when the caller does not say: the old fixed tail size.
pub const DEFAULT_MAX_BYTES: u64 = 8192;
/// The most one page carries, whatever `max_bytes` says.
pub const PAGE_MAX: u64 = 1024 * 1024;
/// One ssh round trip's worth when copying a view here.
const FETCH_CHUNK: u64 = 1024 * 1024;
/// Largest view the Agent can produce: `RAW_CAP` bytes of stream, each
/// invalid byte of which may become the 3-byte U+FFFD. Anything larger is
/// not a view.
const VIEW_MAX: u64 = 3 * RAW_CAP;
/// Cursors remembered by one `ccnm rpc` process. The oldest go first; a
/// caller holding one of those starts over from `cursor: null`.
const CURSORS_KEPT: usize = 1024;

/// What the caller asked for in `output`.
pub struct Params {
    pub stream: Stream,
    pub max_bytes: u64,
    pub cursor: Option<String>,
}

pub fn params(output: Option<&Value>) -> Result<Params, RpcError> {
    let mut params = Params {
        stream: Stream::Stdout,
        max_bytes: DEFAULT_MAX_BYTES,
        cursor: None,
    };
    let Some(output) = output else {
        return Ok(params);
    };
    let output = output
        .as_object()
        .ok_or_else(|| RpcError::refused(code::INVALID_PARAMS, "output must be an object"))?;
    super::reject_unknown(output, &["max_bytes", "cursor", "stream"])?;
    if let Some(value) = output.get("max_bytes") {
        params.max_bytes = value.as_u64().filter(|n| *n >= 1).ok_or_else(|| {
            RpcError::refused(
                code::INVALID_PARAMS,
                "max_bytes must be an integer of at least 1",
            )
        })?;
    }
    match output.get("cursor") {
        None | Some(Value::Null) => {}
        Some(Value::String(cursor)) => params.cursor = Some(cursor.clone()),
        Some(_) => {
            return Err(RpcError::refused(
                code::INVALID_PARAMS,
                "cursor must be a string or null",
            ));
        }
    }
    if let Some(value) = output.get("stream") {
        params.stream = match value.as_str() {
            Some("stdout") => Stream::Stdout,
            Some("stderr") => Stream::Stderr,
            _ => {
                return Err(RpcError::refused(
                    code::INVALID_PARAMS,
                    "stream must be \"stdout\" or \"stderr\"",
                ));
            }
        };
    }
    Ok(params)
}

/// Pages handed out by this process: "before byte `end` of this view".
#[derive(Default)]
pub struct Cursors(Mutex<VecDeque<(String, Mark)>>);

#[derive(Clone, PartialEq, Eq)]
struct Mark {
    session: String,
    stream: Stream,
    generation: String,
    end: u64,
}

impl Cursors {
    fn issue(&self, mark: Mark) -> String {
        let mut table = self.0.lock().unwrap_or_else(|e| e.into_inner());
        // Re-reading a page hands back the cursor it had the first time:
        // the same answer twice, and a table that does not grow with every
        // poll.
        if let Some((name, _)) = table.iter().find(|(_, m)| *m == mark) {
            return name.clone();
        }
        let name = format!("c-{}", uuid::Uuid::new_v4().simple());
        if table.len() >= CURSORS_KEPT {
            table.pop_front();
        }
        table.push_back((name.clone(), mark));
        name
    }

    fn find(&self, name: &str) -> Option<Mark> {
        let table = self.0.lock().unwrap_or_else(|e| e.into_inner());
        table
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, m)| m.clone())
    }
}

/// The view a page is cut from.
struct View {
    generation: String,
    source: Source,
    source_bytes: Option<u64>,
    source_truncated: Option<bool>,
    unavailable: Option<&'static str>,
}

enum Source {
    File(PathBuf, u64),
    Memory(Vec<u8>),
}

impl Source {
    fn len(&self) -> u64 {
        match self {
            Source::File(_, len) => *len,
            Source::Memory(bytes) => bytes.len() as u64,
        }
    }

    fn read(&self, start: u64, end: u64) -> CcnmResult<Vec<u8>> {
        match self {
            Source::Memory(bytes) => Ok(bytes[start as usize..end as usize].to_vec()),
            Source::File(path, _) => {
                let mut file = fs::File::open(path)?;
                file.seek(SeekFrom::Start(start))?;
                let mut data = Vec::with_capacity((end - start) as usize);
                file.take(end - start).read_to_end(&mut data)?;
                if data.len() as u64 != end - start {
                    return Err(Error::internal(
                        "the output snapshot is shorter than its record",
                    ));
                }
                Ok(data)
            }
        }
    }
}

fn is_continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

/// `session.result`'s `output` for a session that has ended.
pub fn page(
    ctx: &Context,
    record: &Record,
    finish: &Finish,
    asked: &Params,
) -> Result<Value, RpcError> {
    let view = view_of(ctx, record, finish, asked.stream);
    let internal = |e: Error| super::wire::from_ccnm(&e);
    let total = view.source.len();
    let end = match &asked.cursor {
        None => total,
        Some(name) => {
            let mark = ctx.cursors.find(name).filter(|mark| {
                mark.session == record.session
                    && mark.stream == asked.stream
                    && mark.generation == view.generation
                    && mark.end <= total
            });
            match mark {
                Some(mark) => mark.end,
                None => {
                    return Err(RpcError::refused(
                        code::EXPIRED,
                        "output cursor is no longer valid",
                    )
                    .with_reason("cursor_expired")
                    .with_session(&record.session));
                }
            }
        }
    };
    let budget = asked.max_bytes.min(PAGE_MAX);
    // Walk forward off any continuation bytes: the page starts on a
    // character. At most three steps in valid UTF-8.
    let mut start = end.saturating_sub(budget);
    if start > 0 {
        let probe = view
            .source
            .read(start, (start + 3).min(end))
            .map_err(internal)?;
        start += probe.iter().take_while(|b| is_continuation(**b)).count() as u64;
    }
    if start >= end && end > 0 {
        // Not even one character fits. An empty page with the same cursor
        // would let a caller loop forever; say how much is needed instead.
        let back = view
            .source
            .read(end.saturating_sub(4), end)
            .map_err(internal)?;
        let lead = back.iter().rposition(|b| !is_continuation(*b)).unwrap_or(0);
        let need = back.len() - lead;
        let mut refused = RpcError::refused(
            code::INVALID_PARAMS,
            "max_bytes is smaller than the next character",
        )
        .with_reason("max_bytes_too_small")
        .with_session(&record.session);
        refused.data.min_bytes = Some(need as u64);
        return Err(refused);
    }
    let bytes = view.source.read(start, end).map_err(internal)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| internal(Error::internal("the output snapshot is not valid UTF-8")))?;
    let cursor = (start > 0).then(|| {
        ctx.cursors.issue(Mark {
            session: record.session.clone(),
            stream: asked.stream,
            generation: view.generation.clone(),
            end: start,
        })
    });
    let mut out = Map::new();
    out.insert("stream".into(), Value::from(asked.stream.name()));
    out.insert("bytes_total".into(), Value::from(total));
    out.insert("truncated".into(), Value::from(start > 0));
    out.insert("cursor".into(), cursor.map_or(Value::Null, Value::from));
    out.insert("tail".into(), Value::from(text));
    if let Some(n) = view.source_bytes {
        out.insert("source_bytes".into(), Value::from(n));
    }
    if let Some(lost) = view.source_truncated {
        out.insert("source_truncated".into(), Value::from(lost));
    }
    if let Some(reason) = view.unavailable {
        out.insert("unavailable_reason".into(), Value::from(reason));
    }
    Ok(Value::Object(out))
}

/// Where the page comes from: the snapshot of the Agent's view when the
/// Agent ran something under this record's id, the record's old tail when
/// that is all there is.
fn view_of(ctx: &Context, record: &Record, finish: &Finish, stream: Stream) -> View {
    let ran_here = record.managed_session.is_some()
        && (finish.ccnm_session == record.managed_session
            // Sent, and no answer came back: whatever the Agent kept is
            // still worth showing next to `unknown`.
            || (record.dispatched && finish.ccnm_session.is_none() && record.state == State::Unknown));
    let old_tail = || match stream {
        Stream::Stdout => finish.output.clone().into_bytes(),
        Stream::Stderr => finish.stderr.clone().into_bytes(),
    };
    if !ran_here {
        return View {
            // One per record: a cursor into it stays valid until the
            // process ends, and the text never changes.
            generation: format!("record-{}", record.session),
            source: Source::Memory(old_tail()),
            source_bytes: None,
            source_truncated: None,
            // A record from before P58 kept only this. One without a run on
            // the Agent (stopped before it was sent, refused before it
            // started) really had nothing more.
            unavailable: record.managed_session.is_none().then_some("legacy_tail"),
        };
    }
    match snapshot(ctx, record, stream) {
        Ok((meta, path)) => View {
            source: Source::File(path, meta.view_bytes),
            generation: meta.generation,
            source_bytes: Some(meta.source_bytes),
            source_truncated: Some(meta.source_truncated),
            unavailable: None,
        },
        Err(err) => {
            tracing::warn!(session = %record.session, %err, "cannot copy the retained output; paging the old tail");
            View {
                generation: format!("record-{}", record.session),
                source: Source::Memory(old_tail()),
                source_bytes: None,
                source_truncated: None,
                unavailable: Some(if err.code() == ErrorCode::AgentUnreachable {
                    "agent_unreachable"
                } else {
                    "agent_refused"
                }),
            }
        }
    }
}

/// What a snapshot here is, written after the copy is complete.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotMeta {
    generation: String,
    view_bytes: u64,
    source_bytes: u64,
    source_truncated: bool,
}

/// The local copy of the Agent's view, made now if there is none.
fn snapshot(ctx: &Context, record: &Record, stream: Stream) -> CcnmResult<(SnapshotMeta, PathBuf)> {
    let dir = super::store::Store::open(&ctx.state)?.output_dir(&record.session)?;
    let view_path = dir.join(format!("{}.view", stream.name()));
    let meta_path = dir.join(format!("{}.json", stream.name()));
    if let Some(meta) = fs::read(&meta_path)
        .ok()
        .and_then(|b| serde_json::from_slice::<SnapshotMeta>(&b).ok())
        && fs::metadata(&view_path).is_ok_and(|m| m.len() == meta.view_bytes)
    {
        return Ok((meta, view_path));
    }
    let agent = record
        .launch
        .agent
        .clone()
        .ok_or_else(|| Error::internal("recorded launches bind an instance"))?;
    let ask = |offset| OutputAsk {
        workspace: record.launch.workspace.clone(),
        node: agent.node.clone(),
        instance: agent.instance.clone(),
        session: record.managed_session.clone().unwrap_or_default(),
        stream,
        offset,
        limit: FETCH_CHUNK,
    };
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        stream.name(),
        uuid::Uuid::new_v4().simple()
    ));
    let copied = (|| -> CcnmResult<SnapshotMeta> {
        use base64::Engine as _;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        let first = ctx.runs.output(&ask(0))?;
        if first.view_bytes > VIEW_MAX {
            return Err(Error::internal(
                "the Agent reported an output view larger than it can hold",
            ));
        }
        let mut offset = 0u64;
        let mut report = first.clone();
        loop {
            if report.generation != first.generation || report.view_bytes != first.view_bytes {
                return Err(Error::internal(
                    "the Agent's output view changed while it was being copied",
                ));
            }
            let data = base64::engine::general_purpose::STANDARD
                .decode(&report.data)
                .map_err(|e| {
                    Error::internal("the Agent sent output that is not base64").with_source(e)
                })?;
            if offset + data.len() as u64 > first.view_bytes
                || (data.is_empty() && offset < first.view_bytes)
            {
                return Err(Error::internal(
                    "the Agent's output slices do not add up to its view",
                ));
            }
            file.write_all(&data)?;
            offset += data.len() as u64;
            if offset == first.view_bytes {
                break;
            }
            report = ctx.runs.output(&ask(offset))?;
        }
        file.sync_all()?;
        Ok(SnapshotMeta {
            generation: first.generation,
            view_bytes: first.view_bytes,
            source_bytes: first.source_bytes,
            source_truncated: first.source_truncated,
        })
    })();
    let meta = match copied {
        Ok(meta) => meta,
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
    };
    fs::rename(&tmp, &view_path)?;
    let meta_tmp = dir.join(format!(
        ".{}.json.{}.tmp",
        stream.name(),
        uuid::Uuid::new_v4().simple()
    ));
    fs::write(
        &meta_tmp,
        serde_json::to_vec(&meta)
            .map_err(|e| Error::internal("cannot encode an output record").with_source(e))?,
    )?;
    fs::set_permissions(&meta_tmp, fs::Permissions::from_mode(0o600))?;
    fs::rename(&meta_tmp, &meta_path)?;
    Ok((meta, view_path))
}
