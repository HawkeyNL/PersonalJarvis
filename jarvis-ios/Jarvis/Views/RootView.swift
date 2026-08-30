import SwiftUI

struct RootView: View {
    @ObservedObject var model: JarvisAppModel

    var body: some View {
        ZStack {
            TabView {
                ChatView(model: model)
                    .tabItem { Label("Chat", systemImage: "message.fill") }
                MilestonePlaceholder(
                    title: "Voice",
                    detail: "Push-to-talk arrives in milestone 2. This build does not request microphone access.",
                    symbol: "waveform"
                )
                .tabItem { Label("Voice", systemImage: "waveform") }
                MilestonePlaceholder(title: "Activity", detail: "Jarvis activity will appear here.", symbol: "clock")
                    .tabItem { Label("Activity", systemImage: "clock") }
                MilestonePlaceholder(title: "Agents", detail: "Connected agent status will appear here.", symbol: "person.3")
                    .tabItem { Label("Agents", systemImage: "person.3") }
                SettingsView(model: model)
                    .tabItem { Label("Settings", systemImage: "gearshape") }
            }

            if model.lockState != .unlocked {
                LockView(model: model)
                    .background(.regularMaterial)
                    .ignoresSafeArea()
            }
        }
    }
}

private struct MilestonePlaceholder: View {
    let title: String
    let detail: String
    let symbol: String

    var body: some View {
        NavigationStack {
            ContentUnavailableView(title, systemImage: symbol, description: Text(detail))
                .navigationTitle(title)
        }
    }
}
