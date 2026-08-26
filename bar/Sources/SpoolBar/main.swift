import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate {
    private let client = SpoolClient()
    private let configurationStore = BarConfigurationStore()
    private lazy var barController = WorkspaceBarController(
        client: client,
        preferences: configurationStore.preferences,
        savePreferences: { [configurationStore] preferences in
            try configurationStore.save(preferences)
        }
    )

    func applicationDidFinishLaunching(_ notification: Notification) {
        configurationStore.onChange = { [weak self] preferences in
            self?.barController.apply(preferences)
        }
        configurationStore.start()
        barController.start()
        client.onState = { [weak self] state in
            self?.barController.update(state)
        }
        client.onError = { _ in }
        client.start()
    }

    func applicationWillTerminate(_ notification: Notification) {
        configurationStore.stop()
        client.stop()
        barController.stop()
    }
}

let application = NSApplication.shared
let delegate = AppDelegate()
application.delegate = delegate
application.setActivationPolicy(.accessory)
application.run()
