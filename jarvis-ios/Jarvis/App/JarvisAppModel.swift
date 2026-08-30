import Combine
import Foundation
import UIKit

@MainActor
final class JarvisAppModel: ObservableObject {
    @Published private(set) var connectionState: ConnectionState = .unconfigured
    @Published private(set) var enrollmentState: EnrollmentState = .notStarted
    @Published private(set) var lockState: AppLockState = .unlocked
    @Published private(set) var conversations: [ConversationSummary] = []
    @Published private(set) var messages: [ConversationMessage] = []
    @Published private(set) var currentConversationId: UUID?
    @Published private(set) var currentConversationTitle = "New conversation"
    @Published private(set) var isSending = false
    @Published var endpointText: String
    @Published var notice: String?

    private let endpointStore: EndpointStore
    private let api: JarvisAPIClient
    private let auth: AuthService
    private let chat: ChatService
    private let biometricLock: BiometricLock

    init() {
        let endpointStore = EndpointStore()
        let api = JarvisAPIClient(baseURL: endpointStore.endpoint)
        let auth = AuthService(api: api)
        self.endpointStore = endpointStore
        self.api = api
        self.auth = auth
        self.chat = ChatService(api: api, auth: auth)
        self.biometricLock = BiometricLock()
        self.endpointText = endpointStore.endpoint?.absoluteString ?? ""
    }

    var isAuthenticated: Bool { enrollmentState == .authenticated }

    func start() async {
        guard let endpoint = endpointStore.endpoint else {
            connectionState = .unconfigured
            return
        }
        await api.configure(baseURL: endpoint)
        guard await checkConnection() else { return }
        do {
            if try await auth.requiresLocalUnlock() {
                lockState = .locked
                await unlock()
            } else {
                await restoreAuthentication()
            }
        } catch {
            enrollmentState = .failed(safeMessage(error))
        }
    }

    @discardableResult
    func saveEndpoint() async -> Bool {
        do {
            let endpoint = try EndpointNormalizer.normalize(endpointText)
            endpointStore.save(endpoint)
            endpointText = endpoint.absoluteString
            await api.configure(baseURL: endpoint)
            notice = nil
            guard await checkConnection() else { return false }
            if try await auth.requiresLocalUnlock() {
                lockState = .locked
                await unlock()
            } else {
                await restoreAuthentication()
            }
            return true
        } catch {
            connectionState = .unreachable(safeMessage(error))
            return false
        }
    }

    @discardableResult
    func checkConnection() async -> Bool {
        connectionState = .checking
        do {
            try await api.checkReadiness()
            connectionState = .reachable
            return true
        } catch {
            connectionState = .unreachable(safeMessage(error))
            return false
        }
    }

    func requestEnrollment() async {
        enrollmentState = .requesting
        do {
            apply(awaitResult: try await auth.requestEnrollment(deviceName: UIDevice.current.name))
        }
        catch { handle(error) }
    }

    func refreshEnrollment() async {
        enrollmentState = .authenticating
        do {
            let result = try await auth.refreshEnrollment()
            apply(awaitResult: result)
            if result == .authenticated {
                lockState = .locked
                await unlock()
            }
        } catch { handle(error) }
    }

    func restoreAuthentication() async {
        enrollmentState = .authenticating
        do {
            let result = try await auth.restore()
            apply(awaitResult: result)
            if result == .authenticated { await loadConversations() }
        } catch { handle(error) }
    }

    func unlock() async {
        let result = await biometricLock.unlock(reason: "Unlock your Jarvis conversations")
        switch result {
        case .unlocked:
            lockState = .unlocked
            await restoreAuthentication()
        case .cancelled:
            lockState = .locked
        case .denied:
            lockState = .denied
        case let .unavailable(message):
            lockState = .unavailable(message)
        }
    }

    func beginSignIn() async {
        do {
            if try await auth.requiresLocalUnlock() {
                lockState = .locked
                await unlock()
            } else {
                await restoreAuthentication()
            }
        } catch { handle(error) }
    }

    func lockWhenBackgrounded() {
        if isAuthenticated { lockState = .locked }
    }

    func logout() async {
        do {
            apply(awaitResult: try await auth.logout())
            messages = []
            conversations = []
        } catch { handle(error) }
    }

    func resetDevice() async {
        do {
            try await auth.resetDevice()
            enrollmentState = .notStarted
            lockState = .unlocked
            messages = []
            conversations = []
            currentConversationId = nil
            notice = "This device identity was removed locally. Request enrollment to create a new one."
        } catch { handle(error) }
    }

    func loadConversations() async {
        guard isAuthenticated, lockState == .unlocked else { return }
        do {
            conversations = try await chat.conversations()
            if let id = currentConversationId ?? conversations.first?.id {
                await openConversation(id)
            }
        } catch { handle(error) }
    }

    func openConversation(_ id: UUID) async {
        do {
            let conversation = try await chat.conversation(id: id)
            currentConversationId = conversation.id
            currentConversationTitle = conversation.title
            messages = conversation.messages
        } catch { handle(error) }
    }

    func newConversation() {
        currentConversationId = nil
        currentConversationTitle = "New conversation"
        messages = []
    }

    func send(_ text: String) async {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, isAuthenticated, !isSending else { return }
        let priorMessages = messages
        let now = ISO8601DateFormatter().string(from: Date())
        messages.append(ConversationMessage(role: "user", content: trimmed, model: nil, at: now))
        isSending = true
        defer { isSending = false }
        do {
            let response = try await chat.send(
                text: trimmed,
                conversationId: currentConversationId,
                history: priorMessages
            )
            currentConversationId = response.conversationId
            currentConversationTitle = response.conversationTitle
            if response.newTopic && !priorMessages.isEmpty {
                await openConversation(response.conversationId)
            } else {
                messages.append(ConversationMessage(
                    role: "assistant",
                    content: response.reply,
                    model: response.model,
                    at: ISO8601DateFormatter().string(from: Date())
                ))
            }
            conversations = try await chat.conversations()
        } catch { handle(error) }
    }

    private func apply(awaitResult result: AuthServiceOutcome) {
        switch result {
        case .needsEnrollment: enrollmentState = .notStarted
        case let .awaitingApproval(expiresAt): enrollmentState = .awaitingApproval(expiresAt: expiresAt)
        case .authenticated: enrollmentState = .authenticated
        case .signedOut: enrollmentState = .signedOut
        }
    }

    private func handle(_ error: Error) {
        if (error as? JarvisAPIError) == .unauthorized {
            enrollmentState = .signedOut
        } else if (error as? JarvisAPIError) == .unreachable ||
                    (error as? JarvisAPIError) == .timedOut {
            connectionState = .unreachable(safeMessage(error))
        } else {
            enrollmentState = .failed(safeMessage(error))
        }
        notice = safeMessage(error)
    }

    private func safeMessage(_ error: Error) -> String {
        (error as? LocalizedError)?.errorDescription ?? "Jarvis could not complete the request."
    }
}
