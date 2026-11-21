import AppKit
import Foundation
import RewindKit

enum ClipboardError: Error, CustomStringConvertible {
    case writeFailed

    var description: String {
        switch self {
        case .writeFailed: return "the pasteboard refused the copy"
        }
    }
}

enum Clipboard {
    /// Puts a file reference on the pasteboard, which is what Finder, Messages
    /// and Slack paste as the video or GIF itself.
    @MainActor
    static func copyFile(_ url: URL) throws {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        guard pasteboard.writeObjects([url as NSURL]) else {
            throw ClipboardError.writeFailed
        }
    }

    /// Copied clips live in Caches rather than ~/Movies/Rewind: they are only
    /// there to be pasted, so they should not clutter the clip library. The
    /// file has to outlive the copy for the paste to work, so copies older than
    /// a day are removed here instead of immediately.
    static func newCachedClipURL(format: ExportFormat) -> URL {
        let directory = FileManager.default
            .urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Rewind", isDirectory: true)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)

        let cutoff = Date().addingTimeInterval(-24 * 60 * 60)
        let existing = (try? FileManager.default.contentsOfDirectory(
            at: directory,
            includingPropertiesForKeys: [.contentModificationDateKey]
        )) ?? []
        for file in existing where ["mp4", "gif"].contains(file.pathExtension) {
            let modified = try? file.resourceValues(forKeys: [.contentModificationDateKey])
                .contentModificationDate
            if let modified, modified < cutoff {
                try? FileManager.default.removeItem(at: file)
            }
        }

        let formatter = DateFormatter()
        formatter.dateFormat = "yyyy-MM-dd HH.mm.ss"
        return directory.appendingPathComponent(
            "Rewind \(formatter.string(from: Date())).\(format.fileExtension)"
        )
    }

    /// Puts a decoded frame on the general pasteboard as PNG, which is what
    /// browsers, chat apps and Preview all paste.
    @MainActor
    static func copy(_ image: RawImage) throws {
        // These are CoreGraphics objects from an async task, which has no
        // autorelease pool of its own: without this, every copy left its
        // bitmap and PNG behind (~23 MB each).
        try autoreleasepool {
            let data = try image.pngData()
            let pasteboard = NSPasteboard.general
            pasteboard.clearContents()
            guard pasteboard.setData(data, forType: .png) else {
                throw ClipboardError.writeFailed
            }
        }
    }
}
