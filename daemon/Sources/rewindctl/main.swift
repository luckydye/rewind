import Foundation
import RewindKit

private let client = IPCClient()

private func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data("rewindctl: \(message)\n".utf8))
    exit(1)
}

private func usage() -> Never {
    print(
        """
        rewindctl — talk to a running rewindd

        USAGE:
          rewindctl status [--json]
          rewindctl start | stop | quit
          rewindctl export [--seconds N] [--out PATH]
          rewindctl config [--buffer S] [--fps N] [--height PX] [--display N]
                           [--app BUNDLE_ID | --whole-screen] [--bitrate B]
          rewindctl displays | apps
          rewindctl preview --ago 3 [--width 960] [--count 1] [--out PATH]
        """
    )
    exit(0)
}

private func printJSON<T: Encodable & Sendable>(_ value: T) {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
    guard let data = try? encoder.encode(value), let text = String(data: data, encoding: .utf8)
    else {
        fail("could not encode response")
    }
    print(text)
}

private func describe(_ state: State) -> String {
    let display = state.display?.name ?? "none"
    let buffer = String(
        format: "%5.1fs / %ds   %5.1f MB   %.1f Mbps",
        state.availableSeconds,
        Int(state.bufferSeconds),
        Double(state.bufferBytes) / 1_048_576,
        state.bitrate / 1_000_000
    )
    let error = state.lastError.map { "\nerror        \($0)" } ?? ""

    return """
        recording    \(state.recording ? "yes" : "no")
        display      \(display)
        output       \(state.width)×\(state.height) @ \(state.config.fps)fps
        source       \(state.config.applicationBundleID ?? "entire screen")
        buffer       \(buffer)
        frames       \(state.framesCaptured) captured, \(state.framesEncoded) encoded, \
        \(state.droppedFrames) dropped, \(state.idleNotices) idle
        power        \(state.onBattery ? "battery" : "ac")\(error)
        """
}

private func option(_ name: String, _ arguments: [String]) -> String? {
    guard let index = arguments.firstIndex(of: name), index + 1 < arguments.count else {
        return nil
    }
    return arguments[index + 1]
}

private func has(_ name: String, _ arguments: [String]) -> Bool {
    arguments.contains(name)
}

let arguments = Array(CommandLine.arguments.dropFirst())
let wantsJSON = has("--json", arguments)
guard let command = arguments.first, !command.hasPrefix("--") else {
    usage()
}

do {
    switch command {
    case "status":
        let response = try client.send(Request(command: .status))
        guard response.ok, let state = response.state else {
            fail(response.error ?? "unexpected response")
        }
        if wantsJSON { printJSON(state) } else { print(describe(state)) }

    case "start", "stop":
        let request = Request(command: command == "start" ? .start : .stop)
        let response = try client.send(request)
        guard response.ok, let state = response.state else {
            fail(response.error ?? "unexpected response")
        }
        if wantsJSON { printJSON(state) } else { print(describe(state)) }

    case "quit":
        let response = try client.send(Request(command: .quit))
        guard response.ok else { fail(response.error ?? "unexpected response") }
        print("rewindd is shutting down")

    case "export":
        let seconds = option("--seconds", arguments).flatMap(Double.init) ?? 30
        let out = option("--out", arguments)
        let response = try client.send(
            Request(command: .exportClip, seconds: seconds, path: out)
        )
        guard response.ok, let clip = response.clip else {
            fail(response.error ?? "unexpected response")
        }
        if wantsJSON {
            printJSON(clip)
        } else {
            print(
                String(
                    format: "saved %@ — %.2fs, %.1f MB, %d frames, %dx%d",
                    clip.path,
                    clip.duration,
                    Double(clip.bytes) / 1_048_576,
                    clip.frames,
                    clip.width,
                    clip.height
                )
            )
        }

    case "config":
        let current = try client.send(Request(command: .status))
        guard current.ok, var config = current.state?.config else {
            fail(current.error ?? "unexpected response")
        }
        if let raw = option("--buffer", arguments), let value = Double(raw) {
            config.bufferSeconds = value
        }
        if let raw = option("--fps", arguments), let value = Int(raw) { config.fps = value }
        if let raw = option("--height", arguments), let value = Int(raw) { config.height = value }
        if let raw = option("--bitrate", arguments), let value = Int(raw) { config.bitrate = value }
        if let raw = option("--display", arguments), let value = Int(raw) {
            config.displayIndex = value
        }
        if let raw = option("--capture-resolution", arguments),
            let value = CaptureResolution(rawValue: raw)
        {
            config.captureResolution = value
        }
        if let bundleID = option("--app", arguments) { config.applicationBundleID = bundleID }
        if has("--whole-screen", arguments) { config.applicationBundleID = nil }

        let response = try client.send(Request(command: .setConfig, config: config))
        guard response.ok, let state = response.state else {
            fail(response.error ?? "unexpected response")
        }
        if wantsJSON { printJSON(state) } else { print(describe(state)) }

    case "preview":
        let ago = option("--ago", arguments).flatMap(Double.init) ?? 0
        let width = option("--width", arguments).flatMap(Int.init) ?? 960
        let count = option("--count", arguments).flatMap(Int.init) ?? 1
        let out = option("--out", arguments)

        let started = Date()
        let response = try client.send(
            Request(
                command: count > 1 ? .previewBatch : .previewFrame,
                seconds: ago,
                width: width,
                count: count
            )
        )
        guard response.ok, let previews = response.previews, !previews.isEmpty else {
            fail(response.error ?? "unexpected response")
        }
        let elapsed = Date().timeIntervalSince(started)

        if let out {
            let base = URL(fileURLWithPath: out)
            for (index, data) in previews.enumerated() {
                let url = previews.count == 1
                    ? base
                    : base.deletingPathExtension()
                        .appendingPathExtension(String(format: "%03d", index))
                        .appendingPathExtension("jpg")
                try data.write(to: url)
            }
        }

        let bytes = previews.reduce(0) { $0 + $1.count }
        print(
            String(
                format: "%d frame(s), %.1f KB total, %.0f ms round trip",
                previews.count,
                Double(bytes) / 1024,
                elapsed * 1000
            )
        )

    case "displays":
        let response = try client.send(Request(command: .listDisplays))
        guard response.ok, let displays = response.displays else {
            fail(response.error ?? "unexpected response")
        }
        if wantsJSON {
            printJSON(displays)
        } else {
            for display in displays {
                print("  [\(display.index)] \(display.width)x\(display.height)\(display.isMain ? " (main)" : "")")
            }
        }

    case "apps":
        let response = try client.send(Request(command: .listApplications))
        guard response.ok, let applications = response.applications else {
            fail(response.error ?? "unexpected response")
        }
        if wantsJSON {
            printJSON(applications)
        } else {
            for application in applications {
                print("  \(application.name) — \(application.bundleIdentifier)")
            }
        }

    case "--help", "-h", "help":
        usage()

    default:
        FileHandle.standardError.write(Data("rewindctl: unknown command '\(command)'\n".utf8))
        usage()
    }
} catch {
    fail("\(error)")
}
