import SwiftUI

struct SettingsView: View {
    @ObservedObject var model: JarvisAppModel
    @State private var showResetConfirmation = false

    var body: some View {
        NavigationStack {
            Form {
                Section("Home Node") {
                    TextField("https://jarvis.local", text: $model.endpointText)
                        .textInputAutocapitalization(.never)
                        .keyboardType(.URL)
                        .autocorrectionDisabled()
                    Button("Save and test connection") { Task { _ = await model.saveEndpoint() } }
                    connectionLabel
                }
                Section("Security") {
                    Text("The device key, pairing nonce, device ID, and session token are stored in the device-only Keychain and are never synchronized through iCloud.")
                        .font(.footnote)
                    if model.isAuthenticated {
                        Button("Lock now") { model.lockWhenBackgrounded() }
                        Button("Log out") { Task { await model.logout() } }
                    }
                    Button("Reset this device", role: .destructive) { showResetConfirmation = true }
                }
                if let notice = model.notice {
                    Section("Status") { Text(notice) }
                }
            }
            .navigationTitle("Settings")
            .confirmationDialog(
                "Remove this device identity?",
                isPresented: $showResetConfirmation,
                titleVisibility: .visible
            ) {
                Button("Reset and require re-enrollment", role: .destructive) {
                    Task { await model.resetDevice() }
                }
            } message: {
                Text("Jarvis will attempt to revoke this device, then remove its local key and session even if the Home Node is offline.")
            }
        }
    }

    @ViewBuilder private var connectionLabel: some View {
        switch model.connectionState {
        case .unconfigured: Label("Not configured", systemImage: "circle")
        case .checking: Label("Checking", systemImage: "clock")
        case .reachable: Label("Reachable", systemImage: "checkmark.circle.fill").foregroundStyle(.green)
        case let .unreachable(message):
            Label(message, systemImage: "exclamationmark.triangle.fill").foregroundStyle(.orange)
        }
    }
}
