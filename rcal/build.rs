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

    let xsd_content = fs::read_to_string(&xsd_path)
        .unwrap_or_else(|e| panic!("Cannot read RCAL_XSD_PATH={xsd_path}: {e}"));

    let schema = parse_xsd(&xsd_content);

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

fn parse_xsd(content: &str) -> Schema {
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
                            let abstract_ = attr(e, "abstract")
                                .map(|v| v == "true")
                                .unwrap_or(false);
                            current_complex = Some(ComplexType { name, abstract_, fields: vec![] });
                        }
                    }
                    "restriction" => {
                        in_restriction = true;
                        restriction_base = attr(e, "base");
                    }
                    "enumeration" => {
                        if let (Some(st), Some(val)) = (current_simple.as_mut(), attr(e, "value"))
                        {
                            if let SimpleTypeKind::Enum(ref mut vals) = st.kind {
                                vals.push(val);
                            } else {
                                st.kind = SimpleTypeKind::Enum(vec![val]);
                            }
                        }
                    }
                    "element" => {
                        // Top-level element (direct child of schema) has no parent complex type
                        if current_complex.is_none() && current_simple.is_none() {
                            if let (Some(name), Some(type_)) =
                                (attr(e, "name"), attr(e, "type"))
                            {
                                schema.elements.push(Element { name, type_ });
                            }
                        } else if let Some(ct) = current_complex.as_mut() {
                            // Field inside a complexType
                            if let (Some(name), Some(type_)) =
                                (attr(e, "name"), attr(e, "type"))
                            {
                                let optional = attr(e, "minOccurs")
                                    .map(|v| v == "0")
                                    .unwrap_or(false);
                                ct.fields.push(Field { name, type_, optional });
                            }
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
                            // If we saw a restriction but no enumerations, record base type
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
                        in_restriction = false;
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => panic!("XSD parse error: {e}"),
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
        mod_entries.push(format!("pub mod {};", snake(&st.name)));
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
        mod_entries.push(format!("pub mod {};", snake(&ct.name)));
    }

    // Generate element type aliases
    for el in &schema.elements {
        let type_local = el.type_.rfind(':').map(|i| &el.type_[i + 1..]).unwrap_or(&el.type_);
        let module = snake(type_local);
        let alias_line = format!(
            "pub use crate::uci::types::{}::{} as {};",
            module,
            pascal(type_local),
            pascal(&el.name)
        );
        // Append to the mod.rs as a re-export rather than a separate file
        mod_entries.push(alias_line);
    }

    // Write mod.rs
    let mod_content = mod_entries.join("\n") + "\n";
    fs::write(out_dir.join("mod.rs"), mod_content).unwrap();
}

fn gen_enum(name: &str, vals: &[String], _ns_prefix: &str) -> String {
    let pascal_name = pascal(name);
    let default_variant = vals.first().map(|v| enum_variant(v)).unwrap_or_default();

    let mut out = String::new();
    out.push_str("#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]\n");
    out.push_str(&format!("#[serde(rename = \"{pascal_name}\")]\n"));
    out.push_str(&format!("pub enum {pascal_name} {{\n"));
    for v in vals {
        let variant = enum_variant(v);
        out.push_str(&format!("    #[serde(rename = \"{v}\")]\n    {variant},\n"));
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
    format!("pub type {pascal_name} = {rust_type};\n")
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

            let serde_rename = format!("    #[serde(rename = \"{}\")]\n", f.name);
            let maybe_skip = if f.optional {
                "    #[serde(skip_serializing_if = \"Option::is_none\")]\n"
            } else {
                ""
            };
            format!("{serde_rename}{maybe_skip}    pub {field_name}: {full_type},\n")
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

    let ns = namespace.unwrap_or("");

    let mut out = String::new();
    out.push_str("#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]\n");
    out.push_str(&format!("#[serde(rename = \"{pascal_name}\")]\n"));
    out.push_str(&format!("pub struct {pascal_name} {{\n"));
    out.push_str(&fields);

    if ct.abstract_ {
        out.push_str("}\n\n");
        out.push_str(&format!("impl crate::uci::CalSubMessage for {pascal_name} {{}}\n"));
    } else if let Some(_element_name) = type_to_element.get(ct.name.as_str()) {
        let type_name_fq = format!("{ns}.{}", ct.name);
        out.push_str("    #[serde(skip)]\n");
        out.push_str("    _priv: crate::uci::sealed::Token,\n");
        out.push_str("}\n\n");
        out.push_str(&format!("impl crate::uci::CalMessage for {pascal_name} {{\n"));
        out.push_str(&format!("    fn message_type_name() -> &'static str {{ \"{type_name_fq}\" }}\n"));
        out.push_str("    fn cal_create() -> Self {\n");
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
    out
}

fn enum_variant(s: &str) -> String {
    // OPERATIONAL → Operational, DEGRADED → Degraded
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => {
            first.to_uppercase().collect::<String>()
                + &chars.as_str().to_lowercase()
        }
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
    // User-defined complex or enum type: qualify by module
    format!("crate::uci::types::{}::{}", snake(type_local), pascal(type_local))
}
