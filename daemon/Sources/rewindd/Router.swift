import AppKit
import Foundation
import RewindKit

enum Router {
    static func handle(_ request: Request, recorder: Recorder) async -> Response {
        do {
            switch request.command {
            case .status:
                return Response(ok: true, state: await recorder.state())

            case .start:
                return Response(ok: true, state: await recorder.start())

            case .stop:
                return Response(ok: true, state: await recorder.stop())

            case .setConfig:
                guard let config = request.config else {
                    return .failure("setConfig requires a config")
                }
                return Response(ok: true, state: await recorder.apply(config))

            case .exportClip:
                let seconds = request.seconds ?? 30
                guard seconds > 0 else { return .failure("seconds must be positive") }
                let clip = try await recorder.export(seconds: seconds, path: request.path)
                return Response(ok: true, state: await recorder.state(), clip: clip)

            case .listDisplays:
                return Response(ok: true, displays: try await recorder.displays())

            case .listApplications:
                return Response(ok: true, applications: try await recorder.applications())

            case .quit:
                // Let the response reach the client before the socket goes away.
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
                    NSApp.terminate(nil)
                }
                return Response(ok: true)
            }
        } catch {
            return .failure("\(error)")
        }
    }
}
