use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{
    Ident, ItemFn, LitBool, LitStr, Token,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

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
