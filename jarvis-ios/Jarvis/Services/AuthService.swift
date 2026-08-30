import Foundation

enum AuthServiceOutcome: Equatable {
    case needsEnrollment
    case awaitingApproval(expiresAt: Date)
    case authenticated
    case signedOut
}

actor AuthService {
    private let api: JarvisAPIClient
    private let identity: DeviceIdentityStore
    private let credentials: SecureCredentialStore

    init(
        api: JarvisAPIClient,
        identity: DeviceIdentityStore = DeviceIdentityStore(),
        credentials: SecureCredentialStore = SecureCredentialStore()
    ) {
        self.api = api
        self.identity = identity
        self.credentials = credentials
    }

    func restore() async throws -> AuthServiceOutcome {
        if let session = try await credentials.session() {
            do {
                let _: AuthenticatedIdentity = try await api.get("/v1/auth/me", token: session.token)
                return .authenticated
            } catch JarvisAPIError.unauthorized {
                try await credentials.clearSession()
            }
        }
        if let deviceId = try await credentials.deviceId() {
            try await login(deviceId: deviceId)
            return .authenticated
        }
        if let ticket = try await credentials.pairingTicket() {
            return try await poll(ticket: ticket)
        }
        return .needsEnrollment
    }

    func requestEnrollment(deviceName: String) async throws -> AuthServiceOutcome {
        if let ticket = try await credentials.pairingTicket() {
            return try await poll(ticket: ticket)
        }
        let request = EnrollmentRequest(
            name: deviceName.prefix(128).description,
            platform: "ios",
            publicKey: try await identity.publicKeyHex()
        )
        let response: PairingRequestResponse = try await api.post(
            "/v1/auth/pairing/requests",
            body: request
        )
        let ticket = PairingTicket(
            requestId: response.requestId,
            nonce: response.nonce,
            expiresAt: response.expiresAt
        )
        try await credentials.save(pairingTicket: ticket)
        return .awaitingApproval(expiresAt: Date(timeIntervalSince1970: TimeInterval(ticket.expiresAt)))
    }

    func refreshEnrollment() async throws -> AuthServiceOutcome {
        guard let ticket = try await credentials.pairingTicket() else { return .needsEnrollment }
        return try await poll(ticket: ticket)
    }

    func sessionToken() async throws -> String? { try await credentials.session()?.token }

    func requiresLocalUnlock() async throws -> Bool {
        let hasSession = try await credentials.session() != nil
        let hasRegisteredDevice = try await credentials.deviceId() != nil
        return hasSession || hasRegisteredDevice
    }

    func logout() async throws -> AuthServiceOutcome {
        if let token = try await credentials.session()?.token {
            let _: StatusResponse? = try? await api.post(
                "/v1/auth/logout",
                token: token,
                response: StatusResponse.self
            )
        }
        try await credentials.clearSession()
        return .signedOut
    }

    func resetDevice() async throws {
        let session = try await credentials.session()
        let deviceId = try await credentials.deviceId()
        if let token = session?.token, let deviceId {
            try? await api.delete("/v1/devices/\(deviceId.uuidString)", token: token)
        }
        // Local erasure is authoritative even if the Home Node is offline.
        var resetError: Error?
        do { try await credentials.reset() }
        catch { resetError = error }
        do { try await identity.reset() }
        catch { if resetError == nil { resetError = error } }
        if let resetError { throw resetError }
    }

    private func poll(ticket: PairingTicket) async throws -> AuthServiceOutcome {
        guard ticket.expiresAt > Int64(Date().timeIntervalSince1970) else {
            try await credentials.clearPairingTicket()
            return .needsEnrollment
        }
        let status: PairingStatusResponse = try await api.get(
            "/v1/auth/pairing/requests/\(ticket.requestId.uuidString)/status",
            headers: ["X-Jarvis-Pairing-Nonce": ticket.nonce]
        )
        switch status.status {
        case .pending:
            return .awaitingApproval(expiresAt: Date(timeIntervalSince1970: TimeInterval(ticket.expiresAt)))
        case .approved:
            guard let deviceId = status.deviceId else { throw JarvisAPIError.invalidResponse }
            try await credentials.save(deviceId: deviceId)
            try await credentials.clearPairingTicket()
            try await login(deviceId: deviceId)
            return .authenticated
        case .denied, .expired:
            try await credentials.clearPairingTicket()
            return .needsEnrollment
        }
    }

    private func login(deviceId: UUID) async throws {
        let challenge: ChallengeResponse = try await api.post(
            "/v1/auth/challenge",
            body: ChallengeRequest(deviceId: deviceId)
        )
        let signature = try await identity.signChallenge(hex: challenge.nonce)
        let response: LoginResponse = try await api.post(
            "/v1/auth/login",
            body: LoginRequest(
                deviceId: deviceId,
                challengeId: challenge.challengeId,
                signature: signature
            )
        )
        try await credentials.save(session: SecureSession(token: response.token, expiresAt: response.expiresAt))
    }
}

private struct StatusResponse: Decodable { let status: String }
