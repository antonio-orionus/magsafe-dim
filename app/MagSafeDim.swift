// MagSafe Dim's window: one switch and its status.
//
// The helper switch registers or unregisters the bundled `magsafe-dim` daemon with
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
    /// The state the helper keeps, as `magsafe-dim target` prints it: "off" unless
    /// `magsafe-dim set` chose another.
    @Published private(set) var target = "off"
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
    init(preview status: SMAppService.Status, light: String, target: String = "off",
         error: String? = nil, stuck: Bool = false) {
        self.status = status
        self.light = light
        self.target = target
        self.error = error
        self.stuck = stuck
    }

    /// On once registered, even while macOS still waits for the user's approval.
    var isOn: Bool { status == .enabled || status == .requiresApproval }

    /// False on a Mac without a MagSafe 3 light (`magsafe-dim led` says "unsupported").
    var supported: Bool { light != "unsupported" }

    /// What the user last asked for with the helper switch.
    private var wanted: Bool?

    func set(on: Bool) {
        wanted = on
        error = nil
        do {
            if on {
                try service.register()
            } else {
                // Turning the helper off ends a state chosen with the light switch or
                // `magsafe-dim set`, so the next time it starts it keeps the light off
                // again: the two switches turn on together. Only possible while it runs.
                if target != "off" {
                    _ = Model.helper(["set", "off"])
                }
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

    /// Ends a state chosen with `magsafe-dim set`: the helper keeps the light off again.
    /// The light switch: on while the helper keeps the light off. Off when it
    /// keeps macOS's choice (`system`) or a state a script set.
    var keepsLightOff: Bool { target == "off" }

    /// The "Keep the light off" switch. On: turns light control on if needed (the
    /// helper keeps the light off by default) or sets the light back to off.
    /// Off: leaves the light to macOS but keeps light control for scripts, or,
    /// before macOS has allowed the helper, cancels the request.
    func keepLightOff(_ on: Bool) {
        if !isOn {
            if on { set(on: true) }
        } else if status == .enabled {
            setLight(off: on)
        } else if !on {
            set(on: false)
        }
    }

    /// Switches between keeping the light off and leaving it to macOS (and to
    /// scripts). Works only while the helper runs, like `magsafe-dim set`.
    func setLight(off: Bool) {
        error = nil
        target = off ? "off" : "system"  // the switch moves at once; refresh confirms
        Task.detached(priority: .userInitiated) {
            let done = Model.helper(["set", off ? "off" : "system"]) != nil
            await self.finishSettingLight(done)
        }
    }

    private func finishSettingLight(_ done: Bool) {
        if !done {
            error = "Couldn't change the light. Turn “Allow light control” off and on again."
        }
        refresh()
    }

    /// Reads the helper's state and the light off the main thread, then shows them.
    func refresh() {
        guard !refreshing else { return }
        refreshing = true
        Task.detached(priority: .utility) {
            let status = SMAppService.daemon(plistName: Model.plistName).status
            let light = Model.helper(["led"]) ?? "unknown"
            let target = Model.helper(["target"]) ?? "off"
            await self.show(status: status, light: light, target: target)
        }
    }

    private func show(status: SMAppService.Status, light: String, target: String) {
        refreshing = false
        self.status = status
        self.light = light
        self.target = target
        // On, but the light hasn't gone off within 10 s: the helper isn't working.
        // A state chosen with `magsafe-dim set` is the script's business, not checked.
        if status == .enabled && supported && target == "off" && light != "off" {
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

    /// Runs the bundled helper binary with `arguments` and returns its output's
    /// first line, or nil if it failed. Its read commands need no root.
    nonisolated private static func helper(_ arguments: [String]) -> String? {
        guard let url = Bundle.main.url(forAuxiliaryExecutable: "magsafe-dim") else { return nil }
        let process = Process()
        let pipe = Pipe()
        process.executableURL = url
        process.arguments = arguments
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
        } catch {
            return nil
        }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        let text = String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        return process.terminationStatus == 0 && !text.isEmpty ? text : nil
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

            VStack(alignment: .leading, spacing: 6) {
                Card {
                    // The promise comes first. Turning it on also turns on light control,
                    // so the first click is the only one most people need.
                    SwitchRow(title: "Keep the light off", status: lightStatus,
                              isOn: Binding(get: { model.supported && model.isOn && model.keepsLightOff },
                                            set: { model.keepLightOff($0) }),
                              // Nothing to turn on on a Mac without the light; turning off stays possible.
                              enabled: model.supported || model.isOn,
                              // Only once the light is off: under a warning it's noise.
                              note: model.status == .enabled && model.supported && model.keepsLightOff && model.light == "off"
                                  ? "It comes back while the Mac sleeps, and goes off when it wakes." : nil)
                    // Shown once macOS has allowed it: a first open has one switch, and
                    // from then on light control can stay on with the light left to macOS
                    // (for scripts and AI). Turning it off hides it again.
                    if model.status == .enabled {
                        Divider()
                        SwitchRow(title: "Allow light control", status: helperStatus,
                                  isOn: Binding(get: { model.isOn }, set: { model.set(on: $0) }),
                                  enabled: true)
                    }
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

            // Nothing to control on a Mac without the light. Until the helper runs,
            // `magsafe-dim set` refuses, so the button waits for it.
            if model.supported {
                AIButton(waitingFor: aiWaitingFor) { openWindow(id: "agent") }
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

    /// What the AI button waits for, or nil once the helper runs.
    private var aiWaitingFor: String? {
        switch model.status {
        case .enabled: nil
        case .requiresApproval: "Available once macOS allows it"
        default: "Turn on “Keep the light off” first"
        }
    }

    typealias Status = (text: String, symbol: String?, color: Color)

    /// The line under the light-control switch.
    private var helperStatus: Status {
        if !model.supported {
            return ("Not available", nil, .secondary)
        }
        switch model.status {
        case .requiresApproval:
            // The light row shows the waiting; this says what it waits for.
            return ("Needs your OK once", nil, .secondary)
        case .enabled:
            // Plain: the light row's check is the one success that matters.
            return ("Scripts and AI can set the light", nil, .secondary)
        default:
            // .notFound included: some macOS 13 versions report it before the first
            // registration. A real problem shows up as an error when switching on.
            return ("Needs your OK once", nil, .secondary)
        }
    }

    /// The line under the light switch.
    private var lightStatus: Status {
        if !model.supported {
            return ("This Mac has no MagSafe light", "minus.circle", .secondary)
        }
        switch model.status {
        case .requiresApproval:
            return ("Waiting for your OK", "exclamationmark.circle.fill", .orange)
        case .enabled:
            break
        default:
            return ("macOS controls the light", nil, .secondary)
        }
        switch model.target {
        case "off":
            break
        case "system":
            return ("macOS controls the light", nil, .secondary)
        default:
            return ("A script set the light to “\(model.target)”", "terminal", .secondary)
        }
        if model.light == "off" {
            return ("The light is off", "checkmark.circle.fill", .green)
        }
        if model.light == "unknown" {
            return ("Can't read the light's state", "exclamationmark.triangle.fill", .red)
        }
        if model.stuck {
            return ("The light stays on. Turn “Allow light control” off and on again.", "exclamationmark.triangle.fill", .red)
        }
        return ("Turning the light off…", "exclamationmark.circle.fill", .orange)
    }
}

/// One setting: a title with a status line under it, and a switch. Dimmed while
/// it can't be changed.
struct SwitchRow: View {
    let title: String
    let status: ContentView.Status
    @Binding var isOn: Bool
    var enabled = true
    /// A footnote under the status, part of the row.
    var note: String?

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(.body.weight(.medium))
                    .accessibilityHidden(true)  // the switch carries the same label
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
                if let note {
                    Text(note)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.top, 2)
                }
            }
            .opacity(enabled ? 1 : 0.55)
            Spacer()
            Toggle(title, isOn: $isOn)
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(.large)
                .disabled(!enabled)
        }
    }
}

/// The "Use with AI" button: a row like the ones that open a page in System
/// Settings, with a colored icon tile, a title, a subtitle and a chevron.
struct AIButton: View {
    /// Until the helper runs: what it waits for, shown instead of the subtitle
    /// while the button is disabled.
    var waitingFor: String?
    var action: () -> Void
    @State private var hovering = false

    private var enabled: Bool { waitingFor == nil }

    var body: some View {
        Button(action: action) {
            HStack(spacing: 12) {
                Image(systemName: "sparkles")
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundStyle(.white)
                    .frame(width: 34, height: 34)
                    .background(
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .fill(LinearGradient(colors: enabled ? [.indigo, .purple, .pink] : [.gray, .gray],
                                                 startPoint: .topLeading, endPoint: .bottomTrailing))
                    )
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Use with AI")
                        .font(.body.weight(.medium))
                    Text(waitingFor ?? "Let an AI agent set up the light for you")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if enabled {
                    Image(systemName: "chevron.right")
                        .font(.callout.weight(.semibold))
                        .foregroundStyle(.tertiary)
                        .accessibilityHidden(true)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(RoundedRectangle(cornerRadius: 10))
        }
        .buttonStyle(.plain)
        // macOS dims a disabled button's contents; the card is drawn outside the
        // button, so it stays as solid as the others.
        .disabled(!enabled)
        .background(RoundedRectangle(cornerRadius: 10).fill(.quaternary.opacity(hovering && enabled ? 1 : 0.6)))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(.separator))
        .onHover { hovering = $0 }
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

// MARK: - Agents

/// Controlling the light from a coding agent: a prompt to copy. The prompt only
/// points the agent at the helper's `--help`, which documents the commands, so
/// it never goes out of date.
struct AgentView: View {
    /// The helper binary inside this app.
    var toolPath = Bundle.main.url(forAuxiliaryExecutable: "magsafe-dim")?.path
        ?? "/Applications/MagSafe Dim.app/Contents/MacOS/magsafe-dim"
    @State private var copied = false

    private var prompt: String {
        """
        The MagSafe Dim app controls the charging light on my MacBook's MagSafe \
        cable. Its command-line tool is "\(toolPath)". Run it with --help to see \
        what it can do. Then suggest a few useful ways to use the light, ask me \
        which ones I want, and set them up.
        """
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Control the light with an AI agent")
                .font(.title3.weight(.semibold))
            Text("Paste this into an AI agent that can run commands on your Mac, such as Claude Code or Codex. It suggests ideas and asks what you want.")
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Card {
                Text(prompt)
                    .font(.system(.callout, design: .monospaced))
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Spacer()
                Button(copied ? "Copied" : "Copy Prompt") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(prompt, forType: .string)
                    copied = true
                }
            }
        }
        .padding(20)
        .frame(width: 440)
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
