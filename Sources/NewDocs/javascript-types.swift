// Represents a single documented symbol (function, module, property, etc.).
struct DocItem: Codable {
  let description: DescriptionNode
  let tags: [Tag]
  let loc: Location
  let context: Context
  let name: String
  let namespace: String
  let path: [PathSegment]
  let members: Members

  // Optional properties that may not exist on all items
  let kind: ItemKind?
  let memberof: String?
  let scope: ItemScope?
  let augments: [String]? // Assuming string, adjust if structure is different
  let examples: [String]? // Assuming string, adjust if structure is different
  let implements: [String]? // Assuming string, adjust if structure is different
  let params: [Parameter]?
  let properties: [Parameter]? // Properties often share the same structure as params
  let returns: [ReturnValue]?
  let sees: [String]? // Assuming string, adjust if structure is different
  let `throws`: [String]? // Assuming string, adjust if structure is different
  let todos: [String]? // Assuming string, adjust if structure is different
  let yields: [String]? // Assuming string, adjust if structure is different
  let access: String? // e.g., "public"
}

// Represents the structured description block, which is like a Markdown AST.
struct DescriptionNode: Codable {
  let type: String // e.g., "root", "paragraph", "text", "inlineCode", "code", "link"
  let children: [DescriptionNode]?
  let value: String?
  let url: String?
  let title: String?
  let lang: String?
  let meta: String?
}

// Represents a JSDoc-style tag, like @param or @return.
struct Tag: Codable {
  let title: String
  let description: String?
  let lineNumber: Int?
  let type: TypeInfo?
  let name: String?
}

// Describes the type of a parameter, return value, or property.
struct TypeInfo: Codable {
  let type: String // e.g., "NameExpression"
  let name: String // e.g., "Function", "String"
}

// Represents a file location with start and end positions.
struct Location: Codable {
  let start: Position
  let end: Position
}

// A specific position in a file.
struct Position: Codable {
  let line: Int
  let column: Int
  let index: Int
}

// Provides context for where the documented item was found.
struct Context: Codable {
  let loc: Location
  let file: String
}

// Represents a segment in the namespace path of an item.
struct PathSegment: Codable {
  let name: String
  let kind: ItemKind?
  let scope: ItemScope?
}

// The kind of the documented symbol.
enum ItemKind: String, Codable {
  case function
  // Add other kinds as they appear in your data
}

// The scope of the documented symbol.
enum ItemScope: String, Codable {
  case `static`
  case instance
  case inner
  // Add other scopes as needed
}

// A container for members of a namespace or object.
// This is recursive, as members are themselves DocItems.
struct Members: Codable {
  let global: [DocItem]?
  let inner: [DocItem]?
  let instance: [DocItem]?
  let events: [DocItem]?
  let `static`: [DocItem]?
}

// A processed parameter, often derived from an @param tag.
struct Parameter: Codable {
  let name: String
  let type: TypeInfo?
  let description: String?
  let title: String? // e.g. "param"
  let lineNumber: Int?
}

// A processed return value, often derived from an @return tag.
struct ReturnValue: Codable {
  let type: TypeInfo?
  // Description for returns can be a simple string or a structured node.
  // This model assumes a structured node is possible but not guaranteed.
  let description: DescriptionNode?
}
