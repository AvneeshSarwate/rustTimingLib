//! Timing Macros - Maximum terseness proc-macros for the timing library
//!
//! This crate provides proc-macros that transform a DSL-like syntax into
//! valid Rust async code for the timing library. It flagrantly breaks
//! Rust best practices in favor of notational brevity.
//!
//! ## Syntax Supported:
//!
//! - `~expr;` - Wait for `expr` beats
//! - `~s expr;` - Wait for `expr` seconds
//! - `fork { ... }` - Spawn a branch (fire-and-forget)
//! - `join { ... }` - Spawn a branch and wait for completion
//! - `repeat N { ... }` - Cancellable loop N times
//! - `loop { ... }` - Cancellable infinite loop
//! - `shared!(expr)` - Wrap in Shared<>
//! - Method calls on shared vars auto-use .modify()

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{quote, ToTokens};
use syn::visit_mut::VisitMut;
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input, parse_quote,
    punctuated::Punctuated,
    spanned::Spanned,
    token, Block, Expr, ExprBlock, ExprCall, ExprMethodCall, ExprPath, Ident, ItemFn, Lit,
    Pat, Result, Stmt, Token,
};

/// The main timing block macro.
///
/// Usage:
/// ```ignore
/// timing! {
///     config { bpm: 120, seed: "demo" }
///
///     fork {
///         ~1.0;
///         ~2.0;
///     }
///
///     ~4.0;
/// }
/// ```
#[proc_macro]
pub fn timing(input: TokenStream) -> TokenStream {
    let timing_block = parse_macro_input!(input as TimingBlock);
    timing_block.expand().into()
}

/// Agent attribute macro - transforms a function into a timing agent.
///
/// Usage:
/// ```ignore
/// #[agent(bpm = 120, seed = "demo")]
/// fn my_piece(midi: Midi) {
///     fork {
///         midi.on(60, 100);
///         ~1.0;
///         midi.off(60);
///     }
///     ~4.0;
/// }
/// ```
#[proc_macro_attribute]
pub fn agent(attr: TokenStream, item: TokenStream) -> TokenStream {
    let config = parse_macro_input!(attr as AgentConfig);
    let mut func = parse_macro_input!(item as ItemFn);

    // Transform the function body
    let mut transformer = BodyTransformer::new();
    transformer.visit_block_mut(&mut func.block);

    let bpm = config.bpm;
    let seed = &config.seed;
    let fn_name = &func.sig.ident;
    let fn_body = &func.block;
    let params = &func.sig.inputs;

    // Extract parameter names for cloning
    let param_names: Vec<_> = func.sig.inputs.iter().filter_map(|arg| {
        if let syn::FnArg::Typed(pat_type) = arg {
            if let Pat::Ident(pat_ident) = &*pat_type.pat {
                return Some(pat_ident.ident.clone());
            }
        }
        None
    }).collect();

    let clone_stmts: Vec<_> = param_names.iter().map(|name| {
        quote! { let #name = #name.clone(); }
    }).collect();

    let expanded = quote! {
        fn #fn_name(#params) {
            use rust_timing_lib::{Engine, EngineConfig, SchedulerMode, BranchOptions, Ctx};
            use std::sync::atomic::{AtomicBool, Ordering};
            use std::sync::Arc;

            let config = EngineConfig {
                bpm: #bpm,
                seed: #seed.to_string(),
                ..Default::default()
            };

            let mut engine = Engine::new(SchedulerMode::Realtime, config);

            #(#clone_stmts)*

            engine.spawn(move |ctx: Ctx| async move #fn_body);

            engine.run_until_complete();
        }
    };

    expanded.into()
}

// ============================================================================
// PARSING STRUCTURES
// ============================================================================

struct TimingBlock {
    config: Option<TimingConfig>,
    stmts: Vec<TimingStmt>,
}

struct TimingConfig {
    bpm: f64,
    seed: String,
}

enum TimingStmt {
    Wait(Expr),
    WaitSec(Expr),
    Fork(Block),
    Join(Block),
    Repeat(Expr, Block),
    InfiniteLoop(Block),
    Let(Ident, Expr, bool), // bool = is_shared
    Regular(Stmt),
}

struct AgentConfig {
    bpm: f64,
    seed: String,
}

impl Parse for AgentConfig {
    fn parse(input: ParseStream) -> Result<Self> {
        let mut bpm = 120.0;
        let mut seed = "default".to_string();

        while !input.is_empty() {
            let ident: Ident = input.parse()?;
            let _: Token![=] = input.parse()?;

            match ident.to_string().as_str() {
                "bpm" => {
                    let lit: Lit = input.parse()?;
                    if let Lit::Float(f) = lit {
                        bpm = f.base10_parse()?;
                    } else if let Lit::Int(i) = lit {
                        bpm = i.base10_parse::<u64>()? as f64;
                    }
                }
                "seed" => {
                    let lit: Lit = input.parse()?;
                    if let Lit::Str(s) = lit {
                        seed = s.value();
                    }
                }
                _ => {}
            }

            if input.peek(Token![,]) {
                let _: Token![,] = input.parse()?;
            }
        }

        Ok(AgentConfig { bpm, seed })
    }
}

impl Parse for TimingBlock {
    fn parse(input: ParseStream) -> Result<Self> {
        let mut config = None;
        let mut stmts = Vec::new();

        while !input.is_empty() {
            // Check for config block
            if input.peek(Ident) {
                let ident: Ident = input.fork().parse()?;
                if ident == "config" {
                    let _: Ident = input.parse()?;
                    let content;
                    syn::braced!(content in input);
                    config = Some(parse_config(&content)?);
                    continue;
                }
            }

            stmts.push(input.parse()?);
        }

        Ok(TimingBlock { config, stmts })
    }
}

fn parse_config(input: ParseStream) -> Result<TimingConfig> {
    let mut bpm = 120.0;
    let mut seed = "default".to_string();

    while !input.is_empty() {
        let ident: Ident = input.parse()?;
        let _: Token![:] = input.parse()?;

        match ident.to_string().as_str() {
            "bpm" => {
                let lit: Lit = input.parse()?;
                if let Lit::Float(f) = lit {
                    bpm = f.base10_parse()?;
                } else if let Lit::Int(i) = lit {
                    bpm = i.base10_parse::<u64>()? as f64;
                }
            }
            "seed" => {
                let lit: Lit = input.parse()?;
                if let Lit::Str(s) = lit {
                    seed = s.value();
                }
            }
            _ => {}
        }

        if input.peek(Token![,]) {
            let _: Token![,] = input.parse()?;
        }
    }

    Ok(TimingConfig { bpm, seed })
}

impl Parse for TimingStmt {
    fn parse(input: ParseStream) -> Result<Self> {
        // Check for ~ (wait)
        if input.peek(Token![~]) {
            let _: Token![~] = input.parse()?;

            // Check for ~s (wait seconds)
            if input.peek(Ident) {
                let ident: Ident = input.fork().parse()?;
                if ident == "s" {
                    let _: Ident = input.parse()?;
                    let expr: Expr = input.parse()?;
                    let _: Token![;] = input.parse()?;
                    return Ok(TimingStmt::WaitSec(expr));
                }
            }

            let expr: Expr = input.parse()?;
            let _: Token![;] = input.parse()?;
            return Ok(TimingStmt::Wait(expr));
        }

        // Check for fork
        if input.peek(Ident) {
            let ident: Ident = input.fork().parse()?;

            if ident == "fork" {
                let _: Ident = input.parse()?;
                let block: Block = input.parse()?;
                return Ok(TimingStmt::Fork(block));
            }

            if ident == "join" {
                let _: Ident = input.parse()?;
                let block: Block = input.parse()?;
                return Ok(TimingStmt::Join(block));
            }

            if ident == "repeat" {
                let _: Ident = input.parse()?;
                let count: Expr = input.parse()?;
                let block: Block = input.parse()?;
                return Ok(TimingStmt::Repeat(count, block));
            }

            if ident == "loop" {
                let _: Ident = input.parse()?;
                let block: Block = input.parse()?;
                return Ok(TimingStmt::InfiniteLoop(block));
            }

            // Check for let with shared!
            if ident == "let" {
                let _: Token![let] = input.parse()?;
                let name: Ident = input.parse()?;
                let _: Token![=] = input.parse()?;

                // Check for shared!()
                if input.peek(Ident) {
                    let maybe_shared: Ident = input.fork().parse()?;
                    if maybe_shared == "shared" {
                        let _: Ident = input.parse()?;
                        let _: Token![!] = input.parse()?;
                        let content;
                        syn::parenthesized!(content in input);
                        let inner_expr: Expr = content.parse()?;
                        let _: Token![;] = input.parse()?;
                        return Ok(TimingStmt::Let(name, inner_expr, true));
                    }
                }

                let expr: Expr = input.parse()?;
                let _: Token![;] = input.parse()?;
                return Ok(TimingStmt::Let(name, expr, false));
            }
        }

        // Regular statement
        let stmt: Stmt = input.parse()?;
        Ok(TimingStmt::Regular(stmt))
    }
}

impl TimingBlock {
    fn expand(&self) -> TokenStream2 {
        let config = self.config.as_ref().map(|c| {
            let bpm = c.bpm;
            let seed = &c.seed;
            quote! {
                let __config = rust_timing_lib::EngineConfig {
                    bpm: #bpm,
                    seed: #seed.to_string(),
                    ..Default::default()
                };
            }
        }).unwrap_or_else(|| quote! {
            let __config = rust_timing_lib::EngineConfig::default();
        });

        let stmts: Vec<_> = self.stmts.iter().map(|s| s.expand()).collect();

        quote! {
            {
                use rust_timing_lib::{Engine, EngineConfig, SchedulerMode, BranchOptions, Ctx};

                #config

                let mut __engine = Engine::new(SchedulerMode::Realtime, __config);

                __engine.spawn(|__ctx: Ctx| async move {
                    #(#stmts)*
                });

                __engine.run_until_complete();
            }
        }
    }
}

impl TimingStmt {
    fn expand(&self) -> TokenStream2 {
        match self {
            TimingStmt::Wait(expr) => {
                quote! { let _ = __ctx.wait(#expr).await; }
            }
            TimingStmt::WaitSec(expr) => {
                quote! { let _ = __ctx.wait_sec(#expr).await; }
            }
            TimingStmt::Fork(block) => {
                let inner = transform_block_stmts(block);
                quote! {
                    __ctx.branch(|__ctx| async move { #inner }, BranchOptions::default());
                }
            }
            TimingStmt::Join(block) => {
                let inner = transform_block_stmts(block);
                quote! {
                    let _ = __ctx.branch_wait(|__ctx| async move { #inner }, BranchOptions::default()).await;
                }
            }
            TimingStmt::Repeat(count, block) => {
                let inner = transform_block_stmts(block);
                quote! {
                    for _ in 0..(#count) {
                        if __ctx.is_canceled() { break; }
                        #inner
                    }
                }
            }
            TimingStmt::InfiniteLoop(block) => {
                let inner = transform_block_stmts(block);
                quote! {
                    loop {
                        if __ctx.is_canceled() { break; }
                        #inner
                    }
                }
            }
            TimingStmt::Let(name, expr, is_shared) => {
                if *is_shared {
                    quote! {
                        let #name = Shared::new(#expr);
                    }
                } else {
                    quote! {
                        let #name = #expr;
                    }
                }
            }
            TimingStmt::Regular(stmt) => {
                quote! { #stmt }
            }
        }
    }
}

fn transform_block_stmts(block: &Block) -> TokenStream2 {
    // Re-parse the block contents as TimingStmts
    let stmts = &block.stmts;
    let transformed: Vec<TokenStream2> = stmts.iter().map(|stmt| {
        // Try to detect our special syntax within regular stmts
        transform_stmt(stmt)
    }).collect();

    quote! { #(#transformed)* }
}

fn transform_stmt(stmt: &Stmt) -> TokenStream2 {
    match stmt {
        Stmt::Expr(expr, semi) => {
            let transformed = transform_expr(expr);
            if semi.is_some() {
                quote! { #transformed; }
            } else {
                quote! { #transformed }
            }
        }
        Stmt::Local(local) => {
            quote! { #local }
        }
        _ => quote! { #stmt }
    }
}

fn transform_expr(expr: &Expr) -> TokenStream2 {
    match expr {
        // Transform ~expr to wait
        Expr::Unary(unary) if matches!(unary.op, syn::UnOp::Not(_)) => {
            // This is ! not ~, but we can check for our pattern
            quote! { #expr }
        }

        // Transform method calls on potential Shared types
        Expr::MethodCall(call) => {
            let receiver = transform_expr(&call.receiver);
            let method = &call.method;
            let args = &call.args;

            // Check if this looks like a shared type method (heuristic)
            // For now, transform all method calls to use modify pattern
            // This is aggressive but that's the point
            quote! {
                #receiver.modify(|__inner| __inner.#method(#args))
            }
        }

        // Transform blocks recursively
        Expr::Block(block) => {
            let inner = transform_block_stmts(&block.block);
            quote! { { #inner } }
        }

        // Check for fork/join/repeat as expressions
        Expr::Call(call) => {
            if let Expr::Path(path) = &*call.func {
                if let Some(ident) = path.path.get_ident() {
                    if ident == "fork" || ident == "join" || ident == "repeat" {
                        // These should be statements, not expressions
                        // Pass through for now
                    }
                }
            }
            quote! { #expr }
        }

        _ => quote! { #expr }
    }
}

// ============================================================================
// BODY TRANSFORMER - For #[agent] attribute
// ============================================================================

struct BodyTransformer {
    // Track shared variables
    shared_vars: Vec<Ident>,
}

impl BodyTransformer {
    fn new() -> Self {
        BodyTransformer { shared_vars: Vec::new() }
    }
}

impl VisitMut for BodyTransformer {
    fn visit_stmt_mut(&mut self, stmt: &mut Stmt) {
        // First, handle special syntax patterns
        match stmt {
            Stmt::Expr(expr, semi) => {
                // Check for ~ pattern (using unary minus as a hack since ~ isn't valid)
                if let Expr::Unary(unary) = expr {
                    if matches!(unary.op, syn::UnOp::Neg(_)) {
                        // Transform -expr to wait
                        let inner = &unary.expr;
                        *stmt = parse_quote! {
                            let _ = __ctx.wait(#inner).await;
                        };
                        return;
                    }
                }

                // Check for fork { } pattern
                if let Expr::Block(block_expr) = expr {
                    if block_expr.label.is_some() {
                        if let Some(label) = &block_expr.label {
                            let label_name = label.name.ident.to_string();
                            if label_name == "fork" {
                                let inner = &block_expr.block;
                                *stmt = parse_quote! {
                                    __ctx.branch(|__ctx| async move #inner, BranchOptions::default());
                                };
                                return;
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        // Continue visiting children
        syn::visit_mut::visit_stmt_mut(self, stmt);
    }

    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        // Transform method calls on shared vars
        if let Expr::MethodCall(call) = expr {
            if let Expr::Path(path) = &*call.receiver {
                if let Some(ident) = path.path.get_ident() {
                    if self.shared_vars.contains(ident) {
                        let method = &call.method;
                        let args = &call.args;
                        *expr = parse_quote! {
                            #ident.modify(|__m| __m.#method(#args))
                        };
                        return;
                    }
                }
            }
        }

        syn::visit_mut::visit_expr_mut(self, expr);
    }
}
