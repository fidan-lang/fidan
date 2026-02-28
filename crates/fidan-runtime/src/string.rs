/// Copy-on-Write string. Clone is cheap; copy happens only on mutation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FidanString(pub std::sync::Arc<str>);

impl FidanString {
    pub fn new(s: &str) -> Self { FidanString(s.into()) }
    pub fn as_str(&self) -> &str { &self.0 }
    pub fn len(&self) -> usize { self.0.len() }
    pub fn is_empty(&self) -> bool { self.0.is_empty() }
    /// Append: makes a new Arc<str>. Caller owns the result.
    pub fn append(&self, other: &FidanString) -> FidanString {
        FidanString(format!("{}{}", self.0, other.0).into())
    }
}

impl std::fmt::Display for FidanString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { self.0.fmt(f) }
}
