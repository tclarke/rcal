use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use quick_xml::events::Event;
use quick_xml::reader::Reader;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(rcal_has_xsd)");
    println!("cargo::rerun-if-env-changed=RCAL_XSD_PATH");
    println!("cargo::rerun-if-env-changed=RCAL_SCHEMA_VERSION");
    println!("cargo::rerun-if-env-changed=RCAL_OMS_COMPILER_VERSION");

    eprintln!("Starting generation step");
    if let Some(compiler_version) = option_env!("RCAL_OMS_COMPILER_VERSION") {
        println!("cargo::rustc-env=RCAL_OMS_COMPILER_VERSION={compiler_version}");
        eprintln!("OMS compiler version={compiler_version}");
    } else {
        let compiler_version = format!("{}/{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        println!("cargo::rustc-env=RCAL_OMS_COMPILER_VERSION={compiler_version}");
        eprintln!("OMS compiler version={compiler_version}");
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let types_dir = out_dir.join("uci_types");
    fs::create_dir_all(&types_dir).unwrap();
    eprintln!("OUT_DIR={}", out_dir.display());

    let xsd_path = match option_env!("RCAL_XSD_PATH") {
        Some(p) => p.to_string(),
        None => {
            use glob::glob;

            let pattern = "schema/UCI_MessageDefinitions_*.xsd";
            let mut files: Vec<String> = glob(pattern)
                .expect("invalid glob pattern")
                .filter_map(Result::ok)
                .map(|path| path.to_string_lossy().into_owned())
                .collect();
            files.sort_by(|a, b| b.cmp(a));
            let Some(v) = files.first() else {
                println!("cargo::error=Unable to find a valid schema files {:?}", files);
                return;
            };
            v.to_string()
        }
    };
    eprintln!("XSD path={xsd_path}");

    println!("cargo::rustc-cfg=rcal_has_xsd");

    let xsd_path = PathBuf::from(&xsd_path);
    let xsd_content = match fs::read_to_string(&xsd_path) {
        Ok(content) => content,
        Err(e) => {
            println!("cargo::error=Cannot read RCAL_XSD_PATH={}: {e}", xsd_path.display());
            return;
        }
    };

    println!("cargo::rerun-if-changed={}", xsd_path.display());

    let schema = parse_xsd_file(&xsd_path, &xsd_content, &mut std::collections::HashSet::new());

    if let Some(schema_version) = option_env!("RCAL_SCHEMA_VERSION") {
        println!("cargo::rustc-env=RCAL_SCHEMA_VERSION={schema_version}");
        eprintln!("Schema version={schema_version}");
    } else {
        let schema_version = format!("UCI_{}", schema.version.as_ref().unwrap());
        println!("cargo::rustc-env=RCAL_SCHEMA_VERSION={schema_version}");
        eprintln!("Schema version={schema_version}");
    }

    eprintln!("Generating");
    generate_types(&schema, &types_dir);
}

// ════════════════════════════════════════════════════════════════════════════
// Build-time namespace resolver
// ════════════════════════════════════════════════════════════════════════════

/// Build-script parallel of the library's `NamespaceResolver`.
///
/// Kept separate because build scripts compile independently of the crate.
#[derive(Debug, Clone)]
struct XsdResolver {
    default_ns: Option<String>,
    prefix_to_uri: HashMap<String, String>,
    uri_to_prefix: HashMap<String, String>,
}

impl Default for XsdResolver {
    fn default() -> Self {
        let mut r = Self {
            default_ns: None,
            prefix_to_uri: HashMap::new(),
            uri_to_prefix: HashMap::new(),
        };
        r.add_prefix("xs", "http://www.w3.org/2001/XMLSchema");
        r
    }
}

impl XsdResolver {
    fn add_prefix(&mut self, prefix: &str, uri: &str) {
        self.prefix_to_uri.insert(prefix.to_string(), uri.to_string());
        self.uri_to_prefix.insert(uri.to_string(), prefix.to_string());
    }

    /// Resolve `prefix:local` or bare `local` to `(namespace_uri, local_name)`.
    fn resolve_pair(&self, name: &str) -> (Option<String>, String) {
        if let Some(colon) = name.find(':') {
            let prefix = &name[..colon];
            let local = name[colon + 1..].to_string();
            let ns = self.prefix_to_uri.get(prefix).cloned();
            (ns, local)
        } else {
            (self.default_ns.clone(), name.to_string())
        }
    }

    /// Shortest display form: bare local for the default namespace,
    /// `prefix:local` for mapped namespaces, Clark notation as last resort.
    fn format_display(&self, ns: Option<&str>, local: &str) -> String {
        match ns {
            None => local.to_string(),
            Some(n) if self.default_ns.as_deref() == Some(n) => local.to_string(),
            Some(n) => match self.uri_to_prefix.get(n) {
                Some(p) => format!("{p}:{local}"),
                None => format!("{{{n}}}{local}"),
            },
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// XSD data model
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Default)]
struct Schema {
    version: Option<String>,
    namespace: Option<String>,
    resolver: XsdResolver,
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
    Restriction(String), // raw XSD base (e.g. "xs:double", "uci:DoubleNonNegativeType")
}

#[derive(Debug)]
struct ComplexType {
    name: String,
    abstract_: bool,
    extension_base: Option<String>,
    fields: Vec<Field>,
}

#[derive(Debug)]
struct Field {
    name: String,
    type_: String, // raw XSD type reference
    optional: bool,
}

#[derive(Debug)]
struct Element {
    name: String,
    type_: String, // raw XSD type reference
}

// ════════════════════════════════════════════════════════════════════════════
// XSD parser
// ════════════════════════════════════════════════════════════════════════════

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
                        if let Some(ref ns) = schema.namespace {
                            schema.resolver.default_ns = Some(ns.clone());
                        }
                        // Collect all xmlns:prefix declarations.
                        for a in e.attributes().filter_map(|a| a.ok()) {
                            let key = std::str::from_utf8(a.key.as_ref()).unwrap_or("");
                            if let Some(prefix) = key.strip_prefix("xmlns:") {
                                let uri = std::str::from_utf8(a.value.as_ref())
                                    .unwrap_or("")
                                    .to_string();
                                schema.resolver.add_prefix(prefix, &uri);
                            }
                        }
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
                            println!("cargo::rerun-if-changed={}", inc_path.display());
                            let inc_schema = parse_xsd_file(&inc_path, &inc_content, seen);
                            // Merge namespace declarations from included schema.
                            for (p, u) in &inc_schema.resolver.prefix_to_uri {
                                if !schema.resolver.prefix_to_uri.contains_key(p) {
                                    schema.resolver.add_prefix(p, u);
                                }
                            }
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
                            current_complex = Some(ComplexType {
                                name,
                                abstract_,
                                extension_base: None,
                                fields: vec![],
                            });
                        }
                    }
                    "extension" => {
                        if let Some(ct) = current_complex.as_mut() {
                            ct.extension_base = attr(e, "base");
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
                        // in_restriction/restriction_base reset at End("simpleType")
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
    let resolver = &schema.resolver;
    let mut mod_entries: Vec<String> = vec![];

    // Build a lookup: simple-type local name → fully-qualified Rust type path.
    let simple_type_map: HashMap<&str, String> = schema
        .simple_types
        .iter()
        .map(|st| {
            let rust_ty = match &st.kind {
                SimpleTypeKind::Enum(_) => {
                    format!("crate::uci::types::{}", pascal(&st.name))
                }
                SimpleTypeKind::Restriction(base) => {
                    let (ns, local) = resolver.resolve_pair(base);
                    xsd_to_rust(ns.as_deref(), &local)
                }
            };
            (st.name.as_str(), rust_ty)
        })
        .collect();

    // Generate simple types
    eprintln!("Generating simple types");
    for st in &schema.simple_types {
        eprintln!("- {}", st.name);
        let file_name = format!("{}.rs", snake(&st.name));
        let code = match &st.kind {
            SimpleTypeKind::Enum(vals) => gen_enum(&st.name, vals),
            SimpleTypeKind::Restriction(base) => gen_type_alias(&st.name, base, resolver),
        };
        fs::write(out_dir.join(&file_name), code).unwrap();
        let mod_name = snake(&st.name);
        mod_entries.push(format!(
            "/// Generated module for XSD type `{}`.\npub mod {mod_name};\npub use {mod_name}::*;",
            st.name
        ));
    }

    // Invert: complex-type local name → element name
    let type_to_element: HashMap<&str, &str> = schema
        .elements
        .iter()
        .map(|el| {
            let colon = el.type_.find(':');
            let local = colon.map(|i| &el.type_[i + 1..]).unwrap_or(&el.type_);
            (local, el.name.as_str())
        })
        .collect();

    // Build complex-type lookup for inheritance delegation
    let complex_type_map: HashMap<&str, &ComplexType> = schema
        .complex_types
        .iter()
        .map(|ct| (ct.name.as_str(), ct))
        .collect();

    // Pure-extension complex types (no own fields) are emitted as type aliases (no `_` suffix).
    // Add them to simple_type_map so field_rust_type resolves them without appending `_`.
    let mut simple_type_map = simple_type_map;
    for ct in &schema.complex_types {
        if ct.fields.is_empty() && ct.extension_base.is_some() {
            let pascal_name = pascal(&ct.name);
            simple_type_map
                .entry(ct.name.as_str())
                .or_insert_with(|| format!("crate::uci::types::{pascal_name}"));
        }
    }

    // Generate complex types
    eprintln!("Generating complex types");
    for ct in &schema.complex_types {
        eprintln!("- {}", ct.name);
        let file_name = format!("{}.rs", snake(&ct.name));
        let code = gen_struct(ct, &simple_type_map, &type_to_element, resolver, &complex_type_map);
        fs::write(out_dir.join(&file_name), code).unwrap();
        let mod_name = snake(&ct.name);
        mod_entries.push(format!(
            "/// Generated module for XSD type `{}`.\npub mod {mod_name};\npub use {mod_name}::*;",
            ct.name
        ));
    }

    // Generate element newtype wrappers
    eprintln!("Generating elements");
    for el in &schema.elements {
        eprintln!("- {}", el.name);
        let (type_ns, type_local) = resolver.resolve_pair(&el.type_);
        let type_pascal = pascal(&type_local);
        let el_module = snake(&el.name);
        let el_pascal = pascal(&el.name);
        let type_path_concrete = format!("crate::uci::types::{type_pascal}_");
        let display = resolver.format_display(type_ns.as_deref(), &type_local);
        let ns_arg = match &type_ns {
            Some(ns) => format!("Some(\"{ns}\")"),
            None => "None".to_string(),
        };
        let code = format!(
            "// @generated — do not edit.\n#![allow(non_camel_case_types)]\n\n\
             /// XSD element `{el_name}`. Wraps [`{type_pascal}_`]({type_path_concrete}).\n\
             #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]\n\
             #[serde(transparent)]\n\
             pub struct {el_pascal}_(pub {type_path_concrete});\n\n\
             impl std::ops::Deref for {el_pascal}_ {{\n\
             \x20   type Target = {type_path_concrete};\n\
             \x20   fn deref(&self) -> &Self::Target {{ &self.0 }}\n\
             }}\n\n\
             impl std::ops::DerefMut for {el_pascal}_ {{\n\
             \x20   fn deref_mut(&mut self) -> &mut Self::Target {{ &mut self.0 }}\n\
             }}\n\n\
             impl crate::uci::CalMessage for {el_pascal}_ {{\n\
             \x20   fn message_type_name() -> crate::QName {{\n\
             \x20       crate::QName::with_display({ns_arg}, \"{type_local}\", \"{display}\")\n\
             \x20   }}\n\
             \x20   fn cal_create() -> Self {{ Self({type_path_concrete}::_cal_create()) }}\n\
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
    eprintln!("Generating types :: Done");
}

fn gen_enum(name: &str, vals: &[String]) -> String {
    let pascal_name = pascal(name);

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

    let _default_variant = variants.first().map(|(v, _)| v.as_str()).unwrap_or("").to_string();

    // Match arms and variant-name list for the custom Deserialize impl.
    let match_arms: String = variants
        .iter()
        .map(|(variant, orig)| format!("                    \"{orig}\" => Ok({pascal_name}::{variant}),\n"))
        .collect();
    let variant_names: Vec<String> = variants.iter().map(|(_, orig)| format!("\"{orig}\"")).collect();
    let variant_names_str = variant_names.join(", ");

    let mut out = String::new();
    out.push_str("// @generated — do not edit.\n#![allow(non_camel_case_types)]\n\n");
    out.push_str(&format!("/// XSD simpleType `{name}`.\n"));
    // Derive Serialize + Default; Deserialize is hand-written below to handle
    // quick_xml's $text map representation for element text content.
    out.push_str("#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]\n");
    out.push_str(&format!("#[serde(rename = \"{pascal_name}\")]\n"));
    out.push_str(&format!("pub enum {pascal_name} {{\n"));
    for (i, (variant, orig)) in variants.iter().enumerate() {
        let default_attr = if i == 0 { "    #[default]\n" } else { "" };
        out.push_str(&format!("    /// `{orig}` variant.\n{default_attr}    #[serde(rename = \"{orig}\")]\n    {variant},\n"));
    }
    out.push_str("}\n\n");
    // Custom Deserialize: quick_xml delivers element text as a map with "$text" key.
    // This impl accepts both a plain string (JSON/other) and the $text-map form.
    out.push_str(&format!(
        "impl<'de> serde::Deserialize<'de> for {pascal_name} {{\n\
         \x20   fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {{\n\
         \x20       fn from_str<E: serde::de::Error>(v: &str) -> Result<{pascal_name}, E> {{\n\
         \x20           match v {{\n\
         {match_arms}\
         \x20               other => Err(E::unknown_variant(other, &[{variant_names_str}])),\n\
         \x20           }}\n\
         \x20       }}\n\
         \x20       struct Visitor_;\n\
         \x20       impl<'de> serde::de::Visitor<'de> for Visitor_ {{\n\
         \x20           type Value = {pascal_name};\n\
         \x20           fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {{\n\
         \x20               write!(f, \"a {pascal_name} variant\")\n\
         \x20           }}\n\
         \x20           fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {{\n\
         \x20               from_str(v)\n\
         \x20           }}\n\
         \x20           fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {{\n\
         \x20               // Drain all map entries; the closing XML tag must be consumed\n\
         \x20               // before returning, or the parent map's cursor is left misaligned.\n\
         \x20               let mut result = None;\n\
         \x20               while let Some(key) = map.next_key::<std::borrow::Cow<str>>()? {{\n\
         \x20                   if key == \"$text\" {{\n\
         \x20                       let val: std::borrow::Cow<str> = map.next_value()?;\n\
         \x20                       result = Some(from_str::<A::Error>(&val)?);\n\
         \x20                   }} else {{\n\
         \x20                       let _: serde::de::IgnoredAny = map.next_value()?;\n\
         \x20                   }}\n\
         \x20               }}\n\
         \x20               result.ok_or_else(|| serde::de::Error::missing_field(\"$text\"))\n\
         \x20           }}\n\
         \x20       }}\n\
         \x20       deserializer.deserialize_any(Visitor_)\n\
         \x20   }}\n\
         }}\n"
    ));
    out
}

fn gen_type_alias(name: &str, base: &str, resolver: &XsdResolver) -> String {
    let pascal_name = pascal(name);
    let (ns, local) = resolver.resolve_pair(base);
    let rust_type = xsd_to_rust(ns.as_deref(), &local);
    format!("// @generated — do not edit.\n#![allow(non_camel_case_types)]\n\n/// XSD simpleType `{name}`.\npub type {pascal_name} = {rust_type};\n")
}

fn resolve_base_rust_type(base: &str, simple_map: &HashMap<&str, String>, resolver: &XsdResolver) -> String {
    let (ns, local) = resolver.resolve_pair(base);
    if let Some(resolved) = simple_map.get(local.as_str()) {
        resolved.clone()
    } else {
        xsd_to_rust_concrete(ns.as_deref(), &local)
    }
}

fn field_rust_type(f: &Field, simple_map: &HashMap<&str, String>, resolver: &XsdResolver) -> String {
    let (type_ns, type_local) = resolver.resolve_pair(&f.type_);
    let base = if let Some(resolved) = simple_map.get(type_local.as_str()) {
        resolved.clone()
    } else {
        xsd_to_rust_concrete(type_ns.as_deref(), &type_local)
    };
    if f.optional { format!("Option<{base}>") } else { base }
}

/// Collect the inheritance chain: [(local_name, &ComplexType)] from immediate base upward.
fn base_chain<'a>(
    ct: &'a ComplexType,
    complex_map: &'a HashMap<&str, &'a ComplexType>,
) -> Vec<(&'a str, &'a ComplexType)> {
    let mut chain = Vec::new();
    let mut current = ct;
    while let Some(base_ref) = &current.extension_base {
        let local = base_ref.rfind(':').map(|i| &base_ref[i + 1..]).unwrap_or(base_ref.as_str());
        match complex_map.get(local) {
            Some(base_ct) => {
                chain.push((local, *base_ct));
                current = base_ct;
            }
            None => break,
        }
    }
    chain
}

fn gen_struct(
    ct: &ComplexType,
    simple_map: &HashMap<&str, String>,
    type_to_element: &HashMap<&str, &str>,
    resolver: &XsdResolver,
    complex_map: &HashMap<&str, &ComplexType>,
) -> String {
    let pascal_name = pascal(&ct.name);

    // Extension with no additional fields → type alias; no trait needed.
    if ct.fields.is_empty() && let Some(base) = &ct.extension_base {
        let rust_type = resolve_base_rust_type(base, simple_map, resolver);
        return format!(
            "// @generated — do not edit.\n#![allow(non_camel_case_types)]\n\n\
             /// XSD complexType `{}` (extension of `{}`).\n\
             pub type {pascal_name} = {rust_type};\n",
            ct.name, base,
        );
    }

    let chain = base_chain(ct, complex_map);
    let immediate_base_local = chain.first().map(|(n, _)| *n);

    // Supertrait clause for the generated trait.
    let supertrait = match immediate_base_local {
        Some(base_local) => {
            let base_pascal = pascal(base_local);
            format!(": crate::uci::types::{base_pascal} ")
        }
        None => String::new(),
    };

    // --- Trait methods ---
    let mut trait_methods = String::new();
    for f in &ct.fields {
        let field_name = snake(&f.name);
        let (type_ns, type_local) = resolver.resolve_pair(&f.type_);
        let rust_type = if let Some(resolved) = simple_map.get(type_local.as_str()) {
            resolved.clone()
        } else {
            xsd_to_rust_concrete(type_ns.as_deref(), &type_local)
        };
        if f.optional {
            trait_methods.push_str(&format!(
                "    /// Returns the optional XSD element `{elem}`.\n\
                 \x20   fn {field_name}(&self) -> Option<&{rust_type}>;\n\
                 \x20   /// Returns a mutable reference to the optional XSD element `{elem}`.\n\
                 \x20   fn {field_name}_mut(&mut self) -> Option<&mut {rust_type}>;\n",
                elem = f.name,
            ));
        } else {
            trait_methods.push_str(&format!(
                "    /// Returns the XSD element `{elem}`.\n\
                 \x20   fn {field_name}(&self) -> &{rust_type};\n\
                 \x20   /// Returns a mutable reference to the XSD element `{elem}`.\n\
                 \x20   fn {field_name}_mut(&mut self) -> &mut {rust_type};\n",
                elem = f.name,
            ));
        }
    }

    // --- Struct fields (private) ---
    // Ancestor fields are inlined directly (deepest-first) rather than using
    // #[serde(flatten)], which quick_xml cannot deserialize reliably.
    let inherited_fields_str: String = chain
        .iter()
        .rev()
        .flat_map(|(_, ancestor_ct)| &ancestor_ct.fields)
        .map(|f| {
            let field_name = snake(&f.name);
            let full_type = field_rust_type(f, simple_map, resolver);
            let optional_tag = if f.optional { " (optional, inherited)" } else { " (inherited)" };
            let doc = format!("    /// XSD element `{}`{optional_tag}.\n", f.name);
            let serde_rename = format!("    #[serde(rename = \"{}\")]\n", f.name);
            let maybe_skip = if f.optional {
                "    #[serde(skip_serializing_if = \"Option::is_none\")]\n"
            } else {
                ""
            };
            format!("{doc}{serde_rename}{maybe_skip}    {field_name}: {full_type},\n")
        })
        .collect();
    let inherited_defaults_str: String = chain
        .iter()
        .rev()
        .flat_map(|(_, ancestor_ct)| &ancestor_ct.fields)
        .map(|f| {
            let field_name = snake(&f.name);
            let default_val = if f.optional { "None".to_string() } else { "Default::default()".to_string() };
            format!("            {field_name}: {default_val},\n")
        })
        .collect();

    let struct_fields: String = ct
        .fields
        .iter()
        .map(|f| {
            let field_name = snake(&f.name);
            let full_type = field_rust_type(f, simple_map, resolver);
            let optional_tag = if f.optional { " (optional)" } else { "" };
            let doc = format!("    /// XSD element `{}`{optional_tag}.\n", f.name);
            let serde_rename = format!("    #[serde(rename = \"{}\")]\n", f.name);
            let maybe_skip = if f.optional {
                "    #[serde(skip_serializing_if = \"Option::is_none\")]\n"
            } else {
                ""
            };
            format!("{doc}{serde_rename}{maybe_skip}    {field_name}: {full_type},\n")
        })
        .collect();

    let field_defaults: String = ct
        .fields
        .iter()
        .map(|f| {
            let field_name = snake(&f.name);
            let default_val = if f.optional { "None".to_string() } else { "Default::default()".to_string() };
            format!("            {field_name}: {default_val},\n")
        })
        .collect();

    // --- Own trait impl bodies ---
    let own_trait_impl: String = ct
        .fields
        .iter()
        .map(|f| {
            let field_name = snake(&f.name);
            let (type_ns, type_local) = resolver.resolve_pair(&f.type_);
            let rust_type = if let Some(resolved) = simple_map.get(type_local.as_str()) {
                resolved.clone()
            } else {
                xsd_to_rust_concrete(type_ns.as_deref(), &type_local)
            };
            if f.optional {
                format!(
                    "    fn {field_name}(&self) -> Option<&{rust_type}> {{ self.{field_name}.as_ref() }}\n\
                     fn {field_name}_mut(&mut self) -> Option<&mut {rust_type}> {{ self.{field_name}.as_mut() }}\n"
                )
            } else {
                format!(
                    "    fn {field_name}(&self) -> &{rust_type} {{ &self.{field_name} }}\n\
                     fn {field_name}_mut(&mut self) -> &mut {rust_type} {{ &mut self.{field_name} }}\n"
                )
            }
        })
        .collect();

    // --- Ancestor delegation impls ---
    // Fields are inlined, so all ancestor traits are implemented by direct field access.
    let ancestor_impls: String = chain
        .iter()
        .map(|(ancestor_local, ancestor_ct)| {
            let ancestor_pascal = pascal(ancestor_local);
            let methods: String = ancestor_ct
                .fields
                .iter()
                .map(|f| {
                    let field_name = snake(&f.name);
                    let (type_ns, type_local) = resolver.resolve_pair(&f.type_);
                    let rust_type = if let Some(resolved) = simple_map.get(type_local.as_str()) {
                        resolved.clone()
                    } else {
                        xsd_to_rust_concrete(type_ns.as_deref(), &type_local)
                    };
                    if f.optional {
                        format!(
                            "    fn {field_name}(&self) -> Option<&{rust_type}> {{ self.{field_name}.as_ref() }}\n\
                             fn {field_name}_mut(&mut self) -> Option<&mut {rust_type}> {{ self.{field_name}.as_mut() }}\n"
                        )
                    } else {
                        format!(
                            "    fn {field_name}(&self) -> &{rust_type} {{ &self.{field_name} }}\n\
                             fn {field_name}_mut(&mut self) -> &mut {rust_type} {{ &mut self.{field_name} }}\n"
                        )
                    }
                })
                .collect();
            format!("impl crate::uci::types::{ancestor_pascal} for {pascal_name}_ {{\n{methods}}}\n\n")
        })
        .collect();

    let is_element_backed = type_to_element.contains_key(ct.name.as_str());

    let mut out = String::new();
    out.push_str("// @generated — do not edit.\n#![allow(non_camel_case_types)]\n\n");

    // Trait
    out.push_str(&format!("/// Accessor trait for XSD complexType `{}`.\n", ct.name));
    out.push_str(&format!("pub trait {pascal_name} {supertrait}{{\n"));
    out.push_str(&trait_methods);
    out.push_str("}\n\n");

    // Struct
    out.push_str(&format!("/// XSD complexType `{}`.\n", ct.name));
    let derives = if is_element_backed {
        "#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]\n"
    } else {
        "#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]\n"
    };
    out.push_str(derives);
    out.push_str(&format!("#[serde(rename = \"{pascal_name}\")]\n"));
    out.push_str(&format!("pub struct {pascal_name}_ {{\n"));
    out.push_str(&inherited_fields_str);
    out.push_str(&struct_fields);
    if is_element_backed {
        out.push_str("    #[serde(skip)]\n");
        out.push_str("    _priv: crate::uci::sealed::Token,\n");
    }
    out.push_str("}\n\n");

    // _cal_create for element-backed types
    if is_element_backed {
        out.push_str(&format!("impl {pascal_name}_ {{\n"));
        out.push_str("    pub(crate) fn _cal_create() -> Self {\n");
        out.push_str("        Self {\n");
        out.push_str(&inherited_defaults_str);
        out.push_str(&field_defaults);
        out.push_str("            _priv: crate::uci::sealed::Token(()),\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
        out.push_str("}\n\n");
    }

    // Own trait impl
    out.push_str(&format!("impl {pascal_name} for {pascal_name}_ {{\n"));
    out.push_str(&own_trait_impl);
    out.push_str("}\n\n");

    // Ancestor delegation impls
    out.push_str(&ancestor_impls);

    // CalSubMessage marker
    if ct.abstract_ || !is_element_backed {
        out.push_str(&format!("impl crate::uci::CalSubMessage for {pascal_name}_ {{}}\n"));
    }

    out
}

// ════════════════════════════════════════════════════════════════════════════
// Name helpers
// ════════════════════════════════════════════════════════════════════════════

fn pascal(s: &str) -> String {
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
    let mut prev_under = false;
    for (i, c) in local.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 && !prev_upper && !prev_under {
                out.push('_');
            }
            out.push(c.to_lowercase().next().unwrap());
            prev_upper = true;
            prev_under = false;
        } else if c == '_' {
            prev_under = true;
        } else {
            out.push(c);
            prev_upper = false;
            prev_under = false;
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
            format!("V{}{}", first, chars.as_str().to_lowercase())
        }
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
    }
}

/// Map a resolved XSD type `(namespace_uri, local_name)` to a Rust type expression.
///
/// `xs:*` primitives map to `crate::xs::*`; all other types resolve to
/// `crate::uci::types::TypeName`.
fn xsd_to_rust(ns: Option<&str>, local: &str) -> String {
    const XS: &str = "http://www.w3.org/2001/XMLSchema";
    if ns == Some(XS) {
        match local {
            "boolean" => return "crate::xs::Boolean".to_string(),
            "long" => return "crate::xs::Long".to_string(),
            "int" => return "crate::xs::Int".to_string(),
            "short" => return "crate::xs::Short".to_string(),
            "byte" => return "crate::xs::Byte".to_string(),
            "unsignedLong" => return "crate::xs::UnsignedLong".to_string(),
            "unsignedInt" => return "crate::xs::UnsignedInt".to_string(),
            "unsignedShort" => return "crate::xs::UnsignedShort".to_string(),
            "unsignedByte" => return "crate::xs::UnsignedByte".to_string(),
            "double" => return "crate::xs::Double".to_string(),
            "float" => return "crate::xs::Float".to_string(),
            "integer" => return "crate::xs::Integer".to_string(),
            "duration" => return "crate::xs::Duration".to_string(),
            "dateTime" => return "crate::xs::DateTime".to_string(),
            "time" => return "crate::xs::Time".to_string(),
            "string" => return "crate::xs::XsString".to_string(),
            "hexBinary" => return "crate::xs::HexBinary".to_string(),
            _ => {}
        }
    }
    format!("crate::uci::types::{}", pascal(local))
}

/// Like `xsd_to_rust` but appends `_` for non-XS types (concrete struct name, not the trait).
fn xsd_to_rust_concrete(ns: Option<&str>, local: &str) -> String {
    const XS: &str = "http://www.w3.org/2001/XMLSchema";
    if ns == Some(XS) {
        xsd_to_rust(ns, local)
    } else {
        format!("crate::uci::types::{}_", pascal(local))
    }
}

