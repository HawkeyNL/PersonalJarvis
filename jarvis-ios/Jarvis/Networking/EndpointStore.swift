import Foundation

enum EndpointValidationError: LocalizedError, Equatable {
    case empty
    case invalid
    case unsupportedScheme
    case containsCredentials
    case insecureRemote

    var errorDescription: String? {
        switch self {
        case .empty: "Enter the address of your Home Node."
        case .invalid: "Enter a valid Home Node URL."
        case .unsupportedScheme: "The Home Node URL must use http or https."
        case .containsCredentials: "Do not put credentials in the Home Node URL."
        case .insecureRemote: "Public or remote Home Nodes must use https."
        }
    }
}

struct EndpointNormalizer {
    static func normalize(_ input: String) throws -> URL {
        let trimmed = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { throw EndpointValidationError.empty }

        let candidate = trimmed.contains("://") ? trimmed : "https://\(trimmed)"
        guard var components = URLComponents(string: candidate),
              let scheme = components.scheme?.lowercased(),
              let host = components.host, !host.isEmpty else {
            throw EndpointValidationError.invalid
        }
        guard scheme == "http" || scheme == "https" else {
            throw EndpointValidationError.unsupportedScheme
        }
        guard components.user == nil, components.password == nil else {
            throw EndpointValidationError.containsCredentials
        }
        guard scheme == "https" || isLocalHost(host) else {
            throw EndpointValidationError.insecureRemote
        }
        guard components.path.isEmpty || components.path == "/" else {
            throw EndpointValidationError.invalid
        }
        components.scheme = scheme
        components.path = ""
        components.query = nil
        components.fragment = nil
        guard let url = components.url else { throw EndpointValidationError.invalid }
        return url
    }

    private static func isLocalHost(_ host: String) -> Bool {
        let host = host.lowercased()
        if host == "localhost" || host.hasSuffix(".local") || host == "::1" {
            return true
        }
        if host.contains(":"),
           host.hasPrefix("fe80:") || host.hasPrefix("fc") || host.hasPrefix("fd") {
            return true
        }
        let octets = host.split(separator: ".").compactMap { Int($0) }
        guard octets.count == 4, octets.allSatisfy({ (0...255).contains($0) }) else {
            return false
        }
        return octets[0] == 10
            || octets[0] == 127
            || (octets[0] == 169 && octets[1] == 254)
            || (octets[0] == 172 && (16...31).contains(octets[1]))
            || (octets[0] == 192 && octets[1] == 168)
    }
}

final class EndpointStore {
    private let defaults: UserDefaults
    private let key = "homeNodeEndpoint"

    init(defaults: UserDefaults = .standard) { self.defaults = defaults }

    var endpoint: URL? {
        guard let value = defaults.string(forKey: key) else { return nil }
        return try? EndpointNormalizer.normalize(value)
    }

    func save(_ endpoint: URL) {
        defaults.set(endpoint.absoluteString, forKey: key)
    }
}
