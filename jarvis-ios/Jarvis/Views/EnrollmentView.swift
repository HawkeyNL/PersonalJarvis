import SwiftUI

struct EnrollmentView: View {
    @ObservedObject var model: JarvisAppModel

    var body: some View {
        ContentUnavailableView {
            Label(title, systemImage: symbol)
        } description: {
            Text(detail)
        } actions: {
            actions
        }
        .padding()
    }

    private var title: String {
        switch model.connectionState {
        case .unconfigured: "Connect a Home Node"
        case .checking: "Checking Home Node"
        case .unreachable: "Home Node unavailable"
        case .reachable:
            switch model.enrollmentState {
            case .awaitingApproval: "Approval required"
            case .authenticated: "Connected"
            default: "Enroll this iPhone or iPad"
            }
        }
    }

    private var symbol: String {
        model.connectionState == .reachable ? "key.horizontal" : "network.slash"
    }

    private var detail: String {
        switch model.connectionState {
        case .unconfigured: "Set your Home Node address in Settings."
        case .checking: "Waiting for a readiness response."
        case let .unreachable(message): message
        case .reachable:
            switch model.enrollmentState {
            case let .awaitingApproval(expiresAt):
                "Approve this unique device identity from an existing trusted Jarvis device before \(expiresAt.formatted())."
            case .signedOut: "Your session is signed out. Sign in again with this device identity."
            case let .failed(message): message
            default: "This creates a unique Ed25519 identity in this device's Keychain."
            }
        }
    }

    @ViewBuilder private var actions: some View {
        switch model.connectionState {
        case .unconfigured:
            Text("Open Settings to configure the address.")
        case .checking:
            ProgressView()
        case .unreachable:
            Button("Retry") { Task { _ = await model.checkConnection() } }
                .buttonStyle(.borderedProminent)
        case .reachable:
            switch model.enrollmentState {
            case .awaitingApproval:
                Button("Check approval") { Task { await model.refreshEnrollment() } }
                    .buttonStyle(.borderedProminent)
            case .requesting, .authenticating:
                ProgressView()
            case .signedOut:
                Button("Sign in") { Task { await model.beginSignIn() } }
                    .buttonStyle(.borderedProminent)
            case .authenticated:
                EmptyView()
            default:
                Button("Request enrollment") { Task { await model.requestEnrollment() } }
                    .buttonStyle(.borderedProminent)
            }
        }
    }
}
