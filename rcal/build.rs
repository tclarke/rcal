use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use quick_xml::events::Event;
use quick_xml::reader::Reader;

fn main() {
    println!("cargo::rerun-if-env-changed=RCAL_XSD_PATH");
    println!("cargo::rerun-if-env-changed=RCAL_SCHEMA_VERSION");
    println!("cargo::rerun-if-env-changed=RCAL_OMS_COMPILER_VERSION");

    eprintln!("Starting generation step");
    if let Ok(compiler_version) = std::env::var("RCAL_OMS_COMPILER_VERSION") {
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

    let xsd_path = match std::env::var("RCAL_XSD_PATH").ok() {
        Some(p) => p,
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
                println!(
                    "cargo::error=Unable to find a valid schema files {:?}",
                    files
                );
                return;
            };
            v.to_string()
        }
    };
    eprintln!("XSD path={xsd_path}");

    let xsd_path = PathBuf::from(&xsd_path);
    let xsd_content = match fs::read_to_string(&xsd_path) {
        Ok(content) => content,
        Err(e) => {
            println!(
                "cargo::error=Cannot read RCAL_XSD_PATH={}: {e}",
                xsd_path.display()
            );
            return;
        }
    };

    println!("cargo::rerun-if-changed={}", xsd_path.display());

    let schema = parse_xsd_file(
        &xsd_path,
        &xsd_content,
        &mut std::collections::HashSet::new(),
    );

    if let Ok(schema_version) = std::env::var("RCAL_SCHEMA_VERSION") {
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
        self.prefix_to_uri
            .insert(prefix.to_string(), uri.to_string());
        self.uri_to_prefix
            .insert(uri.to_string(), prefix.to_string());
    }

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

/// XSD restriction facets for string and numeric types.
#[derive(Debug, Default, Clone)]
struct Facets {
    length: Option<u32>,
    min_length: Option<u32>,
    max_length: Option<u32>,
    pattern: Option<String>,
    min_inclusive: Option<String>,
    max_inclusive: Option<String>,
}

impl Facets {
    fn is_empty(&self) -> bool {
        self.length.is_none()
            && self.min_length.is_none()
            && self.max_length.is_none()
            && self.pattern.is_none()
            && self.min_inclusive.is_none()
            && self.max_inclusive.is_none()
    }
}

#[derive(Debug)]
enum SimpleTypeKind {
    Enum(Vec<String>),
    Restriction { base: String, facets: Facets },
}

#[derive(Debug)]
struct ComplexType {
    name: String,
    abstract_: bool,
    extension_base: Option<String>,
    fields: Vec<Field>,
    is_choice: bool,
}

/// Maximum occurrences constraint on an XSD element.
#[derive(Debug, PartialEq, Clone)]
enum MaxOccurs {
    Bounded(u32),
    Unbounded,
}

#[derive(Debug)]
struct Field {
    name: String,
    type_: String,
    min_occurs: u32,
    max_occurs: MaxOccurs,
}

impl Field {
    /// True when the field is represented as `Option<T>` (minOccurs=0, maxOccurs=1).
    fn is_optional(&self) -> bool {
        self.min_occurs == 0 && self.max_occurs == MaxOccurs::Bounded(1)
    }

    /// True when the field is represented as `Vec<T>` (maxOccurs > 1 or unbounded).
    fn is_vec(&self) -> bool {
        matches!(self.max_occurs, MaxOccurs::Unbounded)
            || matches!(self.max_occurs, MaxOccurs::Bounded(n) if n > 1)
    }
}

#[derive(Debug)]
struct Element {
    name: String,
    type_: String,
}

// ════════════════════════════════════════════════════════════════════════════
// XSD parser
// ════════════════════════════════════════════════════════════════════════════

fn parse_xsd_file(
    xsd_path: &Path,
    content: &str,
    seen: &mut std::collections::HashSet<PathBuf>,
) -> Schema {
    let canonical = xsd_path
        .canonicalize()
        .unwrap_or_else(|_| xsd_path.to_path_buf());
    seen.insert(canonical);
    let base_dir = xsd_path.parent().unwrap_or(Path::new("."));

    let mut reader = Reader::from_str(content);
    reader.config_mut().trim_text(true);

    let mut schema = Schema::default();
    let mut current_simple: Option<SimpleType> = None;
    let mut current_complex: Option<ComplexType> = None;
    let mut in_restriction = false;
    let mut in_choice_depth: u32 = 0;
    let mut restriction_base: Option<String> = None;
    let mut current_facets = Facets::default();

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
                                kind: SimpleTypeKind::Restriction {
                                    base: "xs:string".into(),
                                    facets: Facets::default(),
                                },
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
                                is_choice: false,
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
                        current_facets = Facets::default();
                    }
                    "enumeration" => {
                        if let (Some(st), Some(val)) = (current_simple.as_mut(), attr(e, "value")) {
                            if let SimpleTypeKind::Enum(ref mut vals) = st.kind {
                                vals.push(val);
                            } else {
                                st.kind = SimpleTypeKind::Enum(vec![val]);
                            }
                        }
                    }
                    // XSD restriction facets
                    "length" => {
                        if in_restriction {
                            current_facets.length = attr(e, "value").and_then(|v| v.parse().ok());
                        }
                    }
                    "minLength" => {
                        if in_restriction {
                            current_facets.min_length =
                                attr(e, "value").and_then(|v| v.parse().ok());
                        }
                    }
                    "maxLength" => {
                        if in_restriction {
                            current_facets.max_length =
                                attr(e, "value").and_then(|v| v.parse().ok());
                        }
                    }
                    "pattern" => {
                        if in_restriction {
                            current_facets.pattern = attr(e, "value");
                        }
                    }
                    "minInclusive" => {
                        if in_restriction {
                            current_facets.min_inclusive = attr(e, "value");
                        }
                    }
                    "maxInclusive" => {
                        if in_restriction {
                            current_facets.max_inclusive = attr(e, "value");
                        }
                    }
                    "choice" => {
                        in_choice_depth += 1;
                        if let Some(ct) = current_complex.as_mut() {
                            ct.is_choice = true;
                        }
                    }
                    "element" => {
                        let min_occurs: u32 = attr(e, "minOccurs")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(1);
                        let max_occurs = match attr(e, "maxOccurs").as_deref() {
                            Some("unbounded") => MaxOccurs::Unbounded,
                            Some(n) => MaxOccurs::Bounded(n.parse().unwrap_or(1)),
                            None => MaxOccurs::Bounded(1),
                        };

                        if current_complex.is_none() && current_simple.is_none() {
                            if let (Some(name), Some(type_)) = (attr(e, "name"), attr(e, "type")) {
                                schema.elements.push(Element { name, type_ });
                            }
                        } else if let Some(ct) = current_complex.as_mut()
                            && let (Some(name), Some(type_)) = (attr(e, "name"), attr(e, "type"))
                        {
                            ct.fields.push(Field {
                                name,
                                type_,
                                min_occurs,
                                max_occurs,
                            });
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
                                && let SimpleTypeKind::Restriction {
                                    ref mut base,
                                    ref mut facets,
                                } = st.kind
                            {
                                if let Some(b) = restriction_base.take() {
                                    *base = b;
                                }
                                *facets = current_facets.clone();
                            }
                            schema.simple_types.push(st);
                        }
                        in_restriction = false;
                        restriction_base = None;
                        current_facets = Facets::default();
                    }
                    "complexType" => {
                        if let Some(ct) = current_complex.take() {
                            schema.complex_types.push(ct);
                        }
                    }
                    "choice" if in_choice_depth > 0 => {
                        in_choice_depth = in_choice_depth.saturating_sub(1);
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
        // unescape_value decodes XML entity refs (e.g. &#x20; → ' ') so regex patterns are valid.
        .and_then(|a| a.unescape_value().ok().map(|s| s.into_owned()))
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
                SimpleTypeKind::Restriction { base, .. } => {
                    let (ns, local) = resolver.resolve_pair(base);
                    xsd_to_rust(ns.as_deref(), &local)
                }
            };
            let rust_ty = if st.name == "UniversallyUniqueIdentifierType" {
                "crate::uci::base::UUID".to_string()
            } else {
                rust_ty
            };
            (st.name.as_str(), rust_ty)
        })
        .collect();

    // Names of all enum simple types (for generating enum checks in is_valid).
    let enum_names: HashSet<&str> = schema
        .simple_types
        .iter()
        .filter(|st| matches!(st.kind, SimpleTypeKind::Enum(_)))
        .map(|st| st.name.as_str())
        .collect();

    // Map: type local name → facets (for string/double constraint checks).
    let facets_map: HashMap<&str, &Facets> = schema
        .simple_types
        .iter()
        .filter_map(|st| match &st.kind {
            SimpleTypeKind::Restriction { facets, .. } if !facets.is_empty() => {
                Some((st.name.as_str(), facets))
            }
            _ => None,
        })
        .collect();

    // Generate simple types
    eprintln!("Generating simple types");
    for st in &schema.simple_types {
        eprintln!("- {}", st.name);
        // UniversallyUniqueIdentifierType maps directly to crate::uci::base::UUID — no alias needed.
        if st.name == "UniversallyUniqueIdentifierType" {
            continue;
        }
        let file_name = format!("{}.rs", snake(&st.name));
        let code = match &st.kind {
            SimpleTypeKind::Enum(vals) => gen_enum(&st.name, vals),
            SimpleTypeKind::Restriction { base, facets: _ } => {
                gen_type_alias(&st.name, base, resolver)
            }
        };
        fs::write(out_dir.join(&file_name), code).unwrap();
        let mod_name = snake(&st.name);
        mod_entries.push(format!(
            "#[doc(hidden)]\npub mod {mod_name};\n#[doc(inline)]\npub use {mod_name}::*;"
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

    // Names of all xs:choice complex types — used to suppress trait generation and dyn dispatch.
    let choice_type_names: HashSet<&str> = schema
        .complex_types
        .iter()
        .filter(|ct| ct.is_choice)
        .map(|ct| ct.name.as_str())
        .collect();

    // Pure-extension complex types (no own fields) are emitted as type aliases.
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
        let code = gen_struct(
            ct,
            &simple_type_map,
            &type_to_element,
            resolver,
            &complex_type_map,
            &enum_names,
            &facets_map,
            &choice_type_names,
        );
        fs::write(out_dir.join(&file_name), code).unwrap();
        let mod_name = snake(&ct.name);
        mod_entries.push(format!(
            "#[doc(hidden)]\n#[allow(missing_docs)]\npub mod {mod_name};\n#[doc(inline)]\npub use {mod_name}::*;"
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
        // Build xmlns field declarations and value initializers for the __Ns<'a> serialize wrapper.
        let mut ns_struct_fields = String::new();
        let mut ns_struct_values = String::new();
        if let Some(ref default_ns) = resolver.default_ns {
            ns_struct_fields.push_str(
                "            #[serde(rename = \"@xmlns\")]\n            xmlns: &'static str,\n",
            );
            ns_struct_values.push_str(&format!("            xmlns: \"{default_ns}\",\n"));
        }
        ns_struct_fields.push_str(
            "            #[serde(rename = \"@xmlns:xsi\")]\n            xmlns_xsi: &'static str,\n",
        );
        ns_struct_values
            .push_str("            xmlns_xsi: \"http://www.w3.org/2001/XMLSchema-instance\",\n");
        let mut sorted_prefixes: Vec<_> = resolver.prefix_to_uri.iter().collect();
        sorted_prefixes.sort_by_key(|(k, _)| k.as_str());
        for (prefix, uri) in &sorted_prefixes {
            let field_name = format!("xmlns_{}", prefix.replace(['-', ':'], "_"));
            ns_struct_fields.push_str(&format!(
                "            #[serde(rename = \"@xmlns:{prefix}\")]\n            {field_name}: &'static str,\n"
            ));
            ns_struct_values.push_str(&format!("            {field_name}: \"{uri}\",\n"));
        }
        let code = format!(
            "// @generated — do not edit.\n#![allow(non_camel_case_types)]\n\n\
             /// XSD element `{el_name}`. Wraps [`{type_pascal}_`]({type_path_concrete}).\n\
             #[derive(Debug, Clone, serde::Deserialize)]\n\
             #[serde(transparent)]\n\
             pub struct {el_pascal}_(pub {type_path_concrete});\n\n\
             impl serde::Serialize for {el_pascal}_ {{\n\
             \x20   fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {{\n\
             \x20       #[derive(serde::Serialize)]\n\
             \x20       struct __Ns<'a> {{\n\
             {ns_struct_fields}\
             \x20           #[serde(flatten)]\n\
             \x20           inner: &'a {type_path_concrete},\n\
             \x20       }}\n\
             \x20       __Ns {{\n\
             {ns_struct_values}\
             \x20           inner: &self.0,\n\
             \x20       }}.serialize(serializer)\n\
             \x20   }}\n\
             }}\n\n\
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
             \x20   fn is_valid(&self) -> Result<(), crate::uci::ValidationError> {{\n\
             \x20       self.0.is_valid_at(\"{el_name}\")\n\
             \x20   }}\n\
             }}\n",
            el_name = el.name,
            ns_struct_fields = ns_struct_fields,
            ns_struct_values = ns_struct_values,
        );
        fs::write(out_dir.join(format!("{el_module}.rs")), code).unwrap();
        mod_entries.push(format!(
            "#[doc(hidden)]\n#[allow(missing_docs)]\npub mod {el_module};\n#[doc(inline)]\npub use {el_module}::*;"
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

    let mut match_arms: String =
        format!("                    \"enumNotSet\" => Ok({pascal_name}::EnumNotSet),\n");
    match_arms.push_str(
        &variants
            .iter()
            .map(|(variant, orig)| {
                format!("                    \"{orig}\" => Ok({pascal_name}::{variant}),\n")
            })
            .collect::<String>(),
    );
    let mut variant_names: Vec<String> = vec!["\"enumNotSet\"".to_string()];
    variant_names.extend(variants.iter().map(|(_, orig)| format!("\"{orig}\"")));
    let variant_names_str = variant_names.join(", ");

    let mut out = String::new();
    out.push_str("// @generated — do not edit.\n#![allow(non_camel_case_types, non_snake_case, clippy::approx_constant, clippy::excessive_precision, clippy::wrong_self_convention)]\n\n");
    out.push_str(&format!("/// XSD simpleType `{name}`.\n"));
    out.push_str("#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]\n");
    out.push_str(&format!("#[serde(rename = \"{pascal_name}\")]\n"));
    out.push_str(&format!("pub enum {pascal_name} {{\n"));
    out.push_str("    /// Unset/default sentinel.\n    #[default]\n    #[serde(rename = \"enumNotSet\")]\n    EnumNotSet,\n");
    for (variant, orig) in &variants {
        out.push_str(&format!(
            "    /// `{orig}` variant.\n    #[serde(rename = \"{orig}\")]\n    {variant},\n"
        ));
    }
    out.push_str("}\n\n");
    // Custom Deserialize impl
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
         }}\n\n"
    ));
    // is_valid
    out.push_str(&format!(
        "impl {pascal_name} {{\n\
         \x20   /// Returns `Err` if this enum is still at the default `EnumNotSet` sentinel.\n\
         \x20   pub fn is_valid_at(&self, path: &str) -> Result<(), crate::uci::ValidationError> {{\n\
         \x20       if matches!(self, {pascal_name}::EnumNotSet) {{\n\
         \x20           return Err(crate::uci::ValidationError {{\n\
         \x20               path: path.to_owned(),\n\
         \x20               reason: \"enum not set\".to_owned(),\n\
         \x20           }});\n\
         \x20       }}\n\
         \x20       Ok(())\n\
         \x20   }}\n\
         }}\n"
    ));
    out
}

fn gen_type_alias(name: &str, base: &str, resolver: &XsdResolver) -> String {
    let pascal_name = pascal(name);
    let (ns, local) = resolver.resolve_pair(base);
    let rust_type = xsd_to_rust(ns.as_deref(), &local);
    format!(
        "// @generated — do not edit.\n#![allow(non_camel_case_types)]\n\n/// XSD simpleType `{name}`.\npub type {pascal_name} = {rust_type};\n"
    )
}

fn resolve_base_rust_type(
    base: &str,
    simple_map: &HashMap<&str, String>,
    resolver: &XsdResolver,
) -> String {
    let (ns, local) = resolver.resolve_pair(base);
    if let Some(resolved) = simple_map.get(local.as_str()) {
        resolved.clone()
    } else {
        xsd_to_rust_concrete(ns.as_deref(), &local)
    }
}

fn field_rust_type(
    f: &Field,
    simple_map: &HashMap<&str, String>,
    resolver: &XsdResolver,
) -> String {
    let (type_ns, type_local) = resolver.resolve_pair(&f.type_);
    let base = if let Some(resolved) = simple_map.get(type_local.as_str()) {
        resolved.clone()
    } else {
        xsd_to_rust_concrete(type_ns.as_deref(), &type_local)
    };
    if f.is_vec() {
        format!("Vec<{base}>")
    } else if f.is_optional() {
        format!("Option<{base}>")
    } else {
        base
    }
}

/// Returns true when a ComplexType is emitted as a type alias (not a trait).
///
/// gen_struct early-returns a `type Foo = ...` alias when the type has no
/// own fields and merely re-exports an extension base.  Such entries must be
/// excluded from ancestor delegation chains because type aliases cannot be
/// used as trait bounds.
fn is_type_alias(ct: &ComplexType) -> bool {
    ct.fields.is_empty() && ct.extension_base.is_some()
}

/// Collect the inheritance chain: [(local_name, &ComplexType)] from immediate base upward.
///
/// Ancestors that would be emitted as type aliases are skipped — they have no
/// corresponding trait definition and cannot appear in `impl Foo for Bar`.
fn base_chain<'a>(
    ct: &'a ComplexType,
    complex_map: &'a HashMap<&str, &'a ComplexType>,
) -> Vec<(&'a str, &'a ComplexType)> {
    let mut chain = Vec::new();
    let mut current = ct;
    while let Some(base_ref) = &current.extension_base {
        let local = base_ref
            .rfind(':')
            .map(|i| &base_ref[i + 1..])
            .unwrap_or(base_ref.as_str());
        match complex_map.get(local) {
            Some(base_ct) => {
                if !is_type_alias(base_ct) {
                    chain.push((local, *base_ct));
                }
                current = base_ct;
            }
            None => break,
        }
    }
    chain
}

/// Generate the is_valid() check code for a single field.
///
/// Returns a (possibly empty) block of Rust statements to be placed inside
/// the is_valid() body. Each statement returns early with Err on violation.
fn gen_field_validation(
    f: &Field,
    simple_map: &HashMap<&str, String>,
    resolver: &XsdResolver,
    enum_names: &HashSet<&str>,
    facets_map: &HashMap<&str, &Facets>,
) -> String {
    let field_name = snake(&f.name);
    let xsd_name = &f.name;
    let (type_ns, type_local) = resolver.resolve_pair(&f.type_);

    let rust_type = if let Some(resolved) = simple_map.get(type_local.as_str()) {
        resolved.clone()
    } else {
        xsd_to_rust_concrete(type_ns.as_deref(), &type_local)
    };

    // ── Vec fields ────────────────────────────────────────────────────────
    if f.is_vec() {
        let mut out = String::new();

        // Length check: skip only when min=0 AND unbounded (no constraint at all).
        let needs_len_check = f.min_occurs > 0 || matches!(f.max_occurs, MaxOccurs::Bounded(_));
        if needs_len_check {
            let min = f.min_occurs;
            // Use range-contains syntax to satisfy clippy::manual_range_contains.
            let (cond, max_desc) = match (min > 0, &f.max_occurs) {
                (true, MaxOccurs::Bounded(n)) => {
                    (format!("!({min}..={n}).contains(&_n)"), n.to_string())
                }
                (true, MaxOccurs::Unbounded) => (format!("_n < {min}"), "unbounded".to_string()),
                (false, MaxOccurs::Bounded(n)) => (format!("_n > {n}"), n.to_string()),
                (false, MaxOccurs::Unbounded) => {
                    unreachable!("needs_len_check requires min>0 or bounded max")
                }
            };
            out.push_str(&format!(
                "    {{\n\
                 \x20       let _n = self.{field_name}.len();\n\
                 \x20       if {cond} {{\n\
                 \x20           return Err(crate::uci::ValidationError {{\n\
                 \x20               path: format!(\"{{path}}.{xsd_name}\"),\n\
                 \x20               reason: format!(\"incorrect number of elements: got {{_n}}, expected {min}..={max_desc}\"),\n\
                 \x20           }});\n\
                 \x20       }}\n\
                 \x20   }}\n"
            ));
        }

        // Recurse into Vec elements if they have their own is_valid().
        if is_validatable_type(&rust_type, enum_names, &type_local) {
            out.push_str(&format!(
                "    for (_i, _item) in self.{field_name}.iter().enumerate() {{\n\
                 \x20       _item.is_valid_at(&format!(\"{{path}}.{xsd_name}[{{_i}}]\"))?;\n\
                 \x20   }}\n"
            ));
        }

        return out;
    }

    let is_opt = f.is_optional();

    // ── Enum fields ───────────────────────────────────────────────────────
    if enum_names.contains(type_local.as_str()) {
        return if is_opt {
            format!(
                "    if let Some(ref _v) = self.{field_name} {{\n\
                 \x20       _v.is_valid_at(&format!(\"{{path}}.{xsd_name}\"))?;\n\
                 \x20   }}\n"
            )
        } else {
            format!("    self.{field_name}.is_valid_at(&format!(\"{{path}}.{xsd_name}\"))?;\n")
        };
    }

    // ── Complex struct fields ─────────────────────────────────────────────
    if rust_type.starts_with("crate::uci::types::") && rust_type.ends_with('_') {
        return if is_opt {
            format!(
                "    if let Some(ref _v) = self.{field_name} {{\n\
                 \x20       _v.is_valid_at(&format!(\"{{path}}.{xsd_name}\"))?;\n\
                 \x20   }}\n"
            )
        } else {
            format!("    self.{field_name}.is_valid_at(&format!(\"{{path}}.{xsd_name}\"))?;\n")
        };
    }

    // ── xs:string with facets ─────────────────────────────────────────────
    if rust_type == "crate::xs::XsString"
        && let Some(facets) = facets_map.get(type_local.as_str())
    {
        let mut out = String::new();

        // String length check
        let eff_min = facets.length.or(facets.min_length).unwrap_or(0) as usize;
        let eff_max = facets.length.or(facets.max_length).map(|v| v as usize);
        let has_len_check = eff_min > 0 || eff_max.is_some();

        if has_len_check {
            // Use range-contains syntax to satisfy clippy::manual_range_contains.
            let cond = match (eff_min > 0, eff_max) {
                (true, Some(max)) => format!("!({eff_min}..={max}).contains(&_n)"),
                (true, None) => format!("_n < {eff_min}"),
                (false, Some(max)) => format!("_n > {max}"),
                (false, None) => unreachable!("has_len_check requires min>0 or max set"),
            };
            if is_opt {
                out.push_str(&format!(
                    "    if let Some(ref _v) = self.{field_name} {{\n\
                     \x20       let _n = _v.chars().count();\n\
                     \x20       if {cond} {{\n\
                     \x20           return Err(crate::uci::ValidationError {{\n\
                     \x20               path: format!(\"{{path}}.{xsd_name}\"),\n\
                     \x20               reason: \"string does not match constraints\".to_owned(),\n\
                     \x20           }});\n\
                     \x20       }}\n\
                     \x20   }}\n"
                ));
            } else {
                out.push_str(&format!(
                    "    {{\n\
                     \x20       let _n = self.{field_name}.chars().count();\n\
                     \x20       if {cond} {{\n\
                     \x20           return Err(crate::uci::ValidationError {{\n\
                     \x20               path: format!(\"{{path}}.{xsd_name}\"),\n\
                     \x20               reason: \"string does not match constraints\".to_owned(),\n\
                     \x20           }});\n\
                     \x20       }}\n\
                     \x20   }}\n"
                ));
            }
        }

        // Pattern check
        if let Some(pattern) = &facets.pattern {
            let static_name = format!("PATTERN_{}", snake(&f.name).to_uppercase());
            let pattern_escaped = pattern.replace('\\', "\\\\").replace('"', "\\\"");
            if is_opt {
                out.push_str(&format!(
                    "    if let Some(ref _v) = self.{field_name} {{\n\
                     \x20       static {static_name}: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();\n\
                     \x20       let _re = {static_name}.get_or_init(|| regex::Regex::new(\"{pattern_escaped}\").unwrap());\n\
                     \x20       if !_re.is_match(_v) {{\n\
                     \x20           return Err(crate::uci::ValidationError {{\n\
                     \x20               path: format!(\"{{path}}.{xsd_name}\"),\n\
                     \x20               reason: \"string does not match constraints\".to_owned(),\n\
                     \x20           }});\n\
                     \x20       }}\n\
                     \x20   }}\n"
                ));
            } else {
                out.push_str(&format!(
                    "    {{\n\
                     \x20       static {static_name}: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();\n\
                     \x20       let _re = {static_name}.get_or_init(|| regex::Regex::new(\"{pattern_escaped}\").unwrap());\n\
                     \x20       if !_re.is_match(&self.{field_name}) {{\n\
                     \x20           return Err(crate::uci::ValidationError {{\n\
                     \x20               path: format!(\"{{path}}.{xsd_name}\"),\n\
                     \x20               reason: \"string does not match constraints\".to_owned(),\n\
                     \x20           }});\n\
                     \x20       }}\n\
                     \x20   }}\n"
                ));
            }
        }

        if !out.is_empty() {
            return out;
        }
    }

    // ── xs:double / xs:float with range facets ────────────────────────────
    if (rust_type == "crate::xs::Double" || rust_type == "crate::xs::Float")
        && let Some(facets) = facets_map.get(type_local.as_str())
    {
        let float_suffix = if rust_type == "crate::xs::Double" {
            "f64"
        } else {
            "f32"
        };
        let has_min = facets.min_inclusive.is_some();
        let has_max = facets.max_inclusive.is_some();
        if has_min || has_max {
            let min_val = facets.min_inclusive.as_deref().unwrap_or("0");
            let max_val = facets.max_inclusive.as_deref().unwrap_or("0");
            // Use range-contains to satisfy clippy::manual_range_contains.
            // approx_constant / excessive_precision are suppressed at the file level.
            let cond = match (has_min, has_max) {
                (true, true) => {
                    format!("!({min_val}_{float_suffix}..={max_val}_{float_suffix}).contains(&_v)")
                }
                (true, false) => format!("_v < {min_val}_{float_suffix}"),
                (false, true) => format!("_v > {max_val}_{float_suffix}"),
                (false, false) => unreachable!(),
            };
            let range_str = match (has_min, has_max) {
                (true, true) => format!("[{min_val}, {max_val}]"),
                (true, false) => format!("[{min_val}, ∞)"),
                (false, true) => format!("(-∞, {max_val}]"),
                (false, false) => unreachable!(),
            };
            return if is_opt {
                // Combine let + range check to avoid collapsible_if.
                format!(
                    "    if let Some(_v) = self.{field_name}\n\
                     \x20       && {cond}\n\
                     \x20   {{\n\
                     \x20       return Err(crate::uci::ValidationError {{\n\
                     \x20           path: format!(\"{{path}}.{xsd_name}\"),\n\
                     \x20           reason: \"double is outside allowed range {range_str}\".to_owned(),\n\
                     \x20       }});\n\
                     \x20   }}\n"
                )
            } else {
                format!(
                    "    {{\n\
                     \x20       let _v = self.{field_name};\n\
                     \x20       if {cond} {{\n\
                     \x20           return Err(crate::uci::ValidationError {{\n\
                     \x20               path: format!(\"{{path}}.{xsd_name}\"),\n\
                     \x20               reason: \"double is outside allowed range {range_str}\".to_owned(),\n\
                     \x20           }});\n\
                     \x20       }}\n\
                     \x20   }}\n"
                )
            };
        }
    }

    String::new()
}

/// True if the given Rust type exposes an `is_valid(path: &str)` method.
fn is_validatable_type(rust_type: &str, enum_names: &HashSet<&str>, type_local: &str) -> bool {
    enum_names.contains(type_local)
        || (rust_type.starts_with("crate::uci::types::") && rust_type.ends_with('_'))
}

/// Converts a concrete complex-type path (`crate::uci::types::FooType_`) to its
/// `dyn Trait` form (`dyn crate::uci::types::FooType`) for use in trait signatures.
/// Non-complex types are returned unchanged.
fn dyn_type(rust_type: &str) -> String {
    if rust_type.starts_with("crate::uci::types::") && rust_type.ends_with('_') {
        format!("dyn {}", &rust_type[..rust_type.len() - 1])
    } else {
        rust_type.to_string()
    }
}

fn gen_choice_enum(
    ct: &ComplexType,
    simple_map: &HashMap<&str, String>,
    resolver: &XsdResolver,
    enum_names: &HashSet<&str>,
) -> String {
    let pascal_name = pascal(&ct.name);
    let mut out = String::new();

    out.push_str("// @generated — do not edit.\n#![allow(non_camel_case_types, non_snake_case, clippy::approx_constant, clippy::excessive_precision, clippy::wrong_self_convention, clippy::large_enum_variant)]\n\n");

    out.push_str(&format!(
        "/// XSD complexType `{}` (xs:choice).\n",
        ct.name
    ));
    out.push_str(
        "#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]\n\
         #[serde(untagged)]\n",
    );
    out.push_str(&format!("pub enum {pascal_name}_ {{\n"));

    let mut first_variant_name = String::new();
    let mut first_payload_type = String::new();

    for (i, f) in ct.fields.iter().enumerate() {
        let variant_name = pascal(&f.name);
        let (type_ns, type_local) = resolver.resolve_pair(&f.type_);
        let payload_type = if let Some(resolved) = simple_map.get(type_local.as_str()) {
            resolved.clone()
        } else {
            xsd_to_rust_concrete(type_ns.as_deref(), &type_local)
        };
        if i == 0 {
            first_variant_name = variant_name.clone();
            first_payload_type = payload_type.clone();
        }
        let doc = format!("    /// XSD element `{}`.\n", f.name);
        out.push_str(&format!(
            "{doc}    {variant_name} {{\n\
             \x20       #[serde(rename = \"{xsd_name}\")]\n\
             \x20       inner: {payload_type},\n\
             \x20   }},\n",
            xsd_name = f.name,
        ));
    }
    out.push_str("}\n\n");

    // Manual Default impl — derive(Default) requires #[default] which only works on unit variants.
    if !first_variant_name.is_empty() {
        out.push_str(&format!(
            "impl Default for {pascal_name}_ {{\n\
             \x20   fn default() -> Self {{\n\
             \x20       {pascal_name}_::{first_variant_name} {{ inner: <{first_payload_type}>::default() }}\n\
             \x20   }}\n\
             }}\n\n"
        ));
    }

    // is_valid_at: match on active variant only.
    let any_validatable = ct.fields.iter().any(|f| {
        let (type_ns, type_local) = resolver.resolve_pair(&f.type_);
        let rust_type = if let Some(resolved) = simple_map.get(type_local.as_str()) {
            resolved.clone()
        } else {
            xsd_to_rust_concrete(type_ns.as_deref(), &type_local)
        };
        is_validatable_type(&rust_type, enum_names, &type_local)
    });
    let path_param = if any_validatable { "path" } else { "_path" };
    out.push_str(&format!("impl {pascal_name}_ {{\n"));
    out.push_str(&format!(
        "    pub fn is_valid_at(&self, {path_param}: &str) -> Result<(), crate::uci::ValidationError> {{\n\
         \x20       match self {{\n"
    ));
    for f in &ct.fields {
        let variant_name = pascal(&f.name);
        let (type_ns, type_local) = resolver.resolve_pair(&f.type_);
        let rust_type = if let Some(resolved) = simple_map.get(type_local.as_str()) {
            resolved.clone()
        } else {
            xsd_to_rust_concrete(type_ns.as_deref(), &type_local)
        };
        let (binding, validation) = if is_validatable_type(&rust_type, enum_names, &type_local) {
            (
                "inner".to_string(),
                format!("inner.is_valid_at(&format!(\"{{path}}.{}\"))", f.name),
            )
        } else {
            ("inner: _".to_string(), "Ok(())".to_string())
        };
        out.push_str(&format!(
            "            {pascal_name}_::{variant_name} {{ {binding} }} => {validation},\n"
        ));
    }
    out.push_str("        }\n    }\n}\n\n");

    out.push_str(&format!(
        "impl crate::uci::CalSubMessage for {pascal_name}_ {{}}\n"
    ));

    out
}

#[allow(clippy::too_many_arguments)]
fn gen_struct(
    ct: &ComplexType,
    simple_map: &HashMap<&str, String>,
    type_to_element: &HashMap<&str, &str>,
    resolver: &XsdResolver,
    complex_map: &HashMap<&str, &ComplexType>,
    enum_names: &HashSet<&str>,
    facets_map: &HashMap<&str, &Facets>,
    choice_type_names: &HashSet<&str>,
) -> String {
    let pascal_name = pascal(&ct.name);

    // xs:choice types are emitted as enums, not structs.
    if ct.is_choice {
        return gen_choice_enum(ct, simple_map, resolver, enum_names);
    }

    // Extension with no additional fields → type alias; no trait needed.
    if ct.fields.is_empty()
        && let Some(base) = &ct.extension_base
    {
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
        let dyn_rt = if choice_type_names.contains(type_local.as_str()) {
            rust_type.clone()
        } else {
            dyn_type(&rust_type)
        };
        if f.is_vec() {
            trait_methods.push_str(&format!(
                "    /// Returns the XSD element sequence `{elem}`.\n\
                 \x20   fn {field_name}(&self) -> &[{rust_type}];\n\
                 \x20   /// Returns a mutable reference to the XSD element sequence `{elem}`.\n\
                 \x20   fn {field_name}_mut(&mut self) -> &mut Vec<{rust_type}>;\n",
                elem = f.name,
            ));
        } else if f.is_optional() {
            trait_methods.push_str(&format!(
                "    /// Returns the optional XSD element `{elem}`.\n\
                 \x20   fn {field_name}(&self) -> Option<&{dyn_rt}>;\n\
                 \x20   /// Returns a mutable reference to the optional XSD element `{elem}`.\n\
                 \x20   fn {field_name}_mut(&mut self) -> Option<&mut {dyn_rt}>;\n",
                elem = f.name,
            ));
        } else {
            trait_methods.push_str(&format!(
                "    /// Returns the XSD element `{elem}`.\n\
                 \x20   fn {field_name}(&self) -> &{dyn_rt};\n\
                 \x20   /// Returns a mutable reference to the XSD element `{elem}`.\n\
                 \x20   fn {field_name}_mut(&mut self) -> &mut {dyn_rt};\n",
                elem = f.name,
            ));
        }
    }

    // --- Struct fields ---
    let inherited_fields_str: String = chain
        .iter()
        .rev()
        .flat_map(|(_, ancestor_ct)| &ancestor_ct.fields)
        .map(|f| {
            let field_name = snake(&f.name);
            let full_type = field_rust_type(f, simple_map, resolver);
            let tag = if f.is_vec() {
                " (sequence, inherited)"
            } else if f.is_optional() {
                " (optional, inherited)"
            } else {
                " (inherited)"
            };
            let doc = format!("    /// XSD element `{}`{tag}.\n", f.name);
            let serde_rename = format!("    #[serde(rename = \"{}\")]\n", f.name);
            let maybe_skip = if f.is_optional() {
                "    #[serde(skip_serializing_if = \"Option::is_none\")]\n"
            } else if f.is_vec() {
                "    #[serde(default, skip_serializing_if = \"Vec::is_empty\")]\n"
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
            let default_val = if f.is_optional() {
                "None".to_string()
            } else {
                "Default::default()".to_string()
            };
            format!("            {field_name}: {default_val},\n")
        })
        .collect();

    let struct_fields: String = ct
        .fields
        .iter()
        .map(|f| {
            let field_name = snake(&f.name);
            let full_type = field_rust_type(f, simple_map, resolver);
            let tag = if f.is_vec() {
                " (sequence)"
            } else if f.is_optional() {
                " (optional)"
            } else {
                ""
            };
            let doc = format!("    /// XSD element `{}`{tag}.\n", f.name);
            let serde_rename = format!("    #[serde(rename = \"{}\")]\n", f.name);
            let maybe_skip = if f.is_optional() {
                "    #[serde(skip_serializing_if = \"Option::is_none\")]\n"
            } else if f.is_vec() {
                "    #[serde(default, skip_serializing_if = \"Vec::is_empty\")]\n"
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
            let default_val = if f.is_optional() {
                "None".to_string()
            } else {
                "Default::default()".to_string()
            };
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
            let is_choice_field = choice_type_names.contains(type_local.as_str());
            let dyn_rt = if is_choice_field {
                rust_type.clone()
            } else {
                dyn_type(&rust_type)
            };
            if f.is_vec() {
                format!(
                    "    fn {field_name}(&self) -> &[{rust_type}] {{ &self.{field_name} }}\n\
                     fn {field_name}_mut(&mut self) -> &mut Vec<{rust_type}> {{ &mut self.{field_name} }}\n"
                )
            } else if f.is_optional() {
                if is_choice_field {
                    format!(
                        "    fn {field_name}(&self) -> Option<&{dyn_rt}> {{ self.{field_name}.as_ref() }}\n\
                         fn {field_name}_mut(&mut self) -> Option<&mut {dyn_rt}> {{ self.{field_name}.as_mut() }}\n"
                    )
                } else {
                    format!(
                        "    fn {field_name}(&self) -> Option<&{dyn_rt}> {{ self.{field_name}.as_ref().map(|v| v as &{dyn_rt}) }}\n\
                         fn {field_name}_mut(&mut self) -> Option<&mut {dyn_rt}> {{ self.{field_name}.as_mut().map(|v| v as &mut {dyn_rt}) }}\n"
                    )
                }
            } else {
                format!(
                    "    fn {field_name}(&self) -> &{dyn_rt} {{ &self.{field_name} }}\n\
                     fn {field_name}_mut(&mut self) -> &mut {dyn_rt} {{ &mut self.{field_name} }}\n"
                )
            }
        })
        .collect();

    // --- Ancestor delegation impls ---
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
                    let is_choice_field = choice_type_names.contains(type_local.as_str());
                    let dyn_rt = if is_choice_field {
                        rust_type.clone()
                    } else {
                        dyn_type(&rust_type)
                    };
                    if f.is_vec() {
                        format!(
                            "    fn {field_name}(&self) -> &[{rust_type}] {{ &self.{field_name} }}\n\
                             fn {field_name}_mut(&mut self) -> &mut Vec<{rust_type}> {{ &mut self.{field_name} }}\n"
                        )
                    } else if f.is_optional() {
                        if is_choice_field {
                            format!(
                                "    fn {field_name}(&self) -> Option<&{dyn_rt}> {{ self.{field_name}.as_ref() }}\n\
                                 fn {field_name}_mut(&mut self) -> Option<&mut {dyn_rt}> {{ self.{field_name}.as_mut() }}\n"
                            )
                        } else {
                            format!(
                                "    fn {field_name}(&self) -> Option<&{dyn_rt}> {{ self.{field_name}.as_ref().map(|v| v as &{dyn_rt}) }}\n\
                                 fn {field_name}_mut(&mut self) -> Option<&mut {dyn_rt}> {{ self.{field_name}.as_mut().map(|v| v as &mut {dyn_rt}) }}\n"
                            )
                        }
                    } else {
                        format!(
                            "    fn {field_name}(&self) -> &{dyn_rt} {{ &self.{field_name} }}\n\
                             fn {field_name}_mut(&mut self) -> &mut {dyn_rt} {{ &mut self.{field_name} }}\n"
                        )
                    }
                })
                .collect();
            format!("impl crate::uci::types::{ancestor_pascal} for {pascal_name}_ {{\n{methods}}}\n\n")
        })
        .collect();

    let is_element_backed = type_to_element.contains_key(ct.name.as_str());

    // --- is_valid() body ---
    // Collect checks for all fields (inherited first, then own).
    let all_fields: Vec<&Field> = chain
        .iter()
        .rev()
        .flat_map(|(_, ancestor_ct)| ancestor_ct.fields.iter())
        .chain(ct.fields.iter())
        .collect();

    let validation_checks: String = all_fields
        .iter()
        .map(|f| gen_field_validation(f, simple_map, resolver, enum_names, facets_map))
        .collect();

    let mut out = String::new();
    out.push_str("// @generated — do not edit.\n#![allow(non_camel_case_types, non_snake_case, clippy::approx_constant, clippy::excessive_precision, clippy::wrong_self_convention)]\n\n");

    // Trait
    out.push_str(&format!(
        "/// Accessor trait for XSD complexType `{}`.\n",
        ct.name
    ));
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

    // is_valid()
    let path_param = if validation_checks.is_empty() {
        "_path"
    } else {
        "path"
    };
    out.push_str(&format!("impl {pascal_name}_ {{\n"));
    out.push_str("    /// Validates all fields against their XSD schema constraints.\n");
    out.push_str("    ///\n");
    out.push_str(
        "    /// `path` is the dot-separated path to this element, used in error messages.\n",
    );
    out.push_str(&format!("    pub fn is_valid_at(&self, {path_param}: &str) -> Result<(), crate::uci::ValidationError> {{\n"));
    out.push_str(&validation_checks);
    out.push_str("        Ok(())\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");

    // Own trait impl
    out.push_str(&format!("impl {pascal_name} for {pascal_name}_ {{\n"));
    out.push_str(&own_trait_impl);
    out.push_str("}\n\n");

    // Ancestor delegation impls
    out.push_str(&ancestor_impls);

    // CalSubMessage marker
    if ct.abstract_ || !is_element_backed {
        out.push_str(&format!(
            "impl crate::uci::CalSubMessage for {pascal_name}_ {{}}\n"
        ));
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
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
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

fn xsd_to_rust_concrete(ns: Option<&str>, local: &str) -> String {
    const XS: &str = "http://www.w3.org/2001/XMLSchema";
    if ns == Some(XS) {
        xsd_to_rust(ns, local)
    } else {
        format!("crate::uci::types::{}_", pascal(local))
    }
}
