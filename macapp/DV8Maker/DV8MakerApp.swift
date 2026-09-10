import SwiftUI

@main
struct DV8MakerApp: App {
    @StateObject private var model = AppModel()

    var body: some Scene {
        WindowGroup("DV8 Maker") {
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
