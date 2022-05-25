use proc_macro2::{Ident, TokenStream};
use quote::quote;

use crate::{flatten, TokenParseData};

pub(crate) fn gen_class_match(tokens_to_parse: &[TokenParseData], enum_ident: &Ident) -> TokenStream {
    let mut matches = vec![];
    for info in tokens_to_parse {
        let ident = &info.ident;

        if info.has_target {
            //todo: what to do with its a parsed target that doesn't impl Display?
            matches.push(quote! {
                (#enum_ident::#ident(_), #enum_ident::#ident(_)) => true,
            })
        } else {
            matches.push(quote! {
                (#enum_ident::#ident, #enum_ident::#ident) => true,
            })
        }
    }
    matches.push(quote! {
        _ => false,
    });
    flatten(matches)
}
