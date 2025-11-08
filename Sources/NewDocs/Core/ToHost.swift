import Foundation
import SwiftGitX

/// Returns the JSON representation of an inputted git repository, after running the specified command, as a string
public func getJSON(source: URL, command: Process, output_location: URL) async throws -> Data {
  let out_dir = URL(
    fileURLWithPath: "out",
    relativeTo: URL(fileURLWithPath: FileManager.default.currentDirectoryPath))
  let repo = try await Repository.clone(from: source, to: out_dir)

  let outputPipe = Pipe()
  let errorPipe = Pipe()
  command.standardOutput = outputPipe
  command.standardError = errorPipe
  command.currentDirectoryURL = out_dir

  print("Running command: \(command)")
  print("In directory: \(out_dir.path)")

  try command.run()

  // Read pipes asynchronously to avoid deadlocks
  let outputData = outputPipe.fileHandleForReading.readDataToEndOfFile()
  let errorData = errorPipe.fileHandleForReading.readDataToEndOfFile()

  command.waitUntilExit()

  let output = String(data: outputData, encoding: .utf8) ?? ""
  let errorOutput = String(data: errorData, encoding: .utf8) ?? ""

  if command.terminationStatus != 0 {
    print("Command failed with status: \(command.terminationStatus)")
    print("Error output: \(errorOutput)")
    throw NSError(
      domain: "CommandError", code: Int(command.terminationStatus),
      userInfo: [NSLocalizedDescriptionKey: errorOutput])
  }

  print("Command output: \(output)")
  print("Looking for output at: \(output_location.path)")

  // Check if file exists before reading
  guard FileManager.default.fileExists(atPath: output_location.path) else {
    throw NSError(
      domain: "FileError", code: 404,
      userInfo: [NSLocalizedDescriptionKey: "Output file not found at \(output_location.path)"])
  }

  return try Data(contentsOf: output_location)
}
