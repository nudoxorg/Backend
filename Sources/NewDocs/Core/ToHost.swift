import Foundation
import SwiftGitX

/// Returns the JSON representation of an inputted git repository, as a string
public func getJSON(source: URL, command: Process, output_location: URL) async throws -> String {
  let repo = try await Repository.clone(from: source, to: URL(string: "out")!)

  let outputPipe = Pipe()
  let errorPipe = Pipe()
  command.standardOutput = outputPipe
  command.standardError = errorPipe

  var output: String?
  do {
    try command.run()
    command.waitUntilExit()

    let outputData = outputPipe.fileHandleForReading.readDataToEndOfFile()
    output = String(data: outputData, encoding: .utf8)

    let errorData = errorPipe.fileHandleForReading.readDataToEndOfFile()

  } catch {
    // This catch handles errors from process.run() itself,
    // not necessarily the command failing internally.
    print("Failed to run command: \(error.localizedDescription)")
  }

  return ""
}
