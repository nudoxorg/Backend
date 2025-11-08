import Foundation
import SemVer
import libzstd

public struct NewDocumentations {
  public func registry(for language: Language) -> PackageRegistry {
    switch language {
    case .Rust:
      return CargoRegistry()
    default:
      fatalError("Language \(language) not yet supported")
    }
  }

  public func buildPackage(
    package: Package,
    version: Version,
    features: [String] = [],
  ) async throws -> Documentation {
    try await package.retrieve(at: version, flags: features)
  }
}

func decompress_zstd(from compressedData: Data) async throws -> Data {
  // Step 2: Create a decompression context
  guard let dctx = ZSTD_createDCtx() else {
    throw NSError(
      domain: "ZstdError", code: 10,
      userInfo: [
        NSLocalizedDescriptionKey: "Failed to create decompression context"
      ])
  }
  defer { ZSTD_freeDCtx(dctx) }

  // Step 3: Prepare buffers
  var output = Data()
  let chunkSize = 16384  // 16 KB output buffer
  var inputPos = 0

  // Step 4: Stream loop
  while inputPos < compressedData.count {
    var outBuffer = [UInt8](repeating: 0, count: chunkSize)

    var input = ZSTD_inBuffer(
      src: compressedData.withUnsafeBytes { $0.baseAddress! + inputPos },
      size: compressedData.count - inputPos,
      pos: 0
    )

    var outputBuf = ZSTD_outBuffer(
      dst: &outBuffer,
      size: chunkSize,
      pos: 0
    )

    let ret = ZSTD_decompressStream(dctx, &outputBuf, &input)

    if ZSTD_isError(ret) != 0 {
      let errMsg = String(cString: ZSTD_getErrorName(ret))
      throw NSError(
        domain: "ZstdError", code: 11,
        userInfo: [
          NSLocalizedDescriptionKey: "Streaming decompression failed: \(errMsg)"
        ])
    }

    // Append decompressed bytes
    output.append(outBuffer, count: outputBuf.pos)

    // Advance input position
    inputPos += input.pos

    // If ret == 0, frame is done
    if ret == 0 {
      break
    }
  }

  return output
}
