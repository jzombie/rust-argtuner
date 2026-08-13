use proc_macro::TokenStream;
use quote::quote;
use syn::parse_macro_input;
use syn::punctuated::Punctuated;
use syn::{Fields, GenericArgument, ItemStruct, Lit, PathArguments, Token};

/// Generate the `argtuner_sdk::Params` implementation for a plain struct,
/// turning it into both a production `clap` CLI and an argtuner
/// template/search-space definition.
///
/// ```rust
/// use argtuner_derive::talkback_args;
///
/// #[talkback_args]
/// struct ModelParams {
///     /// Learning rate
///     #[param(default = 0.001, min = 0.0001, max = 0.1, log = true)]
///     lr: f64,
///     #[param(choices = ["adam", "adamw", "sgd"])]
///     optimizer: String,
///     #[param(value_name = "trial_dir")]
///     checkpoint_dir: Option<String>,
/// }
///
/// fn main() {
///     assert_eq!(<ModelParams as argtuner_sdk::Params>::params().len(), 3);
/// }
/// ```
#[proc_macro_attribute]
pub fn talkback_args(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(item as ItemStruct);

    // Capture `#[param(...)]` metadata first, then strip the helper attribute
    // from the re-emitted struct so the compiler doesn't see an unknown
    // attribute after expansion.
    let param_attrs: Vec<ParamAttrs> = match &input.fields {
        Fields::Named(fields) => fields.named.iter().map(parse_param_attrs).collect(),
        _ => Vec::new(),
    };
    if let Fields::Named(fields) = &mut input.fields {
        for field in &mut fields.named {
            field.attrs.retain(|a| !a.path().is_ident("param"));
        }
    }

    if !input.generics.params.is_empty() {
        return syn::Error::new_spanned(&input, "talkback_args does not support generic structs")
            .to_compile_error()
            .into();
    }

    let struct_ident = &input.ident;
    let fields = match &input.fields {
        Fields::Named(fields) => fields,
        _ => {
            return syn::Error::new_spanned(
                &input,
                "talkback_args only supports structs with named fields",
            )
            .to_compile_error()
            .into();
        }
    };
    let field_count = fields.named.len();

    let mut param_specs = Vec::new();
    let mut command_args = Vec::new();
    let mut field_inits = Vec::new();

    for (field, attrs) in fields.named.iter().zip(param_attrs.iter()) {
        let ident = field.ident.as_ref().expect("named field");
        let name = ident.to_string();
        let ty = &field.ty;
        let (is_option, inner_ty) = unwrap_option(ty);
        let is_bool = !is_option && type_is_ident(ty, "bool");
        let is_float = !is_option && type_is_float(ty);
        let is_int = !is_option && type_is_int(ty);
        let is_string = !is_option && type_is_ident(ty, "String");
        let help = doc_comment(field);

        let long = attrs
            .long
            .as_deref()
            .map(str::to_string)
            .unwrap_or_else(|| name.replace('_', "-"));
        let long_lit = lit_str(&long);
        let name_lit = lit_str(&name);

        let kind = if is_bool {
            quote!(::argtuner_sdk::ParamKind::Bool)
        } else if is_float {
            quote!(::argtuner_sdk::ParamKind::Float)
        } else if is_int {
            quote!(::argtuner_sdk::ParamKind::Int)
        } else if !attrs.choices.is_empty() {
            quote!(::argtuner_sdk::ParamKind::Choice)
        } else {
            quote!(::argtuner_sdk::ParamKind::Other)
        };

        let value_name = match attrs.value_name.as_deref() {
            Some(vn) => quote!(Some(#vn)),
            None => quote!(None),
        };
        let default_tok = match attrs.default.as_deref() {
            Some(d) => quote!(Some(#d)),
            None => quote!(None),
        };
        let help_tok = match help.as_deref() {
            Some(h) => quote!(Some(#h)),
            None => quote!(None),
        };
        let min = f64_tok(attrs.min);
        let max = f64_tok(attrs.max);
        let log = attrs.log;
        let step = f64_tok(attrs.step);
        let choices = if attrs.choices.is_empty() {
            quote!(&[])
        } else {
            let cs = attrs.choices.iter().map(|c| lit_str(c));
            quote!(&[#(#cs),*])
        };
        let skip = attrs.skip;
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
            ::argtuner_sdk::ParamHint {
                name: #name_lit,
                long: #long_lit,
                value_name: #value_name,
                default: #default_tok,
                help: #help_tok,
                kind: #kind,
                skip: #skip,
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
        let default_arg = match attrs.default.as_deref() {
            Some(d) => quote!(.default_value(#d)),
            None => quote!(),
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

        impl ::argtuner_sdk::Params for #struct_ident {
            fn app_name() -> &'static str {
                env!("CARGO_PKG_NAME")
            }

            fn params() -> &'static [::argtuner_sdk::ParamHint] {
                static PARAMS: [::argtuner_sdk::ParamHint; #field_count] = [
                    #(#param_specs),*
                ];
                &PARAMS
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
    default: Option<String>,
    long: Option<String>,
    value_name: Option<String>,
    min: Option<f64>,
    max: Option<f64>,
    log: bool,
    step: Option<f64>,
    choices: Vec<String>,
    skip: bool,
    parent: Option<String>,
    parent_values: Vec<String>,
}
fn parse_param_attrs(field: &syn::Field) -> ParamAttrs {
    let mut out = ParamAttrs {
        default: None,
        long: None,
        value_name: None,
        min: None,
        max: None,
        log: false,
        step: None,
        choices: Vec::new(),
        skip: false,
        parent: None,
        parent_values: Vec::new(),
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
                "default" => out.default = expr_string(value),
                "long" => out.long = expr_string(value),
                "value_name" => out.value_name = expr_string(value),
                "min" => out.min = expr_f64(value),
                "max" => out.max = expr_f64(value),
                "step" => out.step = expr_f64(value),
                "log" => out.log = expr_bool(value),
                "choices" => out.choices = expr_string_array(value),
                "skip" => out.skip = expr_bool(value),
                "parent" => out.parent = expr_string(value),
                "parent_values" => out.parent_values = expr_string_array(value),
                _ => {}
            }
        }
    }
    out
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

fn f64_tok(v: Option<f64>) -> proc_macro2::TokenStream {
    match v {
        Some(x) => {
            let lit = x;
            quote!(Some(#lit))
        }
        None => quote!(None),
    }
}
