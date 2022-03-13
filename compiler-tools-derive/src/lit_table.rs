use std::collections::BTreeMap;

use proc_macro2::{Ident, TokenStream};
use quote::quote;

use crate::{flatten, TokenParseData};

#[derive(Default)]
pub(super) struct LitTable {
    table: BTreeMap<char, LitTable>,
    token: Option<LitTableToken>,
}

pub(super) struct LitTableToken {
    ident: Ident,
    has_target: bool,
    target_needs_parse: bool,
    literal: String,
}

impl LitTableToken {
    fn emit(&self, enum_ident: &Ident) -> TokenStream {
        let variant = &self.ident;
        let len = self.literal.len();
        let newlines = self.literal.chars().filter(|c| *c == '\n').count() as u64;

        if self.has_target {
            if self.target_needs_parse {
                //TODO: emit better error for parsefail
                quote! {
                    {
                        let (before, after) = from.split_at(#len);
                        from = after;
                        (#enum_ident::#variant(before.parse().ok()?), #newlines)
                    }
                }
            } else {
                quote! {
                    {
                        let (before, after) = from.split_at(#len);
                        from = after;
                        (#enum_ident::#variant(before), #newlines)
                    }
                }
            }
        } else {
            quote! {
                {
                    from = &from[#len..];
                    (#enum_ident::#variant, #newlines)
                }
            }
        }
    }
}

impl LitTable {
    pub(super) fn push(&mut self, item: &TokenParseData, literal: &str, remaining: &mut impl Iterator<Item = char>) {
        match remaining.next() {
            Some(c) => {
                let entry = self.table.entry(c).or_default();
                entry.push(item, literal, remaining);
            }
            None => {
                self.token = Some(LitTableToken {
                    ident: item.ident.clone(),
                    has_target: item.has_target,
                    target_needs_parse: item.target_needs_parse,
                    literal: literal.to_string(),
                })
            }
        }
    }

    fn emit_internal(&self, enum_ident: &Ident, pending_default: Option<&LitTableToken>) -> TokenStream {
        if self.table.is_empty() {
            if let Some(token) = &self.token {
                let emitted = token.emit(enum_ident);
                quote! {
                    Some(#emitted)
                }
            } else {
                quote! {
                    None
                }
            }
        } else {
            let default = self.token.as_ref().or(pending_default);
            let mut entries = vec![];
            for (c, table) in &self.table {
                let internal = table.emit_internal(enum_ident, default);
                entries.push(quote! {
                    Some(#c) => #internal,
                });
            }
            if let Some(token) = default {
                let emitted = token.emit(enum_ident);
                entries.push(quote! {
                    _ => Some(#emitted),
                });
            } else {
                entries.push(quote! {
                    _ => None,
                });
            }
            let entries = flatten(entries);

            quote! {
                match iter.next() {
                    #entries
                }
            }
        }
    }

    //TODO: straightshot optimization
    pub(super) fn emit(&self, fn_name: &Ident, enum_ident: &Ident) -> TokenStream {
        let internal = self.emit_internal(&enum_ident, None);
        // println!("{}", internal);
        quote! {
            // returns (token, remaining, newlines_skipped)
            #[inline]
            fn #fn_name(mut from: &str) -> Option<(#enum_ident, &str, u64)> {
                let start = from;
                let mut iter = from.chars();
                let (token, newlines) = #internal?;
                Some((token, from, newlines))
            }
        }
    }
}
