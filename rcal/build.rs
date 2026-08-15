use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use quick_xml::events::Event;
use quick_xml::reader::Reader;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(rcal_has_xsd)");
    println!("cargo:rerun-if-env-changed=RCAL_XSD_PATH");
    println!("cargo:rerun-if-env-changed=RCAL_SCHEMA_VERSION");
    println!("cargo:rerun-if-env-changed=RCAL_OMS_COMPILER_VERSION");

    let compiler_version = std::env::var("RCAL_OMS_COMPILER_VERSION")
        .unwrap_or_else(|_| "rcal-build/0.1".to_string());
    println!("cargo:rustc-env=RCAL_OMS_COMPILER_VERSION={compiler_version}");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let types_dir = out_dir.join("uci_types");
    fs::create_dir_all(&types_dir).unwrap();

    let Some(xsd_path) = std::env::var("RCAL_XSD_PATH").ok() else {
        // No XSD configured — emit empty module and unknown version.
        println!("cargo:rustc-env=RCAL_SCHEMA_VERSION=unknown");
        fs::write(types_dir.join("mod.rs"), "").unwrap();
        return;
    };

    println!("cargo:rustc-cfg=rcal_has_xsd");

    println!("cargo:rerun-if-changed={xsd_path}");

    let xsd_path = PathBuf::from(&xsd_path);
    let xsd_content = fs::read_to_string(&xsd_path)
        .unwrap_or_else(|e| panic!("Cannot read RCAL_XSD_PATH={}: {e}", xsd_path.display()));

    let schema = parse_xsd_file(&xsd_path, &xsd_content, &mut std::collections::HashSet::new());

    let schema_version = std::env::var("RCAL_SCHEMA_VERSION").unwrap_or_else(|_| {
        schema.version.clone().unwrap_or_else(|| "unknown".to_string())
    });
    println!("cargo:rustc-env=RCAL_SCHEMA_VERSION={schema_version}");

    generate_types(&schema, &types_dir);
}

// ════════════════════════════════════════════════════════════════════════════
// XSD data model
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Default)]
struct Schema {
    version: Option<String>,
    namespace: Option<String>,
    simple_types: Vec<SimpleType>,
    complex_types: Vec<ComplexType>,
    elements: Vec<Element>,
}

#[derive(Debug)]
struct SimpleType {
    name: String,
    kind: SimpleTypeKind,
}

#[derive(Debug)]
enum SimpleTypeKind {
    Enum(Vec<String>),
    Restriction(String), // base XSD type
}

#[derive(Debug)]
struct ComplexType {
    name: String,
    abstract_: bool,
    fields: Vec<Field>,
}

#[derive(Debug)]
struct Field {
    name: String,
    type_: String,
    optional: bool,
}

#[derive(Debug)]
struct Element {
    name: String,
    type_: String,
}

// ════════════════════════════════════════════════════════════════════════════
// XSD parser
// ════════════════════════════════════════════════════════════════════════════

/// Parse an XSD file, resolving `xs:include` relative to `xsd_path`'s directory.
/// `seen` prevents infinite inclusion loops.
fn parse_xsd_file(
    xsd_path: &Path,
    content: &str,
    seen: &mut std::collections::HashSet<PathBuf>,
) -> Schema {
    let canonical = xsd_path.canonicalize().unwrap_or_else(|_| xsd_path.to_path_buf());
    seen.insert(canonical);
    let base_dir = xsd_path.parent().unwrap_or(Path::new("."));

    let mut reader = Reader::from_str(content);
    reader.config_mut().trim_text(true);

    let mut schema = Schema::default();
    let mut current_simple: Option<SimpleType> = None;
    let mut current_complex: Option<ComplexType> = None;
    let mut in_restriction = false;
    let mut restriction_base: Option<String> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e) | Event::Empty(ref e)) => {
                let local = local_name(e.name().as_ref());
                match local.as_str() {
                    "schema" => {
                        schema.version = attr(e, "version");
                        schema.namespace = attr(e, "targetNamespace");
                    }
                    "include" => {
                        if let Some(loc) = attr(e, "schemaLocation") {
                            let inc_path = base_dir.join(&loc);
                            let canonical_inc =
                                inc_path.canonicalize().unwrap_or_else(|_| inc_path.clone());
                            if seen.contains(&canonical_inc) {
                                continue;
                            }
                            let inc_content = fs::read_to_string(&inc_path).unwrap_or_else(|e| {
                                panic!(
                                    "xs:include '{}' not found (included from '{}'): {e}",
                                    inc_path.display(),
                                    xsd_path.display()
                                )
                            });
                            println!("cargo:rerun-if-changed={}", inc_path.display());
                            let inc_schema =
                                parse_xsd_file(&inc_path, &inc_content, seen);
                            schema.simple_types.extend(inc_schema.simple_types);
                            schema.complex_types.extend(inc_schema.complex_types);
                            schema.elements.extend(inc_schema.elements);
                        }
                    }
                    "simpleType" => {
                        if let Some(name) = attr(e, "name") {
                            current_simple = Some(SimpleType {
                                name,
                                kind: SimpleTypeKind::Restriction("xs:string".into()),
                            });
                        }
                    }
                    "complexType" => {
                        if let Some(name) = attr(e, "name") {
                            let abstract_ =
                                attr(e, "abstract").map(|v| v == "true").unwrap_or(false);
                            current_complex =
                                Some(ComplexType { name, abstract_, fields: vec![] });
                        }
                    }
                    "restriction" => {
                        in_restriction = true;
                        restriction_base = attr(e, "base");
                    }
                    "enumeration" => {
                        if let (Some(st), Some(val)) =
                            (current_simple.as_mut(), attr(e, "value"))
                        {
                            if let SimpleTypeKind::Enum(ref mut vals) = st.kind {
                                vals.push(val);
                            } else {
                                st.kind = SimpleTypeKind::Enum(vec![val]);
                            }
                        }
                    }
                    "element" => {
                        if current_complex.is_none() && current_simple.is_none() {
                            if let (Some(name), Some(type_)) =
                                (attr(e, "name"), attr(e, "type"))
                            {
                                schema.elements.push(Element { name, type_ });
                            }
                        } else if let Some(ct) = current_complex.as_mut()
                            && let (Some(name), Some(type_)) =
                                (attr(e, "name"), attr(e, "type"))
                        {
                            let optional =
                                attr(e, "minOccurs").map(|v| v == "0").unwrap_or(false);
                            ct.fields.push(Field { name, type_, optional });
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let local = local_name(e.name().as_ref());
                match local.as_str() {
                    "simpleType" => {
                        if let Some(mut st) = current_simple.take() {
                            if in_restriction
                                && let SimpleTypeKind::Restriction(_) = &st.kind
                                && let Some(base) = restriction_base.take()
                            {
                                st.kind = SimpleTypeKind::Restriction(base);
                            }
                            schema.simple_types.push(st);
                        }
                        in_restriction = false;
                        restriction_base = None;
                    }
                    "complexType" => {
                        if let Some(ct) = current_complex.take() {
                            schema.complex_types.push(ct);
                        }
                    }
                    "restriction" => {
                        // Keep in_restriction/restriction_base until End("simpleType") resets them
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => panic!("XSD parse error in '{}': {e}", xsd_path.display()),
            _ => {}
        }
    }

    schema
}

fn local_name(name: &[u8]) -> String {
    let s = std::str::from_utf8(name).unwrap_or("");
    s.rfind(':').map(|i| &s[i + 1..]).unwrap_or(s).to_string()
}

fn attr(e: &quick_xml::events::BytesStart, key: &str) -> Option<String> {
    e.attributes()
        .filter_map(|a| a.ok())
        .find(|a| local_name(a.key.as_ref()) == key)
        .and_then(|a| std::str::from_utf8(a.value.as_ref()).ok().map(|s| s.to_string()))
}

// ════════════════════════════════════════════════════════════════════════════
// Code generator
// ════════════════════════════════════════════════════════════════════════════

fn generate_types(schema: &Schema, out_dir: &Path) {
    let ns_prefix = namespace_prefix(schema.namespace.as_deref());
    let mut mod_entries: Vec<String> = vec![];

    // Build a lookup: simple-type name → resolved Rust type (for use in complex types)
    let _ns_prefix = namespace_prefix(schema.namespace.as_deref());
    let simple_type_map: HashMap<&str, String> = schema
        .simple_types
        .iter()
        .map(|st| {
            let rust_ty = match &st.kind {
                SimpleTypeKind::Enum(_) => pascal(&st.name).to_string(),
                SimpleTypeKind::Restriction(base) => xsd_to_rust(base),
            };
            (st.name.as_str(), rust_ty)
        })
        .collect();

    // Generate simple types
    for st in &schema.simple_types {
        let file_name = format!("{}.rs", snake(&st.name));
        let code = match &st.kind {
            SimpleTypeKind::Enum(vals) => gen_enum(&st.name, vals, &ns_prefix),
            SimpleTypeKind::Restriction(base) => gen_type_alias(&st.name, base),
        };
        fs::write(out_dir.join(&file_name), code).unwrap();
        let mod_name = snake(&st.name);
        mod_entries.push(format!(
            "/// Generated module for XSD type `{}`.\npub mod {mod_name};\npub use {mod_name}::*;",
            st.name
        ));
    }

    // Invert: complex-type name → element name (for CalMessage impl on the struct)
    let type_to_element: HashMap<&str, &str> = schema
        .elements
        .iter()
        .map(|el| {
            let type_local = el.type_.rfind(':').map(|i| &el.type_[i + 1..]).unwrap_or(&el.type_);
            (type_local, el.name.as_str())
        })
        .collect();

    // Generate complex types
    for ct in &schema.complex_types {
        let file_name = format!("{}.rs", snake(&ct.name));
        let code = gen_struct(ct, &simple_type_map, &type_to_element, schema.namespace.as_deref());
        fs::write(out_dir.join(&file_name), code).unwrap();
        let mod_name = snake(&ct.name);
        mod_entries.push(format!(
            "/// Generated module for XSD type `{}`.\npub mod {mod_name};\npub use {mod_name}::*;",
            ct.name
        ));
    }

    // Generate element type aliases
    for el in &schema.elements {
        let type_local = el.type_.rfind(':').map(|i| &el.type_[i + 1..]).unwrap_or(&el.type_);
        let type_pascal = pascal(type_local);
        let el_module = snake(&el.name);
        let el_pascal = pascal(&el.name);
        let type_path = format!("crate::uci::types::{type_pascal}");
        let type_name_fq = format!("{}.{type_local}", ns_prefix);
        let code = format!(
            "// @generated — do not edit.\n#![allow(non_camel_case_types)]\n\n\
             /// XSD element `{el_name}`. Wraps [`{type_pascal}`]({type_path}).\n\
             #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]\n\
             #[serde(transparent)]\n\
             pub struct {el_pascal}(pub {type_path});\n\n\
             impl std::ops::Deref for {el_pascal} {{\n\
             \x20   type Target = {type_path};\n\
             \x20   fn deref(&self) -> &Self::Target {{ &self.0 }}\n\
             }}\n\n\
             impl std::ops::DerefMut for {el_pascal} {{\n\
             \x20   fn deref_mut(&mut self) -> &mut Self::Target {{ &mut self.0 }}\n\
             }}\n\n\
             impl crate::uci::CalMessage for {el_pascal} {{\n\
             \x20   fn message_type_name() -> &'static str {{ \"{type_name_fq}\" }}\n\
             \x20   fn cal_create() -> Self {{ Self({type_path}::_cal_create()) }}\n\
             }}\n",
            el_name = el.name,
        );
        fs::write(out_dir.join(format!("{el_module}.rs")), code).unwrap();
        mod_entries.push(format!(
            "/// Generated module for XSD element `{}`.\npub mod {el_module};\npub use {el_module}::*;",
            el.name
        ));
    }

    // Write mod.rs
    let mod_content = format!(
        "// @generated — do not edit.\n\n{}\n",
        mod_entries.join("\n")
    );
    fs::write(out_dir.join("mod.rs"), mod_content).unwrap();
}

fn gen_enum(name: &str, vals: &[String], _ns_prefix: &str) -> String {
    let pascal_name = pascal(name);

    // Deduplicate variant names: same Rust identifier may map to multiple XSD values.
    // Keep first occurrence; subsequent duplicates get a numeric suffix.
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let variants: Vec<(String, &str)> = vals
        .iter()
        .map(|v| {
            let base = enum_variant(v);
            let count = seen.entry(base.clone()).or_insert(0);
            *count += 1;
            let variant = if *count == 1 {
                base
            } else {
                format!("{base}{}", *count)
            };
            (variant, v.as_str())
        })
        .collect();

    let default_variant = variants.first().map(|(v, _)| v.as_str()).unwrap_or("").to_string();

    let mut out = String::new();
    out.push_str("// @generated — do not edit.\n#![allow(non_camel_case_types)]\n\n");
    out.push_str(&format!("/// XSD simpleType `{name}`.\n"));
    out.push_str("#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]\n");
    out.push_str(&format!("#[serde(rename = \"{pascal_name}\")]\n"));
    out.push_str(&format!("pub enum {pascal_name} {{\n"));
    for (variant, orig) in &variants {
        out.push_str(&format!("    /// `{orig}` variant.\n    #[serde(rename = \"{orig}\")]\n    {variant},\n"));
    }
    out.push_str("}\n\n");
    out.push_str(&format!("impl Default for {pascal_name} {{\n"));
    out.push_str(&format!("    fn default() -> Self {{ Self::{default_variant} }}\n"));
    out.push_str("}\n");
    out
}

fn gen_type_alias(name: &str, base: &str) -> String {
    let pascal_name = pascal(name);
    let rust_type = xsd_to_rust(base);
    format!("// @generated — do not edit.\n#![allow(non_camel_case_types)]\n\n/// XSD simpleType `{name}`.\npub type {pascal_name} = {rust_type};\n")
}

fn gen_struct(
    ct: &ComplexType,
    simple_map: &HashMap<&str, String>,
    type_to_element: &HashMap<&str, &str>,
    namespace: Option<&str>,
) -> String {
    let pascal_name = pascal(&ct.name);

    let fields: String = ct
        .fields
        .iter()
        .map(|f| {
            let field_name = snake(&f.name);
            let type_local = f.type_.rfind(':').map(|i| &f.type_[i + 1..]).unwrap_or(&f.type_);
            let base_type = if let Some(resolved) = simple_map.get(type_local) {
                // Simple type — reference by module path
                // Enums are resolved to their pascal name, aliases to the Rust primitive
                // We need to emit the full path for enums; aliases are inlined primitives.
                resolved.clone()
            } else {
                xsd_to_rust(&f.type_)
            };

            // If the base_type looks like a user-defined type (not a primitive), qualify it
            let rust_type = qualify_type(type_local, &base_type);

            let full_type = if f.optional {
                format!("Option<{rust_type}>")
            } else {
                rust_type
            };

            let optional_tag = if f.optional { " (optional)" } else { "" };
            let doc = format!("    /// XSD element `{}`{optional_tag}.\n", f.name);
            let serde_rename = format!("    #[serde(rename = \"{}\")]\n", f.name);
            let maybe_skip = if f.optional {
                "    #[serde(skip_serializing_if = \"Option::is_none\")]\n"
            } else {
                ""
            };
            format!("{doc}{serde_rename}{maybe_skip}    pub {field_name}: {full_type},\n")
        })
        .collect();

    // Default values for each field (for cal_create)
    let field_defaults: String = ct
        .fields
        .iter()
        .map(|f| {
            let field_name = snake(&f.name);
            let default_val = if f.optional {
                "None".to_string()
            } else {
                "Default::default()".to_string()
            };
            format!("            {field_name}: {default_val},\n")
        })
        .collect();

    let _ns = namespace.unwrap_or("");

    let mut out = String::new();
    out.push_str("// @generated — do not edit.\n#![allow(non_camel_case_types)]\n\n");
    out.push_str(&format!("/// XSD complexType `{}`.\n", ct.name));
    out.push_str("#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]\n");
    out.push_str(&format!("#[serde(rename = \"{pascal_name}\")]\n"));
    out.push_str(&format!("pub struct {pascal_name} {{\n"));
    out.push_str(&fields);

    if ct.abstract_ {
        out.push_str("}\n\n");
        out.push_str(&format!("impl crate::uci::CalSubMessage for {pascal_name} {{}}\n"));
    } else if type_to_element.contains_key(ct.name.as_str()) {
        // Top-level element maps to this type; CalMessage goes on the element newtype, not here.
        out.push_str("    #[serde(skip)]\n");
        out.push_str("    _priv: crate::uci::sealed::Token,\n");
        out.push_str("}\n\n");
        out.push_str(&format!("impl {pascal_name} {{\n"));
        out.push_str("    pub(crate) fn _cal_create() -> Self {\n");
        out.push_str("        Self {\n");
        out.push_str(&field_defaults);
        out.push_str("            _priv: crate::uci::sealed::Token(()),\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
        out.push_str("}\n");
    } else {
        // Concrete nested sub-message: sealed token, CalSubMessage
        out.push_str("    #[serde(skip)]\n");
        out.push_str("    _priv: crate::uci::sealed::Token,\n");
        out.push_str("}\n\n");
        out.push_str(&format!("impl crate::uci::CalSubMessage for {pascal_name} {{}}\n"));
    }
    out
}

// ════════════════════════════════════════════════════════════════════════════
// Name helpers
// ════════════════════════════════════════════════════════════════════════════

fn pascal(s: &str) -> String {
    // Already PascalCase from XSD — return as-is after stripping namespace prefix
    let local = s.rfind(':').map(|i| &s[i + 1..]).unwrap_or(s);
    local.to_string()
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
    "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe",
    "use", "where", "while", "async", "await", "dyn", "abstract", "become", "box", "do",
    "final", "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
];

fn snake(s: &str) -> String {
    let local = s.rfind(':').map(|i| &s[i + 1..]).unwrap_or(s);
    let mut out = String::new();
    let mut prev_upper = false;
    for (i, c) in local.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 && !prev_upper {
                out.push('_');
            }
            out.push(c.to_lowercase().next().unwrap());
            prev_upper = true;
        } else {
            out.push(c);
            prev_upper = false;
        }
    }
    if RUST_KEYWORDS.contains(&out.as_str()) {
        out.push('_');
    }
    out
}

fn enum_variant(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) if first.is_ascii_digit() => {
            // Prefix digit-starting values so the identifier is valid
            format!("V{}{}", first, chars.as_str().to_lowercase())
        }
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
    }
}

fn namespace_prefix(ns: Option<&str>) -> String {
    ns.unwrap_or("").to_string()
}

/// Map an XSD type reference to a Rust type expression.
fn xsd_to_rust(xsd_type: &str) -> String {
    let local = xsd_type.rfind(':').map(|i| &xsd_type[i + 1..]).unwrap_or(xsd_type);
    match local {
        "boolean" => "crate::xs::Boolean",
        "long" => "crate::xs::Long",
        "int" => "crate::xs::Int",
        "short" => "crate::xs::Short",
        "byte" => "crate::xs::Byte",
        "unsignedLong" => "crate::xs::UnsignedLong",
        "unsignedInt" => "crate::xs::UnsignedInt",
        "unsignedShort" => "crate::xs::UnsignedShort",
        "unsignedByte" => "crate::xs::UnsignedByte",
        "double" => "crate::xs::Double",
        "float" => "crate::xs::Float",
        "integer" => "crate::xs::Integer",
        "duration" => "crate::xs::Duration",
        "dateTime" => "crate::xs::DateTime",
        "time" => "crate::xs::Time",
        "string" => "crate::xs::XsString",
        "hexBinary" => "crate::xs::HexBinary",
        other => other, // user-defined — will be qualified by qualify_type
    }
    .to_string()
}

/// If `base_type` is still the raw local name (i.e. user-defined type, not a primitive),
/// qualify it as a crate path.
fn qualify_type(type_local: &str, base_type: &str) -> String {
    // Primitives start with "crate::xs::" — already qualified
    if base_type.starts_with("crate::") {
        return base_type.to_string();
    }
    // User-defined type alias (from simple restriction without enum): the alias IS the primitive
    if base_type != type_local {
        return base_type.to_string();
    }
    // User-defined complex or enum type: flat path in uci::types
    format!("crate::uci::types::{}", pascal(type_local))
}
