use std::collections::BTreeMap;

use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};

use crate::{construct_variant, flatten, TokenParseData};

pub(crate) fn gen_full_regex(
    tokens_to_parse: &[TokenParseData],
    conflicts: &BTreeMap<(Ident, String), Vec<(Ident, String)>>,
    enum_ident: &Ident,
    parse_fns: &mut BTreeMap<usize, Vec<TokenStream>>,
) -> Result<(), TokenStream> {
    for (token_index, item) in tokens_to_parse.iter().enumerate() {
        for regex in &item.regexes {
            let key = (item.ident.clone(), regex.clone());
            let regex = format!("\\A(?:{})", regex);

            let fn_ident = format_ident!("parse_r_{}", item.ident);
            let regex_fn = quote! {
                fn #fn_ident(from: &str) -> Option<(&str, &str)> {
                    static REGEX: ::compiler_tools::once_cell::sync::OnceCell<::compiler_tools::regex::Regex> = ::compiler_tools::once_cell::sync::OnceCell::new();
                    let regex = REGEX.get_or_init(|| ::compiler_tools::regex::Regex::new(#regex).unwrap());
                    if let Some(matching) = regex.find(from) {
                        assert_eq!(matching.start(), 0);
                        Some((&from[..matching.end()], &from[matching.end()..]))
                    } else {
                        None
                    }
                }
            };

            let constructed = construct_variant(item, enum_ident);

            let span = quote! {
                ::compiler_tools::Span {
                    line_start: self.line,
                    col_start: self.col,
                    line_stop: {
                        self.line += passed.chars().filter(|x| *x == '\n').count() as u64;
                        self.line
                    },
                    //todo: handle utf8 better with newline seeking here
                    col_stop: if let Some(newline_offset) = passed.as_bytes().iter().rev().position(|x| *x == b'\n') {
                        let newline_offset = passed.len() - newline_offset;
                        self.col = (newline_offset as u64).saturating_sub(1);
                        self.col
                    } else {
                        self.col += passed.len() as u64;
                        self.col
                    },
                }
            };

            let conflicts = conflicts.get(&key).cloned().unwrap_or_default();
            let mut conflict_resolutions = vec![];
            for (ident, literal) in conflicts {
                let subitem = tokens_to_parse.iter().find(|x| x.ident == ident).expect("missing subitem");
                let constructed = construct_variant(subitem, enum_ident);

                conflict_resolutions.push(quote! {
                    #literal => return Some(::compiler_tools::Spanned {
                        token: #constructed,
                        span,
                    }),
                })
            }
            let conflict_resolutions = flatten(conflict_resolutions);

            parse_fns.entry(token_index).or_default().push(quote! {
                {
                    #regex_fn
                    if let Some((passed, remaining)) = #fn_ident(self.inner) {
                        let span = #span;
                        self.inner = remaining;
                        match passed {
                            #conflict_resolutions
                            passed => return Some(::compiler_tools::Spanned {
                                token: #constructed,
                                span,
                            }),
                        }
                    }
                }
            });
        }
    }
    Ok(())
}
