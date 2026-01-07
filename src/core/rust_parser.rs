use rustdoc_types::{Crate, Id, Item, ItemEnum};
use std::collections::{HashMap, HashSet};

pub type Result<T> = std::result::Result<T, ParseError>;

use crate::{
    core::rust::ParseError,
    error::NewDocsError,
    ir::{
        entry::Entry,
        function::{Attribute as FnAttribute, Function},
        generics::*,
        kind::{Kind, Visibility},
        parameter::Parameter,
        primitives::Primitive,
        protocols::*,
        record::*,
        ty::{DynTrait, FunctionPointer, Path as IRPath, PolyTrait, QualifiedPath, Type},
    },
};

impl From<crate::core::rust::ParseError> for NewDocsError {
    fn from(err: crate::core::rust::ParseError) -> Self {
        NewDocsError::ParseError(err.to_string())
    }
}

pub struct RustdocParser {
    krate: Crate,
    /// Maps rustdoc IDs to resolved paths
    id_to_path: HashMap<Id, Vec<String>>,
    /// Tracks visited items to detect circular dependencies
    visiting: HashSet<Id>,
    /// Cache of parsed entries
    entry_cache: HashMap<Id, Entry>,
}

impl RustdocParser {
    pub fn new(krate: Crate) -> Result<Self> {
        let mut parser = Self {
            krate,
            id_to_path: HashMap::new(),
            visiting: HashSet::new(),
            entry_cache: HashMap::new(),
        };
        parser.build_path_map()?;
        Ok(parser)
    }

    fn build_path_map(&mut self) -> Result<()> {
        let root = self.krate.root.clone();
        self.build_path_map_recursive(&root, vec![])?;
        Ok(())
    }

    fn build_path_map_recursive(&mut self, id: &Id, mut path: Vec<String>) -> Result<()> {
        let item = self
            .krate
            .index
            .get(id)
            .ok_or_else(|| ParseError::ItemNotFound(id.0))?;

        let name = item.name.clone();
        let children: Vec<_> = match &item.inner {
            ItemEnum::Module(m) => m.items.clone(),
            ItemEnum::Struct(s) => s.impls.clone(),
            ItemEnum::Enum(e) => e.impls.clone(),
            ItemEnum::Trait(t) => t.items.clone(),
            _ => vec![],
        };

        if let Some(n) = name {
            path.push(n);
        }

        self.id_to_path.insert(id.clone(), path.clone());

        for child_id in children {
            self.build_path_map_recursive(&child_id, path.clone())?;
        }

        Ok(())
    }

    pub fn parse_crate(&mut self) -> Result<Vec<Entry>> {
        let index = self.krate.index.clone();

        let root_item = index
            .get(&self.krate.root)
            .ok_or_else(|| ParseError::ItemNotFound(self.krate.root.0.clone()))?;

        let mut entries = Vec::new();

        if let ItemEnum::Module(module) = &root_item.inner {
            for item_id in &module.items {
                if let Ok(entry) = self.parse_item(item_id) {
                    entries.push(entry);
                }
            }
        }

        Ok(entries)
    }

    fn parse_item(&mut self, id: &Id) -> Result<Entry> {
        // Check cache first
        if let Some(cached) = self.entry_cache.get(id) {
            return Ok(cached.clone());
        }

        // Detect circular dependencies
        if self.visiting.contains(id) {
            return Err(ParseError::CircularDependency {
                path: self.get_path(id)?.join("::"),
            });
        }

        self.visiting.insert(id.clone());

        let index = self.krate.index.clone();

        let item = index
            .get(id)
            .ok_or_else(|| ParseError::ItemNotFound(id.0))?;

        let entry = self.convert_item(id, item)?;

        self.visiting.remove(id);
        self.entry_cache.insert(id.clone(), entry.clone());

        Ok(entry)
    }

    fn convert_item(&mut self, id: &Id, item: &Item) -> Result<Entry> {
        let name = item.name.clone().unwrap_or_default();
        // Path logic handles the lookup, potentially falling back if path map failed
        let path = self.get_path(id).unwrap_or_else(|_| vec![name.clone()]);
        let visibility = Some(self.parse_visibility(&item.visibility));
        let documentation = item.docs.clone();
        let kind = self.parse_item_kind(id, &item.inner)?;
        let id_num = self.id_to_number(id);

        // Identify items that have impls and collect their children
        let members = match &item.inner {
            ItemEnum::Struct(s) => Some(self.collect_impl_members(&s.impls)?),
            ItemEnum::Enum(e) => Some(self.collect_impl_members(&e.impls)?),
            ItemEnum::Union(u) => Some(self.collect_impl_members(&u.impls)?),
            ItemEnum::Primitive(p) => Some(self.collect_impl_members(&p.impls)?),
            // Trait definitions already contain their required/provided methods in `parse_trait`
            // but if we want them in `members` as well:
            ItemEnum::Trait(t) => {
                let mut trait_members = Vec::new();
                for method_id in &t.items {
                    if let Ok(entry) = self.parse_item(method_id) {
                        trait_members.push(entry);
                    }
                }
                Some(trait_members)
            }
            _ => None,
        };

        Ok(Entry {
            name,
            id: id_num,
            path,
            kind,
            visibility,
            documentation,
            members,
            input_parameters: None,
            output_parameters: None,
            type_parameters: None,
        })
    }

    fn parse_item_kind(&mut self, id: &Id, inner: &ItemEnum) -> Result<Kind> {
        match inner {
            ItemEnum::Module(_) => Ok(Kind::Module),

            ItemEnum::Struct(s) => {
                let record = self.parse_struct(id, s)?;
                Ok(Kind::RecordType(record))
            }

            ItemEnum::Enum(e) => {
                let variants = self.parse_enum_variants(e)?;
                Ok(Kind::SumType(variants))
            }

            ItemEnum::Function(f) => {
                let function = self.parse_function(id, f)?;
                Ok(Kind::Function(function))
            }

            ItemEnum::Trait(t) => {
                let trait_def = self.parse_trait(id, t)?;
                Ok(Kind::TraitDef(trait_def))
            }

            ItemEnum::Impl(i) => {
                let trait_impl = self.parse_impl(id, i)?;
                Ok(Kind::TraitImpl(trait_impl))
            }

            ItemEnum::TypeAlias(_) => Ok(Kind::TypeAlias),

            ItemEnum::Constant { .. } => Ok(Kind::Constant),

            ItemEnum::Static(_) => Ok(Kind::Variable),

            ItemEnum::Macro(_) => Ok(Kind::Macro),

            ItemEnum::ProcMacro(_) => Ok(Kind::Macro),

            ItemEnum::Primitive(_) => Ok(Kind::PrimitiveType),

            ItemEnum::Union(u) => {
                let types = self.parse_union_fields(u)?;
                Ok(Kind::UnionType(types))
            }

            ItemEnum::StructField(ty) => Ok(Kind::Field),

            _ => Err(ParseError::UnsupportedItemType(format!("{:?}", inner))),
        }
    }

    fn parse_struct(&mut self, id: &Id, s: &rustdoc_types::Struct) -> Result<Record> {
        let index = self.krate.index.clone();

        let item = index.get(id).unwrap();
        let generics = s
            .generics
            .params
            .is_empty()
            .then(|| self.parse_generic_params(&s.generics))
            .flatten();

        let generic_args = generics.map(|g| self.generics_to_args(&g)).flatten();

        let (kind, fields) = match &s.kind {
            rustdoc_types::StructKind::Unit => (RecordKind::Unit, None),
            rustdoc_types::StructKind::Tuple(field_ids) => {
                let fields = self.parse_tuple_fields(field_ids)?;
                (RecordKind::Tuple, Some(fields))
            }
            rustdoc_types::StructKind::Plain {
                fields: field_ids, ..
            } => {
                let fields = self.parse_named_fields(field_ids)?;
                (RecordKind::Named, Some(fields))
            }
        };

        let visibility = Some(self.parse_visibility(&item.visibility));

        Ok(Record {
            name: item.name.clone(),
            generics: generic_args,
            kind,
            fields,
            visibility,
        })
    }

    fn parse_tuple_fields(&mut self, field_ids: &[Option<Id>]) -> Result<Vec<RecordField>> {
        field_ids
            .iter()
            .enumerate()
            .filter_map(|(idx, opt_id)| {
                opt_id.as_ref().map(|id| {
                    let index = self.krate.index.clone();
                    let item = index.get(id)?;
                    if let ItemEnum::StructField(ty) = &item.inner {
                        let ty = self.parse_type(ty).ok()?;
                        Some(RecordField {
                            name: Some(idx.to_string()),
                            ty: Some(Box::new(ty)),
                            default_value: None,
                            attributes: None,
                            visibility: Some(self.parse_visibility(&item.visibility)),
                        })
                    } else {
                        None
                    }
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| ParseError::MissingField {
                field: "tuple fields".to_string(),
                context: "struct".to_string(),
            })
    }

    fn parse_named_fields(&mut self, field_ids: &[Id]) -> Result<Vec<RecordField>> {
        field_ids
            .iter()
            .map(|id| {
                let item = self
                    .krate
                    .index
                    .get(id)
                    .ok_or_else(|| ParseError::ItemNotFound(id.0))?
                    .clone();

                if let ItemEnum::StructField(ty) = &item.inner {
                    let ty = self.parse_type(&ty)?;
                    Ok(RecordField {
                        name: item.name.clone(),
                        ty: Some(Box::new(ty)),
                        default_value: None,
                        attributes: None,
                        visibility: Some(self.parse_visibility(&item.visibility)),
                    })
                } else {
                    Err(ParseError::InvalidItemKind {
                        id: id.0.to_string(),
                        expected: "StructField".to_string(),
                        actual: format!("{:?}", item.inner),
                    })
                }
            })
            .collect()
    }

    fn parse_enum_variants(&mut self, e: &rustdoc_types::Enum) -> Result<Vec<SumVariant>> {
        e.variants
            .iter()
            .map(|variant_id| {
                let index = self.krate.index.clone();
                let item = index
                    .get(variant_id)
                    .ok_or_else(|| ParseError::ItemNotFound(variant_id.0.clone()))?;

                let name = item.name.clone().unwrap_or_default();

                if let ItemEnum::Variant(v) = &item.inner {
                    let types = match &v.kind {
                        rustdoc_types::VariantKind::Plain => None,
                        rustdoc_types::VariantKind::Tuple(fields) => {
                            let types: Result<Vec<Type>> = fields
                                .iter()
                                .filter_map(|opt_id| opt_id.as_ref())
                                .map(|id| {
                                    let field_item = index
                                        .get(id)
                                        .ok_or_else(|| ParseError::ItemNotFound(id.0.clone()))?;
                                    if let ItemEnum::StructField(ty) = &field_item.inner {
                                        self.parse_type(ty)
                                    } else {
                                        Err(ParseError::InvalidItemKind {
                                            id: id.0.to_string(),
                                            expected: "StructField".to_string(),
                                            actual: format!("{:?}", field_item.inner),
                                        })
                                    }
                                })
                                .collect();
                            Some(types?)
                        }
                        rustdoc_types::VariantKind::Struct { fields, .. } => {
                            let types: Result<Vec<Type>> = fields
                                .iter()
                                .map(|id| {
                                    let field_item = index
                                        .get(id)
                                        .ok_or_else(|| ParseError::ItemNotFound(id.0.clone()))?;
                                    if let ItemEnum::StructField(ty) = &field_item.inner {
                                        self.parse_type(&ty)
                                    } else {
                                        Err(ParseError::InvalidItemKind {
                                            id: id.0.to_string(),
                                            expected: "StructField".to_string(),
                                            actual: format!("{:?}", field_item.inner),
                                        })
                                    }
                                })
                                .collect();
                            Some(types?)
                        }
                    };

                    Ok(SumVariant { name, types })
                } else {
                    Err(ParseError::InvalidItemKind {
                        id: variant_id.0.to_string(),
                        expected: "Variant".to_string(),
                        actual: format!("{:?}", item.inner),
                    })
                }
            })
            .collect()
    }

    fn parse_function(&mut self, id: &Id, f: &rustdoc_types::Function) -> Result<Function> {
        let item = self.krate.index.get(id).unwrap();
        let visibility_clone = item.visibility.clone();
        let name = item.name.clone().unwrap_or_default();

        let input_parameters = if f.sig.inputs.is_empty() {
            None
        } else {
            Some(self.parse_function_inputs(&f.sig.inputs)?)
        };

        let output_parameters = if let Some(output_ty) = &f.sig.output {
            Some(vec![Parameter {
                name: "return".to_string(),
                ty: Some(self.parse_type(output_ty)?),
                attributes: None,
                default_value: None,
                description: None,
            }])
        } else {
            None
        };

        let attributes = self.parse_function_attributes(f);

        let generics = if f.generics.params.is_empty() {
            None
        } else {
            self.parse_generic_params(&f.generics)
        };

        let visibility = Some(self.parse_visibility(&visibility_clone));

        Ok(Function {
            input_parameters,
            output_parameters,
            attributes,
            generics,
            name,
            implemented: true,
            visibility,
        })
    }

    fn parse_function_inputs(
        &mut self,
        inputs: &[(String, rustdoc_types::Type)],
    ) -> Result<Vec<Parameter>> {
        inputs
            .iter()
            .map(|(name, ty)| {
                let parsed_ty = self.parse_type(ty)?;
                Ok(Parameter {
                    name: name.clone(),
                    ty: Some(parsed_ty),
                    attributes: None,
                    default_value: None,
                    description: None,
                })
            })
            .collect()
    }

    fn parse_function_attributes(&self, f: &rustdoc_types::Function) -> Option<Vec<FnAttribute>> {
        let mut attrs = Vec::new();

        let header = &f.header;
        if header.is_const {
            attrs.push(FnAttribute::Const);
        }
        if header.is_unsafe {
            attrs.push(FnAttribute::Unsafe);
        }
        if header.is_async {
            attrs.push(FnAttribute::Async);
        }

        if attrs.is_empty() { None } else { Some(attrs) }
    }

    fn parse_trait(&mut self, id: &Id, t: &rustdoc_types::Trait) -> Result<TraitDef> {
        let item = self.krate.index.get(id).unwrap();
        let visibility_clone = item.visibility.clone();
        let name = item.name.clone().unwrap_or_default();
        let docs = item.docs.clone();

        let generics = if t.generics.params.is_empty() {
            None
        } else {
            self.parse_generic_params(&t.generics)
        };

        let super_traits = if t.bounds.is_empty() {
            None
        } else {
            Some(self.parse_trait_bounds(&t.bounds)?)
        };

        let mut required_methods = Vec::new();
        let mut provided_methods = Vec::new();
        let mut associated_types = Vec::new();
        let mut required_constants = Vec::new();

        // Clone items to avoid borrow issues
        let trait_items: Vec<Id> = t.items.clone();

        for item_id in trait_items {
            let trait_item = self
                .krate
                .index
                .get(&item_id)
                .ok_or_else(|| ParseError::ItemNotFound(item_id.0))?
                .clone();

            match &trait_item.inner {
                ItemEnum::Function(f) => {
                    let method = self.parse_trait_method(&item_id, f)?;
                    if f.has_body {
                        provided_methods.push(method);
                    } else {
                        required_methods.push(method);
                    }
                }
                ItemEnum::AssocType {
                    generics: _,
                    bounds,
                    type_,
                } => {
                    let assoc_type = AssociatedType {
                        name: trait_item.name.clone().unwrap_or_default(),
                        bounds: if bounds.is_empty() {
                            None
                        } else {
                            Some(self.parse_generic_bounds(bounds)?)
                        },
                        default_type: if let Some(ty) = type_ {
                            Some(self.parse_type(ty)?)
                        } else {
                            None
                        },
                        docs: trait_item.docs.clone(),
                    };
                    associated_types.push(assoc_type);
                }
                ItemEnum::AssocConst { type_, value } => {
                    let constant = TraitConstant {
                        name: trait_item.name.clone().unwrap_or_default(),
                        ty: Box::new(self.parse_type(type_)?),
                        default_value: value.as_ref().map(|d| ConstExpr { expr: d.clone() }),
                        docs: trait_item.docs.clone(),
                    };
                    required_constants.push(constant);
                }
                _ => {}
            }
        }

        let attributes = if t.is_auto {
            Some(vec![TraitAttribute::Auto])
        } else if t.is_unsafe {
            Some(vec![TraitAttribute::Unsafe])
        } else {
            None
        };

        let visibility = Some(self.parse_visibility(&visibility_clone));

        Ok(TraitDef {
            name,
            generics,
            super_traits,
            associated_types: if associated_types.is_empty() {
                None
            } else {
                Some(associated_types)
            },
            required_methods: if required_methods.is_empty() {
                None
            } else {
                Some(required_methods)
            },
            provided_methods: if provided_methods.is_empty() {
                None
            } else {
                Some(provided_methods)
            },
            required_constants: if required_constants.is_empty() {
                None
            } else {
                Some(required_constants)
            },
            attributes,
            visibility,
            docs,
        })
    }

    fn parse_trait_method(&mut self, id: &Id, f: &rustdoc_types::Function) -> Result<TraitMethod> {
        let index = self.krate.index.clone();
        let item = index.get(id).unwrap();

        let parameters = if f.sig.inputs.is_empty() {
            None
        } else {
            Some(self.parse_function_inputs(&f.sig.inputs)?)
        };

        let return_type = f
            .sig
            .output
            .as_ref()
            .map(|ty| self.parse_type(ty).map(Box::new))
            .transpose()?;

        let generics = if f.generics.params.is_empty() {
            None
        } else {
            self.parse_generic_params(&f.generics)
        };

        let attributes = self.parse_function_attributes(f);

        let receiver = Self::determine_receiver(&f.sig.inputs);

        Ok(TraitMethod {
            name: item.name.clone().unwrap_or_default(),
            parameters,
            return_type,
            generics,
            attributes,
            receiver,
            has_default_implementation: f.has_body,
            docs: item.docs.clone(),
        })
    }

    fn determine_receiver(inputs: &[(String, rustdoc_types::Type)]) -> Option<ReceiverKind> {
        if let Some((name, ty)) = inputs.first() {
            if name == "self" {
                return Some(ReceiverKind::Owned);
            }
            match ty {
                rustdoc_types::Type::BorrowedRef { is_mutable, .. } => {
                    if *is_mutable {
                        Some(ReceiverKind::MutRef)
                    } else {
                        Some(ReceiverKind::SharedRef)
                    }
                }
                _ => Some(ReceiverKind::Static),
            }
        } else {
            Some(ReceiverKind::Static)
        }
    }

    fn parse_impl(&mut self, id: &Id, i: &rustdoc_types::Impl) -> Result<TraitImpl> {
        let index = self.krate.index.clone();
        let item = index.get(id).unwrap();

        let tr = i
            .trait_
            .as_ref()
            .map(|path| self.parse_path_to_trait_ref(path))
            .transpose()?
            .ok_or_else(|| ParseError::ImplBlockParsing {
                reason: "Inherent impl blocks not supported as TraitImpl".to_string(),
            })?;

        let for_type = Box::new(self.parse_type(&i.for_)?);

        let generics = if i.generics.params.is_empty() {
            None
        } else {
            self.parse_generic_params(&i.generics)
        };

        let where_constraints = if i.generics.where_predicates.is_empty() {
            None
        } else {
            Some(self.parse_where_predicates(&i.generics.where_predicates)?)
        };

        let mut methods = Vec::new();
        let mut associated_types = Vec::new();
        let mut associated_constants = Vec::new();

        for item_id in &i.items {
            let index = self.krate.index.clone();

            let impl_item = index
                .get(item_id)
                .ok_or_else(|| ParseError::ItemNotFound(item_id.0.clone()))?;

            match &impl_item.inner {
                ItemEnum::Function(f) => {
                    let function = self.parse_function(item_id, f)?;
                    methods.push(function);
                }
                ItemEnum::AssocType { type_, .. } => {
                    if let Some(ty) = type_ {
                        let assoc_type_impl = AssociatedTypeImpl {
                            name: impl_item.name.clone().unwrap_or_default(),
                            ty: Box::new(self.parse_type(ty)?),
                        };
                        associated_types.push(assoc_type_impl);
                    }
                }
                ItemEnum::AssocConst { type_, value } => {
                    let constant = TraitConstant {
                        name: impl_item.name.clone().unwrap_or_default(),
                        ty: Box::new(self.parse_type(type_)?),
                        default_value: value.as_ref().map(|d| ConstExpr { expr: d.clone() }),
                        docs: impl_item.docs.clone(),
                    };
                    associated_constants.push(constant);
                }
                _ => {}
            }
        }

        let visibility = Some(self.parse_visibility(&item.visibility));

        Ok(TraitImpl {
            tr,
            for_type,
            generics,
            where_constraints,
            methods: if methods.is_empty() {
                None
            } else {
                Some(methods)
            },
            associated_types: if associated_types.is_empty() {
                None
            } else {
                Some(associated_types)
            },
            associated_constants: if associated_constants.is_empty() {
                None
            } else {
                Some(associated_constants)
            },
            is_negative: i.is_negative,
            is_blanket: i.blanket_impl.is_some(),
            is_unsafe: i.is_unsafe,
            visibility,
            docs: item.docs.clone(),
        })
    }

    fn parse_union_fields(&mut self, u: &rustdoc_types::Union) -> Result<Vec<Type>> {
        u.fields
            .iter()
            .map(|id| {
                let index = self.krate.index.clone();

                let item = index
                    .get(id)
                    .ok_or_else(|| ParseError::ItemNotFound(id.0.clone()))?;
                if let ItemEnum::StructField(ty) = &item.inner {
                    self.parse_type(ty)
                } else {
                    Err(ParseError::InvalidItemKind {
                        id: id.0.to_string(),
                        expected: "StructField".to_string(),
                        actual: format!("{:?}", item.inner),
                    })
                }
            })
            .collect()
    }

    fn parse_type(&mut self, ty: &rustdoc_types::Type) -> Result<Type> {
        match ty {
            rustdoc_types::Type::ResolvedPath(path) => {
                let ir_path = self.parse_resolved_path(path)?;
                Ok(Type::ResolvedPath(ir_path))
            }

            rustdoc_types::Type::DynTrait(dyn_trait) => {
                let traits = dyn_trait
                    .traits
                    .iter()
                    .map(|pt| self.parse_poly_trait(pt))
                    .collect::<Result<Vec<_>>>()?;

                Ok(Type::DynTrait(DynTrait {
                    traits,
                    lifetime: dyn_trait.lifetime.clone(),
                }))
            }

            rustdoc_types::Type::Generic(name) => Ok(Type::GenericParam(name.clone())),

            rustdoc_types::Type::Primitive(prim) => {
                Ok(Type::Primitive(self.parse_primitive(prim)?))
            }

            rustdoc_types::Type::FunctionPointer(fp) => {
                let function_pointer = self.parse_function_pointer(fp)?;
                Ok(Type::FunctionPointer(function_pointer))
            }

            rustdoc_types::Type::Tuple(types) => {
                let parsed_types = types
                    .iter()
                    .map(|t| self.parse_type(t))
                    .collect::<Result<Vec<_>>>()?;
                Ok(Type::Tuple(parsed_types))
            }

            rustdoc_types::Type::Slice(inner) => {
                let parsed_inner = Box::new(self.parse_type(inner)?);
                Ok(Type::Slice(parsed_inner))
            }

            rustdoc_types::Type::Array { type_, len } => {
                let parsed_ty = Box::new(self.parse_type(type_)?);
                let length = len
                    .parse::<usize>()
                    .map_err(|_| ParseError::TypeResolution {
                        type_name: "array".to_string(),
                        reason: format!("Invalid array length: {}", len),
                    })?;
                Ok(Type::Array {
                    ty: parsed_ty,
                    length,
                })
            }

            rustdoc_types::Type::Pat { type_, .. } => {
                let parsed_ty = Box::new(self.parse_type(type_)?);
                Ok(Type::Pattern { ty: parsed_ty })
            }

            rustdoc_types::Type::ImplTrait(bounds) => {
                let generic_bounds = self.parse_generic_bounds(bounds)?;
                Ok(Type::ImplTrait(generic_bounds))
            }

            rustdoc_types::Type::Infer => Ok(Type::Infer),

            rustdoc_types::Type::RawPointer { is_mutable, type_ } => {
                let parsed_ty = Box::new(self.parse_type(type_)?);
                Ok(Type::RawPointer {
                    is_mutable: *is_mutable,
                    ty: parsed_ty,
                })
            }

            rustdoc_types::Type::BorrowedRef {
                lifetime,
                is_mutable,
                type_,
            } => {
                let parsed_ty = Box::new(self.parse_type(type_)?);
                Ok(Type::BorrowedRef {
                    lifetime: lifetime.clone(),
                    is_mutable: *is_mutable,
                    ty: parsed_ty,
                })
            }

            rustdoc_types::Type::QualifiedPath {
                name,
                args,
                self_type,
                trait_,
            } => {
                let parsed_self_type = Box::new(self.parse_type(self_type)?);
                let parsed_trait = trait_
                    .as_ref()
                    .map(|path| self.parse_resolved_path(path))
                    .transpose()?;
                let generic_args = args
                    .as_ref()
                    .map(|ga| self.parse_generic_args(ga))
                    .transpose()?
                    .and_then(|v| if v.is_empty() { None } else { Some(v) });

                Ok(Type::QualifiedPath(QualifiedPath {
                    name: name.clone(),
                    generic_arguments: generic_args,
                    self_type: parsed_self_type,
                    tr: parsed_trait,
                }))
            }
        }
    }

    fn parse_resolved_path(&mut self, path: &rustdoc_types::Path) -> Result<IRPath> {
        let path_str = path.path.clone();
        let generic_args = path
            .args
            .as_ref()
            .map(|ga| self.parse_generic_args(ga))
            .transpose()?
            .and_then(|v| if v.is_empty() { None } else { Some(v) });

        Ok(IRPath {
            path: path_str,
            generic_args,
        })
    }

    fn parse_poly_trait(&mut self, pt: &rustdoc_types::PolyTrait) -> Result<PolyTrait> {
        let trait_ref = self.parse_path_to_trait_ref(&pt.trait_)?;
        Ok(PolyTrait {
            tr: trait_ref,
            lifetimes: pt.generic_params.iter().map(|gp| gp.name.clone()).collect(),
        })
    }

    fn parse_path_to_trait_ref(&mut self, path: &rustdoc_types::Path) -> Result<TraitRef> {
        let args = path
            .args
            .as_ref()
            .map(|ga| {
                self.parse_generic_args(ga).and_then(|args| {
                    args.into_iter()
                        .map(|arg| match arg {
                            GenericArg::Type(ty) => Ok(TypeExpr {
                                name: format!("{:?}", ty),
                                args: vec![],
                            }),
                            GenericArg::ConstExpr(ce) => Ok(TypeExpr {
                                name: ce.expr,
                                args: vec![],
                            }),
                            GenericArg::Lifetime(lt) => Ok(TypeExpr {
                                name: lt,
                                args: vec![],
                            }),
                        })
                        .collect()
                })
            })
            .transpose()?
            .unwrap_or_default();

        Ok(TraitRef {
            name: path.path.clone(),
            args,
        })
    }

    fn parse_primitive(&self, prim: &str) -> Result<Primitive> {
        match prim {
            "i8" => Ok(Primitive::Int8(None)),
            "i16" => Ok(Primitive::Int16(None)),
            "isize" => Ok(Primitive::Int(None)),
            "i64" => Ok(Primitive::Int64(None)),
            "i128" => Ok(Primitive::Int128(None)),
            "u8" => Ok(Primitive::UInt8(None)),
            "u16" => Ok(Primitive::UInt16(None)),
            "usize" => Ok(Primitive::UInt(None)),
            "u64" => Ok(Primitive::UInt64(None)),
            "u128" => Ok(Primitive::UInt128(None)),
            "f32" => Ok(Primitive::Float(None)),
            "f64" => Ok(Primitive::Double(None)),
            "bool" => Ok(Primitive::Bool(None)),
            "str" => Ok(Primitive::String(None)),
            "char" => Ok(Primitive::Char(None)),
            _ => Err(ParseError::InvalidPrimitive(prim.to_string())),
        }
    }

    fn parse_function_pointer(
        &mut self,
        fp: &rustdoc_types::FunctionPointer,
    ) -> Result<FunctionPointer> {
        let inputs = if fp.sig.inputs.is_empty() {
            None
        } else {
            Some(self.parse_function_inputs(&fp.sig.inputs)?)
        };

        let outputs = fp
            .sig
            .output
            .as_ref()
            .map(|ty| {
                self.parse_type(ty).map(|t| {
                    vec![Parameter {
                        name: "return".to_string(),
                        ty: Some(t),
                        attributes: None,
                        default_value: None,
                        description: None,
                    }]
                })
            })
            .transpose()?;

        let generic_params = if fp.generic_params.is_empty() {
            None
        } else {
            Some(
                fp.generic_params
                    .iter()
                    .map(|gp| TypeParam {
                        name: gp.name.clone(),
                        kind: TypeKind::Type,
                        variance: Variance::Invariant,
                        default_type: None,
                    })
                    .collect(),
            )
        };

        let mut attributes = Vec::new();

        if fp.header.is_const {
            attributes.push(FnAttribute::Const);
        }
        if fp.header.is_unsafe {
            attributes.push(FnAttribute::Unsafe);
        }
        if fp.header.is_async {
            attributes.push(FnAttribute::Async);
        }

        Ok(FunctionPointer {
            inputs,
            outputs,
            generic_params,
            attributes: if attributes.is_empty() {
                None
            } else {
                Some(attributes)
            },
        })
    }

    fn parse_generic_params(&mut self, generics: &rustdoc_types::Generics) -> Option<Generics> {
        if generics.params.is_empty() && generics.where_predicates.is_empty() {
            return None;
        }

        let mut type_params = Vec::new();
        let mut const_params = Vec::new();
        let mut lifetime_params = Vec::new();

        for param in &generics.params {
            match &param.kind {
                rustdoc_types::GenericParamDefKind::Type {
                    bounds,
                    default,
                    is_synthetic,
                } => {
                    if !is_synthetic {
                        type_params.push(TypeParam {
                            name: param.name.clone(),
                            kind: TypeKind::Type,
                            variance: Variance::Invariant,
                            default_type: default
                                .as_ref()
                                .and_then(|ty| self.parse_type(ty).ok())
                                .map(|ty| TypeExpr {
                                    name: format!("{:?}", ty),
                                    args: vec![],
                                }),
                        });
                    }
                }
                rustdoc_types::GenericParamDefKind::Const { type_, default } => {
                    if let Ok(ty) = self.parse_type(type_) {
                        const_params.push(ConstParam {
                            name: param.name.clone(),
                            ty: TypeExpr {
                                name: format!("{:?}", ty),
                                args: vec![],
                            },
                            default_value: default.as_ref().map(|d| ConstExpr { expr: d.clone() }),
                        });
                    }
                }
                rustdoc_types::GenericParamDefKind::Lifetime { outlives } => {
                    lifetime_params.push(LifetimeParam {
                        name: param.name.clone(),
                        variance: Variance::Invariant,
                    });
                }
            }
        }

        let constraints = self
            .parse_where_predicates(&generics.where_predicates)
            .unwrap_or_default();

        Some(Generics {
            type_params,
            const_params,
            lifetime_params,
            constraints,
        })
    }

    fn parse_where_predicates(
        &mut self,
        predicates: &[rustdoc_types::WherePredicate],
    ) -> Result<Vec<Constraint>> {
        predicates
            .iter()
            .map(|pred| match pred {
                rustdoc_types::WherePredicate::BoundPredicate {
                    type_,
                    bounds,
                    generic_params,
                } => {
                    let param_name = match type_ {
                        rustdoc_types::Type::Generic(name) => name.clone(),
                        _ => format!("{:?}", type_),
                    };

                    bounds
                        .iter()
                        .map(|bound| self.parse_generic_bound_to_constraint(&param_name, bound))
                        .collect::<Result<Vec<_>>>()
                }
                rustdoc_types::WherePredicate::EqPredicate { lhs, rhs } => {
                    let lhs_str = format!("{:?}", lhs);
                    Ok(vec![Constraint::AssociatedTypeBound {
                        param: lhs_str.clone(),
                        assoc_name: lhs_str,
                        bound: TypeExpr {
                            name: format!("{:?}", rhs),
                            args: vec![],
                        },
                    }])
                }
                rustdoc_types::WherePredicate::LifetimePredicate { lifetime, outlives } => todo!(),
            })
            .collect::<Result<Vec<Vec<_>>>>()
            .map(|v| v.into_iter().flatten().collect())
    }

    fn parse_generic_bound_to_constraint(
        &mut self,
        param: &str,
        bound: &rustdoc_types::GenericBound,
    ) -> Result<Constraint> {
        match bound {
            rustdoc_types::GenericBound::TraitBound {
                trait_,
                generic_params,
                modifier,
            } => {
                let trait_ref = self.parse_path_to_trait_ref(trait_)?;
                Ok(Constraint::TraitBound {
                    param: param.to_string(),
                    trait_ref,
                })
            }
            rustdoc_types::GenericBound::Use(_) => Ok(Constraint::TraitBound {
                param: param.to_string(),
                trait_ref: TraitRef {
                    name: "Use".to_string(),
                    args: vec![],
                },
            }),
            rustdoc_types::GenericBound::Outlives(lifetime) => Ok(Constraint::LifetimeBound {
                shorter: param.to_string(),
                longer: lifetime.clone(),
            }),
        }
    }

    fn parse_trait_bounds(
        &mut self,
        bounds: &[rustdoc_types::GenericBound],
    ) -> Result<Vec<TraitRef>> {
        bounds
            .iter()
            .filter_map(|bound| match bound {
                rustdoc_types::GenericBound::TraitBound { trait_, .. } => {
                    Some(self.parse_path_to_trait_ref(trait_))
                }
                _ => None,
            })
            .collect()
    }

    fn parse_generic_bounds(
        &mut self,
        bounds: &[rustdoc_types::GenericBound],
    ) -> Result<Vec<GenericBound>> {
        bounds
            .iter()
            .map(|bound| match bound {
                rustdoc_types::GenericBound::TraitBound { trait_, .. } => {
                    let trait_ref = self.parse_path_to_trait_ref(trait_)?;
                    Ok(GenericBound::Trait(trait_ref))
                }
                rustdoc_types::GenericBound::Outlives(lifetime) => {
                    Ok(GenericBound::Lifetime(lifetime.clone()))
                }
                rustdoc_types::GenericBound::Use(precise_capturing_args) => todo!(),
            })
            .collect()
    }

    fn parse_generic_args(&mut self, args: &rustdoc_types::GenericArgs) -> Result<Vec<GenericArg>> {
        match args {
            rustdoc_types::GenericArgs::AngleBracketed { args, constraints } => {
                let mut result = Vec::new();

                for arg in args {
                    match arg {
                        rustdoc_types::GenericArg::Lifetime(lt) => {
                            result.push(GenericArg::Lifetime(lt.clone()));
                        }
                        rustdoc_types::GenericArg::Type(ty) => {
                            let parsed_ty = self.parse_type(ty)?;
                            result.push(GenericArg::Type(parsed_ty));
                        }
                        rustdoc_types::GenericArg::Const(c) => {
                            result.push(GenericArg::ConstExpr(ConstExpr {
                                expr: c.expr.clone(),
                            }));
                        }
                        rustdoc_types::GenericArg::Infer => {
                            result.push(GenericArg::Type(Type::Infer));
                        }
                    }
                }

                Ok(result)
            }
            rustdoc_types::GenericArgs::Parenthesized { inputs, output } => {
                let mut result = Vec::new();

                for input in inputs {
                    let parsed_ty = self.parse_type(input)?;
                    result.push(GenericArg::Type(parsed_ty));
                }

                if let Some(output) = output {
                    let parsed_output = self.parse_type(output)?;
                    result.push(GenericArg::Type(parsed_output));
                }

                Ok(result)
            }
            rustdoc_types::GenericArgs::ReturnTypeNotation => Ok(vec![]),
        }
    }

    fn parse_visibility(&self, vis: &rustdoc_types::Visibility) -> Visibility {
        match vis {
            rustdoc_types::Visibility::Public => Visibility::Public,
            rustdoc_types::Visibility::Default => Visibility::Private,
            rustdoc_types::Visibility::Crate => Visibility::Internal,
            rustdoc_types::Visibility::Restricted { .. } => Visibility::Package,
        }
    }

    fn get_path(&self, id: &Id) -> Result<Vec<String>> {
        self.id_to_path
            .get(id)
            .cloned()
            .ok_or_else(|| ParseError::PathParsing(format!("No path found for ID: {}", id.0)))
    }

    fn id_to_number(&self, id: &Id) -> i64 {
        // Simple hash of the ID string to generate a consistent number
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        id.0.hash(&mut hasher);
        hasher.finish() as i64
    }

    fn generics_to_args(&self, generics: &Generics) -> Option<Vec<GenericArg>> {
        let mut args = Vec::new();

        for type_param in &generics.type_params {
            if let Some(default) = &type_param.default_type {
                args.push(GenericArg::Type(Type::GenericParam(default.name.clone())));
            } else {
                args.push(GenericArg::Type(Type::GenericParam(
                    type_param.name.clone(),
                )));
            }
        }

        for const_param in &generics.const_params {
            args.push(GenericArg::ConstExpr(ConstExpr {
                expr: const_param.name.clone(),
            }));
        }

        for lifetime_param in &generics.lifetime_params {
            args.push(GenericArg::Lifetime(lifetime_param.name.clone()));
        }

        if args.is_empty() { None } else { Some(args) }
    }
}
