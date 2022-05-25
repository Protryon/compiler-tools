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
        if self.line_start == other.line_stop {
            Span {
                line_start: self.line_start,
                line_stop: self.line_stop,
                col_start: self.col_start.min(other.col_start),
                col_stop: self.col_stop.max(other.col_stop),
            }
        } else if self.line_start < other.line_start {
            Span {
                line_start: self.line_start,
                line_stop: other.line_stop,
                col_start: self.col_start,
                col_stop: other.col_stop,
            }
        } else {
            Span {
                line_start: other.line_start,
                line_stop: self.line_stop,
                col_start: other.col_start,
                col_stop: self.col_stop,
            }
        }
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
