//! Bounded shell parsing and path handling, also tested on the host.

pub const MAX_LINE: usize = 256;
const MAX_ARGS: usize = 16;

pub struct Command {
    bytes: [u8; MAX_LINE],
    spans: [(usize, usize); MAX_ARGS],
    count: usize,
}

impl Command {
    pub fn parse(line: &str) -> Result<Self, &'static str> {
        let mut cmd = Self {
            bytes: [0; MAX_LINE],
            spans: [(0, 0); MAX_ARGS],
            count: 0,
        };
        let mut end = 0;
        let mut chars = line.chars().peekable();
        while chars.peek().is_some() {
            while chars.peek().is_some_and(|c| c.is_ascii_whitespace()) {
                chars.next();
            }
            if chars.peek().is_none() {
                break;
            }
            if cmd.count == MAX_ARGS {
                return Err("too many arguments");
            }
            let start = end;
            let mut quote = None;
            while let Some(&c) = chars.peek() {
                if quote.is_none() && c.is_ascii_whitespace() {
                    break;
                }
                chars.next();
                if Some(c) == quote {
                    quote = None;
                    continue;
                }
                if quote.is_none() && (c == '\'' || c == '"') {
                    quote = Some(c);
                    continue;
                }
                if !c.is_ascii() {
                    return Err("only ASCII input is supported");
                }
                if end == MAX_LINE {
                    return Err("command too long");
                }
                cmd.bytes[end] = c as u8;
                end += 1;
            }
            if quote.is_some() {
                return Err("unclosed quote");
            }
            cmd.spans[cmd.count] = (start, end);
            cmd.count += 1;
        }
        Ok(cmd)
    }

    pub fn len(&self) -> usize {
        self.count
    }
    pub fn arg(&self, i: usize) -> &str {
        let (start, end) = self.spans[i];
        core::str::from_utf8(&self.bytes[start..end]).unwrap()
    }
}

#[derive(Clone)]
pub struct Path {
    bytes: [u8; MAX_LINE],
    len: usize,
}

impl Path {
    pub fn root() -> Self {
        let mut p = Self {
            bytes: [0; MAX_LINE],
            len: 1,
        };
        p.bytes[0] = b'/';
        p
    }
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap()
    }
    pub fn resolve(&self, name: &str) -> Result<Self, &'static str> {
        if !name.is_ascii() || name.contains(':') {
            return Err("use an ASCII path without a drive letter");
        }
        let mut path = if name.starts_with(['/', '\\']) {
            Self::root()
        } else {
            self.clone()
        };
        for part in name.split(['/', '\\']) {
            match part {
                "" | "." => {}
                ".." => {
                    while path.len > 1 && path.bytes[path.len - 1] != b'/' {
                        path.len -= 1;
                    }
                    if path.len > 1 {
                        path.len -= 1;
                    }
                }
                _ => {
                    let slash = usize::from(path.len > 1);
                    if path.len + slash + part.len() > MAX_LINE {
                        return Err("path too long");
                    }
                    if slash != 0 {
                        path.bytes[path.len] = b'/';
                        path.len += 1;
                    }
                    path.bytes[path.len..path.len + part.len()].copy_from_slice(part.as_bytes());
                    path.len += part.len();
                }
            }
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quotes_and_empty_arguments() {
        let c = Command::parse(" COPY \"a b\" 'c d' \"\" ").unwrap();
        assert_eq!(c.len(), 4);
        assert_eq!(
            (c.arg(0), c.arg(1), c.arg(2), c.arg(3)),
            ("COPY", "a b", "c d", "")
        );
        assert!(Command::parse("type 'unfinished").is_err());
        assert_eq!(Command::parse("  \t").unwrap().len(), 0);
    }
    #[test]
    fn limits() {
        assert!(Command::parse(&"x".repeat(MAX_LINE + 1)).is_err());
        assert!(Command::parse(&"x ".repeat(MAX_ARGS + 1)).is_err());
        assert!(Command::parse("é").is_err());
    }
    #[test]
    fn paths() {
        let p = Path::root().resolve("BIN/sub").unwrap();
        assert_eq!(
            p.resolve(".././HELLO.ELF").unwrap().as_str(),
            "/BIN/HELLO.ELF"
        );
        assert_eq!(p.resolve("\\BIN\\..\\test").unwrap().as_str(), "/test");
        assert_eq!(p.resolve("../../../../").unwrap().as_str(), "/");
        assert!(p.resolve("C:\\BIN").is_err());
        assert!(p.resolve(&"x".repeat(MAX_LINE)).is_err());
    }
}
