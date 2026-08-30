import SwiftUI

@main
struct JarvisApp: App {
    @Environment(\.scenePhase) private var scenePhase
    @StateObject private var model = JarvisAppModel()

    var body: some Scene {
        WindowGroup {
            RootView(model: model)
                .task { await model.start() }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase != .active { model.lockWhenBackgrounded() }
        }
    }
}
