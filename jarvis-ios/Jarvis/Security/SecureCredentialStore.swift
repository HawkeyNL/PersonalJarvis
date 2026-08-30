import Foundation

actor SecureCredentialStore {
    private let keychain: KeychainStore
    private let deviceAccount = "registered-device-id-v1"
    private let sessionAccount = "authenticated-session-v1"
    private let pairingAccount = "pending-pairing-v1"
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    init(keychain: KeychainStore = KeychainStore()) { self.keychain = keychain }

    func session() throws -> SecureSession? {
        guard let data = try keychain.read(account: sessionAccount) else { return nil }
        return try decoder.decode(SecureSession.self, from: data)
    }

    func save(session: SecureSession) throws {
        try keychain.save(encoder.encode(session), account: sessionAccount)
    }

    func clearSession() throws { try keychain.delete(account: sessionAccount) }

    func deviceId() throws -> UUID? {
        guard let data = try keychain.read(account: deviceAccount),
              let value = String(data: data, encoding: .utf8),
              let id = UUID(uuidString: value) else { return nil }
        return id
    }

    func save(deviceId: UUID) throws {
        guard let data = deviceId.uuidString.data(using: .utf8) else {
            throw KeychainError.invalidData
        }
        try keychain.save(data, account: deviceAccount)
    }

    func pairingTicket() throws -> PairingTicket? {
        guard let data = try keychain.read(account: pairingAccount) else { return nil }
        return try decoder.decode(PairingTicket.self, from: data)
    }

    func save(pairingTicket: PairingTicket) throws {
        try keychain.save(encoder.encode(pairingTicket), account: pairingAccount)
    }

    func clearPairingTicket() throws { try keychain.delete(account: pairingAccount) }

    func reset() throws {
        var firstError: Error?
        for account in [sessionAccount, pairingAccount, deviceAccount] {
            do { try keychain.delete(account: account) }
            catch { if firstError == nil { firstError = error } }
        }
        if let firstError { throw firstError }
    }
}
