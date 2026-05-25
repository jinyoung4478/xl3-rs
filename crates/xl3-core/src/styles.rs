//! Parse the bits of a template workbook that calamine doesn't expose:
//! the `xl/styles.xml` numFmt / cellXfs tables, and each worksheet's
//! cell-level `s="<xf>"` attribute. We need this for ADR-0003 numFmt
//! cell coercion (`{{ [Amount] }}` in a numeric-format cell yields a
//! Number, not the source string).
//!
//! Implementation note: we re-open the template as a zip archive,
//! independent of the calamine handle. Both pull from the same
//! transitive zip + quick-xml deps, so this doesn't add to the binary
//! footprint.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use zip::ZipArchive;

/// Information extracted from a template's `xl/styles.xml` and the
/// `s="..."` attributes on every cell. Keyed by *sheet name* — the
/// caller resolves names via calamine's `sheet_names()` and matches
/// them up.
#[derive(Debug, Clone, Default)]
pub struct TemplateStyles {
    /// `cellXfs[xf_index] -> numFmtId`. Indexed by xf id (zero-based).
    pub cell_xf_to_num_fmt_id: Vec<u32>,
    /// Per-sheet cell style index: (row, col) — 0-based — → xf id.
    /// Sheets keyed by sheet name (the human-readable name, matching
    /// what calamine returns).
    pub per_sheet_cell_xf: HashMap<String, HashMap<(u32, u32), u32>>,
    /// Custom numFmt definitions (`numFmtId -> formatCode`).
    pub custom_num_fmts: HashMap<u32, String>,
}

impl TemplateStyles {
    /// Resolve `(sheet, row, col)` to its format code, falling back
    /// through cellXfs → numFmtId → built-in or custom format. `None`
    /// when there's no style attached at all.
    pub fn format_code(&self, sheet: &str, row: u32, col: u32) -> Option<String> {
        let xf = *self.per_sheet_cell_xf.get(sheet)?.get(&(row, col))?;
        let num_fmt_id = *self.cell_xf_to_num_fmt_id.get(xf as usize)?;
        if let Some(custom) = self.custom_num_fmts.get(&num_fmt_id) {
            return Some(custom.clone());
        }
        builtin_num_fmt(num_fmt_id).map(str::to_string)
    }
}

/// Built-in numFmtIds per OOXML §18.8.30 — the subset we need. Anything
/// not listed here (or not in the custom map) falls back to General.
fn builtin_num_fmt(id: u32) -> Option<&'static str> {
    Some(match id {
        0 => "General",
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        12 => "# ?/?",
        13 => "# ??/??",
        14 => "m/d/yyyy",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 => "h:mm",
        21 => "h:mm:ss",
        22 => "m/d/yyyy h:mm",
        37 => "#,##0 ;(#,##0)",
        38 => "#,##0 ;[Red](#,##0)",
        39 => "#,##0.00;(#,##0.00)",
        40 => "#,##0.00;[Red](#,##0.00)",
        45 => "mm:ss",
        46 => "[h]:mm:ss",
        47 => "mmss.0",
        48 => "##0.0E+0",
        49 => "@",
        _ => return None,
    })
}

/// What kind of conversion the cell's numFmt asks for. Driven by the
/// shape of the format code (xl3 uses the same broad buckets — date,
/// number, text — as the spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumFmtKind {
    General,
    Numeric,
    Date,
    Text,
}

pub fn classify_num_fmt(code: &str) -> NumFmtKind {
    let trimmed = code.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("general") {
        return NumFmtKind::General;
    }
    if trimmed == "@" {
        return NumFmtKind::Text;
    }
    // Strip quoted literal segments so a literal like `"Year:" yyyy`
    // is still classified as a date format.
    let mut without_quotes = String::with_capacity(trimmed.len());
    let mut in_quote = false;
    for c in trimmed.chars() {
        if c == '"' {
            in_quote = !in_quote;
            continue;
        }
        if !in_quote {
            without_quotes.push(c);
        }
    }
    let lower = without_quotes.to_ascii_lowercase();
    let date_tokens = ["yyyy", "yy", "mm", "dd", "hh", "ss", "am/pm", "a/p"];
    if date_tokens.iter().any(|t| lower.contains(t))
        || lower.contains('y')
        || lower.contains('d')
        || lower.contains('h')
    {
        return NumFmtKind::Date;
    }
    // Anything with a digit-placeholder or numeric formatting char is
    // numeric. `m` alone is ambiguous (month vs minute) but it only
    // shows up in date contexts in practice — we already handled
    // date-looking codes above.
    if lower
        .chars()
        .any(|c| matches!(c, '0' | '#' | '.' | ',' | '%' | 'e' | '?'))
    {
        return NumFmtKind::Numeric;
    }
    NumFmtKind::General
}

pub fn parse_template_styles(path: &Path) -> Result<TemplateStyles> {
    let file = File::open(path)
        .with_context(|| format!("open template zip at {}", path.display()))?;
    let archive =
        ZipArchive::new(file).with_context(|| format!("zip read at {}", path.display()))?;
    parse_template_styles_from_archive(archive)
}

/// Variant that reads styles from an in-memory XLSX byte buffer. Used
/// by the WASM `convert()` entry point and any host that already has
/// the template bytes in memory.
pub fn parse_template_styles_bytes(bytes: &[u8]) -> Result<TemplateStyles> {
    let cursor = std::io::Cursor::new(bytes.to_vec());
    let archive = ZipArchive::new(cursor).context("zip read of template bytes")?;
    parse_template_styles_from_archive(archive)
}

fn parse_template_styles_from_archive<R: Read + std::io::Seek>(
    mut archive: ZipArchive<R>,
) -> Result<TemplateStyles> {
    let (cell_xf_to_num_fmt_id, custom_num_fmts) = read_styles_xml(&mut archive)?;
    let name_to_path = read_workbook_sheet_map(&mut archive)?;

    let mut per_sheet_cell_xf: HashMap<String, HashMap<(u32, u32), u32>> = HashMap::new();
    for (sheet_name, sheet_path) in &name_to_path {
        let cell_xf = read_sheet_cell_xf(&mut archive, sheet_path)?;
        per_sheet_cell_xf.insert(sheet_name.clone(), cell_xf);
    }

    Ok(TemplateStyles {
        cell_xf_to_num_fmt_id,
        per_sheet_cell_xf,
        custom_num_fmts,
    })
}

fn read_styles_xml<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
) -> Result<(Vec<u32>, HashMap<u32, String>)> {
    let mut content = String::new();
    {
        let mut file = match archive.by_name("xl/styles.xml") {
            Ok(f) => f,
            Err(_) => return Ok((Vec::new(), HashMap::new())),
        };
        file.read_to_string(&mut content)
            .context("read styles.xml")?;
    }
    let mut reader = Reader::from_str(&content);
    reader.config_mut().trim_text(false);

    let mut in_num_fmts = false;
    let mut in_cell_xfs = false;
    let mut custom_num_fmts: HashMap<u32, String> = HashMap::new();
    let mut cell_xfs: Vec<u32> = Vec::new();

    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = e.name();
                let local = name.as_ref();
                match local {
                    b"numFmts" => in_num_fmts = true,
                    b"cellXfs" => in_cell_xfs = true,
                    b"numFmt" if in_num_fmts => {
                        let id = attr_u32(&e, b"numFmtId").unwrap_or(0);
                        let code = attr_str(&e, b"formatCode").unwrap_or_default();
                        custom_num_fmts.insert(id, code);
                    }
                    b"xf" if in_cell_xfs => {
                        cell_xfs.push(attr_u32(&e, b"numFmtId").unwrap_or(0));
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"numFmts" => in_num_fmts = false,
                b"cellXfs" => in_cell_xfs = false,
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow!("styles.xml parse: {e}")),
            _ => {}
        }
    }
    Ok((cell_xfs, custom_num_fmts))
}

/// Map sheet name → "xl/worksheets/sheetN.xml" by following the
/// workbook → workbook.xml.rels chain.
fn read_workbook_sheet_map<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
) -> Result<Vec<(String, String)>> {
    let workbook_xml = read_zip_entry(archive, "xl/workbook.xml")?;
    let rels_xml = read_zip_entry(archive, "xl/_rels/workbook.xml.rels").unwrap_or_default();

    // sheet name + relationship id
    let mut sheets: Vec<(String, String)> = Vec::new();
    let mut reader = Reader::from_str(&workbook_xml);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.name().as_ref() == b"sheet" => {
                let name = attr_str(&e, b"name").unwrap_or_default();
                // r:id attribute — quick-xml stores the prefix.
                let rid = attr_str(&e, b"r:id")
                    .or_else(|| attr_str(&e, b"id"))
                    .unwrap_or_default();
                if !name.is_empty() && !rid.is_empty() {
                    sheets.push((name, rid));
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow!("workbook.xml parse: {e}")),
            _ => {}
        }
    }

    // rid → target path
    let mut rid_to_target: HashMap<String, String> = HashMap::new();
    let mut reader = Reader::from_str(&rels_xml);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.name().as_ref() == b"Relationship" => {
                let id = attr_str(&e, b"Id").unwrap_or_default();
                let target = attr_str(&e, b"Target").unwrap_or_default();
                if !id.is_empty() && !target.is_empty() {
                    let resolved = if target.starts_with('/') {
                        target.trim_start_matches('/').to_string()
                    } else {
                        format!("xl/{target}")
                    };
                    rid_to_target.insert(id, resolved);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow!("workbook.rels parse: {e}")),
            _ => {}
        }
    }

    let mut out = Vec::with_capacity(sheets.len());
    for (name, rid) in sheets {
        if let Some(target) = rid_to_target.get(&rid) {
            out.push((name, target.clone()));
        }
    }
    Ok(out)
}

fn read_sheet_cell_xf<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    sheet_path: &str,
) -> Result<HashMap<(u32, u32), u32>> {
    let xml = match read_zip_entry(archive, sheet_path) {
        Ok(s) => s,
        Err(_) => return Ok(HashMap::new()),
    };
    let mut out: HashMap<(u32, u32), u32> = HashMap::new();
    let mut reader = Reader::from_str(&xml);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.name().as_ref() == b"c" => {
                // Only cells that have a `s` attribute matter — others
                // use the default (xf 0 = General). We still skip
                // those to keep the map small.
                let Some(s_attr) = attr_u32(&e, b"s") else {
                    continue;
                };
                let Some(r_attr) = attr_str(&e, b"r") else {
                    continue;
                };
                if let Some((row, col)) = parse_a1_cell_ref(&r_attr) {
                    out.insert((row, col), s_attr);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow!("{sheet_path} parse: {e}")),
            _ => {}
        }
    }
    Ok(out)
}

fn read_zip_entry<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
) -> Result<String> {
    let mut file = archive
        .by_name(name)
        .with_context(|| format!("zip entry {name}"))?;
    let mut s = String::new();
    file.read_to_string(&mut s)
        .with_context(|| format!("read {name}"))?;
    Ok(s)
}

fn attr_str(start: &BytesStart, key: &[u8]) -> Option<String> {
    for attr in start.attributes().flatten() {
        if attr.key.as_ref() == key {
            return Some(String::from_utf8_lossy(&attr.value).into_owned());
        }
    }
    None
}

fn attr_u32(start: &BytesStart, key: &[u8]) -> Option<u32> {
    attr_str(start, key).and_then(|s| s.parse().ok())
}

/// Parse an A1 cell reference (`B3`, `AA10`) into (row, col) — both
/// 0-based.
fn parse_a1_cell_ref(s: &str) -> Option<(u32, u32)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut col: u32 = 0;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        let c = bytes[i].to_ascii_uppercase();
        col = col * 26 + (c - b'A' + 1) as u32;
        i += 1;
    }
    if col == 0 || i == 0 || i == bytes.len() {
        return None;
    }
    let row: u32 = std::str::from_utf8(&bytes[i..]).ok()?.parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((row - 1, col - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_basic() {
        assert_eq!(classify_num_fmt("0.00"), NumFmtKind::Numeric);
        assert_eq!(classify_num_fmt("@"), NumFmtKind::Text);
        assert_eq!(classify_num_fmt("yyyy-mm-dd"), NumFmtKind::Date);
        assert_eq!(classify_num_fmt(""), NumFmtKind::General);
        assert_eq!(classify_num_fmt("General"), NumFmtKind::General);
    }

    #[test]
    fn a1_parse() {
        assert_eq!(parse_a1_cell_ref("A1"), Some((0, 0)));
        assert_eq!(parse_a1_cell_ref("B3"), Some((2, 1)));
        assert_eq!(parse_a1_cell_ref("AA10"), Some((9, 26)));
    }
}
