use std::{
    fmt::{self, Debug, Display},
    ops::{Deref, DerefMut},
};

#[derive(Clone, Debug, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Span {
    pub line_start: u64,
    pub line_stop: u64,
    pub col_start: u64,
    pub col_stop: u64,
}

impl PartialEq for Span {
    fn eq(&self, _other: &Span) -> bool {
        true
    }
}

impl std::hash::Hash for Span {
    fn hash<H: std::hash::Hasher>(&self, _state: &mut H) {}
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line_start == self.line_stop {
            write!(f, "{}:{}-{}", self.line_start, self.col_start, self.col_stop)
        } else {
            write!(f, "{}:{}-{}:{}", self.line_start, self.col_start, self.line_stop, self.col_stop)
        }
    }
}

impl std::ops::Add for Span {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        // Combine into the smallest span that contains both. Positions are
        // compared as (line, col) pairs so the earliest start and latest stop win,
        // which is correct regardless of operand order or how many lines are spanned.
        let (line_start, col_start) = (self.line_start, self.col_start).min((other.line_start, other.col_start));
        let (line_stop, col_stop) = (self.line_stop, self.col_stop).max((other.line_stop, other.col_stop));
        Span {
            line_start,
            line_stop,
            col_start,
            col_stop,
        }
    }
}

impl std::ops::AddAssign for Span {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy)]
pub struct Spanned<T: Clone + Copy> {
    pub token: T,
    pub span: Span,
}

impl<T: Clone + Copy> Deref for Spanned<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.token
    }
}

impl<T: Clone + Copy> DerefMut for Spanned<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.token
    }
}

impl<T: Clone + Copy + Display> Display for Spanned<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "'{}' @ {}", self.token, self.span)
    }
}

impl<T: Clone + Copy + Debug> Debug for Spanned<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "'{:?}' @ {}", self.token, self.span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(line_start: u64, col_start: u64, line_stop: u64, col_stop: u64) -> Span {
        Span {
            line_start,
            line_stop,
            col_start,
            col_stop,
        }
    }

    #[test]
    fn span_partial_eq_ignores_position() {
        // Spans always compare equal so that tokens compare on their value, not location.
        assert_eq!(span(0, 0, 0, 1), span(9, 4, 12, 7));
    }

    #[test]
    fn span_display_single_line() {
        assert_eq!(span(3, 5, 3, 9).to_string(), "3:5-9");
    }

    #[test]
    fn span_display_multi_line() {
        assert_eq!(span(3, 5, 7, 2).to_string(), "3:5-7:2");
    }

    #[test]
    fn span_add_same_line() {
        let combined = span(0, 0, 0, 3) + span(0, 5, 0, 8);
        assert_eq!((combined.line_start, combined.col_start), (0, 0));
        assert_eq!((combined.line_stop, combined.col_stop), (0, 8));
    }

    #[test]
    fn span_add_is_commutative() {
        let a = span(1, 2, 3, 4);
        let b = span(5, 0, 8, 9);
        let ab = a + b;
        let ba = b + a;
        assert_eq!((ab.line_start, ab.col_start), (ba.line_start, ba.col_start));
        assert_eq!((ab.line_stop, ab.col_stop), (ba.line_stop, ba.col_stop));
    }

    #[test]
    fn span_add_spans_all_lines() {
        // Regression: an earlier implementation could drop the earliest lines when
        // one span's start line equaled the other's stop line.
        let later = span(3, 2, 5, 4);
        let earlier = span(1, 0, 3, 7);
        let combined = later + earlier;
        assert_eq!(combined.line_start, 1);
        assert_eq!(combined.col_start, 0);
        assert_eq!(combined.line_stop, 5);
        assert_eq!(combined.col_stop, 4);
    }

    #[test]
    fn span_add_contained() {
        // A span fully contained in another yields the outer span.
        let outer = span(1, 0, 9, 0);
        let inner = span(3, 2, 4, 1);
        let combined = outer + inner;
        assert_eq!(combined.line_start, 1);
        assert_eq!(combined.col_start, 0);
        assert_eq!(combined.line_stop, 9);
        assert_eq!(combined.col_stop, 0);
    }

    #[test]
    fn span_add_assign_matches_add() {
        let mut acc = span(2, 1, 2, 4);
        let other = span(6, 0, 6, 3);
        let expected = acc + other;
        acc += other;
        assert_eq!(acc.line_start, expected.line_start);
        assert_eq!(acc.col_start, expected.col_start);
        assert_eq!(acc.line_stop, expected.line_stop);
        assert_eq!(acc.col_stop, expected.col_stop);
    }

    #[test]
    fn spanned_deref_and_display() {
        let spanned = Spanned {
            token: 42u32,
            span: span(1, 0, 1, 2),
        };
        assert_eq!(*spanned, 42);
        assert_eq!(spanned.to_string(), "'42' @ 1:0-2");
    }
}
