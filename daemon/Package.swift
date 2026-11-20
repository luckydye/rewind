// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "Rewind",
    platforms: [.macOS(.v15)],
    products: [
        .executable(name: "rewindd", targets: ["rewindd"]),
        .executable(name: "rewindctl", targets: ["rewindctl"]),
        .library(name: "RewindKit", targets: ["RewindKit"]),
    ],
    targets: [
        .target(
            name: "RewindKit",
            path: "Sources/RewindKit",
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
        .executableTarget(
            name: "rewindd",
            dependencies: ["RewindKit"],
            path: "Sources/rewindd",
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
        .executableTarget(
            name: "rewindctl",
            dependencies: ["RewindKit"],
            path: "Sources/rewindctl",
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
    ]
)
