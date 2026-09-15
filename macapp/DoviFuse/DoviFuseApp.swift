import SwiftUI

@main
struct DoviFuseApp: App {
    @StateObject private var model = AppModel()

    var body: some Scene {
        WindowGroup("DoviFuse") {
            ContentView(model: model)
                .frame(minWidth: 760, minHeight: 540)
        }
        .windowResizability(.contentMinSize)
        .windowStyle(.hiddenTitleBar)

        Settings {
            SettingsView(model: model)
        }
    }
}
