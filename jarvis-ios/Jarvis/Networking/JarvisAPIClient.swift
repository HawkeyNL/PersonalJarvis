import Foundation

enum JarvisAPIError: LocalizedError, Equatable {
    case invalidConfiguration
    case unreachable
    case timedOut
    case unauthorized
    case rejected(status: Int, message: String?)
    case invalidResponse

    var errorDescription: String? {
        switch self {
        case .invalidConfiguration: "The Home Node address is not configured."
        case .unreachable: "The Home Node could not be reached. Check its address and your network."
        case .timedOut: "The Home Node did not respond in time."
        case .unauthorized: "This session is no longer authorized."
        case let .rejected(status, message): message ?? "The Home Node rejected the request (HTTP \(status))."
        case .invalidResponse: "The Home Node returned an unexpected response."
        }
    }
}

actor JarvisAPIClient {
    private var baseURL: URL?
    private let session: URLSession
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    init(baseURL: URL? = nil, session: URLSession? = nil) {
        self.baseURL = baseURL
        if let session {
            self.session = session
        } else {
            let configuration = URLSessionConfiguration.ephemeral
            configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
            configuration.urlCache = nil
            configuration.urlCredentialStorage = nil
            configuration.httpCookieStorage = nil
            configuration.httpShouldSetCookies = false
            configuration.timeoutIntervalForRequest = 15
            configuration.timeoutIntervalForResource = 30
            self.session = URLSession(configuration: configuration)
        }
    }

    func configure(baseURL: URL) { self.baseURL = baseURL }

    func checkReadiness() async throws {
        _ = try await request(path: "/readyz", method: "GET", response: EmptyOrJSON.self)
    }

    func get<Response: Decodable>(
        _ path: String,
        token: String? = nil,
        headers: [String: String] = [:],
        response: Response.Type = Response.self
    ) async throws -> Response {
        try await request(path: path, method: "GET", token: token, headers: headers, response: response)
    }

    func post<Body: Encodable, Response: Decodable>(
        _ path: String,
        body: Body,
        token: String? = nil,
        response: Response.Type = Response.self
    ) async throws -> Response {
        try await request(path: path, method: "POST", body: body, token: token, response: response)
    }

    func post<Response: Decodable>(
        _ path: String,
        token: String? = nil,
        response: Response.Type = Response.self
    ) async throws -> Response {
        try await request(path: path, method: "POST", token: token, response: response)
    }

    func delete(_ path: String, token: String) async throws {
        _ = try await request(path: path, method: "DELETE", token: token, response: EmptyOrJSON.self)
    }

    private func request<Response: Decodable>(
        path: String,
        method: String,
        body: (any Encodable)? = nil,
        token: String? = nil,
        headers: [String: String] = [:],
        response: Response.Type
    ) async throws -> Response {
        guard let baseURL else { throw JarvisAPIError.invalidConfiguration }
        guard let url = URL(string: path, relativeTo: baseURL)?.absoluteURL else {
            throw JarvisAPIError.invalidConfiguration
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let body {
            request.httpBody = try encoder.encode(AnyEncodable(body))
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        if let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        headers.forEach { request.setValue($1, forHTTPHeaderField: $0) }

        do {
            let (data, rawResponse) = try await session.data(for: request)
            guard let http = rawResponse as? HTTPURLResponse else { throw JarvisAPIError.invalidResponse }
            guard (200..<300).contains(http.statusCode) else {
                if http.statusCode == 401 { throw JarvisAPIError.unauthorized }
                let body = try? decoder.decode(APIErrorBody.self, from: data)
                throw JarvisAPIError.rejected(status: http.statusCode, message: body?.error)
            }
            if Response.self == EmptyOrJSON.self, data.isEmpty {
                return EmptyOrJSON() as! Response
            }
            return try decoder.decode(Response.self, from: data)
        } catch let error as JarvisAPIError {
            throw error
        } catch let error as URLError where error.code == .timedOut {
            throw JarvisAPIError.timedOut
        } catch is DecodingError {
            throw JarvisAPIError.invalidResponse
        } catch {
            throw JarvisAPIError.unreachable
        }
    }
}

private struct EmptyOrJSON: Codable {}

private struct AnyEncodable: Encodable {
    private let encodeValue: (Encoder) throws -> Void
    init(_ value: any Encodable) { encodeValue = { try value.encode(to: $0) } }
    func encode(to encoder: Encoder) throws { try encodeValue(encoder) }
}
