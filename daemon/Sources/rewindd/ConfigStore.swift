import Foundation
import RewindKit

enum ConfigStore {
    private static var url: URL {
        FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Rewind", isDirectory: true)
            .appendingPathComponent("config.json")
    }

    static func load() -> RecorderConfig {
        guard
            let data = try? Data(contentsOf: url),
            let config = try? JSONDecoder().decode(RecorderConfig.self, from: data)
        else {
            return RecorderConfig()
        }
        return config
    }

    static func save(_ config: RecorderConfig) {
        do {
            try FileManager.default.createDirectory(
                at: url.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            try encoder.encode(config).write(to: url)
        } catch {
            FileHandle.standardError.write(
                Data("rewindd: could not save config: \(error)\n".utf8)
            )
        }
    }
}
