pub struct InvalidGlobError;

enum Op {
    Slice { from: u16, to: u16 },
    Separator,
    Skip { len: u16 },
    Many,
    ManyComponents,
    AnyWithinRanges { start: u16, count: u16 },
    ExceptWithinRanges { start: u16, count: u16 },
    SubPatternGroup { len: u16 },
    SubPattern { len: u16 },
}

#[derive(Default)]
pub struct Glob {
    bytes: Vec<u8>,
    ops: Vec<Op>,
}

impl Glob {
    pub fn compile(&mut self, pattern: &[u8]) -> Result<(), InvalidGlobError> {
        self.bytes.clear();
        self.ops.clear();

        match self.compile_recursive(pattern) {
            Ok(len) if len == pattern.len() => Ok(()),
            _ => Err(InvalidGlobError),
        }
    }

    fn compile_recursive(&mut self, pattern: &[u8]) -> Result<usize, InvalidGlobError> {
        let mut start_ops_index = self.ops.len();
        let mut index = 0;

        macro_rules! next {
            () => {{
                let i = index;
                if i < pattern.len() {
                    index += 1;
                    Some(pattern[i])
                } else {
                    None
                }
            }};
        }

        macro_rules! peek {
            () => {
                if index < pattern.len() {
                    Some(pattern[index])
                } else {
                    None
                }
            };
        }

        loop {
            match next!() {
                None => break,
                Some(b'?') => match self.ops[start_ops_index..].last_mut() {
                    Some(Op::Skip { len }) => *len += 1,
                    _ => self.ops.push(Op::Skip { len: 1 }),
                },
                Some(b'*') => match peek!() {
                    Some(b'*') => {
                        index += 1;
                        match peek!() {
                            None | Some(b'/') => self.ops.push(Op::ManyComponents),
                            _ => return Err(InvalidGlobError),
                        }
                    }
                    _ => self.ops.push(Op::Many),
                },
                Some(b'[') => {
                    let inverse = match peek!() {
                        Some(b'!') => {
                            index += 1;
                            true
                        }
                        _ => false,
                    };
                    let start = self.bytes.len();
                    loop {
                        let start = match next!() {
                            None => return Err(InvalidGlobError),
                            Some(b']') => break,
                            Some(b) => b,
                        };
                        let end = match peek!() {
                            Some(b'-') => {
                                index += 1;
                                let end = match next!() {
                                    None | Some(b']') => return Err(InvalidGlobError),
                                    Some(b) => b,
                                };
                                if end < start {
                                    return Err(InvalidGlobError);
                                }
                                end
                            }
                            _ => start,
                        };

                        self.bytes.push(start);
                        self.bytes.push(end);
                    }
                    let count = ((self.bytes.len() - start) / 2) as _;
                    let start = start as _;
                    if inverse {
                        self.ops.push(Op::ExceptWithinRanges { start, count })
                    } else {
                        self.ops.push(Op::AnyWithinRanges { start, count })
                    }
                }
                Some(b']') => return Err(InvalidGlobError),
                Some(b'{') => {
                    let fix_index = self.ops.len();
                    self.ops.push(Op::SubPatternGroup { len: 0 });

                    loop {
                        let fix_index = self.ops.len();
                        self.ops.push(Op::SubPattern { len: 0 });

                        index += self.compile_recursive(&pattern[index..])?;

                        let ops_count = self.ops.len();
                        match &mut self.ops[fix_index] {
                            Op::SubPattern { len } => *len = (ops_count - fix_index - 1) as _,
                            _ => unreachable!(),
                        }

                        match next!() {
                            Some(b'}') => break,
                            Some(b',') => continue,
                            _ => return Err(InvalidGlobError),
                        }
                    }

                    let ops_count = self.ops.len();
                    match &mut self.ops[fix_index] {
                        Op::SubPatternGroup { len } => *len = (ops_count - fix_index - 1) as _,
                        _ => unreachable!(),
                    }

                    start_ops_index = self.ops.len();
                }
                Some(b'}') | Some(b',') => {
                    index -= 1;
                    break;
                }
                Some(b'/') => self.ops.push(Op::Separator),
                Some(b) => match self.ops[start_ops_index..].last_mut() {
                    Some(Op::Slice { to, .. }) if *to == self.bytes.len() as u16 => {
                        self.bytes.push(b);
                        *to += 1;
                    }
                    _ => {
                        let from = self.bytes.len() as _;
                        let to = from + 1;
                        self.bytes.push(b);
                        self.ops.push(Op::Slice { from, to });
                    }
                },
            }
        }

        Ok(index)
    }

    pub fn matches(&self, path: &[u8]) -> bool {
        matches_recursive(&self.ops, &self.bytes, path, &Continuation::None)
    }
}

enum Continuation<'this, 'ops> {
    None,
    Next(&'ops [Op], &'this Continuation<'this, 'ops>),
}

fn matches_recursive<'data, 'cont>(
    mut ops: &'data [Op],
    bytes: &'data [u8],
    mut path: &'data [u8],
    continuation: &'cont Continuation<'cont, 'data>,
) -> bool {
    macro_rules! advance {
        ($slice:ident, $len:expr) => {
            $slice = &$slice[$len..]
        };
    }

    #[inline]
    fn is_path_separator(b: &u8) -> bool {
        std::path::is_separator(*b as _)
    }

    'op_loop: loop {
        let op = match ops.split_first() {
            Some((op, rest)) => {
                ops = rest;
                op
            }
            None => match continuation {
                Continuation::None => return path.is_empty(),
                Continuation::Next(ops, continuation) => {
                    return matches_recursive(ops, bytes, path, continuation)
                }
            },
        };

        match op {
            Op::Slice { from, to } => {
                let prefix = &bytes[(*from as usize)..(*to as usize)];
                if !path.starts_with(prefix) {
                    return false;
                }
                advance!(path, prefix.len());
            }
            Op::Separator => {
                if path.is_empty() || !is_path_separator(&path[0]) {
                    return false;
                }
                advance!(path, 1);
            }
            Op::Skip { len } => {
                let len = *len as usize;
                if path.len() < len || path[..len].iter().any(is_path_separator) {
                    return false;
                }
                advance!(path, len);
            }
            Op::Many => loop {
                if matches_recursive(ops, bytes, path, continuation) {
                    return true;
                }
                if path.is_empty() || is_path_separator(&path[0]) {
                    return false;
                } else {
                    advance!(path, 1);
                }
            },
            Op::ManyComponents => loop {
                if matches_recursive(ops, bytes, path, continuation) {
                    return true;
                }
                if path.is_empty() {
                    return false;
                }
                advance!(path, 1);
                match path.iter().position(is_path_separator) {
                    Some(i) => advance!(path, i),
                    None => return false,
                }
            },
            Op::AnyWithinRanges { start, count } => {
                if path.is_empty() {
                    return false;
                }
                let b = path[0];
                advance!(path, 1);
                for range in bytes[(*start as usize)..].chunks(2).take(*count as _) {
                    let start = range[0];
                    let end = range[1];
                    if start <= b && b <= end {
                        continue 'op_loop;
                    }
                }
                return false;
            }
            Op::ExceptWithinRanges { start, count } => {
                if path.is_empty() {
                    return false;
                }
                let b = path[0];
                advance!(path, 1);
                for range in bytes[(*start as usize)..].chunks(2).take(*count as _) {
                    let start = range[0];
                    let end = range[1];
                    if b < start || end < b {
                        continue 'op_loop;
                    }
                }
                return false;
            }
            Op::SubPatternGroup { len } => {
                let jump = &ops[(*len as usize)..];
                loop {
                    let len = match ops[0] {
                        Op::SubPattern { len } => len as usize,
                        _ => return false,
                    };
                    advance!(ops, 1);
                    let continuation = Continuation::Next(jump, continuation);
                    if matches_recursive(&ops[..len], bytes, path, &continuation) {
                        return true;
                    }
                    advance!(ops, len);
                }
            }
            Op::SubPattern { .. } => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile() {
        let mut glob = Glob::default();

        assert!(glob.compile(b"").is_ok());
        assert!(glob.compile(b"abc").is_ok());
        assert!(glob.compile(b"a?c").is_ok());
        assert!(glob.compile(b"a[A-Z]c").is_ok());
        assert!(glob.compile(b"a[!0-9]c").is_ok());

        assert!(glob.compile(b"a*c").is_ok());
        assert!(glob.compile(b"a*/").is_ok());
        assert!(glob.compile(b"a*/c").is_ok());
        assert!(glob.compile(b"a*[0-9]/c").is_ok());
        assert!(glob.compile(b"a*bx*cy*d").is_ok());

        assert!(glob.compile(b"a**/").is_ok());
        assert!(glob.compile(b"a**/c").is_ok());
        assert!(glob.compile(b"a**c").is_err());

        assert!(glob.compile(b"a{b,c}d").is_ok());
        assert!(glob.compile(b"a*{b,c}d").is_ok());
        assert!(glob.compile(b"a*{b*,c}d").is_ok());
        assert!(glob.compile(b"}").is_err());
        assert!(glob.compile(b",").is_err());
    }

    #[test]
    fn matches() {
        let mut glob = Glob::default();

        macro_rules! assert_glob {
            ($expected:expr, $pattern:expr, $path:expr) => {
                if glob.compile($pattern).is_err() {
                    panic!(
                        "invalid glob pattern '{}'",
                        std::str::from_utf8($pattern).unwrap()
                    );
                }
                assert_eq!(
                    $expected,
                    glob.matches($path),
                    "'{}' did{} match pattern '{}'",
                    std::str::from_utf8($path).unwrap(),
                    if $expected { " not" } else { "" },
                    std::str::from_utf8($pattern).unwrap(),
                );
            };
        }

        assert_glob!(true, b"", b"");
        assert_glob!(true, b"abc", b"abc");
        assert_glob!(false, b"ab", b"abc");
        assert_glob!(true, b"a?c", b"abc");
        assert_glob!(false, b"a??", b"a/c");
        assert_glob!(true, b"a[A-Z]c", b"aBc");
        assert_glob!(false, b"a[A-Z]c", b"abc");
        assert_glob!(true, b"a[!0-9A-CD-FGH]c", b"abc");

        assert_glob!(true, b"*", b"");
        assert_glob!(true, b"*", b"a");
        assert_glob!(true, b"*", b"abc");
        assert_glob!(true, b"a*c", b"ac");
        assert_glob!(true, b"a*c", b"abc");
        assert_glob!(true, b"a*c", b"abbbc");
        assert_glob!(true, b"a*/", b"abc/");
        assert_glob!(true, b"a*/c", b"a/c");
        assert_glob!(true, b"a*/c", b"abbb/c");
        assert_glob!(true, b"a*[0-9]/c", b"abbb5/c");
        assert_glob!(false, b"a*c", b"a/c");
        assert_glob!(true, b"a*bx*cy*d", b"a00bx000cy0000d");

        assert_glob!(true, b"a**/c", b"a/c");
        assert_glob!(true, b"a**/c", b"a/b/c");
        assert_glob!(true, b"a**/c", b"a/bb/bbb/c");
        assert_glob!(true, b"a**/c", b"aaaaa/bb/bbb/c");

        assert_glob!(true, b"a{b,c}d", b"abd");
        assert_glob!(true, b"a{b,c}d", b"acd");
        assert_glob!(true, b"a*{b,c}d", b"aaabd");
        assert_glob!(true, b"a*{b,c}d", b"abbbd");
        assert_glob!(true, b"a*{b*,c}d", b"acdbbczzcd");
        assert_glob!(true, b"a{b,c*}d", b"aczd");
        assert_glob!(true, b"a*{b,c*}d", b"acdbczzzd");
    }
}