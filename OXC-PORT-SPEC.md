# TypeScript Producer: deno_doc → OXC Migration Port Spec

**Target**: Preserve exact IR output shapes while replacing deno_doc-based AST parsing with OXC.

**Date**: 2026-07-11  
**Scope**: Files in `/Users/philocalyst/Projects/Backend/workspace/compiler/compile/typescript/`:
- `mod.rs`, `item.rs`, `function.rs`, `types.rs`, `package.rs`, `entry_point.rs`, `traversal.rs`, `error.rs`, `producer.rs`

---

## Table of Contents

1. [Entry-Production Surface](#1-entry-production-surface)
2. [IR Type Definitions](#2-ir-type-definitions)
3. [type_links & Path→ID Mechanism](#3-type_links--pathid-mechanism)
4. [Entry Point & Graph Discovery](#4-entry-point--graph-discovery)
5. [Error Taxonomy](#5-error-taxonomy)

---

## 1. Entry-Production Surface

This section documents which `ir::kind::Entry` variant each deno_doc declaration produces, and the exact field-construction shapes currently emitted.

### 1.1 Declaration Dispatch (`item.rs`, `symbol()`, `declaration()`)

**File**: `item.rs:98–188`  
**Entry point**: `symbol_at_path()` → `symbol()` → `declaration()`

The dispatch follows `deno_doc::Declaration::def` enum:

#### Function → `Entry::Function`

**Source**: `item.rs:196–201`  
**deno_doc**: `DeclarationDef::Function(FunctionDef)`

```rust
// item.rs:198–200
let path = vec![module_name.to_string(), symbol_name.to_string()];
let func = self.function_def(symbol_name, f, &path)?;
Ok((Entry::Function(ir::kind::Symbol::placeholder(func)), vec![], vec![]))
```

**IR Construction** (`ir::kind::Symbol<ir::function::Function>`):
- `name`: symbol_name
- `path`: `NudoxPath::Local(PathBuf::from(path.join("::")))` where path = `[module_name, symbol_name]`
- `aliases`: `None`
- `visibility`: `declaration_kind_to_visibility(decl.declaration_kind)` → `Visibility::{Public|Private}`
- `documentation`: `extract_doc(&decl.js_doc)` → `Option<String>`
- `deprecation`: `None`
- `doc_links`: `None`
- `inner`: `Function` (see §1.4)
- **Overloads** (§1.3): if multiple function declarations exist, all non-primary become `function.overloads`

**Helper**: `pick_primary_declaration()` (mod.rs:231–238) selects the declaration with `has_body=true`, falling back to last.

---

#### Variable → `Entry::Constant | Entry::Variable`

**Source**: `item.rs:203–210`  
**deno_doc**: `DeclarationDef::Variable(VarDecl)`

```rust
// item.rs:204–209
let entry = if v.kind == VarDeclKind::Const {
    Entry::Constant(ir::kind::Symbol::placeholder(()))
} else {
    Entry::Variable(ir::kind::Symbol::placeholder(()))
};
Ok((entry, vec![], vec![]))
```

**IR Construction**: `ir::kind::Symbol<()>`
- All fields except `inner` populated as above
- `inner`: `()` (unit type, no additional data)
- **Variant selection**: `Const` → `Constant`, otherwise → `Variable`

---

#### Enum → `Entry::SumType`

**Source**: `item.rs:212–215`  
**deno_doc**: `DeclarationDef::Enum(EnumDef)`

```rust
// item.rs:213–214
let variants = self.enum_def(e)?;
Ok((Entry::SumType(ir::kind::Symbol::placeholder(variants)), vec![], vec![]))
```

**Helper** (`item.rs:666–678`):
```rust
pub fn enum_def(&self, enum_def: &EnumDef) -> Result<Vec<SumVariant>> {
    Ok(enum_def.members.iter().map(|m| SumVariant {
        name: m.name.clone(),
        data: None,
        documentation: None,
    }).collect())
}
```

**IR Construction**: `ir::kind::Symbol<Vec<ir::record::SumVariant>>`
- `inner`: `Vec<SumVariant>` where each variant has:
  - `name`: member name
  - `data`: `None` (enum members treated as unit variants)
  - `documentation`: `None`

---

#### Class → `Entry::RecordType` + member entries

**Source**: `item.rs:217–220`  
**deno_doc**: `DeclarationDef::Class(ClassDef)`

```rust
// item.rs:218–219
let (record, extra, member_refs) = self.class_def(module_name, symbol_name, cls)?;
Ok((Entry::RecordType(ir::kind::Symbol::placeholder(record)), extra, member_refs))
```

**Detailed in §1.5 (Classes)**. Produces:
- Primary entry: `Entry::RecordType(Symbol<Record>)`
- Extra entries: constructors & methods as `Entry::Function`
- Member refs: paths to constructor/methods for inclusion in `Record.members`

---

#### Type Alias → `Entry::TypeAlias`

**Source**: `item.rs:222–225`  
**deno_doc**: `DeclarationDef::TypeAlias(TypeAliasDef)`

```rust
// item.rs:223–224
let ty = self.ts_type(&ta.ts_type)?;
Ok((Entry::TypeAlias(ir::kind::Symbol::placeholder(ty)), vec![], vec![]))
```

**IR Construction**: `ir::kind::Symbol<ir::ty::Type>`
- `inner`: result of `ts_type()` (see §2 for type definitions)

---

#### Namespace → `Entry::Module` (nested)

**Source**: `item.rs:227–284`  
**deno_doc**: `DeclarationDef::Namespace(NamespaceDef)`

```rust
// item.rs:280–282
Ok((
    Entry::Module(ir::kind::Symbol::placeholder(Module { members: None })),
    extra_entries,  // recursively parsed namespace elements
    member_refs,    // paths to namespace elements
))
```

**Behavior**: 
- Namespace elements stored in `extra_entries`, not inline
- Member refs collected for `Module.members` field
- Each element recursively calls `self.symbol()` with module_name = `"{module_name}.{namespace_name}"`

---

#### Interface → `Entry::TraitDef`

**Source**: `item.rs:286–290`  
**deno_doc**: `DeclarationDef::Interface(InterfaceDef)`

```rust
// item.rs:287–288
let trait_def = self.interface_def(symbol_name, iface)?;
Ok((trait_def, vec![], vec![]))
```

**Detailed in §1.6 (Interfaces)**. Produces `Entry::TraitDef(Symbol<TraitDef>)`.

---

#### Reference → `Entry::Info`

**Source**: `item.rs:291–293`  
**deno_doc**: `DeclarationDef::Reference(...)`

```rust
// item.rs:292
Ok((Entry::Info(ir::kind::Symbol::placeholder(String::new())), vec![], vec![]))
```

**IR Construction**: `ir::kind::Symbol<String>`
- `inner`: empty string (triple-slash references carry no meaningful IR content)

---

### 1.2 Symbol-Level Assembly (`item.rs:98–188`)

After `declaration()` returns the primary kind + extra entries + member refs, `symbol()` assembles the final entry:

**File**: `item.rs:118–188`

```rust
// item.rs:118–127 (template)
let symbol_template = ir::kind::Symbol {
    name: symbol.name.to_string(),
    path: NudoxPath::Local(PathBuf::from(path.join("::"))),
    aliases: None,
    visibility,
    documentation,
    deprecation: None,
    doc_links: None,
    inner: (),
};
```

**Assembly logic** (item.rs:129–184): Each variant's `inner` is replaced, and:
- `Module`, `RecordType`, `TraitDef`, `TraitImpl`, `Function`: members propagated from `member_refs`
- `Function`: `overloads` populated if multiple declarations exist
- All others: `inner` unchanged

**Overload Collection** (`item.rs:106`):
```rust
let overloads = self.function_overloads(&symbol.declarations, decl)?;
```

**Helper** (`function.rs:11–28`):
```rust
pub fn function_overloads(
    &mut self,
    declarations: &[Declaration],
    primary_decl: &Declaration,
) -> Result<Option<Vec<Function>>> {
    if !is_function_declaration(primary_decl) {
        return Ok(None);
    }
    let overloads = declarations
        .iter()
        .filter(|decl| is_function_declaration(decl) && !std::ptr::eq(*decl, primary_decl))
        .map(|decl| match &decl.def {
            DeclarationDef::Function(function_def) => self.function_def("", function_def, &[]),
            _ => unreachable!(),
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(empty_to_none(overloads))
}
```

---

### 1.3 Function Construction (`function.rs`)

**Primary entrypoint**: `function_def()` (function.rs:160–196)

```rust
pub fn function_def(
    &mut self,
    _name: &str,
    func: &FunctionDef,
    _path: &[String],
) -> Result<Function> {
    let params = func.params.iter().collect::<Vec<_>>();
    let (receiver, input_parameters) = self.params_with_receiver(&params, None)?;
    let output_parameters = func
        .return_type
        .as_ref()
        .map(|rt| self.ts_type(rt))
        .transpose()?
        .and_then(output_parameters_from_type);

    let type_links = self.build_type_links_for_params(
        input_parameters.as_deref(),
        output_parameters.as_deref(),
    );

    let attributes = self.function_attributes(func);
    let generics = if func.type_params.is_empty() {
        None
    } else {
        self.type_params(&func.type_params)?
    };

    Ok(Function {
        input_parameters,
        output_parameters,
        type_links,
        attributes,
        generics,
        receiver,
        overloads: None,
        implemented: func.has_body,
        members: None,
        implemented_protocols: None,
        body: None,
    })
}
```

**Fields**:
- `input_parameters`: `Option<Vec<Parameter>>` from `params_with_receiver()`
- `output_parameters`: `Option<Vec<Parameter>>` from `output_parameters_from_type(ty)` on return type
- `type_links`: `Option<HashMap<String, i64>>` from `build_type_links_for_params()` (§3)
- `attributes`: `Option<Vec<Attribute>>` from `function_attributes()` → `[Async, Generator]`
- `generics`: `Option<Generics>` from `type_params()` (§1.7)
- `receiver`: `Option<ReceiverKind>` from `params_with_receiver()` detection
- `overloads`: populated later by `symbol()` (§1.2)
- `implemented`: `func.has_body` (bool)
- `members`, `implemented_protocols`, `body`: always `None`

#### Receiver & Parameter Parsing (`function.rs:30–50`)

```rust
pub fn params_with_receiver(
    &mut self,
    params: &[&ParamDef],
    default_receiver: Option<ReceiverKind>,
) -> Result<(Option<ReceiverKind>, Option<Vec<Parameter>>)> {
    let mut receiver = default_receiver;
    let mut parsed = Vec::new();

    for (idx, param) in params.iter().enumerate() {
        if idx == 0
            && matches!(&param.pattern, ParamPatternDef::Identifier { name, .. } if name == "this")
        {
            receiver = Some(ReceiverKind::SharedRef);
            continue;
        }
        parsed.push(self.param(param)?);
    }

    let parsed = if parsed.is_empty() { None } else { Some(parsed) };
    Ok((receiver, parsed))
}
```

**Logic**: First param named `"this"` → receiver = `ReceiverKind::SharedRef`, skipped from parameter list.

#### Parameter Patterns (`function.rs:241–306`)

**Source**: `param()` matches `ParamPatternDef`:

1. **Identifier** (basic): → `Parameter::Literal(LiteralParameter)` with optional `ParameterAttribute::Optional`
2. **Rest** (`...args`): → `ParameterAttribute::Variadic`
3. **Assign** (default value): → `default_value: Some(ConstExpr::Var(right_side))`
4. **Array destructure**: → `Parameter::Literal` with type from `ts_type()`
5. **Object destructure**: → `Parameter::Literal` with type from `ts_type()`

#### Function Attributes (`function.rs:198–207`)

```rust
pub fn function_attributes(&self, func: &FunctionDef) -> Option<Vec<Attribute>> {
    let mut attrs = Vec::new();
    if func.is_async {
        attrs.push(Attribute::Async);
    }
    if func.is_generator {
        attrs.push(Attribute::Generator);
    }
    if attrs.is_empty() { None } else { Some(attrs) }
}
```

**Emitted attributes**: `[Async, Generator]` (or `None` if neither set).

---

### 1.4 Constructor Handling (`item.rs:391–434`)

**Context**: Produced as member entries when class has constructors.

**Primary**: `constructor_signature()` (function.rs:52–71)

```rust
pub fn constructor_signature(&mut self, ctor: &ClassConstructorDef) -> Result<Function> {
    let (_, input_parameters) = self.params_with_receiver(
        &ctor.params.iter().map(|param| &param.param).collect::<Vec<_>>(),
        Some(ReceiverKind::Static),
    )?;
    let type_links = self.build_type_links_for_params(input_parameters.as_deref(), None);
    Ok(Function {
        input_parameters,
        output_parameters: None,
        type_links,
        attributes: None,
        generics: None,
        receiver: Some(ReceiverKind::Static),
        overloads: None,
        implemented: ctor.has_body,
        members: None,
        implemented_protocols: None,
        body: None,
    })
}
```

**Entry wrapper** (`item.rs:400–410`):
```rust
Entry::Function(ir::kind::Symbol {
    name: "constructor".to_string(),
    path: NudoxPath::Local(PathBuf::from(path.join("::"))),
    aliases: None,
    visibility: accessibility_to_visibility(ctor.accessibility),
    documentation: extract_doc(&ctor.js_doc),
    deprecation: None,
    doc_links: None,
    inner: func,
})
```

**Overload grouping** (`item.rs:412–434`): Multiple constructors form a single entry with `overloads` set.

---

### 1.5 Class Construction (`item.rs:436–526`)

**Entrypoint**: `class_def()` (item.rs:436–526)

```rust
pub fn class_def(
    &mut self,
    module_name: &str,
    class_name: &str,
    cls: &ClassDef,
) -> Result<(Record, Vec<Entry>, Vec<NudoxPath>)> {
    // ... field collection, index signatures, super types, generics ...
    
    let record = Record {
        name: Some(class_name.to_string()),
        generics,
        fields,
        call_signatures: None,
        constructors: None,
        methods: None,
        index_signatures: empty_to_none(index_signatures),
        super_types: empty_to_none(super_types),
        implemented_protocols: None,
        members: None,  // filled after method extraction
    };

    // Extract constructors & methods as separate entries
    let mut extra_entries = Vec::new();
    let mut member_refs = Vec::new();
    
    if !cls.constructors.is_empty() {
        let constructors: Vec<&ClassConstructorDef> = cls.constructors.iter().collect();
        let ctor_entry = self.constructor_group_entry(module_name, class_name, &constructors)?;
        member_refs.push(ctor_entry.path().clone());
        extra_entries.push(ctor_entry);
    }
    
    // (similar for methods)
    
    Ok((record, extra_entries, member_refs))
}
```

**Record fields**:
- `name`: class name
- `generics`: from `type_params(cls.type_params)`
- `fields`: from `cls.properties`, each → `Field::Known(KnownField)`
- `call_signatures`: `None`
- `constructors`: `None` (moved to entries)
- `methods`: `None` (moved to entries)
- `index_signatures`: from `cls.index_signatures`
- `super_types`: `extends` + `implements` as `Type`
- `implemented_protocols`: `None`
- `members`: populated from `member_refs`

**Property→Field** (`types.rs:10–30`):
```rust
pub fn property_field(
    &mut self,
    name: &str,
    ts_type: Option<&TsTypeDef>,
    metadata: PropertyFieldMetadata<'_>,
) -> Result<Field> {
    let ty = ts_type.map(|t| self.ts_type(t).map(Box::new)).transpose()?;
    Ok(Field::Known(KnownField {
        key: FieldKey::Ident(name.to_string()),
        r#type: ty,
        default_value: None,
        attributes: FieldAttributes {
            is_mutable: !metadata.readonly,
            is_optional: metadata.optional,
            decorators: metadata.decorators.to_vec(),
            is_static: metadata.is_static,
        },
        visibility: metadata.visibility,
        documentation: metadata.documentation,
    }))
}
```

---

### 1.6 Interface Construction (`item.rs:528–592`)

**Entrypoint**: `interface_def()` (item.rs:528–592)

```rust
pub fn interface_def(&mut self, name: &str, iface: &InterfaceDef) -> Result<Entry> {
    let generics = if iface.type_params.is_empty() {
        None
    } else {
        self.type_params(&iface.type_params)?
    };

    let super_traits: Option<Vec<TraitRef>> = {
        let v: Vec<TraitRef> = iface.extends
            .iter()
            .map(|ty| self.ts_type_to_trait_ref(ty))
            .collect::<Result<Vec<_>>>()?;
        if v.is_empty() { None } else { Some(v) }
    };

    let mut required_methods: Vec<TraitMethod> = Vec::new();
    for m in &iface.methods {
        required_methods.push(self.interface_method(m)?);
    }
    for (i, sig) in iface.call_signatures.iter().enumerate() {
        required_methods.push(self.call_signature_as_method(sig, i)?);
    }
    for (i, sig) in iface.index_signatures.iter().enumerate() {
        required_methods.push(self.index_signature_as_method(sig, i)?);
    }

    let properties = iface.properties.iter().map(|prop| {
        // ... similar to class property_field ...
    }).collect::<Result<Vec<_>>>()?;

    Ok(Entry::TraitDef(ir::kind::Symbol {
        name: name.to_string(),
        path: NudoxPath::Local(PathBuf::from("blank")),  // ← NOTE: placeholder
        aliases: None,
        visibility: Visibility::Public,
        documentation: None,
        deprecation: None,
        doc_links: None,
        inner: TraitDef {
            generics,
            super_traits,
            associated_types: None,
            properties: empty_to_none(properties),
            required_methods: empty_to_none(required_methods),
            provided_methods: None,
            required_constants: None,
            attributes: None,
            object_safe: None,
            sealed: None,
            cfg: None,
            members: None,
        },
    }))
}
```

**Call Signatures → `__call*` methods** (item.rs:615–640):
```rust
pub fn call_signature_as_method(
    &mut self,
    sig: &CallSignatureDef,
    idx: usize,
) -> Result<TraitMethod> {
    let name = if idx == 0 { "__call".to_string() } else { format!("__call_{}", idx) };
    // ... parse params, return type, generics ...
    Ok(TraitMethod { name, parameters, return_type, generics, ... })
}
```

**Index Signatures → `__index*` methods** (item.rs:642–664):
```rust
pub fn index_signature_as_method(
    &mut self,
    sig: &IndexSignatureDef,
    idx: usize,
) -> Result<TraitMethod> {
    let name = if idx == 0 { "__index".to_string() } else { format!("__index_{}", idx) };
    // ... parse params, return type ...
    Ok(TraitMethod { name, parameters, return_type, ... })
}
```

---

### 1.7 Generics & Type Parameters (`types.rs:96–124`)

**Entrypoint**: `type_params()` (types.rs:96–124)

```rust
pub fn type_params(&mut self, params: &[TsTypeParamDef]) -> Result<Option<Generics>> {
    if params.is_empty() {
        return Ok(None);
    }

    let mut type_params = Vec::new();
    let mut constraints = Vec::new();

    for p in params {
        let default_type = p.default
            .as_ref()
            .map(|t| self.ts_type(t))
            .transpose()?
            .map(|ty| self.type_to_expr(&ty));

        type_params.push(Parameter::Type(TypeParam {
            name: Some(p.name.clone()),
            kind: Kind::Type,
            variance: Variance::Invariant,
            default_type,
            params: None,
            origin: TypeParamOrigin::Free,
        }));

        if let Some(constraint) = &p.constraint {
            let trait_ref = self.ts_type_to_trait_ref(constraint)?;
            constraints.push(Constraint::TraitBound { param: p.name.clone(), trait_ref });
        }
    }

    Ok(Some(Generics { params: type_params, constraints }))
}
```

**Fields**:
- `kind`: always `Kind::Type`
- `variance`: always `Variance::Invariant`
- `default_type`: from `TypeExpr` (see `type_to_expr()`)
- `origin`: always `TypeParamOrigin::Free`
- `params`: always `None`
- `constraints`: `TraitBound` if constraint present

---

## 2. IR Type Definitions

Complete verbatim definitions of all IR types constructed by the producer.

### 2.1 `ir::kind::Entry` Variants & `ir::kind::Symbol<T>`

**File**: `intermediate-representation/kind.rs:7–261`

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Visibility {
    Public,
    Private,
    Protected,
    Internal,
    Package,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Deprecation {
    pub since: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Symbol<T> {
    pub name: String,
    pub path: NudoxPath,
    pub aliases: Option<HashSet<Vec<String>>>,
    pub visibility: Visibility,
    pub documentation: Option<String>,
    pub deprecation: Option<Deprecation>,
    pub doc_links: Option<HashMap<String, NudoxPath>>,
    pub inner: T,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Module(Symbol<Module>),
    RecordType(Symbol<Record>),
    Info(Symbol<String>),
    UnionType(Symbol<Vec<Type>>),
    TraitDef(Symbol<TraitDef>),
    TraitImpl(Symbol<TraitImpl>),
    SumType(Symbol<Vec<SumVariant>>),
    Function(Symbol<Function>),
    TypeAlias(Symbol<Type>),
    Constant(Symbol<()>),
    Variable(Symbol<()>),
    Macro(Symbol<()>),
    PrimitiveType(Symbol<()>),
    Field(Symbol<()>),
    Event(Symbol<()>),
}
```

---

### 2.2 `ir::ty::Type` & Related

**File**: `intermediate-representation/ty.rs:1–213`

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    TypeReference(TypeReference),
    SelfType,
    DynTrait(DynTrait),
    GenericParam(GenericParam),
    Primitive(Primitive),
    FunctionPointer(FunctionPointer),
    Tuple(Vec<Type>),
    RecordLiteral(Box<Record>),
    Slice(Box<Type>),
    Array { r#type: Box<Type>, length: usize },
    ImplTrait(Vec<GenericBound>),
    Infer,
    Never,
    Any,
    RawPointer { is_mutable: bool, r#type: Box<Type> },
    BorrowedRef { lifetime: Option<String>, is_mutable: bool, r#type: Box<Type> },
    Union(Vec<Type>),
    Intersection(Vec<Type>),
    Sum(Vec<SumVariant>),
    QualifiedPath(QualifiedPath),
    Variadic(Box<Type>),
    TypeOperator(TypeOperator),
    Conditional(ConditionalType),
    Mapped(MappedType),
    Predicate(TypePredicate),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeReference {
    pub identifier: String,
    pub generic_args: Option<Vec<GenericArg>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QualifiedPath {
    pub name: String,
    pub generic_arguments: Option<Vec<GenericArg>>,
    pub self_type: Box<Type>,
    pub tr: Option<TypeReference>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenericParam {
    pub name: String,
    pub kind: Option<Box<Type>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionPointer {
    pub inputs: Option<Vec<Parameter>>,
    pub outputs: Option<Vec<Parameter>>,
    pub attributes: Option<Vec<function::Attribute>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DynTrait {
    pub traits: Vec<PolyTrait>,
    pub lifetime: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolyTrait {
    pub trait_ref: TraitRef,
    pub lifetimes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeOperator {
    pub operator: String,
    pub r#type: Box<Type>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConditionalType {
    pub check_type: Box<Type>,
    pub extends_type: Box<Type>,
    pub true_type: Box<Type>,
    pub false_type: Box<Type>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModifierPrefix {
    Preserve,
    Add,
    Remove,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MappedType {
    pub readonly: Option<ModifierPrefix>,
    pub optional: Option<ModifierPrefix>,
    pub parameter: String,
    pub source_type: Box<Type>,
    pub name_type: Option<Box<Type>>,
    pub value_type: Option<Box<Type>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PredicateSubject {
    This,
    Identifier(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypePredicate {
    pub asserts: bool,
    pub subject: PredicateSubject,
    pub r#type: Option<Box<Type>>,
}
```

---

### 2.3 `ir::generics::{Generics, TypeParam, Constraint, TraitRef, TypeExpr, ...}`

**File**: `intermediate-representation/generics.rs:1–272`

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct Generics {
    pub params: Vec<Parameter>,
    pub constraints: Vec<Constraint>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Constraint {
    TraitBound { param: String, trait_ref: TraitRef },
    AssociatedTypeBound { param: String, assoc_name: String, bound: TypeExpr },
    HigherKindedBound { param: String, kind: Kind },
    AssociatedItem { name: String, args: Option<Vec<GenericArg>>, term: Term },
    LifetimeBound { shorter: String, longer: String },
    ConstExprBound { param: String, expr: ConstExpr },
    LogicalPredicate { pred: Predicate },
    FunctionalDependency { sources: Vec<String>, determined: Vec<String> },
    ImplicitBound { param: String, trait_ref: TraitRef },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Term {
    Equality(Box<Type>),
    Bound(Vec<Constraint>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Type,
    Constraint,
    Row,
    Arrow(Box<Kind>, Box<Kind>),
    Var(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConstExpr {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Var(String),
    BinOp { op: BinOp, lhs: Box<ConstExpr>, rhs: Box<ConstExpr> },
    UnaryOp { op: UnaryOp, operand: Box<ConstExpr> },
    Call { func: String, args: Vec<ConstExpr> },
    Ascription { expr: Box<ConstExpr>, ty: Box<TypeExpr> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum BinOp {
    Add, Sub, Mul, Div, Rem, BitAnd, BitOr, BitXor, Shl, Shr,
    Eq, Ne, Lt, Le, Gt, Ge, And, Or,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UnaryOp {
    Neg, Not, Ref, Deref,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    Atom(Box<Constraint>),
    And(Vec<Predicate>),
    Or(Vec<Predicate>),
    Not(Box<Predicate>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraitRef {
    pub name: String,
    pub args: Vec<TypeExpr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeExpr {
    pub name: String,
    pub args: Vec<TypeExpr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GenericArg {
    Type(Type),
    ConstExpr(ConstExpr),
    Lifetime(String),
    Constraint(Constraint),
    Module(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Variance {
    Covariant,
    Contravariant,
    Invariant,
    Bivariant,
}
```

---

### 2.4 `ir::parameter::{Parameter, TypeParam, LiteralParameter, ParameterAttribute, TypeParamOrigin}`

**File**: `intermediate-representation/parameter.rs:1–229`

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Parameter {
    Literal(LiteralParameter),
    Type(TypeParam),
    Const(ConstParam),
    Lifetime(LifetimeParam),
    Dependent(DependentParam),
    Module(ModuleParam),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiteralParameter {
    pub name: String,
    pub r#type: Option<Type>,
    pub attributes: Option<Vec<ParameterAttribute>>,
    pub default_value: Option<ConstExpr>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParameterAttribute {
    Inout,
    Mutable,
    Consuming,
    Borrowing,
    Isolated,
    Variadic,
    Optional,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeParam {
    pub name: Option<String>,
    pub kind: Kind,
    pub variance: Variance,
    pub default_type: Option<TypeExpr>,
    pub params: Option<Vec<Parameter>>,
    pub origin: TypeParamOrigin,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeParamOrigin {
    Free,
    Associated,
    Inferred,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConstParam {
    pub name: String,
    pub r#type: TypeExpr,
    pub default_value: Option<ConstExpr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LifetimeParam {
    pub name: String,
    pub variance: Variance,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DependentParam {
    pub name: String,
    pub r#type: TypeExpr,
    pub default_value: Option<ConstExpr>,
    pub implicit: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModuleParam {
    pub name: String,
    pub signature: Option<TypeExpr>,
}
```

---

### 2.5 `ir::record::{Record, Field, KnownField, FieldKey, FieldAttributes, IndexSignature, SumVariant}`

**File**: `intermediate-representation/record.rs:1–157`

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub name: Option<String>,
    pub generics: Option<Generics>,
    pub fields: Vec<Field>,
    pub call_signatures: Option<Vec<Function>>,
    pub constructors: Option<Vec<Function>>,
    pub methods: Option<Vec<Function>>,
    pub index_signatures: Option<Vec<IndexSignature>>,
    pub super_types: Option<Vec<Type>>,
    pub members: Option<Vec<NudoxPath>>,
    pub implemented_protocols: Option<Vec<NudoxPath>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexSignature {
    pub key_type: Box<Type>,
    pub value_type: Box<Type>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Field {
    Known(KnownField),
    Pattern(IndexSignature),
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldKey {
    Ident(String),
    Index(usize),
    Computed(ConstExpr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct KnownField {
    pub key: FieldKey,
    pub r#type: Option<Box<Type>>,
    pub default_value: Option<ConstExpr>,
    pub attributes: FieldAttributes,
    pub visibility: Option<Visibility>,
    pub documentation: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldAttributes {
    pub decorators: Vec<String>,
    pub is_mutable: bool,
    pub is_optional: bool,
    pub is_static: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SumVariant {
    pub name: String,
    pub data: Option<SumField>,
    pub documentation: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SumField {
    Tuple(Vec<Type>),
    StructLike(Vec<Field>),
}
```

---

### 2.6 `ir::primitives::{Primitive, Width}`

**File**: `intermediate-representation/primitives.rs:1–50`

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Width {
    W8, W16, W32, W64, W128, Arch,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Primitive {
    Int(Width),
    UInt(Width),
    Float(Width),
    Bool,
    String,
    Char,
    Bytes,
    Date,
    Address,
}
```

---

### 2.7 `ir::entry::{Index, NudoxPath}`

**File**: `intermediate-representation/entry.rs:1–36`

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NudoxPath {
    External { path: PathBuf, dependency: String },
    Local(PathBuf),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Index {
    pub root_ids: Vec<NudoxPath>,
    pub entries_by_path: HashMap<NudoxPath, Entry>,
}
```

---

### 2.8 `ir::function::{Function, Attribute}` & `ir::module::Module`

**File**: `intermediate-representation/function.rs:1–90`

```rust
#[derive(Debug, Clone)]
pub struct Function {
    pub input_parameters: Option<Vec<Parameter>>,
    pub output_parameters: Option<Vec<Parameter>>,
    pub type_links: Option<HashMap<String, i64>>,
    pub attributes: Option<Vec<Attribute>>,
    pub generics: Option<Generics>,
    pub receiver: Option<ReceiverKind>,
    pub overloads: Option<Vec<Function>>,
    pub implemented: bool,
    pub members: Option<Vec<NudoxPath>>,
    pub implemented_protocols: Option<Vec<NudoxPath>>,
    pub body: Option<crate::syntax::ParsedBody>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Attribute {
    Variadic, Generator, Const, Pure, Async, Unsafe,
}
```

**File**: `intermediate-representation/module.rs:1–11`

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub members: Option<Vec<NudoxPath>>,
}
```

---

### 2.9 `ir::protocols::{TraitDef, TraitMethod, ReceiverKind, TraitImpl}`

**File**: `intermediate-representation/protocols.rs:1–*` (excerpt)

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct TraitDef {
    pub generics: Option<Generics>,
    pub super_traits: Option<Vec<TraitRef>>,
    pub associated_types: Option<Vec<AssociatedType>>,
    pub properties: Option<Vec<Field>>,
    pub required_methods: Option<Vec<TraitMethod>>,
    pub provided_methods: Option<Vec<TraitMethod>>,
    pub required_constants: Option<Vec<TraitConstant>>,
    pub attributes: Option<Vec<TraitAttribute>>,
    pub object_safe: Option<bool>,
    pub sealed: Option<bool>,
    pub cfg: Option<String>,
    pub members: Option<Vec<NudoxPath>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraitMethod {
    pub name: String,
    pub parameters: Option<Vec<Parameter>>,
    pub return_type: Option<Box<Type>>,
    pub generics: Option<Generics>,
    pub attributes: Option<Vec<function::Attribute>>,
    pub documentation: Option<String>,
    pub receiver: Option<ReceiverKind>,
    pub has_default_implementation: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReceiverKind {
    Owned, SharedRef, MutRef, Static, Arbitrary,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraitImpl {
    pub tr: TraitRef,
    pub for_type: Box<Type>,
    pub generics: Option<Generics>,
    pub where_constraints: Option<Vec<Constraint>>,
    pub methods: Option<Vec<Function>>,
    pub associated_types: Option<Vec<AssociatedTypeImpl>>,
    pub associated_constants: Option<Vec<TraitConstant>>,
    pub is_negative: bool,
    pub is_blanket: bool,
    // ... additional fields ...
}
```

---

## 3. type_links & Path→ID Mechanism

This section documents the current `type_links` construction and ID resolution, which **will be replaced by Phase 3** (symbol-accurate resolution). However, understanding the current shapes is critical for ensuring the output format is preserved.

### 3.1 `type_links` Construction (`function.rs:209–239`)

**Function**: `build_type_links_for_params()` (function.rs:209–239)

```rust
pub fn build_type_links_for_params(
    &self,
    inputs: Option<&[Parameter]>,
    outputs: Option<&[Parameter]>,
) -> Option<HashMap<String, i64>> {
    let mut links = HashMap::default();

    if let Some(params) = inputs {
        for (idx, param) in params.iter().enumerate() {
            if let Parameter::Literal(l) = param
                && let Some(ref ty) = l.r#type
                && let Some(entry_id) = self.resolve_ir_type_to_entry_id(ty)
            {
                links.insert(parameter_link_key("in", idx, params.len(), &l.name), entry_id);
            }
        }
    }

    if let Some(params) = outputs {
        for (idx, param) in params.iter().enumerate() {
            if let Parameter::Literal(l) = param
                && let Some(ref ty) = l.r#type
                && let Some(entry_id) = self.resolve_ir_type_to_entry_id(ty)
            {
                links.insert(parameter_link_key("out", idx, params.len(), &l.name), entry_id);
            }
        }
    }

    if links.is_empty() { None } else { Some(links) }
}
```

**Output**: `Option<HashMap<String, i64>>` where:
- **Key**: `parameter_link_key(prefix, idx, total, name)` → `"in.{name}"` or `"in.{idx}"` or `"in"`
- **Value**: `i64` entry ID from `resolve_ir_type_to_entry_id()`

### 3.2 Path→ID Mapping (`mod.rs:82–88`)

**Function**: `path_to_id()` (mod.rs:82–88)

```rust
pub fn path_to_id(path: &[String]) -> i64 {
    use std::{collections::hash_map::DefaultHasher, hash::{Hash, Hasher}};
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish() as i64
}
```

**Algorithm**: 64-bit stable hash of path segments (module name + symbol name + member names).

**Example paths**:
- `["module"]` → module entry
- `["module", "MyClass"]` → class symbol
- `["module", "MyClass", "constructor"]` → constructor method

### 3.3 ID Resolution (`types.rs:371–386`)

**Function**: `resolve_ir_type_to_entry_id()` (types.rs:371–386)

```rust
pub fn resolve_ir_type_to_entry_id(&self, ty: &Type) -> Option<i64> {
    match ty {
        Type::TypeReference(tr) => {
            if let Some(&id) = self.ctx.type_name_to_id.get(tr.identifier.as_str()) {
                return Some(id);
            }
            for (k, &v) in &self.ctx.type_name_to_id {
                if k.ends_with(tr.identifier.as_str()) {
                    return Some(v);
                }
            }
            None
        }
        _ => None,
    }
}
```

**Logic**:
1. For `Type::TypeReference(identifier)`, lookup in `type_name_to_id`
2. Exact match first, fallback to suffix match
3. Non-reference types → `None` (no link)

### 3.4 Path & Type Name Maps (`mod.rs:51–63, 315–379`)

**Context**: Built during parser initialization (mod.rs:278–289)

```rust
pub(super) struct TsParseContext {
    documents: HashMap<String, Document>,
    path_to_id: HashMap<Vec<String>, i64>,
    type_name_to_id: HashMap<String, i64>,
    module_name_to_specifier: HashMap<String, String>,
    specifier_to_module_name: HashMap<String, String>,
}
```

**Build process** (`mod.rs:315–379`):
- `build_path_map()`: traverses all modules/symbols/members, inserts `path_to_id()` for each
- `build_type_name_map()`: for each path, inserts name-only and fully-qualified keys into `type_name_to_id`

---

### 3.5 Index Assembly (`package.rs:123–128`)

**Function**: `documents_to_ir()` (package.rs:123–128)

```rust
fn documents_to_ir(documents: HashMap<String, Document>) -> Result<Ir<Collected>, PackageError> {
    let mut parser = TsDocParser::from_doc(documents)?;
    let entries = parser.parse()?;
    info!(entries = entries.len(), "IR generation complete");
    Ok(Ir::from_entries(entries))
}
```

**Flow**:
1. `TsDocParser::from_doc()` → builds path/type-name maps
2. `parser.parse()` → iterates sorted paths, calls `item_at_path()` → collects `Entry` list
3. `Ir::from_entries()` → assembles `Index { root_ids, entries_by_path }`

**Note**: Phase 3 will replace this with OXC-based symbol resolution.

---

## 4. Entry Point & Graph Discovery

### 4.1 Entry-Point Resolution (`entry_point.rs`)

**Public functions**:
- `uses_repository(entry_point: &str) -> bool`: checks for `repo:` prefix
- `repository_entry_hint(entry_point: &str) -> Option<&str>`: extracts hint after `repo:`
- `resolve_repository_entry_point(repository_root: &Path, entry_hint: Option<&str>) -> Result<PathBuf, PackageError>`

**Algorithm** (`entry_point.rs:30–51`):
1. If `entry_hint` provided, resolve it within repository root
2. Else, read `package.json` fields (types/typings/module/main)
3. Fallback to conventional locations (mod.ts, index.ts, src/mod.ts, src/index.ts)
4. Use `source_fallback_candidates()` to strip build prefixes (lib/, dist/, esm/, cjs/) and re-resolve

---

### 4.2 Declaration Roots Discovery (`package.rs:156–233`)

**Function**: `documentation_roots_for_entry_point()` (package.rs:156–169)

```rust
pub fn documentation_roots_for_entry_point(
    entry_point: &Path,
) -> Result<Vec<PathBuf>, PackageError> {
    let Some(package_root) = find_package_root(entry_point) else {
        return Ok(vec![entry_point.to_path_buf()]);
    };

    let roots = canonicalize_documentation_roots(resolve_package_documentation_roots(
        &package_root,
        entry_point,
    )?);
    if roots.is_empty() { Ok(vec![entry_point.to_path_buf()]) } else { Ok(roots) }
}
```

**Detailed process** (`resolve_package_documentation_roots()`, package.rs:181–233):
1. Read `package.json` fields: types, typings, module, main, exports
2. Collect declaration files (.d.ts, .d.tsx, etc.) into `preferred_declaration_roots`
3. Collect source files (.ts, .tsx) into `preferred_source_roots`
4. Recursively expand declaration roots by following triple-slash references
5. Deduplicate & sort

**Key helpers**:
- `expand_declaration_roots()`: recursively follows `/// <reference path="">` and `/// <reference types="">`
- `declaration_dependency_specifiers()`: regex-based extraction of import/reference strings
- `resolve_declaration_specifier()`: resolves relative paths to .d.ts candidates

---

### 4.3 Module Graph & Documentation Parsing (`package.rs:92–121`)

**Function**: `build_documents_from_roots()` (package.rs:92–121)

```rust
fn build_documents_from_roots(
    roots: Vec<ModuleSpecifier>,
    loader: &impl Loader,
) -> Result<HashMap<String, Document>, PackageError> {
    let analyzer = CapturingModuleAnalyzer::default();
    let mut graph = ModuleGraph::new(GraphKind::TypesOnly);
    block_on(async {
        graph
            .build(roots.clone(), Vec::new(), loader, BuildOptions {
                module_analyzer: &analyzer,
                ..Default::default()
            })
            .await;
    })?;

    let entry_path = roots
        .first()
        .and_then(|s| s.to_file_path().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let parser = DocParser::new(&graph, &analyzer, &roots, DocParserOptions {
        diagnostics: false,
        private: true,
    })
    .map_err(|source| PackageError::DocParserCreationFailed { entry: entry_path.clone(), source })?;
    let parse_output = parser.parse()
        .map_err(|source| PackageError::DocParseFailed { entry: entry_path, source })?;
    Ok(parse_output
        .into_iter()
        .map(|(specifier, document)| (specifier.to_string(), document))
        .collect())
}
```

**Flow**:
1. Build `deno_graph::ModuleGraph` with `GraphKind::TypesOnly`
2. Use `SourceFileLoader` to serve file:// URLs
3. Create `deno_doc::DocParser` with `diagnostics=false, private=true`
4. Parse → returns `HashMap<String, Document>` keyed by specifier

---

### 4.4 Async Runtime Management (`package.rs:58–90`)

**Function**: `block_on()` (package.rs:68–90)

```rust
fn block_on<F: Future>(future: F) -> Result<F::Output, PackageError> {
    use std::cell::OnceCell;

    thread_local! {
        static RT: OnceCell<tokio::runtime::Runtime> = const { OnceCell::new() };
    }

    RT.with(|cell| {
        let rt = match cell.get() {
            Some(rt) => rt,
            None => {
                let built = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(PackageError::Io)?;
                let _ = cell.set(built);
                cell.get().expect("runtime just installed")
            }
        };
        Ok(rt.block_on(future))
    })
}
```

**Key points**:
- Thread-local, long-lived `tokio::runtime::Runtime`
- `OnceCell` prevents re-entrant panics
- Will be **deleted in Phase 3** (OXC is synchronous)

---

### 4.5 Module Name Assignment (`mod.rs:152–195`)

**Function**: `assign_unique_module_names()` (mod.rs:152–195)

```rust
fn assign_unique_module_names(specifiers: &[String]) -> HashMap<String, String> {
    let mut segment_map: Vec<(String, Vec<String>)> = specifiers
        .iter()
        .cloned()
        .map(|specifier| {
            let segments = specifier_to_module_segments(&specifier);
            (specifier, if segments.is_empty() { vec!["module".to_string()] } else { segments })
        })
        .collect();

    let shared_prefix_len = shared_segment_prefix_len(
        &segment_map.iter().map(|(_, segments)| segments.as_slice()).collect::<Vec<_>>(),
    );
    if shared_prefix_len > 0 {
        for (_, segments) in &mut segment_map {
            if segments.len() > shared_prefix_len {
                segments.drain(..shared_prefix_len);
            }
        }
    }

    let mut assignments = HashMap::with_capacity_and_hasher(segment_map.len(), Default::default());

    for (specifier, segments) in &segment_map {
        let mut chosen = segments.join(".");
        for suffix_len in 1..=segments.len() {
            let candidate = segments[segments.len() - suffix_len..].join(".");
            let duplicate = segment_map.iter().any(|(other_specifier, other_segments)| {
                if other_specifier == specifier {
                    return false;
                }
                other_segments.len() >= suffix_len
                    && other_segments[other_segments.len() - suffix_len..].join(".") == candidate
            });
            if !duplicate {
                chosen = candidate;
                break;
            }
        }
        assignments.insert(specifier.clone(), chosen);
    }

    assignments
}
```

**Output**: `HashMap<specifier_string, module_name>` where module names are:
- Unique within the package
- Suffix-based (lib/foo.ts and src/foo.ts → "foo" and "src.foo")
- Derived from path segments with file extensions stripped

---

## 5. Error Taxonomy

### 5.1 Public Error Types

All defined in `/Users/philocalyst/Projects/Backend/workspace/compiler/compile/typescript/error.rs`:

#### `TsTypeError` (error.rs:13–39)

Emitted during type expression lowering (TsTypeDef → Type):

```rust
pub enum TsTypeError {
    #[error("missing key parameter in index signature")]
    MissingIndexSignatureKeyParameter,

    #[error("missing key type in index signature")]
    MissingIndexSignatureKeyType,

    #[error("missing value type in index signature")]
    MissingIndexSignatureValueType,

    #[error("missing source constraint for mapped type (type param `{param}`)")]
    MissingMappedTypeConstraint { param: String },

    #[error("type reference resolution failed for `{identifier}`")]
    TypeReferenceResolutionFailed { identifier: String },

    #[error("unsupported TS type definition kind `{kind}` while lowering `{context}`")]
    UnsupportedTypeDefinitionKind { kind: String, context: String },

    #[error("generic type parameter parse failed for `{name}`")]
    GenericTypeParameterFailed { name: String },

    #[error("type literal could not be lowered to record shape")]
    TypeLiteralLoweringFailed,
}
```

#### `TsDeclarationError` (error.rs:44–57)

Emitted during symbol/declaration lookup and lowering:

```rust
pub enum TsDeclarationError {
    #[error("symbol not found: {module}::{symbol}")]
    SymbolNotFound { module: String, symbol: String },

    #[error("declaration has no usable kind for symbol `{symbol}`")]
    NoUsableDeclaration { symbol: String },

    #[error("unsupported declaration kind: {kind}")]
    UnsupportedDeclarationKind { kind: String },

    #[error("namespace element `{element}` could not be resolved under `{parent}`")]
    NamespaceElementResolutionFailed { parent: String, element: String },
}
```

#### `TsInterfaceError` (error.rs:62–75)

Emitted during interface/trait definition lowering:

```rust
pub enum TsInterfaceError {
    #[error("interface method parsing failed for `{name}`")]
    InterfaceMethodParsingFailed { name: String },

    #[error("call signature parsing failed at position {index}")]
    CallSignatureParsingFailed { index: usize },

    #[error("index signature (as method) parsing failed at position {index}")]
    IndexSignatureAsMethodFailed { index: usize },

    #[error("generic constraint resolution failed for parameter `{name}`")]
    GenericConstraintResolutionFailed { name: String },
}
```

#### `Parse` (error.rs:78–101)

Umbrella error for IR lowering, aggregates the three above:

```rust
pub enum Parse {
    #[error(transparent)]
    Type(#[from] TsTypeError),

    #[error(transparent)]
    Declaration(#[from] TsDeclarationError),

    #[error(transparent)]
    Interface(#[from] TsInterfaceError),

    #[error("invalid parameter shape in `{context}`: {detail}")]
    InvalidParameter { context: String, detail: String },

    #[error("circular dependency detected at path: {path:?}")]
    CircularDependency { path: PathBuf },

    #[error("parameter parsing failed: {detail}")]
    ParameterParsingFailed { detail: String },

    #[error("deno-doc symbol lowering issue: {detail}")]
    DenoDocSymbolIssue { detail: String },
}
```

#### `Package` (error.rs:105–141)

Entry point / package-level failures:

```rust
pub enum Package {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Parse(#[from] Parse),

    #[error(transparent)]
    Serialization(#[from] serde_json::Error),

    #[error("invalid local entry point `{path:?}`")]
    InvalidLocalEntryPoint { path: PathBuf },

    #[error("could not discover a TypeScript entry point under `{path:?}`")]
    EntryPointDiscoveryFailed { path: PathBuf },

    #[error("entry point `{path:?}` does not exist")]
    EntryPointMissing { path: PathBuf },

    #[error("could not convert entry point `{path:?}` into module specifier")]
    InvalidModuleSpecifier { path: PathBuf },

    #[error("entry point `{candidate:?}` does not exist under root `{root:?}`")]
    EntryPointDoesNotExist { candidate: PathBuf, root: PathBuf },

    #[error("TypeScript document graph generation failed for `{entry:?}`")]
    GraphGenerationFailed { entry: PathBuf, #[source] source: AnyhowError },

    #[error("deno-doc parser creation failed under `{entry:?}`")]
    DocParserCreationFailed { entry: PathBuf, #[source] source: AnyhowError },

    #[error("deno-doc parse failed under `{entry:?}`")]
    DocParseFailed { entry: PathBuf, #[source] source: DocError },
}
```

---

### 5.2 Public Re-exports (`mod.rs:24–28`)

```rust
pub use self::{
    error::{Package, Parse, TsDeclarationError, TsInterfaceError, TsTypeError},
    package::TypescriptPackage,
    producer::TypescriptProducer,
};
```

**Must be preserved** in OXC port.

---

## Summary: Critical Invariants for Port

1. **Entry output shapes** must match exactly — each declaration kind produces a specific `Entry` variant with specific field populations.

2. **Type definitions** must construct the exact IR types defined in §2 — field-for-field, variant-for-variant.

3. **type_links IDs** must remain stable hashes of path segments (can be replaced by OXC symbol resolution in Phase 3, but the key format is structural).

4. **Module name assignment** logic must be replicated if relative path deduplication is required.

5. **Error variants** must be preserved for API compatibility, though OXC may fail in different places.

6. **Async machinery** (tokio runtime) will be deleted in Phase 3 (OXC is sync).

---

**End of OXC Port Spec**
