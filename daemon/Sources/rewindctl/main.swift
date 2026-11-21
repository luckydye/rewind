import CoreGraphics
import Foundation
import ImageIO
import RewindKit
import UniformTypeIdentifiers

private let client = IPCClient()

private enum ClientError: Error, CustomStringConvertible {
    case unexpectedReply
    case missingPayload(String)

    var description: String {
        switch self {
        case .unexpectedReply: return "rewindd sent a reply this command cannot use"
        case .missingPayload(let field): return "rewindd sent no \(field)"
        }
    }
}

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
          rewindctl export [--seconds N | --from A --to B] [--out PATH]
                           [--format mp4 | gif | png]
                           [--gif-fps N] [--gif-width PX]   (gif: 15 fps, 1280 px)
                           [--at SECONDS]                    (png: frame that long ago)
                           [--overlay RGBA] [--crop x,y,w,h]
          rewindctl copy   (same options as export, mp4 or gif; puts the file on the clipboard)
          rewindctl config [--buffer S] [--fps N] [--height PX] [--display N]
                           [--app BUNDLE_ID | --whole-screen] [--bitrate B]
          rewindctl preview --ago SECONDS [--width PX] [--out clip.png]
          rewindctl displays | apps
        """
    )
    exit(0)
}

private func json(_ reply: Reply) throws -> Response {
    guard case .json(let response) = reply else { throw ClientError.unexpectedReply }
    return response
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

/// `--crop x,y,w,h` as fractions of the capture.
private func crop(_ arguments: [String]) -> Crop? {
    guard let raw = option("--crop", arguments) else { return nil }
    let parts = raw.split(separator: ",").compactMap { Double($0.trimmingCharacters(in: .whitespaces)) }
    guard parts.count == 4 else { fail("--crop takes x,y,w,h as fractions") }
    return Crop(x: parts[0], y: parts[1], width: parts[2], height: parts[3])
}

/// Writes the raw BGRA frame as a lossless PNG, so what you look at is exactly
/// what the daemon decoded — no scaling, no JPEG.
private func writePNG(_ image: RawImage, to url: URL) throws {
    guard let provider = CGDataProvider(data: image.pixels as CFData) else {
        throw ClientError.missingPayload("pixels")
    }
    let bitmapInfo = CGBitmapInfo(
        rawValue: CGImageAlphaInfo.noneSkipFirst.rawValue
            | CGBitmapInfo.byteOrder32Little.rawValue
    )
    guard
        let cgImage = CGImage(
            width: image.width,
            height: image.height,
            bitsPerComponent: 8,
            bitsPerPixel: 32,
            bytesPerRow: image.width * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: bitmapInfo,
            provider: provider,
            decode: nil,
            shouldInterpolate: false,
            intent: .defaultIntent
        )
    else {
        throw ClientError.missingPayload("image")
    }

    guard
        let destination = CGImageDestinationCreateWithURL(
            url as CFURL,
            UTType.png.identifier as CFString,
            1,
            nil
        )
    else {
        throw ClientError.missingPayload("destination")
    }
    CGImageDestinationAddImage(destination, cgImage, nil)
    guard CGImageDestinationFinalize(destination) else {
        throw ClientError.missingPayload("png data")
    }
}

let arguments = Array(CommandLine.arguments.dropFirst())
let wantsJSON = has("--json", arguments)

guard let command = arguments.first, !command.hasPrefix("--") else {
    usage()
}

do {
    switch command {
    case "status":
        let response = try json(try client.send(Request(command: .status)))
        guard response.ok, let state = response.state else {
            fail(response.error ?? "unexpected response")
        }
        if wantsJSON { printJSON(state) } else { print(describe(state)) }

    case "start", "stop":
        let request = Request(command: command == "start" ? .start : .stop)
        let response = try json(try client.send(request))
        guard response.ok, let state = response.state else {
            fail(response.error ?? "unexpected response")
        }
        if wantsJSON { printJSON(state) } else { print(describe(state)) }

    case "quit":
        let response = try json(try client.send(Request(command: .quit)))
        guard response.ok else { fail(response.error ?? "unexpected response") }
        print("rewindd is shutting down")

    case "export", "copy":
        let seconds = option("--seconds", arguments).flatMap(Double.init) ?? 30
        let from = option("--from", arguments).flatMap(Double.init) ?? seconds
        let to = option("--to", arguments).flatMap(Double.init) ?? 0
        let out = option("--out", arguments)
        let format = option("--format", arguments) ?? "mp4"
        let request: Request
        if format == "png" {
            guard command == "export" else { fail("copy takes --format mp4 or gif") }
            request = Request(
                command: .exportFrame,
                seconds: option("--at", arguments).flatMap(Double.init) ?? 0,
                path: out,
                overlayPath: option("--overlay", arguments),
                crop: crop(arguments)
            )
        } else {
            guard let clipFormat = ExportFormat(rawValue: format) else {
                fail("--format must be mp4, gif or png")
            }
            request = Request(
                command: command == "export" ? .exportClip : .copyClip,
                path: out,
                width: option("--gif-width", arguments).flatMap(Int.init),
                range: ClipRange(from: from, to: to),
                overlayPath: option("--overlay", arguments),
                crop: crop(arguments),
                format: clipFormat,
                fps: option("--gif-fps", arguments).flatMap(Double.init)
            )
        }
        let response = try json(try client.send(request))
        guard response.ok, let clip = response.clip else {
            fail(response.error ?? "unexpected response")
        }
        if wantsJSON {
            printJSON(clip)
        } else {
            print(
                String(
                    format: "%@ %@ — %.2fs, %.1f MB, %d frames, %dx%d",
                    command == "export" ? "saved" : "copied",
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
        let current = try json(try client.send(Request(command: .status)))
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

        let response = try json(try client.send(Request(command: .setConfig, config: config)))
        guard response.ok, let state = response.state else {
            fail(response.error ?? "unexpected response")
        }
        if wantsJSON { printJSON(state) } else { print(describe(state)) }

    case "preview":
        let ago = option("--ago", arguments).flatMap(Double.init) ?? 0
        let width = option("--width", arguments).flatMap(Int.init) ?? 0
        let out = option("--out", arguments)

        let started = Date()
        let reply = try client.send(
            Request(command: .previewFrame, seconds: ago, width: width)
        )
        let elapsed = Date().timeIntervalSince(started)

        guard case .image(let image) = reply else {
            let response = try json(reply)
            fail(response.error ?? "expected an image reply")
        }

        if let out {
            try writePNG(image, to: URL(fileURLWithPath: out))
        }
        print(
            String(
                format: "%dx%d, %d bytes, %.0f ms round trip%@",
                image.width,
                image.height,
                image.pixels.count,
                elapsed * 1000,
                out.map { " — \($0)" } ?? ""
            )
        )

    case "displays":
        let response = try json(try client.send(Request(command: .listDisplays)))
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
        let response = try json(try client.send(Request(command: .listApplications)))
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
