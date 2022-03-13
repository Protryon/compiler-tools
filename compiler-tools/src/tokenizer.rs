use crate::span::Spanned;

pub trait TokenParse<'a> {
    type Token: Clone + Copy + 'a;

    fn next(&mut self) -> Option<Spanned<Self::Token>>;
}
