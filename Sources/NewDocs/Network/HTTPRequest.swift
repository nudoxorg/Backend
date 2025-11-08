import Alamofire
import Foundation
import Logging

public protocol HTTPRequesting {
  func request(
    _ url: String,
    method: HTTPMethod,
    parameters: [String: Sendable]?,
    headers: HTTPHeaders?
  ) async throws -> HTTPResponse
}

extension HTTPRequesting {
  public func request(_ url: String) async throws -> HTTPResponse {
    return try await request(url, method: .get, parameters: nil, headers: nil)
  }
}

public struct HTTPRequest: HTTPRequesting, Instrumentable {
  public let logger: Logger
  private let session: Session

  public init(logger: Logger = Logger(label: "HTTPRequest")) {
    self.logger = logger
    self.session = Session()
  }

  public func request(
    _ url: String,
    method: HTTPMethod = .get,
    parameters: [String: Sendable]? = nil,
    headers: HTTPHeaders? = nil
  ) async throws -> HTTPResponse {
    try await instrument("http_request", metadata: ["url": url]) {
      // Capture parameters in a synchronous context
      let capturedParams = parameters

      return try await withCheckedThrowingContinuation { continuation in
        session.request(url, method: method, parameters: capturedParams, headers: headers)
          .responseData { response in
            switch response.result {
            case .success(let data):
              continuation.resume(
                returning: HTTPResponse(
                  url: response.request?.url?.absoluteString ?? url,
                  statusCode: response.response?.statusCode ?? 0,
                  headers: response.response?.allHeaderFields as? [String: String] ?? [:],
                  data: data
                ))
            case .failure(let error):
              continuation.resume(throwing: NewDocsError.networkError(error.localizedDescription))
            }
          }
      }
    }
  }
}

public struct HTTPResponse {
  public let url: String
  public let statusCode: Int
  public let headers: [String: String]
  public let data: Data

  public var body: String {
    return String(data: data, encoding: .utf8) ?? ""
  }

  public var isSuccess: Bool {
    return statusCode == 200
  }

  public var isError: Bool {
    return statusCode == 0
      || (statusCode >= 400 && statusCode <= 599 && statusCode != 404 && statusCode != 403)
  }

  public var isEmpty: Bool {
    return data.isEmpty
  }

  public var contentLength: Int {
    return Int(headers["Content-Length"] ?? "0") ?? 0
  }

  public var mimeType: String {
    return headers["Content-Type"] ?? "text/plain"
  }

  public var isHTML: Bool {
    return mimeType.contains("html")
  }

  public func asJSON() throws -> [String: Any] {
    return try JSONSerialization.jsonObject(with: data) as? [String: Any] ?? [:]
  }
}
