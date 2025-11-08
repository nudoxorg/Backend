import Foundation

// MARK: - Full rustdoc_types mirror in Swift

struct RustdocCrate: Decodable {
  let root: Int
  let crate_version: String?
  let includes_private: Bool
  let index: [Int: RustdocItem]
  let paths: [Int: RustdocPathSummary]
  let external_crates: [Int: ExternalCrate]
  let target: Target
  let format_version: Int
}

struct RustdocPathSummary: Decodable {
  let crate_id: Int
  let kind: String
  let path: [String]
}

struct ExternalCrate: Decodable {
  let name: String
  let html_root_url: String?
}

struct Target: Decodable {
  let triple: String
  let target_features: [TargetFeature]
}

struct TargetFeature: Decodable {
  let name: String
  let implies_features: [String]
  let unstable_feature_gate: String?
  let globally_enabled: Bool
}

struct RustdocItem: Decodable {
  let id: Int
  let crate_id: Int
  let name: String?
  let span: Span?
  let visibility: String
  let docs: String?
  let links: [String: Int]
  let attrs: [String]
  let deprecation: Deprecation?
  let inner: ItemEnum
}

enum rustVisibility: Decodable {
  case `public`
  case `default`
  case crate
  case restricted(parent: Int, path: String)

  private enum CodingKeys: String, CodingKey {
    case kind
    case parent
    case path
  }

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)

    // The "kind" field tells us which variant it is
    let kind = try container.decode(String.self, forKey: .kind)
    switch kind {
    case "public":
      self = .public
    case "default":
      self = .default
    case "crate":
      self = .crate
    case "restricted":
      let parent = try container.decode(Int.self, forKey: .parent)
      let path = try container.decode(String.self, forKey: .path)
      self = .restricted(parent: parent, path: path)
    default:
      throw DecodingError.dataCorruptedError(
        forKey: .kind,
        in: container,
        debugDescription: "Unknown visibility kind: \(kind)")
    }
  }
}

struct Deprecation: Decodable {
  let since: String?
  let note: String?
}

// MARK: - Tuple helpers
enum Tuple2<T: Decodable, U: Decodable>: Decodable {
  case first(T)
  case second(U)
}

// MARK: - Span
struct Span: Decodable {
  let filename: String
  let begin: [Int]
  let end: [Int]
}

struct AnyDecodable: Decodable {
  let value: Any

  init(from decoder: Decoder) throws {
    let container = try decoder.singleValueContainer()
    if let intVal = try? container.decode(Int.self) {
      value = intVal
    } else if let doubleVal = try? container.decode(Double.self) {
      value = doubleVal
    } else if let boolVal = try? container.decode(Bool.self) {
      value = boolVal
    } else if let stringVal = try? container.decode(String.self) {
      value = stringVal
    } else if let dictVal = try? container.decode([String: AnyDecodable].self) {
      value = dictVal.mapValues { $0.value }
    } else if let arrayVal = try? container.decode([AnyDecodable].self) {
      value = arrayVal.map { $0.value }
    } else {
      value = NSNull()
    }
  }
}

// MARK: - ItemEnum (complete)

enum ItemEnum: Decodable {
  case module(Module)
  case externCrate(name: String, rename: String?)
  case use(Use)
  case union(Union)
  case structItem(rustStruct)
  case structField(rustType)
  case enumItem(Enum)
  case variant(Variant)
  case function(Function)
  case traitItem(Trait)
  case traitAlias(TraitAlias)
  case impl(Impl)
  case typeAlias(TypeAlias)
  case constant(type: rustType, const: Constant)
  case staticItem(Static)
  case externType
  case macroItem(String)
  case procMacro(ProcMacro)
  case primitive(rustPrimitive)
  case assocConst(type: rustType, value: String?)
  case assocType(generics: rustGenerics, bounds: [GenericBound], type: rustType?)
  case unknown

  private enum TopLevelCodingKeys: String, CodingKey {
    case module
    case externCrate = "extern_crate"
    case `use`
    case union
    case `struct`
    case structField = "struct_field"
    case enumItem = "enum"
    case variant
    case function
    case trait  // "trait" is the key for Trait items
    case traitAlias = "trait_alias"
    case `impl`
    case typeAlias = "type_alias"
    case constant
    case `static`
    case externType = "extern_type"
    case macro  // "macro" is the key for macroItem(String)
    case procMacro = "proc_macro"
    case primitive
    case assocConst = "assoc_const"
    case assocType = "assoc_type"
  }

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: TopLevelCodingKeys.self)

    // Use a series of `if let try?` to check for each possible key
    if let value = try? container.decode(Module.self, forKey: .module) {
      self = .module(value)
    } else if let value = try? container.decode(ExternCrate.self, forKey: .externCrate) {
      self = .externCrate(name: value.name, rename: value.rename)
    } else if let value = try? container.decode(Use.self, forKey: .use) {
      self = .use(value)
    } else if let value = try? container.decode(Union.self, forKey: .union) {
      self = .union(value)
    } else if let value = try? container.decode(rustStruct.self, forKey: .struct) {
      self = .structItem(value)
    } else if let value = try? container.decode(rustType.self, forKey: .structField) {
      self = .structField(value)
    } else if let value = try? container.decode(Enum.self, forKey: .enumItem) {
      self = .enumItem(value)
    } else if let value = try? container.decode(Variant.self, forKey: .variant) {
      self = .variant(value)
    } else if let value = try? container.decode(Function.self, forKey: .function) {
      self = .function(value)
    } else if let value = try? container.decode(Trait.self, forKey: .trait) {
      self = .traitItem(value)
    } else if let value = try? container.decode(TraitAlias.self, forKey: .traitAlias) {
      self = .traitAlias(value)
    } else if let value = try? container.decode(Impl.self, forKey: .impl) {
      self = .impl(value)
    } else if let value = try? container.decode(TypeAlias.self, forKey: .typeAlias) {
      self = .typeAlias(value)
    } else if let value = try? container.decode(ConstantItem.self, forKey: .constant) {
      self = .constant(type: value.type, const: value.const)
    } else if let value = try? container.decode(Static.self, forKey: .static) {
      self = .staticItem(value)
    } else if container.contains(.externType) {  // extern_type is a marker, no associated value
      self = .externType
    } else if let value = try? container.decode(String.self, forKey: .macro) {  // macro's value is just a string
      self = .macroItem(value)
    } else if let value = try? container.decode(ProcMacro.self, forKey: .procMacro) {
      self = .procMacro(value)
    } else if let value = try? container.decode(rustPrimitive.self, forKey: .primitive) {
      self = .primitive(value)
    } else if let value = try? container.decode(AssocConstItem.self, forKey: .assocConst) {
      self = .assocConst(type: value.type, value: value.value)
    } else if let value = try? container.decode(AssocTypeItem.self, forKey: .assocType) {
      self = .assocType(generics: value.generics, bounds: value.bounds, type: value.type)
    } else {
      // Fallback for unknown keys or if none of the above succeeded
      if let firstKey = container.allKeys.first {
        // Attempt to get the raw JSON for better logging
        // This still uses a temporary AnyEncodable, but ONLY for logging.
        let debugJsonString: String
        if let nestedContainer = try? container.decode(AnyDecodableValue.self, forKey: firstKey),
          let jsonData = try? JSONEncoder().encode(nestedContainer)
        {
          debugJsonString = String(data: jsonData, encoding: .utf8) ?? "<unprintable JSON>"
        } else {
          debugJsonString = "<could not extract nested JSON for logging>"
        }
        print("⚠️ Unknown ItemEnum case: \(firstKey.stringValue)\n\(debugJsonString)")
      } else {
        print("⚠️ Unknown ItemEnum case: (no keys found in container)")
      }
      self = .unknown
    }
  }
}

private func decodeFromRaw<T: Decodable>(_ raw: AnyDecodable, as type: T.Type) throws -> T {
  let data = try JSONEncoder().encode(AnyEncodable(raw.value))
  return try JSONDecoder().decode(T.self, from: data)
}

struct constantItem {
  let `type`: rustType
  let const: Constant
}

struct RawJSON: Decodable {
  private let value: AnyDecodableValue

  init(from decoder: Decoder) throws {
    self.value = try decoder.singleValueContainer().decode(AnyDecodableValue.self)
  }

  func decoder() throws -> Decoder {
    let data = try JSONEncoder().encode(value)
    return try JSONDecoder().decode(DecodableDecoder.self, from: data).decoder
  }
}

// MARK: - AnyDecodableValue
struct AnyDecodableValue: Codable {
  let value: Any

  init(from decoder: Decoder) throws {
    let container = try decoder.singleValueContainer()
    if let intVal = try? container.decode(Int.self) {
      value = intVal
    } else if let doubleVal = try? container.decode(Double.self) {
      value = doubleVal
    } else if let boolVal = try? container.decode(Bool.self) {
      value = boolVal
    } else if let stringVal = try? container.decode(String.self) {
      value = stringVal
    } else if let dictVal = try? container.decode([String: AnyDecodableValue].self) {
      value = dictVal.mapValues { $0.value }
    } else if let arrayVal = try? container.decode([AnyDecodableValue].self) {
      value = arrayVal.map { $0.value }
    } else {
      value = NSNull()
    }
  }

  func encode(to encoder: Encoder) throws {
    var container = encoder.singleValueContainer()
    switch value {
    case let intVal as Int:
      try container.encode(intVal)
    case let doubleVal as Double:
      try container.encode(doubleVal)
    case let boolVal as Bool:
      try container.encode(boolVal)
    case let stringVal as String:
      try container.encode(stringVal)
    case let dictVal as [String: Any]:
      try container.encode(dictVal.mapValues { AnyEncodable($0) })
    case let arrayVal as [Any]:
      try container.encode(arrayVal.map { AnyEncodable($0) })
    default:
      try container.encodeNil()
    }
  }
}

struct AnyEncodable: Encodable {
  private let value: Any

  init(_ value: Any) {
    self.value = value
  }

  func encode(to encoder: Encoder) throws {
    var container = encoder.singleValueContainer()
    switch value {
    case let intVal as Int:
      try container.encode(intVal)
    case let doubleVal as Double:
      try container.encode(doubleVal)
    case let boolVal as Bool:
      try container.encode(boolVal)
    case let stringVal as String:
      try container.encode(stringVal)
    case let dictVal as [String: Any]:
      try container.encode(dictVal.mapValues { AnyEncodable($0) })
    case let arrayVal as [Any]:
      try container.encode(arrayVal.map { AnyEncodable($0) })
    default:
      try container.encodeNil()
    }
  }
}

// MARK: - DecodableDecoder
struct DecodableDecoder: Decodable {
  let decoder: Decoder
  init(from decoder: Decoder) throws {
    self.decoder = decoder
  }
}

// MARK: - Small helper structs
struct ExternCrate: Decodable {
  let name: String
  let rename: String?
}

struct ConstantItem: Decodable {
  let type: rustType
  let const: Constant
}

struct AssocConstItem: Decodable {
  let type: rustType
  let value: String?
}

struct AssocTypeItem: Decodable {
  let generics: rustGenerics
  let bounds: [GenericBound]
  let `type`: rustType?
}
// MARK: - Supporting structs

struct Module: Decodable {
  let is_crate: Bool
  let items: [Int]
  let is_stripped: Bool
}

struct Use: Decodable {
  let source: String
  let name: String
  let id: Int?
  let is_glob: Bool
}

struct Union: Decodable {
  let generics: rustGenerics
  let has_stripped_fields: Bool
  let fields: [Int]
  let impls: [Int]
}

struct rustStruct: Decodable {
  let kind: rustStructKind
  let generics: rustGenerics
  let impls: [Int]
}

enum rustStructKind: Decodable {
  case unit
  case tuple([Int?])
  case plain(fields: [Int], has_stripped_fields: Bool)

  private enum CodingKeys: String, CodingKey {
    case unit
    case tuple
    case plain
  }

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)

    if container.contains(.unit) {
      self = .unit
    } else if let tupleVals = try? container.decode([Int?].self, forKey: .tuple) {
      self = .tuple(tupleVals)
    } else if let plainPayload = try? container.decode(PlainStructPayload.self, forKey: .plain) {
      self = .plain(
        fields: plainPayload.fields, has_stripped_fields: plainPayload.has_stripped_fields)
    } else {
      throw DecodingError.dataCorruptedError(
        forKey: .plain,
        in: container,
        debugDescription: "Unknown StructKind case: \(container.allKeys)"
      )
    }
  }
}

private struct PlainStructPayload: Decodable {
  let fields: [Int]
  let has_stripped_fields: Bool
}

struct Enum: Decodable {
  let generics: rustGenerics
  let has_stripped_variants: Bool
  let variants: [Int]
  let impls: [Int]
}

struct Variant: Decodable {
  let kind: VariantKind
  let discriminant: Discriminant?
}

enum VariantKind: Decodable {
  case plain
  case tuple([Int])
  case `struct`(fields: [Int], has_stripped_fields: Bool)

  private enum CodingKeys: String, CodingKey {
    case plain
    case tuple
    case `struct`
  }

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)

    if container.contains(.plain) {
      self = .plain
    } else if let tupleVals = try? container.decode([Int].self, forKey: .tuple) {
      self = .tuple(tupleVals)
    } else if let structPayload = try? container.decode(StructVariantPayload.self, forKey: .struct)
    {
      self = .struct(
        fields: structPayload.fields, has_stripped_fields: structPayload.has_stripped_fields)
    } else {
      throw DecodingError.dataCorruptedError(
        forKey: .plain,
        in: container,
        debugDescription: "Unknown VariantKind case: \(container.allKeys)"
      )
    }
  }
}

private struct StructVariantPayload: Decodable {
  let fields: [Int]
  let has_stripped_fields: Bool
}

struct Discriminant: Decodable {
  let expr: String
  let value: String
}

struct Function: Decodable {
  let sig: rustFunctionSignature
  let generics: rustGenerics
  let header: rustFunctionHeader
  let has_body: Bool
}

struct rustFunctionSignature: Decodable {
  let inputs: [FunctionInput]
  let output: rustType?
  let is_c_variadic: Bool
}

struct rustFunctionHeader: Decodable {
  let is_const: Bool
  let is_unsafe: Bool
  let is_async: Bool
  let abi: String
}

struct Trait: Decodable {
  let is_auto: Bool
  let is_unsafe: Bool
  let is_dyn_compatible: Bool
  let items: [Int]
  let generics: rustGenerics
  let bounds: [GenericBound]
  let implementations: [Int]
}

struct TraitAlias: Decodable {
  let generics: rustGenerics
  let params: [GenericBound]
}

struct Impl: Decodable {
  let is_unsafe: Bool
  let generics: rustGenerics
  let provided_trait_methods: [String]
  let trait: rustPath?
  let `for`: rustType
  let items: [Int]
  let is_negative: Bool
  let is_synthetic: Bool
  let blanket_impl: rustType?
}

struct TypeAlias: Decodable {
  let type: rustType
  let generics: rustGenerics
}

struct Static: Decodable {
  let type: rustType
  let is_mutable: Bool
  let expr: String
  let is_unsafe: Bool
}

struct ProcMacro: Decodable {
  let kind: String
  let helpers: [String]
}

struct rustPrimitive: Decodable {
  let name: String
  let impls: [Int]
}

struct rustGenerics: Decodable {
  let params: [GenericParamDef]
  let where_predicates: [WherePredicate]
}

struct GenericParamDef: Decodable {
  let name: String
  let kind: GenericParamDefKind
}

enum GenericParamDefKind: Decodable {
  case lifetime(outlives: [String])
  case type(default: rustType?, bounds: [GenericBound], is_synthetic: Bool)
  case const(type: rustType, default: String?)
}

private struct LifetimePayload: Decodable {
  let outlives: [String]
}

private struct TypeParamPayload: Decodable {
  let bounds: [GenericBound]
  let `default`: rustType?
  let is_synthetic: Bool
}

private struct ConstParamPayload: Decodable {
  let type_: rustType
  let `default`: String?
}

enum rustGenericBound: Decodable {
  case trait_bound(trait: rustPath, generic_params: [GenericParamDef], modifier: String)
  case outlives(String)
  case use([PreciseCapturingArg])

  private enum CodingKeys: String, CodingKey {
    case trait_bound
    case outlives
    case use
  }

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)

    if let payload = try? container.decode(TraitBoundPayload.self, forKey: .trait_bound) {
      self = .trait_bound(
        trait: payload.trait,
        generic_params: payload.generic_params,
        modifier: payload.modifier)
      return
    }
    if let lifetime = try? container.decode(String.self, forKey: .outlives) {
      self = .outlives(lifetime)
      return
    }
    if let args = try? container.decode([PreciseCapturingArg].self, forKey: .use) {
      self = .use(args)
      return
    }

    throw DecodingError.dataCorruptedError(
      forKey: .use,
      in: container,
      debugDescription: "Unknown GenericBound case: \(container.allKeys)"
    )
  }
}

private struct TraitBoundPayload: Decodable {
  let trait: rustPath
  let generic_params: [GenericParamDef]
  let modifier: String
}

enum PreciseCapturingArg: Decodable {
  case Lifetime(String)
  case Param(String)
}

enum WherePredicate: Decodable {
  case bound_predicate(type: rustType, generic_params: [GenericParamDef], bounds: [GenericBound])
  case lifetime_predicate(lifetime: String, outlives: [String])
  case eq_predicate(lhs: rustType, rhs: Term)
}

// MARK: - Path
struct rustPath: Decodable {
  let path: String
  let id: Int
  let args: rustGenericArgs?
}

// MARK: - DynTrait
struct rustDynTrait: Decodable {
  let traits: [rustPolyTrait]
  let lifetime: String?
}

// MARK: - PolyTrait
struct rustPolyTrait: Decodable {
  let trait: rustPath
  let generic_params: [GenericParamDef]
}

// MARK: - GenericArgs
enum rustGenericArgs: Decodable {
  case angle_bracketed(args: [rustGenericArg], constraints: [AssocItemConstraint])
  case parenthesized(inputs: [rustType], output: rustType?)
  case return_type_notation

  private enum CodingKeys: String, CodingKey {
    case angle_bracketed
    case parenthesized
    case return_type_notation
  }

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)

    if let payload = try? container.decode(AngleBracketedArgs.self, forKey: .angle_bracketed) {
      self = .angle_bracketed(args: payload.args ?? [], constraints: payload.constraints)
      return
    }
    if let payload = try? container.decode(ParenthesizedArgs.self, forKey: .parenthesized) {
      self = .parenthesized(inputs: payload.inputs, output: payload.output)
      return
    }
    if container.contains(.return_type_notation) {
      self = .return_type_notation
      return
    }

    throw DecodingError.dataCorruptedError(
      forKey: .return_type_notation,
      in: container,
      debugDescription: "Unknown GenericArgs case: \(container.allKeys)"
    )
  }
}

private struct AngleBracketedArgs: Decodable {
  let args: [rustGenericArg]?
  let constraints: [AssocItemConstraint]
}

private struct ParenthesizedArgs: Decodable {
  let inputs: [rustType]
  let output: rustType?
}

// MARK: - GenericArg
enum rustGenericArg: Decodable {
  case lifetime(String)
  case type(rustType)
  case const(Constant)
  case infer

  init(from decoder: Decoder) throws {
    let container = try decoder.singleValueContainer()
    let raw = try container.decode([String: RawJSON].self)

    guard let (key, rawValue) = raw.first else {
      throw DecodingError.dataCorruptedError(
        in: container,
        debugDescription: "GenericArg object had no keys"
      )
    }

    let valueDecoder = try rawValue.decoder()

    switch key {
    case "lifetime":
      self = .lifetime(try String(from: valueDecoder))
    case "type":
      self = .type(try rustType(from: valueDecoder))  // <-- This now works because Type is fixed
    case "const":
      self = .const(try Constant(from: valueDecoder))
    case "infer":
      self = .infer
    default:
      throw DecodingError.dataCorruptedError(
        in: container,
        debugDescription: "Unknown GenericArg variant: \(key)"
      )
    }
  }
}

// MARK: - AssocItemConstraint
struct AssocItemConstraint: Decodable {
  let name: String
  let args: rustGenericArgs?
  let binding: AssocItemConstraintKind

  private enum CodingKeys: String, CodingKey {
    case name
    case args
    case binding
  }

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    name = try container.decode(String.self, forKey: .name)
    args = try container.decodeIfPresent(rustGenericArgs.self, forKey: .args)
    binding = try container.decode(AssocItemConstraintKind.self, forKey: .binding)
  }
}

enum AssocItemConstraintKind: Decodable {
  case equality(Term)
  case constraint([rustGenericBound])

  private enum CodingKeys: String, CodingKey {
    case equality
    case constraint
  }

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)

    if let term = try? container.decode(Term.self, forKey: .equality) {
      self = .equality(term)
      return
    }
    if let bounds = try? container.decode([rustGenericBound].self, forKey: .constraint) {
      self = .constraint(bounds)
      return
    }

    throw DecodingError.dataCorruptedError(
      forKey: .constraint,
      in: container,
      debugDescription: "Unknown AssocItemConstraintKind case: \(container.allKeys)"
    )
  }
}

struct rustFunctionPointer: Decodable {
  let sig: rustFunctionSignature
  let generic_params: [GenericParamDef]
  let header: rustFunctionHeader
}

// MARK: - Abi
enum Abi: String, Decodable {
  case rust = "Rust"
  case c = "C"
  case cdecl = "Cdecl"
  case stdcall = "Stdcall"
  case fastcall = "Fastcall"
  case aapcs = "Aapcs"
  case win64 = "Win64"
  case sysv64 = "SysV64"
  case system = "System"
  case other
}

// MARK: - Constant
struct Constant: Decodable {
  let expr: String
  let value: String?
  let is_literal: Bool
}

// MARK: - Term
enum Term: Decodable {
  case type(rustType)
  case constant(Constant)

  private enum CodingKeys: String, CodingKey {
    case type
    case constant
  }

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)

    if let t = try? container.decode(rustType.self, forKey: .type) {
      self = .type(t)
      return
    }
    if let c = try? container.decode(Constant.self, forKey: .constant) {
      self = .constant(c)
      return
    }

    throw DecodingError.dataCorruptedError(
      forKey: .constant,
      in: container,
      debugDescription: "Unknown Term case: \(container.allKeys)"
    )
  }
}

indirect enum rustType: Decodable {
  case resolvedPath(rustPath)
  case dynTrait(rustDynTrait)
  case generic(String)
  case primitive(String)
  case functionPointer(rustFunctionPointer)
  case tuple([rustType])
  case slice(rustType)
  case array(type: rustType, len: String)
  case pat(type: rustType, unstable: String)
  case implTrait([rustGenericBound])
  case infer
  case rawPointer(isMutable: Bool, type: rustType)
  case borrowedRef(
    lifetime: String?,
    isMutable:
      Bool, type: rustType)
  case qualifiedPath(name: String, args: rustGenericArgs?, selfType: rustType, trait: rustPath?)

  private enum CodingKeys: String, CodingKey {
    case resolvedPath = "resolved_path"
    case dynTrait = "dyn_trait"
    case generic
    case primitive
    case functionPointer = "fn_pointer"
    case tuple
    case slice
    case array
    case pat
    case implTrait = "impl_trait"
    case infer
    case rawPointer = "raw_pointer"
    case borrowedRef = "borrowed_ref"
    case qualifiedPath = "qualified_path"
  }

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)

    // Try each case in order
    if let value = try? container.decode(rustPath.self, forKey: .resolvedPath) {
      self = .resolvedPath(value)
      return
    }
    if let value = try? container.decode(rustDynTrait.self, forKey: .dynTrait) {
      self = .dynTrait(value)
      return
    }
    if let value = try? container.decode(String.self, forKey: .generic) {
      self = .generic(value)
      return
    }
    if let value = try? container.decode(String.self, forKey: .primitive) {
      self = .primitive(value)
      return
    }
    if let value = try? container.decode(rustFunctionPointer.self, forKey: .functionPointer) {
      self = .functionPointer(value)
      return
    }
    if let value = try? container.decode([rustType].self, forKey: .tuple) {
      self = .tuple(value)
      return
    }
    if let value = try? container.decode(rustType.self, forKey: .slice) {
      self = .slice(value)
      return
    }
    if let payload = try? container.decode(ArrayPayload.self, forKey: .array) {
      self = .array(type: payload.type, len: payload.len)
      return
    }
    if let payload = try? container.decode(PatPayload.self, forKey: .pat) {
      self = .pat(type: payload.type, unstable: payload.unstable)
      return
    }
    if let value = try? container.decode([rustGenericBound].self, forKey: .implTrait) {
      self = .implTrait(value)
      return
    }
    if container.contains(.infer) {
      self = .infer
      return
    }
    if let payload = try? container.decode(RawPointerPayload.self, forKey: .rawPointer) {
      self = .rawPointer(isMutable: payload.is_mutable, type: payload.type)
      return
    }
    if let payload = try? container.decode(BorrowedRefPayload.self, forKey: .borrowedRef) {
      self = .borrowedRef(
        lifetime: payload.lifetime,
        isMutable: payload.is_mutable,
        type: payload.type
      )
      return
    }
    if let payload = try? container.decode(QualifiedPathPayload.self, forKey: .qualifiedPath) {
      self = .qualifiedPath(
        name: payload.name,
        args: payload.args,
        selfType: payload.self_type,
        trait: payload.trait
      )
      return
    }

    // Debugging: if we got here, decoding failed
    let keys = container.allKeys.map { $0.stringValue }
    var debugJSON = "<unavailable>"
    if let raw = try? JSONSerialization.jsonObject(
      with: decoder.codingPath.isEmpty ? Data() : Data())
    {
      debugJSON = String(describing: raw)
    }
    print(
      "⚠️ Unknown Type variant. Keys: \(keys). CodingPath: \(decoder.codingPath). JSON: \(debugJSON)"
    )

    throw NewDocsError.fileNotFound("nope")
  }
}

// MARK: - Payload helper structs
private struct ArrayPayload: Decodable {
  let type: rustType
  let len: String
}

private struct PatPayload: Decodable {
  let type: rustType
  let unstable: String
  private enum CodingKeys: String, CodingKey {
    case type = "type"
    case unstable = "__pat_unstable_do_not_use"
  }
}

private struct RawPointerPayload: Decodable {
  let is_mutable: Bool
  let type: rustType
}

private struct BorrowedRefPayload: Decodable {
  let lifetime: String?
  let is_mutable: Bool
  let type: rustType
}

private struct QualifiedPathPayload: Decodable {
  let name: String
  let args: rustGenericArgs?
  let self_type: rustType
  let trait: rustPath?
}

extension rustType {
  func toString() -> String {
    switch self {
    case .resolvedPath(let path):
      // e.g. std::option::Option<u32>
      if let args = path.args {
        return "\(path.path)\(argsToString(args))"
      }
      return path.path

    case .dynTrait(let dynTrait):
      let traits = dynTrait.traits.map { polyTraitToString($0) }.joined(separator: " + ")
      if let lifetime = dynTrait.lifetime {
        return "dyn \(traits) + \(lifetime)"
      }
      return "dyn \(traits)"

    case .generic(let name):
      return name

    case .primitive(let name):
      return name

    case .functionPointer(let fnPtr):
      let params = ""
      let output = fnPtr.sig.output.map { " -> \($0.toString())" } ?? ""
      return "fn(\(params))\(output)"

    case .tuple(let types):
      return "(\(types.map { $0.toString() }.joined(separator: ", ")))"

    case .slice(let inner):
      return "[\(inner.toString())]"

    case .array(let type, let len):
      return "[\(type.toString()); \(len)]"

    case .pat(let type, _):
      return "\(type.toString()) /* pattern */"

    case .implTrait(let bounds):
      let boundsStr = bounds.map { genericBoundToString($0) }.joined(separator: " + ")
      return "impl \(boundsStr)"

    case .infer:
      return "_"

    case .rawPointer(let isMutable, let type):
      return "*\(isMutable ? "mut" : "const") \(type.toString())"

    case .borrowedRef(let lifetime, let isMutable, let type):
      let lt = lifetime.map { "\($0) " } ?? ""
      return "&\(lt)\(isMutable ? "mut " : "")\(type.toString())"

    case .qualifiedPath(let name, let args, let selfType, let trait):
      let traitStr = trait.map { $0.path } ?? ""
      let argsStr = args.map { argsToString($0) } ?? ""
      return "<\(selfType.toString()) as \(traitStr)>::\(name)\(argsStr)"
    }
  }
}

private func argsToString(_ args: rustGenericArgs) -> String {
  switch args {
  case .angle_bracketed(let argsList, let constraints):
    var parts: [String] = []
    parts.append(contentsOf: argsList.map { genericArgToString($0) })
    parts.append(contentsOf: constraints.map { assocItemConstraintToString($0) })
    return "<\(parts.joined(separator: ", "))>"

  case .parenthesized(let inputs, let output):
    let inStr = inputs.map { $0.toString() }.joined(separator: ", ")
    let outStr = output.map { " -> \($0.toString())" } ?? ""
    return "(\(inStr))\(outStr)"

  case .return_type_notation:
    return ""
  }
}
private func genericArgToString(_ arg: rustGenericArg) -> String {
  switch arg {
  case .lifetime(let lt):
    return lt
  case .type(let t):
    return t.toString()
  case .const(let c):
    return c.expr
  case .infer:
    return "_"
  }
}

private func assocItemConstraintToString(_ constraint: AssocItemConstraint) -> String {
  let argsStr = constraint.args.map { argsToString($0) } ?? ""
  switch constraint.binding {
  case .equality(let term):
    return "\(constraint.name)\(argsStr) = \(termToString(term))"
  case .constraint(let bounds):
    let boundsStr = bounds.map { genericBoundToString($0) }.joined(separator: " + ")
    return "\(constraint.name)\(argsStr): \(boundsStr)"
  }
}

private func termToString(_ term: Term) -> String {
  switch term {
  case .type(let t):
    return t.toString()
  case .constant(let c):
    return c.expr
  }
}

private func polyTraitToString(_ poly: rustPolyTrait) -> String {
  let traitPath = poly.trait.path
  if !poly.generic_params.isEmpty {
    let params = poly.generic_params.map { $0.name }.joined(separator: ", ")
    return "\(traitPath)<\(params)>"
  }
  return traitPath
}

private func genericBoundToString(_ bound: rustGenericBound) -> String {
  switch bound {
  case .trait_bound(let trait, let generic_params, let _):
    if !generic_params.isEmpty {
      let params = generic_params.map { $0.name }.joined(separator: ", ")
      return "\(trait.path)<\(params)>"
    }
    return trait.path
  case .outlives(let lifetime):
    return lifetime
  case .use(let args):
    return "use<>"
  }
}

struct FunctionInput: Decodable {
  let name: String
  let type: rustType

  init(from decoder: Decoder) throws {
    var container = try decoder.unkeyedContainer()
    name = try container.decode(String.self)
    type = try container.decode(rustType.self)
  }
}

extension rustType {
  /// Converts a Rust type representation to the universal Type representation.
  func toType() -> Type {
    switch self {
    case .resolvedPath(let path):
      return .resolvedPath(path.toPath())

    case .dynTrait(let dynTrait):
      return .dynTrait(dynTrait.toDynTrait())

    case .generic(let name):
      return .genericParam(name)

    case .primitive(let name):
      return .primitive(name.toPrimitive())

    case .functionPointer(let fnPtr):
      return .functionPointer(fnPtr.toFunctionPointer())

    case .tuple(let types):
      return .tuple(types.map { $0.toType() })

    case .slice(let inner):
      return .slice(inner.toType())

    case .array(let type, let len):
      // Convert string length to UInt if possible, default to 0
      let length = UInt(len) ?? 0
      return .array(type: type.toType(), length: length)

    case .pat(let type, _):
      return .pattern(type: type.toType())

    case .implTrait(let bounds):
      return .implTrait(bounds.map { $0.toGenericBound() })

    case .infer:
      return .infer

    case .rawPointer(let isMutable, let type):
      return .rawPointer(isMutable: isMutable, type: type.toType())

    case .borrowedRef(let lifetime, let isMutable, let type):
      return .borrowedRef(lifetime: lifetime, isMutable: isMutable, type: type.toType())

    case .qualifiedPath(let name, let args, let selfType, let trait):
      return .qualifiedPath(
        QualifiedPath(
          name: name,
          args: args?.toGenericArgs(),
          selfType: selfType.toType(),
          trait: trait?.toPath()
        ))
    }
  }
}

extension rustPath {
  func toPath() -> Path {
    Path(
      path: path,
      args: args?.toGenericArgs()
    )
  }
}

extension rustDynTrait {
  func toDynTrait() -> DynTrait {
    DynTrait(
      traits: traits.map { $0.toPolyTrait() },
      lifetime: lifetime
    )
  }
}

extension rustPolyTrait {
  func toPolyTrait() -> PolyTrait {
    let typeExprs: [TypeExpr] =
      trait.args?.toGenericArgs().args.compactMap { arg -> TypeExpr? in
        if case .type(let type) = arg {
          // Use toString() method instead of description
          return TypeExpr(name: type.toTypeString(), args: [])
        }
        return nil
      } ?? []

    return PolyTrait(
      trait: TraitRef(
        name: trait.path,
        args: typeExprs
      ),
      lifetimes: generic_params.compactMap { param in
        if case .lifetime(let outlives) = param.kind {
          return param.name
        }
        return nil
      }
    )
  }
}

extension rustFunctionPointer {
  func toFunctionPointer() -> FunctionPointer {
    var attrs: [FunctionAttributes] = []
    if sig.is_c_variadic { attrs.append(.variadic) }
    if header.is_const { attrs.append(.const) }
    if header.is_async { attrs.append(.async) }
    if header.is_unsafe { attrs.append(.unsafe) }

    let typeParams: [TypeParam]? = {
      let params = generic_params.compactMap { param -> TypeParam? in
        guard case .type(let defaultType, _, let isSynthetic) = param.kind else {
          return nil
        }
        return TypeParam(
          name: param.name,
          kind: isSynthetic ? .associated : .type,
          variance: .invariant,
          defaultType: defaultType.map { TypeExpr(name: $0.toString(), args: []) }
        )
      }
      return params.isEmpty ? nil : params
    }()

    return FunctionPointer(
      inputs: nil,
      outputs: nil,
      genericParams: typeParams,
      attributes: attrs.isEmpty ? nil : attrs
    )
  }
}

extension rustGenericArgs {
  func toGenericArgs() -> GenericArgs {
    switch self {
    case .angle_bracketed(let args, _):
      return GenericArgs(args: args.map { $0.toGenericArg() })

    case .parenthesized(let inputs, let output):
      // Convert parenthesized args to type arguments
      var result = inputs.map { GenericArg.type($0.toType()) }
      if let out = output {
        result.append(.type(out.toType()))
      }
      return GenericArgs(args: result)

    case .return_type_notation:
      return GenericArgs(args: [])
    }
  }
}

extension rustGenericArg {
  func toGenericArg() -> GenericArg {
    switch self {
    case .lifetime(let name):
      return .lifetime(name)

    case .type(let type):
      return .type(type.toType())

    case .const(let constant):
      return .constExpr(ConstExpr(expr: constant.expr))

    case .infer:
      return .type(.infer)
    }
  }
}

extension rustGenericBound {
  func toGenericBound() -> GenericBound {
    switch self {
    case .trait_bound(let trait, _, _):
      let typeExprs: [TypeExpr] =
        trait.args?.toGenericArgs().args.compactMap { arg -> TypeExpr? in
          if case .type(let type) = arg {
            return TypeExpr(name: type.toTypeString(), args: [])
          }
          return nil
        } ?? []

      return .trait(
        TraitRef(
          name: trait.path,
          args: typeExprs
        ))

    case .outlives(let lifetime):
      return .lifetime(lifetime)

    case .use(_):
      // Use bounds don't have a direct equivalent, treat as lifetime
      return .lifetime("'_")
    }
  }
}

extension String {
  func toPrimitive() -> Primitive {
    switch self {
    case "i8": return .int8(0)
    case "i16": return .int16(0)
    case "i32", "isize": return .int(0)
    case "i64": return .int64(0)
    case "i128": return .int128(0)
    case "u8": return .uint8(0)
    case "u16": return .uint16(0)
    case "u32", "usize": return .uint(0)
    case "u64": return .uint64(0)
    case "u128": return .uint128(0)
    case "f16": return .f16(0)
    case "f32": return .float(0)
    case "f64": return .double(0)
    case "bool": return .bool(false)
    case "str", "String": return .string("")
    case "char": return .char(" ")
    default: return .null
    }
  }
}

// Helper to convert Type to string representation
extension Type {
  func toTypeString() -> String {
    switch self {
    case .resolvedPath(let path):
      return path.path
    case .genericParam(let name):
      return name
    case .primitive(let prim):
      return primitiveToString(prim)
    case .tuple(let types):
      return "(\(types.map { $0.toTypeString() }.joined(separator: ", ")))"
    case .slice(let inner):
      return "[\(inner.toTypeString())]"
    case .array(let type, let len):
      return "[\(type.toTypeString()); \(len)]"
    case .infer:
      return "_"
    case .rawPointer(let isMutable, let type):
      return "*\(isMutable ? "mut" : "const") \(type.toTypeString())"
    case .borrowedRef(let lifetime, let isMutable, let type):
      let lt = lifetime.map { "\($0) " } ?? ""
      return "&\(lt)\(isMutable ? "mut " : "")\(type.toTypeString())"
    default:
      return "unknown"
    }
  }

  private func primitiveToString(_ prim: Primitive) -> String {
    switch prim {
    case .int8: return "i8"
    case .int16: return "i16"
    case .int: return "i32"
    case .int64: return "i64"
    case .int128: return "i128"
    case .uint8: return "u8"
    case .uint16: return "u16"
    case .uint: return "u32"
    case .uint64: return "u64"
    case .uint128: return "u128"
    case .f16: return "f16"
    case .float: return "f32"
    case .double: return "f64"
    case .bool: return "bool"
    case .string: return "String"
    case .char: return "char"
    default: return "unknown"
    }
  }
}
