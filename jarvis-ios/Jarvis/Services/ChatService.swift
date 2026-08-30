import Foundation

actor ChatService {
    private let api: JarvisAPIClient
    private let auth: AuthService

    init(api: JarvisAPIClient, auth: AuthService) {
        self.api = api
        self.auth = auth
    }

    func conversations() async throws -> [ConversationSummary] {
        let token = try await requiredToken()
        let response: ConversationListResponse = try await api.get("/v1/conversations", token: token)
        return response.conversations
    }

    func conversation(id: UUID) async throws -> ConversationResponse {
        let token = try await requiredToken()
        return try await api.get("/v1/conversations/\(id.uuidString)", token: token)
    }

    func send(text: String, conversationId: UUID?, history: [ConversationMessage]) async throws -> ChatResponse {
        let token = try await requiredToken()
        let turns = history.suffix(19).map {
            ChatTurn(role: $0.isAssistant ? "assistant" : "user", content: $0.content)
        } + [ChatTurn(role: "user", content: text)]
        return try await api.post(
            "/v1/assistant/chat",
            body: ChatRequest(messages: turns, conversationId: conversationId),
            token: token
        )
    }

    private func requiredToken() async throws -> String {
        guard let token = try await auth.sessionToken() else { throw JarvisAPIError.unauthorized }
        return token
    }
}
