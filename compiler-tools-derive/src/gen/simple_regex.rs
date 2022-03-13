use std::collections::BTreeMap;

use proc_macro2::{TokenStream, Ident};
use quote::{quote, format_ident};

use crate::{simple_regex::SimpleRegex, TokenParseData, construct_variant, flatten};

pub(crate) fn gen_simple_regex(tokens_to_parse: &[TokenParseData], parsed: &BTreeMap<(Ident, String), SimpleRegex>, conflicts: &BTreeMap<(Ident, String), Vec<(Ident, String)>>, enum_ident: &Ident) -> Result<(TokenStream, TokenStream), TokenStream> {
    let mut simple_regex_fns = vec![];
    let mut simple_regex_calls = vec![];
    for item in tokens_to_parse.iter() {
        for simple_regex in &item.simple_regexes {
            let key = (item.ident.clone(), simple_regex.clone());
            let parsed = parsed.get(&key).unwrap();
            let fn_ident = format_ident!("parse_sr_{}", item.ident);
            let parse_fn = parsed.generate_parser(fn_ident.clone());
            simple_regex_fns.push(parse_fn);

            let constructed = construct_variant(item, enum_ident);

            let span = if parsed.could_capture_newline() {
                quote! {
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
                }
            } else {
                quote! {
                    ::compiler_tools::Span {
                        line_start: self.line,
                        col_start: self.col,
                        line_stop: self.line,
                        col_stop: {
                            self.col += passed.len() as u64;
                            self.col
                        },
                    }
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

            simple_regex_calls.push(quote! {
                {
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
    Ok((flatten(simple_regex_fns), flatten(simple_regex_calls)))
}