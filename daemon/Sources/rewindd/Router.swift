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
                    path: request.path
                )
                return .json(Response(ok: true, state: await recorder.state(), clip: clip))

            case .openUI:
                launcher.open()
                return .json(Response(ok: true))

            case .listDisplays:
                return .json(Response(ok: true, displays: try await recorder.displays()))

            case .listApplications:
                return .json(Response(ok: true, applications: try await recorder.applications()))

            case .previewFrame:
                let image = try await recorder.preview(
                    secondsAgo: request.seconds ?? 0,
                    width: request.width ?? 0
                )
                return .image(image)

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
