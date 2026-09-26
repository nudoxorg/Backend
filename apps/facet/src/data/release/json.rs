//! A small JSON reader for the release fixture (tests and gallery scenes).
//! The product gets its releases from the index, never from JSON; this
//! exists so the fixture can be read without a new dependency.

use std::collections::BTreeMap;

/// A JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// A number.
    Num(f64),
    /// A string.
    Str(String),
    /// An array.
    Arr(Vec<Json>),
    /// An object (key order is not kept).
    Obj(BTreeMap<String, Json>),
}

impl Json {
    /// A field of an object.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Self::Obj(map) => map.get(key),
            _ => None,
        }
    }

    /// The string, if this is one.
    #[must_use]
    pub fn str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The array's items (empty for anything else).
    #[must_use]
    pub fn items(&self) -> &[Json] {
        match self {
            Self::Arr(items) => items,
            _ => &[],
        }
    }

    /// The object's entries (empty for anything else).
    pub fn entries(&self) -> impl Iterator<Item = (&String, &Json)> {
        match self {
            Self::Obj(map) => Some(map.iter()),
            _ => None,
        }
        .into_iter()
        .flatten()
    }

    /// `true` for `true`.
    #[must_use]
    pub fn truthy(&self) -> bool {
        matches!(self, Self::Bool(true))
    }

    /// The number, if this is one.
    #[must_use]
    pub fn num(&self) -> Option<f64> {
        match self {
            Self::Num(n) => Some(*n),
            _ => None,
        }
    }
}

/// Parses `text`.
///
/// # Errors
/// The byte offset where the text stops being JSON.
pub fn parse(text: &str) -> Result<Json, usize> {
    let mut p = Parser { s: text.as_bytes(), at: 0 };
    let value = p.value()?;
    p.ws();
    if p.at == p.s.len() { Ok(value) } else { Err(p.at) }
}

struct Parser<'a> {
    s: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.at < self.s.len() && self.s[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> Result<(), usize> {
        self.ws();
        if self.s.get(self.at) == Some(&byte) {
            self.at += 1;
            Ok(())
        } else {
            Err(self.at)
        }
    }

    fn value(&mut self) -> Result<Json, usize> {
        self.ws();
        match self.s.get(self.at) {
            Some(b'{') => {
                self.at += 1;
                let mut map = BTreeMap::new();
                self.ws();
                if self.s.get(self.at) == Some(&b'}') {
                    self.at += 1;
                    return Ok(Json::Obj(map));
                }
                loop {
                    self.ws();
                    let key = self.string()?;
                    self.eat(b':')?;
                    let value = self.value()?;
                    map.insert(key, value);
                    self.ws();
                    match self.s.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {
                            self.at += 1;
                            return Ok(Json::Obj(map));
                        }
                        _ => return Err(self.at),
                    }
                }
            }
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                self.ws();
                if self.s.get(self.at) == Some(&b']') {
                    self.at += 1;
                    return Ok(Json::Arr(items));
                }
                loop {
                    items.push(self.value()?);
                    self.ws();
                    match self.s.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b']') => {
                            self.at += 1;
                            return Ok(Json::Arr(items));
                        }
                        _ => return Err(self.at),
                    }
                }
            }
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') if self.s[self.at..].starts_with(b"true") => {
                self.at += 4;
                Ok(Json::Bool(true))
            }
            Some(b'f') if self.s[self.at..].starts_with(b"false") => {
                self.at += 5;
                Ok(Json::Bool(false))
            }
            Some(b'n') if self.s[self.at..].starts_with(b"null") => {
                self.at += 4;
                Ok(Json::Null)
            }
            Some(_) => {
                let start = self.at;
                while self.at < self.s.len() && matches!(self.s[self.at], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    self.at += 1;
                }
                std::str::from_utf8(&self.s[start..self.at])
                    .ok()
                    .and_then(|n| n.parse().ok())
                    .map(Json::Num)
                    .ok_or(start)
            }
            None => Err(self.at),
        }
    }

    fn string(&mut self) -> Result<String, usize> {
        if self.s.get(self.at) != Some(&b'"') {
            return Err(self.at);
        }
        self.at += 1;
        let mut out = Vec::new();
        while let Some(&b) = self.s.get(self.at) {
            self.at += 1;
            match b {
                b'"' => return String::from_utf8(out).map_err(|_| self.at),
                b'\\' => {
                    let e = *self.s.get(self.at).ok_or(self.at)?;
                    self.at += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'u' => {
                            let hex = std::str::from_utf8(self.s.get(self.at..self.at + 4).ok_or(self.at)?).map_err(|_| self.at)?;
                            self.at += 4;
                            let mut code = u32::from_str_radix(hex, 16).map_err(|_| self.at)?;
                            // A surrogate pair.
                            if (0xD800..0xDC00).contains(&code) && self.s[self.at..].starts_with(b"\\u") {
                                let low = std::str::from_utf8(&self.s[self.at + 2..self.at + 6]).map_err(|_| self.at)?;
                                let low = u32::from_str_radix(low, 16).map_err(|_| self.at)?;
                                self.at += 6;
                                code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            }
                            let ch = char::from_u32(code).unwrap_or('\u{fffd}');
                            let mut buf = [0; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        other => out.push(other),
                    }
                }
                other => out.push(other),
            }
        }
        Err(self.at)
    }
}

#[cfg(test)]
mod tests {
    use super::{Json, parse};

    #[test]
    fn reads_nested_values_and_escapes() {
        let v = parse(r#"{"a":[1,2.5,-3e2],"b":{"c":"x\"y→é"},"d":true,"e":null}"#).expect("json");
        assert_eq!(v.get("a").map(|a| a.items().len()), Some(3));
        assert_eq!(v.get("a").and_then(|a| a.items()[2].num()), Some(-300.0));
        assert_eq!(v.get("b").and_then(|b| b.get("c")).and_then(Json::str), Some("x\"y→é"));
        assert!(v.get("d").is_some_and(Json::truthy));
        assert_eq!(v.get("e"), Some(&Json::Null));
        assert!(parse("{\"a\":}").is_err());
    }
}
