// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "Rewind",
    platforms: [.macOS(.v15)],
    products: [
        .executable(name: "rewindd", targets: ["RewindDaemon"])
    ],
    targets: [
        .executableTarget(
            name: "RewindDaemon",
            path: "Sources/RewindDaemon",
            swiftSettings: [
                // The capture pipeline is owned by a serial DispatchQueue, which
                // Swift's actor model cannot express. Swift 6 strict concurrency
                // would only force `@unchecked Sendable` annotations onto every
                // stage, so the language mode stays at 5 and ownership is
                // documented at each boundary instead.
                .swiftLanguageMode(.v5)
            ]
        )
    ]
)
