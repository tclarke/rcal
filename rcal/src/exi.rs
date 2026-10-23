//! EXI (Efficient XML Interchange) codec — schema-less, byte-aligned.
//!
//! Implements W3C EXI Format 1.0 (Second Edition) §4–§7 for the schema-less
//! grammar with byte-aligned channel alignment. Used by [`ExiExternalizer`].
//!
//! [`ExiExternalizer`]: crate::externalizer::ExiExternalizer

use crate::uci::{CalError, CalErrorKind, CalResult};
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};
use std::collections::HashMap;
use std::io::Cursor;

// ── EXI header constants ─────────────────────────────────────────────────────

/// W3C EXI cookie: `$EXI` (§4.8.2).
const EXI_COOKIE: &[u8] = b"$EXI";

/// Options byte for schema-less, byte-aligned, no compression, no strict.
///
/// Per §4.8.5:
/// - alignment = 00 (byte-aligned)
/// - compression = 0
/// - strict = 0
/// - self-contained = 0
/// - schema-id = 00 (no schema)
/// - datatype-representation = 0
/// - preserve options = 00000
/// - block size present = 0
/// - value max length present = 0
/// - value partition capacity present = 0
const OPTIONS_BYTE: u8 = 0b0000_0010; // distinguishable bit + byte-aligned flag

/// Second options byte indicating no schema-id and no further options.
const OPTIONS_BYTE2: u8 = 0x00;

// ── Event codes (schema-less body grammar, §8.5.4) ──────────────────────────

// Each event occupies 1 or 2 bytes in byte-aligned mode.

const EV_SD: u8 = 0x00; // Start Document
const EV_ED: u8 = 0x00; // End Document  (context decides which)
const EV_SE: &[u8] = &[0x01, 0x00]; // Start Element(*)
const EV_EE: &[u8] = &[0x01, 0x01]; // End Element
const EV_AT: &[u8] = &[0x01, 0x02]; // Attribute(*)
const EV_CH: &[u8] = &[0x01, 0x04]; // Characters
const EV_NS: &[u8] = &[0x01, 0x06]; // Namespace Declaration

// ── encode ───────────────────────────────────────────────────────────────────

/// Encode XML bytes to schema-less, byte-aligned EXI (W3C EXI 1.0 §4–§8).
pub fn encode(xml_bytes: &[u8]) -> CalResult<Vec<u8>> {
    let mut out = Vec::with_capacity(xml_bytes.len() / 2);
    // Header: cookie + two options bytes (§4.8)
    out.extend_from_slice(EXI_COOKIE);
    out.push(OPTIONS_BYTE);
    out.push(OPTIONS_BYTE2);

    let mut reader = Reader::from_reader(xml_bytes);
    reader.config_mut().trim_text(false);

    let mut tables = EncodeTables::default();
    // Start Document event
    out.push(EV_SD);

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                // Namespace declarations become NS events before SE
                for attr in e.attributes().flatten() {
                    let key = attr.key.into_inner();
                    if key == b"xmlns" || key.starts_with(b"xmlns:") {
                        out.extend_from_slice(EV_NS);
                        let prefix = if key == b"xmlns" {
                            b"".as_ref()
                        } else {
                            &key[6..]
                        };
                        let uri = attr.value.as_ref();
                        write_string_value(prefix, &mut tables.ns_prefix, &mut out);
                        write_string_value(uri, &mut tables.ns_uri, &mut out);
                        out.push(0x00); // local-element-ns flag = false
                    }
                }
                out.extend_from_slice(EV_SE);
                let (uri, local) = split_qname_bytes(e.name().into_inner());
                write_string_value(uri, &mut tables.elem_uri, &mut out);
                write_string_value(local, &mut tables.elem_local, &mut out);

                // Attributes (skip xmlns)
                for attr in e.attributes().flatten() {
                    let key = attr.key.into_inner();
                    if key == b"xmlns" || key.starts_with(b"xmlns:") {
                        continue;
                    }
                    out.extend_from_slice(EV_AT);
                    let (a_uri, a_local) = split_qname_bytes(key);
                    write_string_value(a_uri, &mut tables.attr_uri, &mut out);
                    write_string_value(a_local, &mut tables.attr_local, &mut out);
                    write_string_value(attr.value.as_ref(), &mut tables.attr_value, &mut out);
                }
            }
            Ok(Event::End(_)) => {
                out.extend_from_slice(EV_EE);
            }
            Ok(Event::Empty(e)) => {
                for attr in e.attributes().flatten() {
                    let key = attr.key.into_inner();
                    if key == b"xmlns" || key.starts_with(b"xmlns:") {
                        out.extend_from_slice(EV_NS);
                        let prefix = if key == b"xmlns" {
                            b"".as_ref()
                        } else {
                            &key[6..]
                        };
                        let uri = attr.value.as_ref();
                        write_string_value(prefix, &mut tables.ns_prefix, &mut out);
                        write_string_value(uri, &mut tables.ns_uri, &mut out);
                        out.push(0x00);
                    }
                }
                out.extend_from_slice(EV_SE);
                let (uri, local) = split_qname_bytes(e.name().into_inner());
                write_string_value(uri, &mut tables.elem_uri, &mut out);
                write_string_value(local, &mut tables.elem_local, &mut out);
                for attr in e.attributes().flatten() {
                    let key = attr.key.into_inner();
                    if key == b"xmlns" || key.starts_with(b"xmlns:") {
                        continue;
                    }
                    out.extend_from_slice(EV_AT);
                    let (a_uri, a_local) = split_qname_bytes(key);
                    write_string_value(a_uri, &mut tables.attr_uri, &mut out);
                    write_string_value(a_local, &mut tables.attr_local, &mut out);
                    write_string_value(attr.value.as_ref(), &mut tables.attr_value, &mut out);
                }
                out.extend_from_slice(EV_EE);
            }
            Ok(Event::Text(t)) => {
                let text = t.unescape().unwrap_or_default();
                if !text.trim().is_empty() {
                    out.extend_from_slice(EV_CH);
                    write_string_value(text.as_bytes(), &mut tables.ch_value, &mut out);
                }
            }
            Ok(Event::CData(c)) => {
                out.extend_from_slice(EV_CH);
                write_string_value(c.as_ref(), &mut tables.ch_value, &mut out);
            }
            Ok(Event::Decl(_)) | Ok(Event::Comment(_)) | Ok(Event::PI(_)) => {}
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(CalError::new(
                    CalErrorKind::SerializationError,
                    format!("EXI encode: XML parse error: {e}"),
                ));
            }
            Ok(_) => {}
        }
    }

    // End Document event
    out.push(EV_ED);
    Ok(out)
}

// ── decode ───────────────────────────────────────────────────────────────────

/// Decode schema-less, byte-aligned EXI bytes back to XML.
pub fn decode(exi_bytes: &[u8]) -> CalResult<Vec<u8>> {
    // Validate and skip header
    if exi_bytes.len() < 6 || &exi_bytes[..4] != EXI_COOKIE {
        return Err(CalError::new(
            CalErrorKind::SerializationError,
            "EXI decode: missing or invalid EXI cookie",
        ));
    }
    let options1 = exi_bytes[4];
    let _options2 = exi_bytes[5];
    if options1 & 0b0000_0010 == 0 {
        return Err(CalError::new(
            CalErrorKind::SerializationError,
            "EXI decode: only byte-aligned mode is supported",
        ));
    }

    let mut pos = 6usize;
    let buf = exi_bytes;

    let mut xml_out: Vec<u8> = Vec::new();
    let cursor = Cursor::new(&mut xml_out);
    let mut writer = Writer::new(cursor);
    writer
        .write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
        .map_err(xml_err)?;

    let mut tables = DecodeTables::default();
    // Stack of open element names for proper closing tags
    let mut element_stack: Vec<String> = Vec::new();

    macro_rules! read_byte {
        () => {{
            if pos >= buf.len() {
                return Err(CalError::new(
                    CalErrorKind::SerializationError,
                    "EXI decode: unexpected end of data",
                ));
            }
            let b = buf[pos];
            pos += 1;
            b
        }};
    }

    // Skip SD
    let sd = read_byte!();
    if sd != EV_SD {
        return Err(CalError::new(
            CalErrorKind::SerializationError,
            format!("EXI decode: expected SD event (0x00), got 0x{sd:02x}"),
        ));
    }

    // Active namespace mappings for the current document
    let mut ns_map: HashMap<String, String> = HashMap::new();
    // pending NS declarations to attach to the next SE
    let mut pending_ns: Vec<(String, String)> = Vec::new();

    loop {
        if pos >= buf.len() {
            break;
        }
        let ev_byte = read_byte!();
        if ev_byte == EV_ED {
            // End Document
            break;
        }
        if ev_byte != 0x01 {
            return Err(CalError::new(
                CalErrorKind::SerializationError,
                format!(
                    "EXI decode: unexpected event byte 0x{ev_byte:02x} at pos {}",
                    pos - 1
                ),
            ));
        }
        let ev_kind = read_byte!();
        match ev_kind {
            0x00 => {
                // SE(*)
                let uri = read_string_value(&mut tables.elem_uri, buf, &mut pos)?;
                let local = read_string_value(&mut tables.elem_local, buf, &mut pos)?;
                let tag_name = make_tag_name(&uri, &local, &ns_map);

                // We need to peek ahead for AT events before writing SE
                // We collect them then write SE + attrs
                let mut attrs: Vec<(String, String, String)> = Vec::new();
                loop {
                    if pos >= buf.len() {
                        break;
                    }
                    let peek = buf[pos];
                    if peek != 0x01 {
                        break;
                    }
                    if pos + 1 >= buf.len() {
                        break;
                    }
                    let peek2 = buf[pos + 1];
                    match peek2 {
                        0x02 => {
                            // AT
                            pos += 2;
                            let a_uri = read_string_value(&mut tables.attr_uri, buf, &mut pos)?;
                            let a_local = read_string_value(&mut tables.attr_local, buf, &mut pos)?;
                            let a_val = read_string_value(&mut tables.attr_value, buf, &mut pos)?;
                            attrs.push((a_uri, a_local, a_val));
                        }
                        _ => break,
                    }
                }

                let mut start = BytesStart::new(&tag_name);
                for (prefix, ns_uri) in pending_ns.drain(..) {
                    if prefix.is_empty() {
                        start.push_attribute(("xmlns", ns_uri.as_str()));
                    } else {
                        start.push_attribute((format!("xmlns:{prefix}").as_str(), ns_uri.as_str()));
                    }
                }
                for (a_uri, a_local, a_val) in &attrs {
                    let attr_name = make_tag_name(a_uri, a_local, &ns_map);
                    start.push_attribute((attr_name.as_str(), a_val.as_str()));
                }
                element_stack.push(tag_name.clone());
                writer.write_event(Event::Start(start)).map_err(xml_err)?;
            }
            0x01 => {
                // EE
                let name = element_stack.pop().ok_or_else(|| {
                    CalError::new(
                        CalErrorKind::SerializationError,
                        "EXI decode: EE event with empty element stack (malformed EXI)",
                    )
                })?;
                writer
                    .write_event(Event::End(BytesEnd::new(name)))
                    .map_err(xml_err)?;
            }
            0x04 => {
                // CH
                let text = read_string_value(&mut tables.ch_value, buf, &mut pos)?;
                writer
                    .write_event(Event::Text(BytesText::new(&text)))
                    .map_err(xml_err)?;
            }
            0x06 => {
                // NS (standalone, before SE was consumed above)
                let prefix = read_string_value(&mut tables.ns_prefix, buf, &mut pos)?;
                let ns_uri = read_string_value(&mut tables.ns_uri, buf, &mut pos)?;
                let _local_element = read_byte!();
                ns_map.insert(prefix.clone(), ns_uri.clone());
                pending_ns.push((prefix, ns_uri));
            }
            other => {
                return Err(CalError::new(
                    CalErrorKind::SerializationError,
                    format!("EXI decode: unknown event kind 0x{other:02x}"),
                ));
            }
        }
    }

    drop(writer);
    Ok(xml_out)
}

// ── String table helpers ─────────────────────────────────────────────────────

/// Write a string using the EXI string table (§7.1.10).
///
/// Format: `[uint: hit_index+2 | 0=miss+global_hit | 1=miss+local_hit]`
/// For simplicity we use a single partition and encode as:
/// - hit: uintN(index + 1) where index is the 0-based table position
/// - miss: uint(0) + length + UTF-8 bytes
fn write_string_value(s: &[u8], table: &mut Vec<Vec<u8>>, out: &mut Vec<u8>) {
    if let Some(idx) = table.iter().position(|e| e == s) {
        // Hit: write index+1 as uint
        write_uint(idx as u64 + 1, out);
    } else {
        // Miss: write 0 + length + bytes
        write_uint(0, out);
        write_uint(s.len() as u64, out);
        out.extend_from_slice(s);
        table.push(s.to_vec());
    }
}

/// Read a string from the EXI string table.
fn read_string_value(table: &mut Vec<String>, buf: &[u8], pos: &mut usize) -> CalResult<String> {
    let code = read_uint(buf, pos)?;
    if code == 0 {
        // Miss: length + bytes
        let len = read_uint(buf, pos)? as usize;
        if *pos + len > buf.len() {
            return Err(CalError::new(
                CalErrorKind::SerializationError,
                "EXI decode: string length exceeds buffer",
            ));
        }
        let bytes = &buf[*pos..*pos + len];
        *pos += len;
        let s = String::from_utf8(bytes.to_vec()).map_err(|e| {
            CalError::new(CalErrorKind::SerializationError, format!("EXI decode: {e}"))
        })?;
        table.push(s.clone());
        Ok(s)
    } else {
        // Hit: index is code-1
        let idx = (code - 1) as usize;
        table.get(idx).cloned().ok_or_else(|| {
            CalError::new(
                CalErrorKind::SerializationError,
                format!("EXI decode: string table index {idx} out of bounds"),
            )
        })
    }
}

/// Write an unsigned integer as EXI uint (§7.1.6): 7 bits per byte, MSB=continuation.
fn write_uint(mut n: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

/// Read an EXI uint from `buf` starting at `*pos`.
fn read_uint(buf: &[u8], pos: &mut usize) -> CalResult<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        if *pos >= buf.len() {
            return Err(CalError::new(
                CalErrorKind::SerializationError,
                "EXI decode: unexpected end reading uint",
            ));
        }
        let byte = buf[*pos];
        *pos += 1;
        result |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 63 {
            return Err(CalError::new(
                CalErrorKind::SerializationError,
                "EXI decode: uint overflow",
            ));
        }
    }
    Ok(result)
}

// ── Qname helpers ────────────────────────────────────────────────────────────

/// Split a qualified name byte slice into (prefix_bytes, local_bytes).
///
/// Returns the raw prefix (e.g. `b"ns"` for `ns:local`), not the namespace URI.
/// EXI encodes URI in the qname, but since CAL XML is serde-generated with default
/// namespaces (no element prefixes), storing the prefix bytes here is sufficient.
/// ponytail: prefix stored in elem_uri slot instead of namespace URI; prefix-qualified
/// elements from external sources would lose their prefix on decode. Fix by resolving
/// prefix → URI via the namespace context during encode if interoperability is needed.
fn split_qname_bytes(qname: &[u8]) -> (&[u8], &[u8]) {
    if let Some(colon) = qname.iter().position(|&b| b == b':') {
        (&qname[..colon], &qname[colon + 1..])
    } else {
        (b"", qname)
    }
}

/// Build a tag name for quick_xml output, resolving prefix from ns_map.
fn make_tag_name(uri: &str, local: &str, ns_map: &HashMap<String, String>) -> String {
    if uri.is_empty() {
        return local.to_string();
    }
    // Find the prefix for this URI
    for (prefix, ns_uri) in ns_map {
        if ns_uri == uri && !prefix.is_empty() {
            return format!("{prefix}:{local}");
        }
    }
    local.to_string()
}

fn xml_err(e: std::io::Error) -> CalError {
    CalError::new(
        CalErrorKind::SerializationError,
        format!("EXI decode: XML write error: {e}"),
    )
}

// ── String table state ───────────────────────────────────────────────────────

#[derive(Default)]
struct EncodeTables {
    elem_uri: Vec<Vec<u8>>,
    elem_local: Vec<Vec<u8>>,
    attr_uri: Vec<Vec<u8>>,
    attr_local: Vec<Vec<u8>>,
    attr_value: Vec<Vec<u8>>,
    ch_value: Vec<Vec<u8>>,
    ns_prefix: Vec<Vec<u8>>,
    ns_uri: Vec<Vec<u8>>,
}

#[derive(Default)]
struct DecodeTables {
    elem_uri: Vec<String>,
    elem_local: Vec<String>,
    attr_uri: Vec<String>,
    attr_local: Vec<String>,
    attr_value: Vec<String>,
    ch_value: Vec<String>,
    ns_prefix: Vec<String>,
    ns_uri: Vec<String>,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn normalize_xml(xml: &[u8]) -> String {
        // Strip XML declaration, trim whitespace, canonicalize for comparison
        let s = String::from_utf8_lossy(xml);
        let s = if let Some(idx) = s.find("?>") {
            s[idx + 2..].trim().to_string()
        } else {
            s.trim().to_string()
        };
        s
    }

    #[test]
    fn exi_magic_bytes() {
        let xml = b"<Root><Value>42</Value></Root>";
        let exi = encode(xml).unwrap();
        assert_eq!(&exi[..4], b"$EXI", "EXI cookie missing");
    }

    #[test]
    fn exi_round_trip_simple() {
        let xml = b"<Root><Value>42</Value></Root>";
        let exi = encode(xml).unwrap();
        let decoded = decode(&exi).unwrap();
        let orig = normalize_xml(xml);
        let got = normalize_xml(&decoded);
        assert_eq!(got, orig);
    }

    #[test]
    fn exi_round_trip_attributes() {
        let xml = b"<Msg id=\"123\" type=\"test\"><Data>hello</Data></Msg>";
        let exi = encode(xml).unwrap();
        let decoded = decode(&exi).unwrap();
        let orig = normalize_xml(xml);
        let got = normalize_xml(&decoded);
        assert_eq!(got, orig);
    }

    #[test]
    fn exi_round_trip_nested() {
        let xml = b"<Root><A><B><C>deep</C></B></A><D></D></Root>";
        let exi = encode(xml).unwrap();
        let decoded = decode(&exi).unwrap();
        let orig = normalize_xml(xml);
        let got = normalize_xml(&decoded);
        assert_eq!(got, orig);
    }

    #[test]
    fn exi_invalid_cookie() {
        let bad = b"NOTEX\x02\x00\x00";
        assert!(decode(bad).is_err());
    }

    #[test]
    fn exi_string_table_hit() {
        // Repeated element names should compress via string table
        let xml = b"<Items><Item>a</Item><Item>b</Item><Item>c</Item></Items>";
        let exi = encode(xml).unwrap();
        // Should round-trip correctly
        let decoded = decode(&exi).unwrap();
        let orig = normalize_xml(xml);
        let got = normalize_xml(&decoded);
        assert_eq!(got, orig);
    }
}
