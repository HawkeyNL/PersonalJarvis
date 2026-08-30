import Foundation

enum ConnectionState: Equatable {
    case unconfigured
    case checking
    case reachable
    case unreachable(String)
}

enum EnrollmentState: Equatable {
    case notStarted
    case requesting
    case awaitingApproval(expiresAt: Date)
    case authenticating
    case authenticated
    case signedOut
    case failed(String)
}

enum AppLockState: Equatable {
    case unlocked
    case locked
    case unavailable(String)
    case denied
}

struct PairingTicket: Codable, Equatable {
    let requestId: UUID
    let nonce: String
    let expiresAt: Int64
}

struct SecureSession: Codable, Equatable {
    let token: String
    let expiresAt: Int64
}
