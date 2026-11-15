// MagSafe Dim's window: one switch and its status.
//
// The switch registers or unregisters the bundled `magsafe-dim` daemon with
// SMAppService. macOS asks the user to approve it once in Login Items; after
// that it runs as root at every boot, whether or not this app is open.

import ServiceManagement
import SwiftUI

@MainActor
final class Model: ObservableObject {
    nonisolated static let plistName = "dev.orionus.magsafe-dim.daemon.plist"
    /// A new object on every use, so a status read never comes from a stale one.
    private var service: SMAppService { SMAppService.daemon(plistName: Self.plistName) }
    @Published private(set) var status: SMAppService.Status = .notRegistered
    /// The LED state's name as `magsafe-dim led` prints it (`Led` in src/smc.rs):
    /// "off", "green", ..., or "unknown" or "unsupported".
    @Published private(set) var light = "unknown"
    /// The helper is on, but the light has stayed on for a while: it isn't working.
    @Published private(set) var stuck = false
    @Published var error: String?
    private var timer: Timer?
    private var refreshing = false
    private var notOffSince: Date?

    init() {
        refresh()
        timer = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.refresh() }
        }
        // Coming back from System Settings: show the new state at once.
        NotificationCenter.default.addObserver(
            forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in
            Task { @MainActor in self?.refresh() }
        }
    }

    /// A fixed state for the preview tool: no service calls, no timer.
    init(preview status: SMAppService.Status, light: String, error: String? = nil, stuck: Bool = false) {
        self.status = status
        self.light = light
        self.error = error
        self.stuck = stuck
    }

    /// On once registered, even while macOS still waits for the user's approval.
    var isOn: Bool { status == .enabled || status == .requiresApproval }

    /// False on a Mac without a MagSafe 3 light (`magsafe-dim led` says "unsupported").
    var supported: Bool { light != "unsupported" }

    /// What the user last asked for with the switch.
    private var wanted: Bool?

    func set(on: Bool) {
        wanted = on
        error = nil
        do {
            if on {
                try service.register()
            } else {
                try service.unregister()
            }
        } catch {
            // While the helper waits for approval, register() throws "Operation not
            // permitted" although the registration went through. Not an error: the
            // approval card explains what to do.
            if !(on && service.status == .requiresApproval) {
                self.error = "Couldn't change the setting. \(error.localizedDescription)"
            }
        }
        // No jump to System Settings here: macOS posts its own notification asking
        // the user to allow it, and the card below offers the button.
        refresh()
    }

    /// Reads the helper's state and the light off the main thread, then shows them.
    func refresh() {
        guard !refreshing else { return }
        refreshing = true
        Task.detached(priority: .utility) {
            let status = SMAppService.daemon(plistName: Model.plistName).status
            let light = Model.readLight()
            await self.show(status: status, light: light)
        }
    }

    private func show(status: SMAppService.Status, light: String) {
        refreshing = false
        self.status = status
        self.light = light
        // On, but the light hasn't gone off within 10 s: the helper isn't working.
        if status == .enabled && supported && light != "off" {
            let since = notOffSince ?? Date()
            notOffSince = since
            stuck = Date().timeIntervalSince(since) > 10
        } else {
            notOffSince = nil
            stuck = false
        }
        // An error goes away once the state is what the user asked for.
        if let wanted, (wanted && status == .enabled) || (!wanted && status != .enabled) {
            error = nil
        }
    }

    /// Asks the bundled daemon binary; reading the LED needs no root.
    nonisolated private static func readLight() -> String {
        guard let url = Bundle.main.url(forAuxiliaryExecutable: "magsafe-dim") else { return "unknown" }
        let process = Process()
        let pipe = Pipe()
        process.executableURL = url
        process.arguments = ["led"]
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
        } catch {
            return "unknown"
        }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        let text = String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        return process.terminationStatus == 0 && !text.isEmpty ? text : "unknown"
    }
}

// MARK: - View

struct ContentView: View {
    @ObservedObject var model: Model
    var icon: NSImage = NSApp.applicationIconImage
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 14) {
                Image(nsImage: icon)
                    .resizable()
                    .frame(width: 64, height: 64)
                VStack(alignment: .leading, spacing: 3) {
                    Text("MagSafe Dim")
                        .font(.title2.weight(.semibold))
                    Text("Keeps your MagSafe charging light off.")
                        .foregroundStyle(.secondary)
                }
            }

            Card {
                HStack {
                    Text("Keep the light off")
                        .font(.body.weight(.medium))
                        .accessibilityHidden(true)  // the switch carries the same label
                    Spacer()
                    Toggle("Keep the light off", isOn: Binding(get: { model.isOn }, set: { model.set(on: $0) }))
                        .labelsHidden()
                        .toggleStyle(.switch)
                        .controlSize(.large)
                        // Nothing to switch off on a Mac without the light; turning off stays possible.
                        .disabled(!model.supported && !model.isOn)
                }

                Label {
                    Text(status.text)
                } icon: {
                    if let symbol = status.symbol {
                        Image(systemName: symbol)
                            .foregroundStyle(status.color)
                            .accessibilityHidden(true)
                    }
                }
                .font(.callout)
                .foregroundStyle(.secondary)

                if model.status == .enabled && model.supported {
                    Text("It comes back while the Mac sleeps, and goes off when it wakes.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }

            if model.status == .requiresApproval {
                Card {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("macOS needs your OK. Click Allow in its notification, or turn on MagSafe Dim in Login Items.")
                            .fixedSize(horizontal: false, vertical: true)
                        Button("Open Login Items…") { SMAppService.openSystemSettingsLoginItems() }
                    }
                }
            }

            if let error = model.error {
                Card {
                    Label {
                        Text(error).fixedSize(horizontal: false, vertical: true)
                    } icon: {
                        Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
                    }
                }
            }

            // Footer links don't take keyboard focus, so a window never opens with
            // the focus ring on them; About stays in the app menu for the keyboard.
            HStack(alignment: .firstTextBaseline) {
                MadeBy(linkTakesFocus: false)
                Spacer()
                Button("About") { openWindow(id: "about") }
                    .buttonStyle(.link)
                    .focusable(false)
            }
            .font(.footnote)
            .padding(.horizontal, 4)
        }
        .padding(20)
        .frame(width: 400)
    }

    /// One plain-words line under the switch.
    private var status: (text: String, symbol: String?, color: Color) {
        if !model.supported {
            return ("This Mac has no MagSafe light", "minus.circle", .secondary)
        }
        switch model.status {
        case .requiresApproval:
            return ("Waiting for your OK in Login Items", "exclamationmark.circle.fill", .orange)
        case .enabled:
            if model.light == "off" {
                return ("The light is off", "checkmark.circle.fill", .green)
            }
            if model.light == "unknown" {
                return ("Can't read the light's state", "exclamationmark.triangle.fill", .red)
            }
            if model.stuck {
                return ("The light stays on. Turn the switch off and on again.", "exclamationmark.triangle.fill", .red)
            }
            return ("Turning the light off…", "exclamationmark.circle.fill", .orange)
        default:
            // .notFound included: some macOS 13 versions report it before the first
            // registration. A real problem shows up as an error when switching on.
            return ("macOS controls the light", nil, .secondary)
        }
    }
}

/// A rounded group, like the ones in System Settings.
struct Card<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) { content }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 10).fill(.quaternary.opacity(0.6)))
            .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(.separator))
    }
}

// MARK: - About

struct AboutView: View {
    var icon: NSImage = NSApp.applicationIconImage
    var version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "dev"

    private static let repo = URL(string: "https://github.com/antonio-orionus/magsafe-dim")!

    var body: some View {
        VStack(spacing: 16) {
            Image(nsImage: icon)
                .resizable()
                .frame(width: 96, height: 96)

            VStack(spacing: 4) {
                Text("MagSafe Dim")
                    .font(.title2.weight(.semibold))
                Text("Version \(version)")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }

            Text("Keeps the charging light on your MagSafe cable off. A small helper turns the light off whenever macOS turns it on, and touches nothing else.")
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)

            Card {
                LinkRow(title: "Source code", detail: "GitHub", url: Self.repo)
                Divider()
                LinkRow(title: "Report a problem", detail: "GitHub Issues", url: Self.repo.appending(path: "issues"))
                Divider()
                LinkRow(title: "License", detail: "MIT", url: Self.repo.appending(path: "blob/main/LICENSE"))
            }

            MadeBy()
                .font(.callout)
        }
        .padding(24)
        .frame(width: 380)
    }
}

/// "Made with ♥ by Antonio Orionus", the name linking to the author's GitHub profile.
struct MadeBy: View {
    var linkTakesFocus = true

    var body: some View {
        HStack(spacing: 4) {
            Text("Made with").foregroundStyle(.secondary)
            Image(systemName: "heart.fill")
                .foregroundStyle(.red)
                .accessibilityLabel("love")
            Text("by").foregroundStyle(.secondary)
            Link("Antonio Orionus", destination: URL(string: "https://github.com/antonio-orionus")!)
                .focusable(linkTakesFocus)
        }
    }
}

struct LinkRow: View {
    let title: String
    let detail: String
    let url: URL

    var body: some View {
        HStack {
            Text(title)
            Spacer()
            Link(destination: url) {
                HStack(spacing: 3) {
                    Text(detail)
                    Image(systemName: "arrow.up.right")
                        .font(.caption)
                        .accessibilityHidden(true)
                }
            }
            .accessibilityLabel("\(title): \(detail)")
        }
    }
}
