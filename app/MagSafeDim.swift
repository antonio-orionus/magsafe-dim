// MagSafe Dim's window: the light switches and their status.
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
    /// `magsafe-dim set` chose another; "night" for off from sunset to sunrise.
    @Published private(set) var target = "off"
    /// Tonight's or the current night's times, read with `magsafe-dim sun` while
    /// the helper keeps the light off at night.
    @Published private(set) var sun: Sun?
    /// The daily heartbeat as `magsafe-dim heartbeat` prints it: "on", "off", or
    /// "unavailable" in a build that never sends it (not the author's Developer ID).
    @Published private(set) var heartbeat = "on"
    /// Why the heartbeat switch couldn't change, shown in the About window.
    @Published var heartbeatError: String?
    /// The helper is on, but the light has stayed on for a while: it isn't working.
    @Published private(set) var stuck = false
    @Published var error: String?
    private var timer: Timer?
    private var refreshing = false
    /// Counts writes, so a refresh that started before one doesn't undo its switch.
    private var writes = 0
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
         sun: Sun? = nil, error: String? = nil, stuck: Bool = false) {
        self.status = status
        self.light = light
        self.target = target
        self.sun = sun
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

    /// The light switch: on while the helper keeps the light off, all the time
    /// or at night. Off when it keeps macOS's choice (`system`) or a state a script set.
    var keepsLightOff: Bool { target == "off" || target == "night" }

    /// The "Only at night" switch.
    var nightOnly: Bool { target == "night" }

    /// Whether the helper keeps the light off right now.
    var offNow: Bool { target == "off" || (target == "night" && sun?.night == true) }

    /// The "Keep the light off" switch. On: turns light control on if needed (the
    /// helper keeps the light off by default) or sets the light back to off.
    /// Off: leaves the light to macOS but keeps light control for scripts, or,
    /// before macOS has allowed the helper, cancels the request.
    func keepLightOff(_ on: Bool) {
        if !isOn {
            if on { set(on: true) }
        } else if status == .enabled {
            setTarget(on ? "off" : "system")
        } else if !on {
            set(on: false)
        }
    }

    /// The "Only at night" switch: off from sunset to sunrise, or all the time.
    func setNightOnly(_ on: Bool) {
        setTarget(on ? "night" : "off")
    }

    /// Makes the helper keep `state` ("off", "system", "night"). Works only while
    /// the helper runs, like `magsafe-dim set`.
    func setTarget(_ state: String) {
        error = nil
        writes += 1
        target = state  // the switch moves at once; refresh confirms
        Task.detached(priority: .userInitiated) {
            let done = Model.run(["set", state]).ok
            // Night's times come with it, so the status never shows the light turning off by day.
            let sun = state == "night" ? Model.helper(["sun"]).flatMap(Sun.init) : nil
            await self.finishSettingLight(done, sun: sun)
        }
    }

    /// The heartbeat switch in the About window.
    func setHeartbeat(_ on: Bool) {
        heartbeatError = nil
        writes += 1
        heartbeat = on ? "on" : "off"
        Task.detached(priority: .userInitiated) {
            let result = Model.run(["heartbeat", on ? "on" : "off"])
            await self.finishSettingHeartbeat(result)
        }
    }

    private func finishSettingHeartbeat(_ result: (ok: Bool, output: String, error: String)) {
        if !result.ok {
            let why = result.error.isEmpty ? "Only administrators can change it." : result.error
            heartbeatError = "Couldn't change the heartbeat: \(why)"
        }
        writes += 1  // a refresh that started meanwhile read the old state
        refresh()
    }

    private func finishSettingLight(_ done: Bool, sun: Sun?) {
        if !done {
            error = "Couldn't change the light. Turn “Allow light control” off and on again."
        }
        if let sun { self.sun = sun }
        writes += 1
        refresh()
    }

    /// Reads the helper's state and the light off the main thread, then shows them.
    func refresh() {
        guard !refreshing else { return }
        refreshing = true
        let started = writes
        Task.detached(priority: .utility) {
            let status = SMAppService.daemon(plistName: Model.plistName).status
            let light = Model.helper(["led"]) ?? "unknown"
            let target = Model.helper(["target"]) ?? "off"
            let sun = target == "night" ? Model.helper(["sun"]).flatMap(Sun.init) : nil
            let heartbeat = Model.helper(["heartbeat"]) ?? "on"
            await self.show(status: status, light: light, target: target, sun: sun, heartbeat: heartbeat,
                            started: started)
        }
    }

    private func show(status: SMAppService.Status, light: String, target: String, sun: Sun?, heartbeat: String,
                      started: Int) {
        refreshing = false
        // A switch changed while this was read: what it read is out of date.
        guard started == writes else {
            refresh()
            return
        }
        self.status = status
        self.light = light
        self.target = target
        self.sun = sun
        self.heartbeat = heartbeat
        // On, but the light hasn't gone off within 10 s: the helper isn't working.
        // A state chosen with `magsafe-dim set` is the script's business, not checked.
        if status == .enabled && supported && offNow && light != "off" {
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
        let result = run(arguments)
        return result.ok && !result.output.isEmpty ? result.output : nil
    }

    /// Runs the bundled helper binary: whether it succeeded, its output, and its
    /// error message without the "magsafe-dim: " prefix.
    nonisolated private static func run(_ arguments: [String]) -> (ok: Bool, output: String, error: String) {
        guard let url = Bundle.main.url(forAuxiliaryExecutable: "magsafe-dim") else { return (false, "", "") }
        let process = Process()
        let out = Pipe(), err = Pipe()
        process.executableURL = url
        process.arguments = arguments
        process.standardOutput = out
        process.standardError = err
        do {
            try process.run()
        } catch {
            return (false, "", "")
        }
        // Both are a line or two, far below a pipe's buffer, so reading one after the other can't block.
        let output = out.fileHandleForReading.readDataToEndOfFile()
        let errors = err.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        let text = { (data: Data) in String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines) }
        var message = text(errors)
        if message.hasPrefix("magsafe-dim: ") { message.removeFirst("magsafe-dim: ".count) }
        return (process.terminationStatus == 0, text(output), message)
    }
}

/// Day or night, and the night's times, as `magsafe-dim sun` prints them:
/// "night 1795009266 1795064389 macos".
struct Sun: Equatable {
    var night: Bool
    var sunset: Date
    var sunrise: Date
    /// False when macOS has no times (no location) and night is 20:00 to 07:00.
    var fromMacOS = true

    init(night: Bool, sunset: Date, sunrise: Date, fromMacOS: Bool = true) {
        self.night = night
        self.sunset = sunset
        self.sunrise = sunrise
        self.fromMacOS = fromMacOS
    }

    init?(_ line: String) {
        let words = line.split(separator: " ")
        guard words.count == 4, words[0] == "night" || words[0] == "day",
              let sunset = Double(words[1]), let sunrise = Double(words[2]) else { return nil }
        self.init(night: words[0] == "night",
                  sunset: Date(timeIntervalSince1970: sunset),
                  sunrise: Date(timeIntervalSince1970: sunrise),
                  fromMacOS: words[3] == "macos")
    }

    /// A time as the user's clock shows it: "16:41" or "4:41 PM".
    static func clock(_ date: Date) -> String {
        date.formatted(date: .omitted, time: .shortened)
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
                              note: model.status == .enabled && model.supported && model.offNow && model.light == "off"
                                  ? "It comes back while the Mac sleeps, and goes off when it wakes." : nil)
                    // Only while the light is kept off: at night only, or all the time.
                    // Indented with no divider, so it reads as part of the switch above.
                    if model.status == .enabled && model.supported && model.keepsLightOff {
                        SwitchRow(title: "Only at night", status: (nightText, nil, .secondary),
                                  isOn: Binding(get: { model.nightOnly }, set: { model.setNightOnly($0) }),
                                  enabled: true)
                            .padding(.leading, 20)
                    }
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

    /// The line under "Only at night".
    private var nightText: String {
        guard model.nightOnly else { return "Let the light show by day" }
        guard let sun = model.sun else { return "Light off from sunset to sunrise" }
        let times = "Light off from \(Sun.clock(sun.sunset)) to \(Sun.clock(sun.sunrise))"
        return sun.fromMacOS ? times : "\(times): turn on Location Services for sunset times"
    }

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
        case "night":
            guard let sun = model.sun else { break }
            if !sun.night {
                return ("The light goes off at \(Sun.clock(sun.sunset))", "moon", .secondary)
            }
            if model.light == "off" {
                return ("The light is off until \(Sun.clock(sun.sunrise))", "checkmark.circle.fill", .green)
            }
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
    @ObservedObject var model: Model
    var icon: NSImage = NSApp.applicationIconImage
    var version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "dev"

    private static let repo = URL(string: "https://github.com/antonio-orionus/magsafe-dim")!

    private var heartbeatNote: String {
        if model.heartbeat == "unavailable" {
            return "This copy wasn't built by the author, so it never sends one."
        }
        if model.status != .enabled {
            return "Turn on “Allow light control” in the main window to change this."
        }
        return "Once a day: app version, macOS version and Mac model. It counts how many Macs use MagSafe Dim."
    }

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

            // The helper sends it, so it can only change while the helper runs.
            Card {
                SwitchRow(title: "Send a daily heartbeat", status: ("Anonymous, no ID", nil, .secondary),
                          isOn: Binding(get: { model.heartbeat == "on" }, set: { model.setHeartbeat($0) }),
                          enabled: model.status == .enabled && model.heartbeat != "unavailable",
                          note: heartbeatNote)
                if let error = model.heartbeatError {
                    Label {
                        Text(error).fixedSize(horizontal: false, vertical: true)
                    } icon: {
                        Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
                    }
                    .font(.callout)
                }
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
