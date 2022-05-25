use std::marker::PhantomData;

use crate::span::Spanned;

pub trait TokenExt: Clone + Copy + PartialEq {
    fn matches_class(&self, other: &Self) -> bool;
}

pub trait TokenParse<'a> {
    type Token: TokenExt + 'a;

    fn next(&mut self) -> Option<Spanned<Self::Token>>;
}

pub struct TokenizerWrap<'a, T: TokenParse<'a>> {
    inner: T,
    peeked: Option<Spanned<T::Token>>,
    tokens_to_ignore: Vec<T::Token>,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a, T: TokenParse<'a>> TokenizerWrap<'a, T> {
    pub fn new(inner: T, tokens_to_ignore: impl IntoIterator<Item = T::Token>) -> Self {
        Self {
            inner,
            tokens_to_ignore: tokens_to_ignore.into_iter().collect(),
            peeked: None,
            _lifetime: PhantomData,
        }
    }

    pub fn next(&mut self) -> Option<Spanned<T::Token>> {
        if let Some(peeked) = self.peeked.take() {
            Some(peeked)
        } else {
            loop {
                let next = self.inner.next()?;
                if self.tokens_to_ignore.iter().all(|x| !x.matches_class(&*next)) {
                    break Some(next);
                }
            }
        }
    }

    pub fn peek(&mut self) -> Option<&Spanned<T::Token>> {
        if self.peeked.is_none() {
            self.peeked = self.next();
        }
        self.peeked.as_ref()
    }

    pub fn eat(&mut self, token: T::Token) -> Option<Spanned<T::Token>> {
        let next = self.next()?;
        if next.matches_class(&token) {
            Some(next)
        } else {
            self.peeked = Some(next);
            None
        }
    }

    pub fn eat_any(&mut self, tokens: &[T::Token]) -> Option<Spanned<T::Token>> {
        let next = self.next()?;
        for token in tokens {
            if next.matches_class(token) {
                return Some(next);
            }
        }
        self.peeked = Some(next);
        None
    }
}
