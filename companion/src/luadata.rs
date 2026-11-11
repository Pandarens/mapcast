//! Reads the data tables addons ship as Lua source (`ns.Name = { ... }`), literals only.
//!
//! Tables with only positional entries become JSON arrays; tables with named or bracketed keys
//! become objects, keyed by the key as text, their positional entries under "1", "2", ... as in
//! Lua. Bare names other than true / false / nil (OpenMap's colour constants, say) read as null.

use serde_json::{Map, Number, Value};

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Str(String),
    Num(Value),
    Name(String),
    Op(char),
}

fn unquote(body: &str) -> String {
    let bytes = body.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c != b'\\' || i + 1 >= bytes.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let n = bytes[i + 1];
        if n.is_ascii_digit() {
            let mut j = i + 1;
            let mut v: u32 = 0;
            while j < bytes.len() && j < i + 4 && bytes[j].is_ascii_digit() {
                v = v * 10 + (bytes[j] - b'0') as u32;
                j += 1;
            }
            out.push((v & 255) as u8); // \ddd is a byte
            i = j;
            continue;
        }
        let esc = match n {
            b'n' => b'\n',
            b't' => b'\t',
            b'r' => b'\r',
            b'a' => 7,
            b'b' => 8,
            b'f' => 12,
            b'v' => 11,
            other => other,
        };
        out.push(esc);
        i += 2;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn tokenize(text: &str) -> Vec<Tok> {
    let b = text.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'-' && b.get(i + 1) == Some(&b'-') {
            // a comment: --[=*[ ... ]=*] or to the end of the line
            let mut j = i + 2;
            if b.get(j) == Some(&b'[') {
                let mut k = j + 1;
                while b.get(k) == Some(&b'=') {
                    k += 1;
                }
                if b.get(k) == Some(&b'[') {
                    let eqs = k - j - 1;
                    let close = format!("]{}]", "=".repeat(eqs));
                    i = text[k + 1..].find(&close).map(|p| k + 1 + p + close.len()).unwrap_or(b.len());
                    continue;
                }
            }
            while j < b.len() && b[j] != b'\n' {
                j += 1;
            }
            i = j;
        } else if c == b'"' || c == b'\'' {
            let mut j = i + 1;
            while j < b.len() && b[j] != c && b[j] != b'\n' {
                if b[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            toks.push(Tok::Str(unquote(&text[i + 1..j.min(b.len())])));
            i = j + 1;
        } else if c.is_ascii_digit() || (c == b'-' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit() || *d == b'.'))
            || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            let mut j = i + 1;
            if b.get(i) == Some(&b'0') && matches!(b.get(i + 1), Some(b'x') | Some(b'X'))
                || c == b'-' && b.get(i + 1) == Some(&b'0') && matches!(b.get(i + 2), Some(b'x') | Some(b'X'))
            {
                j = i + if c == b'-' { 3 } else { 2 };
                while j < b.len() && b[j].is_ascii_hexdigit() {
                    j += 1;
                }
                let s = &text[i..j];
                let (neg, digits) = s.strip_prefix('-').map(|d| (true, d)).unwrap_or((false, s));
                let v = i64::from_str_radix(&digits[2..], 16).unwrap_or(0);
                toks.push(Tok::Num(Value::from(if neg { -v } else { v })));
            } else {
                while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.'
                    || ((b[j] == b'e' || b[j] == b'E') && j + 1 < b.len())
                    || ((b[j] == b'+' || b[j] == b'-') && matches!(b[j - 1], b'e' | b'E')))
                {
                    j += 1;
                }
                let s = &text[i..j];
                let v = if s.contains(['.', 'e', 'E']) {
                    s.parse::<f64>().ok().and_then(Number::from_f64).map(Value::Number).unwrap_or(Value::Null)
                } else {
                    s.parse::<i64>().map(Value::from).unwrap_or(Value::Null)
                };
                toks.push(Tok::Num(v));
            }
            i = j;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let mut j = i + 1;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            toks.push(Tok::Name(text[i..j].to_string()));
            i = j;
        } else {
            if "{}[]=,;".contains(c as char) {
                toks.push(Tok::Op(c as char));
            }
            i += 1;
        }
    }
    toks
}

struct Parser {
    toks: Vec<Tok>,
    i: usize,
}

impl Parser {
    fn peek(&self, k: usize) -> Option<&Tok> {
        self.toks.get(self.i + k)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.i).cloned();
        self.i += 1;
        t
    }
    fn expect(&mut self, c: char) -> Result<(), String> {
        match self.next() {
            Some(Tok::Op(o)) if o == c => Ok(()),
            other => Err(format!("expected {c:?}, got {other:?}")),
        }
    }
    fn value(&mut self) -> Result<Value, String> {
        match self.next() {
            Some(Tok::Str(s)) => Ok(Value::String(s)),
            Some(Tok::Num(n)) => Ok(n),
            Some(Tok::Name(n)) => Ok(match n.as_str() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => Value::Null,
            }),
            Some(Tok::Op('{')) => self.table(),
            other => Err(format!("unexpected {other:?}")),
        }
    }
    fn table(&mut self) -> Result<Value, String> {
        let mut array = Vec::new();
        let mut named = Map::new();
        loop {
            match self.peek(0) {
                Some(Tok::Op('}')) => break,
                None => return Err("unterminated table".into()),
                Some(Tok::Op('[')) => {
                    self.next();
                    let key = self.value()?;
                    self.expect(']')?;
                    self.expect('=')?;
                    let k = match key {
                        Value::String(s) => s,
                        other => other.to_string(),
                    };
                    named.insert(k, self.value()?);
                }
                Some(Tok::Name(n)) if self.peek(1) == Some(&Tok::Op('=')) => {
                    let n = n.clone();
                    self.next();
                    self.next();
                    named.insert(n, self.value()?);
                }
                _ => array.push(self.value()?),
            }
            match self.peek(0) {
                Some(Tok::Op(',')) | Some(Tok::Op(';')) => {
                    self.next();
                }
                Some(Tok::Op('}')) => {}
                other => return Err(format!("expected , or }} but got {other:?}")),
            }
        }
        self.expect('}')?;
        if named.is_empty() {
            return Ok(Value::Array(array));
        }
        for (n, v) in array.into_iter().enumerate() {
            named.insert((n + 1).to_string(), v);
        }
        Ok(Value::Object(named))
    }
}

/// The table assigned to `ns.<name>` (or a global `<name>` at a line's start) in a Lua file.
pub fn read_table(text: &str, name: &str) -> Result<Value, String> {
    let mut start = None;
    for (pos, _) in text.match_indices(name) {
        let before_ok = text[..pos].ends_with("ns.") || pos == 0 || text[..pos].ends_with('\n');
        let rest = text[pos + name.len()..].trim_start();
        if before_ok && rest.starts_with('=') && rest[1..].trim_start().starts_with('{') {
            start = Some(pos + name.len() + (text[pos + name.len()..].len() - rest.len()) + 1);
            break;
        }
    }
    let start = start.ok_or(format!("no table {name}"))?;
    let brace = text[start..].find('{').ok_or("no table")? + start;
    let mut parser = Parser { toks: tokenize(&text[brace..]), i: 0 };
    parser.expect('{')?;
    parser.table()
}

/// A table's positional entries (1..n) as a list.
pub fn positional(table: &Value) -> Vec<Value> {
    match table {
        Value::Array(a) => a.clone(),
        Value::Object(m) => {
            let mut out = Vec::new();
            while let Some(v) = m.get(&(out.len() + 1).to_string()) {
                out.push(v.clone());
            }
            out
        }
        _ => vec![],
    }
}

/// The `n`th positional entry (1-based), or null.
pub fn at(table: &Value, n: usize) -> Value {
    match table {
        Value::Array(a) => a.get(n - 1).cloned().unwrap_or(Value::Null),
        Value::Object(m) => m.get(&n.to_string()).cloned().unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

pub fn field<'a>(table: &'a Value, key: &str) -> Option<&'a Value> {
    table.as_object().and_then(|m| m.get(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lua_tables() {
        let text = r#"
        local _, ns = ...
        --[[ a block comment { not a table } ]]
        ns.Data = {
            list = { 1, -2.5, 0x10, 1e3, "a\"b\n", 'single', true, false, nil },   -- a comment
            [7] = { "x", y = SOME_CONSTANT, ["key with space"] = 3; },
            nested = { { 1, 2 }, { } },
            escaped = "\208\144",
        }
        ns.Other = { 1 }
        "#;
        let data = read_table(text, "Data").unwrap();
        assert_eq!(data["list"], json!([1, -2.5, 16, 1000.0, "a\"b\n", "single", true, false, null]));
        assert_eq!(data["7"], json!({"1": "x", "y": null, "key with space": 3}));
        assert_eq!(data["nested"], json!([[1, 2], []]));
        assert_eq!(data["escaped"], json!("А"));
        assert_eq!(read_table(text, "Other").unwrap(), json!([1]));
        assert_eq!(positional(&data["7"]), vec![json!("x")]);
        assert!(read_table(text, "Nope").is_err());
    }
}
