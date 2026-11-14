// The app's entry point. The windows and their logic are in MagSafeDim.swift, so the
// preview tool (Preview.swift) can draw the same views without this file.

import SwiftUI

@main
struct MagSafeDimApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var model = Model()

    var body: some Scene {
        Window("MagSafe Dim", id: "main") {
            ContentView(model: model)
        }
        .windowResizability(.contentSize)
        .commands { AboutCommand() }

        Window("About MagSafe Dim", id: "about") {
            AboutView()
        }
        .windowResizability(.contentSize)
    }
}

/// MagSafe Dim > About MagSafe Dim opens our About window instead of the standard panel.
struct AboutCommand: Commands {
    @Environment(\.openWindow) private var openWindow

    var body: some Commands {
        CommandGroup(replacing: .appInfo) {
            Button("About MagSafe Dim") { openWindow(id: "about") }
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    /// Before any window exists, so the question shows on its own.
    func applicationWillFinishLaunching(_: Notification) {
        AppMover.moveIfNeeded()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_: NSApplication) -> Bool { true }
}

/// The helper runs from inside the app, so the app has to live in /Applications:
/// opened from Downloads, macOS runs it from a hidden temporary copy ("App
/// Translocation"), and a helper registered from there would break. Offers to move
/// the app on first launch from anywhere else, then reopens it from Applications.
@MainActor
enum AppMover {
    static let destination = URL(fileURLWithPath: "/Applications/MagSafe Dim.app")

    static func moveIfNeeded() {
        let here = Bundle.main.bundleURL.standardizedFileURL
        guard here.deletingLastPathComponent().path != "/Applications" else { return }

        NSApp.windows.forEach { $0.orderOut(nil) }
        NSApp.activate(ignoringOtherApps: true)
        let ask = NSAlert()
        ask.messageText = "Move MagSafe Dim to your Applications folder?"
        ask.informativeText = "MagSafe Dim keeps a small helper running in the background, which works only from Applications."
        ask.addButton(withTitle: "Move to Applications")
        ask.addButton(withTitle: "Quit")
        guard ask.runModal() == .alertFirstButtonReturn else {
            NSApp.terminate(nil)
            return
        }

        // Another copy running (the old one in Applications, say) would stay open
        // next to the new one.
        for other in NSRunningApplication.runningApplications(withBundleIdentifier: Bundle.main.bundleIdentifier ?? "")
        where other.processIdentifier != ProcessInfo.processInfo.processIdentifier {
            other.terminate()
        }

        // Copy under a temporary name first, then swap it in, so a failed copy never
        // leaves Applications without the app. An older copy's helper notices the
        // swap and restarts from the new one.
        let files = FileManager.default
        let staging = destination.deletingLastPathComponent().appendingPathComponent(".MagSafe Dim.app.new")
        do {
            try? files.removeItem(at: staging)
            try files.copyItem(at: here, to: staging)
            if files.fileExists(atPath: destination.path) {
                _ = try files.replaceItemAt(destination, withItemAt: staging)
            } else {
                try files.moveItem(at: staging, to: destination)
            }
        } catch {
            try? files.removeItem(at: staging)
            let fail = NSAlert()
            fail.messageText = "Couldn't move MagSafe Dim"
            fail.informativeText = "Drag it into your Applications folder yourself, then open it from there.\n\n\(error.localizedDescription)"
            fail.runModal()
            NSApp.terminate(nil)
            return
        }

        // You already allowed this download to open; don't ask again for the copy.
        var copy = destination
        var values = URLResourceValues()
        values.quarantineProperties = nil
        try? copy.setResourceValues(values)

        // The original can go, unless macOS is running a temporary copy of it.
        if !here.path.contains("/AppTranslocation/") {
            try? files.trashItem(at: here, resultingItemURL: nil)
        }

        let config = NSWorkspace.OpenConfiguration()
        config.createsNewApplicationInstance = true
        NSWorkspace.shared.openApplication(at: destination, configuration: config) { _, error in
            DispatchQueue.main.async {
                // If it doesn't reopen, at least show where it is now.
                if error != nil {
                    NSWorkspace.shared.activateFileViewerSelecting([destination])
                }
                NSApp.terminate(nil)
            }
        }
    }
}
