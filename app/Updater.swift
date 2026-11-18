// In-app updates with Sparkle. The app has no menu bar icon, so Sparkle checks
// only while the window is open: on each launch once a day has passed since the
// last check (Info.plist: SUEnableAutomaticChecks, SUScheduledCheckInterval), and
// from "Check for Updates…" in the app menu.
//
// The feed is appcast.xml on the latest GitHub release; Sparkle accepts it and the
// update only with the EdDSA signature of the key in Info.plist (SUPublicEDKey),
// and the update only if it is signed with the same Developer ID as this app.
// When the new app replaces this one, the helper notices and restarts from it.
// Not part of the preview tool, which has no Sparkle.

import Combine
import Sparkle
import SwiftUI

@MainActor
final class Updater: ObservableObject {
    private let controller = SPUStandardUpdaterController(
        startingUpdater: true, updaterDelegate: nil, userDriverDelegate: nil)
    @Published private(set) var canCheck = false

    init() {
        controller.updater.publisher(for: \.canCheckForUpdates)
            .receive(on: RunLoop.main)
            .assign(to: &$canCheck)
    }

    func checkForUpdates() {
        controller.checkForUpdates(nil)
    }
}
