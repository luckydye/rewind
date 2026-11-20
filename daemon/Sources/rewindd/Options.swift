import Foundation
import RewindKit

enum Command {
    case daemon
    /// Registers or removes the login item, so `make install` can install the
    /// service rather than asking the user to click a menu item.
    case loginItem(enabled: Bool)
    case list
    case doctor
    case run(RunOptions)
}

struct RunOptions {
    var config = RecorderConfig()
    var clipSeconds: Double = 8
    var exportPath: String?
    var runSeconds: Double?
    var statisticsInterval: Double = 5
}

enum OptionsError: Error, CustomStringConvertible {
    case unknownCommand(String)
    case missingValue(String)
    case invalidValue(flag: String, value: String)

    var description: String {
        switch self {
        case .unknownCommand(let command):
            return "unknown command '\(command)'"
        case .missingValue(let flag):
            return "missing value for \(flag)"
        case .invalidValue(let flag, let value):
            return "invalid value '\(value)' for \(flag)"
        }
    }
}

enum Options {
    static let usage = """
        rewindd — background capture daemon for Rewind

        USAGE:
          rewindd                  run the daemon (menu bar item + IPC)
          rewindd login-item on|off
                                   register or remove the login item
          rewindd list             list displays and capturable applications
          rewindd doctor           permissions, displays, hardware encoder
          rewindd run [options]    capture in the foreground, for debugging

        RUN OPTIONS:
          --buffer <seconds>       rolling buffer length (default 60)
          --clip <seconds>         length of the clip exported at exit (default 8)
          --export <path>          export a clip when the run ends
          --seconds <seconds>      stop after this long instead of waiting for SIGINT
          --height <pixels>        output height; 0 = native display resolution (default 0)
          --fps <n>                capture frame rate (default 30)
          --bitrate <bits/s>       encoder target; 0 = derived from resolution (default 0)
          --display <index>        display index from 'rewindd list' (default 0)
          --app <bundle-id>        capture only this application instead of the whole display
          --capture-resolution     automatic | best | nominal (default automatic)
          --stats <seconds>        statistics interval, 0 to disable (default 5)
          --no-cursor              omit the cursor from the capture
        """

    static func parse(_ arguments: [String]) throws -> Command {
        guard arguments.count >= 2 else { return .daemon }

        switch arguments[1] {
        case "daemon":
            return .daemon
        case "login-item":
            guard arguments.count >= 3 else { throw OptionsError.missingValue("login-item on|off") }
            switch arguments[2] {
            case "on": return .loginItem(enabled: true)
            case "off": return .loginItem(enabled: false)
            default: throw OptionsError.invalidValue(flag: "login-item", value: arguments[2])
            }
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
                options.config.bufferSeconds = try number(try value(&index, flag))
            case "--clip":
                options.clipSeconds = try number(try value(&index, flag))
            case "--export":
                options.exportPath = try value(&index, flag)
            case "--seconds":
                options.runSeconds = try number(try value(&index, flag))
            case "--height":
                options.config.height = try integer(try value(&index, flag))
            case "--fps":
                options.config.fps = try integer(try value(&index, flag))
            case "--bitrate":
                options.config.bitrate = try integer(try value(&index, flag))
            case "--display":
                options.config.displayIndex = try integer(try value(&index, flag))
            case "--app":
                options.config.applicationBundleID = try value(&index, flag)
            case "--capture-resolution":
                let raw = try value(&index, flag)
                guard let resolution = CaptureResolution(rawValue: raw) else {
                    throw OptionsError.invalidValue(flag: flag, value: raw)
                }
                options.config.captureResolution = resolution
            case "--stats":
                options.statisticsInterval = try number(try value(&index, flag))
            case "--no-cursor":
                options.config.showsCursor = false
            default:
                throw OptionsError.unknownCommand(flag)
            }
            index += 1
        }

        return options
    }

    private static func integer(_ raw: String) throws -> Int {
        guard let value = Int(raw) else {
            throw OptionsError.invalidValue(flag: "value", value: raw)
        }
        return value
    }

    private static func number(_ raw: String) throws -> Double {
        guard let value = Double(raw) else {
            throw OptionsError.invalidValue(flag: "value", value: raw)
        }
        return value
    }
}
