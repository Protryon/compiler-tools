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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::Span;

    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Tok {
        A,
        B,
        Ws,
    }

    impl TokenExt for Tok {
        fn matches_class(&self, other: &Self) -> bool {
            self == other
        }
    }

    struct VecTokenizer {
        tokens: std::vec::IntoIter<Tok>,
    }

    impl<'a> TokenParse<'a> for VecTokenizer {
        type Token = Tok;

        fn next(&mut self) -> Option<Spanned<Tok>> {
            self.tokens.next().map(|token| Spanned {
                token,
                span: Span::default(),
            })
        }
    }

    fn wrap(tokens: Vec<Tok>, ignore: Vec<Tok>) -> TokenizerWrap<'static, VecTokenizer> {
        TokenizerWrap::new(
            VecTokenizer {
                tokens: tokens.into_iter(),
            },
            ignore,
        )
    }

    fn val(spanned: Option<Spanned<Tok>>) -> Option<Tok> {
        spanned.map(|s| *s)
    }

    #[test]
    fn next_skips_ignored_tokens() {
        let mut w = wrap(vec![Tok::A, Tok::Ws, Tok::Ws, Tok::B, Tok::Ws], vec![Tok::Ws]);
        assert_eq!(val(w.next()), Some(Tok::A));
        assert_eq!(val(w.next()), Some(Tok::B));
        assert_eq!(val(w.next()), None);
    }

    #[test]
    fn peek_does_not_consume() {
        let mut w = wrap(vec![Tok::A, Tok::B], vec![]);
        assert_eq!(val(w.peek().copied()), Some(Tok::A));
        assert_eq!(val(w.peek().copied()), Some(Tok::A));
        assert_eq!(val(w.next()), Some(Tok::A));
        assert_eq!(val(w.next()), Some(Tok::B));
        assert_eq!(val(w.peek().copied()), None);
    }

    #[test]
    fn eat_consumes_on_match_and_restores_on_miss() {
        let mut w = wrap(vec![Tok::A, Tok::B], vec![]);
        assert_eq!(val(w.eat(Tok::A)), Some(Tok::A));
        // next token is B, so eating A fails and B is put back
        assert_eq!(val(w.eat(Tok::A)), None);
        assert_eq!(val(w.eat(Tok::B)), Some(Tok::B));
        assert_eq!(val(w.next()), None);
    }

    #[test]
    fn eat_any_matches_any_listed_class() {
        let mut w = wrap(vec![Tok::A, Tok::B], vec![]);
        assert_eq!(val(w.eat_any(&[Tok::B, Tok::A])), Some(Tok::A));
        // next is B, not in the list, so it is restored
        assert_eq!(val(w.eat_any(&[Tok::A])), None);
        assert_eq!(val(w.eat_any(&[Tok::B])), Some(Tok::B));
    }

    #[test]
    fn eat_skips_ignored_tokens() {
        let mut w = wrap(vec![Tok::Ws, Tok::Ws, Tok::A], vec![Tok::Ws]);
        assert_eq!(val(w.eat(Tok::A)), Some(Tok::A));
    }

    #[test]
    fn peek_then_eat_returns_same_token() {
        let mut w = wrap(vec![Tok::A], vec![]);
        assert_eq!(val(w.peek().copied()), Some(Tok::A));
        assert_eq!(val(w.eat(Tok::A)), Some(Tok::A));
        assert_eq!(val(w.peek().copied()), None);
    }
}
