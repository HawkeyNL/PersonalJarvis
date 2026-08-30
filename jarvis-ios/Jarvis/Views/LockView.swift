import SwiftUI

struct LockView: View {
    @ObservedObject var model: JarvisAppModel

    var body: some View {
        VStack(spacing: 20) {
            Image(systemName: "lock.shield.fill")
                .font(.system(size: 54))
                .foregroundStyle(.tint)
            Text("Jarvis is locked").font(.title2.bold())
            switch model.lockState {
            case let .unavailable(message):
                Text(message).multilineTextAlignment(.center).foregroundStyle(.secondary)
            case .denied:
                Text("Authentication was not accepted.").foregroundStyle(.secondary)
            default:
                Text("Authenticate on this device to continue.").foregroundStyle(.secondary)
            }
            Button("Unlock") { Task { await model.unlock() } }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
        }
        .padding(32)
    }
}
