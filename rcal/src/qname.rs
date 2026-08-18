//! XML qualified name and namespace resolution utilities.
//!
//! Provides [`QName`] (an owned XML qualified name) and [`NamespaceResolver`]
//! (a prefix-to-URI map used to format qualified names in their shortest form).
//!
//! ## Formats understood by [`QName::parse`]
//!
//! | Input | Interpretation |
//! |---|---|
//! | `local` | Local name, no namespace |
//! | `prefix:local` | Prefixed name (URI unknown without a resolver) |
//! | `{uri}local` | Clark notation — namespace URI explicit |
//!
//! ## Display / shortest-form resolution
//!
//! [`QName::resolve`] returns the shortest string given a [`NamespaceResolver`]:
//! - no prefix when the namespace matches the resolver's default namespace
//! - `prefix:local` when the namespace maps to a known prefix
//! - `{uri}local` Clark notation as a last resort

use std::collections::HashMap;
use std::fmt;

// ════════════════════════════════════════════════════════════════════════════
// QName
// ════════════════════════════════════════════════════════════════════════════

/// An owned XML qualified name.
///
/// Stores the namespace URI (if any), local name, and a pre-computed display
/// string chosen at creation time. [`Display`](fmt::Display) returns that
/// pre-computed form; use [`clark`](QName::clark) for the always-unambiguous
/// Clark notation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QName {
    namespace: Option<String>,
    local: String,
    /// Pre-computed display string (Clark, prefixed, or bare, depending on how
    /// the QName was created).
    display: String,
}

impl QName {
    /// Construct from an optional namespace URI and local name.
    ///
    /// The display form defaults to Clark notation (`{uri}local`) when a
    /// namespace is present, or the bare local name otherwise.
    pub fn new(namespace: Option<&str>, local: &str) -> Self {
        let display = match namespace {
            Some(ns) => format!("{{{ns}}}{local}"),
            None => local.to_string(),
        };
        Self {
            namespace: namespace.map(|s| s.to_string()),
            local: local.to_string(),
            display,
        }
    }

    /// Construct with an explicit pre-computed display form.
    ///
    /// Used by the build script to embed a namespace-resolved display (e.g.
    /// `"SystemStatusType"` for a type in the default namespace) directly into
    /// generated code.
    pub fn with_display(namespace: Option<&str>, local: &str, display: &str) -> Self {
        Self {
            namespace: namespace.map(|s| s.to_string()),
            local: local.to_string(),
            display: display.to_string(),
        }
    }

    /// Parse a string in any of the three supported formats:
    ///
    /// - `{uri}local` — Clark notation; namespace URI is captured.
    /// - `prefix:local` — prefixed; namespace URI is *not* captured (use a
    ///   [`NamespaceResolver`] to resolve it afterwards).
    /// - `local` — bare local name; no namespace.
    pub fn parse(s: &str) -> Self {
        if let Some(rest) = s.strip_prefix('{')
            && let Some(end) = rest.find('}')
        {
            let ns = rest[..end].to_string();
            let local = rest[end + 1..].to_string();
            let display = s.to_string();
            return Self { namespace: Some(ns), local, display };
        }
        if let Some(colon) = s.find(':') {
            // Prefixed form — URI unknown without a resolver.
            Self {
                namespace: None,
                local: s[colon + 1..].to_string(),
                display: s.to_string(),
            }
        } else {
            Self {
                namespace: None,
                local: s.to_string(),
                display: s.to_string(),
            }
        }
    }

    /// The local part of the qualified name.
    pub fn local(&self) -> &str { &self.local }

    /// The namespace URI, if one is known.
    pub fn namespace(&self) -> Option<&str> { self.namespace.as_deref() }

    /// The unambiguous Clark notation: `{uri}local` or bare `local` when there
    /// is no namespace.
    pub fn clark(&self) -> String {
        match &self.namespace {
            Some(ns) => format!("{{{ns}}}{}", self.local),
            None => self.local.clone(),
        }
    }

    /// Format this QName using `resolver` to produce the shortest display form.
    ///
    /// - Returns the bare local name if `namespace` matches the resolver's
    ///   default namespace.
    /// - Returns `prefix:local` if the namespace has a known prefix.
    /// - Returns Clark notation `{uri}local` as a last resort.
    pub fn resolve(&self, resolver: &NamespaceResolver) -> String {
        resolver.format(self.namespace.as_deref(), &self.local)
    }
}

impl fmt::Display for QName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display)
    }
}

impl From<&str> for QName {
    fn from(s: &str) -> Self { Self::parse(s) }
}

impl From<String> for QName {
    fn from(s: String) -> Self { Self::parse(&s) }
}

impl From<QName> for String {
    fn from(q: QName) -> Self { q.display }
}

impl PartialEq<str> for QName {
    fn eq(&self, other: &str) -> bool { self.display == other }
}

impl PartialEq<&str> for QName {
    fn eq(&self, other: &&str) -> bool { self.display == *other }
}

impl PartialEq<String> for QName {
    fn eq(&self, other: &String) -> bool { self.display == *other }
}

// ════════════════════════════════════════════════════════════════════════════
// NamespaceResolver
// ════════════════════════════════════════════════════════════════════════════

/// Maps XML namespace prefixes to URIs and formats [`QName`] display strings.
///
/// The `xs` → `http://www.w3.org/2001/XMLSchema` mapping is always present
/// (added by [`NamespaceResolver::new`] and [`Default`]).
///
/// # Example
/// ```ignore
/// let mut resolver = NamespaceResolver::with_default_ns(
///     "https://www.vdl.afrl.af.mil/programs/oam"
/// );
/// resolver.add_prefix("xs", "http://www.w3.org/2001/XMLSchema");
///
/// let q = QName::new(Some("https://www.vdl.afrl.af.mil/programs/oam"), "SystemStatusType");
/// assert_eq!(q.resolve(&resolver), "SystemStatusType"); // default ns → no prefix
/// ```
#[derive(Debug, Clone)]
pub struct NamespaceResolver {
    default_ns: Option<String>,
    prefix_to_uri: HashMap<String, String>,
    uri_to_prefix: HashMap<String, String>,
}

impl Default for NamespaceResolver {
    fn default() -> Self { Self::new() }
}

impl NamespaceResolver {
    /// Create a resolver pre-loaded with the `xs` namespace mapping.
    pub fn new() -> Self {
        let mut r = Self {
            default_ns: None,
            prefix_to_uri: HashMap::new(),
            uri_to_prefix: HashMap::new(),
        };
        r.add_prefix("xs", "http://www.w3.org/2001/XMLSchema");
        r
    }

    /// Create a resolver with the given default namespace (and the `xs` mapping).
    pub fn with_default_ns(ns: impl Into<String>) -> Self {
        let mut r = Self::new();
        r.set_default_ns(ns);
        r
    }

    /// Set the default namespace URI (types in this namespace display without a prefix).
    pub fn set_default_ns(&mut self, ns: impl Into<String>) {
        self.default_ns = Some(ns.into());
    }

    /// Add a prefix → URI mapping.
    pub fn add_prefix(&mut self, prefix: impl Into<String>, uri: impl Into<String>) {
        let prefix = prefix.into();
        let uri = uri.into();
        self.uri_to_prefix.insert(uri.clone(), prefix.clone());
        self.prefix_to_uri.insert(prefix, uri);
    }

    /// The default namespace URI, if set.
    pub fn default_ns(&self) -> Option<&str> { self.default_ns.as_deref() }

    /// Find the prefix associated with a namespace URI.
    pub fn prefix_for_uri(&self, uri: &str) -> Option<&str> {
        self.uri_to_prefix.get(uri).map(String::as_str)
    }

    /// Find the namespace URI associated with a prefix.
    pub fn uri_for_prefix(&self, prefix: &str) -> Option<&str> {
        self.prefix_to_uri.get(prefix).map(String::as_str)
    }

    /// Format `(namespace, local)` in the shortest resolvable form.
    ///
    /// - `None` or matches the default namespace → bare local name.
    /// - Known prefix → `prefix:local`.
    /// - Unknown → Clark notation `{uri}local`.
    pub fn format(&self, namespace: Option<&str>, local: &str) -> String {
        match namespace {
            None => local.to_string(),
            Some(ns) if self.default_ns.as_deref() == Some(ns) => local.to_string(),
            Some(ns) => match self.prefix_for_uri(ns) {
                Some(prefix) => format!("{prefix}:{local}"),
                None => format!("{{{ns}}}{local}"),
            },
        }
    }

    /// Resolve a prefixed name (`prefix:local`, bare `local`, or Clark `{uri}local`)
    /// to a [`QName`] with the display form already computed.
    pub fn resolve(&self, name: &str) -> QName {
        if let Some(rest) = name.strip_prefix('{')
            && let Some(end) = rest.find('}')
        {
            let ns = rest[..end].to_string();
            let local = rest[end + 1..].to_string();
            let display = self.format(Some(&ns), &local);
            return QName { namespace: Some(ns), local, display };
        }
        if let Some(colon) = name.find(':') {
            let prefix = &name[..colon];
            let local = &name[colon + 1..];
            let ns = self.uri_for_prefix(prefix).map(|s| s.to_string());
            let display = self.format(ns.as_deref(), local);
            QName { namespace: ns, local: local.to_string(), display }
        } else {
            let display = self.format(self.default_ns.as_deref(), name);
            QName { namespace: self.default_ns.clone(), local: name.to_string(), display }
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clark() {
        let q = QName::parse("{https://example.com}Foo");
        assert_eq!(q.namespace(), Some("https://example.com"));
        assert_eq!(q.local(), "Foo");
        assert_eq!(q.to_string(), "{https://example.com}Foo");
    }

    #[test]
    fn parse_prefixed() {
        let q = QName::parse("xs:string");
        assert_eq!(q.local(), "string");
        assert_eq!(q.namespace(), None); // URI unknown without resolver
        assert_eq!(q.to_string(), "xs:string");
    }

    #[test]
    fn parse_bare() {
        let q = QName::parse("Foo");
        assert_eq!(q.local(), "Foo");
        assert_eq!(q.namespace(), None);
        assert_eq!(q.to_string(), "Foo");
    }

    #[test]
    fn resolve_default_ns() {
        let mut r = NamespaceResolver::new();
        r.set_default_ns("https://example.com");
        let q = QName::new(Some("https://example.com"), "Foo");
        assert_eq!(q.resolve(&r), "Foo");
    }

    #[test]
    fn resolve_mapped_prefix() {
        let mut r = NamespaceResolver::new();
        r.add_prefix("ex", "https://example.com");
        let q = QName::new(Some("https://example.com"), "Foo");
        assert_eq!(q.resolve(&r), "ex:Foo");
    }

    #[test]
    fn resolve_clark_fallback() {
        let r = NamespaceResolver::new();
        let q = QName::new(Some("https://unmapped.example.com"), "Foo");
        assert_eq!(q.resolve(&r), "{https://unmapped.example.com}Foo");
    }

    #[test]
    fn resolver_resolve_prefixed_name() {
        let mut r = NamespaceResolver::new();
        r.set_default_ns("https://oam.example.com");
        r.add_prefix("uci", "https://oam.example.com");
        let q = r.resolve("uci:SystemStatusType");
        assert_eq!(q.namespace(), Some("https://oam.example.com"));
        assert_eq!(q.local(), "SystemStatusType");
        // Default ns → bare local
        assert_eq!(q.to_string(), "SystemStatusType");
    }

    #[test]
    fn into_string_returns_display() {
        let q = QName::with_display(Some("https://example.com"), "Foo", "Foo");
        let s: String = q.into();
        assert_eq!(s, "Foo");
    }

    #[test]
    fn partial_eq_str() {
        let q = QName::with_display(Some("https://example.com"), "Foo", "Foo");
        assert!(q == "Foo");
        assert!(q != "Bar");
    }
}
