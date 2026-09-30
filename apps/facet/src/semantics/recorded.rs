//! One conservative signature projection, shared by symbol, find and compare.
//! A captured declaration is read only with an explicit source language.
//! Unsupported or unannotated syntax stays unavailable.

use crate::semantics::bounds::generics;
use crate::semantics::members::{is_receiver, params};
use crate::semantics::model::{Input, Pipe, Where};
use crate::semantics::types::{Nowhere, Scope, TypeExpr, parse, split_top};

/// A callable's recorded signature, expressed with the shared type vocabulary.
/// Unsupported or incomplete syntax stays unavailable instead of guessing.
#[must_use]
fn rust_pipe(signature: &str, expected: &str) -> Option<Pipe> {
    let signature = declaration_start(signature)?;
    let start = keyword(signature, "fn")?;
    if !rust_prefix(&signature[..start]) { return None; }
    let before = &signature[..start];
    let declaration = signature[start + 2..].trim_start();
    let mut depth = 0_i32;
    let mut open = None;
    let mut previous = None;
    for (at, ch) in declaration.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' if previous != Some('-') => depth -= 1,
            '(' if depth == 0 => { open = Some(at); break; }
            _ => {}
        }
        previous = Some(ch);
    }
    let open = open?;
    let mut depth = 1_i32;
    let mut close = None;
    for (at, ch) in declaration[open + 1..].char_indices() {
        match ch { '(' => depth += 1, ')' => depth -= 1, _ => {} }
        if depth == 0 { close = Some(open + 1 + at); break; }
    }
    let close = close?;
    let head = declaration[..open].trim();
    let name = head.split('<').next()?.trim().trim_start_matches("r#");
    if (!expected.is_empty() && name != expected.trim_start_matches("r#")) || name.is_empty() || !name.chars().all(|ch| ch.is_alphanumeric() || ch == '_') { return None; }
    let generic = head.find('<').and_then(|at| head.strip_suffix('>').map(|head| &head[at + 1..])).unwrap_or("");
    let tail = declaration[close + 1..].trim().trim_end_matches([';', '{']).trim();
    if tail.contains(['{', '}', ';']) { return None; }
    let (output, where_clause) = keyword(tail, "where").map_or((tail, ""), |at| (&tail[..at], &tail[at + 5..]));
    let bounds = generics(&[generic], where_clause);
    let scope = Scope::new(&Nowhere).generics(bounds.iter().map(|bound| &bound.name));
    let raw = split_top(&declaration[open + 1..close], ',');
    if raw.iter().filter(|p| !p.trim().is_empty() && !is_receiver(p)).any(|p| crate::semantics::types::name_colon(p).is_none()) {
        return None;
    }
    let mut inputs = Vec::new();
    if let Some(receiver) = raw.iter().find(|p| is_receiver(p)) {
        // A by-value receiver is said as taken: Copy is not inferred here.
        let words = if receiver.contains('&') { if receiver.split(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '\'')).any(|word| word == "mut") { "changes it" } else { "reads it" } } else { "takes it" };
        let behavior = if receiver.contains('&') {
            if words == "changes it" { super::members::Receiver::Changes } else { super::members::Receiver::Reads }
        } else { super::members::Receiver::UsesUp };
        inputs.push(Input { name: words.into(), ty: None, receiver: Some(behavior) });
    }
    inputs.extend(params(&raw).into_iter().filter(|p| !p.ty.is_empty()).map(|param| Input {
        name: param.name.into(), ty: Some(scope.spell_text(&param.ty)), receiver: None,
    }));
    let (output, fails) = if output.is_empty() { (None, None) } else {
        let ret = output.strip_prefix("->")?.trim();
        if ret.is_empty() { return None; }
        let expr = parse(ret);
        match scope.fallible(&expr) {
            Some((ok, err)) => ((ok != TypeExpr::Tuple(Vec::new())).then(|| scope.spell(&ok)), Some(err.map(|err| scope.spell(&err)))),
            None => ((expr != TypeExpr::Tuple(Vec::new())).then(|| scope.spell(&expr)), None),
        }
    };
    let wheres = bounds.iter().map(|bound| Where {
        name: bound.name.clone().into(), sentence: scope.sentence(bound), source: bound.bounds.join(" + ").into(),
    }).collect();
    let mut flags = Vec::new();
    if before.split_whitespace().any(|word| word == "async") { flags.push("waits (async)"); }
    if before.split_whitespace().any(|word| word == "unsafe") { flags.push("you uphold its rules (unsafe)"); }
    Some(Pipe { inputs, output, fails, wheres, flags })
}


/// Remove only complete leading comments and Rust attributes. Their contents
/// never participate in finding a declaration name or its parameter list.
fn declaration_start(mut source: &str) -> Option<&str> {
    loop {
        source = source.trim_start();
        if source.starts_with("//") {
            source = source.get(source.find('\n')? + 1..)?;
        } else if source.starts_with("/*") {
            let mut depth = 1; let mut at = 2;
            while depth > 0 {
                let tail = source.get(at..)?;
                if tail.starts_with("/*") { depth += 1; at += 2; }
                else if tail.starts_with("*/") { depth -= 1; at += 2; }
                else { at += tail.chars().next()?.len_utf8(); }
            }
            source = source.get(at..)?;
        } else if source.starts_with("#[") || source.starts_with("#![") {
            let start = source.find('[')?;
            let mut depth = 1; let mut quote = false; let mut escape = false; let mut end = None;
            for (at, ch) in source[start + 1..].char_indices() {
                if quote {
                    if escape { escape = false; }
                    else if ch == '\\' { escape = true; }
                    else if ch == '"' { quote = false; }
                } else {
                    match ch { '"' => quote = true, '[' => depth += 1, ']' => depth -= 1, _ => {} }
                    if depth == 0 { end = Some(start + 1 + at + 1); break; }
                }
            }
            source = source.get(end?..)?;
        } else { return Some(source); }
    }
}

fn rust_prefix(prefix: &str) -> bool {
    let prefix = prefix.trim();
    let prefix = if let Some(after) = prefix.strip_prefix("pub(") {
        let Some(close) = after.find(')') else { return false; };
        &after[close + 1..]
    } else { prefix };
    prefix.split_whitespace().all(|word| matches!(word, "pub" | "async" | "unsafe" | "const" | "default" | "extern" | "\"C\"" | "\"C-unwind\""))
}

fn keyword(text: &str, word: &str) -> Option<usize> {
    let mut depths = [0_i32; 3];
    let mut previous = None;
    for (at, ch) in text.char_indices() {
        if depths == [0, 0, 0] && text[at..].starts_with(word)
            && (at == 0 || !text[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_'))
            && !text[at + word.len()..].chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_') { return Some(at); }
        match ch { '<' => depths[0] += 1, '>' if previous != Some('-') => depths[0] -= 1,
            '(' => depths[1] += 1, ')' => depths[1] -= 1, '[' => depths[2] += 1, ']' => depths[2] -= 1, _ => {} }
        previous = Some(ch);
    }
    None
}


/// The grammar used to read a recorded callable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Language { Rust, TypeScript, Python, Go, Java, CSharp, Cpp, C, Unknown }
impl Language {
    /// The canonical frontend name, read at the shell boundary.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name { "rust" => Self::Rust, "typescript" => Self::TypeScript, "python" => Self::Python,
            "go" => Self::Go, "java" => Self::Java, "csharp" => Self::CSharp, "cpp" => Self::Cpp, "c" => Self::C, _ => Self::Unknown }
    }
}

/// Read a C declaration from the grammar tree. Declarator names are removed
/// only by their syntax-node range; whitespace and token guessing cannot
/// mistake a nested function-pointer parameter for the outer parameter name.
fn c_pipe(signature: &str, expected: &str) -> Option<Pipe> {
    if expected.is_empty() || !expected.bytes().all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
        || expected.as_bytes().first().is_some_and(|byte| byte.is_ascii_digit())
    {
        return None;
    }

    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_c::LANGUAGE.into()).ok()?;
    let tree = parser.parse(signature, None)?;
    let root = tree.root_node();
    if root.has_error() || root.kind() != "translation_unit" { return None; }

    let mut selected = None;
    for index in 0..root.named_child_count() {
        let top = root.named_child(index as u32)?;
        if !matches!(top.kind(), "declaration" | "function_definition") { continue; }
        for child_index in 0..top.named_child_count() {
            let declarator = top.named_child(child_index as u32)?;
            if !c_declarator_kind(declarator.kind()) { continue; }
            let Some((identifier, path)) = c_declarator_path(declarator, signature) else { continue };
            if c_node_text(signature, identifier)? != expected { continue; }

            // The operator nearest the identifier determines what the name
            // declares. A pointer nearer than a function means this is a
            // function-pointer object, not a callable declaration.
            let nearest = path.iter().enumerate().rev().find(|(_, node)| c_operator(node.kind()))?;
            if nearest.1.kind() != "function_declarator" { return None; }
            if selected.is_some() { return None; }
            let function_at = nearest.0;
            selected = Some((top, path, function_at));
        }
    }

    let (declaration, path, function_at) = selected?;
    let function = *path.get(function_at)?;
    let parameters = function.child_by_field_name("parameters")?;
    let scope = Scope::new(&Nowhere);
    let mut inputs = Vec::new();
    let mut variadic = false;
    let mut parameter_count = 0;
    for index in 0..parameters.named_child_count() {
        let parameter = parameters.named_child(index as u32)?;
        match parameter.kind() {
            "parameter_declaration" => {
                parameter_count += 1;
                let (name, ty) = c_parameter(signature, parameter, inputs.len() + 1, &scope)?;
                // In a prototype, the single unqualified `void` parameter is
                // the grammar's spelling for an empty parameter list.
                if ty.source.as_ref() == "void" {
                    if parameter_count != 1 || parameters.named_child_count() != 1 {
                        return None;
                    }
                    continue;
                }
                inputs.push(Input { name: name.into(), ty: Some(ty), receiver: None });
            }
            "variadic_parameter" => {
                if variadic || index + 1 != parameters.named_child_count() { return None; }
                variadic = true;
            }
            // An empty `()` and old-style identifier lists do not tell us the
            // callable's input types. Do not present them as known ports.
            _ => return None,
        }
    }
    if parameter_count == 0 || (variadic && inputs.is_empty()) { return None; }

    let base = c_base_type(signature, declaration)?;
    let mut pointers = String::new();
    for node in path.iter().take(function_at) {
        match node.kind() {
            "pointer_declarator" => {
                let inner = c_declarator_child(*node)?;
                pointers.push_str(c_node_text(signature, *node).and_then(|text| {
                    let cut = inner.start_byte().checked_sub(node.start_byte())?;
                    text.get(..cut)
                })?);
            }
            "parenthesized_declarator" | "attributed_declarator" => {}
            // A function cannot return an array or another function. More
            // involved vendor declarators remain unavailable until their
            // complete return type can be represented without inference.
            _ => return None,
        }
    }
    let result = if pointers.is_empty() { base } else { format!("{base} {pointers}") };
    let output = (result.trim() != "void").then(|| scope.spell_text(&result));
    let flags = if variadic { vec!["accepts additional arguments"] } else { Vec::new() };
    Some(Pipe { inputs, output, fails: None, wheres: Vec::new(), flags })
}

fn c_operator(kind: &str) -> bool {
    matches!(kind, "function_declarator" | "pointer_declarator" | "array_declarator")
}

fn c_declarator_kind(kind: &str) -> bool {
    matches!(kind,
        "identifier" | "pointer_declarator" | "function_declarator" | "array_declarator"
        | "parenthesized_declarator" | "attributed_declarator" | "abstract_pointer_declarator"
        | "abstract_function_declarator" | "abstract_array_declarator" | "abstract_parenthesized_declarator")
}

fn c_declarator_child<'tree>(node: tree_sitter::Node<'tree>) -> Option<tree_sitter::Node<'tree>> {
    if let Some(child) = node.child_by_field_name("declarator") { return Some(child); }
    for index in 0..node.named_child_count() {
        let child = node.named_child(index as u32)?;
        if c_declarator_kind(child.kind()) && child.kind() != "identifier" { return Some(child); }
    }
    None
}

/// The ordered path follows only declarator fields, never parameter-list
/// children. The final node is the operator immediately surrounding the name.
fn c_declarator_path<'tree>(
    node: tree_sitter::Node<'tree>,
    source: &str,
) -> Option<(tree_sitter::Node<'tree>, Vec<tree_sitter::Node<'tree>>)> {
    if node.kind() == "identifier" { return Some((node, Vec::new())); }
    if !c_declarator_kind(node.kind()) { return None; }
    let child = c_declarator_child(node)?;
    let (identifier, mut path) = c_declarator_path(child, source)?;
    let _ = source.get(identifier.byte_range())?;
    path.insert(0, node);
    Some((identifier, path))
}

fn c_node_text<'source>(source: &'source str, node: tree_sitter::Node<'_>) -> Option<&'source str> {
    source.get(node.byte_range())
}

/// Type specifiers and qualifiers form the base type. Storage classes affect
/// declaration placement, not the parameter or result type.
fn c_base_type(source: &str, declaration: tree_sitter::Node<'_>) -> Option<String> {
    let mut parts = Vec::new();
    let mut types = 0;
    for index in 0..declaration.named_child_count() {
        let child = declaration.named_child(index as u32)?;
        match child.kind() {
            "type_specifier" => {
                types += 1;
                parts.push(c_node_text(source, child)?);
            }
            "type_qualifier" => parts.push(c_node_text(source, child)?),
            "storage_class_specifier" | "compound_statement" => {}
            "attribute_specifier" | "attribute_declaration" | "ms_declspec_modifier" | "ms_call_modifier" => return None,
            kind if c_declarator_kind(kind) => {}
            _ => return None,
        }
    }
    (types == 1 && !parts.is_empty()).then(|| parts.join(" "))
}

fn c_parameter(
    source: &str,
    parameter: tree_sitter::Node<'_>,
    ordinal: usize,
    scope: &Scope<'_>,
) -> Option<(String, crate::semantics::types::Spelled)> {
    let mut specifiers = Vec::new();
    let mut types = 0;
    let mut declarator = None;
    for index in 0..parameter.named_child_count() {
        let child = parameter.named_child(index as u32)?;
        match child.kind() {
            "type_specifier" => {
                types += 1;
                specifiers.push(c_node_text(source, child)?);
            }
            "type_qualifier" => specifiers.push(c_node_text(source, child)?),
            "storage_class_specifier" => {}
            "attribute_specifier" | "attribute_declaration" | "ms_declspec_modifier" => return None,
            kind if c_declarator_kind(kind) => {
                if declarator.replace(child).is_some() { return None; }
            }
            _ => return None,
        }
    }
    if types != 1 || specifiers.is_empty() { return None; }
    let mut ty = specifiers.join(" ");
    let name = if let Some(declarator) = declarator {
        let text = c_node_text(source, declarator)?;
        let mut spelling = text.to_owned();
        let label = if let Some((identifier, _)) = c_declarator_path(declarator, source) {
            let name = c_node_text(source, identifier)?.to_owned();
            let range = identifier.byte_range();
            let start = range.start.checked_sub(declarator.start_byte())?;
            let end = range.end.checked_sub(declarator.start_byte())?;
            spelling = format!("{}{}", text.get(..start)?, text.get(end..)?);
            name
        } else {
            format!("argument {ordinal}")
        };
        if !spelling.trim().is_empty() {
            ty.push(' ');
            ty.push_str(spelling.trim());
        }
        label
    } else {
        format!("argument {ordinal}")
    };
    Some((name, scope.spell_text(&ty)))
}

/// Project one recorded declaration into named input ports, a success port,
/// an alternative failure port, bounds and qualifier facts. This says nothing
/// about interchangeability or behavior not expressed by the signature.
#[must_use]
pub fn callable(signature: &str, name: &str, language: Language) -> Option<Pipe> {
    if language == Language::Rust { return rust_pipe(signature, name); }
    if language == Language::C { return c_pipe(signature, name); }
    let signature = declaration_start(signature)?;
    if matches!(language, Language::Unknown | Language::Cpp | Language::Go) || name.is_empty() { return None; }
    let named = signature.match_indices(name).find(|(at, _)| {
        (*at == 0 || !signature[..*at].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_'))
            && !signature[*at + name.len()..].chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_')
    })?.0;
    let prefix = signature[..named].trim();
    // An annotation or comment may mention the same name. Only a declaration
    // head in the explicit grammar can own the outer parameter list.
    if prefix.contains(['\n', '\r', '@', '"', '\'', '(', ')', '{', '}']) { return None; }
    match language {
        Language::Python if !matches!(prefix, "def" | "async def") => return None,
        Language::TypeScript if !matches!(prefix, "function" | "export function" | "export default function" | "async function" | "export async function" | "" | "public" | "private" | "static") => return None,
        _ => {}
    }
    let after = named + name.len();
    let mut depth = 0_i32; let mut previous = None; let mut open = None;
    for (at, ch) in signature[after..].char_indices() {
        match ch { '<' => depth += 1, '>' if previous != Some('-') => depth -= 1, '(' if depth == 0 => { open = Some(after + at); break; }, _ => {} }
        previous = Some(ch);
    }
    let open = open?;
    let mut depth = 1_i32; let mut close = None;
    for (at, ch) in signature[open + 1..].char_indices() {
        match ch { '(' => depth += 1, ')' => depth -= 1, _ => {} }
        if depth == 0 { close = Some(open + 1 + at); break; }
    }
    let close = close?;
    let scope = Scope::new(&Nowhere); let mut inputs = Vec::new();
    for parameter in split_top(&signature[open + 1..close], ',') {
        let parameter = parameter.trim();
        if parameter.is_empty() || parameter == "self" || parameter == "cls" { continue; }
        let parameter = parameter.split_once('=').map_or(parameter, |(p, _)| p).trim();
        let (label, ty) = match language {
            Language::Python | Language::TypeScript => {
                let colon = crate::semantics::types::name_colon(parameter)?;
                (parameter[..colon].trim(), parameter[colon + 1..].trim())
            }
            Language::Go => parameter.split_once(char::is_whitespace)?,
            Language::Java | Language::CSharp | Language::Cpp => {
                let split = parameter.rfind(char::is_whitespace)?;
                (parameter[split..].trim(), parameter[..split].trim())
            }
            _ => return None,
        };
        if ty.is_empty() { return None; }
        inputs.push(Input { name: label.to_owned().into(), ty: Some(scope.spell_text(ty.trim())), receiver: None });
    }
    let tail = signature[close + 1..].trim().trim_end_matches(';').trim();
    let result = match language {
        Language::Python => tail.strip_prefix("->")?.trim().strip_suffix(':').unwrap_or(tail.strip_prefix("->")?.trim()).trim(),
        Language::TypeScript => tail.strip_prefix(':')?.trim(),
        Language::Go => tail,
        Language::Java | Language::CSharp | Language::Cpp => {
            let prefix = signature[..named].trim();
            let mut angle = 0_i32; let mut start = 0;
            for (at, c) in prefix.char_indices().rev() {
                match c { '>' => angle += 1, '<' => angle -= 1, _ => {} }
                if angle == 0 && c.is_whitespace() { start = at + c.len_utf8(); break; }
            }
            let result = prefix[start..].trim();
            if matches!(result, "public" | "private" | "protected" | "static" | "virtual" | "override") || result.is_empty() { return None; }
            result
        }
        Language::C | Language::Unknown => return None,
        _ => return None,
    };
    // Bodies, object-literal returns and trailing statements are unsupported.
    // Never turn a partially read annotation into a known outcome.
    if result.contains(['{', '}', '\n', '\r', ';']) || result.is_empty() { return None; }
    let result = result.trim();
    let output = (!result.is_empty() && !matches!(result, "void" | "None" | "()")) .then(|| scope.spell_text(result));
    Some(Pipe { inputs, output, fails: None, wheres: Vec::new(), flags: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_callable_keeps_input_result_bounds_and_receiver_distinct() {
        let pipe = rust_pipe("pub fn from_str<'de, T>(s: &'de str) -> Result<T, Error> where T: Deserialize<'de>", "").unwrap();
        assert_eq!(pipe.inputs[0].name.as_ref(), "s");
        assert_eq!(pipe.inputs[0].ty.as_ref().unwrap().plain(), "text");
        assert_eq!(pipe.output.as_ref().unwrap().plain(), "T");
        assert_eq!(pipe.fails.as_ref().unwrap().as_ref().unwrap().plain(), "Error");
        assert_eq!(pipe.wheres[0].name.as_ref(), "T");
        let method = rust_pipe("pub fn as_table(&self) -> Option<&Table>", "").unwrap();
        assert_eq!(method.inputs[0].name.as_ref(), "reads it");
        assert!(method.output.unwrap().plain().contains("maybe"));
    }

    #[test]
    fn annotated_languages_use_the_same_typed_ports() {
        let ts = callable("function size(items: Array<Item>): number", "size", Language::TypeScript).unwrap();
        assert_eq!(ts.inputs[0].name.as_ref(), "items");
        assert_eq!(ts.output.unwrap().source.as_ref(), "number");
        let java = callable("public String name(int index)", "name", Language::Java).unwrap();
        assert_eq!(java.inputs[0].ty.as_ref().unwrap().source.as_ref(), "int");
        assert_eq!(java.output.unwrap().source.as_ref(), "String");
        assert!(callable("def size(items)", "size", Language::Python).is_none());
        assert!(callable("function size(items: Item)", "size", Language::TypeScript).is_none());
    }

    #[test]
    fn c_uses_declarator_nodes_for_names_types_and_variadic_arity() {
        let pointer = callable("const char *copy(const char *source);", "copy", Language::C).unwrap();
        assert_eq!(pointer.inputs[0].name.as_ref(), "source");
        assert_eq!(pointer.inputs[0].ty.as_ref().unwrap().source.as_ref(), "const char *");
        assert_eq!(pointer.output.as_ref().unwrap().source.as_ref(), "const char *");

        let definition = callable(
            "static inline char *find(const char *text) { return (char *)text; }",
            "find",
            Language::C,
        ).unwrap();
        assert_eq!(definition.output.as_ref().unwrap().source.as_ref(), "char *");

        let callback = callable(
            "int apply(void *context, int (*callback)(const char *value, unsigned count));",
            "apply",
            Language::C,
        ).unwrap();
        assert_eq!(callback.inputs[1].name.as_ref(), "callback");
        assert_eq!(callback.inputs[1].ty.as_ref().unwrap().source.as_ref(), "int (*)(const char *value, unsigned count)");

        let array = callable("size_t fill(char destination[static 8], size_t capacity);", "fill", Language::C).unwrap();
        assert_eq!(array.inputs[0].name.as_ref(), "destination");
        assert_eq!(array.inputs[0].ty.as_ref().unwrap().source.as_ref(), "char [static 8]");

        let qualified = callable("void accept(int * const * restrict value);", "accept", Language::C).unwrap();
        assert_eq!(qualified.inputs[0].ty.as_ref().unwrap().source.as_ref(), "int * const * restrict");

        let variadic = callable("int log_line(const char *format, ...);", "log_line", Language::C).unwrap();
        assert_eq!(variadic.flags, vec!["accepts additional arguments"]);
        assert_eq!(variadic.inputs[0].name.as_ref(), "format");
    }

    #[test]
    fn c_distinguishes_unnamed_void_empty_and_unprototyped_parameter_lists() {
        let unnamed = callable("void consume(const char *, int);", "consume", Language::C).unwrap();
        assert_eq!(unnamed.inputs[0].name.as_ref(), "argument 1");
        assert_eq!(unnamed.inputs[1].name.as_ref(), "argument 2");
        assert_eq!(unnamed.inputs[0].ty.as_ref().unwrap().source.as_ref(), "const char *");
        assert!(unnamed.output.is_none());

        let empty = callable("void reset(void);", "reset", Language::C).unwrap();
        assert!(empty.inputs.is_empty());
        assert!(empty.output.is_none());
        assert!(callable("void unknown();", "unknown", Language::C).is_none());
        assert!(callable("int (*not_callable)(int);", "not_callable", Language::C).is_none());
        assert!(callable("int broken(int value", "broken", Language::C).is_none());
        assert!(callable("/* expected */ int other(void);", "expected", Language::C).is_none());
    }

    #[test]
    fn nested_callable_inputs_do_not_end_the_outer_signature_and_unknown_is_not_nothing() {
        let pipe = rust_pipe("fn map<F: Fn(i32) -> i32>(f: F, value: Vec<(i32, i32)>) -> Vec<i32>", "").unwrap();
        assert_eq!(pipe.inputs.len(), 2);
        assert_eq!(pipe.inputs[1].name.as_ref(), "value");
        assert!(rust_pipe("fn broken(text: &str", "").is_none());
        assert!(rust_pipe("function parse(text: string): Value", "").is_none());
        assert!(rust_pipe("fn unknown(value)", "").is_none());
        assert_eq!(rust_pipe("fn go() -> Somewhere", "").unwrap().output.unwrap().plain(), "Somewhere");
    }
    #[test]
    fn comments_attributes_names_and_lifetimes_cannot_invent_callable_facts() {
        let source = "#[doc = \"fn fake(text: Wrong) -> Wrong\"]\n/// fn other(x: Wrong)\npub fn actual(&'muted self, text: &str) -> String";
        let actual = callable(source, "actual", Language::Rust).unwrap();
        assert_eq!(actual.inputs[0].name.as_ref(), "reads it");
        assert_eq!(actual.inputs[1].name.as_ref(), "text");
        assert!(callable(source, "fake", Language::Rust).is_none());
        assert!(callable("fn other() -> Wrong", "actual", Language::Rust).is_none());
        assert!(callable("@note(\"size\")\ndef size(items: Item) -> int:", "size", Language::Python).is_none());
        assert!(callable("function size(items: Item): { value: number }", "size", Language::TypeScript).is_none());
        assert!(callable("def size(items: Item) -> dict[str, int]:", "size", Language::Python).is_some());
        assert!(callable("func size(items []Item) (value int, err error)", "size", Language::Go).is_none());
        assert!(callable("auto size(Item item) -> Value", "size", Language::Cpp).is_none());
    }

}
