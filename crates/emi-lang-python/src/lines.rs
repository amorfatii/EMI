//! Positions in CPython's conventions: 1-based lines split on `\n` only (as
//! `tokenize` sees them through `StringIO.readline`), token columns in code
//! points (as `tokenize` reports them) and AST columns in UTF-8 bytes (as
//! `ast` reports them). The oracle compares the two unit systems directly,
//! so both are kept.

use ruff_text_size::TextSize;

pub struct LineIndex<'s> {
    source: &'s str,
    starts: Vec<usize>,
}

impl<'s> LineIndex<'s> {
    pub fn new(source: &'s str) -> Self {
        let mut starts = vec![0];
        starts.extend(source.match_indices('\n').map(|(i, _)| i + 1));
        LineIndex { source, starts }
    }

    fn index(&self, offset: usize) -> usize {
        self.starts.partition_point(|&s| s <= offset) - 1
    }

    /// 1-based line holding the byte at `offset`.
    pub fn line(&self, offset: TextSize) -> u32 {
        u32::try_from(self.index(offset.to_usize()) + 1).unwrap_or(u32::MAX)
    }

    /// 1-based line of the last byte of a range ending (exclusively) at `end`.
    pub fn end_line(&self, end: TextSize) -> u32 {
        self.line(end.checked_sub(TextSize::from(1)).unwrap_or(end))
    }

    /// `(line, column in bytes)` as `ast` reports positions.
    pub fn ast_pos(&self, offset: TextSize) -> (u32, usize) {
        let offset = offset.to_usize();
        let idx = self.index(offset);
        (
            u32::try_from(idx + 1).unwrap_or(u32::MAX),
            offset - self.starts[idx],
        )
    }

    /// `(line, column in code points)` as `tokenize` reports positions.
    pub fn token_pos(&self, offset: TextSize) -> (u32, usize) {
        let offset = offset.to_usize();
        let idx = self.index(offset);
        let col = self.source[self.starts[idx]..offset].chars().count();
        (u32::try_from(idx + 1).unwrap_or(u32::MAX), col)
    }
}
