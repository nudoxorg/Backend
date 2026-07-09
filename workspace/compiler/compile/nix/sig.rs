//! A recursive-descent parser for the informal Nix `::` type-signature
//! convention — the first of its kind. `nix-doc` stores signatures as raw
//! strings, `pesto` captures the code block verbatim, `nixdoc` keeps
//! `fn_type: Option<String>`; nobody parses the grammar into a type AST.
//!
//! Grammar (recoverable by recursive descent, loosest → tightest):
//! ```text
//! sig      ::= (ident '::')? typeExpr
//! typeExpr ::= unionExpr ('->' typeExpr)?          -- arrow, right-assoc, loosest
//! unionExpr::= postfix ('|' postfix)*
//! postfix  ::= app '?'*                            -- `T?` nullable
//! app      ::= atom atom*                          -- `Option a`, `AttrSet String`
//! atom     ::= '(' typeExpr ')' | '[' typeExpr ']' | '{' fields '}' | ident
//! fields   ::= (ident '::' typeExpr ';')* ('...' ('::' typeExpr)?)?
//! ```
//!
//! Vocabulary: `String Int Float Bool Null Path Any AttrSet Derivation Module`
//! and `Option a`; lowercase idents are type variables (`GenericParam`);
//! `T?` and `Null | T` both normalise to `Union[T, Null]`.
//!
//! Parse failures return `None` — the caller then keeps the raw string in the
//! documentation only, never blocking ingest on the informal grammar.

use ir::generics::GenericArg;
use ir::parameter::{LiteralParameter, Parameter};
use ir::primitives::{Primitive, Width};
use ir::record::{Field, FieldAttributes, FieldKey, IndexSignature, KnownField, Record};
use ir::ty::{FunctionPointer, GenericParam, Type, TypeReference};

/// A parsed signature: an optional leading name, the arrow chain flattened
/// into parameter types, and the final result type.
#[derive(Debug, Clone)]
pub struct Signature {
    pub name:   Option<String>,
    /// Flattened arrow inputs (`a -> b -> c` ⇒ `[a, b]`). Empty for a plain type.
    pub params: Vec<Type>,
    /// The final arrow result (or the whole type when there is no arrow).
    pub ret:    Type,
}

impl Signature {
    /// Reconstruct the signature as a single [`Type`] — a [`FunctionPointer`]
    /// when it has parameters, otherwise the plain result type. Used for
    /// `TypeAlias` entries and render round-tripping.
    pub fn to_type(&self) -> Type {
        if self.params.is_empty() {
            return self.ret.clone();
        }
        Type::FunctionPointer(FunctionPointer {
            inputs:     Some(self.params.iter().cloned().map(literal_param).collect()),
            outputs:    Some(vec![literal_param(self.ret.clone())]),
            attributes: None,
        })
    }
}

fn literal_param(ty: Type) -> Parameter {
    Parameter::Literal(LiteralParameter {
        name:          String::new(),
        r#type:        Some(ty),
        attributes:    None,
        default_value: None,
        description:   None,
    })
}

/// Parse a full signature (`name :: type` or a bare `type`). Returns `None` on
/// any grammar violation.
pub fn parse(input: &str) -> Option<Signature> {
    let tokens = tokenize(input)?;
    let mut p = Parser { tokens: &tokens, pos: 0 };

    // Optional leading `name ::`.
    let name = match (p.peek(), p.peek_at(1)) {
        (Some(Tok::Ident(id)), Some(Tok::ColonColon)) => {
            let id = id.clone();
            p.pos += 2;
            Some(id)
        }
        _ => None,
    };

    let ty = p.type_expr()?;
    if !p.at_end() {
        return None; // trailing garbage → reject
    }

    // Flatten the arrow chain into params + ret.
    let (params, ret) = flatten_arrows(ty);
    Some(Signature { name, params, ret })
}

/// Parse a bare type expression (no `name ::`, no arrow flattening).
pub fn parse_type(input: &str) -> Option<Type> {
    let tokens = tokenize(input)?;
    let mut p = Parser { tokens: &tokens, pos: 0 };
    let ty = p.type_expr()?;
    p.at_end().then_some(ty)
}

/// Walk a right-associated `FunctionPointer` nest into a flat `(params, ret)`.
fn flatten_arrows(ty: Type) -> (Vec<Type>, Type) {
    let mut params = Vec::new();
    let mut current = ty;
    loop {
        match current {
            Type::FunctionPointer(ref fp) => {
                // Our parser always builds single-input FunctionPointers for
                // arrows (see `arrow`), so this unwinds the chain cleanly.
                let input = fp
                    .inputs
                    .as_ref()
                    .and_then(|v| (v.len() == 1).then(|| v[0].clone()))
                    .and_then(param_type);
                let output = fp
                    .outputs
                    .as_ref()
                    .and_then(|v| (v.len() == 1).then(|| v[0].clone()))
                    .and_then(param_type);
                match (input, output) {
                    (Some(i), Some(o)) => {
                        params.push(i);
                        current = o;
                    }
                    _ => break,
                }
            }
            other => {
                current = other;
                break;
            }
        }
    }
    (params, current)
}

fn param_type(p: Parameter) -> Option<Type> {
    match p {
        Parameter::Literal(l) => l.r#type,
        _ => None,
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tokenizer
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    ColonColon, // ::
    Arrow,      // ->
    Ellipsis,   // ...
    Pipe,       // |
    Question,   // ?
    Semi,       // ;
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
}

fn tokenize(input: &str) -> Option<Vec<Tok>> {
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            c if c.is_whitespace() => i += 1,
            ':' if bytes.get(i + 1) == Some(&b':') => {
                out.push(Tok::ColonColon);
                i += 2;
            }
            '-' if bytes.get(i + 1) == Some(&b'>') => {
                out.push(Tok::Arrow);
                i += 2;
            }
            '.' if bytes.get(i + 1) == Some(&b'.') && bytes.get(i + 2) == Some(&b'.') => {
                out.push(Tok::Ellipsis);
                i += 3;
            }
            '|' => {
                out.push(Tok::Pipe);
                i += 1;
            }
            '?' => {
                out.push(Tok::Question);
                i += 1;
            }
            ';' => {
                out.push(Tok::Semi);
                i += 1;
            }
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '[' => {
                out.push(Tok::LBracket);
                i += 1;
            }
            ']' => {
                out.push(Tok::RBracket);
                i += 1;
            }
            '{' => {
                out.push(Tok::LBrace);
                i += 1;
            }
            '}' => {
                out.push(Tok::RBrace);
                i += 1;
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < bytes.len() {
                    let ch = bytes[i] as char;
                    if ch.is_ascii_alphanumeric() || ch == '_' || ch == '\'' {
                        i += 1;
                    } else {
                        break;
                    }
                }
                out.push(Tok::Ident(input[start..i].to_string()));
            }
            // Any other character (commas, operators we don't model) → reject.
            _ => return None,
        }
    }
    Some(out)
}

// ───────────────────────────────────────────────────────────────────────────
// Parser
// ───────────────────────────────────────────────────────────────────────────

struct Parser<'a> {
    tokens: &'a [Tok],
    pos:    usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }
    fn peek_at(&self, n: usize) -> Option<&Tok> {
        self.tokens.get(self.pos + n)
    }
    fn at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }
    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == Some(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// `typeExpr := unionExpr ('->' typeExpr)?` — arrow is loosest, right-assoc.
    fn type_expr(&mut self) -> Option<Type> {
        let lhs = self.union_expr()?;
        if self.eat(&Tok::Arrow) {
            let rhs = self.type_expr()?;
            return Some(Type::FunctionPointer(FunctionPointer {
                inputs:     Some(vec![literal_param(lhs)]),
                outputs:    Some(vec![literal_param(rhs)]),
                attributes: None,
            }));
        }
        Some(lhs)
    }

    /// `unionExpr := postfix ('|' postfix)*` — normalising `Null | T`.
    fn union_expr(&mut self) -> Option<Type> {
        let mut members = vec![self.postfix()?];
        while self.eat(&Tok::Pipe) {
            members.push(self.postfix()?);
        }
        if members.len() == 1 {
            Some(members.pop().unwrap())
        } else {
            Some(Type::Union(members))
        }
    }

    /// `postfix := app '?'*` — `T?` becomes `Union[T, Null]`.
    fn postfix(&mut self) -> Option<Type> {
        let mut ty = self.app()?;
        while self.eat(&Tok::Question) {
            ty = Type::Union(vec![ty, null_type()]);
        }
        Some(ty)
    }

    /// `app := atom atom*` — type application (`Option a`).
    fn app(&mut self) -> Option<Type> {
        let head = self.atom()?;
        let mut args: Vec<GenericArg> = Vec::new();
        while self.starts_atom() {
            args.push(GenericArg::Type(self.atom()?));
        }
        if args.is_empty() {
            return Some(head);
        }
        // Application only makes sense on a named head; fold the args into it.
        match head {
            Type::TypeReference(mut r) => {
                r.generic_args = Some(args);
                Some(Type::TypeReference(r))
            }
            Type::GenericParam(g) => Some(Type::TypeReference(TypeReference {
                identifier:   g.name,
                generic_args: Some(args),
            })),
            _ => None,
        }
    }

    fn starts_atom(&self) -> bool {
        matches!(
            self.peek(),
            Some(Tok::Ident(_)) | Some(Tok::LParen) | Some(Tok::LBracket) | Some(Tok::LBrace)
        )
    }

    /// `atom := '(' typeExpr ')' | '[' typeExpr ']' | '{' fields '}' | ident`
    fn atom(&mut self) -> Option<Type> {
        match self.peek()?.clone() {
            Tok::LParen => {
                self.pos += 1;
                let inner = self.type_expr()?;
                self.eat(&Tok::RParen).then_some(inner)
            }
            Tok::LBracket => {
                self.pos += 1;
                let inner = self.type_expr()?;
                self.eat(&Tok::RBracket)
                    .then_some(Type::Slice(Box::new(inner)))
            }
            Tok::LBrace => {
                self.pos += 1;
                self.record()
            }
            Tok::Ident(name) => {
                self.pos += 1;
                Some(name_to_type(&name))
            }
            _ => None,
        }
    }

    /// `fields := (ident '::' typeExpr ';')* ('...' ('::' typeExpr)?)?`
    fn record(&mut self) -> Option<Type> {
        let mut fields: Vec<Field> = Vec::new();
        let mut index_sig: Option<IndexSignature> = None;
        loop {
            match self.peek()?.clone() {
                Tok::RBrace => {
                    self.pos += 1;
                    break;
                }
                Tok::Ellipsis => {
                    self.pos += 1;
                    // Optional `... :: T` rest type; else Any-valued rest.
                    let value = if self.eat(&Tok::ColonColon) {
                        self.type_expr()?
                    } else {
                        Type::Any
                    };
                    index_sig = Some(IndexSignature {
                        key_type:   Box::new(Type::Primitive(Primitive::String)),
                        value_type: Box::new(value),
                    });
                    self.eat(&Tok::Semi);
                }
                Tok::Ident(name) => {
                    self.pos += 1;
                    if !self.eat(&Tok::ColonColon) {
                        return None;
                    }
                    let ty = self.type_expr()?;
                    fields.push(Field::Known(KnownField {
                        key:           FieldKey::Ident(name),
                        r#type:        Some(Box::new(ty)),
                        default_value: None,
                        attributes:    FieldAttributes {
                            decorators:  Vec::new(),
                            is_mutable:  false,
                            is_optional: false,
                            is_static:   false,
                        },
                        visibility:    None,
                        documentation: None,
                    }));
                    self.eat(&Tok::Semi);
                }
                _ => return None,
            }
        }
        Some(Type::RecordLiteral(Box::new(Record {
            name:                  None,
            generics:              None,
            fields,
            call_signatures:       None,
            constructors:          None,
            methods:               None,
            index_signatures:      index_sig.map(|s| vec![s]),
            super_types:           None,
            members:               None,
            implemented_protocols: None,
        })))
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Vocabulary
// ───────────────────────────────────────────────────────────────────────────

/// The literal `Null` type used in nullable unions.
fn null_type() -> Type {
    Type::TypeReference(TypeReference { identifier: "Null".to_string(), generic_args: None })
}

/// Map an identifier to its type: known primitives/typenames, else a lowercase
/// type variable becomes a `GenericParam` and an uppercase name a reference.
fn name_to_type(name: &str) -> Type {
    match name {
        "String" => Type::Primitive(Primitive::String),
        "Int" => Type::Primitive(Primitive::Int(Width::W64)),
        "Float" => Type::Primitive(Primitive::Float(Width::W64)),
        "Bool" => Type::Primitive(Primitive::Bool),
        "Any" => Type::Any,
        "Null" | "Path" | "AttrSet" | "Derivation" | "Module" | "List" | "Package" => {
            Type::TypeReference(TypeReference {
                identifier:   name.to_string(),
                generic_args: None,
            })
        }
        _ => {
            if name.starts_with(|c: char| c.is_ascii_lowercase()) {
                Type::GenericParam(GenericParam { name: name.to_string(), kind: None })
            } else {
                Type::TypeReference(TypeReference { identifier: name.to_string(), generic_args: None })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_named_arrow_chain() {
        let sig = parse("mapAttrs :: (String -> a -> b) -> AttrSet a -> AttrSet b").unwrap();
        assert_eq!(sig.name.as_deref(), Some("mapAttrs"));
        // Two arrow inputs at top level: the grouped `(…)` and `AttrSet a`.
        assert_eq!(sig.params.len(), 2);
        // The result is a reference to AttrSet.
        assert!(matches!(sig.ret, Type::TypeReference(ref r) if r.identifier == "AttrSet"));
    }

    #[test]
    fn slice_and_primitives() {
        let ty = parse_type("[String]").unwrap();
        match ty {
            Type::Slice(inner) => assert!(matches!(*inner, Type::Primitive(Primitive::String))),
            other => panic!("expected slice, got {other:?}"),
        }
    }

    #[test]
    fn nullable_normalises_to_union_with_null() {
        let ty = parse_type("String?").unwrap();
        match ty {
            Type::Union(members) => {
                assert_eq!(members.len(), 2);
                assert!(matches!(members[0], Type::Primitive(Primitive::String)));
                assert!(matches!(members[1], Type::TypeReference(ref r) if r.identifier == "Null"));
            }
            other => panic!("expected union, got {other:?}"),
        }
    }

    #[test]
    fn record_literal_fields() {
        let ty = parse_type("{ name :: String; version :: String?; }").unwrap();
        match ty {
            Type::RecordLiteral(rec) => assert_eq!(rec.fields.len(), 2),
            other => panic!("expected record, got {other:?}"),
        }
    }

    #[test]
    fn open_record_gets_index_signature() {
        let ty = parse_type("{ name :: String; ... }").unwrap();
        match ty {
            Type::RecordLiteral(rec) => {
                assert_eq!(rec.fields.len(), 1);
                assert!(rec.index_signatures.is_some());
            }
            other => panic!("expected open record, got {other:?}"),
        }
    }

    #[test]
    fn type_variable_is_generic_param() {
        assert!(matches!(parse_type("a"), Some(Type::GenericParam(_))));
        assert!(matches!(
            parse_type("String"),
            Some(Type::Primitive(Primitive::String))
        ));
    }

    #[test]
    fn application_folds_into_reference_args() {
        let ty = parse_type("Option a").unwrap();
        match ty {
            Type::TypeReference(r) => {
                assert_eq!(r.identifier, "Option");
                assert_eq!(r.generic_args.as_ref().map(|a| a.len()), Some(1));
            }
            other => panic!("expected reference, got {other:?}"),
        }
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse_type("String ->").is_none());
        assert!(parse_type("{ unterminated").is_none());
        assert!(parse_type("a , b").is_none());
    }
}
