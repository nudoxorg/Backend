import Foundation

indirect public enum Type: Codable, Sendable {
  case resolvedPath(Path)
  case dynTrait(DynTrait)
  case genericParam(String)
  case primitive(Primitive)
  case functionPointer(FunctionPointer)
  case tuple([Type])
  case slice(Type)
  case array(type: Type, length: UInt)
  case pattern(type: Type)
  case implTrait([GenericBound])
  case infer
  case rawPointer(isMutable: Bool, type: Type)
  case union([Type])
  case sum([SumVariant])
  case intersection([Type])
  case borrowedRef(lifetime: String?, isMutable: Bool, type: Type)
  case qualifiedPath(QualifiedPath)
}

public struct SumVariant: Codable, Sendable {
  /// The variant/tag name (e.g., "Some", "None", "Ok", "Err")
  public let name: String

  /// Associated types for this variant (nil for unit variants)
  public let types: [Type]?

  public init(name: String, types: [Type]? = nil) {
    self.name = name
    self.types = types
  }
}

public enum Primitive: Sendable, Codable {
  case int8(Int8?)
  case int16(Int16?)
  case int(Int?)
  case int64(Int64?)
  case int128(Int128?)
  case uint8(UInt8?)
  case uint16(UInt16?)
  case uint(UInt?)
  case uint64(UInt64?)
  case uint128(UInt128?)
  case f16(Float16?)
  case float(Float?)
  case double(Double?)
  case bool(Bool?)
  case string(String?)
  case char(Character?)

  // MARK: - Special Values
  case null
  case date(Date?)
  case data(Data?)

  // MARK: - Codable Support
  private enum CodingKeys: String, CodingKey {
    case type, value
  }

  private enum CaseType: String, Codable {
    case int8, int16, int, int64, int128
    case uint8, uint16, uint, uint64, uint128
    case f16, float, double, bool, string, char
    case null, date, data
  }

  public func encode(to encoder: Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)

    switch self {
    case .int8(let v):
      try container.encode(CaseType.int8, forKey: .type)
      try container.encode(v, forKey: .value)
    case .int16(let v):
      try container.encode(CaseType.int16, forKey: .type)
      try container.encode(v, forKey: .value)
    case .int(let v):
      try container.encode(CaseType.int, forKey: .type)
      try container.encode(v, forKey: .value)
    case .int64(let v):
      try container.encode(CaseType.int64, forKey: .type)
      try container.encode(v, forKey: .value)
    case .int128(let v):
      try container.encode(CaseType.int128, forKey: .type)
      try container.encode(v, forKey: .value)
    case .uint8(let v):
      try container.encode(CaseType.uint8, forKey: .type)
      try container.encode(v, forKey: .value)
    case .uint16(let v):
      try container.encode(CaseType.uint16, forKey: .type)
      try container.encode(v, forKey: .value)
    case .uint(let v):
      try container.encode(CaseType.uint, forKey: .type)
      try container.encode(v, forKey: .value)
    case .uint64(let v):
      try container.encode(CaseType.uint64, forKey: .type)
      try container.encode(v, forKey: .value)
    case .uint128(let v):
      try container.encode(CaseType.uint128, forKey: .type)
      try container.encode(v, forKey: .value)
    case .f16(let v):
      try container.encode(CaseType.f16, forKey: .type)
      try container.encode(v, forKey: .value)
    case .float(let v):
      try container.encode(CaseType.float, forKey: .type)
      try container.encode(v, forKey: .value)
    case .double(let v):
      try container.encode(CaseType.double, forKey: .type)
      try container.encode(v, forKey: .value)
    case .bool(let v):
      try container.encode(CaseType.bool, forKey: .type)
      try container.encode(v, forKey: .value)
    case .string(let v):
      try container.encode(CaseType.string, forKey: .type)
      try container.encode(v, forKey: .value)
    case .char(let v):
      try container.encode(CaseType.char, forKey: .type)
      // Default initailizae
      let val = v ?? Character("")
      try container.encode(String(val), forKey: .value)
    case .null:
      try container.encode(CaseType.null, forKey: .type)
    case .date(let v):
      try container.encode(CaseType.date, forKey: .type)
      try container.encode(v, forKey: .value)
    case .data(let v):
      try container.encode(CaseType.data, forKey: .type)
      try container.encode(v, forKey: .value)
    }
  }

  public init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    let type = try container.decode(CaseType.self, forKey: .type)

    switch type {
    case .int8:
      self = .int8(try container.decode(Int8.self, forKey: .value))
    case .int16:
      self = .int16(try container.decode(Int16.self, forKey: .value))
    case .int:
      self = .int(try container.decode(Int.self, forKey: .value))
    case .int64:
      self = .int64(try container.decode(Int64.self, forKey: .value))
    case .int128:
      self = .int128(try container.decode(Int128.self, forKey: .value))
    case .uint8:
      self = .uint8(try container.decode(UInt8.self, forKey: .value))
    case .uint16:
      self = .uint16(try container.decode(UInt16.self, forKey: .value))
    case .uint:
      self = .uint(try container.decode(UInt.self, forKey: .value))
    case .uint64:
      self = .uint64(try container.decode(UInt64.self, forKey: .value))
    case .uint128:
      self = .uint128(try container.decode(UInt128.self, forKey: .value))
    case .f16:
      self = .f16(try container.decode(Float16.self, forKey: .value))
    case .float:
      self = .float(try container.decode(Float.self, forKey: .value))
    case .double:
      self = .double(try container.decode(Double.self, forKey: .value))
    case .bool:
      self = .bool(try container.decode(Bool.self, forKey: .value))
    case .string:
      self = .string(try container.decode(String.self, forKey: .value))
    case .char:
      let str = try container.decode(String.self, forKey: .value)
      guard let c = str.first, str.count == 1 else {
        throw DecodingError.dataCorruptedError(
          forKey: .value,
          in: container,
          debugDescription: "Invalid Character encoding"
        )
      }
      self = .char(c)
    case .null:
      self = .null
    case .date:
      self = .date(try container.decode(Date.self, forKey: .value))
    case .data:
      let base64 = try container.decode(String.self, forKey: .value)
      guard let d = Data(base64Encoded: base64) else {
        throw DecodingError.dataCorruptedError(
          forKey: .value,
          in: container,
          debugDescription: "Invalid base64 for Data"
        )
      }
      self = .data(d)
    }
  }
}

// MARK: - Path

public struct Path: Codable, Sendable {
  public var path: String
  public var genericArgs: [GenericArg]?
}

// MARK: - DynTrait

public struct DynTrait: Codable, Sendable {
  public var traits: [PolyTrait]
  public var lifetime: String?
}

// MARK: - FunctionPointer

public struct FunctionPointer: Codable, Sendable {
  public var inputs: [Parameter]?
  public var outputs: [Parameter]?
  public var genericParams: [TypeParam]?
  public var attributes: [FunctionAttributes]?
}

// MARK: - QualifiedPath

public struct QualifiedPath: Codable, Sendable {
  public var name: String
  public var genericArguments: [GenericArg]?
  public var selfType: Type
  public var trait: Path?  // optional, None if inherent
}

// MARK: - GenericArgs

public enum GenericArg: Codable, Sendable {
  case type(Type)
  case constExpr(ConstExpr)
  case lifetime(String)
}

// MARK: - GenericBound

public enum GenericBound: Codable, Sendable {
  case trait(TraitRef)
  case lifetime(String)
}

// MARK: - PolyTrait

public struct PolyTrait: Codable, Sendable {
  public var trait: TraitRef
  public var lifetimes: [String]
}

// MARK: Kind Types
public struct DocsFunction: Codable, Sendable {
  public let inputParameters: [Parameter]?
  public let outputParameters: [Parameter]?
  public let attributes: [FunctionAttributes]?
  public let generics: Generics?
  public let name: String
  /// Is there a function body?
  public let implemented: Bool
  public let visibility: Visibility?
}

public struct DocsRecord: Codable, Sendable {
  /// Optional name of the record (e.g., "User", "Point").
  /// Anonymous records (like tuples or JS objects) may omit this.
  public let name: String?

  /// Optional generic parameters (e.g., <T, U>).
  public let generics: [GenericArg]?

  /// The kind of record (named, tuple, unit, dynamic).
  public let kind: RecordKind

  /// The fields of the record (if applicable).
  public let fields: [RecordField]?

  /// The visibility of the record
  public let visibility: Visibility?
}

public enum Visibility: String, Codable, Sendable {
  /// Public symbols are accessible from any module or translation unit.
  /// This level of visibility indicates that the function signature is part
  /// of the module’s or library’s external interface and can be linked or
  /// invoked by external code.
  case `public`

  /// Private symbols are scoped only to the defining context (e.g., a class
  /// or compilation unit). These are used for implementation details that
  /// should not be accessible outside their immediate scope.
  case `private`

  /// Protected symbols are visible to the defining entity and any of its
  /// subclasses. This level is often used in object-oriented hierarchies
  /// where inherited customization is allowed but external access is not.
  case `protected`

  /// Internal symbols are visible within the same module but not exported
  /// outside it. This level supports encapsulation at the module level, such
  /// that functions can be shared internally without becoming part of the
  /// module’s public API.
  case `internal`

  /// Package-level visibility is used for symbols that should be accessible
  /// to other code in the same package or namespace, but not externally.
  /// This level is commonly seen in languages like Java and Kotlin and is
  /// useful when modeling multi-module projects within a single codebase.
  case package
}

public enum RecordKind: String, Codable, Sendable {
  /// A record with no fields (unit struct, empty object).
  case unit

  /// A record with ordered, positional fields (tuple, tuple struct).
  case tuple

  /// A record with named fields (struct, class, record, object).
  case named

  /// A record with dynamic/unknown fields (JS object, Python dict).
  case dynamic
}

public struct RecordField: Codable, Sendable {
  /// Field name (nil if tuple-like).
  public let name: String?

  /// Type of the field (if known).
  public let type: Type?

  /// The default value
  public let defaultValue: ConstExpr?

  /// Attributes on the field
  public let attributes: FieldAttribute?

  /// Visbility
  public let visibility: Visibility?
}

public enum FieldAttribute: Codable, Sendable {
  case mutable
  case optional
}

/// The various attributes a function can have
public enum FunctionAttributes: Codable, Sendable {
  /// Takes an arbitrary amount of arguments in the
  case variadic
  /// Determined at compile-time (i know this is wrong)
  case const
  /// No side-effects
  case pure
  /// Runs asynchrnously
  case `async`
  /// For rust, happens within an unsafe context
  case unsafe
}

/// Represents different kinds of code elements in a programming language or API.
public enum Kind: Sendable, Codable {
  /// A namespace, package, or module.
  case module

  /// Struct, class, record, or data class.
  case recordType(DocsRecord)

  /// Unlinked documentation
  case info

  /// Union type
  case unionType([Type])

  /// Enum, algebraic data type, discriminated union.
  case sumType([SumVariant])

  /// Trait, interface, abstract base class.
  case interfaceType

  /// Function, method, lambda (with metadata).
  case function(DocsFunction)

  /// Type alias, typedef, using alias.
  case typeAlias

  /// Constant or immutable global.
  case constant

  /// Mutable global/static variable.
  case variable

  /// Macro, template, codegen hook.
  case macro

  /// Built‑in primitive type.
  case primitiveType

  /// Field or property of a type.
  case field

  /// Event, signal, or callback definition.
  case event

  /// A human-readable description of the code element kind.
  public var description: String {
    switch self {
    case .module:
      return "A namespace, package, or module."
    case .info:
      return "A piece of unlinked documentation"
    case .recordType:
      return "Struct, class, record, or data class."
    case .unionType:
      return "Union type (C, C++, Rust, etc.)."
    case .sumType:
      return "Enum, algebraic data type, discriminated union."
    case .interfaceType:
      return "Trait, interface, abstract base class."
    case .function:
      return "Function, method, lambda (with metadata)."
    case .typeAlias:
      return "Type alias, typedef, using alias."
    case .constant:
      return "Constant or immutable global."
    case .variable:
      return "Mutable global/static variable."
    case .macro:
      return "Macro, template, codegen hook."
    case .primitiveType:
      return "Built‑in primitive type."
    case .field:
      return "Field or property of a type."
    case .event:
      return "Event, signal, or callback definition."
    }
  }

  // /// Attempts to create a `CodeElementKind` from a case-insensitive string.
  // public static func from(_ raw: String) -> Kind? {
  //   return self.allCases.first { $0.rawValue.lowercased() == raw.lowercased() }
  // }
}

extension Kind: CustomStringConvertible {}
