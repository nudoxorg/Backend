use pyrefly::alt::answers::Answers;
use pyrefly::binding::bindings::Bindings;
use pyrefly::state::state::Transaction;
use pyrefly_build::handle::Handle;
use pyrefly_types::callable::{FuncFlags, Param, Params};
use pyrefly_types::class::ClassFields;
use pyrefly_types::types::{BoundMethodType, Forallable, Overload, Type};

use super::types;
use ir::function::{Attribute, Function};
use ir::parameter::{LiteralParameter, Parameter as IrParameter};
use ir::protocols::ReceiverKind;

/// Lower a single Python function definition into `ir::function::Function`.
///
/// Uses pyrefly's `Answers` to extract parameter types and the return type.
///
/// IMPLEMENTATION NOTE: To find functions in a module, iterate over
/// `Bindings::keys::<KeyDecoratedFunction>()` or inspect the module's
/// exports. This function is called once per discovered callable.
pub fn lower_function(
    _handle: &Handle,
    _tx: &Transaction,
    _bindings: &Bindings,
    _answers: &Answers,
    name: &str,
    py_type: &Type,
) -> Function {
    // Overloaded callables: Pyrefly pre-merges every `@overload`-decorated
    // signature into a single `Type::Overload`, so the grouping is already done
    // for us — we just lower each branch and surface them via `overloads`.
    if let Type::Overload(overload) = py_type {
        return lower_overload(name, overload);
    }
    lower_single(name, py_type)
}

/// Lower a single (non-overloaded) callable type, honoring decorator-derived
/// `FuncFlags` (async / static / classmethod / abstract / stub body) and
/// declaration-site generics on `Type::Forall`.
fn lower_single(name: &str, py_type: &Type) -> Function {
    let _ = name;
    let mut attrs = Vec::new();
    let input_params = extract_inputs(py_type);
    let output_params = extract_outputs(py_type);
    let mut receiver = detect_receiver(&input_params);
    let mut implemented = true;
    // Prefer declaration-site Forall tparams; if the callable is not wrapped
    // in Forall but still mentions free quantified vars in its signature,
    // synthesize a Generics list from those names so consumers still see a
    // non-empty `generics` field (legacy TypeVar functions).
    let generics = types::extract_forall_tparams(py_type)
        .or_else(|| types::generics_from_free_params(&input_params, &output_params));

    // Decorators surface as flags on the function metadata; Pyrefly resolves
    // them for us, so we never need to read the decorator AST here. See
    // `FuncFlags` in `pyrefly_types::callable`.
    if let Some(flags) = func_flags(py_type) {
        // `async def` (not a sync function annotated to return a coroutine).
        if flags.is_async {
            attrs.push(Attribute::Async);
        }
        // `@staticmethod` / `@classmethod` → no instance receiver. (For a
        // classmethod the leading `cls` param also maps to `Static` via
        // `detect_receiver`, but a staticmethod has neither `self` nor `cls`,
        // so we must set it explicitly.)
        if flags.is_staticmethod || flags.is_classmethod {
            receiver = Some(ReceiverKind::Static);
        }
        // `@abstractmethod`, or a `...`/absent stub body, means there is no
        // concrete implementation to point at.
        if flags.is_abstract_method || flags.lacks_implementation {
            implemented = false;
        }
        // `@property` / `@final` are recorded on the enclosing field / Symbol
        // (see `item.rs`); Function has no dedicated slot for either.
    }

    Function {
        input_parameters: if input_params.is_empty() {
            None
        } else {
            Some(input_params)
        },
        output_parameters: output_params.map(|v| vec![v]),
        type_links: None,
        attributes: if attrs.is_empty() { None } else { Some(attrs) },
        generics,
        receiver,
        overloads: None,
        implemented,
        members: None,
        implemented_protocols: None,
    }
}

/// Lower a `Type::Overload` into one `Function` whose `overloads` field carries
/// every branch.
///
/// The primary `Function` is shaped from the first branch (so its `signature`
/// surface is usable directly); the complete branch list — including the first
/// — is preserved in `overloads` so consumers can enumerate every variant.
///
/// NOTE: at this Pyrefly revision the *implementation* signature of an
/// overloaded function is not exposed separately from the `@overload`-decorated
/// branches, so the primary is the first declared overload rather than the
/// implementation.
fn lower_overload(name: &str, overload: &Overload) -> Function {
    let branches: Vec<Function> = overload
        .signatures
        .iter()
        .map(|ot| {
            let ty = ot.as_type();
            lower_single(name, &ty)
        })
        .collect();

    // `Overload::signatures` is a `Vec1`, so there is always at least one branch.
    let mut primary = branches
        .first()
        .cloned()
        .expect("Type::Overload always carries at least one signature");
    primary.overloads = Some(branches);
    primary
}

/// Extract the decorator-derived `FuncFlags` from any callable-shaped type.
pub fn func_flags(py_type: &Type) -> Option<&FuncFlags> {
    match py_type {
        Type::Function(f) => Some(&f.metadata.flags),
        Type::Overload(o) => Some(&o.metadata.flags),
        Type::Forall(forall) => match &forall.body {
            Forallable::Function(f) => Some(&f.metadata.flags),
            _ => None,
        },
        Type::BoundMethod(bm) => match &bm.func {
            BoundMethodType::Function(f) => Some(&f.metadata.flags),
            BoundMethodType::Forall(fa) => Some(&fa.body.metadata.flags),
            BoundMethodType::Overload(o) => Some(&o.metadata.flags),
        },
        _ => None,
    }
}

/// Deprecation note from `@warnings.deprecated` / `@typing_extensions.deprecated`
/// when Pyrefly recorded it on the function's `FuncFlags`.
pub fn deprecation_from_flags(py_type: &Type) -> Option<ir::kind::Deprecation> {
    let flags = func_flags(py_type)?;
    let dep = flags.deprecation.as_ref()?;
    Some(ir::kind::Deprecation {
        since: None,
        note: dep.message.clone(),
    })
}

/// Lower a class constructor (`__init__`) or method into a function.
/// `is_init` controls whether the return type is the class type.
pub fn lower_method(
    handle: &Handle,
    tx: &Transaction,
    bindings: &Bindings,
    answers: &Answers,
    method_name: &str,
    py_type: &Type,
    _class_fields: &ClassFields,
) -> Function {
    let mut func = lower_function(handle, tx, bindings, answers, method_name, py_type);
    if method_name == "__init__" || method_name == "__new__" {
        // Constructor: return type is the containing class.
        func.output_parameters = None;
    }
    func
}

/// Extract the `&[Param]` from a pyrefly `Params`, which is only a concrete
/// list in the `Params::List` case (Ellipsis / Materialization / ParamSpec
/// have no enumerable parameters).
fn params_as_slice(params: &Params) -> &[Param] {
    match params {
        Params::List(list) => list.items(),
        _ => &[],
    }
}

fn extract_inputs(py_type: &Type) -> Vec<IrParameter> {
    let params: &[Param] = match py_type {
        Type::Function(f) => params_as_slice(&f.signature.params),
        Type::Callable(c) => params_as_slice(&c.params),
        Type::BoundMethod(bm) => match &bm.func {
            BoundMethodType::Function(f) => params_as_slice(&f.signature.params),
            BoundMethodType::Forall(fa) => params_as_slice(&fa.body.signature.params),
            BoundMethodType::Overload(o) => {
                // Prefer the first overload's params for the primary surface.
                return extract_inputs(&o.signatures.first().as_type());
            }
        },
        Type::Forall(f) => match &f.body {
            Forallable::Function(func) => params_as_slice(&func.signature.params),
            _ => return Vec::new(),
        },
        _ => return Vec::new(),
    };

    params.iter().map(types::lower_param).collect()
}

fn extract_outputs(py_type: &Type) -> Option<IrParameter> {
    let ret = match py_type {
        Type::Function(f) => Some(&f.signature.ret),
        Type::Callable(c) => Some(&c.ret),
        Type::BoundMethod(bm) => match &bm.func {
            BoundMethodType::Function(f) => Some(&f.signature.ret),
            BoundMethodType::Forall(fa) => Some(&fa.body.signature.ret),
            BoundMethodType::Overload(o) => {
                return extract_outputs(&o.signatures.first().as_type());
            }
        },
        Type::Forall(f) => match &f.body {
            Forallable::Function(func) => Some(&func.signature.ret),
            _ => None,
        },
        _ => None,
    };

    ret.map(|ty| {
        IrParameter::Literal(LiteralParameter {
            name: String::new(),
            r#type: Some(types::lower_type(ty)),
            attributes: None,
            default_value: None,
            description: None,
        })
    })
}

fn detect_receiver(params: &[IrParameter]) -> Option<ReceiverKind> {
    let first = params.first()?;
    match &first {
        // `self` is an instance receiver → model as a shared/borrowing receiver.
        IrParameter::Literal(LiteralParameter { name, .. }) if name == "self" => {
            Some(ReceiverKind::SharedRef)
        }
        // `cls` is the class-method receiver → `Static` ("class method, no receiver").
        IrParameter::Literal(LiteralParameter { name, .. }) if name == "cls" => {
            Some(ReceiverKind::Static)
        }
        _ => None,
    }
}
