import SwiftUI

@main
struct EternalMonitorApp: App {
    @StateObject private var connectionManager = ConnectionManager()
    @StateObject private var settings = AppSettings()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(connectionManager)
                .environmentObject(settings)
                .preferredColorScheme(.dark)
        }
    }
}
