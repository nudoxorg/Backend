import Foundation
import SwiftGitX

/// Returns the JSON representation of an inputted git repository, after running the specified command, as a string
public func getJSON(source: URL, command: Process, output_location: URL) async throws -> String {
  let out_dir = URL(string: "out")!
  let repo = try await Repository.clone(from: source, to: out_dir)

  let outputPipe = Pipe()
  let errorPipe = Pipe()
  command.standardOutput = outputPipe
  command.standardError = errorPipe
  command.currentDirectoryURL = out_dir

  var output: String?
  do {
    try command.run()
    command.waitUntilExit()

    let outputData = outputPipe.fileHandleForReading.readDataToEndOfFile()
    output = String(data: outputData, encoding: .utf8)

    return try String(contentsOf: output_location, encoding: .utf8)

    let errorData = errorPipe.fileHandleForReading.readDataToEndOfFile()

  } catch {
    // This catch handles errors from process.run() itself,
    // not necessarily the command failing internally.
    print("Failed to run command: \(error.localizedDescription)")
  }

  return ""
}
