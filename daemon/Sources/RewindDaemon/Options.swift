import CoreGraphics
import Foundation

enum Command {
    case list
    case doctor
    case run(RunOptions)
}

struct RunOptions {
    var bufferSeconds: Double = 60
    var clipSeconds: Double = 8
    var exportPath: String?
    var runSeconds: Double?
    var height: Int = 1080
    var fps: Int = 30
    var bitrate: Int = 5_000_000
    var displayIndex: Int = 0
    var applicationBundleID: String?
    var captureResolution: SCCaptureResolution = .automatic
    var statisticsInterval: Double = 5
    var showsCursor: Bool = true
}

enum SCCaptureResolution: String {
    case automatic
    case best
    case nominal
}

enum OptionsError: Error, CustomStringConvertible {
    case unknownCommand(String)
    case missingValue(String)
    case invalidValue(flag: String, value: String)
    case missingRequired(String)

    var description: String {
        switch self {
        case .unknownCommand(let c):
            return "unknown command '\(c)'"
        case .missingValue(let f):
            return "missing value for \(f)"
        case .invalidValue(let f, let v):
            return "invalid value '\(v)' for \(f)"
        case .missingRequired(let f):
            return "\(f) is required"
        }
    }
}

enum Options {
    static let usage = """
        rewindd — screen capture daemon for Rewind

        USAGE:
          rewindd list
          rewindd doctor
          rewindd run [options]

        RUN OPTIONS:
          --buffer <seconds>      rolling buffer length (default 60)
          --clip <seconds>        length of the clip to export (default 8)
          --export <path>         export a clip when the run ends
          --seconds <seconds>     stop after this long instead of waiting for SIGINT
          --height <pixels>       output height; width follows the display aspect (default 1080)
          --fps <n>               capture frame rate (default 30)
          --bitrate <bits/s>      encoder bitrate ceiling (default 5000000)
          --display <index>       display index from 'rewindd list' (default 0)
          --app <bundle-id>       capture only this application instead of the whole display
          --capture-resolution    automatic | best | nominal (default automatic)
          --stats <seconds>       statistics interval, 0 to disable (default 5)
          --no-cursor             omit the cursor from the capture
        """

    static func parse(_ arguments: [String]) throws -> Command {
        guard arguments.count >= 2 else {
            throw OptionsError.unknownCommand("")
        }

        switch arguments[1] {
        case "list":
            return .list
        case "doctor":
            return .doctor
        case "run":
            return .run(try parseRun(Array(arguments.dropFirst(2))))
        case "--help", "-h", "help":
            print(usage)
            exit(0)
        case let other:
            throw OptionsError.unknownCommand(other)
        }
    }

    static func parseRun(_ arguments: [String]) throws -> RunOptions {
        var options = RunOptions()

        func value(_ index: inout Int, _ flag: String) throws -> String {
            guard index + 1 < arguments.count else {
                throw OptionsError.missingValue(flag)
            }
            index += 1
            return arguments[index]
        }

        var index = 0
        while index < arguments.count {
            let flag = arguments[index]
            switch flag {
            case "--buffer":
                options.bufferSeconds = try double(try value(&index, flag), flag)
            case "--clip":
                options.clipSeconds = try double(try value(&index, flag), flag)
            case "--export":
                options.exportPath = try value(&index, flag)
            case "--seconds":
                options.runSeconds = try double(try value(&index, flag), flag)
            case "--height":
                options.height = try int(try value(&index, flag), flag)
            case "--fps":
                options.fps = try int(try value(&index, flag), flag)
            case "--bitrate":
                options.bitrate = try int(try value(&index, flag), flag)
            case "--display":
                options.displayIndex = try int(try value(&index, flag), flag)
            case "--app":
                options.applicationBundleID = try value(&index, flag)
            case "--capture-resolution":
                let raw = try value(&index, flag)
                guard let resolution = SCCaptureResolution(rawValue: raw) else {
                    throw OptionsError.invalidValue(flag: flag, value: raw)
                }
                options.captureResolution = resolution
            case "--stats":
                options.statisticsInterval = try double(try value(&index, flag), flag)
            case "--no-cursor":
                options.showsCursor = false
            default:
                throw OptionsError.unknownCommand(flag)
            }
            index += 1
        }

        return options
    }

    static func int(_ raw: String, _ flag: String) throws -> Int {
        guard let value = Int(raw) else {
            throw OptionsError.invalidValue(flag: flag, value: raw)
        }
        return value
    }

    static func double(_ raw: String, _ flag: String) throws -> Double {
        guard let value = Double(raw) else {
            throw OptionsError.invalidValue(flag: flag, value: raw)
        }
        return value
    }
}
