# WIRE V2 CONTRACT (frozen for parallel implementation)

The exact `nudox-ir` wire-v2 / intro-v2 API surface that P1c/P2/P3/P4/P5 code
against. This is the single source of truth; if the as-built `nudox-ir` drifts
from this, `nudox-ir` is corrected to match this file (not the reverse).

All `*Wire` types derive `Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug`.
Owned strings are `String`. Reuse existing `IntroId`/`StableRef`/`ContentBlake3`
from `nudox_change`.

## Discriminants (`nudox_ir::kind::KindDiscriminant`, `#[repr(u16)]`, frozen)
Module=1 Record=2 Field=3 Function=4 Type=5 Trait=6 Impl=7 Enum=8 Variant=9 Const=10 Static=11 Reexport=12

## Existing (unchanged)
- `TypeRefWire::{ Same(IntroId), Foreign(StableRef) }`
- `TypeWire::{ SelfType, Primitive(PrimitiveWire), Tuple(Box<[TypeRefWire]>), Slice(Box<TypeRefWire>), Array{ty:Box<TypeRefWire>,length:u64}, Union(Box<[TypeRefWire]>), Intersection(Box<[TypeRefWire]>), Never, Any }`
- `PrimitiveWire::{ Integer{signed:bool,width:WidthWire}, Float(WidthWire), Bool, Char, Str, MutPointer(Box<TypeRefWire>), ConstPointer(Box<TypeRefWire>), Reference{lifetime:Option<String>,mutable:bool,ty:Box<TypeRefWire>}, Builtin(String) }`
- `WidthWire::{ Arch, Fixed(u32) }`
- `ParamWire{ name:Option<String>, ty:TypeRefWire }`
- `Visibility::{ Public=0, Private=1, Protected=2, Internal=3, Package=4, Crate=5 }` (`nudox_ir::symbol`)
- `DeprecationWire{ note:Option<String>, since:Option<String> }`
- `DocLinkWire{ target:StableRef, label:Option<String> }`

## New shared vocab (`nudox_ir::wire`)
```rust
pub enum SelfKind { None, Value, Ref, RefMut, Arbitrary(TypeRefWire) }
pub struct FnSigFlags { pub self_kind: SelfKind, pub is_async: bool, pub is_const: bool, pub is_unsafe: bool, pub abi: Option<String>, pub variadic: bool, pub defaulted: bool } // Default: None/false*/None
pub enum GenericParamWire {
    Lifetime { name: String },
    Type { name: String, bounds: Box<[TypeRefWire]>, default: Option<TypeWire> },
    Const { name: String, ty: TypeRefWire, default: Option<String> },
}
pub struct WherePredWire { pub target: TypeWire, pub bounds: Box<[TypeRefWire]> }
pub enum TriState { Yes, No, Unknown }
pub enum Sealed { None, PubApi, Full }
pub struct TraitFlags { pub is_auto: bool, pub is_unsafe: bool, pub dyn_compat: TriState, pub sealed: Sealed } // Default false/false/Unknown/None
pub struct ImplFlags { pub negative: bool, pub blanket: bool } // Default
pub enum RecordForm { Struct, Tuple, Unit, Union }
pub enum VariantForm { Unit, Tuple, Struct }
pub enum AutoTrait { Send, Sync, Unpin, UnwindSafe, RefUnwindSafe }
pub enum AutoState { Yes, No, Cond }
pub struct AutoFact { pub trait_: AutoTrait, pub state: AutoState }
pub struct AttrTok { pub token: String, pub arg: Option<String> } // ecosystem-scoped opaque token + optional arg (§8.5)
pub enum CfgExpr { All(Box<[CfgExpr]>), Any(Box<[CfgExpr]>), Not(Box<CfgExpr>), Feature(String), TargetOs(String), TargetArch(String), Other(String) }
```

## Kind bodies (`nudox_ir::wire`)
```rust
pub struct ModuleWire {} // unchanged
pub struct RecordWire { pub form: RecordForm, pub fields: Box<[IntroId]>, pub generics: Box<[GenericParamWire]>, pub wheres: Box<[WherePredWire]>, pub auto: Box<[AutoFact]> }
pub struct FieldWire { pub ty: Option<TypeRefWire> } // unchanged
pub struct FunctionWire { pub input_params: Box<[ParamWire]>, pub output_params: Box<[ParamWire]>, pub sig: FnSigFlags, pub generics: Box<[GenericParamWire]>, pub wheres: Box<[WherePredWire]> }
pub struct TraitWire { pub supers: Box<[TypeRefWire]>, pub flags: TraitFlags, pub generics: Box<[GenericParamWire]>, pub wheres: Box<[WherePredWire]> }
pub struct ImplWire { pub of: Option<TypeRefWire>, pub self_ty: TypeWire, pub flags: ImplFlags, pub generics: Box<[GenericParamWire]>, pub wheres: Box<[WherePredWire]> }
pub struct EnumWire { pub variants: Box<[IntroId]>, pub generics: Box<[GenericParamWire]>, pub wheres: Box<[WherePredWire]>, pub auto: Box<[AutoFact]> }
pub struct VariantWire { pub form: VariantForm, pub discr: Option<String>, pub fields: Box<[IntroId]> }
pub struct ConstWire { pub ty: TypeRefWire, pub value: Option<String> }
pub struct StaticWire { pub ty: TypeRefWire, pub mutable: bool }
pub struct ReexportWire { pub target: StableRef }
pub struct TypeAliasWire { pub ty: TypeWire, pub generics: Box<[GenericParamWire]>, pub wheres: Box<[WherePredWire]>, pub auto: Box<[AutoFact]> }
pub enum KindWire { Module(ModuleWire), Record(RecordWire), Field(FieldWire), Function(FunctionWire), Type(TypeAliasWire), Trait(TraitWire), Impl(ImplWire), Enum(EnumWire), Variant(VariantWire), Const(ConstWire), Static(StaticWire), Reexport(ReexportWire) }
impl KindWire { pub fn discriminant(&self) -> KindDiscriminant }
```

## Symbol + payload
```rust
pub struct SymbolWire { pub name:String, pub visibility:Visibility, pub documentation:Option<String>, pub source_path:String, pub span_start:u32, pub span_end:u32, pub aliases:Vec<String>, pub deprecation:Option<DeprecationWire>, pub doc_links:Vec<DocLinkWire>, pub attrs:Vec<AttrTok>, pub cfg:Option<CfgExpr> }
pub struct EntryPayloadFlags(pub u8); // HAS_DEPRECATION = 1<<1 ; bit0 reserved (was IS_REFERENCE, retired v2)
pub struct OwnedEntryPayload { pub symbol:SymbolWire, pub kind_disc:KindDiscriminant, pub kind:KindWire, pub flags:EntryPayloadFlags, pub payload_hash:ContentBlake3 }
impl OwnedEntryPayload { const HASH_DOMAIN:&str = "nudox.entry.v2"; fn sealed(symbol,kind_disc,kind,flags) -> Self; fn compute_payload_hash(&symbol,&kind_disc,&kind,&flags) -> ContentBlake3 }
```

## Intro v2 + SigKey (`nudox_ir::intro`)
```rust
pub enum DisambiguatorV2 { None, FnOverload(Box<[u8]>), Span{start:u32,end:u32}, TraitImpl(Box<[u8]>) } // to_bytes(): None=empty; else prefix 0x01/0x02/0x03 + payload
pub fn bootstrap_intro_id_v2(package:&PackageLineageId, kind_disc:KindDiscriminant, segments:&[&str], name:&str, d:&DisambiguatorV2) -> IntroId // domain "nudox.intro.v2"
pub fn sig_key(inputs:&[ParamWire], outputs:&[ParamWire], sig:&FnSigFlags) -> ContentBlake3 // "nudox.sigkey.v1"
```

## Skeleton (`nudox_ir::skeleton`)
```rust
pub fn function_signature_skeleton(inputs:&[ParamWire], outputs:&[ParamWire]) -> Vec<u8>
pub fn trait_impl_skeleton(of:Option<&TypeRefWire>, self_ty:&TypeWire) -> Vec<u8>
pub fn fnsig_flag_bytes(sig:&FnSigFlags) -> Vec<u8>
pub fn type_skeleton(&TypeRefWire, &mut Vec<u8>); pub fn type_wire_skeleton(&TypeWire, &mut Vec<u8>)
```

## Container (`nudox_ir::apply::PristineIntroTable`)
`new()`, `insert_live(IntroId, OwnedEntryPayload, Option<IntroId>)`, `insert_link(LinkRecord)`,
`live_entries() -> impl Iterator<Item=(IntroId,&OwnedEntryPayload)>`, `parent_of(IntroId)->Option<IntroId>`,
`links()->impl Iterator<Item=&LinkRecord>`, `get(IntroId)->Option<&OwnedEntryPayload>`, `is_live(IntroId)->bool`, `len()`, `is_empty()`.
`LinkRecord{ a:StableRef, b:StableRef, kind_a:KindDiscriminant, kind_b:KindDiscriminant }`.

## nudox_change helpers
`IntroId::{from_raw([u8;32]), as_bytes()->&[u8;32], to_hex()}`; `StableRef{package:PackageLineageId,intro:IntroId}` + `encode(&mut Vec<u8>)`;
`PackageLineageId{ecosystem:EcosystemId,name:PackageName}` + `encode`; `ContentBlake3::{from_domain(domain,&[u8]), from_raw, as_bytes, to_hex}`;
`encode::{encode_str(&mut Vec<u8>,&str), encode_segments, write_u16le, write_u32le, write_u64le}`.

## F1 typeref/typeexpr ASCII (reuse from `nudox-ir-vcs/blob.rs`, already ASCII-pure & frozen)
- typeref: `S:<64hex>` (Same) | `F:<eco>/<pkg>#<64hex>` (Foreign)
- typeexpr: `self|never|any|prim:bool|prim:char|prim:str|prim:int:<s|u>:<width>|prim:float:<width>|prim:mutptr:<typeref>|prim:constptr:<typeref>|prim:ref:<mut|shared>:<typeref>|prim:builtin:<esc>|tuple:<tr,tr,…>|slice:<tr>|array:<tr>:<len>|union:<…>|intersection:<…>` ; width = `arch` | decimal
