#[cfg(span_locations)]
use std::cell::RefCell;
#[cfg(span_locations)]
use std::cmp;
use std::fmt;
use std::iter;
use std::ops::RangeBounds;
#[cfg(procmacro2_semver_exempt)]
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use std::vec;

use crate::{Delimiter, Punct, Spacing, TokenTree};
use rustc_lexer::{first_token, Token, TokenKind};
use unicode_xid::UnicodeXID;

#[derive(Clone)]
pub struct TokenStream {
    inner: Vec<TokenTree>,
}

#[derive(Debug)]
pub struct LexError;

struct Cursor<'a> {
    pub rest: &'a str,
    pub head: Option<Token>,
    #[cfg(span_locations)]
    pub off: u32,
}

#[cfg(span_locations)]
impl<'a> Clone for Cursor<'a> {
    fn clone(&self) -> Self {
        Cursor::new(self.rest, self.off)
    }
}

#[cfg(not(span_locations))]
impl<'a> Clone for Cursor<'a> {
    fn clone(&self) -> Self {
        Cursor::new(self.rest, 0)
    }
}

impl TokenStream {
    pub fn new() -> TokenStream {
        TokenStream { inner: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.inner.len() == 0
    }
}

#[cfg(span_locations)]
fn get_cursor(src: &str) -> Cursor {
    // Create a dummy file & add it to the source map
    SOURCE_MAP.with(|cm| {
        let mut cm = cm.borrow_mut();
        let name = format!("<parsed string {}>", cm.files.len());
        let span = cm.add_file(&name, src);
        Cursor::new(src, span.lo)
    })
}

#[cfg(not(span_locations))]
fn get_cursor(src: &str) -> Cursor {
    Cursor::new(src, 0)
}

impl FromStr for TokenStream {
    type Err = LexError;

    fn from_str(src: &str) -> Result<TokenStream, LexError> {
        // Create a dummy file & add it to the source map
        let mut cursor = get_cursor(src);
        match token_stream(&mut cursor) {
            Ok(tts) if cursor.is_empty() => Ok(tts),
            _ => Err(LexError),
        }
    }
}

impl fmt::Display for TokenStream {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut joint = false;
        for (i, tt) in self.inner.iter().enumerate() {
            if i != 0 && !joint {
                write!(f, " ")?;
            }
            joint = false;
            match *tt {
                TokenTree::Group(ref tt) => {
                    let (start, end) = match tt.delimiter() {
                        Delimiter::Parenthesis => ("(", ")"),
                        Delimiter::Brace => ("{", "}"),
                        Delimiter::Bracket => ("[", "]"),
                        Delimiter::None => ("", ""),
                    };
                    if tt.stream().into_iter().next().is_none() {
                        write!(f, "{} {}", start, end)?
                    } else {
                        write!(f, "{} {} {}", start, tt.stream(), end)?
                    }
                }
                TokenTree::Ident(ref tt) => write!(f, "{}", tt)?,
                TokenTree::Punct(ref tt) => {
                    write!(f, "{}", tt.as_char())?;
                    match tt.spacing() {
                        Spacing::Alone => {}
                        Spacing::Joint => joint = true,
                    }
                }
                TokenTree::Literal(ref tt) => write!(f, "{}", tt)?,
            }
        }

        Ok(())
    }
}

impl fmt::Debug for TokenStream {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("TokenStream ")?;
        f.debug_list().entries(self.clone()).finish()
    }
}

#[cfg(use_proc_macro)]
impl From<proc_macro::TokenStream> for TokenStream {
    fn from(inner: proc_macro::TokenStream) -> TokenStream {
        inner
            .to_string()
            .parse()
            .expect("compiler token stream parse failed")
    }
}

#[cfg(use_proc_macro)]
impl From<TokenStream> for proc_macro::TokenStream {
    fn from(inner: TokenStream) -> proc_macro::TokenStream {
        inner
            .to_string()
            .parse()
            .expect("failed to parse to compiler tokens")
    }
}

impl From<TokenTree> for TokenStream {
    fn from(tree: TokenTree) -> TokenStream {
        TokenStream { inner: vec![tree] }
    }
}

impl iter::FromIterator<TokenTree> for TokenStream {
    fn from_iter<I: IntoIterator<Item = TokenTree>>(streams: I) -> Self {
        let mut v = Vec::new();

        for token in streams.into_iter() {
            v.push(token);
        }

        TokenStream { inner: v }
    }
}

impl iter::FromIterator<TokenStream> for TokenStream {
    fn from_iter<I: IntoIterator<Item = TokenStream>>(streams: I) -> Self {
        let mut v = Vec::new();

        for stream in streams.into_iter() {
            v.extend(stream.inner);
        }

        TokenStream { inner: v }
    }
}

impl Extend<TokenTree> for TokenStream {
    fn extend<I: IntoIterator<Item = TokenTree>>(&mut self, streams: I) {
        self.inner.extend(streams);
    }
}

impl Extend<TokenStream> for TokenStream {
    fn extend<I: IntoIterator<Item = TokenStream>>(&mut self, streams: I) {
        self.inner
            .extend(streams.into_iter().flat_map(|stream| stream));
    }
}

pub type TokenTreeIter = vec::IntoIter<TokenTree>;

impl IntoIterator for TokenStream {
    type Item = TokenTree;
    type IntoIter = TokenTreeIter;

    fn into_iter(self) -> TokenTreeIter {
        self.inner.into_iter()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SourceFile {
    path: PathBuf,
}

impl SourceFile {
    /// Get the path to this source file as a string.
    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }

    pub fn is_real(&self) -> bool {
        // XXX(nika): Support real files in the future?
        false
    }
}

impl fmt::Debug for SourceFile {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("SourceFile")
            .field("path", &self.path())
            .field("is_real", &self.is_real())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineColumn {
    pub line: usize,
    pub column: usize,
}

#[cfg(span_locations)]
thread_local! {
    static SOURCE_MAP: RefCell<SourceMap> = RefCell::new(SourceMap {
        // NOTE: We start with a single dummy file which all call_site() and
        // def_site() spans reference.
        files: vec![{
            #[cfg(procmacro2_semver_exempt)]
            {
                FileInfo {
                    name: "<unspecified>".to_owned(),
                    span: Span { lo: 0, hi: 0 },
                    lines: vec![0],
                }
            }

            #[cfg(not(procmacro2_semver_exempt))]
            {
                FileInfo {
                    span: Span { lo: 0, hi: 0 },
                    lines: vec![0],
                }
            }
        }],
    });
}

#[cfg(span_locations)]
struct FileInfo {
    #[cfg(procmacro2_semver_exempt)]
    name: String,
    span: Span,
    lines: Vec<usize>,
}

#[cfg(span_locations)]
impl FileInfo {
    fn offset_line_column(&self, offset: usize) -> LineColumn {
        assert!(self.span_within(Span {
            lo: offset as u32,
            hi: offset as u32,
        }));
        let offset = offset - self.span.lo as usize;
        match self.lines.binary_search(&offset) {
            Ok(found) => LineColumn {
                line: found + 1,
                column: 0,
            },
            Err(idx) => LineColumn {
                line: idx,
                column: offset - self.lines[idx - 1],
            },
        }
    }

    fn span_within(&self, span: Span) -> bool {
        span.lo >= self.span.lo && span.hi <= self.span.hi
    }
}

/// Computes the offsets of each line in the given source string.
#[cfg(span_locations)]
fn lines_offsets(s: &str) -> Vec<usize> {
    let mut lines = vec![0];
    let mut prev = 0;
    while let Some(len) = s[prev..].find('\n') {
        prev += len + 1;
        lines.push(prev);
    }
    lines
}

#[cfg(span_locations)]
struct SourceMap {
    files: Vec<FileInfo>,
}

#[cfg(span_locations)]
impl SourceMap {
    fn next_start_pos(&self) -> u32 {
        // Add 1 so there's always space between files.
        //
        // We'll always have at least 1 file, as we initialize our files list
        // with a dummy file.
        self.files.last().unwrap().span.hi + 1
    }

    fn add_file(&mut self, name: &str, src: &str) -> Span {
        let lines = lines_offsets(src);
        let lo = self.next_start_pos();
        // XXX(nika): Shouild we bother doing a checked cast or checked add here?
        let span = Span {
            lo,
            hi: lo + (src.len() as u32),
        };

        #[cfg(procmacro2_semver_exempt)]
        self.files.push(FileInfo {
            name: name.to_owned(),
            span,
            lines,
        });

        #[cfg(not(procmacro2_semver_exempt))]
        self.files.push(FileInfo { span, lines });
        let _ = name;

        span
    }

    fn fileinfo(&self, span: Span) -> &FileInfo {
        for file in &self.files {
            if file.span_within(span) {
                return file;
            }
        }
        panic!("Invalid span with no related FileInfo!");
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Span {
    #[cfg(span_locations)]
    lo: u32,
    #[cfg(span_locations)]
    hi: u32,
}

impl Span {
    #[cfg(not(span_locations))]
    pub fn call_site() -> Span {
        Span {}
    }

    #[cfg(span_locations)]
    pub fn call_site() -> Span {
        Span { lo: 0, hi: 0 }
    }

    #[cfg(procmacro2_semver_exempt)]
    pub fn def_site() -> Span {
        Span::call_site()
    }

    #[cfg(procmacro2_semver_exempt)]
    pub fn resolved_at(&self, _other: Span) -> Span {
        // Stable spans consist only of line/column information, so
        // `resolved_at` and `located_at` only select which span the
        // caller wants line/column information from.
        *self
    }

    #[cfg(procmacro2_semver_exempt)]
    pub fn located_at(&self, other: Span) -> Span {
        other
    }

    #[cfg(procmacro2_semver_exempt)]
    pub fn source_file(&self) -> SourceFile {
        SOURCE_MAP.with(|cm| {
            let cm = cm.borrow();
            let fi = cm.fileinfo(*self);
            SourceFile {
                path: Path::new(&fi.name).to_owned(),
            }
        })
    }

    #[cfg(span_locations)]
    pub fn start(&self) -> LineColumn {
        SOURCE_MAP.with(|cm| {
            let cm = cm.borrow();
            let fi = cm.fileinfo(*self);
            fi.offset_line_column(self.lo as usize)
        })
    }

    #[cfg(span_locations)]
    pub fn end(&self) -> LineColumn {
        SOURCE_MAP.with(|cm| {
            let cm = cm.borrow();
            let fi = cm.fileinfo(*self);
            fi.offset_line_column(self.hi as usize)
        })
    }

    #[cfg(not(span_locations))]
    pub fn join(&self, _other: Span) -> Option<Span> {
        Some(Span {})
    }

    #[cfg(span_locations)]
    pub fn join(&self, other: Span) -> Option<Span> {
        SOURCE_MAP.with(|cm| {
            let cm = cm.borrow();
            // If `other` is not within the same FileInfo as us, return None.
            if !cm.fileinfo(*self).span_within(other) {
                return None;
            }
            Some(Span {
                lo: cmp::min(self.lo, other.lo),
                hi: cmp::max(self.hi, other.hi),
            })
        })
    }

    #[cfg(not(span_locations))]
    fn first_byte(self) -> Self {
        self
    }

    #[cfg(span_locations)]
    fn first_byte(self) -> Self {
        Span {
            lo: self.lo,
            hi: cmp::min(self.lo.saturating_add(1), self.hi),
        }
    }

    #[cfg(not(span_locations))]
    fn last_byte(self) -> Self {
        self
    }

    #[cfg(span_locations)]
    fn last_byte(self) -> Self {
        Span {
            lo: cmp::max(self.hi.saturating_sub(1), self.lo),
            hi: self.hi,
        }
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        #[cfg(procmacro2_semver_exempt)]
        return write!(f, "bytes({}..{})", self.lo, self.hi);

        #[cfg(not(procmacro2_semver_exempt))]
        write!(f, "Span")
    }
}

pub fn debug_span_field_if_nontrivial(debug: &mut fmt::DebugStruct, span: Span) {
    if cfg!(procmacro2_semver_exempt) {
        debug.field("span", &span);
    }
}

#[derive(Clone)]
pub struct Group {
    delimiter: Delimiter,
    stream: TokenStream,
    span: Span,
}

impl Group {
    pub fn new(delimiter: Delimiter, stream: TokenStream) -> Group {
        Group {
            delimiter,
            stream,
            span: Span::call_site(),
        }
    }

    pub fn delimiter(&self) -> Delimiter {
        self.delimiter
    }

    pub fn stream(&self) -> TokenStream {
        self.stream.clone()
    }

    pub fn span(&self) -> Span {
        self.span
    }

    pub fn span_open(&self) -> Span {
        self.span.first_byte()
    }

    pub fn span_close(&self) -> Span {
        self.span.last_byte()
    }

    pub fn set_span(&mut self, span: Span) {
        self.span = span;
    }
}

impl fmt::Display for Group {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let (left, right) = match self.delimiter {
            Delimiter::Parenthesis => ("(", ")"),
            Delimiter::Brace => ("{", "}"),
            Delimiter::Bracket => ("[", "]"),
            Delimiter::None => ("", ""),
        };

        f.write_str(left)?;
        self.stream.fmt(f)?;
        f.write_str(right)?;

        Ok(())
    }
}

impl fmt::Debug for Group {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        let mut debug = fmt.debug_struct("Group");
        debug.field("delimiter", &self.delimiter);
        debug.field("stream", &self.stream);
        #[cfg(procmacro2_semver_exempt)]
        debug.field("span", &self.span);
        debug.finish()
    }
}

#[derive(Clone)]
pub struct Ident {
    sym: String,
    span: Span,
    raw: bool,
}

impl Ident {
    fn _new(string: &str, raw: bool, span: Span) -> Ident {
        validate_ident(string);

        Ident {
            sym: string.to_owned(),
            span,
            raw,
        }
    }

    pub fn new(string: &str, span: Span) -> Ident {
        Ident::_new(string, false, span)
    }

    pub fn new_raw(string: &str, span: Span) -> Ident {
        Ident::_new(string, true, span)
    }

    pub fn span(&self) -> Span {
        self.span
    }

    pub fn set_span(&mut self, span: Span) {
        self.span = span;
    }
}

fn is_ident_start(c: char) -> bool {
    ('a' <= c && c <= 'z')
        || ('A' <= c && c <= 'Z')
        || c == '_'
        || (c > '\x7f' && UnicodeXID::is_xid_start(c))
}

fn is_ident_continue(c: char) -> bool {
    ('a' <= c && c <= 'z')
        || ('A' <= c && c <= 'Z')
        || c == '_'
        || ('0' <= c && c <= '9')
        || (c > '\x7f' && UnicodeXID::is_xid_continue(c))
}

fn validate_ident(string: &str) {
    let validate = string;
    if validate.is_empty() {
        panic!("Ident is not allowed to be empty; use Option<Ident>");
    }

    if validate.bytes().all(|digit| digit >= b'0' && digit <= b'9') {
        panic!("Ident cannot be a number; use Literal instead");
    }

    fn ident_ok(string: &str) -> bool {
        let mut chars = string.chars();
        let first = chars.next().unwrap();
        if !is_ident_start(first) {
            return false;
        }
        for ch in chars {
            if !is_ident_continue(ch) {
                return false;
            }
        }
        true
    }

    if !ident_ok(validate) {
        panic!("{:?} is not a valid Ident", string);
    }
}

impl PartialEq for Ident {
    fn eq(&self, other: &Ident) -> bool {
        self.sym == other.sym && self.raw == other.raw
    }
}

impl<T> PartialEq<T> for Ident
where
    T: ?Sized + AsRef<str>,
{
    fn eq(&self, other: &T) -> bool {
        let other = other.as_ref();
        if self.raw {
            other.starts_with("r#") && self.sym == other[2..]
        } else {
            self.sym == other
        }
    }
}

impl fmt::Display for Ident {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.raw {
            "r#".fmt(f)?;
        }
        self.sym.fmt(f)
    }
}

impl fmt::Debug for Ident {
    // Ident(proc_macro), Ident(r#union)
    #[cfg(not(procmacro2_semver_exempt))]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut debug = f.debug_tuple("Ident");
        debug.field(&format_args!("{}", self));
        debug.finish()
    }

    // Ident {
    //     sym: proc_macro,
    //     span: bytes(128..138)
    // }
    #[cfg(procmacro2_semver_exempt)]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut debug = f.debug_struct("Ident");
        debug.field("sym", &format_args!("{}", self));
        debug.field("span", &self.span);
        debug.finish()
    }
}

#[derive(Clone)]
pub struct Literal {
    text: String,
    span: Span,
}

macro_rules! suffixed_numbers {
    ($($name:ident => $kind:ident,)*) => ($(
        pub fn $name(n: $kind) -> Literal {
            Literal::_new(format!(concat!("{}", stringify!($kind)), n))
        }
    )*)
}

macro_rules! unsuffixed_numbers {
    ($($name:ident => $kind:ident,)*) => ($(
        pub fn $name(n: $kind) -> Literal {
            Literal::_new(n.to_string())
        }
    )*)
}

impl Literal {
    fn _new(text: String) -> Literal {
        Literal {
            text,
            span: Span::call_site(),
        }
    }

    suffixed_numbers! {
        u8_suffixed => u8,
        u16_suffixed => u16,
        u32_suffixed => u32,
        u64_suffixed => u64,
        u128_suffixed => u128,
        usize_suffixed => usize,
        i8_suffixed => i8,
        i16_suffixed => i16,
        i32_suffixed => i32,
        i64_suffixed => i64,
        i128_suffixed => i128,
        isize_suffixed => isize,

        f32_suffixed => f32,
        f64_suffixed => f64,
    }

    unsuffixed_numbers! {
        u8_unsuffixed => u8,
        u16_unsuffixed => u16,
        u32_unsuffixed => u32,
        u64_unsuffixed => u64,
        u128_unsuffixed => u128,
        usize_unsuffixed => usize,
        i8_unsuffixed => i8,
        i16_unsuffixed => i16,
        i32_unsuffixed => i32,
        i64_unsuffixed => i64,
        i128_unsuffixed => i128,
        isize_unsuffixed => isize,
    }

    pub fn f32_unsuffixed(f: f32) -> Literal {
        let mut s = f.to_string();
        if !s.contains(".") {
            s.push_str(".0");
        }
        Literal::_new(s)
    }

    pub fn f64_unsuffixed(f: f64) -> Literal {
        let mut s = f.to_string();
        if !s.contains(".") {
            s.push_str(".0");
        }
        Literal::_new(s)
    }

    pub fn string(t: &str) -> Literal {
        let mut text = String::with_capacity(t.len() + 2);
        text.push('"');
        for c in t.chars() {
            if c == '\'' {
                // escape_debug turns this into "\'" which is unnecessary.
                text.push(c);
            } else {
                text.extend(c.escape_debug());
            }
        }
        text.push('"');
        Literal::_new(text)
    }

    pub fn character(t: char) -> Literal {
        let mut text = String::new();
        text.push('\'');
        if t == '"' {
            // escape_debug turns this into '\"' which is unnecessary.
            text.push(t);
        } else {
            text.extend(t.escape_debug());
        }
        text.push('\'');
        Literal::_new(text)
    }

    pub fn byte_string(bytes: &[u8]) -> Literal {
        let mut escaped = "b\"".to_string();
        for b in bytes {
            match *b {
                b'\0' => escaped.push_str(r"\0"),
                b'\t' => escaped.push_str(r"\t"),
                b'\n' => escaped.push_str(r"\n"),
                b'\r' => escaped.push_str(r"\r"),
                b'"' => escaped.push_str("\\\""),
                b'\\' => escaped.push_str("\\\\"),
                b'\x20'..=b'\x7E' => escaped.push(*b as char),
                _ => escaped.push_str(&format!("\\x{:02X}", b)),
            }
        }
        escaped.push('"');
        Literal::_new(escaped)
    }

    pub fn span(&self) -> Span {
        self.span
    }

    pub fn set_span(&mut self, span: Span) {
        self.span = span;
    }

    pub fn subspan<R: RangeBounds<usize>>(&self, _range: R) -> Option<Span> {
        None
    }
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.text.fmt(f)
    }
}

impl fmt::Debug for Literal {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        let mut debug = fmt.debug_struct("Literal");
        debug.field("lit", &format_args!("{}", self.text));
        #[cfg(procmacro2_semver_exempt)]
        debug.field("span", &self.span);
        debug.finish()
    }
}

fn lex(src: &str) -> Option<Token> {
    if src.is_empty() {
        None
    } else {
        Some(first_token(src))
    }
}

impl<'a> Cursor<'a> {
    #[cfg(span_locations)]
    pub fn new(rest: &'a str, off: u32) -> Self {
        Cursor {
            rest,
            off,
            head: lex(rest),
        }
    }

    #[cfg(not(span_locations))]
    pub fn new(rest: &'a str, _off: u32) -> Self {
        Cursor {
            rest,
            head: lex(rest),
        }
    }

    #[cfg(span_locations)]
    pub fn bumpped(&self) -> Self {
        assert!(!self.is_empty());
        let off = self.head.as_ref().unwrap().len;
        Cursor::new(&self.rest[off..], self.off + off as u32)
    }

    #[cfg(not(span_locations))]
    pub fn bumpped(&self) -> Self {
        assert!(!self.is_empty());
        let off = self.head.as_ref().unwrap().len;
        Cursor::new(&self.rest[off..], off as u32)
    }

    pub fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }

    pub fn slice(&self) -> &'a str {
        assert!(!self.is_empty());
        &self.rest[0..self.head.as_ref().unwrap().len]
    }
}

fn token_stream(input: &mut Cursor<'_>) -> Result<TokenStream, LexError> {
    let mut trees = Vec::new();
    while !input.is_empty() {
        match input.head.as_ref().unwrap().kind {
            TokenKind::Whitespace => *input = input.bumpped(),
            TokenKind::LineComment | TokenKind::BlockComment { .. } => {
                trees.extend(comment(input)?)
            }
            TokenKind::CloseBrace | TokenKind::CloseBracket | TokenKind::CloseParen => break,
            TokenKind::OpenBrace | TokenKind::OpenBracket | TokenKind::OpenParen => {
                trees.push(token_tree(input)?)
            }
            _ => trees.extend(leaf_token(input)?),
        }
    }
    Ok(TokenStream { inner: trees })
}

fn comment(input: &mut Cursor<'_>) -> Result<Vec<TokenTree>, LexError> {
    fn doc_comment_contents<'a>(
        input: &mut Cursor<'a>,
    ) -> Result<Option<(&'a str, bool)>, LexError> {
        assert!(!input.is_empty());
        match input.head.as_ref().unwrap().kind {
            TokenKind::LineComment => {
                let slice = input.slice();
                *input = input.bumpped();
                if slice.starts_with("//!") {
                    Ok(Some((&slice[3..], true)))
                } else if slice.starts_with("///") && !slice.starts_with("////") {
                    Ok(Some((&slice[3..], false)))
                } else {
                    Ok(None)
                }
            }
            TokenKind::BlockComment { terminated: false } => Err(LexError),
            TokenKind::BlockComment { .. } => {
                let slice = input.slice();
                *input = input.bumpped();
                if slice.starts_with("/*!") {
                    Ok(Some((&slice[3..], true)))
                } else if slice.starts_with("/**")
                    && !slice.starts_with("/***")
                    && !slice.starts_with("/**/")
                {
                    Ok(Some((&slice[3..], false)))
                } else {
                    Ok(None)
                }
            }
            kind => unreachable!("comment {:?}", kind),
        }
    }

    let mut trees = Vec::new();
    if let (Some((comment, inner)), span) = spanned(input, doc_comment_contents)? {
        trees.push(TokenTree::Punct(Punct::new('#', Spacing::Alone)));
        if inner {
            trees.push(Punct::new('!', Spacing::Alone).into());
        }
        let mut stream = vec![
            TokenTree::Ident(crate::Ident::new("doc", span)),
            TokenTree::Punct(Punct::new('=', Spacing::Alone)),
            TokenTree::Literal(crate::Literal::string(comment)),
        ];
        for tt in stream.iter_mut() {
            tt.set_span(span);
        }
        let group = Group::new(Delimiter::Bracket, stream.into_iter().collect());
        trees.push(crate::Group::_new_stable(group).into());
        for tt in trees.iter_mut() {
            tt.set_span(span);
        }
    }
    Ok(trees)
}

fn token_tree(input: &mut Cursor<'_>) -> Result<TokenTree, LexError> {
    fn delimited_stream(
        close: TokenKind,
        delimiter: Delimiter,
        input: &mut Cursor<'_>,
    ) -> Result<TokenTree, LexError> {
        *input = input.bumpped();
        let tts = token_stream(input)?;
        if input.head.as_ref().map_or(false, |t| t.kind == close) {
            let g = Group::new(delimiter, tts);
            Ok(TokenTree::Group(crate::Group::_new_stable(g)))
        } else {
            Err(LexError)
        }
    }

    fn token_tree_(input: &mut Cursor<'_>) -> Result<TokenTree, LexError> {
        assert!(!input.is_empty());
        match input.head.as_ref().unwrap().kind {
            TokenKind::OpenParen => {
                delimited_stream(TokenKind::CloseParen, Delimiter::Parenthesis, input)
            }
            TokenKind::OpenBracket => {
                delimited_stream(TokenKind::CloseBracket, Delimiter::Bracket, input)
            }
            TokenKind::OpenBrace => {
                delimited_stream(TokenKind::CloseBrace, Delimiter::Brace, input)
            }
            kind => unreachable!("token_tree {:?}", kind),
        }
    }

    let (mut tt, span) = spanned(input, token_tree_)?;
    tt.set_span(span);
    Ok(tt)
}

fn leaf_token(input: &mut Cursor<'_>) -> Result<Vec<TokenTree>, LexError> {
    fn is_punct(kind: TokenKind) -> bool {
        match kind {
            TokenKind::Semi
            | TokenKind::Comma
            | TokenKind::Dot
            | TokenKind::At
            | TokenKind::Pound
            | TokenKind::Tilde
            | TokenKind::Question
            | TokenKind::Colon
            | TokenKind::Dollar
            | TokenKind::Eq
            | TokenKind::Not
            | TokenKind::Lt
            | TokenKind::Gt
            | TokenKind::Minus
            | TokenKind::And
            | TokenKind::Or
            | TokenKind::Plus
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::Caret
            | TokenKind::Percent => true,
            _ => false,
        }
    }

    fn leaf_token_(input: &mut Cursor<'_>) -> Result<Vec<TokenTree>, LexError> {
        assert!(!input.is_empty());
        match input.head.as_ref().unwrap().kind {
            TokenKind::Literal { .. } => {
                let slice = input.slice();
                let l = Literal::_new(slice.to_string());
                *input = input.bumpped();
                Ok(vec![TokenTree::Literal(crate::Literal::_new_stable(l))])
            }
            TokenKind::Ident => {
                let sym = input.slice();
                let ident = crate::Ident::new(sym, crate::Span::call_site());
                *input = input.bumpped();
                Ok(vec![ident.into()])
            }
            TokenKind::RawIdent => {
                let sym = &input.slice()[2..]; // remove `r#` prefix
                if sym == "_" {
                    Err(LexError)
                } else {
                    let ident = crate::Ident::_new_raw(sym, crate::Span::call_site());
                    *input = input.bumpped();
                    Ok(vec![ident.into()])
                }
            }
            TokenKind::Lifetime { .. } => {
                let slice = &input.slice()[1..]; // strip leading `'`
                let p = Punct::new('\'', Spacing::Joint);
                let i = crate::Ident::new(slice, crate::Span::call_site());
                Ok(vec![p.into(), i.into()]) // FIXME: `spanned` does the wrong thing here
                                             // specifically: both the apostrophe and the ident have the same span
            }
            kind if is_punct(kind) => {
                let slice = input.slice();
                assert!(slice.chars().count() == 1);
                let ch = slice.chars().next().unwrap();
                *input = input.bumpped();
                let joint = if input.head.as_ref().map_or(false, |t| is_punct(t.kind)) {
                    Spacing::Joint
                } else {
                    Spacing::Alone
                };
                let p = Punct::new(ch, joint);
                Ok(vec![TokenTree::Punct(p)])
            }
            TokenKind::Unknown => Err(LexError),
            kind => unreachable!("leaf_token {:?}", kind),
        }
    }

    let (mut tts, span) = spanned(input, leaf_token_)?;
    for tt in &mut tts {
        tt.set_span(span);
    }
    Ok(tts)
}

#[cfg(not(span_locations))]
fn spanned<'a, T>(
    input: &mut Cursor<'a>,
    f: fn(&mut Cursor<'a>) -> Result<T, LexError>,
) -> Result<(T, crate::Span), LexError> {
    let out = f(input)?;
    Ok((out, crate::Span::_new_stable(Span::call_site())))
}

#[cfg(span_locations)]
fn spanned<'a, T>(
    input: &mut Cursor<'a>,
    f: fn(&mut Cursor<'a>) -> Result<T, LexError>,
) -> Result<(T, crate::Span), LexError> {
    let lo = input.off;
    let out = f(input)?;
    let hi = input.off;
    let span = crate::Span::_new_stable(Span { lo, hi });
    Ok((out, span))
}
