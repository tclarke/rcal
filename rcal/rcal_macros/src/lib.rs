use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{
    Expr, FnArg, Ident, ItemFn, ItemStruct, LitBool, LitStr, Pat, Token, Type,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

// ── monitor ──────────────────────────────────────────────────────────────────

/// Injects a `__monitor_mutex: ::std::sync::Mutex<()>` field into a named-field struct.
///
/// Pair with `#[guarded]` on methods to implement the monitor pattern — a
/// single mutex that serialises all externally-callable methods.
///
/// The field is private; constructors must initialise it as
/// `__monitor_mutex: ::std::sync::Mutex::new(())`.
///
/// ```ignore
/// #[rcal_macros::monitor]
/// pub struct Foo {
///     x: i32,
/// }
///
/// impl Foo {
///     pub fn new() -> Self { Self { x: 0, __monitor_mutex: ::std::sync::Mutex::new(()) } }
///
///     #[rcal_macros::guarded]
///     pub fn set_x(&mut self, v: i32) { self.x = v; }
/// }
/// ```
#[proc_macro_attribute]
pub fn monitor(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(item as ItemStruct);

    let mutex_field: syn::Field = syn::parse_quote! {
        __monitor_mutex: ::std::sync::Mutex<()>
    };

    match &mut input.fields {
        syn::Fields::Named(fields) => {
            fields.named.push(mutex_field);
        }
        _ => {
            return syn::Error::new_spanned(
                &input.ident,
                "#[monitor] only supports structs with named fields",
            )
            .to_compile_error()
            .into();
        }
    }

    quote! { #input }.into()
}

// ── guarded ───────────────────────────────────────────────────────────────────

/// Prepends `let _monitor_guard = self.__monitor_mutex.lock().unwrap();` to a
/// method body, acquiring the monitor mutex for the duration of the call.
///
/// Apply only to `&self` or `&mut self` methods on a struct annotated with
/// `#[monitor]`. Do **not** apply to methods that are only called from other
/// guarded methods — doing so will deadlock (std::sync::Mutex is not reentrant).
#[proc_macro_attribute]
pub fn guarded(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut func = parse_macro_input!(item as ItemFn);

    let guard_stmt: syn::Stmt = syn::parse_quote! {
        let _monitor_guard = self.__monitor_mutex.lock().unwrap();
    };

    func.block.stmts.insert(0, guard_stmt);

    quote! { #func }.into()
}

// ── init_test_logger ──────────────────────────────────────────────────────────

/// Attribute macro for test functions. Injects a `logger` variable at the top
/// of the function body, built via `rcal::logging::build_test_logger()`.
///
/// ```ignore
/// #[init_test_logger]
/// fn my_test() {
///     slog::info!(logger, "hello");
/// }
/// ```
#[proc_macro_attribute]
pub fn init_test_logger(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut func = parse_macro_input!(item as ItemFn);

    let init = quote! {
        let logger = {
            use slog::{Drain, Logger, o};
            use slog_term::{FullFormat, PlainDecorator, TermDecorator};
            use slog_async::Async;
            if ::std::env::var("NO_COLOR").is_err() {
                let d = FullFormat::new(TermDecorator::new().stdout().build()).build().fuse();
                let d = Async::new(d).build().fuse();
                Logger::root(d, o!("test_context" => "unit_tests"))
            } else {
                let d = FullFormat::new(PlainDecorator::new(::std::io::stdout())).build().fuse();
                let d = Async::new(d).build().fuse();
                Logger::root(d, o!("test_context" => "unit_tests"))
            }
        };
    };

    let original_stmts = func.block.stmts.clone();
    let new_block: syn::Block = syn::parse_quote! {
        {
            #init
            #(#original_stmts)*
        }
    };
    *func.block = new_block;

    quote!(#func).into()
}

// ── rcal_main ─────────────────────────────────────────────────────────────────

/// Parsed arguments for `#[rcal_main(...)]`.
struct RcalMainArgs {
    tokio_main: bool,
    logger: String,
    config: Option<String>,
}

impl Default for RcalMainArgs {
    fn default() -> Self {
        Self {
            tokio_main: true,
            logger: "root_logger".to_string(),
            config: None,
        }
    }
}

impl Parse for RcalMainArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut args = RcalMainArgs::default();
        while !input.is_empty() {
            let ident: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match ident.to_string().as_str() {
                "tokio_main" => {
                    let val: LitBool = input.parse()?;
                    args.tokio_main = val.value;
                }
                "logger" => {
                    let val: LitStr = input.parse()?;
                    args.logger = val.value();
                }
                "config" => {
                    let val: LitStr = input.parse()?;
                    args.config = Some(val.value());
                }
                other => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("unknown rcal_main argument: `{other}`"),
                    ));
                }
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(args)
    }
}

/// Attribute macro for `async fn main()` (or another async entry point).
///
/// Optionally prepends `#[tokio::main]`, loads the CAL config, and injects a
/// logger variable built from that config.
///
/// Parameters (all optional):
/// - `tokio_main = true|false` — whether to prepend `#[tokio::main]` (default `true`)
/// - `logger = "name"` — variable name for the logger (default `"root_logger"`)
/// - `config = "path"` — config file path; default uses `get_asb_config_location(None)`
///
/// # Examples
///
/// ```ignore
/// #[rcal_macros::rcal_main]
/// async fn main() {
///     slog::info!(root_logger, "started");
/// }
///
/// #[rcal_macros::rcal_main(tokio_main = false, logger = "log")]
/// #[tokio::main(flavor = "multi_thread", worker_threads = 4)]
/// async fn main() {
///     slog::info!(log, "started with custom tokio");
/// }
/// ```
#[proc_macro_attribute]
pub fn rcal_main(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as RcalMainArgs);
    let mut func = parse_macro_input!(item as ItemFn);

    let logger_ident = Ident::new(&args.logger, Span::call_site());

    let config_expr = match &args.config {
        Some(path) => quote! { Some(#path.to_string()) },
        None => quote! { None },
    };

    let init = quote! {
        let rcal_config_path = rcal::asb::get_asb_config_location(#config_expr)
            .unwrap_or_else(|e| { eprintln!("rcal config error: {}", e); std::process::exit(1) });
        let rcal_config = rcal::calconfig::parse_config_from_file(&rcal_config_path)
            .unwrap_or_else(|e| { eprintln!("rcal config parse error: {}", e); std::process::exit(1) });
        let #logger_ident = rcal::logging::build_logger(&rcal_config.system.logging);
    };

    let original_stmts = func.block.stmts.clone();
    let new_block: syn::Block = syn::parse_quote! {
        {
            #init
            #(#original_stmts)*
        }
    };
    *func.block = new_block;

    let tokio_attr: Option<proc_macro2::TokenStream> = if args.tokio_main {
        Some(quote! { #[tokio::main] })
    } else {
        None
    };

    quote! {
        #tokio_attr
        #func
    }
    .into()
}

// ── rcal_trace ────────────────────────────────────────────────────────────────

struct RcalTraceArgs {
    logger: Option<Expr>,
}

impl Parse for RcalTraceArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut logger = None;
        while !input.is_empty() {
            let ident: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match ident.to_string().as_str() {
                "logger" => logger = Some(input.parse::<Expr>()?),
                other => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("unknown rcal_trace argument: `{other}`"),
                    ));
                }
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(Self { logger })
    }
}

fn is_logger_type(ty: &Type) -> bool {
    match ty {
        Type::Path(tp) => tp
            .path
            .segments
            .last()
            .map(|s| s.ident == "Logger")
            .unwrap_or(false),
        Type::Reference(tr) => is_logger_type(&tr.elem),
        _ => false,
    }
}

fn find_logger_param(inputs: &syn::punctuated::Punctuated<FnArg, syn::token::Comma>) -> Option<proc_macro2::TokenStream> {
    inputs.iter().find_map(|arg| {
        if let FnArg::Typed(pt) = arg
            && is_logger_type(&pt.ty)
            && let Pat::Ident(pi) = &*pt.pat
        {
            let id = &pi.ident;
            Some(quote! { #id })
        } else {
            None
        }
    })
}

/// Inserts `slog::trace!(logger, "StructName::fn_name()")` as the first
/// statement of each function or method.
///
/// Can be applied to an individual `fn` or to an entire `impl` block (in which
/// case every method in the block is instrumented automatically).  When applied
/// to an `impl` block the struct name is resolved statically at compile time;
/// when applied to a standalone method it is resolved at runtime via
/// `std::any::type_name::<Self>()`.
///
/// Logger resolution order (same for both usages):
/// 1. `logger=<expr>` attribute argument (explicit override)
/// 2. `self.logger` for methods with a `self` receiver
/// 3. First `Logger`-typed parameter for bare / static functions
///
/// Static methods in an `impl` block that have no `Logger` parameter and no
/// `logger=` override are silently skipped.
///
/// ```ignore
/// #[rcal_macros::rcal_trace]
/// impl Foo {
///     fn bar(&self) { /* trace!(self.logger, "Foo::bar()") injected */ }
///     fn baz(&self) { /* trace!(self.logger, "Foo::baz()") injected */ }
/// }
///
/// impl Bar {
///     #[rcal_macros::rcal_trace(logger = self.other_logger)]
///     fn qux(&self) { /* uses self.other_logger, "Bar::qux()" at runtime */ }
/// }
///
/// #[rcal_macros::rcal_trace]
/// fn standalone(logger: &slog::Logger) { /* trace!(logger, "standalone()") */ }
/// ```
#[proc_macro_attribute]
pub fn rcal_trace(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as RcalTraceArgs);

    if let Ok(mut impl_block) = syn::parse::<syn::ItemImpl>(item.clone()) {
        trace_impl(args, &mut impl_block)
    } else {
        let mut func = parse_macro_input!(item as ItemFn);
        trace_fn(args, &mut func)
    }
}

fn trace_impl(args: RcalTraceArgs, impl_block: &mut syn::ItemImpl) -> TokenStream {
    let struct_name = match &*impl_block.self_ty {
        Type::Path(tp) => tp.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    };
    let struct_name = match struct_name {
        Some(n) => n,
        None => {
            return syn::Error::new_spanned(
                &impl_block.self_ty,
                "#[rcal_trace]: cannot determine type name from impl type",
            )
            .to_compile_error()
            .into();
        }
    };

    let explicit = args.logger.clone();

    for item in &mut impl_block.items {
        if let syn::ImplItem::Fn(method) = item {
            let fn_name = method.sig.ident.to_string();
            let has_self = method.sig.inputs.iter().any(|a| matches!(a, FnArg::Receiver(_)));

            let logger_ts = if let Some(ref expr) = explicit {
                quote! { #expr }
            } else if has_self {
                quote! { self.logger }
            } else {
                match find_logger_param(&method.sig.inputs) {
                    Some(ts) => ts,
                    None => continue,
                }
            };

            let msg = format!("{}::{}()", struct_name, fn_name);
            let trace_stmt: syn::Stmt = syn::parse_quote! {
                slog::trace!(#logger_ts, #msg);
            };
            method.block.stmts.insert(0, trace_stmt);
        }
    }

    quote! { #impl_block }.into()
}

fn trace_fn(args: RcalTraceArgs, func: &mut ItemFn) -> TokenStream {
    let fn_name = func.sig.ident.to_string();
    let has_self = func.sig.inputs.iter().any(|a| matches!(a, FnArg::Receiver(_)));

    let logger_ts = if let Some(expr) = args.logger {
        quote! { #expr }
    } else if has_self {
        quote! { self.logger }
    } else {
        match find_logger_param(&func.sig.inputs) {
            Some(ts) => ts,
            None => {
                return syn::Error::new_spanned(
                    &func.sig.ident,
                    "#[rcal_trace]: no logger found — \
                     add a `self` receiver (uses `self.logger`), \
                     a `Logger`-typed parameter, \
                     or specify `logger=<expr>`",
                )
                .to_compile_error()
                .into();
            }
        }
    };

    let trace_stmt: syn::Stmt = if has_self {
        let msg_fmt = format!("{{}}::{}()", fn_name);
        syn::parse_quote! {
            slog::trace!(#logger_ts, #msg_fmt,
                ::std::any::type_name::<Self>().rsplit("::").next().unwrap_or("?"));
        }
    } else {
        let msg = format!("{}()", fn_name);
        syn::parse_quote! {
            slog::trace!(#logger_ts, #msg);
        }
    };

    func.block.stmts.insert(0, trace_stmt);
    quote! { #func }.into()
}
