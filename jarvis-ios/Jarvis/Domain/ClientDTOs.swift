import Foundation

// Client-local mirrors of the current v1 API. Replace these with generated or
// shared contract types once Jarvis publishes an authoritative mobile schema.

struct EnrollmentRequest: Encodable, Equatable {
    let name: String
    let platform: String
    let publicKey: String

    enum CodingKeys: String, CodingKey {
        case name, platform
        case publicKey = "public_key"
    }
}

struct PairingRequestResponse: Decodable, Equatable {
    let requestId: UUID
    let nonce: String
    let expiresAt: Int64

    enum CodingKeys: String, CodingKey {
        case requestId = "request_id"
        case nonce
        case expiresAt = "expires_at"
    }
}

struct PairingStatusResponse: Decodable, Equatable {
    enum Status: String, Decodable, Equatable {
        case pending, approved, denied, expired
    }

    let status: Status
    let deviceId: UUID?

    enum CodingKeys: String, CodingKey {
        case status
        case deviceId = "device_id"
    }
}

struct ChallengeRequest: Encodable, Equatable {
    let deviceId: UUID

    enum CodingKeys: String, CodingKey { case deviceId = "device_id" }
}

struct ChallengeResponse: Decodable, Equatable {
    let challengeId: UUID
    let nonce: String

    enum CodingKeys: String, CodingKey {
        case challengeId = "challenge_id"
        case nonce
    }
}

struct LoginRequest: Encodable, Equatable {
    let deviceId: UUID
    let challengeId: UUID
    let signature: String

    enum CodingKeys: String, CodingKey {
        case deviceId = "device_id"
        case challengeId = "challenge_id"
        case signature
    }
}

struct LoginResponse: Decodable, Equatable {
    let token: String
    let expiresAt: Int64

    enum CodingKeys: String, CodingKey {
        case token
        case expiresAt = "expires_at"
    }
}

struct AuthenticatedIdentity: Decodable, Equatable {
    let userId: UUID
    let deviceId: UUID

    enum CodingKeys: String, CodingKey {
        case userId = "user_id"
        case deviceId = "device_id"
    }
}

struct ConversationSummary: Decodable, Identifiable, Equatable {
    let id: UUID
    let title: String
    let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case id, title
        case updatedAt = "updated_at"
    }
}

struct ConversationListResponse: Decodable, Equatable {
    let conversations: [ConversationSummary]
}

struct ConversationMessage: Decodable, Identifiable, Equatable {
    let role: String
    let content: String
    let model: String?
    let at: String

    var id: String { "\(at)-\(role)-\(content)" }
    var isAssistant: Bool { role == "assistant" || role == "jarvis" }
}

struct ConversationResponse: Decodable, Equatable {
    let id: UUID
    let title: String
    let messages: [ConversationMessage]
}

struct ChatTurn: Encodable, Equatable {
    let role: String
    let content: String
}

struct ChatRequest: Encodable, Equatable {
    let messages: [ChatTurn]
    let conversationId: UUID?

    enum CodingKeys: String, CodingKey {
        case messages
        case conversationId = "conversation_id"
    }
}

struct ChatResponse: Decodable, Equatable {
    let reply: String
    let model: String?
    let stopReason: String?
    let conversationId: UUID
    let conversationTitle: String
    let newTopic: Bool

    enum CodingKeys: String, CodingKey {
        case reply, model
        case stopReason = "stop_reason"
        case conversationId = "conversation_id"
        case conversationTitle = "conversation_title"
        case newTopic = "new_topic"
    }
}

struct APIErrorBody: Decodable {
    let error: String?
    let hint: String?
}
