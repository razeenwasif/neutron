//! Extended viewer formats: Markdown blocks, virtualized CSV/TSV/JSONL tables,
//! Parquet footers, and zero-copy ML tensor headers (.safetensors, .gguf, .onnx).

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdBlock {
    Heading { level: u8, text: String },
    Paragraph(String),
    CodeBlock { lang: String, code: String },
    Bullet { indent: usize, text: String },
    Quote(String),
    Rule,
}

pub struct MarkdownDoc {
    pub raw: crate::viewer::TextDoc,
    pub blocks: Vec<MdBlock>,
}

pub fn parse_markdown(text: &str) -> Vec<MdBlock> {
    let mut blocks = Vec::new();
    let mut para = String::new();
    let mut in_code = false;
    let mut code_lang = String::new();
    let mut code_buf = String::new();

    let flush_para = |para: &mut String, out: &mut Vec<MdBlock>| {
        let trimmed = para.trim();
        if !trimmed.is_empty() {
            out.push(MdBlock::Paragraph(trimmed.to_owned()));
        }
        para.clear();
    };

    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        let trimmed = line.trim_start();

        if let Some(after) = trimmed.strip_prefix("```") {
            if in_code {
                blocks.push(MdBlock::CodeBlock {
                    lang: std::mem::take(&mut code_lang),
                    code: code_buf.trim_end_matches('\n').to_owned(),
                });
                code_buf.clear();
                in_code = false;
            } else {
                flush_para(&mut para, &mut blocks);
                code_lang = after.trim().to_owned();
                in_code = true;
            }
            continue;
        }

        if in_code {
            code_buf.push_str(line);
            code_buf.push('\n');
            continue;
        }

        if trimmed.is_empty() {
            flush_para(&mut para, &mut blocks);
            continue;
        }

        if (trimmed == "---" || trimmed == "***" || trimmed == "___")
            && trimmed.chars().all(|c| c == trimmed.chars().next().unwrap())
        {
            flush_para(&mut para, &mut blocks);
            blocks.push(MdBlock::Rule);
            continue;
        }

        let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
        if (1..=6).contains(&hashes) && trimmed.as_bytes().get(hashes) == Some(&b' ') {
            flush_para(&mut para, &mut blocks);
            blocks.push(MdBlock::Heading {
                level: hashes as u8,
                text: trimmed[hashes + 1..].trim().to_owned(),
            });
            continue;
        }

        if let Some(q) = trimmed.strip_prefix("> ") {
            flush_para(&mut para, &mut blocks);
            blocks.push(MdBlock::Quote(q.trim().to_owned()));
            continue;
        }

        let indent = line.len().saturating_sub(trimmed.len()) / 2;
        if let Some(b) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            flush_para(&mut para, &mut blocks);
            blocks.push(MdBlock::Bullet {
                indent,
                text: b.trim().to_owned(),
            });
            continue;
        }
        if let Some((num, rest)) = trimmed.split_once(". ")
            && !num.is_empty()
            && num.bytes().all(|b| b.is_ascii_digit())
        {
            flush_para(&mut para, &mut blocks);
            blocks.push(MdBlock::Bullet {
                indent,
                text: format!("{num}. {}", rest.trim()),
            });
            continue;
        }

        if !para.is_empty() {
            para.push(' ');
        }
        para.push_str(trimmed);
    }

    if in_code {
        blocks.push(MdBlock::CodeBlock {
            lang: code_lang,
            code: code_buf.trim_end_matches('\n').to_owned(),
        });
    } else {
        flush_para(&mut para, &mut blocks);
    }
    blocks
}

#[derive(Debug, Clone)]
pub struct TableDoc {
    pub format_label: &'static str,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub total_rows: usize,
    pub truncated: bool,
    pub sort_col: Option<(usize, bool)>,
    pub order: Vec<usize>,
    pub meta: Vec<(String, String)>,
}

impl TableDoc {
    pub fn sort_by(&mut self, col: usize) {
        if col >= self.columns.len() {
            return;
        }
        let ascending = match self.sort_col {
            Some((c, prev)) if c == col => !prev,
            _ => true,
        };
        self.sort_col = Some((col, ascending));
        let rows = &self.rows;
        self.order.sort_by(|&a, &b| {
            let va = rows.get(a).and_then(|r| r.get(col)).map(String::as_str).unwrap_or("");
            let vb = rows.get(b).and_then(|r| r.get(col)).map(String::as_str).unwrap_or("");
            let ord = match (va.trim().parse::<f64>(), vb.trim().parse::<f64>()) {
                (Ok(fa), Ok(fb)) => fa.total_cmp(&fb),
                _ => va.to_ascii_lowercase().cmp(&vb.to_ascii_lowercase()),
            };
            if ascending { ord } else { ord.reverse() }
        });
    }

    pub fn ordered_row(&self, display_idx: usize) -> Option<&[String]> {
        let &raw_idx = self.order.get(display_idx)?;
        self.rows.get(raw_idx).map(Vec::as_slice)
    }
}

pub fn parse_delimited(text: &str, delimiter: u8, format_label: &'static str, max_rows: usize) -> TableDoc {
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut cur_row: Vec<String> = Vec::new();
    let mut cur_cell = String::new();
    let mut in_quotes = false;
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut truncated = false;

    while i < bytes.len() {
        let b = bytes[i];
        if in_quotes {
            if b == b'"' {
                if bytes.get(i + 1) == Some(&b'"') {
                    cur_cell.push('"');
                    i += 2;
                    continue;
                }
                in_quotes = false;
                i += 1;
                continue;
            }
            if let Some(ch) = next_char(text, &mut i) {
                cur_cell.push(ch);
            }
            continue;
        }

        if b == b'"' && cur_cell.is_empty() {
            in_quotes = true;
            i += 1;
            continue;
        }
        if b == delimiter {
            cur_row.push(std::mem::take(&mut cur_cell));
            i += 1;
            continue;
        }
        if b == b'\r' || b == b'\n' {
            if b == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                i += 1;
            }
            cur_row.push(std::mem::take(&mut cur_cell));
            if cur_row.len() > 1 || !cur_row[0].is_empty() {
                records.push(std::mem::take(&mut cur_row));
                if records.len() > max_rows + 1 {
                    truncated = true;
                    break;
                }
            } else {
                cur_row.clear();
            }
            i += 1;
            continue;
        }
        if let Some(ch) = next_char(text, &mut i) {
            cur_cell.push(ch);
        }
    }

    if !truncated && (!cur_cell.is_empty() || !cur_row.is_empty()) {
        cur_row.push(cur_cell);
        records.push(cur_row);
    }

    let mut columns = if records.is_empty() {
        Vec::new()
    } else {
        records.remove(0)
    };
    let max_cols = records
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .max(columns.len());
    while columns.len() < max_cols {
        columns.push(format!("col_{}", columns.len() + 1));
    }
    for r in &mut records {
        r.resize(max_cols, String::new());
    }

    let total_rows = records.len();
    let order = (0..total_rows).collect();
    TableDoc {
        format_label,
        columns,
        rows: records,
        total_rows,
        truncated,
        sort_col: None,
        order,
        meta: Vec::new(),
    }
}

fn next_char(text: &str, idx: &mut usize) -> Option<char> {
    let ch = text[*idx..].chars().next()?;
    *idx += ch.len_utf8();
    Some(ch)
}

pub fn parse_jsonl(text: &str, max_rows: usize) -> TableDoc {
    let mut key_to_col: HashMap<String, usize> = HashMap::new();
    let mut columns: Vec<String> = Vec::new();
    let mut raw_maps: Vec<Vec<(usize, String)>> = Vec::new();
    let mut truncated = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if raw_maps.len() >= max_rows {
            truncated = true;
            break;
        }
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let mut row_pairs = Vec::with_capacity(map.len());
            for (k, v) in map {
                let col = match key_to_col.get(&k) {
                    Some(&idx) => idx,
                    None => {
                        let idx = columns.len();
                        key_to_col.insert(k.clone(), idx);
                        columns.push(k);
                        idx
                    }
                };
                let cell = match v {
                    serde_json::Value::Null => "null".to_owned(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                row_pairs.push((col, cell));
            }
            raw_maps.push(row_pairs);
        } else {
            let col = *key_to_col.entry("value".to_owned()).or_insert_with(|| {
                let idx = columns.len();
                columns.push("value".to_owned());
                idx
            });
            raw_maps.push(vec![(col, trimmed.to_owned())]);
        }
    }

    let col_count = columns.len();
    let rows: Vec<Vec<String>> = raw_maps
        .into_iter()
        .map(|pairs| {
            let mut row = vec![String::new(); col_count];
            for (c, v) in pairs {
                if c < col_count {
                    row[c] = v;
                }
            }
            row
        })
        .collect();

    let total_rows = rows.len();
    let order = (0..total_rows).collect();
    TableDoc {
        format_label: "JSONL",
        columns,
        rows,
        total_rows,
        truncated,
        sort_col: None,
        order,
        meta: Vec::new(),
    }
}

pub fn load_parquet_summary(path: &Path) -> Result<TableDoc, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("Could not open {}: {e}", path.display()))?;
    let file_len = f.metadata().map_err(|e| e.to_string())?.len();
    if file_len < 12 {
        return Err("Not a valid Parquet file (too small).".into());
    }
    let mut head = [0u8; 4];
    f.read_exact(&mut head).map_err(|e| e.to_string())?;
    if &head != b"PAR1" {
        return Err("Not a valid Parquet file (missing PAR1 header).".into());
    }
    f.seek(SeekFrom::End(-8)).map_err(|e| e.to_string())?;
    let mut tail = [0u8; 8];
    f.read_exact(&mut tail).map_err(|e| e.to_string())?;
    if &tail[4..8] != b"PAR1" {
        return Err("Not a valid Parquet file (missing PAR1 trailer).".into());
    }
    let footer_len = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]) as u64;
    if footer_len == 0 || footer_len > 32 * 1024 * 1024 || footer_len + 12 > file_len {
        return Err(format!("Unsupported Parquet footer size ({footer_len} bytes)."));
    }
    f.seek(SeekFrom::End(-(footer_len as i64 + 8)))
        .map_err(|e| e.to_string())?;
    let mut footer = vec![0u8; footer_len as usize];
    f.read_exact(&mut footer).map_err(|e| e.to_string())?;

    parse_parquet_footer(&footer)
}

pub fn parse_parquet_footer(footer: &[u8]) -> Result<TableDoc, String> {
    let mut r = ThriftReader::new(footer);
    let mut version: Option<i32> = None;
    let mut num_rows: Option<i64> = None;
    let mut row_groups: Option<usize> = None;
    let mut created_by: Option<String> = None;
    let mut rows: Vec<Vec<String>> = Vec::new();

    let mut last_id = 0i16;
    while let Some((ftype, fid)) = r.read_field_header(&mut last_id)? {
        match (fid, ftype) {
            (1, 5) => version = Some(r.read_i32_zigzag()?),
            (2, 9) => {
                // List<SchemaElement>
                let (elem_type, count) = r.read_list_header()?;
                if elem_type == 12 {
                    for idx in 0..count {
                        let (name, ptype, rep, has_children) = read_schema_element(&mut r)?;
                        if idx > 0 || !has_children {
                            rows.push(vec![name, ptype, rep]);
                        }
                    }
                } else {
                    for _ in 0..count {
                        r.skip_type(elem_type)?;
                    }
                }
            }
            (3, 6) => num_rows = Some(r.read_i64_zigzag()?),
            (4, 9) => {
                let (elem_type, count) = r.read_list_header()?;
                row_groups = Some(count);
                for _ in 0..count {
                    r.skip_type(elem_type)?;
                }
            }
            (6, 8) => created_by = Some(r.read_string()?),
            _ => r.skip_type(ftype)?,
        }
    }

    let mut meta = Vec::new();
    if let Some(nr) = num_rows {
        meta.push(("Rows".into(), nr.to_string()));
    }
    if let Some(rg) = row_groups {
        meta.push(("Row Groups".into(), rg.to_string()));
    }
    if let Some(v) = version {
        meta.push(("Format Version".into(), v.to_string()));
    }
    if let Some(cb) = created_by {
        meta.push(("Created By".into(), cb));
    }

    let total_rows = rows.len();
    let order = (0..total_rows).collect();
    Ok(TableDoc {
        format_label: "Parquet Schema",
        columns: vec!["Column".into(), "Physical Type".into(), "Repetition".into()],
        rows,
        total_rows,
        truncated: false,
        sort_col: None,
        order,
        meta,
    })
}

fn read_schema_element(r: &mut ThriftReader<'_>) -> Result<(String, String, String, bool), String> {
    let mut name = String::new();
    let mut ptype = String::from("GROUP");
    let mut rep = String::from("-");
    let mut has_children = false;
    let mut last_id = 0i16;
    while let Some((ftype, fid)) = r.read_field_header(&mut last_id)? {
        match (fid, ftype) {
            (1, 5) => ptype = parquet_physical_type(r.read_i32_zigzag()?).to_owned(),
            (3, 5) => rep = parquet_rep_type(r.read_i32_zigzag()?).to_owned(),
            (4, 8) => name = r.read_string()?,
            (5, 5) => {
                let n = r.read_i32_zigzag()?;
                has_children = n > 0;
            }
            _ => r.skip_type(ftype)?,
        }
    }
    Ok((name, ptype, rep, has_children))
}

fn parquet_physical_type(t: i32) -> &'static str {
    match t {
        0 => "BOOLEAN",
        1 => "INT32",
        2 => "INT64",
        3 => "INT96",
        4 => "FLOAT",
        5 => "DOUBLE",
        6 => "BYTE_ARRAY",
        7 => "FIXED_LEN_BYTE_ARRAY",
        _ => "UNKNOWN",
    }
}

fn parquet_rep_type(r: i32) -> &'static str {
    match r {
        0 => "REQUIRED",
        1 => "OPTIONAL",
        2 => "REPEATED",
        _ => "-",
    }
}

struct ThriftReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> ThriftReader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, String> {
        let b = *self.buf.get(self.pos).ok_or("Unexpected EOF in Thrift footer")?;
        self.pos += 1;
        Ok(b)
    }

    fn read_varint(&mut self) -> Result<u64, String> {
        let mut res = 0u64;
        let mut shift = 0u32;
        loop {
            let b = self.read_u8()?;
            res |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(res);
            }
            shift += 7;
            if shift >= 64 {
                return Err("Thrift varint overflow".into());
            }
        }
    }

    fn read_i32_zigzag(&mut self) -> Result<i32, String> {
        let n = self.read_varint()? as u32;
        Ok(((n >> 1) as i32) ^ (-((n & 1) as i32)))
    }

    fn read_i64_zigzag(&mut self) -> Result<i64, String> {
        let n = self.read_varint()?;
        Ok(((n >> 1) as i64) ^ (-((n & 1) as i64)))
    }

    fn read_field_header(&mut self, last_id: &mut i16) -> Result<Option<(u8, i16)>, String> {
        let b = self.read_u8()?;
        if b == 0 {
            return Ok(None);
        }
        let delta = (b >> 4) as i16;
        let ftype = b & 0x0f;
        let fid = if delta == 0 {
            self.read_i32_zigzag()? as i16
        } else {
            *last_id + delta
        };
        *last_id = fid;
        Ok(Some((ftype, fid)))
    }

    fn read_list_header(&mut self) -> Result<(u8, usize), String> {
        let b = self.read_u8()?;
        let elem_type = b & 0x0f;
        let mut count = (b >> 4) as usize;
        if count == 15 {
            count = self.read_varint()? as usize;
        }
        Ok((elem_type, count))
    }

    fn read_string(&mut self) -> Result<String, String> {
        let len = self.read_varint()? as usize;
        let end = self.pos.checked_add(len).ok_or("String length overflow")?;
        let slice = self.buf.get(self.pos..end).ok_or("String past EOF")?;
        self.pos = end;
        Ok(String::from_utf8_lossy(slice).into_owned())
    }

    fn skip_type(&mut self, ftype: u8) -> Result<(), String> {
        match ftype {
            1..=3 => {
                // bool / i8
                if ftype == 3 {
                    let _ = self.read_u8()?;
                }
            }
            4..=6 => {
                let _ = self.read_varint()?;
            }
            7 => {
                self.pos = self.pos.checked_add(8).ok_or("EOF")?;
            }
            8 => {
                let len = self.read_varint()? as usize;
                self.pos = self.pos.checked_add(len).ok_or("EOF")?;
            }
            9 | 10 => {
                let (et, c) = self.read_list_header()?;
                for _ in 0..c {
                    self.skip_type(et)?;
                }
            }
            11 => {
                // map
                let size = self.read_varint()? as usize;
                if size > 0 {
                    let kv = self.read_u8()?;
                    let kt = (kv >> 4) & 0x0f;
                    let vt = kv & 0x0f;
                    for _ in 0..size {
                        self.skip_type(kt)?;
                        self.skip_type(vt)?;
                    }
                }
            }
            12 => {
                let mut nested_last = 0i16;
                while let Some((ft, _)) = self.read_field_header(&mut nested_last)? {
                    self.skip_type(ft)?;
                }
            }

            _ => return Err(format!("Unknown Thrift field type {ftype}")),
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct TensorInfo {
    pub name: String,
    pub dtype: String,
    pub shape: Vec<u64>,
    pub params: u64,
    pub byte_size: u64,
}

#[derive(Debug, Clone)]
pub struct TensorDoc {
    pub format: &'static str,
    pub file_size: u64,
    pub total_params: u64,
    pub dtype_counts: Vec<(String, usize, u64)>,
    pub metadata: Vec<(String, String)>,
    pub tensors: Vec<TensorInfo>,
}

pub fn format_params(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.2}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn summarize_dtypes(tensors: &[TensorInfo]) -> (u64, Vec<(String, usize, u64)>) {
    let mut total_params = 0u64;
    let mut by_dtype: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    for t in tensors {
        total_params = total_params.saturating_add(t.params);
        let entry = by_dtype.entry(t.dtype.clone()).or_default();
        entry.0 += 1;
        entry.1 = entry.1.saturating_add(t.params);
    }
    let list = by_dtype
        .into_iter()
        .map(|(k, (cnt, p))| (k, cnt, p))
        .collect();
    (total_params, list)
}

pub fn load_safetensors(path: &Path) -> Result<TensorDoc, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("Could not open {}: {e}", path.display()))?;
    let file_size = f.metadata().map_err(|e| e.to_string())?.len();
    let mut len_buf = [0u8; 8];
    f.read_exact(&mut len_buf)
        .map_err(|e| format!("Invalid safetensors header: {e}"))?;
    let header_len = u64::from_le_bytes(len_buf);
    if header_len == 0 || header_len > 64 * 1024 * 1024 {
        return Err(format!("Safetensors header size out of bounds ({header_len} bytes)."));
    }
    let mut raw = vec![0u8; header_len as usize];
    f.read_exact(&mut raw)
        .map_err(|e| format!("Could not read safetensors header: {e}"))?;
    let root: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|e| format!("Invalid safetensors JSON header: {e}"))?;
    let obj = root
        .as_object()
        .ok_or_else(|| "Safetensors header is not a JSON object".to_owned())?;

    let mut metadata = Vec::new();
    let mut tensors = Vec::new();

    for (key, val) in obj {
        if key == "__metadata__" {
            if let Some(meta_obj) = val.as_object() {
                for (mk, mv) in meta_obj {
                    let v = match mv {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    metadata.push((mk.clone(), v));
                }
            }
            continue;
        }
        let Some(t_obj) = val.as_object() else { continue };
        let dtype = t_obj
            .get("dtype")
            .and_then(|v| v.as_str())
            .unwrap_or("UNKNOWN")
            .to_owned();
        let shape: Vec<u64> = t_obj
            .get("shape")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|x| x.as_u64()).collect())
            .unwrap_or_default();
        let params = if shape.is_empty() {
            1
        } else {
            shape.iter().copied().fold(1u64, u64::saturating_mul)
        };
        let byte_size = t_obj
            .get("data_offsets")
            .and_then(|v| v.as_array())
            .and_then(|arr| match (arr.first()?.as_u64(), arr.get(1)?.as_u64()) {
                (Some(a), Some(b)) if b >= a => Some(b - a),
                _ => None,
            })
            .unwrap_or(0);
        tensors.push(TensorInfo {
            name: key.clone(),
            dtype,
            shape,
            params,
            byte_size,
        });
    }

    metadata.sort_by(|a, b| a.0.cmp(&b.0));
    tensors.sort_by(|a, b| a.name.cmp(&b.name));
    let (total_params, dtype_counts) = summarize_dtypes(&tensors);

    Ok(TensorDoc {
        format: "Safetensors",
        file_size,
        total_params,
        dtype_counts,
        metadata,
        tensors,
    })
}

pub fn load_gguf(path: &Path) -> Result<TensorDoc, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("Could not open {}: {e}", path.display()))?;
    let file_size = f.metadata().map_err(|e| e.to_string())?.len();
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).map_err(|e| e.to_string())?;
    if &magic != b"GGUF" {
        return Err("Not a GGUF file (missing GGUF magic).".into());
    }
    let version = read_u32_le(&mut f)?;
    if !(2..=3).contains(&version) {
        return Err(format!("Unsupported GGUF version {version} (expected 2 or 3)."));
    }
    let tensor_count = read_u64_le(&mut f)?.min(200_000);
    let kv_count = read_u64_le(&mut f)?.min(200_000);

    let mut metadata = vec![("gguf.version".to_owned(), version.to_string())];
    for _ in 0..kv_count {
        let key = read_gguf_str(&mut f)?;
        let vtype = read_u32_le(&mut f)?;
        let val_str = read_gguf_value(&mut f, vtype, 0)?;
        if metadata.len() < 256 {
            metadata.push((key, val_str));
        }
    }

    let mut tensors = Vec::with_capacity(tensor_count as usize);
    for _ in 0..tensor_count {
        let name = read_gguf_str(&mut f)?;
        let n_dims = read_u32_le(&mut f)?.min(16) as usize;
        let mut shape = Vec::with_capacity(n_dims);
        for _ in 0..n_dims {
            shape.push(read_u64_le(&mut f)?);
        }
        let gtype = read_u32_le(&mut f)?;
        let _offset = read_u64_le(&mut f)?;
        let params = if shape.is_empty() {
            1
        } else {
            shape.iter().copied().fold(1u64, u64::saturating_mul)
        };
        let (dtype, byte_size) = ggml_type_info(gtype, params);
        tensors.push(TensorInfo {
            name,
            dtype: dtype.to_owned(),
            shape,
            params,
            byte_size,
        });
    }

    let (total_params, dtype_counts) = summarize_dtypes(&tensors);
    Ok(TensorDoc {
        format: "GGUF",
        file_size,
        total_params,
        dtype_counts,
        metadata,
        tensors,
    })
}

fn read_u32_le<R: Read>(r: &mut R) -> Result<u32, String> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).map_err(|e| e.to_string())?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64_le<R: Read>(r: &mut R) -> Result<u64, String> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b).map_err(|e| e.to_string())?;
    Ok(u64::from_le_bytes(b))
}

fn read_gguf_str<R: Read>(r: &mut R) -> Result<String, String> {
    let len = read_u64_le(r)?;
    if len > 4 * 1024 * 1024 {
        return Err(format!("GGUF string too large ({len} bytes)"));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn read_gguf_value<R: Read + Seek>(r: &mut R, vtype: u32, depth: usize) -> Result<String, String> {
    if depth > 4 {
        return Err("Nested GGUF array too deep".into());
    }
    let mut b1 = [0u8; 1];
    let mut b2 = [0u8; 2];
    let mut b4 = [0u8; 4];
    let mut b8 = [0u8; 8];
    match vtype {
        0 => { r.read_exact(&mut b1).map_err(|e| e.to_string())?; Ok(b1[0].to_string()) }
        1 => { r.read_exact(&mut b1).map_err(|e| e.to_string())?; Ok((b1[0] as i8).to_string()) }
        2 => { r.read_exact(&mut b2).map_err(|e| e.to_string())?; Ok(u16::from_le_bytes(b2).to_string()) }
        3 => { r.read_exact(&mut b2).map_err(|e| e.to_string())?; Ok(i16::from_le_bytes(b2).to_string()) }
        4 => { r.read_exact(&mut b4).map_err(|e| e.to_string())?; Ok(u32::from_le_bytes(b4).to_string()) }
        5 => { r.read_exact(&mut b4).map_err(|e| e.to_string())?; Ok(i32::from_le_bytes(b4).to_string()) }
        6 => { r.read_exact(&mut b4).map_err(|e| e.to_string())?; Ok(format!("{:.4}", f32::from_le_bytes(b4))) }
        7 => { r.read_exact(&mut b1).map_err(|e| e.to_string())?; Ok((b1[0] != 0).to_string()) }
        8 => read_gguf_str(r),
        9 => {
            let elem_t = read_u32_le(r)?;
            let count = read_u64_le(r)?;
            if let Some(elem_sz) = gguf_scalar_size(elem_t) {
                let skip = (elem_sz as i64).saturating_mul(count as i64);
                r.seek(SeekFrom::Current(skip)).map_err(|e| e.to_string())?;
                return Ok(format!("[{count} items]"));
            }
            let preview_limit = 3;
            let mut preview = Vec::new();
            for i in 0..count {
                let s = read_gguf_value(r, elem_t, depth + 1)?;
                if i < preview_limit {
                    preview.push(s);
                }
            }
            if count > preview_limit {
                Ok(format!("[{}, … ({count} total)]", preview.join(", ")))
            } else {
                Ok(format!("[{}]", preview.join(", ")))
            }
        }
        10 => { r.read_exact(&mut b8).map_err(|e| e.to_string())?; Ok(u64::from_le_bytes(b8).to_string()) }
        11 => { r.read_exact(&mut b8).map_err(|e| e.to_string())?; Ok(i64::from_le_bytes(b8).to_string()) }
        12 => { r.read_exact(&mut b8).map_err(|e| e.to_string())?; Ok(format!("{:.4}", f64::from_le_bytes(b8))) }
        other => Err(format!("Unsupported GGUF value type {other}")),
    }
}

fn gguf_scalar_size(t: u32) -> Option<u64> {
    match t {
        0 | 1 | 7 => Some(1),
        2 | 3 => Some(2),
        4..=6 => Some(4),
        10..=12 => Some(8),
        _ => None,
    }
}

fn ggml_type_info(t: u32, params: u64) -> (&'static str, u64) {
    match t {
        0 => ("F32", params.saturating_mul(4)),
        1 => ("F16", params.saturating_mul(2)),
        2 => ("Q4_0", (params / 32) * 18),
        3 => ("Q4_1", (params / 32) * 20),
        6 => ("Q5_0", (params / 32) * 22),
        7 => ("Q5_1", (params / 32) * 24),
        8 => ("Q8_0", (params / 32) * 34),
        9 => ("Q8_1", (params / 32) * 36),
        10 => ("Q2_K", (params / 256) * 84),
        11 => ("Q3_K", (params / 256) * 110),
        12 => ("Q4_K", (params / 256) * 144),
        13 => ("Q5_K", (params / 256) * 176),
        14 => ("Q6_K", (params / 256) * 210),
        15 => ("Q8_K", (params / 256) * 292),
        24 => ("I8", params),
        25 => ("I16", params.saturating_mul(2)),
        26 => ("I32", params.saturating_mul(4)),
        27 => ("I64", params.saturating_mul(8)),
        28 => ("F64", params.saturating_mul(8)),
        30 => ("BF16", params.saturating_mul(2)),
        _ => ("GGML", 0),
    }
}

pub fn load_onnx(path: &Path) -> Result<TensorDoc, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("Could not open {}: {e}", path.display()))?;
    let file_size = f.metadata().map_err(|e| e.to_string())?.len();
    let mut metadata = Vec::new();
    let mut tensors = Vec::new();

    parse_onnx_model(&mut f, file_size, &mut metadata, &mut tensors)?;
    let (total_params, dtype_counts) = summarize_dtypes(&tensors);

    Ok(TensorDoc {
        format: "ONNX",
        file_size,
        total_params,
        dtype_counts,
        metadata,
        tensors,
    })
}

fn pb_varint<R: Read>(r: &mut R, pos: &mut u64) -> Result<Option<u64>, String> {
    let mut b = [0u8; 1];
    match r.read_exact(&mut b) {
        Ok(()) => *pos += 1,
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.to_string()),
    }
    let mut res = (b[0] & 0x7f) as u64;
    let mut shift = 7u32;
    let mut cur = b[0];
    while cur & 0x80 != 0 {
        r.read_exact(&mut b).map_err(|e| e.to_string())?;
        *pos += 1;
        cur = b[0];
        if shift >= 64 {
            return Err("Protobuf varint overflow".into());
        }
        res |= ((cur & 0x7f) as u64) << shift;
        shift += 7;
    }
    Ok(Some(res))
}

fn pb_skip<R: Read + Seek>(r: &mut R, wire: u64, pos: &mut u64) -> Result<(), String> {
    match wire {
        0 => {
            let _ = pb_varint(r, pos)?;
        }
        1 => {
            r.seek(SeekFrom::Current(8)).map_err(|e| e.to_string())?;
            *pos += 8;
        }
        2 => {
            let len = pb_varint(r, pos)?.ok_or("Unexpected EOF")?;
            r.seek(SeekFrom::Current(len as i64)).map_err(|e| e.to_string())?;
            *pos += len;
        }
        5 => {
            r.seek(SeekFrom::Current(4)).map_err(|e| e.to_string())?;
            *pos += 4;
        }
        _ => return Err(format!("Unsupported protobuf wire type {wire}")),
    }
    Ok(())
}

fn pb_read_string<R: Read + Seek>(r: &mut R, pos: &mut u64) -> Result<String, String> {
    let len = pb_varint(r, pos)?.ok_or("Unexpected EOF")?;
    if len > 4 * 1024 * 1024 {
        r.seek(SeekFrom::Current(len as i64)).map_err(|e| e.to_string())?;
        *pos += len;
        return Ok(format!("<{len} bytes>"));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).map_err(|e| e.to_string())?;
    *pos += len;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn parse_onnx_model<R: Read + Seek>(
    r: &mut R,
    limit: u64,
    meta: &mut Vec<(String, String)>,
    tensors: &mut Vec<TensorInfo>,
) -> Result<(), String> {
    let mut pos = 0u64;
    while pos < limit {
        let Some(tag) = pb_varint(r, &mut pos)? else { break };
        let field = tag >> 3;
        let wire = tag & 7;
        match (field, wire) {
            (1, 0) => {
                if let Some(ir) = pb_varint(r, &mut pos)? {
                    meta.push(("ir_version".into(), ir.to_string()));
                }
            }
            (2, 2) => {
                let s = pb_read_string(r, &mut pos)?;
                if !s.is_empty() {
                    meta.push(("producer".into(), s));
                }
            }
            (3, 2) => {
                let s = pb_read_string(r, &mut pos)?;
                if !s.is_empty() {
                    meta.push(("producer_version".into(), s));
                }
            }
            (7, 2) => {
                // GraphProto
                let len = pb_varint(r, &mut pos)?.ok_or("EOF")?;
                let end = pos + len;
                parse_onnx_graph(r, &mut pos, end, meta, tensors)?;
            }
            _ => pb_skip(r, wire, &mut pos)?,
        }
    }
    Ok(())
}

fn parse_onnx_graph<R: Read + Seek>(
    r: &mut R,
    pos: &mut u64,
    end: u64,
    meta: &mut Vec<(String, String)>,
    tensors: &mut Vec<TensorInfo>,
) -> Result<(), String> {
    let mut node_count = 0usize;
    while *pos < end {
        let Some(tag) = pb_varint(r, pos)? else { break };
        let field = tag >> 3;
        let wire = tag & 7;
        match (field, wire) {
            (1, 2) => {
                node_count += 1;
                pb_skip(r, wire, pos)?;
            }
            (2, 2) => {
                let s = pb_read_string(r, pos)?;
                if !s.is_empty() {
                    meta.push(("graph".into(), s));
                }
            }
            (5, 2) => {
                // TensorProto initializer
                let len = pb_varint(r, pos)?.ok_or("EOF")?;
                let tend = *pos + len;
                tensors.push(parse_onnx_tensor(r, pos, tend)?);
            }
            _ => pb_skip(r, wire, pos)?,
        }
    }
    if node_count > 0 {
        meta.push(("nodes".into(), node_count.to_string()));
    }
    Ok(())
}

fn parse_onnx_tensor<R: Read + Seek>(r: &mut R, pos: &mut u64, end: u64) -> Result<TensorInfo, String> {
    let mut shape = Vec::new();
    let mut dtype_id = 0u64;
    let mut name = String::new();
    let mut byte_size = 0u64;

    while *pos < end {
        let Some(tag) = pb_varint(r, pos)? else { break };
        let field = tag >> 3;
        let wire = tag & 7;
        match (field, wire) {
            (1, 0) => {
                if let Some(d) = pb_varint(r, pos)? {
                    shape.push(d);
                }
            }
            (1, 2) => {
                let len = pb_varint(r, pos)?.ok_or("EOF")?;
                let pend = *pos + len;
                while *pos < pend {
                    if let Some(d) = pb_varint(r, pos)? {
                        shape.push(d);
                    }
                }
            }
            (2, 0) => {
                dtype_id = pb_varint(r, pos)?.unwrap_or(0);
            }
            (8, 2) => {
                name = pb_read_string(r, pos)?;
            }
            (9, 2) => {
                // raw_data: zero-copy seek past weights!
                let len = pb_varint(r, pos)?.ok_or("EOF")?;
                byte_size = byte_size.saturating_add(len);
                r.seek(SeekFrom::Current(len as i64)).map_err(|e| e.to_string())?;
                *pos += len;
            }
            _ => pb_skip(r, wire, pos)?,
        }
    }
    let dtype = onnx_dtype_name(dtype_id);
    let params = if shape.is_empty() {
        1
    } else {
        shape.iter().copied().fold(1u64, u64::saturating_mul)
    };
    if byte_size == 0 {
        byte_size = params.saturating_mul(onnx_dtype_bytes(dtype_id));
    }
    Ok(TensorInfo {
        name: if name.is_empty() { "<unnamed>".into() } else { name },
        dtype: dtype.to_owned(),
        shape,
        params,
        byte_size,
    })
}

fn onnx_dtype_name(id: u64) -> &'static str {
    match id {
        1 => "FLOAT",
        2 => "UINT8",
        3 => "INT8",
        4 => "UINT16",
        5 => "INT16",
        6 => "INT32",
        7 => "INT64",
        9 => "BOOL",
        10 => "FLOAT16",
        11 => "DOUBLE",
        12 => "UINT32",
        13 => "UINT64",
        16 => "BFLOAT16",
        _ => "UNKNOWN",
    }
}

fn onnx_dtype_bytes(id: u64) -> u64 {
    match id {
        2 | 3 | 9 => 1,
        4 | 5 | 10 | 16 => 2,
        1 | 6 | 12 => 4,
        7 | 11 | 13 => 8,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_file(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("neutron-vf-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir.join(name)
    }

    #[test]
    fn markdown_parses_headings_code_lists_and_quotes() {
        let md = "# Title\n\nHello world.\n\n- one\n- two\n\n> note\n\n```rust\nfn main() {}\n```\n\n---\n";
        let blocks = parse_markdown(md);
        assert!(matches!(&blocks[0], MdBlock::Heading { level: 1, text } if text == "Title"));
        assert!(matches!(&blocks[1], MdBlock::Paragraph(p) if p == "Hello world."));
        assert!(matches!(&blocks[2], MdBlock::Bullet { indent: 0, text } if text == "one"));
        assert!(matches!(&blocks[4], MdBlock::Quote(q) if q == "note"));
        assert!(matches!(&blocks[5], MdBlock::CodeBlock { lang, code } if lang == "rust" && code == "fn main() {}"));
        assert!(matches!(&blocks[6], MdBlock::Rule));
    }

    #[test]
    fn csv_parser_handles_quotes_and_sorting() {
        let csv = "name,score,note\nalpha,10,\"hello, world\"\nbeta,2,\"quote \"\"x\"\"\"\n";
        let mut t = parse_delimited(csv, b',', "CSV", 100);
        assert_eq!(t.columns, vec!["name", "score", "note"]);
        assert_eq!(t.total_rows, 2);
        assert_eq!(t.rows[0][2], "hello, world");
        assert_eq!(t.rows[1][2], "quote \"x\"");

        t.sort_by(1); // ascending numeric: 2 before 10
        assert_eq!(t.ordered_row(0).unwrap()[0], "beta");
        assert_eq!(t.ordered_row(1).unwrap()[0], "alpha");
    }

    #[test]
    fn jsonl_parser_extracts_union_of_keys() {
        let j = "{\"id\":1,\"name\":\"a\"}\n{\"id\":2,\"score\":9.5}\n";
        let t = parse_jsonl(j, 100);
        assert_eq!(t.total_rows, 2);
        assert_eq!(t.columns.len(), 3);
        assert!(t.columns.contains(&"id".to_owned()));
        assert!(t.columns.contains(&"name".to_owned()));
        assert!(t.columns.contains(&"score".to_owned()));
    }

    #[test]
    fn safetensors_header_parses_zero_copy() {
        let p = scratch_file("model.safetensors");
        let hdr = br#"{"__metadata__":{"format":"pt"},"w1":{"dtype":"BF16","shape":[2,4],"data_offsets":[0,16]},"b1":{"dtype":"F32","shape":[4],"data_offsets":[16,32]}}"#;
        let mut bytes = (hdr.len() as u64).to_le_bytes().to_vec();
        bytes.extend_from_slice(hdr);
        bytes.extend_from_slice(&[0u8; 32]);
        std::fs::write(&p, bytes).unwrap();

        let doc = load_safetensors(&p).unwrap();
        let _ = std::fs::remove_file(&p);
        assert_eq!(doc.format, "Safetensors");
        assert_eq!(doc.total_params, 12);
        assert_eq!(doc.tensors.len(), 2);
        assert_eq!(doc.metadata, vec![("format".into(), "pt".into())]);
    }

    #[test]
    fn gguf_header_parses_metadata_and_tensors() {
        let p = scratch_file("mini.gguf");
        let mut buf = Vec::new();
        buf.extend_from_slice(b"GGUF");
        buf.extend_from_slice(&3u32.to_le_bytes());
        buf.extend_from_slice(&1u64.to_le_bytes()); // tensor_count
        buf.extend_from_slice(&1u64.to_le_bytes()); // kv_count
        // kv: general.architecture = "llama"
        let k = b"general.architecture";
        buf.extend_from_slice(&(k.len() as u64).to_le_bytes());
        buf.extend_from_slice(k);
        buf.extend_from_slice(&8u32.to_le_bytes()); // STRING
        let v = b"llama";
        buf.extend_from_slice(&(v.len() as u64).to_le_bytes());
        buf.extend_from_slice(v);
        // tensor: tok.weight [128, 64] Q4_K (12)
        let tn = b"tok.weight";
        buf.extend_from_slice(&(tn.len() as u64).to_le_bytes());
        buf.extend_from_slice(tn);
        buf.extend_from_slice(&2u32.to_le_bytes());
        buf.extend_from_slice(&128u64.to_le_bytes());
        buf.extend_from_slice(&64u64.to_le_bytes());
        buf.extend_from_slice(&12u32.to_le_bytes()); // Q4_K
        buf.extend_from_slice(&0u64.to_le_bytes());
        std::fs::write(&p, buf).unwrap();

        let doc = load_gguf(&p).unwrap();
        let _ = std::fs::remove_file(&p);
        assert_eq!(doc.format, "GGUF");
        assert_eq!(doc.tensors.len(), 1);
        assert_eq!(doc.tensors[0].dtype, "Q4_K");
        assert_eq!(doc.total_params, 128 * 64);
        assert!(doc.metadata.iter().any(|(mk, mv)| mk == "general.architecture" && mv == "llama"));
    }

    #[test]
    fn onnx_header_parses_initializers() {
        let p = scratch_file("mini.onnx");
        // TensorProto: field 1=dims(4,2), field 2=dtype(1=FLOAT), field 8=name("w"), field 9=raw_data(32 bytes)
        let mut tp = Vec::new();
        tp.extend_from_slice(&[0x08, 4, 0x08, 2, 0x10, 1, 0x42, 1, b'w', 0x4a, 32]);
        tp.extend_from_slice(&[0u8; 32]);
        // GraphProto: field 2=name("g"), field 5=initializer(tp)
        let mut gp = vec![0x12, 1, b'g', 0x2a, tp.len() as u8];
        gp.extend_from_slice(&tp);
        // ModelProto: field 1=ir_version(9), field 7=graph(gp)
        let mut mp = vec![0x08, 9, 0x3a, gp.len() as u8];
        mp.extend_from_slice(&gp);
        std::fs::write(&p, mp).unwrap();

        let doc = load_onnx(&p).unwrap();
        let _ = std::fs::remove_file(&p);
        assert_eq!(doc.format, "ONNX");
        assert_eq!(doc.tensors.len(), 1);
        assert_eq!(doc.tensors[0].name, "w");
        assert_eq!(doc.tensors[0].dtype, "FLOAT");
        assert_eq!(doc.tensors[0].shape, vec![4, 2]);
        assert_eq!(doc.tensors[0].byte_size, 32);
    }

    #[test]
    fn parquet_footer_parses_schema_and_row_count() {
        // Minimal Thrift Compact FileMetaData:
        // field 1 (i32) = version 2 -> delta 1 type 5 => 0x15, zigzag(2)=4
        // field 3 (i64) = num_rows 42 -> delta 2 type 6 => 0x26, zigzag(42)=84
        // STOP => 0x00
        let footer = [0x15u8, 4, 0x26, 84, 0x00];
        let t = parse_parquet_footer(&footer).unwrap();
        assert_eq!(t.format_label, "Parquet Schema");
        assert!(t.meta.contains(&("Rows".into(), "42".into())));
        assert!(t.meta.contains(&("Format Version".into(), "2".into())));
    }
}
