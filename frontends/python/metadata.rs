//! Bounded static packaging literals parsed with the same Ruff grammar as source facts.
//!
//! This adapter never imports or executes Python. It admits only an entire
//! straight-line module made of docstrings, literal assignments and setuptools
//! imports/calls; arbitrary statements make its result explicitly dynamic.

use ruff_python_ast as ast;
use ruff_python_parser::{Mode, ParseOptions, parse_unchecked};
use std::collections::BTreeMap;

/// A literal value supported by Python packaging declarations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackagingLiteral {
    /// Unicode string, decoded by Ruff.
    String(String),
    /// A list or tuple containing only supported literals.
    Sequence(Vec<Self>),
    /// A dictionary with unique literal string keys.
    Mapping(BTreeMap<String, Self>),
}

/// Static packaging extraction result; dynamic code remains unavailable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackagingSyntax {
    /// The entire bounded module belongs to the admitted static subset.
    Literal {
        /// Single-assignment module bindings.
        bindings: BTreeMap<String, PackagingLiteral>,
        /// Keyword arguments from the single setuptools setup call, if present.
        setup: Option<BTreeMap<String, PackagingLiteral>>,
    },
    /// Arbitrary code, duplicate bindings, or computed metadata was present.
    Dynamic,
}

/// Parses packaging declarations without executing any user code.
///
/// # Errors
/// Returns an error for oversized input, invalid UTF-8, or invalid Python syntax.
pub fn packaging_literals(bytes: &[u8]) -> Result<PackagingSyntax, &'static str> {
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("Python packaging source exceeds bounds");
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "Python packaging source is not UTF-8")?;
    let parsed = parse_unchecked(
        text,
        ParseOptions::from(Mode::Module).with_target_version(ast::PythonVersion::PY314),
    );
    if parsed.has_syntax_errors() {
        return Err("invalid Python packaging syntax");
    }
    let ast::Mod::Module(module) = parsed.syntax() else {
        return Err("Python packaging parser returned non-module syntax");
    };
    let mut bindings = BTreeMap::new();
    let mut setup = None;
    let mut direct_setup = false;
    let mut module_setup = false;
    let mut retained_bytes = 0;
    for statement in &module.body {
        match statement {
            ast::Stmt::ImportFrom(import)
                if import.module.as_deref() == Some("setuptools") && import.level == 0 =>
            {
                if import.names.len() != 1
                    || import.names[0].name.as_str() != "setup"
                    || import.names[0].asname.is_some()
                    || direct_setup
                {
                    return Ok(PackagingSyntax::Dynamic);
                }
                direct_setup = true;
            }
            ast::Stmt::Import(import) => {
                if import.names.len() != 1
                    || import.names[0].name.as_str() != "setuptools"
                    || import.names[0].asname.is_some()
                    || module_setup
                {
                    return Ok(PackagingSyntax::Dynamic);
                }
                module_setup = true;
            }
            ast::Stmt::Assign(assign) if assign.targets.len() == 1 => {
                let ast::Expr::Name(name) = &assign.targets[0] else {
                    return Ok(PackagingSyntax::Dynamic);
                };
                if matches!(name.id.as_str(), "setup" | "setuptools") {
                    return Ok(PackagingSyntax::Dynamic);
                }
                let Some(value) = literal(&assign.value, &bindings, 0) else {
                    return Ok(PackagingSyntax::Dynamic);
                };
                retained_bytes += literal_bytes(&value);
                if retained_bytes > 4 * 1024 * 1024 || bindings.len() >= 256 {
                    return Ok(PackagingSyntax::Dynamic);
                }
                if bindings.insert(name.id.to_string(), value).is_some() {
                    return Ok(PackagingSyntax::Dynamic);
                }
            }
            ast::Stmt::Expr(statement) => match statement.value.as_ref() {
                ast::Expr::StringLiteral(_) => {}
                ast::Expr::Call(call) if setup.is_none() && call.arguments.args.is_empty() => {
                    let admitted = match call.func.as_ref() {
                        ast::Expr::Name(name) => direct_setup && name.id.as_str() == "setup",
                        ast::Expr::Attribute(attribute) => {
                            module_setup
                                && attribute.attr.as_str() == "setup"
                                && matches!(attribute.value.as_ref(), ast::Expr::Name(name) if name.id.as_str() == "setuptools")
                        }
                        _ => false,
                    };
                    if !admitted {
                        return Ok(PackagingSyntax::Dynamic);
                    }
                    let mut fields = BTreeMap::new();
                    for keyword in &call.arguments.keywords {
                        let Some(key) = &keyword.arg else {
                            return Ok(PackagingSyntax::Dynamic);
                        };
                        let Some(value) = literal(&keyword.value, &bindings, 0) else {
                            return Ok(PackagingSyntax::Dynamic);
                        };
                        retained_bytes += literal_bytes(&value);
                        if retained_bytes > 4 * 1024 * 1024 || fields.len() >= 256 {
                            return Ok(PackagingSyntax::Dynamic);
                        }
                        if fields.insert(key.to_string(), value).is_some() {
                            return Ok(PackagingSyntax::Dynamic);
                        }
                    }
                    setup = Some(fields);
                }
                _ => return Ok(PackagingSyntax::Dynamic),
            },
            ast::Stmt::Pass(_) => {}
            _ => return Ok(PackagingSyntax::Dynamic),
        }
    }
    Ok(PackagingSyntax::Literal { bindings, setup })
}

fn literal(
    value: &ast::Expr,
    bindings: &BTreeMap<String, PackagingLiteral>,
    depth: usize,
) -> Option<PackagingLiteral> {
    if depth > 16 {
        return None;
    }
    let result = match value {
        ast::Expr::StringLiteral(value) => {
            Some(PackagingLiteral::String(value.value.to_str().to_owned()))
        }
        ast::Expr::Name(name) => bindings.get(name.id.as_str()).cloned(),
        ast::Expr::List(list) => literal_sequence(&list.elts, bindings, depth),
        ast::Expr::Tuple(tuple) => literal_sequence(&tuple.elts, bindings, depth),
        ast::Expr::Dict(dict) => {
            let mut result = BTreeMap::new();
            let mut retained_bytes = 16;
            for item in &dict.items {
                let Some(ast::Expr::StringLiteral(key)) = item.key.as_ref() else {
                    return None;
                };
                let value = literal(&item.value, bindings, depth + 1)?;
                retained_bytes += key.value.to_str().len() + literal_bytes(&value);
                if retained_bytes > 64 * 1024 {
                    return None;
                }
                if result
                    .insert(key.value.to_str().to_owned(), value)
                    .is_some()
                {
                    return None;
                }
            }
            Some(PackagingLiteral::Mapping(result))
        }
        _ => None,
    };
    result.filter(|value| literal_bytes(value) <= 64 * 1024)
}

fn literal_sequence(
    values: &[ast::Expr],
    bindings: &BTreeMap<String, PackagingLiteral>,
    depth: usize,
) -> Option<PackagingLiteral> {
    let mut result = Vec::new();
    let mut retained_bytes = 16;
    for value in values {
        let value = literal(value, bindings, depth + 1)?;
        retained_bytes += literal_bytes(&value);
        if retained_bytes > 64 * 1024 {
            return None;
        }
        result.push(value);
    }
    Some(PackagingLiteral::Sequence(result))
}

fn literal_bytes(value: &PackagingLiteral) -> usize {
    match value {
        PackagingLiteral::String(value) => value.len() + 16,
        PackagingLiteral::Sequence(values) => 16 + values.iter().map(literal_bytes).sum::<usize>(),
        PackagingLiteral::Mapping(values) => {
            16 + values
                .iter()
                .map(|(key, value)| key.len() + literal_bytes(value))
                .sum::<usize>()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruff_decodes_literals_and_aliases_without_execution() {
        let result = packaging_literals(
            br#"from setuptools import setup
VERSION = '1.2.3'
setup(name='snow\u2603', version=VERSION, install_requires=['requests>=2,<3'])
"#,
        )
        .expect("valid syntax");
        let PackagingSyntax::Literal {
            setup: Some(fields),
            ..
        } = result
        else {
            panic!("static literals");
        };
        assert_eq!(
            fields.get("name"),
            Some(&PackagingLiteral::String("snow☃".to_owned()))
        );
    }

    #[test]
    fn alias_amplification_stops_at_retained_value_bounds() {
        let mut source = "a='x'\n".to_owned();
        for index in 0..100 {
            let previous = if index == 0 {
                "a".to_owned()
            } else {
                format!("v{}", index - 1)
            };
            source.push_str(&format!("v{index}=[{previous},{previous}]\n"));
        }
        assert_eq!(
            packaging_literals(source.as_bytes()).expect("valid syntax"),
            PackagingSyntax::Dynamic
        );
    }

    #[test]
    fn arbitrary_code_and_ambiguous_bindings_remain_dynamic() {
        for source in [
            "__version__ = dangerous()",
            "__version__ = '1'\n__version__ = '2'",
            "if True:\n __version__ = '1'",
            "__version__ = '1'\nexec('anything')",
            "from setuptools import setup\nsetup(**dict(name='x'))",
            "from setuptools import setup\nsetup = print\nsetup(name='x')",
            "from setuptools import setup\nsetup(name=f'{1}')",
        ] {
            assert_eq!(
                packaging_literals(source.as_bytes()).expect("valid syntax"),
                PackagingSyntax::Dynamic
            );
        }
        assert!(packaging_literals(b"setup(").is_err());
    }
}
