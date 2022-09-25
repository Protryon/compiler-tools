use std::collections::{BTreeMap, HashSet};

use indexmap::IndexMap;
use proc_macro::TokenStream;
use proc_macro2::{Delimiter, Ident, TokenStream as TokenStream2, TokenTree};
use quote::{format_ident, quote, quote_spanned, ToTokens, TokenStreamExt};
use regex::Regex;
use syn::{parse_macro_input, spanned::Spanned, DeriveInput, Expr, ExprLit, ExprPath, Fields, FieldsUnnamed, Lifetime, Lit, Type};

use crate::{gen::class_match::gen_class_match, lit_table::LitTable, simple_regex::SimpleRegex};

mod gen;
mod lit_table;
mod simple_regex;

#[proc_macro_attribute]
pub fn token_parse(_metadata: TokenStream, input: TokenStream) -> TokenStream {
    let ast = parse_macro_input!(input as DeriveInput);
    impl_token_parse(&ast).into()
}

fn flatten<S: ToTokens, T: IntoIterator<Item = S>>(iter: T) -> TokenStream2 {
    let mut out = quote! {};
    out.append_all(iter);
    out
}

struct TokenParseData {
    has_target: bool,
    target_needs_parse: bool,
    is_illegal: bool,
    literals: Vec<String>,
    simple_regexes: Vec<String>,
    regexes: Vec<String>,
    parse_fn: Option<String>,
    ident: Ident,
}

fn parse_attributes(input: TokenStream2) -> Option<IndexMap<String, Option<String>>> {
    let mut tokens = input.into_iter().peekable();
    let mut tokens = if let Some(TokenTree::Group(group)) = tokens.peek() {
        if group.delimiter() == Delimiter::Parenthesis {
            group.stream().into_iter()
        } else {
            return None;
        }
    } else {
        return None;
    };
    let mut attributes = IndexMap::<String, Option<String>>::new();

    loop {
        let name = match tokens.next() {
            None => break,
            Some(TokenTree::Ident(ident)) => ident,
            _ => return None,
        };
        match tokens.next() {
            None => {
                attributes.insert(name.to_string(), None);
                break;
            }
            Some(TokenTree::Punct(p)) if p.as_char() == ',' => {
                attributes.insert(name.to_string(), None);
            }
            Some(TokenTree::Punct(p)) if p.as_char() == '=' => {
                let value = if let TokenTree::Literal(literal) = tokens.next()? {
                    let lit = Lit::new(literal);
                    Some(match lit {
                        Lit::Str(s) => s.value(),
                        Lit::Char(c) => c.value().to_string(),
                        _ => return None,
                    })
                } else {
                    return None;
                };
                attributes.insert(name.to_string(), value);
            }
            _ => return None,
        }
    }
    Some(attributes)
}

fn construct_variant(item: &TokenParseData, enum_ident: &Ident) -> TokenStream2 {
    let variant = &item.ident;
    if item.has_target {
        if item.target_needs_parse {
            //TODO: emit better error for parsefail
            quote! {
                #enum_ident::#variant(passed.parse().ok()?)
            }
        } else {
            quote! {
                #enum_ident::#variant(passed)
            }
        }
    } else {
        quote! {
            #enum_ident::#variant
        }
    }
}

fn impl_token_parse(input: &DeriveInput) -> proc_macro2::TokenStream {
    if input.generics.params.len() > 1 || !matches!(input.generics.params.first(), None | Some(syn::GenericParam::Lifetime(_))) {
        return quote_spanned! {
            input.generics.span() =>
            compile_error!("TokenParse can only have a single lifetime type parameter");
        };
    }
    let has_lifetime_param = input.generics.params.len() == 1;
    let original_lifetime_param = if let Some(syn::GenericParam::Lifetime(lifetime)) = input.generics.params.first() {
        Some(lifetime.lifetime.ident.clone())
    } else {
        None
    };

    let items = match &input.data {
        syn::Data::Enum(items) => items,
        _ => {
            return quote_spanned! {
                input.span() =>
                compile_error!("TokenParse can only be derived on enums");
            }
        }
    };

    let mut tokens_to_parse = vec![];
    let mut has_illegal = false;
    for variant in &items.variants {
        let mut parse_data = TokenParseData {
            has_target: false,
            target_needs_parse: false,
            is_illegal: false,
            literals: vec![],
            simple_regexes: vec![],
            regexes: vec![],
            parse_fn: None,
            ident: variant.ident.clone(),
        };

        for attribute in &variant.attrs {
            if attribute.path.segments.len() != 1 || attribute.path.segments.first().unwrap().ident != "token" {
                continue;
            }
            let attributes = match parse_attributes(attribute.tokens.clone()) {
                Some(x) => x,
                None => {
                    return quote_spanned! {
                        attribute.span() =>
                        compile_error!("invalid attribute syntax");
                    }
                }
            };
            for (name, value) in attributes {
                if name != "illegal" && value.is_none() {
                    return quote_spanned! {
                        attribute.span() =>
                        compile_error!("missing attribute value");
                    };
                }

                match &*name {
                    "literal" => {
                        parse_data.literals.push(value.unwrap());
                    }
                    "regex" => {
                        parse_data.simple_regexes.push(value.unwrap());
                    }
                    "regex_full" => {
                        parse_data.regexes.push(value.unwrap());
                    }
                    "parse_fn" => {
                        if parse_data.parse_fn.is_some() {
                            return quote_spanned! {
                                attribute.span() =>
                                compile_error!("redefined 'parse_fn' attribute");
                            };
                        }
                        parse_data.parse_fn = Some(value.unwrap());
                    }
                    "illegal" => {
                        if value.is_some() {
                            return quote_spanned! {
                                attribute.span() =>
                                compile_error!("unexpected attribute value");
                            };
                        }
                        if parse_data.is_illegal || has_illegal {
                            return quote_spanned! {
                                attribute.span() =>
                                compile_error!("redefined 'illegal' attribute");
                            };
                        }
                        parse_data.is_illegal = true;
                        has_illegal = true;
                    }
                    _ => {
                        return quote_spanned! {
                            attribute.span() =>
                            compile_error!("unknown attribute");
                        }
                    }
                }
            }
        }
        if let Some((_, discriminant)) = &variant.discriminant {
            if let Expr::Lit(ExprLit {
                lit: Lit::Str(lit_str),
                ..
            }) = discriminant
            {
                parse_data.literals.push(lit_str.value());
            } else {
                return quote_spanned! {
                    input.span() =>
                    compile_error!("TokenParse enums cannot have non-string discriminants");
                };
            }
        }
        if parse_data.parse_fn.is_some() && (!parse_data.literals.is_empty() || !parse_data.simple_regexes.is_empty() || !parse_data.regexes.is_empty()) {
            return quote_spanned! {
                input.span() =>
                compile_error!("cannot have a 'parse_fn' attribute and a 'literal', 'regex', or 'regex_full' attribute");
            };
        }
        let has_anything =
            parse_data.parse_fn.is_some() || !parse_data.literals.is_empty() || !parse_data.simple_regexes.is_empty() || !parse_data.regexes.is_empty();
        if parse_data.is_illegal && has_anything {
            return quote_spanned! {
                input.span() =>
                compile_error!("cannot have an 'illegal' attribute and a 'literal', 'regex', 'regex_full', or 'parse_fn' attribute");
            };
        } else if !parse_data.is_illegal && !has_anything {
            return quote_spanned! {
                input.span() =>
                compile_error!("must have an enum discriminant or 'illegal', 'literal', 'regex', 'regex_full', or 'parse_fn' attribute");
            };
        }

        match &variant.fields {
            Fields::Named(_) => {
                return quote_spanned! {
                    variant.fields.span() =>
                    compile_error!("cannot have enum struct in TokenParse variant");
                };
            }
            Fields::Unnamed(FieldsUnnamed {
                unnamed,
                ..
            }) => {
                if unnamed.len() != 1 {
                    return quote_spanned! {
                        unnamed.span() =>
                        compile_error!("must have single target type in TokenParse variant");
                    };
                }
                let field = unnamed.first().unwrap();
                match &field.ty {
                    Type::Reference(ty) => {
                        if ty.mutability.is_some() {
                            return quote_spanned! {
                                unnamed.span() =>
                                compile_error!("cannot have `&mut` in TokenParse variant");
                            };
                        }
                        if !matches!(&ty.lifetime, Some(Lifetime { ident, ..}) if Some(ident) == original_lifetime_param.as_ref()) {
                            return quote_spanned! {
                                unnamed.span() =>
                                compile_error!("unexpected lifetime in TokenParse variant (use the same one as defined in enum declaration)");
                            };
                        }
                        if let Type::Path(path) = &*ty.elem {
                            if path.qself.is_some() || path.path.segments.len() != 1 || path.path.segments.first().unwrap().ident != "str" {
                                return quote_spanned! {
                                    unnamed.span() =>
                                    compile_error!("invalid type in reference for TokenParse (only &str allowed)");
                                };
                            }
                        } else {
                            return quote_spanned! {
                                unnamed.span() =>
                                compile_error!("invalid type in reference for TokenParse (only &str allowed)");
                            };
                        }
                        parse_data.has_target = true;
                    }
                    _ => {
                        parse_data.has_target = true;
                        parse_data.target_needs_parse = true;
                    }
                }
            }
            // no target
            Fields::Unit => {
                if parse_data.is_illegal {
                    return quote_spanned! {
                        variant.span() =>
                        compile_error!("'illegal' attributed tokens must have a single field (usually 'char' or '&str')");
                    };
                }
            }
        }

        tokens_to_parse.push(parse_data)
    }

    let mut simple_regexes = BTreeMap::new();
    for item in tokens_to_parse.iter() {
        for simple_regex in &item.simple_regexes {
            let parsed = match SimpleRegex::parse(simple_regex) {
                Some(x) => x,
                None => {
                    return quote_spanned! {
                        item.ident.span() =>
                        compile_error!("invalid simple regex");
                    }
                }
            };
            simple_regexes.insert((item.ident.clone(), simple_regex.clone()), parsed);
        }
    }

    let mut regexes = BTreeMap::new();
    for item in tokens_to_parse.iter() {
        for regex in &item.regexes {
            let modified_regex = format!("^{}", regex);
            let parsed = match Regex::new(&*modified_regex) {
                Ok(x) => x,
                Err(_) => {
                    return quote_spanned! {
                        item.ident.span() =>
                        compile_error!("invalid simple regex");
                    }
                }
            };
            regexes.insert((item.ident.clone(), regex.clone()), parsed);
        }
    }

    // (regex ident, raw regex) => (literal ident, literal)
    let mut simple_regex_ident_conflicts: BTreeMap<(Ident, String), Vec<(Ident, String)>> = BTreeMap::new();
    let mut regex_ident_conflicts: BTreeMap<(Ident, String), Vec<(Ident, String)>> = BTreeMap::new();
    let mut known_literals = HashSet::new();

    let mut lit_table = LitTable::default();
    for item in tokens_to_parse.iter() {
        for literal in &item.literals {
            if !known_literals.insert(literal.clone()) {
                return quote_spanned! {
                    item.ident.span() =>
                    compile_error!("conflicting literals");
                };
            }
            let mut any_matched = false;
            for ((ident, raw_regex), regex) in &simple_regexes {
                if regex.matches(&**literal) {
                    simple_regex_ident_conflicts
                        .entry((ident.clone(), raw_regex.clone()))
                        .or_default()
                        .push((item.ident.clone(), literal.clone()));
                    any_matched = true;
                }
            }
            if any_matched {
                continue;
            }
            for ((ident, raw_regex), regex) in &regexes {
                if regex.is_match(&**literal) {
                    regex_ident_conflicts
                        .entry((ident.clone(), raw_regex.clone()))
                        .or_default()
                        .push((item.ident.clone(), literal.clone()));
                    any_matched = true;
                }
            }
            if any_matched {
                continue;
            }
            lit_table.push(item, &**literal, &mut literal.chars());
        }
    }

    let lit_table_name = format_ident!("parse_lits");
    let lit_table = lit_table.emit(&lit_table_name, &input.ident);

    let (simple_regex_fns, simple_regex_calls) =
        match gen::simple_regex::gen_simple_regex(&tokens_to_parse[..], &simple_regexes, &simple_regex_ident_conflicts, &input.ident) {
            Ok(x) => x,
            Err(e) => return e,
        };
    let (regex_fns, regex_calls) = match gen::full_regex::gen_full_regex(&tokens_to_parse[..], &regex_ident_conflicts, &input.ident) {
        Ok(x) => x,
        Err(e) => return e,
    };

    let lifetime_param = if has_lifetime_param {
        quote! { <'a> }
    } else {
        quote! {}
    };
    let ident_raw = input.ident.to_string();
    let tokenizer_ident = if ident_raw.contains("Token") {
        format_ident!("{}", ident_raw.replace("Token", "Tokenizer"))
    } else {
        format_ident!("{}Tokenizer", ident_raw)
    };
    let token_ident = &input.ident;
    let vis = &input.vis;

    let display_fields = gen::display::gen_display(&tokens_to_parse[..], &input.ident);

    let illegal_emission = if let Some(illegal) = tokens_to_parse.iter().find(|x| x.is_illegal) {
        let constructor = construct_variant(illegal, &input.ident);
        quote! {
            if let Some(value) = self.inner.chars().next() {
                let span = ::compiler_tools::Span {
                    line_start: self.line,
                    col_start: self.col,
                    line_stop: if value == '\n' {
                        self.line
                    } else {
                        self.line += 1;
                        self.line
                    },
                    col_stop: if value != '\n' {
                        self.col += value.len_utf8() as u64;
                        self.col
                    } else {
                        self.col = 0;
                        self.col
                    },
                };
                let passed = &self.inner[..value.len_utf8()];
                self.inner = &self.inner[value.len_utf8()..];
                return Some(::compiler_tools::Spanned {
                    token: #constructor,
                    span,
                })
            } else {
                None
            }
        }
    } else {
        quote! {
            None
        }
    };

    let reinput = {
        let attrs = flatten(&input.attrs);
        let vis = &input.vis;
        let ident = &input.ident;
        let generics = &input.generics;
        let mut variants = vec![];
        for variant in &items.variants {
            let attrs = flatten(
                variant
                    .attrs
                    .iter()
                    .filter(|a| a.path.segments.len() != 1 || a.path.segments.first().unwrap().ident != "token"),
            );
            let ident = &variant.ident;
            let fields = &variant.fields;
            // discriminant ignored
            variants.push(quote! {
                #attrs
                #ident #fields,
            });
        }
        let variants = flatten(variants);
        quote! {
            #attrs
            #vis enum #ident #generics {
                #variants
            }
        }
    };

    let class_matches = gen_class_match(&tokens_to_parse[..], &input.ident);

    let mut custom_parse_fns: Vec<TokenStream2> = vec![];
    for token in &tokens_to_parse {
        if let Some(parse_fn) = &token.parse_fn {
            let path_expr: ExprPath = match syn::parse_str(&parse_fn) {
                Ok(x) => x,
                Err(_e) => {
                    custom_parse_fns.push(quote! { compile_error!("can't parse path for parse_fn"); });
                    continue;
                }
            };
            let constructed = construct_variant(token, &input.ident);
            custom_parse_fns.push(quote! {
                {
                    if let Some((passed, remaining)) = #path_expr(self.inner) {
                        let span = ::compiler_tools::Span {
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
                        };
                        self.inner = remaining;
                        match passed {
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
    let custom_parse_fns = flatten(custom_parse_fns);

    quote! {
        #reinput

        impl #lifetime_param ::core::fmt::Display for #token_ident #lifetime_param {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                match self {
                    #display_fields
                }
            }
        }

        impl #lifetime_param ::compiler_tools::TokenExt for #token_ident #lifetime_param {
            fn matches_class(&self, other: &Self) -> bool {
                match (self, other) {
                    #class_matches
                }
            }
        }

        #vis struct #tokenizer_ident<'a> {
            line: u64,
            col: u64,
            inner: &'a str,
        }

        impl<'a> #tokenizer_ident<'a> {
            pub fn new(input: &'a str) -> Self {
                Self {
                    line: 0,
                    col: 0,
                    inner: input,
                }
            }
        }

        impl<'a> ::compiler_tools::TokenParse<'a> for #tokenizer_ident<'a> {
            type Token = #token_ident #lifetime_param;

            #[allow(non_snake_case, unreachable_pattern, unreachable_code)]
            fn next(&mut self) -> Option<::compiler_tools::Spanned<Self::Token>> {
                #lit_table
                #simple_regex_fns
                #regex_fns

                match #lit_table_name(self.inner) {
                    Some((token, remaining, newlines)) => {
                        let span = ::compiler_tools::Span {
                            line_start: self.line,
                            col_start: self.col,
                            line_stop: if newlines == 0 {
                                self.line
                            } else {
                                self.line += newlines;
                                self.line
                            },
                            col_stop: if newlines == 0 {
                                self.col += (self.inner.len() - remaining.len()) as u64;
                                self.col
                            } else {
                                //todo: handle utf8 better with newline seeking here
                                let newline_offset = self.inner[..self.inner.len() - remaining.len()].as_bytes().iter().rev().position(|x| *x == b'\n').expect("malformed newline state");
                                let newline_offset = (self.inner.len() - remaining.len()) - newline_offset;
                                self.col = (newline_offset as u64).saturating_sub(1);
                                self.col
                            },
                        };
                        self.inner = remaining;
                        return Some(::compiler_tools::Spanned {
                            token,
                            span,
                        });
                    },
                    None => (),
                }
                #simple_regex_calls
                #regex_calls
                #custom_parse_fns
                #illegal_emission
            }
        }
    }
}
