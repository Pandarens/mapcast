"""Reads the data tables addons ship as Lua source (`ns.Name = { ... }`), literals only.

Tables with only positional entries become lists; tables with named or bracketed keys become
dicts, their positional entries under the keys 1, 2, ... as in Lua. Bare names other than
true / false / nil (OpenMap's colour constants, say) read as None.
"""
from __future__ import annotations

import re

_TOKEN = re.compile(r"""
    (?P<space>\s+|--\[(?P<eq>=*)\[.*?\](?P=eq)\]|--[^\n]*)
  | (?P<str>"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*')
  | (?P<num>-?(?:0[xX][0-9a-fA-F]+|(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?))
  | (?P<name>[A-Za-z_][A-Za-z0-9_]*)
  | (?P<op>[{}\[\]=,;])
""", re.S | re.X)

_ESCAPES = {"n": "\n", "t": "\t", "r": "\r", "a": "\a", "b": "\b", "f": "\f", "v": "\v",
            "\\": "\\", '"': '"', "'": "'", "\n": "\n"}


def _unquote(raw: str) -> str:
    body = raw[1:-1]
    if "\\" not in body:
        return body
    out = bytearray()
    i = 0
    while i < len(body):
        ch = body[i]
        if ch != "\\":
            out += ch.encode("utf-8")
            i += 1
            continue
        nxt = body[i + 1]
        digits = re.match(r"\d{1,3}", body[i + 1:])
        if digits:
            out.append(int(digits.group()) & 255)  # \ddd is a byte
            i += 1 + len(digits.group())
        else:
            out += _ESCAPES.get(nxt, nxt).encode("utf-8")
            i += 2
    return out.decode("utf-8", errors="replace")


class _Parser:
    def __init__(self, text: str, pos: int):
        self.tokens = []
        for m in _TOKEN.finditer(text, pos):
            kind = m.lastgroup
            if kind == "eq":
                kind = "space"
            if kind != "space":
                self.tokens.append((kind, m.group(kind)))
        self.i = 0

    def peek(self, offset: int = 0):
        j = self.i + offset
        return self.tokens[j] if j < len(self.tokens) else ("eof", "")

    def take(self, value: str | None = None):
        tok = self.peek()
        if value is not None and tok[1] != value:
            raise ValueError(f"expected {value!r}, got {tok[1]!r}")
        self.i += 1
        return tok

    def value(self):
        kind, text = self.take()
        if kind == "str":
            return _unquote(text)
        if kind == "num":
            return int(text, 0) if re.fullmatch(r"-?(0[xX][0-9a-fA-F]+|\d+)", text) else float(text)
        if kind == "name":
            return {"true": True, "false": False}.get(text)
        if text == "{":
            return self.table()
        raise ValueError(f"unexpected {text!r}")

    def table(self):
        array, named = [], {}
        while self.peek()[1] != "}":
            kind, text = self.peek()
            if text == "[":
                self.take("[")
                key = self.value()
                self.take("]")
                self.take("=")
                named[key] = self.value()
            elif kind == "name" and self.peek(1)[1] == "=":
                self.take()
                self.take("=")
                named[text] = self.value()
            else:
                array.append(self.value())
            if self.peek()[1] in (",", ";"):
                self.take()
            elif self.peek()[1] != "}":
                raise ValueError(f"expected , or }} but got {self.peek()[1]!r}")
        self.take("}")
        if not named:
            return array
        for n, v in enumerate(array, 1):
            named[n] = v
        return named


def read_table(text: str, name: str):
    """The table assigned to `ns.<name>` (or a global `<name>`) in a Lua file."""
    match = re.search(rf"(?:\bns\.|^){re.escape(name)}\s*=\s*\{{", text, re.M)
    if not match:
        raise ValueError(f"no table {name}")
    parser = _Parser(text, match.end() - 1)
    parser.take("{")
    return parser.table()


def positional(table) -> list:
    """A table's positional entries (1..n) as a list."""
    if isinstance(table, list):
        return table
    if isinstance(table, dict):
        out = []
        while len(out) + 1 in table:
            out.append(table[len(out) + 1])
        return out
    return []


def field(table, key, default=None):
    return table.get(key, default) if isinstance(table, dict) else default
