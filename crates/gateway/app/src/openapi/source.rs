//! Reads the source the document is generated from: doc comments, the
//! `.route` calls an area binds, and the multipart field names.
//!
//! A missing item is an error that names the file and the item. Nothing
//! here substitutes an empty string.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use syn::{Attribute, Block, Expr, ExprLit, File, Item, Lit, Meta, Stmt};

/// One `.route` binding: the path, the lowercase method, and the handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Mount {
    /// The path literal the binding resolves to.
    pub path: String,
    /// `get`, `post`, `put`, or `delete`.
    pub method: String,
    /// The handler the method builder names.
    pub handler: String,
}

/// Why a doc comment could not be read.
#[derive(Debug)]
pub(super) enum DocError {
    /// The file could not be read or parsed.
    Unreadable(String),
    /// No item has that name.
    NotFound,
    /// The item has no doc comment.
    Undocumented,
}

impl DocError {
    /// The failure, naming `file` and `item`.
    pub(super) fn message(self, file: &str, item: &str) -> String {
        match self {
            DocError::Unreadable(detail) => detail,
            DocError::NotFound => format!("{file}: no item `{item}`"),
            DocError::Undocumented => format!("{file}: `{item}` has no doc comment"),
        }
    }
}

pub(super) fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn parsed(file: &str) -> Result<File, DocError> {
    let path = root().join(file);
    let text = fs::read_to_string(&path)
        .map_err(|error| DocError::Unreadable(format!("{}: {error}", path.display())))?;
    syn::parse_file(&text).map_err(|error| DocError::Unreadable(format!("{file}: {error}")))
}

fn doc_lines(attrs: &[Attribute]) -> Option<String> {
    let mut lines = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        let Meta::NameValue(pair) = &attr.meta else {
            continue;
        };
        let Expr::Lit(ExprLit {
            lit: Lit::Str(text),
            ..
        }) = &pair.value
        else {
            continue;
        };
        let line = text.value();
        lines.push(line.strip_prefix(' ').unwrap_or(&line).to_owned());
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// The doc comment of the function, type, const, or inherent method named `item`.
pub(super) fn doc(file: &str, item: &str) -> Result<String, DocError> {
    let parsed = parsed(file)?;
    for found in &parsed.items {
        let found = match found {
            Item::Fn(function) if function.sig.ident == item => doc_lines(&function.attrs),
            Item::Struct(found) if found.ident == item => doc_lines(&found.attrs),
            Item::Enum(found) if found.ident == item => doc_lines(&found.attrs),
            Item::Const(found) if found.ident == item => doc_lines(&found.attrs),
            Item::Impl(found) => {
                let method = found.items.iter().find_map(|inner| {
                    let syn::ImplItem::Fn(function) = inner else {
                        return None;
                    };
                    (function.sig.ident == item).then(|| doc_lines(&function.attrs))
                });
                if let Some(doc) = method {
                    return doc.ok_or(DocError::Undocumented);
                }
                continue;
            }
            _ => continue,
        };
        return found.ok_or(DocError::Undocumented);
    }
    Err(DocError::NotFound)
}

/// The doc comment of one enum variant.
pub(super) fn variant_doc(file: &str, enum_name: &str, variant: &str) -> Result<String, DocError> {
    let parsed = parsed(file)?;
    for found in &parsed.items {
        let Item::Enum(item) = found else {
            continue;
        };
        if item.ident != enum_name {
            continue;
        }
        let Some(variant) = item.variants.iter().find(|item| item.ident == variant) else {
            return Err(DocError::NotFound);
        };
        return doc_lines(&variant.attrs).ok_or(DocError::Undocumented);
    }
    Err(DocError::NotFound)
}

/// The inner doc comment at the top of `file`.
pub(super) fn module_doc(file: &str) -> Result<String, DocError> {
    doc_lines(&parsed(file)?.attrs).ok_or(DocError::Undocumented)
}

/// The line `item` is declared on. Fails when the span carries no location.
pub(super) fn item_line(file: &str, item: &str) -> Result<usize, String> {
    let parsed = parsed(file).map_err(|error| error.message(file, item))?;
    for found in parsed.items {
        let located = match found {
            Item::Fn(function) => Some((function.sig.ident.to_string(), function.sig.ident.span())),
            Item::Struct(found) => Some((found.ident.to_string(), found.ident.span())),
            Item::Enum(found) => Some((found.ident.to_string(), found.ident.span())),
            Item::Const(found) => Some((found.ident.to_string(), found.ident.span())),
            Item::Impl(found) => {
                for inner in found.items {
                    let syn::ImplItem::Fn(function) = inner else {
                        continue;
                    };
                    if function.sig.ident == item {
                        return line_of(file, item, function.sig.ident.span());
                    }
                }
                None
            }
            _ => None,
        };
        if let Some((name, span)) = located
            && name == item
        {
            return line_of(file, item, span);
        }
    }
    Err(format!("{file}: no item `{item}`"))
}

fn line_of(file: &str, item: &str, span: proc_macro2::Span) -> Result<usize, String> {
    let line = span.start().line;
    if line == 0 {
        Err(format!("{file}: `{item}` has no source location"))
    } else {
        Ok(line)
    }
}

/// The `.route` bindings in the `routes` function of `file`.
pub(super) fn mounts(file: &str) -> Result<Vec<Mount>, String> {
    let parsed = parsed(file).map_err(|error| error.message(file, "routes"))?;
    let consts = parsed
        .items
        .iter()
        .filter_map(|item| {
            let Item::Const(item) = item else {
                return None;
            };
            const_path(&item.expr).map(|value| (item.ident.to_string(), value))
        })
        .collect::<HashMap<_, _>>();
    let Some(function) = parsed.items.iter().find_map(|item| match item {
        Item::Fn(item) if item.sig.ident == "routes" => Some(item),
        _ => None,
    }) else {
        return Err(format!("{file}: no `routes` function"));
    };
    let mut found = Vec::new();
    walk_block(&function.block, &consts, &mut found);
    if found.is_empty() {
        return Err(format!("{file}: `routes` binds no route"));
    }
    Ok(found)
}

/// The string-literal match arms of `parse_form`, excluding the `_` arm.
pub(super) fn multipart_fields(file: &str) -> Result<Vec<String>, String> {
    let parsed = parsed(file).map_err(|error| error.message(file, "parse_form"))?;
    let Some(function) = parsed.items.iter().find_map(|item| match item {
        Item::Fn(item) if item.sig.ident == "parse_form" => Some(item),
        _ => None,
    }) else {
        return Err(format!("{file}: no `parse_form` function"));
    };
    let mut fields = Vec::new();
    collect_match_strings_block(&function.block, &mut fields);
    if fields.is_empty() {
        return Err(format!("{file}: `parse_form` has no string match arms"));
    }
    Ok(fields)
}

fn const_path(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(text),
            ..
        }) => Some(text.value()),
        Expr::Call(call) => {
            let path = path_idents(&call.func)?;
            if path.len() == 2
                && path[0] == "RouteInfo"
                && matches!(path[1].as_str(), "open" | "walled")
            {
                match call.args.first() {
                    Some(Expr::Lit(ExprLit {
                        lit: Lit::Str(text),
                        ..
                    })) => Some(text.value()),
                    _ => None,
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

fn path_idents(expr: &Expr) -> Option<Vec<String>> {
    let Expr::Path(path) = expr else {
        return None;
    };
    Some(
        path.path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect(),
    )
}

fn walk_block(block: &Block, consts: &HashMap<String, String>, found: &mut Vec<Mount>) {
    for stmt in &block.stmts {
        if let Stmt::Expr(expr, _) = stmt {
            walk_expr(expr, consts, found);
        }
    }
}

fn walk_expr(expr: &Expr, consts: &HashMap<String, String>, found: &mut Vec<Mount>) {
    match expr {
        Expr::MethodCall(call) => {
            if call.method == "route"
                && call.args.len() == 2
                && let Some(path) = call.args.first().and_then(|arg| resolve(arg, consts))
                && let Some(handlers) = call.args.get(1)
            {
                for (method, handler) in method_handlers(handlers) {
                    found.push(Mount {
                        path: path.clone(),
                        method,
                        handler,
                    });
                }
            }
            walk_expr(&call.receiver, consts, found);
            for arg in &call.args {
                walk_expr(arg, consts, found);
            }
        }
        Expr::Call(call) => {
            walk_expr(&call.func, consts, found);
            for arg in &call.args {
                walk_expr(arg, consts, found);
            }
        }
        Expr::Block(block) => walk_block(&block.block, consts, found),
        _ => {}
    }
}

fn resolve(expr: &Expr, consts: &HashMap<String, String>) -> Option<String> {
    match expr {
        Expr::Field(field) => {
            let syn::Member::Named(member) = &field.member else {
                return None;
            };
            if member != "path" {
                return None;
            }
            let name = path_idents(&field.base)?.pop()?;
            consts.get(&name).cloned()
        }
        Expr::Path(_) => {
            let name = path_idents(expr)?.pop()?;
            consts.get(&name).cloned()
        }
        _ => None,
    }
}

fn method_handlers(expr: &Expr) -> Vec<(String, String)> {
    match expr {
        Expr::Call(call) => {
            let Some(name) = path_idents(&call.func).and_then(|mut path| path.pop()) else {
                return Vec::new();
            };
            if !is_method(&name) {
                return Vec::new();
            }
            let Some(handler) = call.args.first().and_then(handler_ident) else {
                return Vec::new();
            };
            vec![(name, handler)]
        }
        Expr::MethodCall(call) => {
            let mut found = method_handlers(&call.receiver);
            let name = call.method.to_string();
            if is_method(&name)
                && let Some(handler) = call.args.first().and_then(handler_ident)
            {
                found.push((name, handler));
            }
            found
        }
        _ => Vec::new(),
    }
}

fn handler_ident(expr: &Expr) -> Option<String> {
    path_idents(expr).and_then(|mut path| path.pop())
}

fn is_method(name: &str) -> bool {
    matches!(name, "get" | "post" | "put" | "delete" | "patch" | "head")
}

fn collect_match_strings_block(block: &Block, fields: &mut Vec<String>) {
    for stmt in &block.stmts {
        if let Stmt::Expr(expr, _) = stmt {
            collect_match_strings(expr, fields);
        }
    }
}

/// The `match name.as_str()` that dispatches multipart fields. Inner
/// matches (the format spellings) are not field names.
fn is_field_match(expr: &Expr) -> bool {
    let Expr::MethodCall(call) = expr else {
        return false;
    };
    call.method == "as_str"
}

fn collect_match_strings(expr: &Expr, fields: &mut Vec<String>) {
    match expr {
        Expr::Match(matched) if is_field_match(&matched.expr) => {
            for arm in &matched.arms {
                if let syn::Pat::Lit(ExprLit {
                    lit: Lit::Str(text),
                    ..
                }) = &arm.pat
                {
                    fields.push(text.value());
                }
            }
        }
        Expr::Match(matched) => {
            collect_match_strings(&matched.expr, fields);
            for arm in &matched.arms {
                collect_match_strings(&arm.body, fields);
            }
        }
        Expr::Block(block) => collect_match_strings_block(&block.block, fields),
        Expr::While(item) => {
            collect_match_strings(&item.cond, fields);
            collect_match_strings_block(&item.body, fields);
        }
        Expr::Loop(item) => collect_match_strings_block(&item.body, fields),
        Expr::ForLoop(item) => {
            collect_match_strings(&item.expr, fields);
            collect_match_strings_block(&item.body, fields);
        }
        Expr::If(item) => {
            collect_match_strings(&item.cond, fields);
            collect_match_strings_block(&item.then_branch, fields);
            if let Some((_, else_branch)) = &item.else_branch {
                collect_match_strings(else_branch, fields);
            }
        }
        Expr::Let(item) => collect_match_strings(&item.expr, fields),
        Expr::Try(item) => collect_match_strings(&item.expr, fields),
        Expr::Await(item) => collect_match_strings(&item.base, fields),
        Expr::MethodCall(call) => {
            collect_match_strings(&call.receiver, fields);
            for arg in &call.args {
                collect_match_strings(arg, fields);
            }
        }
        Expr::Call(call) => {
            collect_match_strings(&call.func, fields);
            for arg in &call.args {
                collect_match_strings(arg, fields);
            }
        }
        Expr::Return(item) => {
            if let Some(expr) = &item.expr {
                collect_match_strings(expr, fields);
            }
        }
        _ => {}
    }
}
