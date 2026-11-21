import AppKit
import Foundation
import RewindKit

enum Router {
    static func handle(_ request: Request, recorder: Recorder, launcher: UILauncher) async -> Reply {
        do {
            switch request.command {
            case .status:
                return .json(Response(ok: true, state: await recorder.state()))

            case .start:
                return .json(Response(ok: true, state: await recorder.start()))

            case .stop:
                return .json(Response(ok: true, state: await recorder.stop()))

            case .setConfig:
                guard let config = request.config else {
                    return .json(.failure("setConfig requires a config"))
                }
                return .json(Response(ok: true, state: await recorder.apply(config)))

            case .exportClip:
                let range = request.range
                    ?? ClipRange(from: request.seconds ?? 30, to: 0)
                guard range.to >= 0, range.duration > 0 else {
                    return .json(.failure("range needs from > to >= 0"))
                }
                let clip = try await recorder.export(
                    from: range.from,
                    to: range.to,
                    path: request.path,
                    anchor: request.anchor,
                    overlayPath: request.overlayPath
                )
                return .json(Response(ok: true, state: await recorder.state(), clip: clip))

            case .copyClip:
                guard let range = request.range, range.to >= 0, range.duration > 0 else {
                    return .json(.failure("copyClip needs a range with from > to >= 0"))
                }
                let clip = try await recorder.export(
                    from: range.from,
                    to: range.to,
                    path: Clipboard.newCachedClipURL().path,
                    anchor: request.anchor,
                    overlayPath: request.overlayPath
                )
                try await Clipboard.copyFile(URL(fileURLWithPath: clip.path))
                return .json(Response(ok: true, state: await recorder.state(), clip: clip))

            case .openUI:
                launcher.open()
                return .json(Response(ok: true))

            case .listDisplays:
                return .json(Response(ok: true, displays: try await recorder.displays()))

            case .listApplications:
                return .json(Response(ok: true, applications: try await recorder.applications()))

            case .thumbnails:
                let images = try await recorder.thumbnails(
                    count: request.count ?? 24,
                    width: request.width ?? 160,
                    anchor: request.anchor
                )
                return .images(images)

            case .estimate:
                guard let range = request.range, range.duration > 0 else {
                    return .json(.failure("estimate needs a range"))
                }
                let bytes = try await recorder.estimate(
                    from: range.from,
                    to: range.to,
                    anchor: request.anchor
                )
                return .json(Response(ok: true, estimatedBytes: bytes))

            case .edgeFrames:
                let images = try await recorder.edgeFrames(
                    from: request.range?.from ?? request.seconds ?? 0,
                    to: request.range?.to ?? 0,
                    width: request.width ?? 480,
                    anchor: request.anchor
                )
                return .images(images)

            case .previewFrame:
                let image = try await recorder.preview(
                    secondsAgo: request.seconds ?? 0,
                    width: request.width ?? 0,
                    anchor: request.anchor
                )
                return .image(image)

            case .copyFrame:
                // Native resolution regardless of what the overlay previews at.
                var image = try await recorder.preview(
                    secondsAgo: request.seconds ?? 0,
                    width: 0,
                    anchor: request.anchor
                )
                if let overlayPath = request.overlayPath {
                    image = try AnnotationOverlay.load(overlayPath).composite(image)
                }
                try await Clipboard.copy(image)
                return .json(Response(ok: true))

            case .quit:
                // Let the response reach the client before the socket goes away.
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
                    NSApp.terminate(nil)
                }
                return .json(Response(ok: true))
            }
        } catch {
            return .json(.failure("\(error)"))
        }
    }
}
