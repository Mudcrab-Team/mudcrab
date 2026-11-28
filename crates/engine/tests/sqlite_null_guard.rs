//! Guard: a SQL NULL column must stay absent in the engine's SQLite readers (#1095).
//!
//! The VMAD property reader once read every value column as `Option<_>` and then replaced
//! NULL with `0`, `""` or `false` (`object.unwrap_or(0)`, `string.unwrap_or_default()`). A
//! corrupt row then looked like a real zero value. This test parses the production code of
//! `crates/engine/src` and reports that shape: a fallback to a default value on a value that
//! comes from a SQLite row.
//!
//! A value comes from a row when it is
//! - a `get` on `row` (or `*_row`), or any `get::<_, Option<..>>`;
//! - a binding made from such a value, also through a tuple pattern or `?`;
//! - a field of a struct that is filled from row reads and declares that field `Option`.
//!
//! It stays that value through methods that keep an absent value absent (`map`, `and_then`,
//! `ok`, ...). A call inside a closure or an argument list is a different value. A binding
//! ends where its scope ends or where a new binding takes its name.
//!
//! A fallback is `unwrap_or_default`, or `unwrap_or`, `map_or`, `unwrap_or_else` or
//! `map_or_else` whose default is a literal number, `bool` or `""`, or a `default()` or empty
//! constructor. Items under `#[cfg(test)]` are skipped.
//!
//! A site is allowed when one of its lines, or the line above, carries `// null-ok: <reason>`.
//! The reason must say why the default is retail data, not an invention. Sites that exist
//! today and are not fixed yet are listed in [`KNOWN`] by function and source text. A new site
//! fails the test, and so does a listed site that is gone.

use proc_macro2::Span;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Block, Expr, ExprMethodCall, Fields, File, FnArg, GenericArgument, ImplItem, Item,
    Lit, Member, Meta, Pat, Token, Type,
};

/// `(file, function, source text)` of each known unlabelled site. None is in the VMAD reader.
/// Do not add an entry for new code: keep NULL absent, or label the site.
const KNOWN: &[(&str, &str, &str)] = &[
    // `world-inspect` prints an interior cell's missing grid as `[0, 0]`; a report that shows
    // `null` instead is the fix.
    (
        "crates/engine/src/bin/world-inspect.rs",
        "find_cell",
        "row.get::<_, Option<i32>>(2)?.unwrap_or(0),",
    ),
    (
        "crates/engine/src/bin/world-inspect.rs",
        "find_cell",
        "row.get::<_, Option<i32>>(3)?.unwrap_or(0),",
    ),
    // The flags are read only with an enable parent, and `map_enable_inputs` rejects a parent
    // with no flags first.
    (
        "crates/engine/src/world/database.rs",
        "map_enable_inputs",
        "parent_flags: parent_flags.unwrap_or(0),",
    ),
    // A missing or invalid base fade becomes 1.0, a Mudcrab compatibility fallback.
    (
        "crates/engine/src/lights.rs",
        "evaluate_light",
        "light.fade.filter(|fade| fade.is_finite() && *fade >= 0.0).unwrap_or(1.0)",
    ),
    // RF-LIGHT-CPU-014 (as the source comments cite it): a missing interior lighting field
    // applies as 0. The missing fog maximum of 1.0 is marked UNVERIFIED there.
    (
        "crates/engine/src/world/environment/interior.rs",
        "from_lighting",
        "let near = lighting.fog_near.unwrap_or(0.0);",
    ),
    (
        "crates/engine/src/world/environment/interior.rs",
        "from_lighting",
        "let far = lighting.fog_far.unwrap_or(0.0);",
    ),
    (
        "crates/engine/src/world/environment/interior.rs",
        "from_lighting",
        "let max = lighting.fog_max.unwrap_or(1.0);",
    ),
    (
        "crates/engine/src/world/environment/interior.rs",
        "from_lighting",
        "power: lighting.fog_power.unwrap_or(0.0),",
    ),
    (
        "crates/engine/src/world/environment/interior.rs",
        "new",
        "lighting.directional_rotation_xy.unwrap_or(0),",
    ),
    (
        "crates/engine/src/world/environment/interior.rs",
        "new",
        "lighting.directional_rotation_z.unwrap_or(0),",
    ),
];

/// Methods that keep an absent value absent, so a later fallback still replaces a NULL.
const KEEPS_ABSENCE: &[&str] = &[
    "and_then",
    "as_deref",
    "as_deref_mut",
    "as_mut",
    "as_ref",
    "clone",
    "cloned",
    "copied",
    "filter",
    "flatten",
    "inspect",
    "map",
    "ok",
    "or",
    "or_else",
    "take",
    "transpose",
    "xor",
];

/// One coercion: where it is and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Site {
    function: String,
    line: usize,
    text: String,
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "tests" {
                rust_files(&path, out);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" && !name.ends_with("_tests.rs") {
            out.push(path);
        }
    }
}

// ---- Test-only items ---------------------------------------------------------------------

/// Whether a `cfg` predicate can only be true in a test build.
fn needs_test(meta: &Meta) -> bool {
    match meta {
        Meta::Path(path) => path.is_ident("test"),
        Meta::List(list) => {
            let Ok(items) = list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
            else {
                return false;
            };
            if list.path.is_ident("all") {
                items.iter().any(needs_test)
            } else {
                list.path.is_ident("any") && !items.is_empty() && items.iter().all(needs_test)
            }
        }
        Meta::NameValue(_) => false,
    }
}

fn is_test_only(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("test")
            || (attribute.path().is_ident("cfg")
                && attribute
                    .parse_args::<Meta>()
                    .is_ok_and(|meta| needs_test(&meta)))
    })
}

fn item_attributes(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        _ => &[],
    }
}

// ---- Expression shapes -------------------------------------------------------------------

fn last_segment(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string()),
        Type::Reference(reference) => last_segment(&reference.elem),
        Type::Paren(paren) => last_segment(&paren.elem),
        _ => None,
    }
}

fn is_option(ty: &Type) -> bool {
    last_segment(ty).is_some_and(|name| name == "Option")
}

/// The name of a plain one-segment path expression, such as a local binding.
fn local_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Path(path) => path.path.get_ident().map(ToString::to_string),
        Expr::Reference(reference) => local_name(&reference.expr),
        Expr::Paren(paren) => local_name(&paren.expr),
        _ => None,
    }
}

/// Whether this call reads a column of a SQLite row.
fn is_row_get(call: &ExprMethodCall) -> bool {
    if call.method != "get" {
        return false;
    }
    let on_row =
        local_name(&call.receiver).is_some_and(|name| name == "row" || name.ends_with("_row"));
    // rusqlite's `get` takes two type arguments, the index and the value. A one-argument
    // `get::<Option<_>>` is another API (`mlua::Table::get`).
    let optional_turbofish = call.turbofish.as_ref().is_some_and(|generics| {
        generics.args.len() == 2
            && matches!(generics.args.last(), Some(GenericArgument::Type(ty)) if is_option(ty))
    });
    on_row || optional_turbofish
}

/// Whether the expression is a row read, directly or through methods that keep absence.
fn reads_a_row(expr: &Expr) -> bool {
    match expr {
        Expr::MethodCall(call) => {
            is_row_get(call)
                || (KEEPS_ABSENCE.iter().any(|name| call.method == name)
                    && reads_a_row(&call.receiver))
        }
        Expr::Try(inner) => reads_a_row(&inner.expr),
        Expr::Paren(inner) => reads_a_row(&inner.expr),
        Expr::Group(inner) => reads_a_row(&inner.expr),
        Expr::Reference(inner) => reads_a_row(&inner.expr),
        _ => false,
    }
}

/// Whether an expression builds a default value out of nothing.
fn is_default_value(expr: &Expr) -> bool {
    match expr {
        Expr::Lit(literal) => match &literal.lit {
            Lit::Int(_) | Lit::Float(_) | Lit::Bool(_) => true,
            Lit::Str(text) => text.value().is_empty(),
            _ => false,
        },
        Expr::Unary(unary) => is_default_value(&unary.expr),
        Expr::Paren(inner) => is_default_value(&inner.expr),
        Expr::Group(inner) => is_default_value(&inner.expr),
        Expr::Block(block) => match block.block.stmts.as_slice() {
            [syn::Stmt::Expr(inner, None)] => is_default_value(inner),
            _ => false,
        },
        Expr::MethodCall(call) => {
            ["into", "to_owned", "to_string"]
                .iter()
                .any(|name| call.method == name)
                && is_default_value(&call.receiver)
        }
        Expr::Call(call) => match call.args.len() {
            0 => is_default_constructor(&call.func),
            1 => {
                is_path_ending(&call.func, &["String", "from"])
                    && call.args.first().is_some_and(is_default_value)
            }
            _ => false,
        },
        _ => false,
    }
}

fn is_path_ending(expr: &Expr, tail: &[&str]) -> bool {
    let Expr::Path(path) = expr else {
        return false;
    };
    let names: Vec<String> = path
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    names.len() >= tail.len() && names[names.len() - tail.len()..] == *tail
}

/// `T::default`, `String::new` or `Vec::new`, as a function path.
fn is_default_constructor(function: &Expr) -> bool {
    is_path_ending(function, &["default"])
        || is_path_ending(function, &["String", "new"])
        || is_path_ending(function, &["Vec", "new"])
}

/// Whether this call replaces an absent value with a default value.
fn replaces_absence(call: &ExprMethodCall) -> bool {
    let method = call.method.to_string();
    let first = call.args.first();
    match method.as_str() {
        "unwrap_or_default" => true,
        "unwrap_or" | "map_or" => first.is_some_and(is_default_value),
        "unwrap_or_else" | "map_or_else" => first.is_some_and(|argument| match argument {
            Expr::Closure(closure) => is_default_value(&closure.body),
            path => is_default_constructor(path),
        }),
        _ => false,
    }
}

// ---- Row structs -------------------------------------------------------------------------

/// Finds structs with `Option` fields, and the fields a struct literal fills from a row.
#[derive(Default)]
struct RowStructs {
    optional: HashSet<(String, String)>,
    filled: HashSet<(String, String)>,
}

impl RowStructs {
    /// `(struct, field)` pairs that are NULL exactly when their column is.
    fn nullable(&self) -> HashSet<(String, String)> {
        self.optional.intersection(&self.filled).cloned().collect()
    }
}

impl<'ast> Visit<'ast> for RowStructs {
    fn visit_item(&mut self, item: &'ast Item) {
        if !is_test_only(item_attributes(item)) {
            visit::visit_item(self, item);
        }
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        if let Fields::Named(fields) = &item.fields {
            for field in &fields.named {
                if is_option(&field.ty)
                    && let Some(name) = &field.ident
                {
                    self.optional
                        .insert((item.ident.to_string(), name.to_string()));
                }
            }
        }
    }

    fn visit_expr_struct(&mut self, expr: &'ast syn::ExprStruct) {
        if let Some(segment) = expr.path.segments.last() {
            for field in &expr.fields {
                if let Member::Named(name) = &field.member
                    && reads_a_row(&field.expr)
                {
                    self.filled
                        .insert((segment.ident.to_string(), name.to_string()));
                }
            }
        }
        visit::visit_expr_struct(self, expr);
    }
}

// ---- The scan ----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Binding {
    Plain,
    /// Absent exactly when a column is NULL.
    Row,
    /// A value of a struct that has such fields.
    Struct(String),
}

struct Scanner<'a> {
    lines: Vec<&'a str>,
    fields: &'a HashSet<(String, String)>,
    scopes: Vec<HashMap<String, Binding>>,
    functions: Vec<String>,
    self_type: Option<String>,
    found: Vec<Site>,
}

impl Scanner<'_> {
    fn is_row_struct(&self, name: &str) -> bool {
        self.fields.iter().any(|(owner, _)| owner == name)
    }

    fn lookup(&self, name: &str) -> Binding {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name))
            .cloned()
            .unwrap_or(Binding::Plain)
    }

    fn declare(&mut self, name: String, binding: Binding) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, binding);
        }
    }

    /// Whether the expression is absent exactly when a SQL column is NULL.
    fn nullable(&self, expr: &Expr) -> bool {
        match expr {
            Expr::MethodCall(call) => {
                is_row_get(call)
                    || (KEEPS_ABSENCE.iter().any(|name| call.method == name)
                        && self.nullable(&call.receiver))
            }
            Expr::Try(inner) => self.nullable(&inner.expr),
            Expr::Paren(inner) => self.nullable(&inner.expr),
            Expr::Group(inner) => self.nullable(&inner.expr),
            Expr::Reference(inner) => self.nullable(&inner.expr),
            Expr::Path(_) => {
                local_name(expr).is_some_and(|name| self.lookup(&name) == Binding::Row)
            }
            Expr::Field(field) => {
                let (Some(base), Member::Named(member)) = (local_name(&field.base), &field.member)
                else {
                    return false;
                };
                matches!(self.lookup(&base), Binding::Struct(owner)
                    if self.fields.contains(&(owner.clone(), member.to_string())))
            }
            _ => false,
        }
    }

    fn classify(&self, init: Option<&Expr>, ty: Option<&Type>) -> Binding {
        if let Some(name) = ty.and_then(last_segment)
            && self.is_row_struct(&name)
        {
            return Binding::Struct(name);
        }
        if let Some(Expr::Struct(literal)) = init
            && let Some(segment) = literal.path.segments.last()
            && self.is_row_struct(&segment.ident.to_string())
        {
            return Binding::Struct(segment.ident.to_string());
        }
        // A declared type that is neither `Option` nor a `Result` cannot be absent.
        if let Some(name) = ty.and_then(last_segment)
            && name != "Option"
            && name != "Result"
        {
            return Binding::Plain;
        }
        if init.is_some_and(|init| self.nullable(init)) {
            Binding::Row
        } else {
            Binding::Plain
        }
    }

    /// Declare every name `pattern` binds, in the innermost scope.
    fn bind(&mut self, pattern: &Pat, init: Option<&Expr>, ty: Option<&Type>) {
        match pattern {
            Pat::Type(typed) => self.bind(&typed.pat, init, Some(&typed.ty)),
            Pat::Paren(inner) => self.bind(&inner.pat, init, ty),
            Pat::Reference(inner) => self.bind(&inner.pat, init, ty),
            Pat::Ident(ident) => {
                let binding = self.classify(init, ty);
                self.declare(ident.ident.to_string(), binding);
                if let Some((_, rest)) = &ident.subpat {
                    self.bind(rest, None, None);
                }
            }
            Pat::Tuple(tuple) => {
                for (position, element) in tuple.elems.iter().enumerate() {
                    let init = match init {
                        Some(Expr::Tuple(values)) => values.elems.iter().nth(position),
                        _ => None,
                    };
                    let ty = match ty {
                        Some(Type::Tuple(types)) => types.elems.iter().nth(position),
                        _ => None,
                    };
                    self.bind(element, init, ty);
                }
            }
            Pat::Struct(destructured) => {
                let owner = destructured
                    .path
                    .segments
                    .last()
                    .map(|segment| segment.ident.to_string())
                    .unwrap_or_default();
                for field in &destructured.fields {
                    let nullable = matches!(&field.member, Member::Named(member)
                        if self.fields.contains(&(owner.clone(), member.to_string())));
                    match &*field.pat {
                        Pat::Ident(ident) if nullable => {
                            self.declare(ident.ident.to_string(), Binding::Row);
                        }
                        other => self.bind(other, None, None),
                    }
                }
            }
            Pat::TupleStruct(inner) => {
                for element in &inner.elems {
                    self.bind(element, None, None);
                }
            }
            Pat::Slice(inner) => {
                for element in &inner.elems {
                    self.bind(element, None, None);
                }
            }
            Pat::Or(inner) => {
                for case in &inner.cases {
                    self.bind(case, None, None);
                }
            }
            _ => {}
        }
    }

    /// Visit a function body with its own scopes: it cannot see the caller's locals.
    fn function(&mut self, signature: &syn::Signature, body: &Block) {
        let outer = std::mem::take(&mut self.scopes);
        self.scopes.push(HashMap::new());
        self.functions.push(signature.ident.to_string());
        for input in &signature.inputs {
            match input {
                FnArg::Receiver(_) => {
                    if let Some(owner) = self.self_type.clone()
                        && self.is_row_struct(&owner)
                    {
                        self.declare("self".to_string(), Binding::Struct(owner));
                    }
                }
                FnArg::Typed(typed) => self.bind(&typed.pat, None, Some(&typed.ty)),
            }
        }
        self.visit_nested_block(body);
        self.functions.pop();
        self.scopes = outer;
    }

    fn visit_nested_block(&mut self, block: &Block) {
        self.scopes.push(HashMap::new());
        for statement in &block.stmts {
            <Self as Visit<'_>>::visit_stmt(self, statement);
        }
        self.scopes.pop();
    }

    fn record(&mut self, start: Span, end: Span) {
        let first = start.start().line;
        let last = end.start().line.max(first);
        let exempt = (first.saturating_sub(1)..=last).any(|line| {
            line.checked_sub(1)
                .and_then(|index| self.lines.get(index))
                .and_then(|text| text.split_once("// null-ok:"))
                .is_some_and(|(_, reason)| !reason.trim().is_empty())
        });
        if exempt {
            return;
        }
        let text = self.lines[first - 1..last]
            .iter()
            .map(|line| line.trim())
            .collect::<String>();
        self.found.push(Site {
            function: self.functions.last().cloned().unwrap_or_default(),
            line: first,
            text,
        });
    }
}

impl<'ast> Visit<'ast> for Scanner<'_> {
    fn visit_item(&mut self, item: &'ast Item) {
        if !is_test_only(item_attributes(item)) {
            visit::visit_item(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        self.function(&item.sig, &item.block);
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let outer = std::mem::replace(&mut self.self_type, last_segment(&item.self_ty));
        visit::visit_item_impl(self, item);
        self.self_type = outer;
    }

    fn visit_impl_item(&mut self, item: &'ast ImplItem) {
        let attributes: &[Attribute] = match item {
            ImplItem::Const(item) => &item.attrs,
            ImplItem::Fn(item) => &item.attrs,
            ImplItem::Macro(item) => &item.attrs,
            _ => &[],
        };
        if !is_test_only(attributes) {
            visit::visit_impl_item(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        self.function(&item.sig, &item.block);
    }

    fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
        if let Some(body) = &item.default
            && !is_test_only(&item.attrs)
        {
            self.function(&item.sig, body);
        }
    }

    fn visit_block(&mut self, block: &'ast Block) {
        self.visit_nested_block(block);
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        if is_test_only(&local.attrs) {
            return;
        }
        let init = local.init.as_ref();
        if let Some(init) = init {
            self.visit_expr(&init.expr);
            if let Some((_, diverge)) = &init.diverge {
                self.visit_expr(diverge);
            }
        }
        self.bind(&local.pat, init.map(|init| &*init.expr), None);
    }

    fn visit_expr_closure(&mut self, closure: &'ast syn::ExprClosure) {
        self.scopes.push(HashMap::new());
        for input in &closure.inputs {
            self.bind(input, None, None);
        }
        self.visit_expr(&closure.body);
        self.scopes.pop();
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        self.scopes.push(HashMap::new());
        self.bind(&arm.pat, None, None);
        visit::visit_arm(self, arm);
        self.scopes.pop();
    }

    fn visit_expr_let(&mut self, expr: &'ast syn::ExprLet) {
        self.visit_expr(&expr.expr);
        self.bind(&expr.pat, None, None);
    }

    fn visit_expr_if(&mut self, expr: &'ast syn::ExprIf) {
        self.scopes.push(HashMap::new());
        self.visit_expr(&expr.cond);
        self.visit_block(&expr.then_branch);
        self.scopes.pop();
        if let Some((_, otherwise)) = &expr.else_branch {
            self.visit_expr(otherwise);
        }
    }

    fn visit_expr_while(&mut self, expr: &'ast syn::ExprWhile) {
        self.scopes.push(HashMap::new());
        self.visit_expr(&expr.cond);
        self.visit_block(&expr.body);
        self.scopes.pop();
    }

    fn visit_expr_for_loop(&mut self, expr: &'ast syn::ExprForLoop) {
        self.visit_expr(&expr.expr);
        self.scopes.push(HashMap::new());
        self.bind(&expr.pat, None, None);
        self.visit_block(&expr.body);
        self.scopes.pop();
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        if replaces_absence(call) && self.nullable(&call.receiver) {
            self.record(call.receiver.span(), call.paren_token.span.close());
        }
        visit::visit_expr_method_call(self, call);
    }

    /// Macro arguments that parse as expressions (`format!`, `info!`, ...) are code too.
    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        if let Ok(arguments) =
            invocation.parse_body_with(Punctuated::<Expr, Token![,]>::parse_terminated)
        {
            for argument in &arguments {
                <Self as Visit<'_>>::visit_expr(self, argument);
            }
        }
    }
}

/// Every unlabelled coercion in one parsed file.
fn scan(source: &str, file: &File, fields: &HashSet<(String, String)>) -> Vec<Site> {
    if is_test_only(&file.attrs) {
        return Vec::new();
    }
    let mut scanner = Scanner {
        lines: source.lines().collect(),
        fields,
        scopes: vec![HashMap::new()],
        functions: Vec::new(),
        self_type: None,
        found: Vec::new(),
    };
    scanner.visit_file(file);
    scanner.found
}

fn parse(name: &str, source: &str) -> File {
    syn::parse_file(source).unwrap_or_else(|error| panic!("{name} does not parse: {error}"))
}

/// Lines of the coercions in a single source text.
fn coercions(source: &str) -> Vec<usize> {
    let file = parse("snippet", source);
    let mut structs = RowStructs::default();
    structs.visit_file(&file);
    scan(source, &file, &structs.nullable())
        .into_iter()
        .map(|site| site.line)
        .collect()
}

/// The sites in `found` that `known` does not list, and the `known` entries with no site.
/// Each entry covers one site.
fn compare(found: &[(String, Site)], known: &[(&str, &str, &str)]) -> (Vec<String>, Vec<String>) {
    let mut left: Vec<_> = known.to_vec();
    let mut unknown = Vec::new();
    for (file, site) in found {
        let entry = left.iter().position(|(name, function, text)| {
            name == file && *function == site.function && *text == site.text
        });
        match entry {
            Some(position) => {
                left.remove(position);
            }
            None => unknown.push(format!(
                "{file}:{} in `{}`: {}",
                site.line, site.function, site.text
            )),
        }
    }
    let retired = left
        .into_iter()
        .map(|(file, function, text)| format!("{file} in `{function}`: {text}"))
        .collect();
    (unknown, retired)
}

// ---- Self tests --------------------------------------------------------------------------

#[test]
fn the_scan_finds_the_shapes_that_caused_1095() {
    // The reader as it was written before the fix, reduced to its coercions.
    let old = r#"
fn read(row: &Row) {
    let object: Option<u32> = row.get(0)?;
    let string: Option<String> = row.get(2)?;
    let boolean: Option<i64> = row.get(5)?;
    Ok(match kind {
        VMAD_OBJECT => form_id(object.unwrap_or(0)),
        VMAD_STRING => PropertyValue::Str(string.unwrap_or_default()),
        VMAD_BOOL => PropertyValue::Bool(boolean.unwrap_or(0) != 0),
    })
}
"#;
    assert_eq!(coercions(old), vec![7, 8, 9]);
    let direct = "fn f() { let x = row.get::<_, Option<i32>>(3)?.unwrap_or(0); }";
    assert_eq!(coercions(direct), vec![1]);
    let mapped = "fn f() { let x = row.get::<_, Option<u32>>(2)?.map(g).unwrap_or(false); }";
    assert_eq!(coercions(mapped), vec![1]);
    let error_as_zero = "fn f() { let x = row.get::<_, i64>(2).ok().unwrap_or(0); }";
    assert_eq!(coercions(error_as_zero), vec![1]);
}

#[test]
fn the_scan_reads_every_default_argument() {
    for fallback in [
        "unwrap_or(0)",
        "unwrap_or(-1)",
        "unwrap_or(\"\")",
        "unwrap_or(String::new())",
        "unwrap_or(\"\".to_string())",
        "unwrap_or(String::from(\"\"))",
        "unwrap_or(Default::default())",
        "unwrap_or(Vec::new())",
        "unwrap_or_default()",
        "unwrap_or_else(|| 0)",
        "unwrap_or_else(|| { false })",
        "unwrap_or_else(String::new)",
        "unwrap_or_else(Default::default)",
        "map_or(0, |v| v + 1)",
        "map_or_else(|| 0.0, |v| v)",
    ] {
        let source = format!(
            "fn f() {{\n    let name: Option<T> = row.get(0)?;\n    use_it(name.{fallback});\n}}"
        );
        assert_eq!(coercions(&source), vec![3], "{fallback}");
        let direct = format!("fn f() {{ let x = row.get(0)?.{fallback}; }}");
        assert_eq!(coercions(&direct), vec![1], "{fallback}");
    }
}

#[test]
fn the_scan_follows_multi_line_lets_and_destructuring() {
    let multi_line = "
fn f() {
    let name: Option<String> =
        row.get(0)?;
    let other = row
        .get::<_, Option<String>>(1)?
        .map(trim);
    keep(name.unwrap_or_default());
    keep(other.unwrap_or_default());
    keep(row
        .get::<_, Option<u32>>(2)?
        .unwrap_or_else(|| 0));
}";
    assert_eq!(coercions(multi_line), vec![8, 9, 10]);
    let tuple = "
fn f() {
    let (a, b): (Option<u32>, u32) = (row.get(0)?, row.get(1)?);
    let (c, d) = (row.get::<_, Option<u32>>(2)?, 7);
    keep(a.unwrap_or(0));
    keep(c.unwrap_or(0));
}";
    assert_eq!(coercions(tuple), vec![5, 6]);
    let value_row = "
struct ValueRow { object: Option<i64>, note: Option<String> }
fn read(row: &Row) -> ValueRow { ValueRow { object: row.get(0)?, note: None } }
fn destructure(value: ValueRow) {
    let ValueRow { object, note } = value;
    keep(object.unwrap_or(0));
    keep(note.unwrap_or_default());
}
fn field(value: &ValueRow) {
    keep(value.object.unwrap_or(0));
}
impl ValueRow {
    fn method(&self) { keep(self.object.unwrap_or(0)); }
}
fn macro_argument() { info!(\"{}\", row.get::<_, Option<u32>>(0)?.unwrap_or(0)); }";
    assert_eq!(coercions(value_row), vec![6, 10, 13, 15]);
}

#[test]
fn a_binding_ends_with_its_scope_or_a_new_binding() {
    let unrelated = "
fn read() { let flags: Option<u32> = row.get(0)?; keep(flags); }
fn later(flags: Option<u32>) { keep(flags.unwrap_or(0)); }
fn shadowed() {
    let flags: Option<u32> = row.get(0)?;
    let flags = other();
    keep(flags.unwrap_or(0));
}
fn inner_scope() {
    { let flags: Option<u32> = row.get(0)?; keep(flags); }
    keep(flags.unwrap_or(0));
    let mask: Option<u32> = row.get(0)?;
    keep(values.map(|mask| mask.unwrap_or(0)));
    if let Some(mask) = mask { keep(mask.checked_sub(1).unwrap_or(0)); }
}";
    assert!(
        coercions(unrelated).is_empty(),
        "{:?}",
        coercions(unrelated)
    );
    let still_bound = "
fn f() {
    let flags: Option<u32> = row.get(0)?;
    if ready { keep(flags.unwrap_or(0)); }
    keep(items.map(|item| flags.unwrap_or(0) + item));
    let copy = flags;
    keep(copy.unwrap_or(0));
}";
    assert_eq!(coercions(still_bound), vec![4, 5, 7]);
}

#[test]
fn the_scan_leaves_honest_handling_alone() {
    // A NULL that stays absent, an explicit sentinel, a non-Option read, a fallback inside a
    // closure or an argument (it handles another value) and a labelled site.
    let clean = r#"
fn f() {
    let object: Option<u32> = row.get(0)?;
    let value = object.ok_or_else(|| "object_form_id is NULL".to_string())?;
    let side = row.get::<_, Option<u32>>(2)?.map(g).unwrap_or(PortalSide::Unknown);
    let total: i64 = row.get(0)?;
    let count = total.checked_sub(1).unwrap_or(0);
    let under = row.get::<_, Option<u32>>(0)?.map(|n| n.checked_sub(1).unwrap_or(0));
    let other = row.get::<_, Option<u32>>(0)?.map(|n| lookup(n, table.get(&n).unwrap_or(0)));
    let map_value = table.get(&key).copied().unwrap_or(0);
    let lua_field = target.get::<Option<i64>>("__alias_id")?.unwrap_or(-1);
}
"#;
    assert!(coercions(clean).is_empty(), "{:?}", coercions(clean));
    let labelled = "fn f() {\n// null-ok: xEdit says an absent flags field is 0\n\
                    let flags = row.get::<_, Option<u32>>(1)?.unwrap_or(0);\n}";
    assert!(coercions(labelled).is_empty());
    let empty_reason =
        "fn f() {\n// null-ok:\nlet flags = row.get::<_, Option<u32>>(1)?.unwrap_or(0);\n}";
    assert_eq!(coercions(empty_reason), vec![3]);
}

#[test]
fn only_test_only_items_are_skipped() {
    let source = "
#[cfg(test)]
fn helper() { keep(row.get::<_, Option<u32>>(0)?.unwrap_or(0)); }
fn production() { keep(row.get::<_, Option<u32>>(1)?.unwrap_or(0)); }
#[cfg(all(test, unix))]
mod fixtures { fn f() { keep(row.get::<_, Option<u32>>(2)?.unwrap_or(0)); } }
impl Reader {
    #[cfg(test)]
    fn probe(&self) { keep(row.get::<_, Option<u32>>(3)?.unwrap_or(0)); }
    fn read(&self) { keep(row.get::<_, Option<u32>>(4)?.unwrap_or(0)); }
}
#[cfg(not(test))]
fn release_only() { keep(row.get::<_, Option<u32>>(5)?.unwrap_or(0)); }
#[cfg(any(test, unix))]
fn also_unix() { keep(row.get::<_, Option<u32>>(6)?.unwrap_or(0)); }
#[cfg(test)]
mod tests { fn f() { keep(row.get::<_, Option<u32>>(7)?.unwrap_or(0)); } }
fn after_tests() { keep(row.get::<_, Option<u32>>(8)?.unwrap_or(0)); }
";
    assert_eq!(coercions(source), vec![4, 10, 13, 15, 18]);
}

#[test]
fn a_known_site_is_matched_by_its_place_not_by_a_count() {
    let site = |function: &str, text: &str| {
        (
            "a.rs".to_string(),
            Site {
                function: function.to_string(),
                line: 1,
                text: text.to_string(),
            },
        )
    };
    let known = [("a.rs", "f", "x"), ("a.rs", "f", "y")];
    assert_eq!(
        compare(&[site("f", "x"), site("f", "y")], &known),
        (Vec::new(), Vec::new())
    );
    // One site fixed and another added: the count is the same, and both are reported.
    let (unknown, retired) = compare(&[site("f", "x"), site("g", "y")], &known);
    assert_eq!(unknown, ["a.rs:1 in `g`: y"]);
    assert_eq!(retired, ["a.rs in `f`: y"]);
    // A second copy of a listed line is a new site.
    let (unknown, retired) = compare(&[site("f", "x"), site("f", "x"), site("f", "y")], &known);
    assert_eq!((unknown.len(), retired.len()), (1, 0));
}

#[test]
fn the_engine_readers_keep_null_absent() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = root.parent().unwrap().parent().unwrap();
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    files.sort();
    let sources: Vec<(String, String, File)> = files
        .iter()
        .map(|path| {
            let relative = path
                .strip_prefix(workspace)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let source = std::fs::read_to_string(path).unwrap();
            let file = parse(&relative, &source);
            (relative, source, file)
        })
        .collect();
    let mut structs = RowStructs::default();
    for (_, _, file) in &sources {
        structs.visit_file(file);
    }
    let fields = structs.nullable();
    let found: Vec<(String, Site)> = sources
        .iter()
        .flat_map(|(name, source, file)| {
            scan(source, file, &fields)
                .into_iter()
                .map(|site| (name.clone(), site))
        })
        .collect();
    let (unknown, retired) = compare(&found, KNOWN);
    assert!(
        unknown.is_empty() && retired.is_empty(),
        "\nNULL-to-default coercions not in KNOWN (keep NULL absent, or label a retail default \
         with `// null-ok: <reason>`):\n{}\nKNOWN entries with no site (remove them):\n{}",
        unknown.join("\n"),
        retired.join("\n")
    );
}
