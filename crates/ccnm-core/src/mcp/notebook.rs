//! Jupyter notebooks, a cell at a time (P40): `read_notebook`, and the
//! `edit_notebook` op of `apply_patch`.
//!
//! # Why not `read_file`
//!
//! `read_file` already returns a notebook, as the JSON it is on disk, and a
//! model can change it with `update` edits against that text. Showing cells
//! there instead would make those edits stop matching, which under the
//! frozen contract is a change of meaning. So the cell view is a tool of its
//! own and `read_file` only points at it.
//!
//! # What the native tools do, and what is copied
//!
//! Claude Code 2.1.273 (read from its bundled code, P40 record):
//!
//! ```text
//! Read           <cell id="…"> per cell; a code cell's outputs follow it;
//!                PNG and JPEG outputs become images
//! NotebookEdit   cell_id, new_source, cell_type, edit_mode
//!                a cell is found by its id, else "cell-N" is the index N
//!                replacing a code cell empties outputs and execution_count
//!                insert goes after cell_id, or first without one
//!                nbformat >= 4.5 gives an inserted cell an 8-character id
//! ```
//!
//! All of that is kept, with two differences. A cell whose type changes
//! loses the keys the new type may not have (nbformat's schema forbids
//! `outputs` on a markdown cell; Claude Code leaves them), and a source is
//! written as a list of lines, as nbformat writes it, so a diff of the file
//! shows the lines that changed.
//!
//! # Writing it back
//!
//! nbformat writes `json.dumps(sort_keys=True, indent=1, ensure_ascii=False)`
//! and a final newline. `serde_json` here keeps object keys sorted and does
//! not escape non-ASCII, so a notebook Jupyter wrote comes back byte for
//! byte when nothing in it changed; the indent width and the final newline
//! are taken from the file. The one known difference is a float in the
//! metadata: Python writes `1e-05` where this writes `1e-5`.

use base64::Engine;
use rmcp::schemars;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::mcp::image::{Format, MAX_IMAGE_BYTES};
use crate::mcp::path;

/// Largest notebook read or edited: the size `apply_patch` edits any file
/// up to, so a notebook `read_notebook` shows is one it can change.
pub const MAX_NOTEBOOK_BYTES: u64 = crate::mcp::patch::MAX_EDIT_BYTES;

/// Text returned by one `read_notebook`, the same default `read_file` has.
const MAX_TEXT_BYTES: usize = 32 * 1024;

/// Text kept from one output. A training loop's progress bars are the
/// usual reason a single output is megabytes.
const MAX_OUTPUT_BYTES: usize = 4 * 1024;

/// Images sent by one call. Claude Code counts an image as 1600 tokens
/// against the 25000 an MCP result may use.
const MAX_IMAGES: usize = 8;

/// Arguments of `read_notebook`.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct ReadNotebookArgs {
    /// Path of an .ipynb file, relative to the workspace root.
    pub path: String,
    /// Index of the first cell to show, from 0. Default 0.
    #[serde(default)]
    pub start_cell: Option<u32>,
    /// Anything this tool does not declare: reported back, not obeyed. See
    /// [`crate::mcp::Ignored`].
    #[serde(flatten)]
    pub ignored: crate::mcp::Ignored,
}

/// One piece of a `read_notebook` result, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Text(String),
    Image { data: String, format: Format },
}

/// A parsed notebook and how its file was laid out.
struct Notebook {
    root: Map<String, Value>,
    indent: usize,
    final_newline: bool,
}

impl Notebook {
    fn parse(bytes: &[u8], rel: &str) -> Result<Notebook> {
        let text = std::str::from_utf8(bytes).map_err(|_| {
            Error::invalid_args(format!("{rel} is not valid UTF-8, so it is not a notebook"))
        })?;
        let value: Value = serde_json::from_str(text).map_err(|e| {
            Error::invalid_args(format!(
                "{rel} is not valid JSON ({e}); it may be truncated or still being written"
            ))
        })?;
        let Value::Object(root) = value else {
            return Err(not_a_notebook(rel));
        };
        if !root.get("cells").is_some_and(Value::is_array) {
            return Err(not_a_notebook(rel));
        }
        match root.get("nbformat").and_then(Value::as_u64) {
            Some(major) if major >= 4 => {}
            Some(major) => {
                return Err(Error::invalid_args(format!(
                    "{rel} is nbformat {major}; only nbformat 4 is supported. Convert it with exec_command first: jupyter nbconvert --to notebook --nbformat 4 {rel}"
                )));
            }
            None => return Err(not_a_notebook(rel)),
        }
        // The width of the first indented line; none means one line of JSON.
        let indent = text
            .split('\n')
            .nth(1)
            .map_or(0, |line| line.len() - line.trim_start_matches(' ').len());
        Ok(Notebook {
            root,
            indent,
            final_newline: text.ends_with('\n'),
        })
    }

    fn cells(&self) -> &Vec<Value> {
        self.root["cells"].as_array().expect("checked in parse")
    }

    fn cells_mut(&mut self) -> &mut Vec<Value> {
        self.root
            .get_mut("cells")
            .and_then(Value::as_array_mut)
            .expect("checked in parse")
    }

    /// nbformat 4.5 is where cells got ids.
    fn has_ids(&self) -> bool {
        let major = self
            .root
            .get("nbformat")
            .and_then(Value::as_u64)
            .unwrap_or(4);
        let minor = self
            .root
            .get("nbformat_minor")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        major > 4 || minor >= 5
    }

    fn language(&self) -> String {
        let meta = self.root.get("metadata");
        meta.and_then(|m| m.pointer("/language_info/name"))
            .or_else(|| meta.and_then(|m| m.pointer("/kernelspec/language")))
            .and_then(Value::as_str)
            .unwrap_or("python")
            .to_string()
    }

    fn to_bytes(&self) -> Vec<u8> {
        let value = Value::Object(self.root.clone());
        let mut out = Vec::new();
        if self.indent == 0 {
            serde_json::to_writer(&mut out, &value).expect("a Value always serializes");
        } else {
            let indent = vec![b' '; self.indent];
            let formatter = serde_json::ser::PrettyFormatter::with_indent(&indent);
            let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
            serde::Serialize::serialize(&value, &mut serializer)
                .expect("a Value always serializes");
        }
        if self.final_newline {
            out.push(b'\n');
        }
        out
    }
}

fn not_a_notebook(rel: &str) -> Error {
    Error::invalid_args(format!(
        "{rel} is not a Jupyter notebook (no cells array or nbformat number)"
    ))
}

/// The id a cell is addressed by: its own, or `cell-N` for a notebook from
/// before cells had ids -- the same fallback Claude Code uses.
fn cell_id(cell: &Value, index: usize) -> String {
    cell.get("id")
        .and_then(Value::as_str)
        .map_or_else(|| format!("cell-{index}"), str::to_string)
}

/// A source or output text is a string or a list of strings.
fn joined(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

/// Read the notebook at `args.path` under the canonical workspace `root`.
pub fn read_notebook(root: &std::path::Path, args: &ReadNotebookArgs) -> Result<Vec<Block>> {
    let target = path::resolve_read(root, &args.path)?;
    let rel = target.rel().to_string();
    let meta = std::fs::metadata(target.abs())
        .map_err(|e| Error::invalid_args(format!("cannot stat {rel}")).with_source(e))?;
    if meta.is_dir() {
        return Err(Error::invalid_args(format!(
            "{rel} is a directory, not a file"
        )));
    }
    if !meta.is_file() {
        return Err(Error::invalid_args(format!(
            "{rel} is not a regular file (fifo, socket or device); ccnm will not open it"
        )));
    }
    if meta.len() > MAX_NOTEBOOK_BYTES {
        return Err(too_big(&rel, meta.len()));
    }
    let version = crate::mcp::version_of(&meta);
    let bytes = std::fs::read(target.abs())
        .map_err(|e| Error::invalid_args(format!("cannot read {rel}")).with_source(e))?;
    let notebook = Notebook::parse(&bytes, &rel)?;
    Ok(render(
        &notebook,
        &rel,
        args.start_cell.unwrap_or(0) as usize,
        &version,
    ))
}

fn too_big(rel: &str, bytes: u64) -> Error {
    Error::invalid_args(format!(
        "{rel} is {bytes} bytes; notebooks up to {MAX_NOTEBOOK_BYTES} bytes are read and edited as cells. Clear its outputs first (jupyter nbconvert --clear-output --inplace {rel}) or read the JSON with read_file"
    ))
}

/// What one cell adds to the answer, before deciding whether it fits.
struct Rendered {
    pieces: Vec<Block>,
    text_bytes: usize,
    images: usize,
    image_bytes: u64,
}

fn render(notebook: &Notebook, rel: &str, start: usize, version: &str) -> Vec<Block> {
    let cells = notebook.cells();
    let total = cells.len();
    let language = notebook.language();
    let mut blocks = vec![Block::Text(format!(
        "[notebook {rel}: {total} cell{}, {language}]\n",
        if total == 1 { "" } else { "s" }
    ))];
    if start >= total {
        blocks.push(Block::Text(format!(
            "[no cells returned: start_cell was {start}; version {version}]"
        )));
        return merge(blocks);
    }

    let (mut text_bytes, mut images, mut image_bytes) = (0usize, 0usize, 0u64);
    let mut next = None;
    for (index, cell) in cells.iter().enumerate().skip(start) {
        let mut one = render_cell(cell, index, &language);
        let first = index == start;
        let fits = text_bytes + one.text_bytes <= MAX_TEXT_BYTES
            && images + one.images <= MAX_IMAGES
            && image_bytes + one.image_bytes <= MAX_IMAGE_BYTES;
        if !fits && !first {
            next = Some(index);
            break;
        }
        if !fits {
            // The first cell always shows, cut down: otherwise one huge
            // cell would be a notebook nobody can page past.
            one = cut_down(one, index);
        }
        text_bytes += one.text_bytes;
        images += one.images;
        image_bytes += one.image_bytes;
        blocks.extend(one.pieces);
    }

    let shown_end = next.unwrap_or(total) - 1;
    blocks.push(Block::Text(match next {
        Some(n) => format!(
            "[cells {start}-{shown_end} of {total} shown; continue with start_cell={n}; version {version}]"
        ),
        None => format!("[cells {start}-{shown_end} of {total} shown, end of notebook; version {version}]"),
    }));
    merge(blocks)
}

/// Adjacent text pieces become one block: a Host shows each block as its
/// own paragraph, and a notebook is hundreds of pieces.
fn merge(blocks: Vec<Block>) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    for block in blocks {
        match (out.last_mut(), block) {
            (Some(Block::Text(last)), Block::Text(text)) => last.push_str(&text),
            (_, block) => out.push(block),
        }
    }
    out
}

fn render_cell(cell: &Value, index: usize, language: &str) -> Rendered {
    let id = cell_id(cell, index);
    let kind = cell
        .get("cell_type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut head = format!("<cell id=\"{id}\" index=\"{index}\" type=\"{kind}\"");
    if kind == "code" {
        if let Some(n) = cell.get("execution_count").and_then(Value::as_u64) {
            head.push_str(&format!(" execution_count=\"{n}\""));
        }
        if language != "python" {
            head.push_str(&format!(" language=\"{language}\""));
        }
    }
    let mut source = joined(cell.get("source"));
    if !source.is_empty() && !source.ends_with('\n') {
        source.push('\n');
    }
    let mut text = format!("{head}>\n{source}</cell>\n");
    let mut pieces = Vec::new();
    let (mut images, mut image_bytes) = (0usize, 0u64);

    let outputs = cell.get("outputs").and_then(Value::as_array);
    for output in outputs.into_iter().flatten() {
        let kind = output
            .get("output_type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let (body, image) = match kind {
            "stream" => (joined(output.get("text")), None),
            "execute_result" | "display_data" => {
                let data = output.get("data");
                let plain = joined(data.and_then(|d| d.get("text/plain")));
                (plain, data.and_then(output_image))
            }
            "error" => {
                let name = output.get("ename").and_then(Value::as_str).unwrap_or("");
                let value = output.get("evalue").and_then(Value::as_str).unwrap_or("");
                let trace: Vec<&str> = output
                    .get("traceback")
                    .and_then(Value::as_array)
                    .map(|t| t.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                (
                    strip_ansi(&format!("{name}: {value}\n{}", trace.join("\n"))),
                    None,
                )
            }
            other => (format!("[a {other} output is not shown]"), None),
        };
        let mut attrs = format!("type=\"{kind}\"");
        if let Some(name) = output.get("name").and_then(Value::as_str) {
            attrs.push_str(&format!(" name=\"{name}\""));
        }
        text.push_str(&format!("<output cell=\"{id}\" {attrs}>\n"));
        let body = body.trim_end_matches('\n');
        if body.len() > MAX_OUTPUT_BYTES {
            text.push_str(crate::mcp::truncate_bytes(body, MAX_OUTPUT_BYTES));
            text.push_str(&format!(
                "\n[output cut at {MAX_OUTPUT_BYTES} of {} bytes; all of it is in the notebook's JSON, which read_file shows]",
                body.len()
            ));
        } else {
            text.push_str(body);
        }
        if !body.is_empty() {
            text.push('\n');
        }
        match image {
            Some(Ok((bytes, format))) => {
                text.push_str(&format!(
                    "[{}, {} bytes, the image follows]\n",
                    format.mime_type(),
                    bytes.len()
                ));
                images += 1;
                image_bytes += bytes.len() as u64;
                pieces.push(Block::Text(std::mem::take(&mut text)));
                pieces.push(Block::Image {
                    data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                    format,
                });
            }
            Some(Err(note)) => text.push_str(&format!("[{note}]\n")),
            None => {}
        }
        text.push_str("</output>\n");
    }
    pieces.push(Block::Text(text));
    let text_bytes = pieces
        .iter()
        .map(|p| match p {
            Block::Text(t) => t.len(),
            Block::Image { .. } => 0,
        })
        .sum();
    Rendered {
        pieces,
        text_bytes,
        images,
        image_bytes,
    }
}

/// A PNG or JPEG in an output's data, decoded and checked; the note to
/// show instead when it cannot be sent. Only those two, as Claude Code's
/// Read sends: SVG is text/plain's job and the rest are rare.
fn output_image(data: &Value) -> Option<std::result::Result<(Vec<u8>, Format), String>> {
    let (mime, encoded) = ["image/png", "image/jpeg"].iter().find_map(|mime| {
        Some((
            *mime,
            joined(data.get(*mime)).replace(char::is_whitespace, ""),
        ))
        .filter(|(_, e)| !e.is_empty())
    })?;
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return Some(Err(format!(
            "{mime} output is not valid base64 and is not shown"
        )));
    };
    match Format::sniff(&bytes) {
        Some(format) if format.mime_type() == mime => {}
        _ => {
            return Some(Err(format!(
                "{mime} output does not decode to a {mime} image and is not shown"
            )));
        }
    }
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Some(Err(format!(
            "{mime} output is {} bytes, over the {MAX_IMAGE_BYTES} an image may be, and is not shown",
            bytes.len()
        )));
    }
    let format = Format::sniff(&bytes).expect("checked above");
    Some(Ok((bytes, format)))
}

/// The first cell of a call when it does not fit: its images past the
/// limits become notes, and its text is cut at the budget.
fn cut_down(one: Rendered, index: usize) -> Rendered {
    let mut pieces = Vec::new();
    let (mut text_bytes, mut images, mut image_bytes) = (0usize, 0usize, 0u64);
    for piece in one.pieces {
        match piece {
            Block::Image { data, format } => {
                let bytes = data.len() as u64 / 4 * 3;
                if images < MAX_IMAGES && image_bytes + bytes <= MAX_IMAGE_BYTES {
                    images += 1;
                    image_bytes += bytes;
                    pieces.push(Block::Image { data, format });
                } else {
                    let note = format!(
                        "[{} not sent: one call carries at most {MAX_IMAGES} images and {MAX_IMAGE_BYTES} bytes of them]\n",
                        format.mime_type()
                    );
                    text_bytes += note.len();
                    pieces.push(Block::Text(note));
                }
            }
            Block::Text(text) => {
                let room = MAX_TEXT_BYTES.saturating_sub(text_bytes);
                if text.len() <= room {
                    text_bytes += text.len();
                    pieces.push(Block::Text(text));
                } else {
                    let cut = crate::mcp::truncate_bytes(&text, room).to_string();
                    text_bytes += cut.len();
                    pieces.push(Block::Text(format!(
                        "{cut}\n[cell {index} is longer than {MAX_TEXT_BYTES} bytes and was cut; all of it is in the notebook's JSON, which read_file shows]\n"
                    )));
                    break;
                }
            }
        }
    }
    Rendered {
        pieces,
        text_bytes,
        images,
        image_bytes,
    }
}

/// Terminal colour codes, which IPython puts in every traceback.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            // Parameters and intermediates, then one final byte in @..~.
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// A cell's type, as `edit_notebook` sets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CellType {
    Code,
    Markdown,
}

impl CellType {
    fn name(self) -> &'static str {
        match self {
            CellType::Code => "code",
            CellType::Markdown => "markdown",
        }
    }
}

/// What `edit_notebook` does to one cell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EditMode {
    #[default]
    Replace,
    Insert,
    Delete,
}

/// One cell edit, named as Claude Code's NotebookEdit names them.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellEdit {
    /// The cell's id as read_notebook shows it (`cell-N` for a notebook
    /// without ids). Required for replace and delete; for insert the new
    /// cell goes after it, or first when it is left out.
    #[serde(default)]
    pub cell_id: Option<String>,
    /// The cell's new source. Required for replace and insert.
    #[serde(default)]
    pub new_source: Option<String>,
    /// `code` or `markdown`. Required for insert; for replace, changes the
    /// cell's type.
    #[serde(default)]
    pub cell_type: Option<CellType>,
    /// `replace` (default), `insert` or `delete`.
    #[serde(default)]
    pub edit_mode: Option<EditMode>,
}

/// Apply `edits` in order to the notebook `bytes`; the new file content.
pub(crate) fn edit(bytes: &[u8], rel: &str, edits: &[CellEdit]) -> Result<Vec<u8>> {
    if edits.is_empty() {
        return Err(Error::invalid_args(format!(
            "{rel}: op \"edit_notebook\" needs at least one entry in cells"
        )));
    }
    let mut notebook = Notebook::parse(bytes, rel)?;
    for (n, one) in edits.iter().enumerate() {
        apply_one(&mut notebook, rel, n, one)?;
    }
    let out = notebook.to_bytes();
    if out.len() as u64 > MAX_NOTEBOOK_BYTES {
        return Err(too_big(rel, out.len() as u64));
    }
    Ok(out)
}

fn apply_one(notebook: &mut Notebook, rel: &str, n: usize, one: &CellEdit) -> Result<()> {
    let at = |what: &str| Error::invalid_args(format!("{rel}: cells[{n}]: {what}"));
    let mode = one.edit_mode.unwrap_or_default();
    let index = match one.cell_id.as_deref() {
        Some(id) => Some(find(notebook.cells(), id).ok_or_else(|| {
            let known: Vec<String> = notebook
                .cells()
                .iter()
                .enumerate()
                .map(|(i, c)| cell_id(c, i))
                .collect();
            at(&format!(
                "no cell has id {id}; the cells are {}",
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            ))
        })?),
        None => None,
    };
    match mode {
        EditMode::Delete => {
            let index = index.ok_or_else(|| at("delete needs cell_id"))?;
            notebook.cells_mut().remove(index);
        }
        EditMode::Replace => {
            let index = index.ok_or_else(|| at("replace needs cell_id"))?;
            let source = one
                .new_source
                .as_deref()
                .ok_or_else(|| at("replace needs new_source"))?;
            let cell = notebook.cells_mut()[index]
                .as_object_mut()
                .ok_or_else(|| at("that cell is not a JSON object"))?;
            cell.insert("source".into(), lines(source));
            // Without a cell_type the cell keeps its own, raw included.
            let kind = match one.cell_type {
                Some(kind) => kind.name().to_string(),
                None => cell
                    .get("cell_type")
                    .and_then(Value::as_str)
                    .unwrap_or("code")
                    .to_string(),
            };
            set_type(cell, &kind);
        }
        EditMode::Insert => {
            let kind = one.cell_type.ok_or_else(|| at("insert needs cell_type"))?;
            let source = one
                .new_source
                .as_deref()
                .ok_or_else(|| at("insert needs new_source"))?;
            let mut cell = Map::new();
            cell.insert("cell_type".into(), Value::from(kind.name()));
            if notebook.has_ids() {
                cell.insert("id".into(), Value::from(new_id(notebook.cells())));
            }
            cell.insert("metadata".into(), Value::Object(Map::new()));
            cell.insert("source".into(), lines(source));
            set_type(&mut cell, kind.name());
            let position = index.map_or(0, |i| i + 1);
            notebook.cells_mut().insert(position, Value::Object(cell));
        }
    }
    Ok(())
}

/// A cell by id, else by `cell-N` when no cell has that id.
fn find(cells: &[Value], id: &str) -> Option<usize> {
    cells
        .iter()
        .position(|c| c.get("id").and_then(Value::as_str) == Some(id))
        .or_else(|| {
            id.strip_prefix("cell-")
                .and_then(|n| n.parse::<usize>().ok())
                .filter(|n| *n < cells.len())
        })
}

/// Make a cell the given type: a code cell gets empty outputs and no
/// execution count, as Claude Code does on every replace; a markdown cell
/// loses both keys, which nbformat's schema does not allow on it.
fn set_type(cell: &mut Map<String, Value>, kind: &str) {
    cell.insert("cell_type".into(), Value::from(kind));
    if kind == "code" {
        cell.insert("execution_count".into(), Value::Null);
        cell.insert("outputs".into(), Value::Array(Vec::new()));
    } else {
        cell.remove("execution_count");
        cell.remove("outputs");
    }
}

/// A source as nbformat stores it: one string per line, each but the last
/// keeping its newline.
fn lines(source: &str) -> Value {
    Value::Array(source.split_inclusive('\n').map(Value::from).collect())
}

/// Eight hex characters no other cell in the notebook has.
fn new_id(cells: &[Value]) -> String {
    loop {
        let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        if !cells
            .iter()
            .any(|c| c.get("id").and_then(Value::as_str) == Some(id.as_str()))
        {
            return id;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use ccnm_testdir::TestDir;
    use std::fs;
    use std::path::Path;

    const ANALYSIS: &str = include_str!("../../../../tests/fixtures/notebook/analysis.ipynb");

    fn workspace(name: &str) -> TestDir {
        let dir = std::env::temp_dir().join(format!("ccnm-notebook-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("analysis.ipynb"), ANALYSIS).unwrap();
        TestDir::adopt(fs::canonicalize(&dir).unwrap())
    }

    fn read(root: &Path, start: Option<u32>) -> Vec<Block> {
        read_notebook(
            root,
            &ReadNotebookArgs {
                path: "analysis.ipynb".into(),
                start_cell: start,
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn all_text(blocks: &[Block]) -> String {
        blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text(t) => Some(t.as_str()),
                Block::Image { .. } => None,
            })
            .collect()
    }

    fn edited(edits: Vec<CellEdit>) -> Value {
        let out = edit(ANALYSIS.as_bytes(), "analysis.ipynb", &edits).unwrap();
        serde_json::from_slice(&out).unwrap()
    }

    fn sources(nb: &Value) -> Vec<(String, String)> {
        nb["cells"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, c)| (cell_id(c, i), joined(c.get("source"))))
            .collect()
    }

    #[test]
    fn cells_and_outputs_render_in_order_with_the_image_between() {
        let root = workspace("read");
        let blocks = read(&root, None);
        // text, image, text: the plot's image sits where its output is.
        assert_eq!(blocks.len(), 3, "{blocks:?}");
        let Block::Text(before) = &blocks[0] else {
            panic!()
        };
        assert!(
            before.starts_with("[notebook analysis.ipynb: 5 cells, python]\n"),
            "{before}"
        );
        assert!(
            before.contains("<cell id=\"5a1c0e2f\" index=\"0\" type=\"markdown\">\n# 销售分析\n\nLoad the data and plot it.\n</cell>\n"),
            "{before}"
        );
        assert!(
            before
                .contains("<cell id=\"b7d3a901\" index=\"1\" type=\"code\" execution_count=\"1\">"),
            "{before}"
        );
        assert!(
            before.contains("<output cell=\"b7d3a901\" type=\"stream\" name=\"stdout\">\nrows: 3\ncolumns: 2\n</output>"),
            "{before}"
        );
        assert!(
            before.ends_with("[image/png, 73 bytes, the image follows]\n"),
            "{before}"
        );
        let Block::Image { format, data } = &blocks[1] else {
            panic!("{:?}", blocks[1])
        };
        assert_eq!(*format, Format::Png);
        let png = base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap();
        assert_eq!(Format::sniff(&png), Some(Format::Png));

        let Block::Text(after) = &blocks[2] else {
            panic!()
        };
        assert!(
            after.contains("<output cell=\"c4e8f7aa\" type=\"execute_result\">\n42\n</output>"),
            "{after}"
        );
        // The traceback without its colour codes.
        assert!(
            after.contains("ZeroDivisionError: division by zero\n"),
            "{after}"
        );
        assert!(!after.contains('\u{1b}'), "{after}");
        // An unrun cell has no execution_count attribute.
        assert!(
            after.contains("<cell id=\"e92b4d10\" index=\"4\" type=\"code\">\n</cell>"),
            "{after}"
        );
        assert!(
            after.ends_with(&format!(
                "[cells 0-4 of 5 shown, end of notebook; version {}]",
                crate::mcp::version_of(&fs::metadata(root.join("analysis.ipynb")).unwrap())
            )),
            "{after}"
        );
    }

    #[test]
    fn start_cell_pages_and_an_index_past_the_end_says_so() {
        let root = workspace("page");
        let text = all_text(&read(&root, Some(3)));
        assert!(!text.contains("index=\"2\""), "{text}");
        assert!(text.contains("index=\"3\""), "{text}");
        assert!(
            text.contains("[cells 3-4 of 5 shown, end of notebook;"),
            "{text}"
        );
        let past = all_text(&read(&root, Some(9)));
        assert!(
            past.contains("[no cells returned: start_cell was 9;"),
            "{past}"
        );
    }

    #[test]
    fn a_long_notebook_stops_at_a_cell_boundary_and_says_where_to_continue() {
        let root = workspace("long");
        let mut nb: Value = serde_json::from_str(ANALYSIS).unwrap();
        let cells = nb["cells"].as_array_mut().unwrap();
        for i in 0..40 {
            cells.push(serde_json::json!({
                "cell_type": "markdown", "id": format!("pad{i:05}"), "metadata": {},
                "source": ["x".repeat(2000)]
            }));
        }
        fs::write(
            root.join("analysis.ipynb"),
            serde_json::to_vec(&nb).unwrap(),
        )
        .unwrap();
        let text = all_text(&read(&root, None));
        assert!(text.len() < MAX_TEXT_BYTES + 512, "{}", text.len());
        let footer = text.lines().last().unwrap();
        assert!(
            footer.contains("of 45 shown; continue with start_cell="),
            "{footer}"
        );
    }

    #[test]
    fn one_enormous_cell_still_shows_cut_rather_than_blocking_the_notebook() {
        let root = workspace("huge");
        let mut nb: Value = serde_json::from_str(ANALYSIS).unwrap();
        nb["cells"][0]["source"] = Value::from("y".repeat(MAX_TEXT_BYTES * 2));
        fs::write(
            root.join("analysis.ipynb"),
            serde_json::to_vec(&nb).unwrap(),
        )
        .unwrap();
        let text = all_text(&read(&root, None));
        assert!(
            text.contains("[cell 0 is longer than"),
            "{}",
            &text[text.len() - 300..]
        );
        assert!(
            text.contains("continue with start_cell=1"),
            "{}",
            &text[text.len() - 300..]
        );
    }

    #[test]
    fn a_huge_output_is_cut_and_says_where_the_rest_is() {
        let root = workspace("output");
        let mut nb: Value = serde_json::from_str(ANALYSIS).unwrap();
        nb["cells"][1]["outputs"][0]["text"] = Value::from("progress\n".repeat(2000));
        fs::write(
            root.join("analysis.ipynb"),
            serde_json::to_vec(&nb).unwrap(),
        )
        .unwrap();
        let text = all_text(&read(&root, None));
        assert!(
            text.contains("[output cut at 4096 of 17999 bytes"),
            "{text}"
        );
    }

    #[test]
    fn not_a_notebook_is_refused_with_a_reason() {
        let root = workspace("bad");
        for (body, needle) in [
            ("{\"cells\": [", "not valid JSON"),
            ("[1, 2]", "not a Jupyter notebook"),
            ("{\"cells\": [], \"nbformat\": 3}", "nbformat 3"),
            ("{\"worksheets\": []}", "not a Jupyter notebook"),
        ] {
            fs::write(root.join("bad.ipynb"), body).unwrap();
            let e = read_notebook(
                &root,
                &ReadNotebookArgs {
                    path: "bad.ipynb".into(),
                    start_cell: None,
                    ..Default::default()
                },
            )
            .unwrap_err();
            assert_eq!(e.code(), ErrorCode::InvalidArgs, "{body}");
            assert!(e.message().contains(needle), "{body} -> {e}");
        }
    }

    /// The property the whole edit path rests on: a notebook written the
    /// way nbformat writes it comes back byte for byte when nothing in it
    /// changed. The fixture is `json.dumps(sort_keys=True, indent=1,
    /// ensure_ascii=False)` plus a newline (tests/fixtures/notebook/
    /// make_notebook.py).
    #[test]
    fn an_untouched_notebook_is_written_back_byte_for_byte() {
        let nb = Notebook::parse(ANALYSIS.as_bytes(), "analysis.ipynb").unwrap();
        assert_eq!(nb.indent, 1);
        assert!(nb.final_newline);
        assert_eq!(String::from_utf8(nb.to_bytes()).unwrap(), ANALYSIS);

        // Another width and no final newline are kept too.
        let value: Value = serde_json::from_str(ANALYSIS).unwrap();
        let two = serde_json::to_string_pretty(&value).unwrap();
        let nb = Notebook::parse(two.as_bytes(), "x.ipynb").unwrap();
        assert_eq!(String::from_utf8(nb.to_bytes()).unwrap(), two);
    }

    #[test]
    fn replace_keeps_the_rest_and_empties_a_code_cells_outputs() {
        let nb = edited(vec![CellEdit {
            cell_id: Some("c4e8f7aa".into()),
            new_source: Some("df.plot(kind=\"bar\")\n42".into()),
            ..Default::default()
        }]);
        let cell = &nb["cells"][2];
        assert_eq!(
            cell["source"],
            serde_json::json!(["df.plot(kind=\"bar\")\n", "42"])
        );
        assert_eq!(cell["outputs"], serde_json::json!([]));
        assert_eq!(cell["execution_count"], Value::Null);
        assert_eq!(
            cell["metadata"],
            serde_json::json!({"tags": ["plot"]}),
            "metadata kept"
        );
        // Every other cell is untouched.
        let before: Value = serde_json::from_str(ANALYSIS).unwrap();
        for i in [0, 1, 3, 4] {
            assert_eq!(nb["cells"][i], before["cells"][i], "cell {i}");
        }
    }

    #[test]
    fn changing_a_cells_type_leaves_only_the_keys_that_type_may_have() {
        let nb = edited(vec![CellEdit {
            cell_id: Some("b7d3a901".into()),
            new_source: Some("Now it is prose.".into()),
            cell_type: Some(CellType::Markdown),
            ..Default::default()
        }]);
        let cell = nb["cells"][1].as_object().unwrap();
        assert_eq!(cell["cell_type"], "markdown");
        assert!(
            !cell.contains_key("outputs") && !cell.contains_key("execution_count"),
            "{cell:?}"
        );

        let nb = edited(vec![CellEdit {
            cell_id: Some("5a1c0e2f".into()),
            new_source: Some("print('now code')".into()),
            cell_type: Some(CellType::Code),
            ..Default::default()
        }]);
        assert_eq!(nb["cells"][0]["outputs"], serde_json::json!([]));
        assert_eq!(nb["cells"][0]["execution_count"], Value::Null);
    }

    #[test]
    fn replacing_without_a_type_keeps_the_cells_own_even_raw() {
        let mut nb: Value = serde_json::from_str(ANALYSIS).unwrap();
        nb["cells"][0]["cell_type"] = Value::from("raw");
        let bytes = serde_json::to_vec(&nb).unwrap();
        let out = edit(
            &bytes,
            "raw.ipynb",
            &[CellEdit {
                cell_id: Some("5a1c0e2f".into()),
                new_source: Some("raw text".into()),
                ..Default::default()
            }],
        )
        .unwrap();
        let nb: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(nb["cells"][0]["cell_type"], "raw");
        assert!(nb["cells"][0].get("outputs").is_none());
    }

    #[test]
    fn insert_goes_after_the_named_cell_or_first_and_gets_a_fresh_id() {
        let nb = edited(vec![
            CellEdit {
                cell_id: Some("b7d3a901".into()),
                new_source: Some("df.head()".into()),
                cell_type: Some(CellType::Code),
                edit_mode: Some(EditMode::Insert),
            },
            CellEdit {
                new_source: Some("# Intro".into()),
                cell_type: Some(CellType::Markdown),
                edit_mode: Some(EditMode::Insert),
                ..Default::default()
            },
        ]);
        let ids: Vec<String> = sources(&nb).into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids.len(), 7);
        assert_eq!(joined(nb["cells"][0].get("source")), "# Intro");
        assert_eq!(ids[1], "5a1c0e2f");
        assert_eq!(ids[2], "b7d3a901");
        assert_eq!(joined(nb["cells"][3].get("source")), "df.head()");
        for new in [&ids[0], &ids[3]] {
            assert_eq!(new.len(), 8, "{new}");
            assert!(new.chars().all(|c| c.is_ascii_hexdigit()), "{new}");
        }
        assert_ne!(ids[0], ids[3]);
        let inserted = nb["cells"][3].as_object().unwrap();
        assert_eq!(inserted["outputs"], serde_json::json!([]));
        let markdown = nb["cells"][0].as_object().unwrap();
        assert!(!markdown.contains_key("outputs"), "{markdown:?}");
    }

    #[test]
    fn delete_removes_and_later_edits_see_the_result() {
        let nb = edited(vec![
            CellEdit {
                cell_id: Some("d0f19b3c".into()),
                edit_mode: Some(EditMode::Delete),
                ..Default::default()
            },
            // Sequential: after the delete, cell-3 is what was index 4.
            CellEdit {
                cell_id: Some("cell-3".into()),
                new_source: Some("print('last')".into()),
                ..Default::default()
            },
        ]);
        let ids: Vec<String> = sources(&nb).into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, ["5a1c0e2f", "b7d3a901", "c4e8f7aa", "e92b4d10"]);
        assert_eq!(joined(nb["cells"][3].get("source")), "print('last')");
    }

    #[test]
    fn a_notebook_without_ids_is_addressed_by_index_and_gets_none_added() {
        let mut nb: Value = serde_json::from_str(ANALYSIS).unwrap();
        nb["nbformat_minor"] = Value::from(4);
        for cell in nb["cells"].as_array_mut().unwrap() {
            cell.as_object_mut().unwrap().remove("id");
        }
        let bytes = serde_json::to_vec_pretty(&nb).unwrap();
        let out = edit(
            &bytes,
            "old.ipynb",
            &[CellEdit {
                cell_id: Some("cell-1".into()),
                new_source: Some("x = 1".into()),
                cell_type: Some(CellType::Code),
                edit_mode: Some(EditMode::Insert),
            }],
        )
        .unwrap();
        let nb: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(joined(nb["cells"][2].get("source")), "x = 1");
        assert!(
            nb["cells"][2].get("id").is_none(),
            "nbformat 4.4 has no ids"
        );
    }

    #[test]
    fn a_bad_edit_says_which_entry_and_what_is_missing() {
        for (one, needle) in [
            (
                CellEdit {
                    cell_id: Some("nope".into()),
                    new_source: Some("x".into()),
                    ..Default::default()
                },
                "no cell has id nope; the cells are 5a1c0e2f, b7d3a901",
            ),
            (
                CellEdit {
                    new_source: Some("x".into()),
                    ..Default::default()
                },
                "replace needs cell_id",
            ),
            (
                CellEdit {
                    cell_id: Some("5a1c0e2f".into()),
                    ..Default::default()
                },
                "replace needs new_source",
            ),
            (
                CellEdit {
                    new_source: Some("x".into()),
                    edit_mode: Some(EditMode::Insert),
                    ..Default::default()
                },
                "insert needs cell_type",
            ),
            (
                CellEdit {
                    edit_mode: Some(EditMode::Delete),
                    ..Default::default()
                },
                "delete needs cell_id",
            ),
            (
                CellEdit {
                    cell_id: Some("cell-99".into()),
                    edit_mode: Some(EditMode::Delete),
                    ..Default::default()
                },
                "no cell has id cell-99",
            ),
        ] {
            let e = edit(ANALYSIS.as_bytes(), "analysis.ipynb", &[one]).unwrap_err();
            assert_eq!(e.code(), ErrorCode::InvalidArgs);
            assert!(e.message().contains("analysis.ipynb: cells[0]:"), "{e}");
            assert!(e.message().contains(needle), "{needle} -> {e}");
        }
        let e = edit(ANALYSIS.as_bytes(), "analysis.ipynb", &[]).unwrap_err();
        assert!(e.message().contains("at least one"), "{e}");
    }

    #[test]
    fn ansi_colour_codes_are_removed_and_nothing_else() {
        let esc = '\u{1b}';
        assert_eq!(
            strip_ansi(&format!("{esc}[0;31mError{esc}[0m: x[1]")),
            "Error: x[1]"
        );
        assert_eq!(strip_ansi(&format!("{esc}[38;5;241;43m1{esc}[39m")), "1");
        assert_eq!(strip_ansi("plain [text]"), "plain [text]");
    }
}
