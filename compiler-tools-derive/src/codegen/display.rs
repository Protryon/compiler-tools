use proc_macro2::{Ident, TokenStream};
use quote::quote;

use crate::{flatten, TokenParseData};

pub(crate) fn gen_display(tokens_to_parse: &[TokenParseData], enum_ident: &Ident) -> TokenStream {
    let mut display_fields = vec![];
    for info in tokens_to_parse {
        let ident = &info.ident;

        if info.has_target {
            //todo: what to do with its a parsed target that doesn't impl Display?
            display_fields.push(quote! {
                #enum_ident::#ident(x) => write!(f, "{}", x),
            })
        } else if !info.literals.is_empty() {
            let target = info.literals.first().unwrap().replace("\n", "\\n");
            display_fields.push(quote! {
                #enum_ident::#ident => write!(f, "{}", #target),
            })
        } else {
            let ident_str = format!("{}", ident);
            display_fields.push(quote! {
                #enum_ident::#ident => write!(f, "{}", #ident_str),
            })
        }
    }
    flatten(display_fields)
}
