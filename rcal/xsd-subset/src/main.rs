//! Developer tool to create a subset of a schema file.
//!
//! This tool extracts the specified messages from a schema file and writes them to another schema file.
//! The specified message elements and all required types are included in the subset. After creating a subset.xsd
//! you can use `export RCAL_XSD_PATH=/path/to/subset.xsd` to only generate the subset elements. Alternatively you can
//! add the following to `.cargo/config.toml`
//! ```toml
//! [env]
//! RCAL_XSD_PATH="/path/to/subset.xsd"
//! ````
//! # Note
//! This is generally used to create a schema for a particular system containing all possible elements.
//! If you are building a service and only want to build struct for the elements used in that service
//! it is preferable to add this to `.cargo/config.toml` instead.
//! ```toml
//! [env]
//! RCAL_CALCONFIG_PATH="/path/to/services/CALConfig.toml"
//! ```
//!
//! This will extract, at compile time, the needed messages from the calconfig file used by the service.
//!
//! # Usage
//!   cargo run --bin xsd-subset -- --schema </path/to/full.xsd> --output </path/to/subset.xsd> \[message...]
//!
//! where `message` is a the name of an OMS Message you want in the subset.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use quick_xml::events::Event;
use quick_xml::reader::Reader;

const TOP_LEVEL_TAGS: &[&str] = &[
    "element",
    "complexType",
    "simpleType",
    "group",
    "attributeGroup",
    "attribute",
];

fn read_file(path: &Path) -> String {
    let raw = fs::read(path).unwrap_or_else(|e| {
        eprintln!("Error reading {}: {e}", path.display());
        std::process::exit(1);
    });
    let bytes = if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &raw[3..]
    } else {
        &raw
    };
    String::from_utf8_lossy(bytes).into_owned()
}

fn local_name_of(key: &[u8]) -> &str {
    let s = std::str::from_utf8(key).unwrap_or("");
    s.rfind(':').map(|i| &s[i + 1..]).unwrap_or(s)
}

fn attr_str(e: &quick_xml::events::BytesStart, key: &str) -> Option<String> {
    e.attributes()
        .filter_map(|a| a.ok())
        .find(|a| local_name_of(a.key.as_ref()) == key)
        .and_then(|a| {
            std::str::from_utf8(a.value.as_ref())
                .ok()
                .map(str::to_owned)
        })
}

fn split_lines_keep_ends(s: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0;
    for (i, ch) in s.char_indices() {
        if ch == '\n' {
            result.push(&s[start..=i]);
            start = i + 1;
        }
    }
    if start < s.len() {
        result.push(&s[start..]);
    }
    result
}

fn is_top_level_def(line: &str) -> bool {
    if !line.starts_with('\t') {
        return false;
    }
    let rest = &line[1..];
    if !rest.starts_with("<xs:") {
        return false;
    }
    let tag = &rest[4..];
    TOP_LEVEL_TAGS.iter().any(|t| {
        tag.starts_with(t) && {
            let after = &tag[t.len()..];
            after.is_empty()
                || !after.starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '-')
        }
    })
}

fn extract_name_attr(line: &str) -> Option<String> {
    let pos = line.find("name=\"")?;
    let after = &line[pos + 6..];
    let end = after.find('"')?;
    Some(after[..end].to_owned())
}

fn extract_blocks(path: &Path) -> HashMap<String, String> {
    let content = read_file(path);
    let lines = split_lines_keep_ends(&content);
    let mut blocks: HashMap<String, String> = HashMap::new();
    let mut current_name: Option<String> = None;
    let mut current_start = 0usize;

    for (i, line) in lines.iter().enumerate() {
        let stripped = line.trim_end_matches(['\r', '\n']);
        if is_top_level_def(stripped) {
            if let Some(name) = current_name.take() {
                blocks.insert(name, lines[current_start..i].concat());
            }
            current_name = extract_name_attr(stripped);
            current_start = i;
        } else if current_name.is_some() && stripped.contains("</xs:schema>") {
            if let Some(name) = current_name.take() {
                blocks.insert(name, lines[current_start..i].concat());
            }
        }
    }
    if let Some(name) = current_name {
        blocks.insert(name, lines[current_start..].concat());
    }
    blocks
}

fn extract_schema_header(path: &Path) -> String {
    let content = read_file(path);
    let lines = split_lines_keep_ends(&content);
    let mut header: Vec<&str> = Vec::new();
    for line in &lines {
        let stripped = line.trim_end_matches(['\r', '\n']);
        if is_top_level_def(stripped) {
            break;
        }
        if stripped.starts_with('\t') && stripped[1..].starts_with("<xs:include") {
            continue;
        }
        header.push(line);
    }
    header.concat()
}

fn find_includes(path: &Path) -> Vec<PathBuf> {
    let content = read_file(path);
    let parent = path.parent().unwrap_or(Path::new("."));
    let mut includes = Vec::new();
    let mut search = content.as_str();
    while let Some(pos) = search.find("<xs:include") {
        let seg = &search[pos..];
        let end = seg.find('>').map(|p| p + 1).unwrap_or(seg.len());
        let chunk = &seg[..end];
        if let Some(loc_pos) = chunk.find("schemaLocation=\"") {
            let after = &chunk[loc_pos + 16..];
            if let Some(quote_end) = after.find('"') {
                let inc = parent.join(&after[..quote_end]);
                if inc.exists() {
                    includes.push(inc);
                } else {
                    eprintln!("Warning: included schema not found: {}", inc.display());
                }
            }
        }
        search = &search[pos + end..];
    }
    includes
}

fn collect_ref(val: &str, refs: &mut HashSet<String>) {
    if val.is_empty() || val.starts_with("xs:") {
        return;
    }
    if let Some((_, local)) = val.split_once(':') {
        refs.insert(local.to_owned());
    } else {
        refs.insert(val.to_owned());
    }
}

fn collect_type_refs(e: &quick_xml::events::BytesStart, refs: &mut HashSet<String>) {
    for attr in e.attributes().filter_map(|a| a.ok()) {
        let key = local_name_of(attr.key.as_ref());
        let Ok(val) = std::str::from_utf8(attr.value.as_ref()) else {
            continue;
        };
        match key {
            "type" | "base" | "ref" | "itemType" => collect_ref(val, refs),
            "memberTypes" => {
                for token in val.split_whitespace() {
                    collect_ref(token, refs);
                }
            }
            _ => {}
        }
    }
}

fn build_named_map(schema_paths: &[PathBuf]) -> HashMap<String, HashSet<String>> {
    let mut named: HashMap<String, HashSet<String>> = HashMap::new();

    for path in schema_paths {
        let content = read_file(path);
        let mut reader = Reader::from_str(&content);
        let mut depth: usize = 0;
        let mut current_name: Option<String> = None;
        let mut current_refs: HashSet<String> = HashSet::new();

        loop {
            match reader.read_event() {
                Ok(Event::Start(ref e)) => {
                    if depth == 1 {
                        if let Some(name) = current_name.take() {
                            named.entry(name).or_default().extend(current_refs.drain());
                        }
                        current_name = attr_str(e, "name");
                    }
                    if current_name.is_some() {
                        collect_type_refs(e, &mut current_refs);
                    }
                    depth += 1;
                }
                Ok(Event::Empty(ref e)) => {
                    if depth == 1 {
                        if let Some(name) = current_name.take() {
                            named.entry(name).or_default().extend(current_refs.drain());
                        }
                        if let Some(name) = attr_str(e, "name") {
                            let mut refs = HashSet::new();
                            collect_type_refs(e, &mut refs);
                            named.entry(name).or_default().extend(refs);
                        }
                    } else if current_name.is_some() {
                        collect_type_refs(e, &mut current_refs);
                    }
                }
                Ok(Event::End(_)) => {
                    depth -= 1;
                    if depth == 1 {
                        if let Some(name) = current_name.take() {
                            named.entry(name).or_default().extend(current_refs.drain());
                        }
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
    }
    named
}

fn resolve_deps(
    element_names: &[String],
    named: &HashMap<String, HashSet<String>>,
) -> HashSet<String> {
    let mut needed: HashSet<String> = HashSet::new();
    let mut queue = element_names.to_vec();
    while let Some(name) = queue.pop() {
        if needed.contains(&name) || !named.contains_key(&name) {
            continue;
        }
        needed.insert(name.clone());
        if let Some(refs) = named.get(&name) {
            for r in refs {
                if !needed.contains(r) {
                    queue.push(r.clone());
                }
            }
        }
    }
    needed
}

struct Args {
    elements: Vec<String>,
    schema: PathBuf,
    output: Option<PathBuf>,
}

fn parse_args() -> Args {
    let mut iter = std::env::args().skip(1).peekable();
    let mut elements = Vec::new();
    let mut schema: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--schema" => schema = iter.next().map(PathBuf::from),
            "-o" | "--output" => output = iter.next().map(PathBuf::from),
            "-h" | "--help" => {
                eprintln!(
                    "Usage: xsd_subset [--schema PATH] [-o PATH] ELEMENT...\n\n\
                     Extract a subset XSD with transitive type dependencies.\n\n\
                     Options:\n  \
                       --schema PATH       Main XSD file\n  \
                                           (default: rcal/schema/UCI_MessageDefinitions_v2_5_0.xsd)\n  \
                       -o, --output PATH   Output file (default: stdout)"
                );
                std::process::exit(0);
            }
            s if s.starts_with('-') => {
                eprintln!("Unknown option: {s}");
                std::process::exit(1);
            }
            s => elements.push(s.to_owned()),
        }
    }

    if elements.is_empty() {
        eprintln!("Error: at least one ELEMENT name required.\nUsage: xsd_subset [--schema PATH] [-o PATH] ELEMENT...");
        std::process::exit(1);
    }

    Args {
        elements,
        schema: schema
            .unwrap_or_else(|| PathBuf::from("rcal/schema/UCI_MessageDefinitions_v2_5_0.xsd")),
        output,
    }
}

fn main() {
    let args = parse_args();

    if !args.schema.exists() {
        eprintln!("Schema not found: {}", args.schema.display());
        std::process::exit(1);
    }

    let includes = find_includes(&args.schema);
    let all_paths: Vec<PathBuf> = includes.into_iter().chain([args.schema.clone()]).collect();

    let named = build_named_map(&all_paths);

    let missing: Vec<&str> = args
        .elements
        .iter()
        .filter(|e| !named.contains_key(e.as_str()))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        eprintln!(
            "Error: elements not found in schema: {}",
            missing.join(", ")
        );
        std::process::exit(1);
    }

    let needed = resolve_deps(&args.elements, &named);
    let requested: HashSet<&str> = args.elements.iter().map(String::as_str).collect();
    let mut support: Vec<&str> = needed
        .iter()
        .map(String::as_str)
        .filter(|n| !requested.contains(n))
        .collect();
    support.sort_unstable();

    let mut all_blocks: HashMap<String, String> = HashMap::new();
    for path in &all_paths {
        all_blocks.extend(extract_blocks(path));
    }

    let mut out = String::new();
    out.push_str(&extract_schema_header(&args.schema));
    out.push_str("\t<!--== START MESSAGES ==-->\n");

    let mut sorted_req: Vec<&str> = requested.iter().copied().collect();
    sorted_req.sort_unstable();
    for name in &sorted_req {
        if let Some(block) = all_blocks.get(*name) {
            out.push_str(block);
        } else {
            eprintln!("Warning: no raw block found for element '{name}'");
        }
    }

    if !support.is_empty() {
        out.push_str("\t<!--== SUPPORTING TYPES ==-->\n");
        for name in &support {
            if let Some(block) = all_blocks.get(*name) {
                out.push_str(block);
            }
        }
    }

    out.push_str("</xs:schema>\n");

    if let Some(ref output_path) = args.output {
        fs::write(output_path, &out).unwrap_or_else(|e| {
            eprintln!("Error writing {}: {e}", output_path.display());
            std::process::exit(1);
        });
        eprintln!(
            "Wrote {}: {} element(s), {} supporting type(s)",
            output_path.display(),
            sorted_req.len(),
            support.len()
        );
    } else {
        std::io::stdout()
            .write_all(out.as_bytes())
            .unwrap_or_else(|e| {
                eprintln!("Error writing stdout: {e}");
                std::process::exit(1);
            });
    }
}
