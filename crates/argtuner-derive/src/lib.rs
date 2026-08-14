use std::collections::BTreeSet;

use proc_macro::TokenStream;
use quote::quote;
use syn::parse_macro_input;
use syn::punctuated::Punctuated;
use syn::{Fields, GenericArgument, ItemStruct, Lit, PathArguments, Token};

/// Generate the `argtuner_sdk::TunerParams` implementation for a plain struct,
/// turning it into both a production `clap` CLI and an argtuner
/// template/search-space definition.
///
/// ```rust
/// use argtuner_derive::tuner_params;
/// use argtuner_sdk::ParamRole;
///
/// #[tuner_params]
/// struct ModelTunerParams {
///     /// Learning rate
///     #[param(role = ParamRole::Tune, default = 0.001, min = 0.0001, max = 0.1, log = true)]
///     lr: f64,
///     #[param(role = ParamRole::Tune, choices = ["adam", "adamw", "sgd"])]
///     optimizer: String,
///     #[param(role = ParamRole::Injected, value_name = "trial_dir")]
///     checkpoint_dir: Option<String>,
/// }
///
/// fn main() {
///     assert_eq!(<ModelTunerParams as argtuner_sdk::TunerParams>::tuner_params().len(), 3);
/// }
/// ```
#[proc_macro_attribute]
pub fn tuner_params(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(item as ItemStruct);

    // Capture `#[param(...)]` metadata first, then strip the helper attribute
    // from the re-emitted struct so the compiler doesn't see an unknown
    // attribute after expansion. Validation errors abort expansion.
    let param_attrs: Vec<ParamAttrs> = match &input.fields {
        Fields::Named(fields) => {
            match fields
                .named
                .iter()
                .map(parse_param_attrs)
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(attrs) => attrs,
                Err(err) => return err.to_compile_error().into(),
            }
        }
        _ => Vec::new(),
    };
    if let Fields::Named(fields) = &mut input.fields {
        for field in &mut fields.named {
            field.attrs.retain(|a| !a.path().is_ident("param"));
        }
    }

    if !input.generics.params.is_empty() {
        return syn::Error::new_spanned(&input, "tuner_params does not support generic structs")
            .to_compile_error()
            .into();
    }

    let struct_ident = &input.ident;
    let fields = match &input.fields {
        Fields::Named(fields) => fields,
        _ => {
            return syn::Error::new_spanned(
                &input,
                "tuner_params only supports structs with named fields",
            )
            .to_compile_error()
            .into();
        }
    };

    let mut param_specs = Vec::new();
    let mut command_args = Vec::new();
    let mut field_inits = Vec::new();
    let mut lazy_default_fns = Vec::new();

    for (field, attrs) in fields.named.iter().zip(param_attrs.iter()) {
        let ident = field.ident.as_ref().expect("named field");
        let name = ident.to_string();
        let ty = &field.ty;
        let (is_option, inner_ty) = unwrap_option(ty);
        // Kind is classified on the unwrapped inner type so `Option<f64>` is a
        // `Float`, `Option<usize>` an `Int`, `Option<bool>` a `Bool`, etc.
        let is_bool = type_is_ident(&inner_ty, "bool");
        let is_float = type_is_float(&inner_ty);
        let is_int = type_is_int(&inner_ty);
        let is_string = type_is_ident(&inner_ty, "String");
        let help = doc_comment(field);

        let long = attrs
            .long
            .as_deref()
            .map(str::to_string)
            .unwrap_or_else(|| name.replace('_', "-"));
        let long_lit = lit_str(&long);
        let name_lit = lit_str(&name);

        let kind_tag = if is_bool {
            "Bool"
        } else if is_float {
            "Float"
        } else if is_int {
            "Int"
        } else if !attrs.choices.is_empty() {
            "Choice"
        } else {
            "Other"
        };
        let kind = match kind_tag {
            "Bool" => quote!(::argtuner_sdk::ParamKind::Bool),
            "Float" => quote!(::argtuner_sdk::ParamKind::Float),
            "Int" => quote!(::argtuner_sdk::ParamKind::Int),
            "Choice" => quote!(::argtuner_sdk::ParamKind::Choice),
            _ => quote!(::argtuner_sdk::ParamKind::Other),
        };

        let role_name = attrs.role.as_deref().unwrap_or("fixed");
        let role = match role_name {
            "tune" => quote!(::argtuner_sdk::ParamRole::Tune),
            "injected" => quote!(::argtuner_sdk::ParamRole::Injected),
            "cli" => quote!(::argtuner_sdk::ParamRole::Cli),
            _ => quote!(::argtuner_sdk::ParamRole::Fixed),
        };

        // Kind-dependent validation: `role = ParamRole::Tune` must carry the constraints
        // its kind requires, and bools reject numeric/categorical constraints.
        if role_name == "tune" {
            match kind_tag {
                "Bool" => {
                    for key in ["min", "max", "step", "log", "choices"] {
                        if attrs.present.contains(key) {
                            return syn::Error::new_spanned(
                                field,
                                format!(
                                    "`role = \"tune\"` on a bool parameter does not take `{key}`; \
                                     a tuned bool is a bare on/off toggle"
                                ),
                            )
                            .to_compile_error()
                            .into();
                        }
                    }
                }
                "Float" | "Int" => {
                    if attrs.min.is_none() || attrs.max.is_none() {
                        return syn::Error::new_spanned(
                            field,
                            format!(
                                "`role = \"tune\"` on a {kind_tag} parameter requires both \
                                 `min` and `max` (bounds define the sampled range)"
                            ),
                        )
                        .to_compile_error()
                        .into();
                    }
                }
                _ => {
                    // Choice has `choices` by construction; `Other` needs them too.
                    if attrs.choices.is_empty() {
                        return syn::Error::new_spanned(
                            field,
                            "`role = \"tune\"` on a string/other parameter requires \
                             `choices = [...]`",
                        )
                        .to_compile_error()
                        .into();
                    }
                }
            }
        }
        // Operational `cli` flags must be optional or carry a default, otherwise
        // `from_matches` would panic on an absent flag during standalone runs.
        if role_name == "cli" && !is_option && attrs.default.is_none() {
            return syn::Error::new_spanned(
                field,
                "`role = \"cli\"` operational flags must be `Option<T>` or carry a \
                 `default` so standalone runs without the flag do not panic",
            )
            .to_compile_error()
            .into();
        }

        let value_name = match attrs.value_name.as_deref() {
            Some(vn) => quote!(Some(#vn)),
            None => quote!(None),
        };
        let default_lit = attrs.default.as_ref().and_then(expr_literal);
        // A non-literal `default` on a numeric/bool field needs a lazily
        // stringified `&'static str` (the value of a `const` is unknowable at
        // macro-expansion time), shared by both the clap default and the
        // `TunerParam` descriptor.
        let default_fn_ident = if default_lit.is_none()
            && attrs.default.is_some()
            && !is_string
        {
            Some(proc_macro2::Ident::new(
                &format!("__argtuner_param_default_{name}"),
                proc_macro2::Span::call_site(),
            ))
        } else {
            None
        };
        if let Some(f) = &default_fn_ident {
            let expr = attrs.default.as_ref().expect("const default");
            lazy_default_fns.push(quote! {
                #[doc(hidden)]
                fn #f() -> &'static str {
                    static VALUE: ::std::sync::OnceLock<::std::string::String> =
                        ::std::sync::OnceLock::new();
                    VALUE.get_or_init(|| ::std::format!("{}", #expr)).as_str()
                }
            });
        }
        let default_tok = match (&attrs.default, default_lit.as_deref(), &default_fn_ident, is_string) {
            (None, _, _, _) => quote!(None),
            (_, Some(lit), _, _) => quote!(Some(#lit)),
            (Some(e), None, None, true) => quote!(Some(#e)),
            (Some(_), None, Some(f), _) => quote!(Some(#f())),
            _ => quote!(None),
        };
        let help_tok = match help.as_deref() {
            Some(h) => quote!(Some(#h)),
            None => quote!(None),
        };
        let min = numeric_tok(&attrs.min);
        let max = numeric_tok(&attrs.max);
        let log = attrs.log;
        let step = numeric_tok(&attrs.step);
        let choices = if attrs.choices.is_empty() {
            quote!(&[])
        } else {
            let cs = attrs.choices.iter().map(|c| lit_str(c));
            quote!(&[#(#cs),*])
        };
        let parent = match attrs.parent.as_deref() {
            Some(p) => quote!(Some(#p)),
            None => quote!(None),
        };
        let parent_values = if attrs.parent_values.is_empty() {
            quote!(&[])
        } else {
            let pvs = attrs.parent_values.iter().map(|v| lit_str(v));
            quote!(&[#(#pvs),*])
        };

        param_specs.push(quote! {
            ::argtuner_sdk::TunerParam {
                name: #name_lit,
                long: #long_lit,
                value_name: #value_name,
                default: #default_tok,
                help: #help_tok,
                kind: #kind,
                role: #role,
                min: #min,
                max: #max,
                log: #log,
                step: #step,
                choices: #choices,
                parent: #parent,
                parent_values: #parent_values,
            }
        });

        // clap Arg builder for this field.
        let value_name_arg = match attrs.value_name.as_deref() {
            Some(vn) => quote!(.value_name(#vn)),
            None => quote!(),
        };
        let help_arg = match help.as_deref() {
            Some(h) => quote!(.help(#h)),
            None => quote!(),
        };
        let default_arg = match (&attrs.default, default_lit.as_deref(), &default_fn_ident, is_string) {
            (None, _, _, _) => quote!(),
            (_, Some(lit), _, _) => quote!(.default_value(#lit)),
            (Some(e), None, None, true) => quote!(.default_value(#e)),
            (Some(_), None, Some(f), _) => quote!(.default_value(#f())),
            _ => quote!(),
        };
        let required = if !is_option && attrs.default.is_none() {
            quote!(.required(true))
        } else {
            quote!()
        };
        // bools are flag-friendly value args: `--flag` alone means `true`,
        // while `--flag false` (from a tuned trial) works too.
        let (value_parser, bool_flag) = if is_bool {
            (quote!(::argtuner_sdk::clap::value_parser!(bool)), true)
        } else if !attrs.choices.is_empty() && is_string {
            // PossibleValuesParser yields String; only attach it to String-typed
            // choice fields so typed get_one::<Inner> never downcasts wrong.
            let cs = attrs.choices.iter().map(|c| lit_str(c));
            (
                quote! {
                    ::argtuner_sdk::clap::builder::PossibleValuesParser::new([#(#cs),*])
                },
                false,
            )
        } else {
            (
                quote!(::argtuner_sdk::clap::value_parser!(#inner_ty)),
                false,
            )
        };
        let num_args = if bool_flag {
            quote!(.num_args(0..=1).default_missing_value("true"))
        } else {
            quote!()
        };
        command_args.push(quote! {
            .arg(
                ::argtuner_sdk::clap::Arg::new(#name_lit)
                    .long(#long_lit)
                    #value_name_arg
                    #help_arg
                    #default_arg
                    #required
                    #num_args
                    .value_parser(#value_parser),
            )
        });

        // Field extraction in from_matches.
        let id_lit = lit_str(&name);
        let err_lit = lit_str(&format!("--{long}"));
        let init = if is_option {
            quote!(m.get_one::<#inner_ty>(#id_lit).cloned())
        } else {
            quote!(m.get_one::<#inner_ty>(#id_lit).cloned().expect(#err_lit))
        };
        field_inits.push(quote!(#ident: #init));
    }

    let expanded = quote! {
        #input

        #(#lazy_default_fns)*

        impl ::argtuner_sdk::TunerParams for #struct_ident {
            fn app_name() -> &'static str {
                env!("CARGO_PKG_NAME")
            }

            fn tuner_params() -> &'static [::argtuner_sdk::TunerParam] {
                static PARAMS: ::std::sync::OnceLock<Vec<::argtuner_sdk::TunerParam>> =
                    ::std::sync::OnceLock::new();
                PARAMS
                    .get_or_init(|| ::std::vec![#(#param_specs),*])
                    .as_slice()
            }

            fn command() -> ::argtuner_sdk::clap::Command {
                ::argtuner_sdk::clap::Command::new(Self::app_name())
                    .version(env!("CARGO_PKG_VERSION"))
                    #(#command_args)*
            }

            fn from_matches(m: &::argtuner_sdk::clap::ArgMatches) -> Self {
                Self {
                    #(#field_inits),*
                }
            }
        }
    };
    TokenStream::from(expanded)
}

struct ParamAttrs {
    role: Option<String>,
    default: Option<syn::Expr>,
    long: Option<String>,
    value_name: Option<String>,
    min: Option<syn::Expr>,
    max: Option<syn::Expr>,
    log: bool,
    step: Option<syn::Expr>,
    choices: Vec<String>,
    parent: Option<String>,
    parent_values: Vec<String>,
    /// Keys that appeared in `#[param(...)]`, for presence-aware validation
    /// (e.g. `log = false` vs. absent, `choices = []` vs. absent).
    present: BTreeSet<&'static str>,
}

/// The reserved placeholders argtuner can inject. Kept in sync with
/// `argtuner_common::{PLACEHOLDER_TRIAL_DIR, PLACEHOLDER_TRIAL_ID}`.
const INJECTED_PLACEHOLDERS: [&str; 2] = ["trial_dir", "trial_id"];

/// Attribute keys permitted per `role` (in addition to `role` itself and
/// `long`, which are allowed on every role). The lookup uses a static array
/// returned by index, then filters out any key that is allowed.
fn prohibited_for_role(role: &str) -> &'static [&'static str] {
    match role {
        "fixed" => &[
            "min",
            "max",
            "step",
            "log",
            "choices",
            "parent",
            "parent_values",
            "value_name",
        ],
        "tune" => &["value_name"],
        "injected" => &[
            "default",
            "min",
            "max",
            "step",
            "log",
            "choices",
            "parent",
            "parent_values",
        ],
        "cli" => &[
            "min",
            "max",
            "step",
            "log",
            "choices",
            "parent",
            "parent_values",
            "value_name",
        ],
        _ => &[],
    }
}

fn parse_param_attrs(field: &syn::Field) -> Result<ParamAttrs, syn::Error> {
    let mut out = ParamAttrs {
        role: None,
        default: None,
        long: None,
        value_name: None,
        min: None,
        max: None,
        log: false,
        step: None,
        choices: Vec::new(),
        parent: None,
        parent_values: Vec::new(),
        present: BTreeSet::new(),
    };
    for attr in &field.attrs {
        if !attr.path().is_ident("param") {
            continue;
        }
        let metas = attr
            .parse_args_with(Punctuated::<syn::MetaNameValue, Token![,]>::parse_terminated)
            .unwrap_or_default();
        for nv in metas {
            let Some(key) = nv.path.get_ident().map(|i| i.to_string()) else {
                continue;
            };
            let value = &nv.value;
            match key.as_str() {
                "role" => {
                    out.role = parse_role(value)?;
                    out.present.insert("role");
                }
                "default" => {
                    out.default = Some(value.clone());
                    out.present.insert("default");
                }
                "long" => {
                    out.long = expr_string(value);
                    out.present.insert("long");
                }
                "value_name" => {
                    out.value_name = expr_string(value);
                    out.present.insert("value_name");
                }
                "min" => {
                    out.min = Some(value.clone());
                    out.present.insert("min");
                }
                "max" => {
                    out.max = Some(value.clone());
                    out.present.insert("max");
                }
                "step" => {
                    out.step = Some(value.clone());
                    out.present.insert("step");
                }
                "log" => {
                    out.log = expr_bool(value);
                    out.present.insert("log");
                }
                "choices" => {
                    out.choices = expr_string_array(value);
                    out.present.insert("choices");
                }
                "parent" => {
                    out.parent = expr_string(value);
                    out.present.insert("parent");
                }
                "parent_values" => {
                    out.parent_values = expr_string_array(value);
                    out.present.insert("parent_values");
                }
                "skip" => {
                    return Err(syn::Error::new_spanned(
                        field,
                        "`skip = true` was removed; use `role = \"cli\"` for an operational \
                         flag excluded from the template and search space, or omit it for a \
                         fixed argument",
                    ));
                }
                _ => {}
            }
        }
    }

    let role = out.role.as_deref().unwrap_or("fixed");
    if !["fixed", "tune", "injected", "cli"].contains(&role) {
        // Unreachable: parse_role validates the canonical variant up front.
        return Err(syn::Error::new_spanned(
            field,
            format!(
                "unknown `role` {role:?}; expected `role = ParamRole::Fixed`, \
                 `role = ParamRole::Tune`, `role = ParamRole::Injected`, or \
                 `role = ParamRole::Cli`"
            ),
        ));
    }

    for key in prohibited_for_role(role) {
        if out.present.contains(key) {
            return Err(syn::Error::new_spanned(
                field,
                format!(
                    "`{key}` is not valid on a `role = {role:?}` parameter; it would be \
                     silently ignored"
                ),
            ));
        }
    }

    if role == "injected" {
        match out.value_name.as_deref() {
            Some(vn) if INJECTED_PLACEHOLDERS.contains(&vn) => {}
            Some(vn) => {
                return Err(syn::Error::new_spanned(
                    field,
                    format!(
                        "`role = \"injected\"` value_name {vn:?} is not a placeholder argtuner \
                         can inject; use \"trial_dir\" or \"trial_id\""
                    ),
                ));
            }
            None => {
                return Err(syn::Error::new_spanned(
                    field,
                    "`role = \"injected\"` requires `value_name = \"trial_dir\"` or \
                     `value_name = \"trial_id\"`",
                ));
            }
        }
    }

    Ok(out)
}

fn unwrap_option(ty: &syn::Type) -> (bool, syn::Type) {
    if let syn::Type::Path(tp) = ty
        && let Some(seg) = tp.path.segments.last()
        && seg.ident == "Option"
        && let PathArguments::AngleBracketed(ab) = &seg.arguments
        && let Some(GenericArgument::Type(inner)) = ab.args.first()
    {
        return (true, inner.clone());
    }
    (false, ty.clone())
}

fn type_is_ident(ty: &syn::Type, ident: &str) -> bool {
    if let syn::Type::Path(tp) = ty
        && let Some(seg) = tp.path.segments.last()
    {
        return seg.ident == ident;
    }
    false
}

fn type_is_float(ty: &syn::Type) -> bool {
    type_is_ident(ty, "f32") || type_is_ident(ty, "f64")
}

fn type_is_int(ty: &syn::Type) -> bool {
    [
        "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
    ]
    .iter()
    .any(|i| type_is_ident(ty, i))
}

fn doc_comment(field: &syn::Field) -> Option<String> {
    let mut lines = Vec::new();
    for attr in &field.attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let syn::Meta::NameValue(nv) = &attr.meta
            && let syn::Expr::Lit(syn::ExprLit {
                lit: Lit::Str(s), ..
            }) = &nv.value
        {
            lines.push(s.value());
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join(" "))
    }
}

fn lit_str(s: &str) -> String {
    s.to_owned()
}

fn expr_string(e: &syn::Expr) -> Option<String> {
    if let syn::Expr::Lit(syn::ExprLit { lit, .. }) = e {
        match lit {
            Lit::Str(s) => return Some(s.value()),
            Lit::Int(i) => return Some(i.base10_digits().to_string()),
            Lit::Float(f) => return Some(f.base10_digits().to_string()),
            Lit::Bool(b) => return Some(b.value().to_string()),
            Lit::Char(c) => return Some(c.value().to_string()),
            _ => {}
        }
    }
    Some(quote!(#e).to_string())
}

/// Parse `role = <expr>` as an enum identifier/path and return the canonical
/// variant name. Accepts bare idents (`tune`), enum paths (`ParamRole::Tune`),
/// and fully qualified paths (`argtuner_sdk::ParamRole::Tune`), matched
/// case-insensitively on the terminal segment. String literals and other
/// non-path expressions are rejected. All errors are spanned on `expr` so the
/// diagnostic underlines exactly the invalid `role` value token.
fn parse_role(expr: &syn::Expr) -> Result<Option<String>, syn::Error> {
    let terminal = match expr {
        syn::Expr::Path(p) => p
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .ok_or_else(|| syn::Error::new_spanned(expr, "empty path provided for `role`"))?,
        syn::Expr::Lit(syn::ExprLit {
            lit: Lit::Str(_), ..
        }) => {
            return Err(syn::Error::new_spanned(
                expr,
                "string literals are no longer accepted for `role`; use \
                 `role = ParamRole::Tune` (or bare `role = tune`)",
            ));
        }
        _ => {
            return Err(syn::Error::new_spanned(
                expr,
                "`role` must be an enum variant path like `role = ParamRole::Tune` \
                 (bare `role = tune` also works)",
            ));
        }
    };

    let canonical = match terminal.as_str() {
        s if s.eq_ignore_ascii_case("fixed") => "fixed",
        s if s.eq_ignore_ascii_case("tune") => "tune",
        s if s.eq_ignore_ascii_case("injected") => "injected",
        s if s.eq_ignore_ascii_case("cli") => "cli",
        _ => {
            return Err(syn::Error::new_spanned(
                expr,
                format!(
                    "unknown `role` `{terminal}`; expected `ParamRole::Fixed`, \
                     `ParamRole::Tune`, `ParamRole::Injected`, or `ParamRole::Cli`"
                ),
            ));
        }
    };

    Ok(Some(canonical.to_string()))
}

fn expr_f64(e: &syn::Expr) -> Option<f64> {
    if let syn::Expr::Lit(syn::ExprLit { lit, .. }) = e {
        match lit {
            Lit::Int(i) => return i.base10_digits().parse().ok(),
            Lit::Float(f) => return f.base10_digits().parse().ok(),
            _ => {}
        }
    }
    None
}

fn expr_bool(e: &syn::Expr) -> bool {
    matches!(
        e,
        syn::Expr::Lit(syn::ExprLit { lit: Lit::Bool(b), .. }) if b.value
    )
}

fn expr_string_array(e: &syn::Expr) -> Vec<String> {
    let syn::Expr::Array(arr) = e else {
        return Vec::new();
    };
    arr.elems
        .iter()
        .filter_map(|el| match el {
            syn::Expr::Lit(syn::ExprLit {
                lit: Lit::Str(s), ..
            }) => Some(s.value()),
            _ => None,
        })
        .collect()
}

/// Emit a `Some(f64)` for a `min`/`max`/`step` value. Literals are inlined;
/// any other (const) expression is forwarded as `Some(#expr as f64)` and
/// evaluated lazily inside `tuner_params()`.
fn numeric_tok(expr: &Option<syn::Expr>) -> proc_macro2::TokenStream {
    match expr {
        None => quote!(None),
        Some(e) => {
            if let Some(f) = expr_f64(e) {
                quote!(Some(#f))
            } else {
                quote!(Some(#e as f64))
            }
        }
    }
}

/// String form if `expr` is a literal (`"..."`, `123`, `1.5`, `true`, ...);
/// `None` for any other expression (e.g. a `const` path).
fn expr_literal(e: &syn::Expr) -> Option<String> {
    if let syn::Expr::Lit(syn::ExprLit { lit, .. }) = e {
        match lit {
            Lit::Str(s) => return Some(s.value()),
            Lit::Int(i) => return Some(i.base10_digits().to_string()),
            Lit::Float(f) => return Some(f.base10_digits().to_string()),
            Lit::Bool(b) => return Some(b.value().to_string()),
            Lit::Char(c) => return Some(c.value().to_string()),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::INJECTED_PLACEHOLDERS;

    #[test]
    fn injected_whitelist_stays_in_sync_with_common() {
        assert_eq!(
            INJECTED_PLACEHOLDERS,
            [
                argtuner_common::PLACEHOLDER_TRIAL_DIR,
                argtuner_common::PLACEHOLDER_TRIAL_ID
            ]
        );
    }
}
