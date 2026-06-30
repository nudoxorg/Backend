use std::sync::Arc;

use pyrefly::alt::answers::Answers;
use pyrefly::binding::bindings::Bindings;
use pyrefly::state::state::Transaction;
use pyrefly_build::handle::Handle;
use pyrefly_types::class::Class;

use super::function;
use super::types;
use ir::entry::NudoxPath;
use ir::kind::{Entry, Symbol, Visibility};
use ir::module::Module;
use ir::ty::TypeReference;

/// Lower the module-level items (classes, functions, constants, type aliases)
/// from pyrefly's `Bindings` into a `Vec<(NudoxPath, Entry)>`.
///
/// Called from `PythonContext::lower_handle`. Each entry is paired with its
/// fully-qualified `NudoxPath` for insertion into the IR index.
pub fn lower_module(
    handle: &Handle,
    tx: &Transaction,
    module_path: &NudoxPath,
) -> Vec<(NudoxPath, Entry)> {
    let mut entries: Vec<(NudoxPath, Entry)> = Vec::new();

    let bindings = match tx.get_bindings(handle) {
        Some(b) => b,
        None => return entries,
    };
    let answers = match tx.get_answers(handle) {
        Some(a) => a,
        None => return entries,
    };

    let module_name = handle.module().as_str().to_string();

    // Module entry
    let nudox_path = NudoxPath::Local(module_path.as_path().to_path_buf());
    entries.push((
        nudox_path.clone(),
        Entry::Module(Symbol {
            name: module_name.clone(),
            path: nudox_path.clone(),
            aliases: None,
            visibility: Visibility::Public,
            documentation: None,
            inner: Module { members: None },
        }),
    ));

    // Iterate over exported names in the module's bindings.
    // We inspect the KeyExport table to find what names are exported
    // from this module, then check their types via Answers.
    for export_idx in bindings.keys::<pyrefly::binding::binding::KeyExport>() {
        let key = bindings.idx_to_key(export_idx);
        let export_name = key.0.to_string();

        // Resolve the binding's type via answers.
        let binding = bindings.get(export_idx);
        let binding_clone = binding.clone();
        let expr_type = answers.binding_type(&binding_clone);
        let ty = expr_type.as_ref().unwrap_or(&pyrefly_types::types::Type::Any(pyrefly_types::types::AnyStyle::Default));

        let entry = lower_binding(
            &export_name,
            ty,
            handle,
            tx,
            &bindings,
            &answers,
        );

        if let Some((path, entry)) = entry {
            entries.push((path, entry));
        }
    }

    entries
}

fn lower_binding(
    name: &str,
    ty: &pyrefly_types::types::Type,
    handle: &Handle,
    tx: &Transaction,
    bindings: &Bindings,
    answers: &Answers,
) -> Option<(NudoxPath, Entry)> {
    let path = NudoxPath::Local(
        std::path::PathBuf::from(format!("{}::{}", handle.module().as_str(), name)),
    );

    let entry = match ty {
        // Class definition — extract fields and methods
        pyrefly_types::types::Type::ClassDef(cls) => {
            lower_class(name, cls, handle, tx, bindings, answers, &path)
        }

        // Function definition
        pyrefly_types::types::Type::Function(_)
        | pyrefly_types::types::Type::Callable(_)
        | pyrefly_types::types::Type::BoundMethod(_) => {
            let ir_func = function::lower_function(handle, tx, bindings, answers, name, ty);
            Some(Entry::Function(Symbol {
                name: name.to_string(),
                path: path.clone(),
                aliases: None,
                visibility: Visibility::Public,
                documentation: None,
                inner: ir_func,
            }))
        }

        // Type alias
        pyrefly_types::types::Type::TypeAlias(_)
        | pyrefly_types::types::Type::UntypedAlias(_) => {
            Some(Entry::TypeAlias(Symbol {
                name: name.to_string(),
                path: path.clone(),
                aliases: None,
                visibility: Visibility::Public,
                documentation: None,
                inner: types::lower_type(ty),
            }))
        }

        // Module-level constant (immutable)
        _ if name.starts_with('_') && name != "__init__" => None,

        // Everything else — treat as a constant or variable
        _ => Some(Entry::Constant(Symbol {
            name: name.to_string(),
            path: path.clone(),
            aliases: None,
            visibility: Visibility::Public,
            documentation: None,
            inner: (),
        })),
    };

    entry.map(|e| (path, e))
}

fn lower_class(
    name: &str,
    cls: &Class,
    handle: &Handle,
    tx: &Transaction,
    _bindings: &Bindings,
    _answers: &Answers,
    path: &NudoxPath,
) -> Option<Entry> {
    // Get class fields from pyrefly's metadata
    let class_fields = tx.get_class_fields(handle, cls);

    let mut ir_fields = Vec::new();
    let mut ir_methods = Vec::new();
    let mut super_types = Vec::new();

    if let Some(fields) = class_fields {
        for field_name in fields.names() {
            let field_entry = ir::record::Field::Known(ir::record::KnownField {
                key: ir::record::FieldKey::Ident(field_name.to_string()),
                r#type: None,
                default_value: None,
                attributes: ir::record::FieldAttributes {
                    decorators: Vec::new(),
                    is_mutable: false,
                    is_optional: false,
                    is_static: false,
                },
                visibility: Some(Visibility::Public),
                documentation: None,
            });
            ir_fields.push(field_entry);
        }
    }

    Some(Entry::RecordType(Symbol {
        name: name.to_string(),
        path: path.clone(),
        aliases: None,
        visibility: Visibility::Public,
        documentation: None,
        inner: ir::record::Record {
            name: Some(name.to_string()),
            generics: None,
            fields: ir_fields,
            call_signatures: if ir_methods.is_empty() {
                None
            } else {
                Some(ir_methods)
            },
            constructors: None,
            methods: None,
            index_signatures: None,
            super_types: if super_types.is_empty() {
                None
            } else {
                Some(super_types)
            },
            members: None,
            implemented_protocols: None,
        },
    }))
}
