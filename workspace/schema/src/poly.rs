#![allow(non_camel_case_types)]

use crate::*;
use crate::poly_containers::*;


pub trait Entry   {

    fn id<'a>(&'a self) -> &'a crate::uriorcurie;
    // fn id_mut(&mut self) -> &mut &'a crate::uriorcurie;
    // fn set_id(&mut self, value: uriorcurie);

    fn type_designator<'a>(&'a self) -> Option<&'a str>;
    // fn type_designator_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_type_designator(&mut self, value: Option<&'a str>);

    fn name<'a>(&'a self) -> &'a str;
    // fn name_mut(&mut self) -> &mut &'a str;
    // fn set_name(&mut self, value: String);

    fn fq_name<'a>(&'a self) -> Option<&'a str>;
    // fn fq_name_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_fq_name(&mut self, value: Option<&'a str>);

    fn aliases<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, String>>;
    // fn aliases_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, String>>;
    // fn set_aliases(&mut self, value: Option<&Vec<String>>);

    fn documentation<'a>(&'a self) -> Option<&'a str>;
    // fn documentation_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_documentation(&mut self, value: Option<&'a str>);

    fn visibility<'a>(&'a self) -> Option<&'a crate::Visibility>;
    // fn visibility_mut(&mut self) -> &mut Option<&'a crate::Visibility>;
    // fn set_visibility(&mut self, value: Option<&'a Visibility>);

    fn path<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, String>>;
    // fn path_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, String>>;
    // fn set_path(&mut self, value: Option<&Vec<String>>);

    fn members<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, String>>;
    // fn members_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, String>>;
    // fn set_members(&mut self, value: Option<&Vec<String>>);

    fn implemented_protocols<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, String>>;
    // fn implemented_protocols_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, String>>;
    // fn set_implemented_protocols(&mut self, value: Option<&Vec<String>>);

    fn kind<'a>(&'a self) -> &'a str;
    // fn kind_mut(&mut self) -> &mut &'a str;
    // fn set_kind(&mut self, value: String);


}

impl Entry for crate::Entry {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        return &self.id;
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        return self.type_designator.as_deref();
    }
        fn name<'a>(&'a self) -> &'a str {
        return &self.name[..];
    }
        fn fq_name<'a>(&'a self) -> Option<&'a str> {
        return self.fq_name.as_deref();
    }
        fn aliases<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, String>> {
        return self.aliases.as_ref();
    }
        fn documentation<'a>(&'a self) -> Option<&'a str> {
        return self.documentation.as_deref();
    }
        fn visibility<'a>(&'a self) -> Option<&'a crate::Visibility> {
        return self.visibility.as_ref();
    }
        fn path<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, String>> {
        return self.path.as_ref();
    }
        fn members<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, String>> {
        return self.members.as_ref();
    }
        fn implemented_protocols<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, String>> {
        return self.implemented_protocols.as_ref();
    }
        fn kind<'a>(&'a self) -> &'a str {
        return &self.kind[..];
    }
}


pub trait Kind   {

    fn id<'a>(&'a self) -> &'a crate::uriorcurie;
    // fn id_mut(&mut self) -> &mut &'a crate::uriorcurie;
    // fn set_id(&mut self, value: uriorcurie);

    fn type_designator<'a>(&'a self) -> Option<&'a str>;
    // fn type_designator_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_type_designator(&mut self, value: Option<&'a str>);


}

impl Kind for crate::Kind {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        return &self.id;
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        return self.type_designator.as_deref();
    }
}
impl Kind for crate::Module {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        return &self.id;
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        return self.type_designator.as_deref();
    }
}
impl Kind for crate::Info {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        return &self.id;
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        return self.type_designator.as_deref();
    }
}
impl Kind for crate::InterfaceType {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        return &self.id;
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        return self.type_designator.as_deref();
    }
}
impl Kind for crate::PrimitiveType {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        return &self.id;
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        return self.type_designator.as_deref();
    }
}
impl Kind for crate::Constant {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        return &self.id;
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        return self.type_designator.as_deref();
    }
}
impl Kind for crate::Variable {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        return &self.id;
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        return self.type_designator.as_deref();
    }
}
impl Kind for crate::Macro {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        return &self.id;
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        return self.type_designator.as_deref();
    }
}
impl Kind for crate::FieldKind {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        return &self.id;
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        return self.type_designator.as_deref();
    }
}
impl Kind for crate::Event {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        return &self.id;
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        return self.type_designator.as_deref();
    }
}
impl Kind for crate::Function {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        return &self.id;
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        return self.type_designator.as_deref();
    }
}
impl Kind for crate::RecordType {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        return &self.id;
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        return self.type_designator.as_deref();
    }
}
impl Kind for crate::SumType {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        return &self.id;
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        return self.type_designator.as_deref();
    }
}
impl Kind for crate::UnionType {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        return &self.id;
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        return self.type_designator.as_deref();
    }
}
impl Kind for crate::TraitDef {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        return &self.id;
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        return self.type_designator.as_deref();
    }
}
impl Kind for crate::TraitImpl {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        return &self.id;
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        return self.type_designator.as_deref();
    }
}
impl Kind for crate::TypeAlias {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        return &self.id;
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        return self.type_designator.as_deref();
    }
}

impl Kind for crate::KindOrSubtype {
        fn id<'a>(&'a self) -> &'a crate::uriorcurie {
        match self {
                KindOrSubtype::Module(val) => val.id(),
                KindOrSubtype::Info(val) => val.id(),
                KindOrSubtype::InterfaceType(val) => val.id(),
                KindOrSubtype::PrimitiveType(val) => val.id(),
                KindOrSubtype::Constant(val) => val.id(),
                KindOrSubtype::Variable(val) => val.id(),
                KindOrSubtype::Macro(val) => val.id(),
                KindOrSubtype::FieldKind(val) => val.id(),
                KindOrSubtype::Event(val) => val.id(),
                KindOrSubtype::Function(val) => val.id(),
                KindOrSubtype::RecordType(val) => val.id(),
                KindOrSubtype::SumType(val) => val.id(),
                KindOrSubtype::UnionType(val) => val.id(),
                KindOrSubtype::TraitDef(val) => val.id(),
                KindOrSubtype::TraitImpl(val) => val.id(),
                KindOrSubtype::TypeAlias(val) => val.id(),

        }
    }
        fn type_designator<'a>(&'a self) -> Option<&'a str> {
        match self {
                KindOrSubtype::Module(val) => val.type_designator(),
                KindOrSubtype::Info(val) => val.type_designator(),
                KindOrSubtype::InterfaceType(val) => val.type_designator(),
                KindOrSubtype::PrimitiveType(val) => val.type_designator(),
                KindOrSubtype::Constant(val) => val.type_designator(),
                KindOrSubtype::Variable(val) => val.type_designator(),
                KindOrSubtype::Macro(val) => val.type_designator(),
                KindOrSubtype::FieldKind(val) => val.type_designator(),
                KindOrSubtype::Event(val) => val.type_designator(),
                KindOrSubtype::Function(val) => val.type_designator(),
                KindOrSubtype::RecordType(val) => val.type_designator(),
                KindOrSubtype::SumType(val) => val.type_designator(),
                KindOrSubtype::UnionType(val) => val.type_designator(),
                KindOrSubtype::TraitDef(val) => val.type_designator(),
                KindOrSubtype::TraitImpl(val) => val.type_designator(),
                KindOrSubtype::TypeAlias(val) => val.type_designator(),

        }
    }
}

pub trait Module : Kind   {


}

impl Module for crate::Module {
}


pub trait Info : Kind   {


}

impl Info for crate::Info {
}


pub trait InterfaceType : Kind   {


}

impl InterfaceType for crate::InterfaceType {
}


pub trait PrimitiveType : Kind   {


}

impl PrimitiveType for crate::PrimitiveType {
}


pub trait Constant : Kind   {


}

impl Constant for crate::Constant {
}


pub trait Variable : Kind   {


}

impl Variable for crate::Variable {
}


pub trait Macro : Kind   {


}

impl Macro for crate::Macro {
}


pub trait FieldKind : Kind   {


}

impl FieldKind for crate::FieldKind {
}


pub trait Event : Kind   {


}

impl Event for crate::Event {
}


pub trait Function : Kind   {

    fn name<'a>(&'a self) -> &'a str;
    // fn name_mut(&mut self) -> &mut &'a str;
    // fn set_name(&mut self, value: String);

    fn visibility<'a>(&'a self) -> Option<&'a crate::Visibility>;
    // fn visibility_mut(&mut self) -> &mut Option<&'a crate::Visibility>;
    // fn set_visibility(&mut self, value: Option<&'a Visibility>);

    fn implemented(&self) -> bool;
    // fn implemented_mut(&mut self) -> &mut bool;
    // fn set_implemented(&mut self, value: bool);

    fn input_parameters<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::Parameter>>;
    // fn input_parameters_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::Parameter>>;
    // fn set_input_parameters<E>(&mut self, value: Option<&Vec<E>>) where E: Into<Parameter>;

    fn output_parameters<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::Parameter>>;
    // fn output_parameters_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::Parameter>>;
    // fn set_output_parameters<E>(&mut self, value: Option<&Vec<E>>) where E: Into<Parameter>;

    fn attributes<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::FunctionAttribute>>;
    // fn attributes_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::FunctionAttribute>>;
    // fn set_attributes(&mut self, value: Option<&Vec<FunctionAttribute>>);

    fn generics<'a>(&'a self) -> Option<&'a crate::Generics>;
    // fn generics_mut(&mut self) -> &mut Option<&'a crate::Generics>;
    // fn set_generics<E>(&mut self, value: Option<E>) where E: Into<Generics>;


}

impl Function for crate::Function {
        fn name<'a>(&'a self) -> &'a str {
        return &self.name[..];
    }
        fn visibility<'a>(&'a self) -> Option<&'a crate::Visibility> {
        return self.visibility.as_ref();
    }
        fn implemented(&self) -> bool {
        return self.implemented;
    }
        fn input_parameters<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::Parameter>> {
        return self.input_parameters.as_ref();
    }
        fn output_parameters<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::Parameter>> {
        return self.output_parameters.as_ref();
    }
        fn attributes<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::FunctionAttribute>> {
        return self.attributes.as_ref();
    }
        fn generics<'a>(&'a self) -> Option<&'a crate::Generics> {
        return self.generics.as_ref();
    }
}


pub trait RecordType : Kind   {

    fn name<'a>(&'a self) -> &'a str;
    // fn name_mut(&mut self) -> &mut &'a str;
    // fn set_name(&mut self, value: String);

    fn visibility<'a>(&'a self) -> Option<&'a crate::Visibility>;
    // fn visibility_mut(&mut self) -> &mut Option<&'a crate::Visibility>;
    // fn set_visibility(&mut self, value: Option<&'a Visibility>);

    fn record_kind<'a>(&'a self) -> Option<&'a crate::RecordKind>;
    // fn record_kind_mut(&mut self) -> &mut Option<&'a crate::RecordKind>;
    // fn set_record_kind(&mut self, value: Option<&'a RecordKind>);

    fn fields<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::RecordField>>;
    // fn fields_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::RecordField>>;
    // fn set_fields<E>(&mut self, value: Option<&Vec<E>>) where E: Into<RecordField>;

    fn generics<'a>(&'a self) -> Option<&'a crate::Generics>;
    // fn generics_mut(&mut self) -> &mut Option<&'a crate::Generics>;
    // fn set_generics<E>(&mut self, value: Option<E>) where E: Into<Generics>;

    fn methods<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::FunctionDetail>>;
    // fn methods_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::FunctionDetail>>;
    // fn set_methods<E>(&mut self, value: Option<&Vec<E>>) where E: Into<FunctionDetail>;


}

impl RecordType for crate::RecordType {
        fn name<'a>(&'a self) -> &'a str {
        return &self.name[..];
    }
        fn visibility<'a>(&'a self) -> Option<&'a crate::Visibility> {
        return self.visibility.as_ref();
    }
        fn record_kind<'a>(&'a self) -> Option<&'a crate::RecordKind> {
        return self.record_kind.as_ref();
    }
        fn fields<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::RecordField>> {
        return self.fields.as_ref();
    }
        fn generics<'a>(&'a self) -> Option<&'a crate::Generics> {
        return self.generics.as_ref();
    }
        fn methods<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::FunctionDetail>> {
        return self.methods.as_ref();
    }
}


pub trait SumType : Kind   {

    fn variants<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::SumVariant>>;
    // fn variants_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::SumVariant>>;
    // fn set_variants<E>(&mut self, value: Option<&Vec<E>>) where E: Into<SumVariant>;


}

impl SumType for crate::SumType {
        fn variants<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::SumVariant>> {
        return self.variants.as_ref();
    }
}


pub trait UnionType : Kind   {

    fn types<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TypeExpression>>;
    // fn types_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::TypeExpression>>;
    // fn set_types<E>(&mut self, value: Option<&Vec<E>>) where E: Into<TypeExpression>;


}

impl UnionType for crate::UnionType {
        fn types<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TypeExpression>> {
        return self.types.as_ref();
    }
}


pub trait TraitDef : Kind   {

    fn name<'a>(&'a self) -> &'a str;
    // fn name_mut(&mut self) -> &mut &'a str;
    // fn set_name(&mut self, value: String);

    fn visibility<'a>(&'a self) -> Option<&'a crate::Visibility>;
    // fn visibility_mut(&mut self) -> &mut Option<&'a crate::Visibility>;
    // fn set_visibility(&mut self, value: Option<&'a Visibility>);

    fn generics<'a>(&'a self) -> Option<&'a crate::Generics>;
    // fn generics_mut(&mut self) -> &mut Option<&'a crate::Generics>;
    // fn set_generics<E>(&mut self, value: Option<E>) where E: Into<Generics>;

    fn super_traits<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TraitRef>>;
    // fn super_traits_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::TraitRef>>;
    // fn set_super_traits<E>(&mut self, value: Option<&Vec<E>>) where E: Into<TraitRef>;

    fn associated_types<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::AssociatedType>>;
    // fn associated_types_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::AssociatedType>>;
    // fn set_associated_types<E>(&mut self, value: Option<&Vec<E>>) where E: Into<AssociatedType>;

    fn required_methods<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TraitMethod>>;
    // fn required_methods_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::TraitMethod>>;
    // fn set_required_methods<E>(&mut self, value: Option<&Vec<E>>) where E: Into<TraitMethod>;

    fn provided_methods<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TraitMethod>>;
    // fn provided_methods_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::TraitMethod>>;
    // fn set_provided_methods<E>(&mut self, value: Option<&Vec<E>>) where E: Into<TraitMethod>;

    fn required_constants<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TraitConstant>>;
    // fn required_constants_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::TraitConstant>>;
    // fn set_required_constants<E>(&mut self, value: Option<&Vec<E>>) where E: Into<TraitConstant>;

    fn docs<'a>(&'a self) -> Option<&'a str>;
    // fn docs_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_docs(&mut self, value: Option<&'a str>);


}

impl TraitDef for crate::TraitDef {
        fn name<'a>(&'a self) -> &'a str {
        return &self.name[..];
    }
        fn visibility<'a>(&'a self) -> Option<&'a crate::Visibility> {
        return self.visibility.as_ref();
    }
        fn generics<'a>(&'a self) -> Option<&'a crate::Generics> {
        return self.generics.as_ref();
    }
        fn super_traits<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TraitRef>> {
        return self.super_traits.as_ref();
    }
        fn associated_types<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::AssociatedType>> {
        return self.associated_types.as_ref();
    }
        fn required_methods<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TraitMethod>> {
        return self.required_methods.as_ref();
    }
        fn provided_methods<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TraitMethod>> {
        return self.provided_methods.as_ref();
    }
        fn required_constants<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TraitConstant>> {
        return self.required_constants.as_ref();
    }
        fn docs<'a>(&'a self) -> Option<&'a str> {
        return self.docs.as_deref();
    }
}


pub trait TraitImpl : Kind   {

    fn trait_ref<'a>(&'a self) -> &'a crate::TraitRef;
    // fn trait_ref_mut(&mut self) -> &mut &'a crate::TraitRef;
    // fn set_trait_ref<E>(&mut self, value: E) where E: Into<TraitRef>;

    fn for_type<'a>(&'a self) -> &'a crate::TypeExpression;
    // fn for_type_mut(&mut self) -> &mut &'a crate::TypeExpression;
    // fn set_for_type<E>(&mut self, value: E) where E: Into<TypeExpression>;

    fn visibility<'a>(&'a self) -> Option<&'a crate::Visibility>;
    // fn visibility_mut(&mut self) -> &mut Option<&'a crate::Visibility>;
    // fn set_visibility(&mut self, value: Option<&'a Visibility>);

    fn generics<'a>(&'a self) -> Option<&'a crate::Generics>;
    // fn generics_mut(&mut self) -> &mut Option<&'a crate::Generics>;
    // fn set_generics<E>(&mut self, value: Option<E>) where E: Into<Generics>;

    fn methods<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::FunctionDetail>>;
    // fn methods_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::FunctionDetail>>;
    // fn set_methods<E>(&mut self, value: Option<&Vec<E>>) where E: Into<FunctionDetail>;

    fn associated_types<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::AssociatedType>>;
    // fn associated_types_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::AssociatedType>>;
    // fn set_associated_types<E>(&mut self, value: Option<&Vec<E>>) where E: Into<AssociatedType>;

    fn associated_constants<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TraitConstant>>;
    // fn associated_constants_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::TraitConstant>>;
    // fn set_associated_constants<E>(&mut self, value: Option<&Vec<E>>) where E: Into<TraitConstant>;

    fn is_negative(&self) -> bool;
    // fn is_negative_mut(&mut self) -> &mut bool;
    // fn set_is_negative(&mut self, value: bool);

    fn is_blanket(&self) -> bool;
    // fn is_blanket_mut(&mut self) -> &mut bool;
    // fn set_is_blanket(&mut self, value: bool);

    fn is_unsafe(&self) -> bool;
    // fn is_unsafe_mut(&mut self) -> &mut bool;
    // fn set_is_unsafe(&mut self, value: bool);

    fn docs<'a>(&'a self) -> Option<&'a str>;
    // fn docs_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_docs(&mut self, value: Option<&'a str>);


}

impl TraitImpl for crate::TraitImpl {
        fn trait_ref<'a>(&'a self) -> &'a crate::TraitRef {
        return &self.trait_ref;
    }
        fn for_type<'a>(&'a self) -> &'a crate::TypeExpression {
        return &self.for_type;
    }
        fn visibility<'a>(&'a self) -> Option<&'a crate::Visibility> {
        return self.visibility.as_ref();
    }
        fn generics<'a>(&'a self) -> Option<&'a crate::Generics> {
        return self.generics.as_ref();
    }
        fn methods<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::FunctionDetail>> {
        return self.methods.as_ref();
    }
        fn associated_types<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::AssociatedType>> {
        return self.associated_types.as_ref();
    }
        fn associated_constants<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TraitConstant>> {
        return self.associated_constants.as_ref();
    }
        fn is_negative(&self) -> bool {
        return self.is_negative;
    }
        fn is_blanket(&self) -> bool {
        return self.is_blanket;
    }
        fn is_unsafe(&self) -> bool {
        return self.is_unsafe;
    }
        fn docs<'a>(&'a self) -> Option<&'a str> {
        return self.docs.as_deref();
    }
}


pub trait TypeAlias : Kind   {

    fn aliased_type<'a>(&'a self) -> &'a crate::TypeExpression;
    // fn aliased_type_mut(&mut self) -> &mut &'a crate::TypeExpression;
    // fn set_aliased_type<E>(&mut self, value: E) where E: Into<TypeExpression>;


}

impl TypeAlias for crate::TypeAlias {
        fn aliased_type<'a>(&'a self) -> &'a crate::TypeExpression {
        return &self.aliased_type;
    }
}


pub trait Parameter   {

    fn name<'a>(&'a self) -> &'a str;
    // fn name_mut(&mut self) -> &mut &'a str;
    // fn set_name(&mut self, value: String);

    fn ty<'a>(&'a self) -> Option<&'a crate::TypeExpression>;
    // fn ty_mut(&mut self) -> &mut Option<&'a crate::TypeExpression>;
    // fn set_ty<E>(&mut self, value: Option<E>) where E: Into<TypeExpression>;

    fn attributes<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::ParameterAttribute>>;
    // fn attributes_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::ParameterAttribute>>;
    // fn set_attributes(&mut self, value: Option<&Vec<ParameterAttribute>>);

    fn default_value<'a>(&'a self) -> Option<&'a str>;
    // fn default_value_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_default_value(&mut self, value: Option<&'a str>);

    fn description<'a>(&'a self) -> Option<&'a str>;
    // fn description_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_description(&mut self, value: Option<&'a str>);


}

impl Parameter for crate::Parameter {
        fn name<'a>(&'a self) -> &'a str {
        return &self.name[..];
    }
        fn ty<'a>(&'a self) -> Option<&'a crate::TypeExpression> {
        return self.ty.as_deref();
    }
        fn attributes<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::ParameterAttribute>> {
        return self.attributes.as_ref();
    }
        fn default_value<'a>(&'a self) -> Option<&'a str> {
        return self.default_value.as_deref();
    }
        fn description<'a>(&'a self) -> Option<&'a str> {
        return self.description.as_deref();
    }
}


pub trait RecordField   {

    fn name<'a>(&'a self) -> Option<&'a str>;
    // fn name_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_name(&mut self, value: Option<&'a str>);

    fn ty<'a>(&'a self) -> Option<&'a crate::TypeExpression>;
    // fn ty_mut(&mut self) -> &mut Option<&'a crate::TypeExpression>;
    // fn set_ty<E>(&mut self, value: Option<E>) where E: Into<TypeExpression>;

    fn default_value<'a>(&'a self) -> Option<&'a str>;
    // fn default_value_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_default_value(&mut self, value: Option<&'a str>);

    fn attributes<'a>(&'a self) -> Option<&'a crate::FieldAttribute>;
    // fn attributes_mut(&mut self) -> &mut Option<&'a crate::FieldAttribute>;
    // fn set_attributes(&mut self, value: Option<&'a FieldAttribute>);

    fn visibility<'a>(&'a self) -> Option<&'a crate::Visibility>;
    // fn visibility_mut(&mut self) -> &mut Option<&'a crate::Visibility>;
    // fn set_visibility(&mut self, value: Option<&'a Visibility>);

    fn type_entry_id(&self) -> Option<isize>;
    // fn type_entry_id_mut(&mut self) -> &mut Option<isize>;
    // fn set_type_entry_id(&mut self, value: Option<isize>);


}

impl RecordField for crate::RecordField {
        fn name<'a>(&'a self) -> Option<&'a str> {
        return self.name.as_deref();
    }
        fn ty<'a>(&'a self) -> Option<&'a crate::TypeExpression> {
        return self.ty.as_ref();
    }
        fn default_value<'a>(&'a self) -> Option<&'a str> {
        return self.default_value.as_deref();
    }
        fn attributes<'a>(&'a self) -> Option<&'a crate::FieldAttribute> {
        return self.attributes.as_ref();
    }
        fn visibility<'a>(&'a self) -> Option<&'a crate::Visibility> {
        return self.visibility.as_ref();
    }
        fn type_entry_id(&self) -> Option<isize> {
        return self.type_entry_id;
    }
}


pub trait SumVariant   {

    fn name<'a>(&'a self) -> &'a str;
    // fn name_mut(&mut self) -> &mut &'a str;
    // fn set_name(&mut self, value: String);

    fn types<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TypeExpression>>;
    // fn types_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::TypeExpression>>;
    // fn set_types<E>(&mut self, value: Option<&Vec<E>>) where E: Into<TypeExpression>;


}

impl SumVariant for crate::SumVariant {
        fn name<'a>(&'a self) -> &'a str {
        return &self.name[..];
    }
        fn types<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TypeExpression>> {
        return self.types.as_ref().map(|x| poly_containers::ListView::new(x));
    }
}


pub trait TypeExpression   {

    fn type_tag<'a>(&'a self) -> &'a str;
    // fn type_tag_mut(&mut self) -> &mut &'a str;
    // fn set_type_tag(&mut self, value: String);

    fn path<'a>(&'a self) -> Option<&'a str>;
    // fn path_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_path(&mut self, value: Option<&'a str>);

    fn generic_args<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::GenericArg>>;
    // fn generic_args_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::GenericArg>>;
    // fn set_generic_args<E>(&mut self, value: Option<&Vec<E>>) where E: Into<GenericArg>;

    fn param_name<'a>(&'a self) -> Option<&'a str>;
    // fn param_name_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_param_name(&mut self, value: Option<&'a str>);

    fn primitive<'a>(&'a self) -> Option<&'a crate::PrimitiveKind>;
    // fn primitive_mut(&mut self) -> &mut Option<&'a crate::PrimitiveKind>;
    // fn set_primitive(&mut self, value: Option<&'a PrimitiveKind>);

    fn inner_type<'a>(&'a self) -> Option<&'a crate::TypeExpression>;
    // fn inner_type_mut(&mut self) -> &mut Option<&'a crate::TypeExpression>;
    // fn set_inner_type<E>(&mut self, value: Option<E>) where E: Into<TypeExpression>;

    fn element_types<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TypeExpression>>;
    // fn element_types_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::TypeExpression>>;
    // fn set_element_types<E>(&mut self, value: Option<&Vec<E>>) where E: Into<TypeExpression>;

    fn array_length(&self) -> Option<isize>;
    // fn array_length_mut(&mut self) -> &mut Option<isize>;
    // fn set_array_length(&mut self, value: Option<isize>);

    fn is_mutable(&self) -> Option<bool>;
    // fn is_mutable_mut(&mut self) -> &mut Option<bool>;
    // fn set_is_mutable(&mut self, value: Option<bool>);

    fn lifetime<'a>(&'a self) -> Option<&'a str>;
    // fn lifetime_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_lifetime(&mut self, value: Option<&'a str>);

    fn fn_inputs<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::Parameter>>;
    // fn fn_inputs_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::Parameter>>;
    // fn set_fn_inputs<E>(&mut self, value: Option<&Vec<E>>) where E: Into<Parameter>;

    fn fn_outputs<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::Parameter>>;
    // fn fn_outputs_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::Parameter>>;
    // fn set_fn_outputs<E>(&mut self, value: Option<&Vec<E>>) where E: Into<Parameter>;

    fn fn_generic_params<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TypeParam>>;
    // fn fn_generic_params_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::TypeParam>>;
    // fn set_fn_generic_params<E>(&mut self, value: Option<&Vec<E>>) where E: Into<TypeParam>;

    fn fn_attributes<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::FunctionAttribute>>;
    // fn fn_attributes_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::FunctionAttribute>>;
    // fn set_fn_attributes(&mut self, value: Option<&Vec<FunctionAttribute>>);

    fn dyn_traits<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::PolyTrait>>;
    // fn dyn_traits_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::PolyTrait>>;
    // fn set_dyn_traits<E>(&mut self, value: Option<&Vec<E>>) where E: Into<PolyTrait>;

    fn sum_variants<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::SumVariant>>;
    // fn sum_variants_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::SumVariant>>;
    // fn set_sum_variants<E>(&mut self, value: Option<&Vec<E>>) where E: Into<SumVariant>;

    fn generic_bounds<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::GenericBound>>;
    // fn generic_bounds_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::GenericBound>>;
    // fn set_generic_bounds<E>(&mut self, value: Option<&Vec<E>>) where E: Into<GenericBound>;

    fn qualified_name<'a>(&'a self) -> Option<&'a str>;
    // fn qualified_name_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_qualified_name(&mut self, value: Option<&'a str>);

    fn self_type<'a>(&'a self) -> Option<&'a crate::TypeExpression>;
    // fn self_type_mut(&mut self) -> &mut Option<&'a crate::TypeExpression>;
    // fn set_self_type<E>(&mut self, value: Option<E>) where E: Into<TypeExpression>;

    fn trait_path<'a>(&'a self) -> Option<&'a str>;
    // fn trait_path_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_trait_path(&mut self, value: Option<&'a str>);


}

impl TypeExpression for crate::TypeExpression {
        fn type_tag<'a>(&'a self) -> &'a str {
        return &self.type_tag[..];
    }
        fn path<'a>(&'a self) -> Option<&'a str> {
        return self.path.as_deref();
    }
        fn generic_args<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::GenericArg>> {
        return self.generic_args.as_ref().map(|x| poly_containers::ListView::new(x));
    }
        fn param_name<'a>(&'a self) -> Option<&'a str> {
        return self.param_name.as_deref();
    }
        fn primitive<'a>(&'a self) -> Option<&'a crate::PrimitiveKind> {
        return self.primitive.as_ref();
    }
        fn inner_type<'a>(&'a self) -> Option<&'a crate::TypeExpression> {
        return self.inner_type.as_deref();
    }
        fn element_types<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TypeExpression>> {
        return self.element_types.as_ref().map(|x| poly_containers::ListView::new(x));
    }
        fn array_length(&self) -> Option<isize> {
        return self.array_length;
    }
        fn is_mutable(&self) -> Option<bool> {
        return self.is_mutable;
    }
        fn lifetime<'a>(&'a self) -> Option<&'a str> {
        return self.lifetime.as_deref();
    }
        fn fn_inputs<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::Parameter>> {
        return self.fn_inputs.as_ref().map(|x| poly_containers::ListView::new(x));
    }
        fn fn_outputs<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::Parameter>> {
        return self.fn_outputs.as_ref().map(|x| poly_containers::ListView::new(x));
    }
        fn fn_generic_params<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TypeParam>> {
        return self.fn_generic_params.as_ref();
    }
        fn fn_attributes<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::FunctionAttribute>> {
        return self.fn_attributes.as_ref();
    }
        fn dyn_traits<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::PolyTrait>> {
        return self.dyn_traits.as_ref();
    }
        fn sum_variants<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::SumVariant>> {
        return self.sum_variants.as_ref().map(|x| poly_containers::ListView::new(x));
    }
        fn generic_bounds<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::GenericBound>> {
        return self.generic_bounds.as_ref();
    }
        fn qualified_name<'a>(&'a self) -> Option<&'a str> {
        return self.qualified_name.as_deref();
    }
        fn self_type<'a>(&'a self) -> Option<&'a crate::TypeExpression> {
        return self.self_type.as_deref();
    }
        fn trait_path<'a>(&'a self) -> Option<&'a str> {
        return self.trait_path.as_deref();
    }
}


pub trait GenericArg   {

    fn arg_kind<'a>(&'a self) -> &'a str;
    // fn arg_kind_mut(&mut self) -> &mut &'a str;
    // fn set_arg_kind(&mut self, value: String);

    fn type_value<'a>(&'a self) -> Option<&'a crate::TypeExpression>;
    // fn type_value_mut(&mut self) -> &mut Option<&'a crate::TypeExpression>;
    // fn set_type_value<E>(&mut self, value: Option<E>) where E: Into<TypeExpression>;

    fn const_expr<'a>(&'a self) -> Option<&'a str>;
    // fn const_expr_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_const_expr(&mut self, value: Option<&'a str>);

    fn lifetime<'a>(&'a self) -> Option<&'a str>;
    // fn lifetime_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_lifetime(&mut self, value: Option<&'a str>);


}

impl GenericArg for crate::GenericArg {
        fn arg_kind<'a>(&'a self) -> &'a str {
        return &self.arg_kind[..];
    }
        fn type_value<'a>(&'a self) -> Option<&'a crate::TypeExpression> {
        return self.type_value.as_deref();
    }
        fn const_expr<'a>(&'a self) -> Option<&'a str> {
        return self.const_expr.as_deref();
    }
        fn lifetime<'a>(&'a self) -> Option<&'a str> {
        return self.lifetime.as_deref();
    }
}


pub trait Generics   {

    fn type_params<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TypeParam>>;
    // fn type_params_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::TypeParam>>;
    // fn set_type_params<E>(&mut self, value: Option<&Vec<E>>) where E: Into<TypeParam>;

    fn const_params<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::ConstParam>>;
    // fn const_params_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::ConstParam>>;
    // fn set_const_params<E>(&mut self, value: Option<&Vec<E>>) where E: Into<ConstParam>;

    fn lifetime_params<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::LifetimeParam>>;
    // fn lifetime_params_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::LifetimeParam>>;
    // fn set_lifetime_params<E>(&mut self, value: Option<&Vec<E>>) where E: Into<LifetimeParam>;

    fn constraints<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::ConstraintExpr>>;
    // fn constraints_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::ConstraintExpr>>;
    // fn set_constraints<E>(&mut self, value: Option<&Vec<E>>) where E: Into<ConstraintExpr>;


}

impl Generics for crate::Generics {
        fn type_params<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TypeParam>> {
        return self.type_params.as_ref();
    }
        fn const_params<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::ConstParam>> {
        return self.const_params.as_ref();
    }
        fn lifetime_params<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::LifetimeParam>> {
        return self.lifetime_params.as_ref();
    }
        fn constraints<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::ConstraintExpr>> {
        return self.constraints.as_ref();
    }
}


pub trait TypeParam   {

    fn name<'a>(&'a self) -> &'a str;
    // fn name_mut(&mut self) -> &mut &'a str;
    // fn set_name(&mut self, value: String);

    fn kind<'a>(&'a self) -> Option<&'a crate::TypeKind>;
    // fn kind_mut(&mut self) -> &mut Option<&'a crate::TypeKind>;
    // fn set_kind(&mut self, value: Option<&'a TypeKind>);

    fn variance<'a>(&'a self) -> Option<&'a crate::Variance>;
    // fn variance_mut(&mut self) -> &mut Option<&'a crate::Variance>;
    // fn set_variance(&mut self, value: Option<&'a Variance>);

    fn default_type<'a>(&'a self) -> Option<&'a crate::TypeExprSimple>;
    // fn default_type_mut(&mut self) -> &mut Option<&'a crate::TypeExprSimple>;
    // fn set_default_type<E>(&mut self, value: Option<E>) where E: Into<TypeExprSimple>;


}

impl TypeParam for crate::TypeParam {
        fn name<'a>(&'a self) -> &'a str {
        return &self.name[..];
    }
        fn kind<'a>(&'a self) -> Option<&'a crate::TypeKind> {
        return self.kind.as_ref();
    }
        fn variance<'a>(&'a self) -> Option<&'a crate::Variance> {
        return self.variance.as_ref();
    }
        fn default_type<'a>(&'a self) -> Option<&'a crate::TypeExprSimple> {
        return self.default_type.as_ref();
    }
}


pub trait ConstParam   {

    fn name<'a>(&'a self) -> &'a str;
    // fn name_mut(&mut self) -> &mut &'a str;
    // fn set_name(&mut self, value: String);

    fn ty<'a>(&'a self) -> Option<&'a crate::TypeExprSimple>;
    // fn ty_mut(&mut self) -> &mut Option<&'a crate::TypeExprSimple>;
    // fn set_ty<E>(&mut self, value: Option<E>) where E: Into<TypeExprSimple>;

    fn default_value<'a>(&'a self) -> Option<&'a str>;
    // fn default_value_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_default_value(&mut self, value: Option<&'a str>);


}

impl ConstParam for crate::ConstParam {
        fn name<'a>(&'a self) -> &'a str {
        return &self.name[..];
    }
        fn ty<'a>(&'a self) -> Option<&'a crate::TypeExprSimple> {
        return self.ty.as_ref();
    }
        fn default_value<'a>(&'a self) -> Option<&'a str> {
        return self.default_value.as_deref();
    }
}


pub trait LifetimeParam   {

    fn name<'a>(&'a self) -> &'a str;
    // fn name_mut(&mut self) -> &mut &'a str;
    // fn set_name(&mut self, value: String);

    fn variance<'a>(&'a self) -> Option<&'a crate::Variance>;
    // fn variance_mut(&mut self) -> &mut Option<&'a crate::Variance>;
    // fn set_variance(&mut self, value: Option<&'a Variance>);


}

impl LifetimeParam for crate::LifetimeParam {
        fn name<'a>(&'a self) -> &'a str {
        return &self.name[..];
    }
        fn variance<'a>(&'a self) -> Option<&'a crate::Variance> {
        return self.variance.as_ref();
    }
}


pub trait ConstraintExpr   {

    fn constraint_kind<'a>(&'a self) -> &'a str;
    // fn constraint_kind_mut(&mut self) -> &mut &'a str;
    // fn set_constraint_kind(&mut self, value: String);

    fn param<'a>(&'a self) -> Option<&'a str>;
    // fn param_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_param(&mut self, value: Option<&'a str>);

    fn trait_ref<'a>(&'a self) -> Option<&'a crate::TraitRef>;
    // fn trait_ref_mut(&mut self) -> &mut Option<&'a crate::TraitRef>;
    // fn set_trait_ref<E>(&mut self, value: Option<E>) where E: Into<TraitRef>;

    fn assoc_name<'a>(&'a self) -> Option<&'a str>;
    // fn assoc_name_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_assoc_name(&mut self, value: Option<&'a str>);

    fn bound<'a>(&'a self) -> Option<&'a crate::TypeExprSimple>;
    // fn bound_mut(&mut self) -> &mut Option<&'a crate::TypeExprSimple>;
    // fn set_bound<E>(&mut self, value: Option<E>) where E: Into<TypeExprSimple>;

    fn kind_signature<'a>(&'a self) -> Option<&'a str>;
    // fn kind_signature_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_kind_signature(&mut self, value: Option<&'a str>);

    fn shorter<'a>(&'a self) -> Option<&'a str>;
    // fn shorter_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_shorter(&mut self, value: Option<&'a str>);

    fn longer<'a>(&'a self) -> Option<&'a str>;
    // fn longer_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_longer(&mut self, value: Option<&'a str>);

    fn const_expr<'a>(&'a self) -> Option<&'a str>;
    // fn const_expr_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_const_expr(&mut self, value: Option<&'a str>);

    fn predicate_expr<'a>(&'a self) -> Option<&'a str>;
    // fn predicate_expr_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_predicate_expr(&mut self, value: Option<&'a str>);


}

impl ConstraintExpr for crate::ConstraintExpr {
        fn constraint_kind<'a>(&'a self) -> &'a str {
        return &self.constraint_kind[..];
    }
        fn param<'a>(&'a self) -> Option<&'a str> {
        return self.param.as_deref();
    }
        fn trait_ref<'a>(&'a self) -> Option<&'a crate::TraitRef> {
        return self.trait_ref.as_ref();
    }
        fn assoc_name<'a>(&'a self) -> Option<&'a str> {
        return self.assoc_name.as_deref();
    }
        fn bound<'a>(&'a self) -> Option<&'a crate::TypeExprSimple> {
        return self.bound.as_ref();
    }
        fn kind_signature<'a>(&'a self) -> Option<&'a str> {
        return self.kind_signature.as_deref();
    }
        fn shorter<'a>(&'a self) -> Option<&'a str> {
        return self.shorter.as_deref();
    }
        fn longer<'a>(&'a self) -> Option<&'a str> {
        return self.longer.as_deref();
    }
        fn const_expr<'a>(&'a self) -> Option<&'a str> {
        return self.const_expr.as_deref();
    }
        fn predicate_expr<'a>(&'a self) -> Option<&'a str> {
        return self.predicate_expr.as_deref();
    }
}


pub trait TypeExprSimple   {

    fn name<'a>(&'a self) -> &'a str;
    // fn name_mut(&mut self) -> &mut &'a str;
    // fn set_name(&mut self, value: String);

    fn args<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TypeExprSimple>>;
    // fn args_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::TypeExprSimple>>;
    // fn set_args<E>(&mut self, value: Option<&Vec<E>>) where E: Into<TypeExprSimple>;


}

impl TypeExprSimple for crate::TypeExprSimple {
        fn name<'a>(&'a self) -> &'a str {
        return &self.name[..];
    }
        fn args<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TypeExprSimple>> {
        return self.args.as_ref().map(|x| poly_containers::ListView::new(x));
    }
}


pub trait TraitRef   {

    fn name<'a>(&'a self) -> &'a str;
    // fn name_mut(&mut self) -> &mut &'a str;
    // fn set_name(&mut self, value: String);

    fn args<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TypeExprSimple>>;
    // fn args_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::TypeExprSimple>>;
    // fn set_args<E>(&mut self, value: Option<&Vec<E>>) where E: Into<TypeExprSimple>;


}

impl TraitRef for crate::TraitRef {
        fn name<'a>(&'a self) -> &'a str {
        return &self.name[..];
    }
        fn args<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::TypeExprSimple>> {
        return self.args.as_ref();
    }
}


pub trait AssociatedType   {

    fn name<'a>(&'a self) -> &'a str;
    // fn name_mut(&mut self) -> &mut &'a str;
    // fn set_name(&mut self, value: String);

    fn bounds<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::GenericBound>>;
    // fn bounds_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::GenericBound>>;
    // fn set_bounds<E>(&mut self, value: Option<&Vec<E>>) where E: Into<GenericBound>;

    fn default_type<'a>(&'a self) -> Option<&'a crate::TypeExpression>;
    // fn default_type_mut(&mut self) -> &mut Option<&'a crate::TypeExpression>;
    // fn set_default_type<E>(&mut self, value: Option<E>) where E: Into<TypeExpression>;

    fn docs<'a>(&'a self) -> Option<&'a str>;
    // fn docs_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_docs(&mut self, value: Option<&'a str>);


}

impl AssociatedType for crate::AssociatedType {
        fn name<'a>(&'a self) -> &'a str {
        return &self.name[..];
    }
        fn bounds<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::GenericBound>> {
        return self.bounds.as_ref();
    }
        fn default_type<'a>(&'a self) -> Option<&'a crate::TypeExpression> {
        return self.default_type.as_ref();
    }
        fn docs<'a>(&'a self) -> Option<&'a str> {
        return self.docs.as_deref();
    }
}


pub trait GenericBound   {

    fn bound_kind<'a>(&'a self) -> &'a str;
    // fn bound_kind_mut(&mut self) -> &mut &'a str;
    // fn set_bound_kind(&mut self, value: String);

    fn trait_ref<'a>(&'a self) -> Option<&'a crate::TraitRef>;
    // fn trait_ref_mut(&mut self) -> &mut Option<&'a crate::TraitRef>;
    // fn set_trait_ref<E>(&mut self, value: Option<E>) where E: Into<TraitRef>;

    fn lifetime<'a>(&'a self) -> Option<&'a str>;
    // fn lifetime_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_lifetime(&mut self, value: Option<&'a str>);


}

impl GenericBound for crate::GenericBound {
        fn bound_kind<'a>(&'a self) -> &'a str {
        return &self.bound_kind[..];
    }
        fn trait_ref<'a>(&'a self) -> Option<&'a crate::TraitRef> {
        return self.trait_ref.as_ref();
    }
        fn lifetime<'a>(&'a self) -> Option<&'a str> {
        return self.lifetime.as_deref();
    }
}


pub trait TraitMethod   {

    fn name<'a>(&'a self) -> &'a str;
    // fn name_mut(&mut self) -> &mut &'a str;
    // fn set_name(&mut self, value: String);

    fn parameters<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::Parameter>>;
    // fn parameters_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::Parameter>>;
    // fn set_parameters<E>(&mut self, value: Option<&Vec<E>>) where E: Into<Parameter>;

    fn return_type<'a>(&'a self) -> Option<&'a crate::TypeExpression>;
    // fn return_type_mut(&mut self) -> &mut Option<&'a crate::TypeExpression>;
    // fn set_return_type<E>(&mut self, value: Option<E>) where E: Into<TypeExpression>;

    fn generics<'a>(&'a self) -> Option<&'a crate::Generics>;
    // fn generics_mut(&mut self) -> &mut Option<&'a crate::Generics>;
    // fn set_generics<E>(&mut self, value: Option<E>) where E: Into<Generics>;

    fn attributes<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::FunctionAttribute>>;
    // fn attributes_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::FunctionAttribute>>;
    // fn set_attributes(&mut self, value: Option<&Vec<FunctionAttribute>>);

    fn receiver<'a>(&'a self) -> Option<&'a crate::ReceiverKind>;
    // fn receiver_mut(&mut self) -> &mut Option<&'a crate::ReceiverKind>;
    // fn set_receiver(&mut self, value: Option<&'a ReceiverKind>);

    fn has_default_implementation(&self) -> bool;
    // fn has_default_implementation_mut(&mut self) -> &mut bool;
    // fn set_has_default_implementation(&mut self, value: bool);

    fn docs<'a>(&'a self) -> Option<&'a str>;
    // fn docs_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_docs(&mut self, value: Option<&'a str>);


}

impl TraitMethod for crate::TraitMethod {
        fn name<'a>(&'a self) -> &'a str {
        return &self.name[..];
    }
        fn parameters<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::Parameter>> {
        return self.parameters.as_ref();
    }
        fn return_type<'a>(&'a self) -> Option<&'a crate::TypeExpression> {
        return self.return_type.as_ref();
    }
        fn generics<'a>(&'a self) -> Option<&'a crate::Generics> {
        return self.generics.as_ref();
    }
        fn attributes<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::FunctionAttribute>> {
        return self.attributes.as_ref();
    }
        fn receiver<'a>(&'a self) -> Option<&'a crate::ReceiverKind> {
        return self.receiver.as_ref();
    }
        fn has_default_implementation(&self) -> bool {
        return self.has_default_implementation;
    }
        fn docs<'a>(&'a self) -> Option<&'a str> {
        return self.docs.as_deref();
    }
}


pub trait TraitConstant   {

    fn name<'a>(&'a self) -> &'a str;
    // fn name_mut(&mut self) -> &mut &'a str;
    // fn set_name(&mut self, value: String);

    fn ty<'a>(&'a self) -> &'a crate::TypeExpression;
    // fn ty_mut(&mut self) -> &mut &'a crate::TypeExpression;
    // fn set_ty<E>(&mut self, value: E) where E: Into<TypeExpression>;

    fn default_value<'a>(&'a self) -> Option<&'a str>;
    // fn default_value_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_default_value(&mut self, value: Option<&'a str>);

    fn docs<'a>(&'a self) -> Option<&'a str>;
    // fn docs_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_docs(&mut self, value: Option<&'a str>);


}

impl TraitConstant for crate::TraitConstant {
        fn name<'a>(&'a self) -> &'a str {
        return &self.name[..];
    }
        fn ty<'a>(&'a self) -> &'a crate::TypeExpression {
        return &self.ty;
    }
        fn default_value<'a>(&'a self) -> Option<&'a str> {
        return self.default_value.as_deref();
    }
        fn docs<'a>(&'a self) -> Option<&'a str> {
        return self.docs.as_deref();
    }
}


pub trait TraitAttributeValue   {

    fn kind<'a>(&'a self) -> &'a crate::TraitAttributeKind;
    // fn kind_mut(&mut self) -> &mut &'a crate::TraitAttributeKind;
    // fn set_kind(&mut self, value: TraitAttributeKind);

    fn custom_name<'a>(&'a self) -> Option<&'a str>;
    // fn custom_name_mut(&mut self) -> &mut Option<&'a str>;
    // fn set_custom_name(&mut self, value: Option<&'a str>);

    fn custom_args<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, String>>;
    // fn custom_args_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, String>>;
    // fn set_custom_args(&mut self, value: Option<&Vec<String>>);


}

impl TraitAttributeValue for crate::TraitAttributeValue {
        fn kind<'a>(&'a self) -> &'a crate::TraitAttributeKind {
        return &self.kind;
    }
        fn custom_name<'a>(&'a self) -> Option<&'a str> {
        return self.custom_name.as_deref();
    }
        fn custom_args<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, String>> {
        return self.custom_args.as_ref();
    }
}


pub trait FunctionDetail   {

    fn name<'a>(&'a self) -> &'a str;
    // fn name_mut(&mut self) -> &mut &'a str;
    // fn set_name(&mut self, value: String);

    fn visibility<'a>(&'a self) -> Option<&'a crate::Visibility>;
    // fn visibility_mut(&mut self) -> &mut Option<&'a crate::Visibility>;
    // fn set_visibility(&mut self, value: Option<&'a Visibility>);

    fn implemented(&self) -> bool;
    // fn implemented_mut(&mut self) -> &mut bool;
    // fn set_implemented(&mut self, value: bool);

    fn input_parameters<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::Parameter>>;
    // fn input_parameters_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::Parameter>>;
    // fn set_input_parameters<E>(&mut self, value: Option<&Vec<E>>) where E: Into<Parameter>;

    fn output_parameters<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::Parameter>>;
    // fn output_parameters_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::Parameter>>;
    // fn set_output_parameters<E>(&mut self, value: Option<&Vec<E>>) where E: Into<Parameter>;

    fn attributes<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::FunctionAttribute>>;
    // fn attributes_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, crate::FunctionAttribute>>;
    // fn set_attributes(&mut self, value: Option<&Vec<FunctionAttribute>>);

    fn generics<'a>(&'a self) -> Option<&'a crate::Generics>;
    // fn generics_mut(&mut self) -> &mut Option<&'a crate::Generics>;
    // fn set_generics<E>(&mut self, value: Option<E>) where E: Into<Generics>;


}

impl FunctionDetail for crate::FunctionDetail {
        fn name<'a>(&'a self) -> &'a str {
        return &self.name[..];
    }
        fn visibility<'a>(&'a self) -> Option<&'a crate::Visibility> {
        return self.visibility.as_ref();
    }
        fn implemented(&self) -> bool {
        return self.implemented;
    }
        fn input_parameters<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::Parameter>> {
        return self.input_parameters.as_ref();
    }
        fn output_parameters<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::Parameter>> {
        return self.output_parameters.as_ref();
    }
        fn attributes<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, crate::FunctionAttribute>> {
        return self.attributes.as_ref();
    }
        fn generics<'a>(&'a self) -> Option<&'a crate::Generics> {
        return self.generics.as_ref();
    }
}


pub trait AssociatedTypeImpl   {

    fn name<'a>(&'a self) -> &'a str;
    // fn name_mut(&mut self) -> &mut &'a str;
    // fn set_name(&mut self, value: String);

    fn ty<'a>(&'a self) -> &'a crate::TypeExpression;
    // fn ty_mut(&mut self) -> &mut &'a crate::TypeExpression;
    // fn set_ty<E>(&mut self, value: E) where E: Into<TypeExpression>;


}

impl AssociatedTypeImpl for crate::AssociatedTypeImpl {
        fn name<'a>(&'a self) -> &'a str {
        return &self.name[..];
    }
        fn ty<'a>(&'a self) -> &'a crate::TypeExpression {
        return &self.ty;
    }
}


pub trait PolyTrait   {

    fn tr<'a>(&'a self) -> &'a crate::TraitRef;
    // fn tr_mut(&mut self) -> &mut &'a crate::TraitRef;
    // fn set_tr<E>(&mut self, value: E) where E: Into<TraitRef>;

    fn lifetimes<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, String>>;
    // fn lifetimes_mut(&mut self) -> &mut Option<impl poly_containers::SeqRef<'a, String>>;
    // fn set_lifetimes(&mut self, value: Option<&Vec<String>>);


}

impl PolyTrait for crate::PolyTrait {
        fn tr<'a>(&'a self) -> &'a crate::TraitRef {
        return &self.tr;
    }
        fn lifetimes<'a>(&'a self) -> Option<impl poly_containers::SeqRef<'a, String>> {
        return self.lifetimes.as_ref();
    }
}
