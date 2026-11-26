// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "Rewind",
    platforms: [.macOS(.v15)],
    products: [
        .executable(name: "rewindd", targets: ["rewindd"]),
        .executable(name: "rewind", targets: ["rewind"]),
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
            name: "rewind",
            dependencies: ["RewindKit"],
            path: "Sources/rewind",
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
    ]
)
