use std::sync::Arc;

use pyrefly::alt::answers::Answers;
use pyrefly::binding::bindings::Bindings;
use pyrefly::state::state::Transaction;
use pyrefly_build::handle::Handle;
use pyrefly_types::callable::{Param, Params};
use pyrefly_types::class::ClassFields;
use pyrefly_types::types::{BoundMethodType, Forallable, Type};

use super::types;
use ir::function::{Attribute, Function};
use ir::generics::{ConstExpr, Generics};
use ir::parameter::{
    LiteralParameter, Parameter as IrParameter, ParameterAttribute,
};
use ir::protocols::ReceiverKind;
use ir::ty::{self, FunctionPointer};

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
    let mut attrs = Vec::new();
    let input_params = extract_inputs(py_type);
    let output_params = extract_outputs(py_type);
    let receiver = detect_receiver(&input_params);

    // Detect async from the pyrefly function metadata. Pyrefly's `FuncFlags`
    // exposes `is_async` (set for `async def`) but has no dedicated generator
    // flag at this revision, so generator detection is left to a later pass.
    if let Type::Function(f) = py_type {
        if f.metadata.flags.is_async {
            attrs.push(Attribute::Async);
        }
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
        generics: None,
        receiver,
        overloads: None,
        implemented: true,
        members: None,
        implemented_protocols: None,
        body: None,
    }
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
            _ => return Vec::new(),
        },
        Type::Forall(f) => match &f.body {
            Forallable::Function(func) => params_as_slice(&func.signature.params),
            _ => return Vec::new(),
        },
        _ => return Vec::new(),
    };

    params
        .iter()
        .map(|p| {
            let (name, ty, attrs) = match p {
                Param::PosOnly(n, t, _) => (opt_name(n), t, vec![]),
                Param::Pos(n, t, _) => (n.to_string(), t, vec![]),
                Param::Varargs(n, t) => (opt_name(n), t, vec![ParameterAttribute::Variadic]),
                Param::KwOnly(n, t, _) => (n.to_string(), t, vec![]),
                Param::Kwargs(n, t) => (opt_name(n), t, vec![ParameterAttribute::Variadic]),
            };

            IrParameter::Literal(LiteralParameter {
                name,
                r#type: Some(types::lower_type(ty)),
                attributes: if attrs.is_empty() { None } else { Some(attrs) },
                default_value: None,
                description: None,
            })
        })
        .collect()
}

/// Render an optional parameter name (anonymous positional-only / `*args` /
/// `**kwargs` slots may carry no name) into a `String`.
fn opt_name<N: ToString>(name: &Option<N>) -> String {
    name.as_ref().map(|n| n.to_string()).unwrap_or_default()
}

fn extract_outputs(py_type: &Type) -> Option<IrParameter> {
    let ret = match py_type {
        Type::Function(f) => Some(&f.signature.ret),
        Type::Callable(c) => Some(&c.ret),
        Type::BoundMethod(bm) => match &bm.func {
            BoundMethodType::Function(f) => Some(&f.signature.ret),
            _ => None,
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
