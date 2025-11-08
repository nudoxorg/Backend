public struct Entry: Codable, Hashable {
  // Required
  public let name: String // Semantic name for the entry (std::time, or to_string)
  public let path: [String] // The absolute path leading to the first instance of this entry
  public let kind: Kind // The kind of entry this is
  public let visibility: String? // The visibility of this entry (public, private, flags?)

  public let documentation: String? // The associated documentation

  // Added missing properties
  public let members: [String]?
  public let inputParameters: [Parameter]?
  public let outputParameters: [Parameter]?
  public let typeParameters: [String]?

  public init(
    path: [String],
    kind: Kind,
    visibility: String? = nil,
    members: [String]? = nil,
    inputParameters: [Parameter]? = nil,
    outputParameters: [Parameter]? = nil,
    typeParameters: [String]? = nil,
    documentation: String? = nil,
    name: String
  ) throws {
    let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !trimmedName.isEmpty else {
      throw NewDocsError.invalidEntry("missing name")
    }

    self.path = path
    self.kind = kind
    self.visibility = visibility
    self.documentation = documentation
    self.name = trimmedName
    self.members = members // Assigning the new property
    self.inputParameters = inputParameters // Assigning the new property
    self.outputParameters = outputParameters // Assigning the new property
    self.typeParameters = typeParameters // Assigning the new property
  }
}
