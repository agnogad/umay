# 0012 — JSON is read and written over counted bytes

Status: accepted

## Context

An HTTP handler needs to read one or two fields out of a request body and write a
small reply. A JSON tree would mean allocating a node per value, then freeing it,
for a document that lives only as long as the request. The body is already a
`Bytes` — an address and a count — so the document is available in place.

Two questions had to be answered: whether a value becomes a tree, and what a number
is.

## Decision

**A value is read where it sits.** `Json` is an enum whose variants borrow from the
document: `Text`, `Array` and `Object` hold a `Bytes` view, not a copy. The parser
is a `Cursor` over the document, and `json_field(&doc, key)` skips over members
without building them. A caller that wants one field never touches the rest.

**A number is its decimal text, not an `f64`.** `Json::Number` holds `Bytes`.
Converting on the way in would change values: `0.1` has no exact binary float, so a
server that echoes a field back would echo a different number than it received.
`json_number_as_i64` and `json_decimal_places` are there for a caller that wants
arithmetic, and they say plainly what they do.

**A malformed document yields `Json::Null`.** There is no error value a handler
could act on differently from "missing", and a `Result` would only move the same
check to every call site.

**Writing needs a buffer the caller supplies.** A `String` grows by reallocating,
so `json_quote` and `json_write_i64` take a pointer and a capacity and return the
number of bytes written, or `-1` when the buffer was too small. The caller checks
once.

## Consequences

- Reading a request body allocates nothing.
- A reply is assembled into one buffer of the caller's choosing.
- Numbers survive a round trip exactly.
- A caller that needs a parsed tree must build it; the library does not.
- Escapes inside a string are not decoded, because a caller that only compares a
  field does not need them and decoding would mean copying the text out.
