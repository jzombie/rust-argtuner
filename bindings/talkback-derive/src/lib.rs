use proc_macro::TokenStream;
use quote::quote;
use syn::parse::Parser;
use syn::{Fields, ItemStruct, parse_macro_input};

#[proc_macro_attribute]
pub fn talkback_args(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(item as ItemStruct);

    let fields = match &mut input.fields {
        Fields::Named(fields) => fields,
        _ => {
            return syn::Error::new_spanned(
                input,
                "talkback_args only supports structs with named fields",
            )
            .to_compile_error()
            .into();
        }
    };

    let has_print_template = fields.named.iter().any(|field| {
        field
            .ident
            .as_ref()
            .is_some_and(|ident| ident == "print_template")
    });
    if !has_print_template {
        fields.named.push(
            syn::Field::parse_named
                .parse2(quote! {
                    #[arg(long, help = "Print command template and exit")]
                    print_template: bool
                })
                .expect("parse injected field"),
        );
    }

    let has_print_template_toml = fields.named.iter().any(|field| {
        field
            .ident
            .as_ref()
            .is_some_and(|ident| ident == "print_template_toml")
    });
    if !has_print_template_toml {
        fields.named.push(
            syn::Field::parse_named
                .parse2(quote! {
                    #[arg(long, help = "Print a starter argtuner.toml and exit")]
                    print_template_toml: bool
                })
                .expect("parse injected field"),
        );
    }

    TokenStream::from(quote! { #input })
}
