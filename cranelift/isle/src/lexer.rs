//! Lexer for the ISLE language.

use crate::error::Error;
use std::borrow::Cow;

#[derive(Clone, Debug)]
pub struct Lexer<'a> {
    pub filenames: Vec<String>,
    file_starts: Vec<usize>,
    buf: Cow<'a, [u8]>,
    pos: Pos,
    lookahead: Option<(Pos, Token)>,
}

#[derive(Clone, Debug)]
enum LexerInput<'a> {
    String { s: &'a str, filename: &'a str },
    File { content: String, filename: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pos {
    pub file: usize,
    pub offset: usize,
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub fn pretty_print(&self, filenames: &[String]) -> String {
        format!("{}:{}:{}", filenames[self.file], self.line, self.col)
    }
    pub fn pretty_print_line(&self, filenames: &[String]) -> String {
        format!("{} line {}", filenames[self.file], self.line)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    LParen,
    RParen,
    Symbol(String),
    Int(i64),
}

impl<'a> Lexer<'a> {
    pub fn from_str(s: &'a str, filename: &'a str) -> Lexer<'a> {
        let mut l = Lexer {
            filenames: vec![filename.to_string()],
            file_starts: vec![0],
            buf: Cow::Borrowed(s.as_bytes()),
            pos: Pos {
                file: 0,
                offset: 0,
                line: 1,
                col: 0,
            },
            lookahead: None,
        };
        l.reload();
        l
    }

    pub fn from_files(filenames: Vec<String>) -> Result<Lexer<'a>, Error> {
        assert!(!filenames.is_empty());
        let file_contents: Vec<String> = filenames
            .iter()
            .map(|f| {
                use std::io::Read;
                let mut f = std::fs::File::open(f)?;
                let mut s = String::new();
                f.read_to_string(&mut s)?;
                Ok(s)
            })
            .collect::<Result<Vec<String>, Error>>()?;

        let mut file_starts = vec![];
        let mut buf = String::new();
        for file in file_contents {
            file_starts.push(buf.len());
            buf += &file;
            buf += "\n";
        }

        let mut l = Lexer {
            filenames,
            buf: Cow::Owned(buf.into_bytes()),
            file_starts,
            pos: Pos {
                file: 0,
                offset: 0,
                line: 1,
                col: 0,
            },
            lookahead: None,
        };
        l.reload();
        Ok(l)
    }

    pub fn offset(&self) -> usize {
        self.pos.offset
    }

    pub fn pos(&self) -> Pos {
        self.pos
    }

    fn advance_pos(&mut self) {
        self.pos.col += 1;
        if self.buf[self.pos.offset] == b'\n' {
            self.pos.line += 1;
            self.pos.col = 0;
        }
        self.pos.offset += 1;
        if self.pos.file + 1 < self.file_starts.len() {
            let next_start = self.file_starts[self.pos.file + 1];
            if self.pos.offset >= next_start {
                assert!(self.pos.offset == next_start);
                self.pos.file += 1;
                self.pos.line = 1;
            }
        }
    }

    fn next_token(&mut self) -> Option<(Pos, Token)> {
        fn is_sym_first_char(c: u8) -> bool {
            match c {
                b'-' | b'0'..=b'9' | b'(' | b')' | b';' => false,
                c if c.is_ascii_whitespace() => false,
                _ => true,
            }
        }
        fn is_sym_other_char(c: u8) -> bool {
            match c {
                b'(' | b')' | b';' => false,
                c if c.is_ascii_whitespace() => false,
                _ => true,
            }
        }

        // Skip any whitespace and any comments.
        while self.pos.offset < self.buf.len() {
            if self.buf[self.pos.offset].is_ascii_whitespace() {
                self.advance_pos();
                continue;
            }
            if self.buf[self.pos.offset] == b';' {
                while self.pos.offset < self.buf.len() && self.buf[self.pos.offset] != b'\n' {
                    self.advance_pos();
                }
                continue;
            }
            break;
        }

        if self.pos.offset == self.buf.len() {
            return None;
        }

        let char_pos = self.pos;
        match self.buf[self.pos.offset] {
            b'(' => {
                self.advance_pos();
                Some((char_pos, Token::LParen))
            }
            b')' => {
                self.advance_pos();
                Some((char_pos, Token::RParen))
            }
            c if is_sym_first_char(c) => {
                let start = self.pos.offset;
                let start_pos = self.pos;
                while self.pos.offset < self.buf.len()
                    && is_sym_other_char(self.buf[self.pos.offset])
                {
                    self.advance_pos();
                }
                let end = self.pos.offset;
                let s = std::str::from_utf8(&self.buf[start..end])
                    .expect("Only ASCII characters, should be UTF-8");
                Some((start_pos, Token::Symbol(s.to_string())))
            }
            c if (c >= b'0' && c <= b'9') || c == b'-' => {
                let start_pos = self.pos;
                let neg = if c == b'-' {
                    self.advance_pos();
                    true
                } else {
                    false
                };
                let mut num = 0;
                while self.pos.offset < self.buf.len()
                    && (self.buf[self.pos.offset] >= b'0' && self.buf[self.pos.offset] <= b'9')
                {
                    num = (num * 10) + (self.buf[self.pos.offset] - b'0') as i64;
                    self.advance_pos();
                }

                let tok = if neg {
                    Token::Int(-num)
                } else {
                    Token::Int(num)
                };
                Some((start_pos, tok))
            }
            c => panic!("Unexpected character '{}' at offset {}", c, self.pos.offset),
        }
    }

    fn reload(&mut self) {
        if self.lookahead.is_none() && self.pos.offset < self.buf.len() {
            self.lookahead = self.next_token();
        }
    }

    pub fn peek(&self) -> Option<&(Pos, Token)> {
        self.lookahead.as_ref()
    }

    pub fn eof(&self) -> bool {
        self.lookahead.is_none()
    }
}

impl<'a> std::iter::Iterator for Lexer<'a> {
    type Item = (Pos, Token);

    fn next(&mut self) -> Option<(Pos, Token)> {
        let tok = self.lookahead.take();
        self.reload();
        tok
    }
}

impl Token {
    pub fn is_int(&self) -> bool {
        match self {
            Token::Int(_) => true,
            _ => false,
        }
    }

    pub fn is_sym(&self) -> bool {
        match self {
            Token::Symbol(_) => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn lexer_basic() {
        assert_eq!(
            Lexer::from_str(
                ";; comment\n; another\r\n   \t(one two three 23 -568  )\n",
                "test"
            )
            .map(|(_, tok)| tok)
            .collect::<Vec<_>>(),
            vec![
                Token::LParen,
                Token::Symbol("one".to_string()),
                Token::Symbol("two".to_string()),
                Token::Symbol("three".to_string()),
                Token::Int(23),
                Token::Int(-568),
                Token::RParen
            ]
        );
    }

    #[test]
    fn ends_with_sym() {
        assert_eq!(
            Lexer::from_str("asdf", "test")
                .map(|(_, tok)| tok)
                .collect::<Vec<_>>(),
            vec![Token::Symbol("asdf".to_string()),]
        );
    }

    #[test]
    fn ends_with_num() {
        assert_eq!(
            Lexer::from_str("23", "test")
                .map(|(_, tok)| tok)
                .collect::<Vec<_>>(),
            vec![Token::Int(23)],
        );
    }

    #[test]
    fn weird_syms() {
        assert_eq!(
            Lexer::from_str("(+ [] => !! _test!;comment\n)", "test")
                .map(|(_, tok)| tok)
                .collect::<Vec<_>>(),
            vec![
                Token::LParen,
                Token::Symbol("+".to_string()),
                Token::Symbol("[]".to_string()),
                Token::Symbol("=>".to_string()),
                Token::Symbol("!!".to_string()),
                Token::Symbol("_test!".to_string()),
                Token::RParen,
            ]
        );
    }
}
