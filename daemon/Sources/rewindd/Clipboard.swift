import AppKit
import ImageIO
import UniformTypeIdentifiers
import CoreGraphics
import Foundation
import RewindKit

enum ClipboardError: Error, CustomStringConvertible {
    case imageCreationFailed
    case encodingFailed
    case writeFailed

    var description: String {
        switch self {
        case .imageCreationFailed: return "could not build an image from the frame"
        case .encodingFailed: return "could not encode the frame"
        case .writeFailed: return "the pasteboard refused the frame"
        }
    }
}

enum Clipboard {
    /// Puts a file reference on the pasteboard, which is what Finder, Messages
    /// and Slack paste as the video itself.
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
    static func newCachedClipURL() -> URL {
        let directory = FileManager.default
            .urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Rewind", isDirectory: true)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)

        let cutoff = Date().addingTimeInterval(-24 * 60 * 60)
        let existing = (try? FileManager.default.contentsOfDirectory(
            at: directory,
            includingPropertiesForKeys: [.contentModificationDateKey]
        )) ?? []
        for file in existing where file.pathExtension == "mp4" {
            let modified = try? file.resourceValues(forKeys: [.contentModificationDateKey])
                .contentModificationDate
            if let modified, modified < cutoff {
                try? FileManager.default.removeItem(at: file)
            }
        }

        let formatter = DateFormatter()
        formatter.dateFormat = "yyyy-MM-dd HH.mm.ss"
        return directory.appendingPathComponent("Rewind \(formatter.string(from: Date())).mp4")
    }

    /// Puts a decoded frame on the general pasteboard as PNG and TIFF. PNG is
    /// what browsers and chat apps take; TIFF is what Preview and older AppKit
    /// apps paste.
    @MainActor
    static func copy(_ image: RawImage) throws {
        // These are AppKit/CoreGraphics objects from an async task, which has no
        // autorelease pool of its own: without this, every copy left its
        // bitmap, TIFF and PNG behind (~23 MB each).
        try autoreleasepool {
        guard
            let provider = CGDataProvider(data: image.pixels as CFData),
            let cgImage = CGImage(
                width: image.width,
                height: image.height,
                bitsPerComponent: 8,
                bitsPerPixel: 32,
                bytesPerRow: image.width * 4,
                space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGBitmapInfo(
                    rawValue: CGImageAlphaInfo.noneSkipFirst.rawValue
                        | CGBitmapInfo.byteOrder32Little.rawValue
                ),
                provider: provider,
                decode: nil,
                shouldInterpolate: false,
                intent: .defaultIntent
            )
        else {
            throw ClipboardError.imageCreationFailed
        }

        // PNG only, encoded straight from the CGImage. Going through
        // NSBitmapImageRep also materialised a full uncompressed TIFF and left
        // ~30 MB behind per copy.
        let data = NSMutableData()
        guard
            let destination = CGImageDestinationCreateWithData(
                data as CFMutableData,
                UTType.png.identifier as CFString,
                1,
                nil
            )
        else {
            throw ClipboardError.encodingFailed
        }
        CGImageDestinationAddImage(destination, cgImage, nil)
        guard CGImageDestinationFinalize(destination) else {
            throw ClipboardError.encodingFailed
        }

        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        guard pasteboard.setData(data as Data, forType: .png) else {
            throw ClipboardError.writeFailed
        }
        }
    }
}
