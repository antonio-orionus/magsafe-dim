// Draws the window offscreen into PNGs, one per state and appearance, so the
// design can be checked without signing, installing or touching the daemon.
// Switches draw grey here: AppKit only colours them for the app in front.
// Built and run by scripts/preview-ui.sh.

import AppKit
import ServiceManagement
import SwiftUI

@main
struct Preview {
    @MainActor
    static func main() {
        _ = NSApplication.shared
        NSApp.setActivationPolicy(.prohibited)
        let out = URL(fileURLWithPath: CommandLine.arguments[1])
        let icon = NSImage(contentsOfFile: CommandLine.arguments[2]) ?? NSImage()

        let states: [(String, SMAppService.Status, String, String?)] = [
            ("on", .enabled, "off", nil),
            ("off", .notRegistered, "green", nil),
            ("approval", .requiresApproval, "green", nil),
            ("waking", .enabled, "green", nil),
            ("unsupported", .notRegistered, "unsupported", nil),
            ("error", .notRegistered, "amber", "The operation couldn’t be completed. Operation not permitted"),
        ]
        func render<V: View>(_ view: V, _ name: String) {
            for (mode, appearance) in [("dark", NSAppearance.Name.darkAqua), ("light", NSAppearance.Name.aqua)] {
                let host = NSHostingView(rootView: view.background(Color(nsColor: .windowBackgroundColor)))
                host.appearance = NSAppearance(named: appearance)
                let window = NSWindow(contentRect: NSRect(origin: .zero, size: host.fittingSize),
                                      styleMask: [.borderless], backing: .buffered, defer: false)
                window.appearance = NSAppearance(named: appearance)
                window.contentView = host
                host.frame = NSRect(origin: .zero, size: host.fittingSize)
                host.layoutSubtreeIfNeeded()
                guard let rep = host.bitmapImageRepForCachingDisplay(in: host.bounds) else { continue }
                host.cacheDisplay(in: host.bounds, to: rep)
                let file = out.appendingPathComponent("\(name)-\(mode).png")
                try? rep.representation(using: .png, properties: [:])?.write(to: file)
                print(file.path)
            }
        }
        render(AboutView(model: Model(preview: .enabled, light: "off"), icon: icon, version: "1.2.0"), "about")
        render(AboutView(model: Model(preview: .notRegistered, light: "green"), icon: icon, version: "1.2.0"), "about-off")
        render(AgentView(toolPath: "/Applications/MagSafe Dim.app/Contents/MacOS/magsafe-dim"), "agent")
        for (name, status, light, error) in states {
            render(ContentView(model: Model(preview: status, light: light, error: error), icon: icon), name)
        }
        render(ContentView(model: Model(preview: .enabled, light: "green", stuck: true), icon: icon), "stuck")
        render(ContentView(model: Model(preview: .enabled, light: "green", target: "green"), icon: icon), "set")
        render(ContentView(model: Model(preview: .enabled, light: "green", target: "system"), icon: icon), "system")
        let evening = Date(timeIntervalSince1970: 1_795_009_266), morning = Date(timeIntervalSince1970: 1_795_064_389)
        render(ContentView(model: Model(preview: .enabled, light: "off", target: "night",
                                        sun: Sun(night: true, sunset: evening, sunrise: morning)), icon: icon), "night")
        render(ContentView(model: Model(preview: .enabled, light: "green", target: "night",
                                        sun: Sun(night: false, sunset: evening, sunrise: morning)), icon: icon), "night-day")
        render(ContentView(model: Model(preview: .enabled, light: "green", target: "night",
                                        sun: Sun(night: false, sunset: evening, sunrise: morning, fromMacOS: false)), icon: icon), "night-fallback")
    }
}
